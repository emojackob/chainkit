# Rust indexer validation

Validation date: 2026-11-13. Application version: 0.1.0. Native target: x86_64-unknown-linux-gnu. Rust toolchain used: 1.99.1. This is implementation validation, not a production performance certification.

## Automated checks

`make check` passes Rust formatting, Clippy with warnings denied, all 54 Rust tests, Python syntax checks, and the existing 38 Python tests. The Rust tests exercise loopback HTTP providers and temporary SQLite databases rather than relying solely on mocked function calls.

Coverage includes endpoint load distribution, concurrency and rate limits, retry budgets and Retry-After across concurrent callers, identity quarantine, unsupported-method fallback, bounded responses, ABI provider fallback/cache, decoding precision and ambiguity, multi-project isolation, token rotation, restart, reorg recovery, stale-worker fencing, signed pagination, consistent database read snapshots, ABI provenance and offline redecoding, HTTP authorization, OpenAPI schemas, and stdio MCP initialization/tool calls.

## Local EVM execution

The release executable was exercised against an isolated Anvil 1.5.1 node with locally deployed EVM contracts. Two independent project registrations each discovered four related transactions: target creation, a successful direct call, a reverted direct call, and an indirect call through a router. Both emitted target events were indexed and decoded, including an integer larger than the signed 256-bit boundary.

A separate lookup of one verified public Ethereum contract also succeeded against the live Sourcify service. This validates ABI discovery only, not public-chain historical indexing.

The local test also verified automatic ABI retrieval through a local provider fixture, manual ABI input, persisted ABI sources, ABI redecoding without RPC requests, idempotent rescanning, injected HTTP 429 failover between two endpoints, HTTP authorization and cursor pagination, stdio MCP queries while the indexing daemon was running, and graceful service shutdown.

Anvil did not support debug_traceBlockByNumber in this environment. This exposed and validated the full-mode fallback that traces every transaction in a complete block using debug_traceTransaction. Required trace failures still stop block commits; the fallback never selects only already-related transactions.

## Linux artifacts

The native release executable and Debian/Arch package archives were built locally. Debian package structure and metadata, Arch metadata, extracted executable startup, and runtime library requirements were inspected. SQLite is embedded rather than a dynamic database dependency. Public release builds remap the builder's home, Cargo cache, and worktree paths.

The APT/pacman repository generation scripts were separately tested using disposable signing keys and isolated temporary directories. Checks covered package/database signatures, architecture-specific indexes, manifest checksums and sizes, repeated metadata generation, semantic version ordering, and glibc ABI requirements. No user signing keyring, host installation, or system service was modified by these checks.

## Remaining release validation

- The aarch64 cross-build is configured in CI but has not been executed locally.
- Native apt/pacman installation, upgrade, removal, and service lifecycle tests remain additional platform checks. CI includes a Debian package installation check; it has not been run on GitHub yet.
- No public-chain historical range comparison, archive-node cost measurement, large-data benchmark, or sustained multi-project load test has been completed.
- No GitHub release, AUR entry, signed public APT repository, or signed public pacman repository has been published by this work.
- Historical proxy resolution, external-token-only relationships, typed ABI argument filters, persistent shared evidence, and trace pagination remain outside the initial implementation. See the architecture and crate documentation for limits.
