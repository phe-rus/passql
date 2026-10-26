//! SQL executor.
//!
//! Takes a [`Plan`] and runs it against the v4 KV store, returning either
//! a row stream (for SELECT) or an affected-rows count (for DML).

use crate::Storage;
use crate::sql::plan::{BinaryOp, Plan, Projection, ScalarExpr};
use crate::sql::schema::{
    TableCatalog, decode_row, encode_row, row_key, table_end_key, table_start_key,
};
use crate::sql::types::Value;
use std::io;

/// The result of executing one SQL statement.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionResult {
    /// `CREATE TABLE` / `DROP TABLE` succeeded.
    SchemaChanged { message: String },
    /// `INSERT` succeeded; `rows_affected` rows were inserted.
    Inserted { rows_affected: u64 },
    /// `UPDATE` succeeded; `rows_affected` rows were updated.
    Updated { rows_affected: u64 },
    /// `DELETE` succeeded; `rows_affected` rows were deleted.
    Deleted { rows_affected: u64 },
    /// `SELECT` produced rows.
    Selected {
        columns: Vec<String>,
        rows: Vec<Row>,
    },
}

/// A single result row.
pub type Row = Vec<Value>;

/// Parse and execute one SQL statement with no bound parameters.
pub fn execute(sql: &str, storage: &mut Storage) -> Result<ExecutionResult, ExecutionError> {
    execute_with_params(sql, storage, &[])
}

/// Parse and execute one SQL statement with bound parameters.
///
/// Parameters are referenced positionally with `?` in the SQL text.
/// `params[0]` corresponds to the first `?`, `params[1]` to the second, etc.
pub fn execute_with_params(
    sql: &str,
    storage: &mut Storage,
    params: &[Value],
) -> Result<ExecutionResult, ExecutionError> {
    let dialect = sqlparser::dialect::GenericDialect {};
    let statements = sqlparser::parser::Parser::parse_sql(&dialect, sql)
        .map_err(|error| ExecutionError::Parse(error.to_string()))?;

    if statements.len() != 1 {
        return Err(ExecutionError::Parse(format!(
            "expected exactly one statement, got {}",
            statements.len()
        )));
    }

    // Plan needs the catalog (which borrows storage mutably). Once the plan
    // is built, the borrow ends and we can re-borrow storage for execution.
    let plan = {
        let mut catalog = TableCatalog::new(storage);
        crate::sql::plan::plan(&statements[0], &mut catalog)?
    };
    run_plan(plan, storage, params)
}

fn run_plan(
    plan: Plan,
    storage: &mut Storage,
    params: &[Value],
) -> Result<ExecutionResult, ExecutionError> {
    match plan {
        Plan::CreateTable {
            schema,
            if_not_exists,
        } => {
            let mut catalog = TableCatalog::new(storage);
            if if_not_exists && catalog.load(&schema.table)?.is_some() {
                return Ok(ExecutionResult::SchemaChanged {
                    message: format!("table {} already exists", schema.table),
                });
            }
            catalog.create_table(&schema)?;
            Ok(ExecutionResult::SchemaChanged {
                message: format!("table {} created", schema.table),
            })
        }
        Plan::DropTable { table, if_exists } => {
            let mut catalog = TableCatalog::new(storage);
            let dropped = catalog.drop_table(&table)?;
            if !dropped && !if_exists {
                return Err(ExecutionError::Plan(crate::sql::plan::PlanError::new(
                    crate::sql::plan::PlanErrorKind::UnknownTable,
                    format!("no such table: {table}"),
                )));
            }
            Ok(ExecutionResult::SchemaChanged {
                message: format!("table {} dropped", table),
            })
        }
        Plan::Insert {
            schema,
            columns,
            rows,
        } => run_insert(storage, &schema, &columns, &rows, params),
        Plan::Select {
            schema,
            projection,
            predicate,
            order_by,
            limit,
        } => run_select(
            storage,
            &schema,
            &projection,
            &predicate,
            order_by,
            limit,
            params,
        ),
        Plan::Update {
            schema,
            assignments,
            predicate,
        } => run_update(storage, &schema, &assignments, &predicate, params),
        Plan::Delete { schema, predicate } => run_delete(storage, &schema, &predicate, params),
    }
}

