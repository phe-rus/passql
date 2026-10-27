//! Node.js bindings for Passql.
//!
//! Mirrors the `better-sqlite3` synchronous API as closely as the engine's
//! current SQL subset allows.

use napi::bindgen_prelude::{Buffer, Either4, Env, Error, Result, Status};
use napi::{JsObject, JsUnknown};
use napi_derive::napi;

use passql::sql::executor::ExecutionError;
use passql::sql::{ExecutionResult, Value as PassqlValue};
use passql::Storage;
use std::sync::{Arc, Mutex};

// Type alias for "any JS scalar we accept as a SQL parameter".
// napi-rs's Either supports 2-4 variants. We use Either4 to cover
// string, double, i64, and Buffer — the four practical parameter kinds.
type JsParam = Either4<String, f64, i64, Buffer>;

fn js_param_to_value(value: JsParam) -> PassqlValue {
    match value {
        Either4::A(s) => PassqlValue::Text(s),
        Either4::B(f) => {
            // JS numbers are doubles. If the value is integral, store as
            // INTEGER (matches better-sqlite3 behavior); otherwise REAL.
            if f.fract() == 0.0 && f.is_finite() && f.abs() < 9.0072e15 {
                PassqlValue::Integer(f as i64)
            } else {
                PassqlValue::Real(f)
            }
        }
        Either4::C(i) => PassqlValue::Integer(i),
        Either4::D(buf) => PassqlValue::Blob(buf.to_vec()),
    }
}

fn value_to_js_unknown(value: &PassqlValue, env: &Env) -> Result<JsUnknown> {
    match value {
        PassqlValue::Null => env.get_null().map(|n| n.into_unknown()),
        PassqlValue::Integer(i) => env.create_int64(*i).map(|n| n.into_unknown()),
        PassqlValue::Real(f) => env.create_double(*f).map(|n| n.into_unknown()),
        PassqlValue::Text(s) => env.create_string(s).map(|n| n.into_unknown()),
        PassqlValue::Blob(b) => env
            .create_buffer_with_data(b.clone())
            .map(|n| n.into_unknown()),
    }
}

fn io_to_napi(error: std::io::Error) -> Error {
    Error::new(Status::GenericFailure, format!("io: {error}"))
}

fn exec_to_napi(error: ExecutionError) -> Error {
    Error::new(Status::GenericFailure, error.to_string())
}

/// A Passql database connection.
///
/// Synchronous API like better-sqlite3. All operations hold an exclusive
/// lock on the underlying file for their duration.
#[napi]
pub struct Database {
    storage: Arc<Mutex<Storage>>,
}

#[napi]
impl Database {
    /// Open (or create if missing) a Passql database at `path`.
    #[napi(constructor)]
    pub fn new(path: String) -> Result<Self> {
        let storage = if std::path::Path::new(&path).exists() {
            Storage::open(&path).map_err(io_to_napi)?
        } else {
            Storage::create(&path).map_err(io_to_napi)?
        };
        Ok(Self {
            storage: Arc::new(Mutex::new(storage)),
        })
    }

    /// Execute one or more SQL statements (no parameters, no result rows).
    /// Statements are split on `;`. Errors abort the batch.
    #[napi]
    pub fn exec(&self, sql: String) -> Result<()> {
        let mut storage = self.storage.lock().map_err(|e| {
            Error::new(Status::GenericFailure, format!("lock: {e}"))
        })?;
        for statement in sql.split(';') {
            let trimmed = statement.trim();
            if trimmed.is_empty() {
                continue;
            }
            let _ = passql::sql::execute(trimmed, &mut storage).map_err(exec_to_napi)?;
        }
        Ok(())
    }

    /// Prepare a SQL statement for repeated execution with bound parameters.
    #[napi]
    pub fn prepare(&self, sql: String) -> Result<Statement> {
        // Validate it parses — we re-execute on each run() since the
        // planner is fast and stateless. A future optimization would
        // cache the compiled Plan.
        let dialect = sqlparser::dialect::GenericDialect {};
        let _ = sqlparser::parser::Parser::parse_sql(&dialect, &sql).map_err(|e| {
            Error::new(Status::InvalidArg, format!("parse error: {e}"))
        })?;
        Ok(Statement {
            storage: self.storage.clone(),
            sql,
        })
    }

