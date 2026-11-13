# Contract indexing and transaction diagnostics

Status: long-term design for a general-purpose contract indexing library. See the [usage guide](README.md) for implemented capabilities and actual interfaces. This document includes unimplemented goals such as shared acquisition across projects, task leases, and precise proxy bindings.

Goal: after an application registers a contract address and ABI, continuously index transactions and events within configured chains, RPC endpoints, and historical ranges. Build database indexes that let programs and AI clients investigate transaction issues using traceable evidence.

## Integration and coverage

Addresses and ABIs are contract-level inputs. The runtime must also supply chain_id, RPC endpoints, a database, and a historical scanning policy. Identical addresses on different chains represent distinct objects. The initial implementation targets EVM chains.

Registration accepts an ABI array or the ABI from a compiled artifact, validates and normalizes it, and computes a content hash. Users do not need to configure individual events. Fetch all logs by emitter address rather than only topics known to the ABI. Retain raw data for unknown events.

Prefer an explicit deployment block as the historical start. When users request complete history, start from genesis or verified deployment information. Code present at the current height does not prove the deployment height, and scanning only after startup must not be described as complete history. Record gaps when historical data is unavailable.

Distinguish related activity by its evidence:

| Type | Discovery method | Coverage requirements |
|---|---|---|
| Contract-emitted events | Scan logs by emitter address | RPC provides logs for the requested range |
| Direct calls, including eventless and failed transactions | Scan full block transactions and match to | Historical block transactions and receipts are available |
| Contract creation transactions | Read contractAddress from creation receipts | The creation block has been scanned |
| Internal calls, internal creation, proxy execution, and internal native-asset transfers | Scan block call traces and match call frames | The node supports historical traces and the adapter supports the trace type |
| Transfer events in external token contracts involving the target address | Optional, explicit token-event relationship rules | Registered token addresses or standard adapters and a defined scanning range |

Tracing only discovered transactions cannot establish that other internal calls were not missed. Internal-call coverage requires block-level traces or an external index with demonstrable coverage. Ordinary logs cannot recover reverted events. An ABI cannot automatically establish every business relationship, source-code line, or causal relationship between fund movements.

The base coverage mode includes events, direct transactions, and creation receipts; the enhanced mode adds block-level traces. Each mode declares its own coverage. An internal-call mode must not silently downgrade while claiming complete indexing. See the usage guide for the current mode names and default.

## Module ownership

- packages/chain: RPC transport, error classification, node capability detection and failover, block/transaction/receipt/trace normalization, and dynamic ABI decoding.
- packages/backend/contract-indexer: registration, scan scheduling, persistence, reorg recovery, coverage evidence, and query services.
- packages/tooling: future registration, backfill, redecoding, status, and diagnostic commands for application Makefiles.
- packages/deploy: future deployment-block and ABI-version exports as registration input. The indexer does not require contracts to be deployed through Chainkit.

The initial implementation uses the Python standard library and SQLite, separating chain acquisition/decoding, backend persistence/scheduling, and HTTP adapters. Consumers can embed the library or run it independently. Evaluate acquisition-engine and PostgreSQL adapters using data volumes and performance measurements.

## Indexing workflow and correctness

1. Validate the RPC chain_id and chain identity. Probe each node for logs, full blocks, receipts, historical traces, and available ranges. Do not expose RPC URL credentials in query responses.
2. Maintain persistent tasks and coverage ranges separately for events, direct transactions, and traces. Share chain-block scans across contracts to avoid duplicate acquisition.
3. Use half-open ranges [from, to). next_block explicitly identifies the next starting point; RPC end bounds become to - 1. Calculate and test confirmation depth and safe endpoints in one place.
4. Fetch complete batches outside database transactions, compare blockHash on receipts and logs, and verify parent-hash continuity. Retain raw data and errors for unknown, malformed, or undecodable objects.
5. Split batches adaptively according to range and filter limits. If a single block exceeds log limits, fall back to block receipts or individual transaction receipts. If that also fails, stop the range and record the reason rather than skipping blocks.
6. Atomically commit raw data, relationships, task versions, coverage ranges, and cursors in small batches. Stage large blocks in segments and publish a complete queryable batch only after every segment succeeds, so partial results cannot be mistaken for complete data.
7. Periodically verify canonical-chain anchors. On a reorg, find the common ancestor, atomically invalidate the old branch's canonical status, relationships, and coverage, then rescan the new branch. Retain old raw evidence for auditing.
8. If no common ancestor exists in the retained window, pause affected tasks and report a deep reorg. Rewinding a fixed number of blocks is insufficient to establish correctness. Distinguish confirmation depth from finalized status.
9. Decode ABIs asynchronously. Decoding failures must not discard raw data or block acquisition. Report acquisition and decoding progress separately.