fn run_insert(
    storage: &mut Storage,
    schema: &crate::sql::schema::Schema,
    columns: &Option<Vec<usize>>,
    rows: &[Vec<ScalarExpr>],
    params: &[Value],
) -> Result<ExecutionResult, ExecutionError> {
    let target_indices: Vec<usize> = columns
        .clone()
        .unwrap_or_else(|| (0..schema.columns.len()).collect());

    let rowid_seq_key = format!("__rowid__:{}:next", schema.table);
    let mut next_rowid: u64 = match storage.get(&rowid_seq_key)? {
        Some(bytes) => {
            let arr: [u8; 8] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| ExecutionError::CorruptRowid)?;
            u64::from_le_bytes(arr)
        }
        None => 1,
    };

    let mut transaction = storage.begin()?;
    let mut inserted = 0u64;
    for row_exprs in rows {
        let mut row_values = vec![Value::Null; schema.columns.len()];
        for (i, expr) in row_exprs.iter().enumerate() {
            let column_index = target_indices[i];
            row_values[column_index] = eval_expr(expr, &[], params)?;
        }

        // Auto-assign rowid for INTEGER PRIMARY KEY columns that were not set.
        let rowid_for_key = if let Some(pk_index) = schema.primary_key_index() {
            if matches!(row_values[pk_index], Value::Null)
                && schema.columns[pk_index].column_type == crate::sql::schema::ColumnType::Integer
            {
                let rowid = next_rowid;
                next_rowid = next_rowid
                    .checked_add(1)
                    .ok_or(ExecutionError::RowidOverflow)?;
                row_values[pk_index] = Value::Integer(rowid as i64);
                rowid
            } else if let Value::Integer(value) = row_values[pk_index] {
                value as u64
            } else {
                return Err(ExecutionError::TypeMismatch(
                    "primary key must be an integer".to_owned(),
                ));
            }
        } else {
            // No primary key column: use the synthetic rowid.
            let rowid = next_rowid;
            next_rowid = next_rowid
                .checked_add(1)
                .ok_or(ExecutionError::RowidOverflow)?;
            rowid
        };

        let key = row_key(&schema.table, rowid_for_key);
        transaction.set(&key, &encode_row(&row_values))?;
        inserted += 1;
    }
    transaction.set(&rowid_seq_key, &next_rowid.to_le_bytes())?;
    transaction.commit()?;
    Ok(ExecutionResult::Inserted {
        rows_affected: inserted,
    })
}

fn run_select(
    storage: &mut Storage,
    schema: &crate::sql::schema::Schema,
    projection: &Projection,
    predicate: &Option<ScalarExpr>,
    order_by: Option<(usize, bool)>,
    limit: Option<u64>,
    params: &[Value],
) -> Result<ExecutionResult, ExecutionError> {
    let start = table_start_key(&schema.table);
    let end = table_end_key(&schema.table);
    let entries = storage.range(Some(&start), Some(&end))?;

    let mut rows: Vec<Row> = Vec::new();
    for (_key, payload) in &entries {
        let values = decode_row(payload)?;
        if let Some(pred) = predicate
            && !eval_expr(pred, &values, params)?.truthy()?
        {
            continue;
        }
        rows.push(values);
    }

    if let Some((col_idx, ascending)) = order_by {
        rows.sort_by(|a, b| {
            let cmp = compare_values(&a[col_idx], &b[col_idx]);
            if ascending { cmp } else { cmp.reverse() }
        });
    }

    if let Some(limit) = limit {
        rows.truncate(limit as usize);
    }

    let (column_names, projected_rows) = match projection {
        Projection::Star => {
            let names = schema.columns.iter().map(|c| c.name.clone()).collect();
            (names, rows)
        }
        Projection::Columns(indices) => {
            let names = indices
                .iter()
                .map(|i| schema.columns[*i].name.clone())
                .collect();
            let projected = rows
                .into_iter()
                .map(|row| indices.iter().map(|i| row[*i].clone()).collect())
                .collect();
            (names, projected)
        }
    };

    Ok(ExecutionResult::Selected {
        columns: column_names,
        rows: projected_rows,
    })
}

