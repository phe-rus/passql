//! Passql — an embedded, edge-native database engine.
//!
//! Passql aims to be a single embedded database with a dependable storage
//! core and two interfaces: practical SQL for relational data, and an
//! ergonomic document API for structured data. Both interfaces share one
//! transaction manager, one page allocator, and one persistent index.
//!
//! ## Current state
//!
//! The storage core (v4) is implemented: a copy-on-write B+ tree with
//! checksummed pages, atomic root publication via alternating commit slots,
//! snapshot readers that don't block writers, and a transactional API
//! supporting insert, update, delete, point lookup, and ordered range scans.
//!
//! SQL parsing/execution, the document API, TypeScript bindings, and WASM
//! adapters are not yet implemented. See [`ROADMAP.md`] for the phased plan.
//!
//! ## Example
//!
//! ```no_run
//! use passql::Storage;
//!
//! let mut database = Storage::create("example.psl")?;
//! let mut transaction = database.begin()?;
//! transaction.set("user:1", b"Alice")?;
//! assert_eq!(transaction.get("user:1")?, Some(b"Alice".to_vec()));
//! transaction.commit()?;
//! let value = database.get("user:1")?;
//! # Ok::<(), std::io::Error>(())
//! ```

pub mod btree;
pub mod document;
pub mod format;
pub mod sql;
pub mod storage;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use btree::PAGE_SIZE;
pub use document::{Collection, Filter, Operator as FilterOperator, Path as DocumentPath};
pub use format::{DatabaseFormat, DatabaseInfo, VerificationReport};
pub use sql::{ExecutionResult, Row, Value, execute, execute_with_params};
pub use storage::{IndexReader, Storage, Transaction};
