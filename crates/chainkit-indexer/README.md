# chainkit-indexer

Embeddable Rust EVM indexing with multi-project registrations, SQLite persistence, and evidence queries. Runtime scanning uses `chainkit-evm`; decoding uses `chainkit-abi`. No Python runtime or external database service is required. The bundled SQLite engine is written in C.

## Integration

```rust,no_run
use chainkit_evm::{RpcConfig, RpcPool};
use chainkit_indexer::{scan_once, Query, ScanOptions, Store};
use serde_json::json;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let store = Store::open("index.sqlite")?;
let identity = store.create_project("demo", "Demo")?;
// Save identity["tokens"]["manage"] privately; creation returns it only once.
store.register("demo", 31337,
    "0x1111111111111111111111111111111111111111", json!([]), 0, "full")?;
let rpc = RpcPool::new(31337,
    vec!["http://127.0.0.1:8545".into()], RpcConfig::default())?;
let result = scan_once(&store, &rpc, &ScanOptions::default()).await?;
// Inspect result["errors"] even when the scan round returns Ok.
let page = store.query("demo", 31337,
    "0x1111111111111111111111111111111111111111", "events", &Query::default())?;
# Ok(())
# }
```

`Store` is a trusted in-process API. Network adapters must enforce management authorization. `authorize(project, token)` checks the project's manage token; `rotate_key(project)` replaces it. Tokens are stored as digests, never plaintext.

`register_with_source` and `update_abi_with_source` preserve ABI provenance as `manual`, `sourcify`, or `etherscan`. The shorter methods default to `manual`. ABI history preserves all revisions. ABI updates redecode retained evidence transactionally without calling RPC.

## Correctness and persistence

- SQLite WAL, full synchronous durability, foreign keys, transactional schema initialization, and explicit application/schema identifiers.
- Project/chain/address registration identity, with persisted chain-level genesis identity across restarts and new projects.
- Atomic per-block evidence and cursor commits, checked against the task revision and expected cursor to fence stale fetches.
- Canonical-branch tracking, bounded common-ancestor recovery, and explicit deep-reorg failures. Old branch evidence remains stored and is excluded from normal queries.
- Basic mode covers emitted events, direct calls including eventless and failed transactions, and top-level creation. Full mode adds callTracer relations through block tracing or, only when that method is unsupported, a complete trace sweep of every transaction in the block. Empty blocks need no trace request. Missing transaction traces stop the block; full mode never silently downgrades to basic.
- Block/transaction/receipt/log inclusion validation and a final canonical-hash check before commit. Single-block log overflow falls back to receipt reads.
- ABI, cursor, pause, and backfill changes invalidate prior snapshot cursors. Pagination is HMAC-signed and bound to the registration, resource, filters, revision, and fixed upper block.
- Query/context reads use a SQLite read transaction so metadata and evidence share one snapshot even across database connections.

## Responses and limits

Queries return `schema_version`, `data`, `coverage`, `warnings`, and `next_cursor`. Ranges include `from_block` and exclude `to_block`. Pages default to 50 items and accept limits of 1..200. Follow `next_cursor` until null, retaining the same filters.

`context` returns transaction evidence and paginated receipt logs. Embedded receipts omit the `logs` array and expose `receipt_log_count`, so log pagination does not duplicate an unbounded log list. Query pages have a 16 MiB evidence budget and may be shortened with a continuation cursor. Context responses are limited to 32 MiB; oversized individual evidence returns `too_large` rather than a partial object.

Scan rounds reuse up to 64 MiB of acquisition data between registrations. Each registration receives its own bounded block budget. A registration failure is persisted and returned in the round's `errors` array while other registrations continue. Initial chain-head/genesis acquisition failures return an error and record it against unchanged task snapshots.

## Current boundaries

Persisted evidence is isolated per registration; cross-project reuse is currently an in-memory acquisition optimization. External token-only relationships and chain-specific system transactions are not discovered automatically. ABI bindings use block granularity and do not establish historical proxy implementations. Trace trees have frame/depth budgets but no trace pagination. Query filters support transaction hash, sender, status, signature, topic0, and block ranges; typed argument filters are not yet implemented in this Rust crate.

History completeness depends on the configured RPC providers. Missing indexed evidence never proves onchain absence. Full-mode nodes must support historical block traces or historical callTracer traces for every transaction in each scanned block. Tracing only previously discovered candidate transactions is insufficient and is never used as a completeness shortcut.
