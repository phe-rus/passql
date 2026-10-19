//! Passql
//! An embedded, edge-native database engine.

mod btree;
pub mod format;
pub mod kv;
pub mod reader;
pub mod record;
pub mod slotted;
pub mod storage;
pub mod storage_v3;
pub mod storage_v4;
pub mod transaction;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use format::{DatabaseFormat, DatabaseInfo, VerificationReport};
pub use reader::DatabaseReader;
pub use record::{MAX_RECORD_SIZE, Record};
pub use slotted::SlottedPage;
pub use storage::{PAGE_SIZE, Storage};
pub use storage_v4::{IndexReader as IndexReaderV4, StorageV4, Transaction as TransactionV4};
pub use transaction::Transaction;
