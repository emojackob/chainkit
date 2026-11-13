# chainkit-evm

Reusable, read-only EVM JSON-RPC acquisition for asynchronous Rust applications.
`RpcPool` clones share HTTP connections, endpoint limits, identity pins, and health
state. Endpoints come from operator configuration, not contract registration.

## Behavior

- Select the least busy eligible endpoint and rotate ties to spread requests across
  providers. Report moving-average latency and request/outcome counters.
- Bound concurrent operations per endpoint. Cancellation releases admission slots.
- Optionally limit every HTTP request per endpoint, including chain identity probes.
- Cache chain ID and genesis validation with a configurable TTL. A mismatched node
  is quarantined, and another eligible endpoint is tried within the attempt budget.
- Pin a known genesis with `expected_genesis_hash`. Without a pin, the first valid
  endpoint establishes the expected genesis for this pool.
- Retry transient transport faults, timeouts, HTTP 408/429/5xx, and recognized
  transient JSON-RPC server errors with bounded exponential backoff and jitter.
- Honor HTTP 429/503 `Retry-After` seconds or HTTP dates using endpoint cooldown.
  Embargoes survive caller deadlines. Values above one day are capped at one day.
- Try eligible peers when a concrete historical block read returns null, without
  quarantining a node globally or treating unknown transaction receipts as missing
  block history. Each missing-history endpoint is tried at most once per operation.
- Cache unsupported methods per endpoint and fail over to a capable peer. For
  example, a node without block tracing does not disable ordinary block reads.
- Do not retry execution reverts, invalid parameters, malformed envelopes, oversized
  bodies, or other permanent request failures. Reject non-allowlisted methods before
  opening a connection; transaction submission is not supported.
- Bound both individual requests and the complete operation, including admission,
  rate-limit waits, identity probes, backoff, and response reads. Cap response bytes
  while reading, even if the provider omits Content-Length.
- Do not expose provider URLs, credentials, or raw error text through errors, Debug,
  or health snapshots. HTTP redirects are disabled.

The pool verifies network identity, not the truth of provider data or agreement at
an arbitrary block height. The caller must validate block hashes, parent continuity,
receipts, canonicality, and historical coverage before committing indexed data.
A healthy current-state endpoint is not necessarily able to serve historical traces.

## Configuration

All fields are optional in deserialized configuration and use these defaults:

```toml
max_attempts = 6
timeout_ms = 15000
total_timeout_ms = 60000
max_response_bytes = 33554432
base_backoff_ms = 100
max_backoff_ms = 5000
cooldown_ms = 1000
max_in_flight_per_endpoint = 4
identity_ttl_ms = 300000
# requests_per_second = 10
# expected_genesis_hash = "0x..." # Exactly 32 bytes
```

`max_attempts` limits endpoint attempts, including failed identity checks. The
request timeout includes that request's rate-limit wait. Admission can wait for
busy endpoints or cooldown expiry, but never beyond the operation's total budget.
Identity probes are serialized per endpoint to avoid duplicate cold-start probes.

## Public API

- `RpcPool::new(chain_id, urls, config)` validates configuration without network I/O.
- `pool.call(method, params).await` returns `serde_json::Value` or a sanitized
  `RpcError` with a public `category` field and `category()` accessor.
- `pool.chain_id()` returns the configured chain ID.
- `pool.genesis_hash()` returns the configured or validated genesis pin, if established.
- `pool.health()` returns serializable health snapshots identified as `rpc-1`, etc.
- `normalize_address`, `normalize_hash`, and `quantity` validate EVM identities and
  canonical hexadecimal quantities. `quantity` accepts values representable in u64.

Run `cargo test -p chainkit-evm` for local mock-HTTP integration tests. They require
no external node, provider credentials, or public network access.
