//! SQL execution layer.
//!
//! Sits on top of the v4 KV store and translates SQL statements into key-
//! prefix-encoded rows. Tables are stored under the `t:<table>:<rowid>` key
//! prefix; the schema catalog lives under `__schema__:<table>`.
//!
//! ## Current coverage
//!
//! - `CREATE TABLE <name> (<col> <type>, ...)` (INTEGER, TEXT, BLOB, REAL)
//! - `INSERT INTO <name> (<cols>, ...) VALUES (<vals>, ...)`
//! - `SELECT <cols> | * FROM <name> [WHERE <pred>] [ORDER BY <col>] [LIMIT n]`
//! - `UPDATE <name> SET <col>=<val>, ... WHERE <pred>`
//! - `DELETE FROM <name> WHERE <pred>`
//! - Parameter binding via `?` placeholders
//!
//! ## Not yet supported
//!
//! Joins, aggregates, GROUP BY, HAVING, subqueries, CTEs, window functions,
//! schema migrations, transactions across multiple statements (each
//! statement commits independently for now).

pub mod executor;
pub mod plan;
pub mod schema;
pub mod types;

pub use executor::{ExecutionResult, Row, execute, execute_with_params};
pub use plan::{PlanError, PlanErrorKind, ScalarExpr};
pub use schema::{Column, ColumnType, Schema, SchemaError, TableCatalog};
pub use types::{Value, ValueError};