Use leases and fencing tokens for tasks, validating task versions at commit time so cursor resets, configuration changes, and stale workers cannot overwrite newer state. Retries have backoff, jitter, attempt/time budgets, and cancellation. Only transient network errors trigger node failover; execution reverts and unsupported capabilities require separate classifications.

## Logical data model and indexes

This table describes a logical model, not migration SQL. Isolate chain data by chain_id. Use consistent binary or normalized address/hash formats rather than filtering with lower(column).

| Entity | Identity and key fields | Query indexes |
|---|---|---|
| contracts | chain_id + address; starting block, mode, configuration version | Unique chain_id + address |
| abi_versions / bindings | ABI content hash; contract, effective execution position, source, verification status | Contract + effective block/transaction/call position |
| blocks | chain_id + block_hash; number, parent_hash, timestamp, canonical | Chain + height; unique canonical block per height |
| transactions | chain_id + tx_hash; from, to, input, value, nonce, type, raw object | Chain + hash; chain + from/to |
| tx_inclusions / receipts | Chain + block_hash + tx_hash; transaction position, execution status, gas, raw receipt | Chain + hash; block + transaction_index |
| logs | Chain + block_hash + log_index; tx_hash, emitter, topics, data | Chain + emitter/topic0 + block/transaction/log order; chain + tx_hash |
| traces | Chain + block_hash + tx_hash + trace_path; type, from, to, context/code address, input/output, error | Chain + from/to + block order; chain + tx_hash |
| contract_tx_relations | Contract, transaction inclusion, relationship type, evidence reference | Contract + relationship type + block/transaction order |
| decodings | Raw evidence reference + ABI hash + decoder version; signature, typed arguments, result/error | Evidence reference; full signature; decoding status |
| decoded_arguments | Decoding record, argument position/path, ABI type, normalized value | Contract/signature + argument path + typed value |
| scan_tasks / ranges | Contract or chain, scan type, range, next_block, anchor, lease, version, failure reason | Task status; target + type + start/end range |

Separate transaction identity from inclusion position so a transaction re-included after a reorg does not overwrite an old receipt. Canonical queries filter by block canonicality; old branches are excluded by default. Store raw objects separately from decodings to allow redecoding after ABI updates without refetching chain data.

Return amounts, uint256, int256, and other large onchain integers as decimal strings without floating-point conversion. Database indexes that compare magnitudes should use sortable fixed-width encodings or exact numeric adapter types rather than lexicographic decimal TEXT ordering. Preserve argument position, name, type, and path, including unnamed or duplicate names and nested tuple/array values.

Indexed dynamic types contain hashes only and must be marked unrecoverable. Preserve candidates and ambiguity for anonymous events, overloaded functions, and selector/topic collisions. For proxies, retain the log emitter and execution context and resolve implementation ABIs through historical bindings. Same-block upgrades need finer transaction/call positions. If only the current ABI is available, label historical decodings as candidates rather than claiming they match the historical implementation.

## Query contract for AI clients

The initial plan uses versioned REST and OpenAPI, with a thin MCP adapter later. MCP should call the same query service rather than introduce separate business logic. Exact transaction lookup does not require a vector database.