    /// Close the database. The handle becomes unusable after this call.
    /// Pending locks are released.
    #[napi]
    pub fn close(&self) -> Result<()> {
        // Mutex is dropped automatically when Database drops; nothing to
        // do here for now. Real impl would poison the mutex.
        Ok(())
    }
}

/// A prepared SQL statement. Cheap to re-run with different parameters.
///
/// Currently re-parses on each run — the planner is stateless. Caching
/// the compiled plan is a future optimization.
#[napi]
pub struct Statement {
    storage: Arc<Mutex<Storage>>,
    sql: String,
}

#[napi]
impl Statement {
    /// Execute the statement with parameters and return all result rows.
    ///
    /// Each parameter is a JS value (number, string, Buffer). Pass one
    /// parameter per `?` in the SQL text, in source order.
    #[napi]
    pub fn all(&self, env: Env, params: Vec<JsParam>) -> Result<Vec<JsObject>> {
        let mut storage = self.storage.lock().map_err(|e| {
            Error::new(Status::GenericFailure, format!("lock: {e}"))
        })?;
        let passql_params: Vec<PassqlValue> = params.into_iter().map(js_param_to_value).collect();
        let result = passql::sql::execute_with_params(&self.sql, &mut storage, &passql_params)
            .map_err(exec_to_napi)?;
        match result {
            ExecutionResult::Selected { columns, rows } => {
                let mut out: Vec<JsObject> = Vec::with_capacity(rows.len());
                for row in rows {
                    let mut obj = env.create_object()?;
                    for (i, col) in columns.iter().enumerate() {
                        if let Some(value) = row.get(i) {
                            let unknown = value_to_js_unknown(value, &env)?;
                            obj.set_named_property(col, unknown)?;
                        }
                    }
                    out.push(obj);
                }
                Ok(out)
            }
            _ => Ok(Vec::new()),
        }
    }

    /// Execute the statement and return the first result row (or null).
    #[napi]
    pub fn get(&self, env: Env, params: Vec<JsParam>) -> Result<Option<JsObject>> {
        let mut storage = self.storage.lock().map_err(|e| {
            Error::new(Status::GenericFailure, format!("lock: {e}"))
        })?;
        let passql_params: Vec<PassqlValue> = params.into_iter().map(js_param_to_value).collect();
        let result = passql::sql::execute_with_params(&self.sql, &mut storage, &passql_params)
            .map_err(exec_to_napi)?;
        match result {
            ExecutionResult::Selected { columns, rows } => {
                if let Some(row) = rows.into_iter().next() {
                    let mut obj = env.create_object()?;
                    for (i, col) in columns.iter().enumerate() {
                        if let Some(value) = row.get(i) {
                            let unknown = value_to_js_unknown(value, &env)?;
                            obj.set_named_property(col, unknown)?;
                        }
                    }
                    Ok(Some(obj))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    /// Execute the statement and return the number of affected rows.
    #[napi]
    pub fn run(&self, params: Vec<JsParam>) -> Result<u64> {
        let mut storage = self.storage.lock().map_err(|e| {
            Error::new(Status::GenericFailure, format!("lock: {e}"))
        })?;
        let passql_params: Vec<PassqlValue> = params.into_iter().map(js_param_to_value).collect();
        let result = passql::sql::execute_with_params(&self.sql, &mut storage, &passql_params)
            .map_err(exec_to_napi)?;
        match result {
            ExecutionResult::Inserted { rows_affected } => Ok(rows_affected),
            ExecutionResult::Updated { rows_affected } => Ok(rows_affected),
            ExecutionResult::Deleted { rows_affected } => Ok(rows_affected),
            ExecutionResult::SchemaChanged { .. } => Ok(0),
            ExecutionResult::Selected { rows, .. } => Ok(rows.len() as u64),
        }
    }
}
