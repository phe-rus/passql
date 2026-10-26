//! Logical plan: SQL AST → Passql operations.
//!
//! The plan is a small enum. Each variant knows how to execute itself
//! against the KV store. This layer is intentionally simple — no
//! optimizer, no cost model, no join reordering. Stage 2 is about
//! correctness, not performance.

use crate::sql::schema::{Column, ColumnType, Schema};
use crate::sql::types::Value;
use sqlparser::ast as sqlast;
use std::io::{self, ErrorKind};

/// A logical plan for one SQL statement.
#[derive(Debug, Clone)]
pub enum Plan {
    CreateTable {
        schema: Schema,
        if_not_exists: bool,
    },
    DropTable {
        table: String,
        if_exists: bool,
    },
    Insert {
        schema: Schema,
        columns: Option<Vec<usize>>,
        rows: Vec<Vec<ScalarExpr>>,
    },
    Select {
        schema: Schema,
        projection: Projection,
        predicate: Option<ScalarExpr>,
        order_by: Option<(usize, bool)>, // (column_index, ascending)
        limit: Option<u64>,
    },
    Update {
        schema: Schema,
        assignments: Vec<(usize, ScalarExpr)>,
        predicate: Option<ScalarExpr>,
    },
    Delete {
        schema: Schema,
        predicate: Option<ScalarExpr>,
    },
}

/// What to project out of a SELECT.
#[derive(Debug, Clone)]
pub enum Projection {
    Star,
    Columns(Vec<usize>),
}