| Planned interface | Purpose |
|---|---|
| POST /v1/contracts | Register chain, address, ABI, historical start, and coverage mode |
| GET /v1/contracts/{chain_id}/{address}/coverage | Inspect coverage by type, gaps, synchronization lag, and capability limits |
| GET /v1/contracts/{chain_id}/{address}/transactions | Filter by range, relationship, sender, full function signature, and execution status |
| GET /v1/contracts/{chain_id}/{address}/events | Filter by full event signature, argument path/value, transaction, or range |
| GET /v1/transactions/{chain_id}/{tx_hash} | Transaction, inclusion position, receipt, and decoding |
| GET /v1/transactions/{chain_id}/{tx_hash}/context | Combined transaction evidence, all receipt logs, call tree, related contracts, ABI provenance, and coverage limits |
| POST /v1/contracts/{chain_id}/{address}/backfills | Asynchronously backfill a range and return a task ID |
| POST /v1/contracts/{chain_id}/{address}/redecode | Asynchronously redecode with a specified ABI version |

Responses provide stable schema_version, items/data, coverage, warnings, and pagination metadata. Coverage includes the requested range, coverage and gaps for each scan type, acquisition/decoding progress, snapshot block hash, finality, and provenance. Distinguish no matching records, unindexed data, unsupported capabilities, and transient failures. Without scan evidence, do not conclude that a transaction does not exist.

Sort pages by block_number, transaction_index, log_index or trace_path, and stable identity. Bind opaque cursors to filters and snapshots, with a fixed upper bound so new blocks do not cause missed records during pagination. Validate canonicality when querying. On a reorg, return an explicit snapshot_invalidated error requiring a new query. The default page size is 50 and the maximum is 200. Paginate context logs, traces, and raw objects separately, reporting truncation and continuation links.

Context is an evidence bundle and does not generate natural-language attribution by default. Each evidence reference can be retrieved again using stable transaction, log, or trace identities and carries block_hash, position, acquisition time, decoding version, and provenance. Distinguish facts, ABI decodings, and diagnostic hypotheses. Receipt status=0 establishes execution failure only. Revert reasons from the original transaction trace should be attributed to that trace. Historical eth_call replay must be labeled as simulation-based inference; latest-state simulation must not be presented as a historical conclusion.

Recommended AI workflow: inspect coverage, retrieve context by hash, then query timelines by event arguments or address. When data is insufficient, identify concrete gaps and available backfill actions. Treat contract-provided strings as untrusted data rather than tool instructions. Separate read access from registration/backfill management. Use parameterized queries, filter allowlists, and query budgets; do not expose arbitrary SQL or automatically send transactions.

## Implementation phases and acceptance criteria

1. Raw acquisition: registration, dynamic ABI support, event and full-block scans, SQLite persistence, cursors, and basic queries. Cover successful eventless calls, failed calls, unknown events, multiple events in one transaction, empty blocks, and the same address on multiple chains.
2. Correctness: common-ancestor recovery, historical gaps, task leases, snapshot pagination, and redecoding. Cover idempotent repeated backfills, interrupted-write recovery, lagging nodes, single-block log limits, ABI completion, shallow/deep reorgs, and transaction reinclusion.
3. AI queries: OpenAPI, typed argument filters, coverage, and context. Verify retrievable evidence, exact integers, correct handling of missing data, and continuation after truncation.
4. Trace enhancement: node adapters, block-level trace discovery, call trees, and historical proxy bindings. Cover indirect calls through external routers, delegatecall, internal creation, reverted child calls, unsupported tracing, and unavailable history.

Provide deterministic RPC-fixture integration tests for each phase. Before production use, compare against live nodes on explicit chains and ranges and record coverage and RPC costs. The usage guide defines the current validation scope. This design document does not substitute for unfinished acceptance work.

## Protocol references

- [Ethereum JSON-RPC](https://ethereum.org/developers/docs/apis/json-rpc/): acquisition interfaces for logs, full block transactions, and receipts.
- [Geth debug API](https://geth.ethereum.org/docs/interacting-with-geth/rpc/ns-debug): historical trace interfaces and execution-environment requirements.
- [Geth built-in tracers](https://geth.ethereum.org/docs/developers/evm-tracing/built-in-tracers): callTracer frames, errors, and return data.

These interfaces define data provenance. Probe actual node capabilities at runtime rather than inferring them from node names.
