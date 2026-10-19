//! Passql
//! An embedded, edge-native database engine.

pub mod kv;
pub mod reader;
pub mod record;
pub mod storage;
pub mod storage_v3;
pub mod transaction;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use reader::DatabaseReader;
pub use record::{MAX_RECORD_SIZE, Record};
pub use storage::{PAGE_SIZE, Storage};
pub use transaction::Transaction;