/// A scalar expression evaluated per row. Renamed from `Expr` to avoid
/// collision with `sqlparser::ast::Expr`.
#[derive(Debug, Clone)]
pub enum ScalarExpr {
    Column(usize),
    Literal(Value),
    Parameter(usize), // 1-indexed parameter placeholder
    BinaryOp {
        op: BinaryOp,
        left: Box<ScalarExpr>,
        right: Box<ScalarExpr>,
    },
    IsNull(Box<ScalarExpr>),
    Not(Box<ScalarExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Add,
    Sub,
    Mul,
    Div,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanErrorKind {
    UnsupportedStatement,
    UnsupportedExpression,
    UnknownTable,
    UnknownColumn,
    AmbiguousColumn,
    TypeMismatch,
    ColumnCountMismatch,
    ValueCountMismatch,
    InvalidDefault,
    SyntaxError,
}

#[derive(Debug, Clone)]
pub struct PlanError {
    pub kind: PlanErrorKind,
    pub message: String,
}

impl PlanError {
    pub fn new(kind: PlanErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for PlanError {}

impl From<PlanError> for io::Error {
    fn from(error: PlanError) -> Self {
        io::Error::new(ErrorKind::InvalidData, error.to_string())
    }
}

impl From<crate::sql::schema::SchemaError> for PlanError {
    fn from(error: crate::sql::schema::SchemaError) -> Self {
        Self::new(PlanErrorKind::SyntaxError, error.to_string())
    }
}

impl From<io::Error> for PlanError {
    fn from(error: io::Error) -> Self {
        Self::new(PlanErrorKind::SyntaxError, error.to_string())
    }
}

/// Plan one SQL statement against the live catalog.
pub fn plan(
    statement: &sqlast::Statement,
    catalog: &mut crate::sql::schema::TableCatalog<'_>,
) -> Result<Plan, PlanError> {
    let mut ctx = PlanContext {
        next_placeholder: 1,
    };
    plan_with_ctx(statement, catalog, &mut ctx)
}

struct PlanContext {
    next_placeholder: usize,
}

fn plan_with_ctx(
    statement: &sqlast::Statement,
    catalog: &mut crate::sql::schema::TableCatalog<'_>,
    ctx: &mut PlanContext,
) -> Result<Plan, PlanError> {
    use sqlast::Statement;
    match statement {
        Statement::CreateTable(create_table) => plan_create_table(
            &create_table.name,
            &create_table.columns,
            create_table.if_not_exists,
        ),
        Statement::Drop {
            object_type: sqlast::ObjectType::Table,
            names,
            if_exists,
            ..
        } => {
            let table = names
                .first()
                .and_then(|name| name.0.first())
                .map(|ident| ident.value.clone())
                .ok_or_else(|| PlanError::new(PlanErrorKind::SyntaxError, "missing table name"))?;
            Ok(Plan::DropTable {
                table,
                if_exists: *if_exists,
            })
        }
        Statement::Insert(insert) => plan_insert(
            &insert.table_name,
            &insert.columns,
            insert.source.as_deref(),
            catalog,
            ctx,
        ),
        Statement::Query(query) => plan_select(query, catalog, ctx),
        Statement::Update {
            table,
            assignments,
            selection,
            ..
        } => plan_update(table, assignments, selection, catalog, ctx),
        Statement::Delete(delete) => plan_delete(&delete.from, &delete.selection, catalog, ctx),
        _ => Err(PlanError::new(
            PlanErrorKind::UnsupportedStatement,
            format!("unsupported statement: {statement:?}"),
        )),
    }
}

fn plan_create_table(
    name: &sqlast::ObjectName,
    columns: &[sqlast::ColumnDef],
    if_not_exists: bool,
) -> Result<Plan, PlanError> {
    let table = name
        .0
        .first()
        .map(|ident| ident.value.clone())
        .ok_or_else(|| PlanError::new(PlanErrorKind::SyntaxError, "missing table name"))?;

    let mut schema_columns = Vec::with_capacity(columns.len());
    for column in columns {
        let column_type = match &column.data_type {
            sqlast::DataType::Integer(_)
            | sqlast::DataType::TinyInt(_)
            | sqlast::DataType::SmallInt(_)
            | sqlast::DataType::BigInt(_) => ColumnType::Integer,
            sqlast::DataType::Real | sqlast::DataType::Float(_) | sqlast::DataType::Double => {
                ColumnType::Real
            }
            sqlast::DataType::Text | sqlast::DataType::Varchar(_) | sqlast::DataType::String(_) => {
                ColumnType::Text
            }
            sqlast::DataType::Blob(_) | sqlast::DataType::Bytes(_) => ColumnType::Blob,
            other => {
                return Err(PlanError::new(
                    PlanErrorKind::TypeMismatch,
                    format!("unsupported column type: {other:?}"),
                ));
            }
        };

        let primary_key = column.options.iter().any(|opt| {
            matches!(
                opt.option,
                sqlast::ColumnOption::Unique {
                    is_primary: true,
                    ..
                }
            )
        });
        let nullable = !primary_key
            && !column
                .options
                .iter()
                .any(|opt| matches!(opt.option, sqlast::ColumnOption::NotNull));

        schema_columns.push(Column {
            name: column.name.value.clone(),
            column_type,
            nullable,
            primary_key,
        });
    }

    Ok(Plan::CreateTable {
        schema: Schema {
            table,
            columns: schema_columns,
        },
        if_not_exists,
    })
}

fn plan_insert(
    table_name: &sqlast::ObjectName,
    columns: &[sqlast::Ident],
    source: Option<&sqlast::Query>,
    catalog: &mut crate::sql::schema::TableCatalog<'_>,
    ctx: &mut PlanContext,
) -> Result<Plan, PlanError> {
    let table = table_name
        .0
        .first()
        .map(|ident| ident.value.clone())
        .ok_or_else(|| PlanError::new(PlanErrorKind::SyntaxError, "missing table name"))?;

    let schema = catalog.load(&table)?.ok_or_else(|| {
        PlanError::new(
            PlanErrorKind::UnknownTable,
            format!("no such table: {table}"),
        )
    })?;

    let column_indices: Option<Vec<usize>> = if columns.is_empty() {
        None
    } else {
        let mut indices = Vec::with_capacity(columns.len());
        for ident in columns {
            let idx = schema.column_index(&ident.value).ok_or_else(|| {
                PlanError::new(
                    PlanErrorKind::UnknownColumn,
                    format!("no such column: {}", ident.value),
                )
            })?;
            indices.push(idx);
        }
        Some(indices)
    };

    let query = source.ok_or_else(|| {
        PlanError::new(
            PlanErrorKind::SyntaxError,
            "INSERT requires a VALUES clause",
        )
    })?;
    let values = match &*query.body {
        sqlast::SetExpr::Values(values) => values,
        _ => {
            return Err(PlanError::new(
                PlanErrorKind::UnsupportedStatement,
                "only VALUES inserts are supported",
            ));
        }
    };

    let target_indices: Vec<usize> = column_indices
        .clone()
        .unwrap_or_else(|| (0..schema.columns.len()).collect());

    let mut rows = Vec::with_capacity(values.rows.len());
    for row in &values.rows {
        if row.len() != target_indices.len() {
            return Err(PlanError::new(
                PlanErrorKind::ValueCountMismatch,
                format!(
                    "expected {} values per row, got {}",
                    target_indices.len(),
                    row.len()
                ),
            ));
        }
        let mut planned_row = Vec::with_capacity(row.len());
        for (value_index, expr) in row.iter().enumerate() {
            let column_index = target_indices[value_index];
            let column = &schema.columns[column_index];
            let planned = plan_expr(expr, &schema, ctx)?;
            if let ScalarExpr::Literal(literal) = &planned
                && !value_matches_column(literal, column.column_type)
            {
                return Err(PlanError::new(
                    PlanErrorKind::TypeMismatch,
                    format!(
                        "value {:?} does not match column {} type {}",
                        literal,
                        column.name,
                        column.column_type.as_str()
                    ),
                ));
            }
            planned_row.push(planned);
        }
        rows.push(planned_row);
    }

    Ok(Plan::Insert {
        schema,
        columns: column_indices,
        rows,
    })
}

fn plan_select(
    query: &sqlast::Query,
    catalog: &mut crate::sql::schema::TableCatalog<'_>,
    ctx: &mut PlanContext,
) -> Result<Plan, PlanError> {
    let select = match &*query.body {
        sqlast::SetExpr::Select(select) => select,
        _ => {
            return Err(PlanError::new(
                PlanErrorKind::UnsupportedStatement,
                "only SELECT is supported",
            ));
        }
    };

    if select.distinct.is_some() {
        return Err(PlanError::new(
            PlanErrorKind::UnsupportedStatement,
            "DISTINCT not yet supported",
        ));
    }
    if !matches!(select.group_by, sqlast::GroupByExpr::Expressions(ref v, _) if v.is_empty()) {
        return Err(PlanError::new(
            PlanErrorKind::UnsupportedStatement,
            "GROUP BY not yet supported",
        ));
    }
    if select.into.is_some() {
        return Err(PlanError::new(
            PlanErrorKind::UnsupportedStatement,
            "SELECT INTO not yet supported",
        ));
    }
    if select.from.len() != 1 || !select.from[0].joins.is_empty() {
        return Err(PlanError::new(
            PlanErrorKind::UnsupportedStatement,
            "only single-table SELECT is supported (no joins)",
        ));
    }

    let table_name = match &select.from[0].relation {
        sqlast::TableFactor::Table { name, .. } => name
            .0
            .first()
            .map(|ident| ident.value.clone())
            .ok_or_else(|| PlanError::new(PlanErrorKind::SyntaxError, "missing table name"))?,
        _ => {
            return Err(PlanError::new(
                PlanErrorKind::UnsupportedStatement,
                "only named tables are supported",
            ));
        }
    };

    let schema = catalog.load(&table_name)?.ok_or_else(|| {
        PlanError::new(
            PlanErrorKind::UnknownTable,
            format!("no such table: {table_name}"),
        )
    })?;

    let projection = if select.projection.len() == 1
        && matches!(select.projection[0], sqlast::SelectItem::Wildcard(_))
    {
        Projection::Star
    } else {
        let mut indices = Vec::with_capacity(select.projection.len());
        for item in &select.projection {
            let name = match item {
                sqlast::SelectItem::UnnamedExpr(expr) => match expr {
                    sqlast::Expr::Identifier(ident) => &ident.value,
                    _ => {
                        return Err(PlanError::new(
                            PlanErrorKind::UnsupportedExpression,
                            "only column identifiers in projection",
                        ));
                    }
                },
                _ => {
                    return Err(PlanError::new(
                        PlanErrorKind::UnsupportedExpression,
                        "only column references in projection",
                    ));
                }
            };
            let idx = schema.column_index(name).ok_or_else(|| {
                PlanError::new(
                    PlanErrorKind::UnknownColumn,
                    format!("no such column: {name}"),
                )
            })?;
            indices.push(idx);
        }
        Projection::Columns(indices)
    };

    let predicate = match &select.selection {
        Some(expr) => Some(plan_expr(expr, &schema, ctx)?),
        None => None,
    };

    let order_by = query.order_by.as_ref().and_then(|order_by| {
        let first = order_by.exprs.first()?;
        let sqlast::Expr::Identifier(ident) = &first.expr else {
            return None;
        };
        let idx = schema.column_index(&ident.value)?;
        Some((idx, first.asc.unwrap_or(true)))
    });

    let limit = query.limit.as_ref().and_then(|value| match value {
        sqlast::Expr::Value(sqlast::Value::Number(s, _)) => s.parse().ok(),
        _ => None,
    });

    Ok(Plan::Select {
        schema,
        projection,
        predicate,
        order_by,
        limit,
    })
}

fn plan_update(
    table: &sqlast::TableWithJoins,
    assignments: &[sqlast::Assignment],
    selection: &Option<sqlast::Expr>,
    catalog: &mut crate::sql::schema::TableCatalog<'_>,
    ctx: &mut PlanContext,
) -> Result<Plan, PlanError> {
    let table_name = match &table.relation {
        sqlast::TableFactor::Table { name, .. } => name
            .0
            .first()
            .map(|ident| ident.value.clone())
            .ok_or_else(|| PlanError::new(PlanErrorKind::SyntaxError, "missing table name"))?,
        _ => {
            return Err(PlanError::new(
                PlanErrorKind::UnsupportedStatement,
                "only named tables",
            ));
        }
    };
    if !table.joins.is_empty() {
        return Err(PlanError::new(
            PlanErrorKind::UnsupportedStatement,
            "joins not yet supported",
        ));
    }

    let schema = catalog.load(&table_name)?.ok_or_else(|| {
        PlanError::new(
            PlanErrorKind::UnknownTable,
            format!("no such table: {table_name}"),
        )
    })?;

    let mut planned_assignments = Vec::with_capacity(assignments.len());
    for assignment in assignments {
        let col_name = match &assignment.target {
            sqlast::AssignmentTarget::ColumnName(name) => name
                .0
                .first()
                .map(|ident| ident.value.clone())
                .ok_or_else(|| PlanError::new(PlanErrorKind::SyntaxError, "missing column name"))?,
            _ => {
                return Err(PlanError::new(
                    PlanErrorKind::UnsupportedExpression,
                    "only single-column assignments are supported",
                ));
            }
        };
        let col_idx = schema.column_index(&col_name).ok_or_else(|| {
            PlanError::new(
                PlanErrorKind::UnknownColumn,
                format!("no such column: {col_name}"),
            )
        })?;
        let expr = plan_expr(&assignment.value, &schema, ctx)?;
        planned_assignments.push((col_idx, expr));
    }

    let predicate = match selection {
        Some(expr) => Some(plan_expr(expr, &schema, ctx)?),
        None => None,
    };

    Ok(Plan::Update {
        schema,
        assignments: planned_assignments,
        predicate,
    })
}

fn plan_delete(
    from: &sqlast::FromTable,
    selection: &Option<sqlast::Expr>,
    catalog: &mut crate::sql::schema::TableCatalog<'_>,
    ctx: &mut PlanContext,
) -> Result<Plan, PlanError> {
    let tables = match from {
        sqlast::FromTable::WithFromKeyword(tables) | sqlast::FromTable::WithoutKeyword(tables) => {
            tables
        }
    };
    let table_with_joins = tables
        .first()
        .ok_or_else(|| PlanError::new(PlanErrorKind::SyntaxError, "missing table name"))?;
    let table_name = match &table_with_joins.relation {
        sqlast::TableFactor::Table { name, .. } => name
            .0
            .first()
            .map(|ident| ident.value.clone())
            .ok_or_else(|| PlanError::new(PlanErrorKind::SyntaxError, "missing table name"))?,
        _ => {
            return Err(PlanError::new(
                PlanErrorKind::UnsupportedStatement,
                "only named tables",
            ));
        }
    };

    let schema = catalog.load(&table_name)?.ok_or_else(|| {
        PlanError::new(
            PlanErrorKind::UnknownTable,
            format!("no such table: {table_name}"),
        )
    })?;

    let predicate = match selection {
        Some(expr) => Some(plan_expr(expr, &schema, ctx)?),
        None => None,
    };

    Ok(Plan::Delete { schema, predicate })
}

fn plan_expr(
    expr: &sqlast::Expr,
    schema: &Schema,
    ctx: &mut PlanContext,
) -> Result<ScalarExpr, PlanError> {
    use sqlast::Expr;
    match expr {
        Expr::Identifier(ident) => {
            let idx = schema.column_index(&ident.value).ok_or_else(|| {
                PlanError::new(
                    PlanErrorKind::UnknownColumn,
                    format!("no such column: {}", ident.value),
                )
            })?;
            Ok(ScalarExpr::Column(idx))
        }
        Expr::Value(value) => match value {
            sqlast::Value::Placeholder(_) => {
                let idx = ctx.next_placeholder;
                ctx.next_placeholder += 1;
                Ok(ScalarExpr::Parameter(idx))
            }
            other => Ok(ScalarExpr::Literal(plan_value(other)?)),
        },
        Expr::BinaryOp { left, op, right } => {
            let bin_op = plan_binary_op(op)?;
            Ok(ScalarExpr::BinaryOp {
                op: bin_op,
                left: Box::new(plan_expr(left, schema, ctx)?),
                right: Box::new(plan_expr(right, schema, ctx)?),
            })
        }
        Expr::IsNull(expr) => Ok(ScalarExpr::IsNull(Box::new(plan_expr(expr, schema, ctx)?))),
        Expr::IsNotNull(expr) => Ok(ScalarExpr::Not(Box::new(ScalarExpr::IsNull(Box::new(
            plan_expr(expr, schema, ctx)?,
        ))))),
        Expr::UnaryOp {
            op: sqlast::UnaryOperator::Not,
            expr,
        } => Ok(ScalarExpr::Not(Box::new(plan_expr(expr, schema, ctx)?))),
        _ => Err(PlanError::new(
            PlanErrorKind::UnsupportedExpression,
            format!("unsupported expression: {expr:?}"),
        )),
    }
}

fn plan_binary_op(op: &sqlast::BinaryOperator) -> Result<BinaryOp, PlanError> {
    use sqlast::BinaryOperator;
    Ok(match op {
        BinaryOperator::Eq => BinaryOp::Eq,
        BinaryOperator::NotEq => BinaryOp::NotEq,
        BinaryOperator::Lt => BinaryOp::Lt,
        BinaryOperator::LtEq => BinaryOp::LtEq,
        BinaryOperator::Gt => BinaryOp::Gt,
        BinaryOperator::GtEq => BinaryOp::GtEq,
        BinaryOperator::Plus => BinaryOp::Add,
        BinaryOperator::Minus => BinaryOp::Sub,
        BinaryOperator::Multiply => BinaryOp::Mul,
        BinaryOperator::Divide => BinaryOp::Div,
        BinaryOperator::And => BinaryOp::And,
        BinaryOperator::Or => BinaryOp::Or,
        _ => {
            return Err(PlanError::new(
                PlanErrorKind::UnsupportedExpression,
                format!("unsupported operator: {op:?}"),
            ));
        }
    })
}

fn plan_value(value: &sqlast::Value) -> Result<Value, PlanError> {
    Ok(match value {
        sqlast::Value::Number(s, _) => {
            if let Ok(i) = s.parse::<i64>() {
                Value::Integer(i)
            } else if let Ok(f) = s.parse::<f64>() {
                Value::Real(f)
            } else {
                return Err(PlanError::new(
                    PlanErrorKind::TypeMismatch,
                    format!("unparseable number: {s}"),
                ));
            }
        }
        sqlast::Value::SingleQuotedString(s) => Value::Text(s.clone()),
        sqlast::Value::DoubleQuotedString(s) => Value::Text(s.clone()),
        sqlast::Value::Null => Value::Null,
        _ => {
            return Err(PlanError::new(
                PlanErrorKind::UnsupportedExpression,
                format!("unsupported literal: {value:?}"),
            ));
        }
    })
}

fn value_matches_column(value: &Value, column_type: ColumnType) -> bool {
    matches!(
        (value, column_type),
        (Value::Null, _)
            | (Value::Integer(_), ColumnType::Integer)
            | (Value::Integer(_), ColumnType::Real)
            | (Value::Real(_), ColumnType::Real)
            | (Value::Text(_), ColumnType::Text)
            | (Value::Blob(_), ColumnType::Blob)
    )
}
