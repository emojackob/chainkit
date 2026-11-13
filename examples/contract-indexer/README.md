# Multi-project contract indexing example

Applies to `chainkit-chain==0.1.0` and `chainkit-indexer==0.1.0` in this repository. Neither package has been published; install from local paths first. Requires Python 3.11+ and an installation environment with pip/setuptools.

1. Install both packages from the Chainkit root: `python3 -m pip install ./packages/chain ./packages/backend/contract-indexer`.
2. Copy this directory's Makefile into your application repository. Copy config.example.json to indexer-config.json there and configure RPC endpoints for your chains. Do not commit the configuration file or SQLite database.
3. Run `make project-create PROJECT=project-a PROJECT_NAME=ProjectA` in the application repository and save the read/manage tokens returned on creation. Use a different PROJECT to create a separate identity for another project.
4. Run `make serve` and register each project's contracts through the project-scoped HTTP endpoints. To share one service, run a single service process and create multiple projects in the same database.
5. Give AI query clients the service URL, project ID, and OpenAPI document. All GET queries are public and do not require a read token. Read coverage first, then retrieve context by transaction hash. POST management endpoints require the manage token.

The local-node example uses chain_id=31337. With `confirmations=12`, the node must produce more blocks before recent transactions can be indexed. Development tests may explicitly set this to 0; that setting does not imply finality.

See the [indexer documentation](../../packages/backend/contract-indexer/README.md) for full interfaces, coverage modes, and limitations. The Makefile invokes installed tools rather than copying shared logic.

## Run local tasks from source

`run-local.sh` reads tasks from the directory specified by `CHAINKIT_JOB_DIR`. It does not embed networks, addresses, or credentials. Prepare the following files in that directory:

- Copy `job.example.json` to `job.json` and set the project ID, chain ID, contract address, and database path. Relative database paths are resolved against the task directory.
- Copy `config.example.json` to `rpc-config.json` and configure RPC endpoints.
- Use the CLI to create a project and register the contract in that database, as described in the indexer documentation.

```sh
export CHAINKIT_JOB_DIR=/path/to/private-task
bash examples/contract-indexer/run-local.sh scan
bash examples/contract-indexer/run-local.sh coverage
bash examples/contract-indexer/run-local.sh events --limit 20
bash examples/contract-indexer/run-local.sh serve
```

Keep task directories in the consuming application repository or another private location. When developing in this repository, use the ignored `local-indexes/` directory.
