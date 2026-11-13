//! Embeddable EVM indexing with isolated registrations and durable evidence.
//!
//! Store is a trusted in-process API. Network adapters must authorize management
//! operations before calling it. Queries expose raw evidence and coverage rather
//! than interpreting missing data as proof of onchain absence.
mod scanner;
mod store;

pub use scanner::{scan_once, ScanOptions};
use serde::Serialize;
pub use store::{Query, Store};

#[derive(Debug, Clone, Serialize)]
pub struct IndexerError {
    pub category: String,
    pub message: String,
}

impl IndexerError {
    pub fn new(category: &str, message: &str) -> Self {
        Self {
            category: category.into(),
            message: message.into(),
        }
    }
    pub fn status(&self) -> u16 {
        match self.category.as_str() {
            "invalid_input" | "invalid_cursor" | "invalid_abi" => 400,
            "forbidden" => 403,
            "not_found" => 404,
            "conflict"
            | "snapshot_invalidated"
            | "deep_reorg"
            | "index_inconsistent"
            | "stale_task"
            | "identity" => 409,
            "unsupported" => 422,
            "storage" => 500,
            _ => 502,
        }
    }
}
impl std::fmt::Display for IndexerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.category, self.message)
    }
}
impl std::error::Error for IndexerError {}
impl From<rusqlite::Error> for IndexerError {
    fn from(_: rusqlite::Error) -> Self {
        Self::new("storage", "SQLite operation failed")
    }
}
impl From<serde_json::Error> for IndexerError {
    fn from(_: serde_json::Error) -> Self {
        Self::new("storage", "Stored evidence is malformed")
    }
}
impl From<chainkit_evm::RpcError> for IndexerError {
    fn from(value: chainkit_evm::RpcError) -> Self {
        Self::new(
            &value.category,
            "RPC operation failed; incomplete block was not committed",
        )
    }
}
type Result<T> = std::result::Result<T, IndexerError>;