fn run_update(
    storage: &mut Storage,
    schema: &crate::sql::schema::Schema,
    assignments: &[(usize, ScalarExpr)],
    predicate: &Option<ScalarExpr>,
    params: &[Value],
) -> Result<ExecutionResult, ExecutionError> {
    let start = table_start_key(&schema.table);
    let end = table_end_key(&schema.table);
    let entries = storage.range(Some(&start), Some(&end))?;

    let mut transaction = storage.begin()?;
    let mut updated = 0u64;
    for (key, payload) in &entries {
        let mut values = decode_row(payload)?;
        if let Some(pred) = predicate
            && !eval_expr(pred, &values, params)?.truthy()?
        {
            continue;
        }
        let mut changed = false;
        for (col_idx, expr) in assignments {
            let new_value = eval_expr(expr, &values, params)?;
            if values[*col_idx] != new_value {
                values[*col_idx] = new_value;
                changed = true;
            }
        }
        if changed {
            transaction.set(key, &encode_row(&values))?;
            updated += 1;
        }
    }
    transaction.commit()?;
    Ok(ExecutionResult::Updated {
        rows_affected: updated,
    })
}

fn run_delete(
    storage: &mut Storage,
    schema: &crate::sql::schema::Schema,
    predicate: &Option<ScalarExpr>,
    params: &[Value],
) -> Result<ExecutionResult, ExecutionError> {
    let start = table_start_key(&schema.table);
    let end = table_end_key(&schema.table);
    let entries = storage.range(Some(&start), Some(&end))?;

    let mut transaction = storage.begin()?;
    let mut deleted = 0u64;
    for (key, payload) in &entries {
        let values = decode_row(payload)?;
        if let Some(pred) = predicate
            && !eval_expr(pred, &values, params)?.truthy()?
        {
            continue;
        }
        transaction.delete(key)?;
        deleted += 1;
    }
    transaction.commit()?;
    Ok(ExecutionResult::Deleted {
        rows_affected: deleted,
    })
}

fn eval_expr(expr: &ScalarExpr, row: &[Value], params: &[Value]) -> Result<Value, ExecutionError> {
    match expr {
        ScalarExpr::Literal(value) => Ok(value.clone()),
        ScalarExpr::Column(index) => row
            .get(*index)
            .cloned()
            .ok_or(ExecutionError::ColumnIndex(*index)),
        ScalarExpr::Parameter(index) => params
            .get(*index - 1)
            .cloned()
            .ok_or(ExecutionError::MissingParameter(*index)),
        ScalarExpr::IsNull(inner) => {
            let value = eval_expr(inner, row, params)?;
            Ok(Value::Integer(if matches!(value, Value::Null) {
                1
            } else {
                0
            }))
        }
        ScalarExpr::Not(inner) => {
            let value = eval_expr(inner, row, params)?;
            Ok(Value::Integer(if !value.truthy()? { 1 } else { 0 }))
        }
        ScalarExpr::BinaryOp { op, left, right } => {
            let lv = eval_expr(left, row, params)?;
            let rv = eval_expr(right, row, params)?;
            Ok(eval_binary_op(*op, &lv, &rv)?)
        }
    }
}

fn eval_binary_op(op: BinaryOp, left: &Value, right: &Value) -> Result<Value, ExecutionError> {
    match op {
        BinaryOp::Eq => Ok(Value::Integer(
            if compare_values(left, right) == std::cmp::Ordering::Equal {
                1
            } else {
                0
            },
        )),
        BinaryOp::NotEq => Ok(Value::Integer(
            if compare_values(left, right) != std::cmp::Ordering::Equal {
                1
            } else {
                0
            },
        )),
        BinaryOp::Lt => Ok(Value::Integer(
            if compare_values(left, right) == std::cmp::Ordering::Less {
                1
            } else {
                0
            },
        )),
        BinaryOp::LtEq => Ok(Value::Integer(
            if matches!(
                compare_values(left, right),
                std::cmp::Ordering::Less | std::cmp::Ordering::Equal
            ) {
                1
            } else {
                0
            },
        )),
        BinaryOp::Gt => Ok(Value::Integer(
            if compare_values(left, right) == std::cmp::Ordering::Greater {
                1
            } else {
                0
            },
        )),
        BinaryOp::GtEq => Ok(Value::Integer(
            if matches!(
                compare_values(left, right),
                std::cmp::Ordering::Greater | std::cmp::Ordering::Equal
            ) {
                1
            } else {
                0
            },
        )),
        BinaryOp::And => Ok(Value::Integer(if left.truthy()? && right.truthy()? {
            1
        } else {
            0
        })),
        BinaryOp::Or => Ok(Value::Integer(if left.truthy()? || right.truthy()? {
            1
        } else {
            0
        })),
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div => {
            let (li, ri) = match (left, right) {
                (Value::Integer(a), Value::Integer(b)) => (a, b),
                _ => {
                    return Err(ExecutionError::TypeMismatch(
                        "arithmetic on non-integers".to_owned(),
                    ));
                }
            };
            let result = match op {
                BinaryOp::Add => li.checked_add(*ri),
                BinaryOp::Sub => li.checked_sub(*ri),
                BinaryOp::Mul => li.checked_mul(*ri),
                BinaryOp::Div => {
                    if *ri == 0 {
                        return Err(ExecutionError::DivideByZero);
                    }
                    Some(li.wrapping_div(*ri))
                }
                _ => unreachable!(),
            };
            Ok(Value::Integer(result.ok_or(ExecutionError::Overflow)?))
        }
    }
}

