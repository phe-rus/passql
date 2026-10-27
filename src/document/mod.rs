//! Document collection layer.
//!
//! A document is a JSON value stored under key `c:<collection>:<id>`.
//! Collections have a metadata entry under `__coll__:<name>` storing their
//! schema (currently just the id counter).
//!
//! ## Operations
//!
//! - `insert` — store a new document, auto-assign id
//! - `get` — fetch by id
//! - `find` — scan with a JSON-shaped filter predicate
//! - `update` — patch fields (deep merge for objects)
//! - `delete` — remove by id
//! - `list` — enumerate ids
//!
//! Documents share the same v4 KV store, transactions, and snapshot readers
//! as SQL tables. A future stage will add field-level indexes that commit
//! atomically with their documents.

pub mod collection;
pub mod filter;
pub mod path;

pub use collection::{Collection, DocumentId, DocumentResult};
pub use filter::{Filter, Operator};
pub use path::Path;
