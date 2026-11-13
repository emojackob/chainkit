# Rust indexer architecture and coverage

Chainkit uses one source repository with independently consumable Rust libraries and a separately versioned executable. The CLI composes the libraries; RPC, decoding, indexing, and query logic do not live exclusively in the binary. All application code in this execution path is Rust. SQLite is bundled as an embedded native dependency; neither a Python runtime nor a separate database service is required.

## Library boundaries

| Package | Responsibility |
|---|---|
| chainkit-evm | Read-only JSON-RPC acquisition, endpoint admission, identity verification, retries, cooldowns, and sanitized health |
| chainkit-abi | Manual ABI validation, bounded Sourcify/Etherscan discovery, successful-lookup caching, and dynamic decoding |
| chainkit-indexer | SQLite schema and transactions, project/contract registrations, historical scans, reorg recovery, ABI bindings, and evidence queries |
| chainkit-api | HTTP routes, OpenAPI discovery, structured result envelopes, and a read-only stdio MCP adapter |
| chainkit-indexer-cli | Configuration, CLI commands, polling service, shutdown, and binary packaging entry point |

Dependencies flow toward the reusable libraries. The Rust executable never shells out to the Python implementation. The two implementations use separate database schemas; there is no implicit migration or data sharing between them.

## RPC operations

Each chain has a shared RPC pool with distinct configured HTTP(S) endpoints. Clones share connections, health, admission limits, and chain identity. Selection spreads requests across available endpoints and favors endpoints with spare capacity. Configure concurrency, requests per second, per-request timeout, total operation deadline, maximum attempts, and response size bounds.

Transient transport failures, timeouts, rate limits, and server failures receive bounded retries with exponential backoff and jitter. Rate-limited endpoints honor Retry-After cooldowns while eligible peers remain available. Unsupported methods are tracked per endpoint, so a non-tracing node does not prevent a tracing peer from serving a full-mode request. Wrong-chain nodes are quarantined. Permanent RPC errors are surfaced rather than retried indefinitely. Only explicitly allowed read methods are accepted; the pool cannot submit transactions.

Identity checks compare chain ID and genesis hash. An explicit expected genesis hash is recommended for operator-controlled chains and forks. Endpoint identifiers and safe error categories are exposed; private URLs, credentials, and raw provider error messages are excluded from public diagnostics. Availability checks and recovery occur through request admission and identity refresh rather than a claim of continuous background probing.

Load balancing changes request routing, not evidence validity. The scanner must check block hashes, parent continuity, receipts, and trace association before committing. A successful HTTP response alone is not evidence of complete history.

## Registrations and evidence

A project contains independent registrations identified by chain ID and contract address. Each registration controls its starting block, coverage mode, pause state, ABI history, and progress. Manage-token digests are persisted; plaintext tokens are returned only when created or rotated. Store is a trusted in-process API; network adapters must authorize mutations.

The initial schema isolates persisted evidence per registration. A scan round can reuse fetched blocks, receipts, and traces across registrations. Persistent cross-project acquisition deduplication and distributed task scheduling remain future extensions. One SQLite writer and bounded scanning are deliberate initial constraints; this is not a distributed indexer.

Commit each complete block atomically with its evidence and next-block cursor. Do not advance after partial acquisition, inconsistent hashes, unsupported required traces, or decoding-independent transport failures. Preserve raw data when ABI decoding is unknown or ambiguous. On reorgs, invalidate the old branch and resume from a common ancestor in the configured window; a deeper reorg requires explicit recovery. Cursor and task revisions prevent stale pagination or work from silently mixing snapshots.

## Meaning of coverage

| Mode | Covered relationships | Required data |
|---|---|---|
| basic | Direct calls including failures and eventless calls, top-level creation, and events emitted by the target | Complete block transactions and receipts |
| full | basic plus target involvement in block-level callTracer frames, including internal calls, creation, proxy execution, and reverted calls | Historical debug_traceBlockByNumber with callTracer, or debug_traceTransaction for every transaction in each complete block |

When block tracing is unsupported, full mode can trace every transaction in the complete block before relationship filtering. Missing even one required transaction trace stops the block. Empty blocks need no trace request. This preserves full discovery coverage and is not a downgrade to basic mode. A full-mode registration reports unsupported tracing if neither strategy can complete the block. Historical completeness is conditional on the configured start, committed intervals, node capabilities, and trustworthy provider responses. Missing query rows do not prove that a transaction does not exist onchain.

Neither mode establishes all external token relationships, system-level balance changes, storage diffs, or chain-specific execution semantics. These need explicit adapters with their own coverage declarations. Related transaction receipts retain logs from other emitters as contextual evidence; those logs must not be decoded using the target's ABI.

## ABI lifecycle

An address must always be paired with a chain. A manually supplied ABI may be an array or a compiler artifact containing an abi array. Automatic discovery uses bounded provider requests and caches successful lookups by chain and address. Provider failures do not establish that a contract is unverified everywhere. Supply an empty ABI explicitly when collecting raw evidence without a known ABI.

ABI content hashes and effective blocks identify decoding inputs. Updating a binding redecodes stored evidence rather than refetching the chain. Integer values remain decimal strings. Indexed dynamic arguments retain their hashes, and selector/topic collisions retain decoding candidates.

The initial implementation uses operator-supplied, block-granular historical bindings. It does not automatically reconstruct proxy upgrade history, beacon or diamond routing, or same-block implementation changes. Fetching an ABI from a verified-contract provider does not prove it matches every historical deployment or execution context. Do not interpret a current implementation ABI as historical proof.

## AI interfaces

The HTTP and MCP adapters call the same Store query methods. Responses carry schema_version, data, coverage, warnings, and pagination metadata. Consumers should inspect coverage first, then retrieve transaction context and follow cursors. Cite block hashes, transaction/log positions, and trace paths as evidence, and distinguish raw facts from ABI interpretations.

GET queries are public in the initial adapter, matching the existing Python behavior. Management POST requests require a project manage token. Stdio MCP is intended for a trusted local caller and exposes read-only tools; it is not an authenticated remote MCP service. Bind HTTP to localhost by default. Expose it remotely only through an explicitly configured deployment boundary appropriate to the intended data visibility.

## Distribution and validation

Linux release packaging targets x86_64 and aarch64 and includes binary archives, Debian packages, Arch packages, checksums, and a systemd unit. Package build scripts and CI are separate from the runtime binary. Generating a .deb or .pkg.tar.zst does not enroll the application in distribution repositories.

APT and pacman repository generation requires hosted artifacts, repository metadata, and signing keys controlled by the operator. Keys and private RPC configuration are not source artifacts. AUR publication and live software repositories are separate publishing steps; checked-in packaging files do not claim those services already exist.

Deterministic tests should use real loopback HTTP fixtures and isolated temporary databases. Validate failures as well as success: endpoint failover, wrong identities, response bounds, unsupported traces, rollback, reorgs, restart, project isolation, ABI redecoding, and snapshot pagination. Live-node comparisons, large historical workloads, resource benchmarks, and installation/upgrade tests define additional release readiness and must be reported separately from unit test success.
