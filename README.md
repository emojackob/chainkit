# Chainkit

> Reusable building blocks and deployment tooling for full-stack onchain applications.

Chainkit is a shared library that applications consume through versioned dependencies. Source code lives in one repository; modules are published independently as needed to make common capabilities reusable across applications.

The Rust workspace provides reusable EVM RPC pooling, ABI discovery and decoding, contract indexing, and HTTP/OpenAPI and stdio MCP adapters. The `chainkit-indexer` executable combines these libraries into a Linux service and CLI with SQLite persistence and multiple project registrations. Start with the [Rust integration example](examples/rust-indexer/README.md) and [architecture and coverage](docs/rust-architecture.md). See the [validation report](docs/rust-validation.md) for verified behavior and remaining release checks.

The earlier Python `chainkit-chain` and `chainkit-indexer` packages remain available as a separate implementation and behavioral reference. See their [usage guide](packages/backend/contract-indexer/README.md). The Rust executable does not invoke Python. The Python and Rust database formats are separate and must not share the same database file. Packages have not been published to package registries.

## Repository structure

```text
chainkit/
  crates/
    chainkit-evm/           # Bounded RPC pooling and chain identity
    chainkit-abi/           # ABI discovery and dynamic decoding
    chainkit-indexer/       # SQLite storage, indexing, and query engine
    chainkit-api/           # HTTP, OpenAPI, and stdio MCP adapters
  apps/
    chainkit-indexer/       # Rust CLI and service executable
  packaging/               # Debian/Arch packages and systemd integration
  packages/
    contracts/     # Reusable contracts
    chain/         # Onchain queries and transactions
    wallet/        # Wallet connections and signing
    ui/            # Frontend components
    backend/       # Shared backend capabilities
    deploy/        # Contract deployment, upgrades, records, and validation
    tooling/       # Engineering commands, builds, and Makefile support
  templates/       # Application scaffolds and Makefile templates
  examples/        # Integration examples
```

Each directory's README describes its responsibilities and boundaries.

## Integration

Applications depend on specific versions of the modules they need and retain their own business logic and configuration. Makefiles provide a common command entry point and invoke Chainkit tools; shared execution logic stays in those tools.

Templates initialize applications. After initialization, shared capabilities are upgraded through dependency versions. Later template changes do not automatically propagate to existing applications.

## Module and dependency principles

- Each module should have an explicit public API, version, and changelog and be published independently as needed. Modules do not need to be implemented or published together.
- Module internals are not public integration entry points. Breaking changes require explicit versioning and migration instructions.
- Declare dependencies explicitly and avoid dependency cycles.
- chain does not depend on wallet frameworks or UI; wallet and deploy may depend on chain as needed.
- contracts maintains the contract-layer boundary. Offchain modules may consume its interfaces or published artifacts; the integration mechanism remains to be defined.
- Separate generic UI presentation from wallet integration so applications do not have to adopt capabilities they do not need.
- tooling orchestrates commands; deployment domain logic belongs in deploy.
- backend includes capabilities with concrete reuse requirements and should not become a miscellaneous collection.

## Application and library boundaries

Business rules belong in application repositories. Extract shared capabilities into Chainkit after validating their reuse. Public modules must not hardcode application addresses, accounts, or dedicated configuration.

Deployment addresses, network configuration, and deployment and upgrade history belong in application repositories. Chainkit provides operations, record formats, and validation conventions. Keys and other credentials must not be committed.

## Development status

The Rust crates, executable, and earlier Python packages are at version `0.1.0`. Build Rust artifacts from this workspace; install Python packages from local paths. The contracts, wallet, UI, deployment, and general tooling modules are planned only.

Future work includes live-node validation, redecoding without refetching chain data, shared acquisition, and scheduling and storage extensions. See each module's documentation for its actual interfaces, coverage, and limitations.

## Local validation

Rust requires version 1.90 or newer; the existing Python implementation requires Python 3.11+. Run `make check` for Rust formatting, linting, workspace tests, and Python checks. Run `make rust-test` or `make python-check` independently. Use `make rust-indexer-help` for the Rust CLI and `make indexer-help` for the Python CLI. See [examples/rust-indexer](examples/rust-indexer/README.md) for Rust configuration and [examples/contract-indexer](examples/contract-indexer/README.md) for the Python application Makefile example.
