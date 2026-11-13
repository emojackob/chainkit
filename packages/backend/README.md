# backend

Shared backend service capabilities.

## Scope and boundaries

- Extract task execution, event synchronization, and service infrastructure based on concrete reuse requirements.
- Reuse chain for onchain access rather than creating duplicate wrappers.
- Keep business data models, application APIs, and business rules in application repositories.
- Give each capability a clear responsibility and avoid a miscellaneous collection.

## Initial capability: contract indexing and transaction diagnostics

Given a contract address and ABI, index events and related transactions using configured chains and RPC endpoints. Persist raw evidence and query indexes to support AI-assisted transaction diagnostics.

- [Operation, integration, and query contract](contract-indexer/README.md)
- [Long-term design](contract-indexer/DESIGN.md)

The embeddable `chainkit-indexer` Python package and standalone HTTP service support project tokens, contract registration, ABI history, SQLite persistence, and transaction evidence queries. Basic mode covers events and direct transactions; full mode discovers internal calls through block-level callTracer data. Each mode declares its coverage and limitations. Runtime dependencies are the Python standard library and the separate `chainkit-chain` package. The packages have not been published.
