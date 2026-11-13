# chain

Shared capabilities for onchain queries and transactions.

## Scope and boundaries

- Chain configuration, RPC access, contract reads and writes, transaction confirmation, and event queries.
- No dependency on UI or wallet frameworks.
- No custody of application keys or hardcoded application addresses or business rules.
- Reusable chain access for wallet, deploy, and application services.

## Implemented: chainkit-chain

Requires Python 3.11+ and uses only the standard library at runtime. Exposes `Rpc`, `ABI`, `address`, and `hash32`.

- JSON-RPC request/response validation, chain ID and genesis identity checks, failover on transient errors, and response size limits.
- Block, log, and receipt reads and generic RPC calls; log range splitting and fallback to receipts for single-block limits.
- Bounded dynamic ABI decoding with tuple/array support, anonymous events, hashes for indexed dynamic values, errors, and ambiguity reporting.
- Ethereum Keccak-256 with standard test vectors; signing is not provided.

`ABI(abi).decode('function', input_hex)` decodes function input; `decode('event', data_hex, topics)` decodes events; `decode('error', output_hex)` decodes errors. Large integers are returned as decimal strings.

Install with `python3 -m pip install ./packages/chain`. The package has not been published to a package registry. See the [indexer documentation](../backend/contract-indexer/README.md) for usage.