trait ValueTruthy {
    fn truthy(&self) -> Result<bool, ExecutionError>;
}

impl ValueTruthy for Value {
    fn truthy(&self) -> Result<bool, ExecutionError> {
        Ok(match self {
            Self::Null => false,
            Self::Integer(value) => *value != 0,
            Self::Real(value) => *value != 0.0,
            Self::Text(value) => !value.is_empty(),
            Self::Blob(value) => !value.is_empty(),
        })
    }
}

fn compare_values(left: &Value, right: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (left, right) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Integer(a), Value::Integer(b)) => a.cmp(b),
        (Value::Real(a), Value::Real(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
        (Value::Integer(a), Value::Real(b)) => {
            (*a as f64).partial_cmp(b).unwrap_or(Ordering::Equal)
        }
        (Value::Real(a), Value::Integer(b)) => {
            a.partial_cmp(&(*b as f64)).unwrap_or(Ordering::Equal)
        }
        (Value::Text(a), Value::Text(b)) => a.cmp(b),
        (Value::Blob(a), Value::Blob(b)) => a.cmp(b),
        _ => Ordering::Equal,
    }
}

#[derive(Debug, Clone)]
pub enum ExecutionError {
    Parse(String),
    Plan(crate::sql::plan::PlanError),
    Io(String),
    CorruptRowid,
    RowidOverflow,
    Overflow,
    DivideByZero,
    ColumnIndex(usize),
    MissingParameter(usize),
    TypeMismatch(String),
}

impl std::fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(message) => write!(f, "parse error: {message}"),
            Self::Plan(error) => write!(f, "plan error: {error}"),
            Self::Io(message) => write!(f, "io error: {message}"),
            Self::CorruptRowid => write!(f, "corrupt rowid sequence"),
            Self::RowidOverflow => write!(f, "rowid overflow"),
            Self::Overflow => write!(f, "arithmetic overflow"),
            Self::DivideByZero => write!(f, "divide by zero"),
            Self::ColumnIndex(idx) => write!(f, "column index out of range: {idx}"),
            Self::MissingParameter(idx) => write!(f, "missing bound parameter {idx}"),
            Self::TypeMismatch(message) => write!(f, "type mismatch: {message}"),
        }
    }
}

impl std::error::Error for ExecutionError {}

impl From<crate::sql::plan::PlanError> for ExecutionError {
    fn from(error: crate::sql::plan::PlanError) -> Self {
        Self::Plan(error)
    }
}

impl From<io::Error> for ExecutionError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl From<crate::sql::types::ValueError> for ExecutionError {
    fn from(error: crate::sql::types::ValueError) -> Self {
        Self::Io(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn test_path() -> PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("passql-sql-{}-{id}.psl", std::process::id()))
    }

    fn fresh_storage() -> io::Result<(PathBuf, Storage)> {
        let path = test_path();
        let storage = Storage::create(&path)?;
        Ok((path, storage))
    }

