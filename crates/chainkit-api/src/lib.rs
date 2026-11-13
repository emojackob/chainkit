//! Reusable HTTP, OpenAPI, and read-only stdio MCP adapters.

pub mod http;
pub mod mcp;
pub mod openapi;

use chainkit_abi::AbiResolver;
use chainkit_indexer::Store;
use std::collections::BTreeSet;

/// Public adapter state. Configuration is operator supplied, never query input.
#[derive(Clone)]
pub struct ApiState {
    pub store: Store,
    pub resolver: AbiResolver,
    pub chain_ids: BTreeSet<u64>,
}

/// Preserve store evidence envelopes and wrap plain values consistently.
pub fn envelope(value: serde_json::Value) -> serde_json::Value {
    if value.get("schema_version").is_some() {
        value
    } else {
        serde_json::json!({"schema_version":"1", "data":value, "warnings":[]})
    }
}
