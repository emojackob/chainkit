use chainkit_abi::{AbiResolver, AbiResolverConfig};
use chainkit_api::{http, mcp, openapi, ApiState};
use chainkit_indexer::Store;
use serde_json::{json, Value};
use std::collections::BTreeSet;

const ADDRESS: &str = "0x1111111111111111111111111111111111111111";
fn fixture() -> (tempfile::TempDir, ApiState, String) {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("index.sqlite")).unwrap();
    let project = store.create_project("alpha", "Alpha").unwrap();
    let token = project["tokens"]["manage"].as_str().unwrap().to_string();
    store.create_project("beta", "Beta").unwrap();
    let state = ApiState {
        store,
        resolver: AbiResolver::new(AbiResolverConfig::default()).unwrap(),
        chain_ids: BTreeSet::from([31337]),
    };
    (temp, state, token)
}

#[tokio::test]
async fn http_authorization_is_project_scoped_and_queries_are_public() {
    let (_temp, state, token) = fixture();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, http::router(state)).await.unwrap();
    });
    let client = reqwest::Client::new();
    let base = format!("http://{address}/v1/projects");
    let registration =
        json!({"chain_id":31337,"address":ADDRESS,"abi":[],"start_block":0,"mode":"basic"});
    let response = client
        .post(format!("{base}/alpha/contracts"))
        .json(&registration)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let response = client
        .post(format!("{base}/beta/contracts"))
        .bearer_auth(&token)
        .json(&registration)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    let response = client
        .post(format!("{base}/alpha/contracts"))
        .bearer_auth(&token)
        .json(&registration)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let response = client
        .get(format!(
            "{base}/alpha/chains/31337/contracts/{ADDRESS}/coverage"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let response = client
        .get(format!("{base}/beta/contracts"))
        .send()
        .await
        .unwrap();
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["data"].as_array().unwrap().len(), 0);
    let response = client
        .get(format!(
            "{base}/alpha/chains/31337/contracts/{ADDRESS}/events?limit=invalid"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["schema_version"], "1");
    assert!(value.get("error").is_some());
    server.abort();
}

#[test]
fn mcp_initialization_and_read_only_tools_share_store_queries() {
    let (_temp, state, _token) = fixture();
    let mut session = mcp::Session::default();
    let denied = session
        .handle(
            &state,
            json!({"jsonrpc":"2.0","id":0,"method":"tools/list"}),
        )
        .unwrap();
    assert!(denied.get("error").is_some());
    let initialized=session.handle(&state,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).unwrap();
    assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
    assert!(session
        .handle(
            &state,
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .is_none());
    let tools = session
        .handle(
            &state,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .unwrap();
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 6);
    for tool in tools["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
    }
    let response=session.handle(&state,json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_contracts","arguments":{"project":"alpha"}}})).unwrap();
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["structuredContent"]["data"], json!([]));
    let response=session.handle(&state,json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"delete_project","arguments":{"project":"alpha"}}})).unwrap();
    assert_eq!(response["error"]["code"], -32602);
}

#[tokio::test]
async fn mcp_stdio_emits_only_json_rpc_lines_and_ignores_notifications() {
    let (_temp, state, _) = fixture();
    let input = concat!(
        "not-json\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n"
    );
    let mut output = Vec::new();
    mcp::run(&state, input.as_bytes(), &mut output)
        .await
        .unwrap();
    let text = String::from_utf8(output).unwrap();
    let responses: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["error"]["code"], -32700);
    assert_eq!(responses[1]["id"], 1);
}

#[test]
fn openapi_documents_auth_paths_and_pagination() {
    let api = openapi::document();
    assert_eq!(api["openapi"], "3.1.0");
    assert!(api["paths"]
        .get("/v1/projects/{project}/contracts")
        .is_some());
    assert_eq!(
        api["paths"]["/v1/projects/{project}/contracts"]["post"]["security"],
        json!([{"ProjectToken":[]}])
    );
    assert!(api["paths"].get("/v1/projects").is_none());
    assert!(api["components"]["schemas"]["Page"]["properties"]
        .get("next_cursor")
        .is_some());
}
