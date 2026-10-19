//! Passql
//! An embedded, edge-native database engine.

pub mod kv;
pub mod record;
pub mod storage;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use record::{MAX_RECORD_SIZE, Record};
pub use storage::{PAGE_SIZE, Storage};
