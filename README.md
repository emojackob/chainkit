# Chainkit

> Reusable building blocks and deployment tooling for full-stack onchain applications.

Chainkit is a shared library that applications consume through versioned dependencies. Source code lives in one repository; modules are published independently as needed to make common capabilities reusable across applications.

The first implemented modules are `chainkit-chain` and `chainkit-indexer`, providing EVM data acquisition, dynamic ABI decoding, and [contract indexing and transaction diagnostics](packages/backend/contract-indexer/README.md) with project isolation. Both Python packages can be embedded as dependencies; the indexer also provides a standalone HTTP service. Neither package has been published to a package registry. Other modules remain planned.

## Repository structure

```text
chainkit/
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

`chainkit-chain` and `chainkit-indexer` are at version `0.1.0` and have not been published to a package registry. Install them from local paths for now. Other directories describe planned module boundaries only.

Future work includes live-node validation, redecoding without refetching chain data, shared acquisition, and scheduling and storage extensions. See each module's documentation for its actual interfaces, coverage, and limitations.

## Local validation

Requires Python 3.11+. Run `make check` for syntax checks and deterministic tests, and `make indexer-help` for CLI help. See the [indexer documentation](packages/backend/contract-indexer/README.md) for integration steps and [examples/contract-indexer](examples/contract-indexer/README.md) for an application Makefile example.
