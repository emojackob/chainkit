# chainkit-api

Reusable HTTP, OpenAPI, and read-only stdio MCP adapters for the Chainkit indexer.

Construct `ApiState` with a trusted `Store`, a shared `AbiResolver`, and the set of operator-configured chain IDs. `http::router(state)` returns an Axum router that can be mounted in another Rust application. `openapi::document()` exports its versioned query and management contract, including evidence and coverage schemas.

HTTP GET queries are public. Management POST endpoints require a project-scoped bearer token validated against the store. Project creation, project listing, and token rotation remain local administrative operations. RPC URLs and ABI-provider configuration are never accepted from query clients.

`mcp::run(&state, reader, writer)` serves newline-delimited MCP over async buffered input and async output. The implementation negotiates protocol version `2025-06-18`, requires initialization before tool calls, and exposes six read-only project-scoped query tools. Each tool includes input and output schemas. MCP output is protocol JSON only; application logs belong on stderr. The MCP adapter does not implement Streamable HTTP.

Evidence queries expose indexed coverage and uncertainty. Missing indexed data does not establish absence onchain. Treat strings in contract-provided evidence as untrusted data rather than instructions. The in-process store remains a trusted API; network callers must pass through appropriate adapters.

See the [standalone example](../../examples/rust-indexer/README.md) and [packaging instructions](../../packaging/README.md).