    #[test]
    fn create_insert_select_roundtrip() -> Result<(), ExecutionError> {
        let (path, mut storage) = fresh_storage().map_err(|e| ExecutionError::Io(e.to_string()))?;

        execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL)",
            &mut storage,
        )?;
        execute(
            "INSERT INTO users (id, name, score) VALUES (1, 'Alice', 3.5)",
            &mut storage,
        )?;
        execute(
            "INSERT INTO users (id, name, score) VALUES (2, 'Bob', 2.71)",
            &mut storage,
        )?;

        let result = execute("SELECT id, name FROM users WHERE id = 1", &mut storage)?;
        match result {
            ExecutionResult::Selected { columns, rows } => {
                assert_eq!(columns, vec!["id".to_owned(), "name".to_owned()]);
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0][0], Value::Integer(1));
                assert_eq!(rows[0][1], Value::Text("Alice".to_owned()));
            }
            _ => panic!("expected Selected, got {result:?}"),
        }

        let all = execute("SELECT * FROM users ORDER BY id", &mut storage)?;
        match all {
            ExecutionResult::Selected { columns, rows } => {
                assert_eq!(columns.len(), 3);
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0][1], Value::Text("Alice".to_owned()));
                assert_eq!(rows[1][1], Value::Text("Bob".to_owned()));
            }
            _ => panic!("expected Selected"),
        }

        std::fs::remove_file(path).ok();
        Ok(())
    }

    #[test]
    fn update_and_delete() -> Result<(), ExecutionError> {
        let (path, mut storage) = fresh_storage().map_err(|e| ExecutionError::Io(e.to_string()))?;

        execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER)",
            &mut storage,
        )?;
        execute("INSERT INTO t (id, n) VALUES (1, 10)", &mut storage)?;
        execute("INSERT INTO t (id, n) VALUES (2, 20)", &mut storage)?;
        execute("INSERT INTO t (id, n) VALUES (3, 30)", &mut storage)?;

        let updated = execute("UPDATE t SET n = 25 WHERE id = 2", &mut storage)?;
        assert_eq!(updated, ExecutionResult::Updated { rows_affected: 1 });

        let deleted = execute("DELETE FROM t WHERE n > 25", &mut storage)?;
        assert_eq!(deleted, ExecutionResult::Deleted { rows_affected: 1 });

        let result = execute("SELECT id, n FROM t ORDER BY id", &mut storage)?;
        match result {
            ExecutionResult::Selected { rows, .. } => {
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0], vec![Value::Integer(1), Value::Integer(10)]);
                assert_eq!(rows[1], vec![Value::Integer(2), Value::Integer(25)]);
            }
            _ => panic!("expected Selected"),
        }

        std::fs::remove_file(path).ok();
        Ok(())
    }

    #[test]
    fn parameterized_query() -> Result<(), ExecutionError> {
        let (path, mut storage) = fresh_storage().map_err(|e| ExecutionError::Io(e.to_string()))?;

        execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
            &mut storage,
        )?;
        execute_with_params(
            "INSERT INTO t (id, name) VALUES (?, ?)",
            &mut storage,
            &[Value::Integer(1), Value::Text("hello".to_owned())],
        )?;

        let result = execute_with_params(
            "SELECT name FROM t WHERE id = ?",
            &mut storage,
            &[Value::Integer(1)],
        )?;
        match result {
            ExecutionResult::Selected { rows, .. } => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0][0], Value::Text("hello".to_owned()));
            }
            _ => panic!("expected Selected"),
        }

        std::fs::remove_file(path).ok();
        Ok(())
    }

    #[test]
    fn auto_assigns_synthetic_rowid_when_no_pk() -> Result<(), ExecutionError> {
        let (path, mut storage) = fresh_storage().map_err(|e| ExecutionError::Io(e.to_string()))?;

        execute("CREATE TABLE log (msg TEXT)", &mut storage)?;
        execute("INSERT INTO log (msg) VALUES ('a')", &mut storage)?;
        execute("INSERT INTO log (msg) VALUES ('b')", &mut storage)?;
        execute("INSERT INTO log (msg) VALUES ('c')", &mut storage)?;

        let result = execute("SELECT msg FROM log", &mut storage)?;
        match result {
            ExecutionResult::Selected { rows, .. } => {
                assert_eq!(rows.len(), 3);
            }
            _ => panic!("expected Selected"),
        }

        std::fs::remove_file(path).ok();
        Ok(())
    }
}
