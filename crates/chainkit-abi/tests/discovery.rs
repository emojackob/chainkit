use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use chainkit_abi::{AbiResolver, AbiResolverConfig};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

async fn sourcify(
    State(count): State<Arc<AtomicUsize>>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<Value> {
    count.fetch_add(1, Ordering::SeqCst);
    assert_eq!(query.get("fields").unwrap(), "abi");
    Json(json!({"abi":[{"type":"event","name":"Changed","inputs":[],"anonymous":false}]}))
}
#[tokio::test]
async fn discovery_uses_chain_scoped_cache_and_minimal_provider_fields() {
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/v2/contract/{chain}/{address}", get(sourcify))
        .with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let resolver = AbiResolver::new(AbiResolverConfig {
        sourcify_url: base,
        etherscan_url: None,
        etherscan_api_key: None,
        timeout_ms: 1000,
    })
    .unwrap();
    let address = format!("0x{}", "11".repeat(20));
    let first = resolver.resolve(31337, &address).await.unwrap();
    assert_eq!(first.source, "sourcify");
    assert_eq!(first.abi[0]["name"], "Changed");
    resolver.resolve(31337, &address).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    resolver.resolve(31338, &address).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    server.abort();
}
#[tokio::test]
async fn unavailable_provider_falls_back_and_never_exposes_credentials() {
    let app = Router::new()
        .route(
            "/v2/contract/{chain}/{address}",
            get(|| async { Json(json!({"error":"not found"})) }),
        )
        .route(
            "/api",
            get(|Query(query): Query<HashMap<String, String>>| async move {
                assert_eq!(query["chainid"], "31337");
                assert_eq!(query["action"], "getabi");
                assert_eq!(query["apikey"], "private-test-token");
                Json(json!({"status":"1","result":"[]"}))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = AbiResolverConfig {
        sourcify_url: base.clone(),
        etherscan_url: Some(format!("{base}/api")),
        etherscan_api_key: Some("private-test-token".into()),
        timeout_ms: 1000,
    };
    assert!(!format!("{config:?}").contains("private-test-token"));
    let resolver = AbiResolver::new(config).unwrap();
    let address = format!("0x{}", "11".repeat(20));
    assert_eq!(
        resolver.resolve(31337, &address).await.unwrap().source,
        "etherscan"
    );
    server.abort();
    let error = resolver.resolve(31338, &address).await.unwrap_err();
    assert_eq!(error.category, "abi_unavailable");
    assert!(!format!("{error:?}").contains("private-test-token"));
    assert!(!format!("{error}").contains("http"));
}
