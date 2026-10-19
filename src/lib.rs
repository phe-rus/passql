//! passql
//! A modern, embedded and edge-native database engine.

pub mod storage;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use storage::{PAGE_SIZE, Storage};
