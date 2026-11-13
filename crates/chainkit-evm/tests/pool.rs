use axum::{
    extract::State,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use chainkit_evm::{normalize_address, normalize_hash, quantity, RpcConfig, RpcPool};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
enum Behavior {
    #[default]
    Healthy,
    TransientOnce,
    TransientAlways,
    MissingHistory,
    RateLimitOnce,
    Unsupported,
    Permanent,
    RevertWithRateMessage,
    BadEnvelope,
    Oversized,
    Slow,
}

#[derive(Clone)]
struct Mock {
    name: &'static str,
    chain: u64,
    genesis: String,
    behavior: Behavior,
    calls: Arc<Mutex<Vec<(String, Instant)>>>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    main_count: Arc<AtomicUsize>,
}

impl Mock {
    fn new(name: &'static str, behavior: Behavior) -> Self {
        Self {
            name,
            behavior,
            chain: 31337,
            genesis: format!("0x{}", "ab".repeat(32)),
            calls: Arc::default(),
            active: Arc::default(),
            max_active: Arc::default(),
            main_count: Arc::default(),
        }
    }
    fn count(&self, method: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .count()
    }
}

async fn handler(State(mock): State<Mock>, Json(body): Json<Value>) -> Response {
    let method = body["method"].as_str().unwrap().to_owned();
    mock.calls
        .lock()
        .unwrap()
        .push((method.clone(), Instant::now()));
    let id = body["id"].clone();
    let result = if method == "eth_chainId" {
        json!(format!("0x{:x}", mock.chain))
    } else if method == "eth_getBlockByNumber" && body["params"][0] == "0x0" {
        json!({"hash":mock.genesis})
    } else {
        let count = mock.main_count.fetch_add(1, Ordering::SeqCst);
        match mock.behavior {
            Behavior::MissingHistory => Value::Null,
            Behavior::TransientAlways => return (StatusCode::BAD_GATEWAY, "secret provider detail").into_response(),
            Behavior::TransientOnce if count == 0 => return (StatusCode::BAD_GATEWAY, "secret provider detail").into_response(),
            Behavior::RateLimitOnce if count == 0 => {
                let mut response = (StatusCode::TOO_MANY_REQUESTS, "secret provider detail").into_response();
                response.headers_mut().insert("retry-after", HeaderValue::from_static("1"));
                return response;
            }
            Behavior::Unsupported => return Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"secret unsupported"}})).into_response(),
            Behavior::Permanent => return Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"secret invalid params"}})).into_response(),
            Behavior::RevertWithRateMessage => return Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":3,"message":"execution reverted: rate limit"}})).into_response(),
            Behavior::BadEnvelope => return Json(json!({"jsonrpc":"2.0","id":999999,"result":"secret"})).into_response(),
            Behavior::Oversized => json!("x".repeat(4096)),
            Behavior::Slow => {
                let active = mock.active.fetch_add(1, Ordering::SeqCst) + 1;
                mock.max_active.fetch_max(active, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(60)).await;
                mock.active.fetch_sub(1, Ordering::SeqCst);
                json!({"node":mock.name})
            }
            _ => json!({"node":mock.name}),
        }
    };
    Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
}

struct Server {
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn serve(mock: Mock) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/?api-key=secret-credential",
        listener.local_addr().unwrap()
    );
    let app = Router::new().route("/", post(handler)).with_state(mock);
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server { url, task }
}
fn config() -> RpcConfig {
    RpcConfig {
        timeout_ms: 500,
        total_timeout_ms: 3000,
        base_backoff_ms: 1,
        max_backoff_ms: 5,
        cooldown_ms: 5,
        ..RpcConfig::default()
    }
}

#[tokio::test]
async fn round_robin_spreads_load_and_caches_identity() {
    let a = Mock::new("a", Behavior::Healthy);
    let b = Mock::new("b", Behavior::Healthy);
    let sa = serve(a.clone()).await;
    let sb = serve(b.clone()).await;
    let pool = RpcPool::new(31337, vec![sa.url.clone(), sb.url.clone()], config()).unwrap();
    for i in 0..8 {
        let result = pool.call("eth_blockNumber", json!([])).await.unwrap();
        assert_eq!(result["node"], if i % 2 == 0 { "a" } else { "b" });
    }
    assert_eq!(a.count("eth_chainId"), 1);
    assert_eq!(b.count("eth_chainId"), 1);
    assert_eq!(pool.health()[0].successes, 4);
    assert_eq!(pool.health()[1].successes, 4);
    assert!(!format!("{pool:?}").contains("secret"));
    assert!(!serde_json::to_string(&pool.health())
        .unwrap()
        .contains("http"));
}

#[tokio::test]
async fn transient_failure_switches_to_a_healthy_provider() {
    let a = Mock::new("a", Behavior::TransientOnce);
    let b = Mock::new("b", Behavior::Healthy);
    let sa = serve(a.clone()).await;
    let sb = serve(b.clone()).await;
    let pool = RpcPool::new(31337, vec![sa.url.clone(), sb.url.clone()], config()).unwrap();
    assert_eq!(
        pool.call("eth_blockNumber", json!([])).await.unwrap()["node"],
        "b"
    );
    assert_eq!(a.count("eth_blockNumber"), 1);
    assert_eq!(b.count("eth_blockNumber"), 1);
    assert_eq!(pool.health()[0].failures, 1);
}

#[tokio::test]
async fn retry_after_embargo_survives_across_calls() {
    let mock = Mock::new("a", Behavior::RateLimitOnce);
    let server = serve(mock.clone()).await;
    let pool = RpcPool::new(31337, vec![server.url.clone()], config()).unwrap();
    pool.call("eth_blockNumber", json!([])).await.unwrap();
    let times: Vec<_> = mock
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m == "eth_blockNumber")
        .map(|(_, t)| *t)
        .collect();
    assert_eq!(times.len(), 2);
    assert!(times[1].duration_since(times[0]) >= Duration::from_millis(990));
}

#[tokio::test]
async fn wrong_chain_is_quarantined_without_blocking_a_valid_peer() {
    let mut a = Mock::new("a", Behavior::Healthy);
    a.chain = 1;
    let b = Mock::new("b", Behavior::Healthy);
    let sa = serve(a.clone()).await;
    let sb = serve(b.clone()).await;
    let pool = RpcPool::new(31337, vec![sa.url.clone(), sb.url.clone()], config()).unwrap();
    assert_eq!(
        pool.call("eth_blockNumber", json!([])).await.unwrap()["node"],
        "b"
    );
    assert_eq!(a.count("eth_blockNumber"), 0);
    assert_eq!(pool.health()[0].status, "quarantined");
    pool.call("eth_blockNumber", json!([])).await.unwrap();
    assert_eq!(a.count("eth_chainId"), 1);
}

#[tokio::test]
async fn genesis_mismatch_is_quarantined() {
    let mut mock = Mock::new("a", Behavior::Healthy);
    mock.genesis = format!("0x{}", "cd".repeat(32));
    let server = serve(mock.clone()).await;
    let config = RpcConfig {
        expected_genesis_hash: Some(format!("0x{}", "ab".repeat(32))),
        ..config()
    };
    let pool = RpcPool::new(31337, vec![server.url.clone()], config).unwrap();
    let error = pool.call("eth_blockNumber", json!([])).await.unwrap_err();
    assert_eq!(error.category, "identity");
    assert_eq!(mock.count("eth_blockNumber"), 0);
    assert_eq!(pool.health()[0].status, "quarantined");
}

#[tokio::test]
async fn unsupported_methods_fail_over_and_are_cached_per_method() {
    let a = Mock::new("a", Behavior::Unsupported);
    let b = Mock::new("b", Behavior::Healthy);
    let sa = serve(a.clone()).await;
    let sb = serve(b.clone()).await;
    let pool = RpcPool::new(31337, vec![sa.url.clone(), sb.url.clone()], config()).unwrap();
    for _ in 0..3 {
        pool.call(
            "debug_traceBlockByNumber",
            json!(["0x1", {"tracer":"callTracer"}]),
        )
        .await
        .unwrap();
    }
    assert_eq!(a.count("debug_traceBlockByNumber"), 1);
    assert_eq!(b.count("debug_traceBlockByNumber"), 3);
    assert_eq!(
        pool.health()[0].unsupported_methods,
        vec!["debug_traceBlockByNumber"]
    );
    let error = pool.call("eth_blockNumber", json!([])).await.unwrap();
    assert_eq!(error["node"], "b");
    assert_eq!(a.count("eth_blockNumber"), 1);
}

#[tokio::test]
async fn permanent_rpc_errors_do_not_retry_or_leak_provider_strings() {
    let mock = Mock::new("a", Behavior::Permanent);
    let server = serve(mock.clone()).await;
    let pool = RpcPool::new(31337, vec![server.url.clone()], config()).unwrap();
    let error = pool
        .call("eth_call", json!([{}, "latest"]))
        .await
        .unwrap_err();
    assert_eq!(error.category, "rpc");
    assert_eq!(mock.count("eth_call"), 1);
    assert!(!format!("{error:?}").contains("secret"));
    assert!(!error.to_string().contains("secret"));
}

#[tokio::test]
async fn execution_revert_messages_cannot_trigger_transient_retries() {
    let mock = Mock::new("a", Behavior::RevertWithRateMessage);
    let server = serve(mock.clone()).await;
    let pool = RpcPool::new(31337, vec![server.url.clone()], config()).unwrap();
    let error = pool
        .call("eth_call", json!([{}, "latest"]))
        .await
        .unwrap_err();
    assert_eq!(error.category, "rpc");
    assert_eq!(mock.count("eth_call"), 1);
}

#[tokio::test]
async fn malformed_envelopes_and_oversized_responses_are_not_retried() {
    for (behavior, category) in [
        (Behavior::BadEnvelope, "invalid_response"),
        (Behavior::Oversized, "too_large"),
    ] {
        let mock = Mock::new("a", behavior);
        let server = serve(mock.clone()).await;
        let config = RpcConfig {
            max_response_bytes: 1024,
            ..config()
        };
        let pool = RpcPool::new(31337, vec![server.url.clone()], config).unwrap();
        assert_eq!(
            pool.call("eth_blockNumber", json!([]))
                .await
                .unwrap_err()
                .category,
            category
        );
        assert_eq!(mock.count("eth_blockNumber"), 1);
    }
}

#[tokio::test]
async fn concurrent_requests_obey_per_endpoint_capacity() {
    let mock = Mock::new("a", Behavior::Slow);
    let server = serve(mock.clone()).await;
    let config = RpcConfig {
        max_in_flight_per_endpoint: 2,
        ..config()
    };
    let pool = RpcPool::new(31337, vec![server.url.clone()], config).unwrap();
    let mut tasks = Vec::new();
    for _ in 0..10 {
        let pool = pool.clone();
        tasks.push(tokio::spawn(async move {
            pool.call("eth_blockNumber", json!([])).await.unwrap()
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert!(mock.max_active.load(Ordering::SeqCst) <= 2);
    assert_eq!(mock.count("eth_blockNumber"), 10);
    assert_eq!(pool.health()[0].in_flight, 0);
}

#[tokio::test]
async fn deadline_bounds_the_operation_and_cancellation_releases_capacity() {
    let mock = Mock::new("a", Behavior::Slow);
    let server = serve(mock.clone()).await;
    let config = RpcConfig {
        timeout_ms: 20,
        total_timeout_ms: 40,
        max_in_flight_per_endpoint: 1,
        ..config()
    };
    let pool = RpcPool::new(31337, vec![server.url.clone()], config).unwrap();
    let start = Instant::now();
    let error = pool.call("eth_blockNumber", json!([])).await.unwrap_err();
    assert_eq!(error.category, "timeout");
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(pool.health()[0].in_flight, 0);
}

#[tokio::test]
async fn request_rate_includes_identity_requests() {
    let mock = Mock::new("a", Behavior::Healthy);
    let server = serve(mock.clone()).await;
    let config = RpcConfig {
        requests_per_second: Some(20),
        ..config()
    };
    let pool = RpcPool::new(31337, vec![server.url.clone()], config).unwrap();
    pool.call("eth_blockNumber", json!([])).await.unwrap();
    let times = mock.calls.lock().unwrap();
    assert_eq!(times.len(), 3);
    for window in times.windows(2) {
        assert!(window[1].1.duration_since(window[0].1) >= Duration::from_millis(40));
    }
}

#[tokio::test]
async fn mutation_methods_are_rejected_before_network_access() {
    let mock = Mock::new("a", Behavior::Healthy);
    let server = serve(mock.clone()).await;
    let pool = RpcPool::new(31337, vec![server.url.clone()], config()).unwrap();
    for method in [
        "eth_sendRawTransaction",
        "eth_sendTransaction",
        "personal_unlockAccount",
        "debug_setHead",
    ] {
        assert_eq!(
            pool.call(method, json!([])).await.unwrap_err().category,
            "method_not_allowed"
        );
    }
    assert!(mock.calls.lock().unwrap().is_empty());
}

#[test]
fn quantities_and_identifiers_are_bounded_and_normalized() {
    assert_eq!(quantity("0xffffffffffffffff").unwrap(), u64::MAX);
    for value in ["0x", "1", "0x01", "0x10000000000000000", "0xgg"] {
        assert!(quantity(value).is_err());
    }
    assert_eq!(
        normalize_address(&format!("0x{}", "AB".repeat(20))).unwrap(),
        format!("0x{}", "ab".repeat(20))
    );
    assert!(normalize_hash("0x12").is_err());
    assert!(RpcPool::new(1, vec!["https://secret credential".into()], config()).is_err());
    assert!(RpcPool::new(1, vec!["file:///secret".into()], config()).is_err());
}

#[tokio::test]
async fn cooldown_skips_a_failed_node_then_recovers_it() {
    let a = Mock::new("a", Behavior::TransientOnce);
    let b = Mock::new("b", Behavior::Healthy);
    let sa = serve(a.clone()).await;
    let sb = serve(b.clone()).await;
    let config = RpcConfig {
        cooldown_ms: 80,
        ..config()
    };
    let pool = RpcPool::new(31337, vec![sa.url.clone(), sb.url.clone()], config).unwrap();
    assert_eq!(
        pool.call("eth_blockNumber", json!([])).await.unwrap()["node"],
        "b"
    );
    assert_eq!(
        pool.call("eth_blockNumber", json!([])).await.unwrap()["node"],
        "b"
    );
    assert_eq!(pool.health()[0].status, "cooldown");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        pool.call("eth_blockNumber", json!([])).await.unwrap()["node"],
        "a"
    );
    assert_eq!(pool.health()[0].status, "ready");
    assert_eq!(pool.health()[0].consecutive_failures, 0);
    assert_eq!(a.count("eth_chainId"), 2);
}

#[tokio::test]
async fn retry_after_blocks_later_callers_without_network_requests() {
    let mock = Mock::new("a", Behavior::RateLimitOnce);
    let server = serve(mock.clone()).await;
    let config = RpcConfig {
        total_timeout_ms: 50,
        ..config()
    };
    let pool = RpcPool::new(31337, vec![server.url.clone()], config).unwrap();
    for _ in 0..2 {
        assert_eq!(
            pool.call("eth_blockNumber", json!([]))
                .await
                .unwrap_err()
                .category,
            "timeout"
        );
    }
    assert_eq!(mock.count("eth_blockNumber"), 1);
    assert_eq!(pool.health()[0].status, "cooldown");
}

#[tokio::test]
async fn retry_attempt_budget_bounds_a_persistently_failing_provider() {
    let mock = Mock::new("a", Behavior::TransientAlways);
    let server = serve(mock.clone()).await;
    let config = RpcConfig {
        max_attempts: 3,
        base_backoff_ms: 0,
        max_backoff_ms: 0,
        cooldown_ms: 0,
        ..config()
    };
    let pool = RpcPool::new(31337, vec![server.url.clone()], config).unwrap();
    assert_eq!(
        pool.call("eth_blockNumber", json!([]))
            .await
            .unwrap_err()
            .category,
        "transient"
    );
    assert_eq!(mock.count("eth_blockNumber"), 3);
    assert_eq!(pool.health()[0].failures, 3);
}

#[tokio::test]
async fn missing_historical_blocks_try_a_peer_without_global_quarantine() {
    let a = Mock::new("a", Behavior::MissingHistory);
    let b = Mock::new("b", Behavior::Healthy);
    let sa = serve(a.clone()).await;
    let sb = serve(b.clone()).await;
    let pool = RpcPool::new(31337, vec![sa.url.clone(), sb.url.clone()], config()).unwrap();
    assert!(pool.genesis_hash().is_none());
    assert_eq!(
        pool.call("eth_getBlockByNumber", json!(["0x1", true]))
            .await
            .unwrap()["node"],
        "b"
    );
    assert_eq!(
        pool.genesis_hash().unwrap(),
        format!("0x{}", "ab".repeat(32))
    );
    assert_eq!(pool.health()[0].status, "ready");
    // A missing receipt may indicate a pending or unknown transaction and is not
    // enough evidence to classify this provider's block history as unavailable.
    assert!(pool
        .call(
            "eth_getTransactionReceipt",
            json!([format!("0x{}", "11".repeat(32))])
        )
        .await
        .unwrap()
        .is_null());
    assert_eq!(a.count("eth_getTransactionReceipt"), 1);
}

#[tokio::test]
async fn missing_history_is_bounded_to_one_attempt_per_endpoint() {
    let a = Mock::new("a", Behavior::MissingHistory);
    let b = Mock::new("b", Behavior::MissingHistory);
    let sa = serve(a.clone()).await;
    let sb = serve(b.clone()).await;
    let pool = RpcPool::new(31337, vec![sa.url.clone(), sb.url.clone()], config()).unwrap();
    let error = pool
        .call("eth_getBlockByNumber", json!(["0x1", true]))
        .await
        .unwrap_err();
    assert_eq!(error.category, "history_unavailable");
    assert_eq!(a.count("eth_getBlockByNumber"), 2);
    assert_eq!(b.count("eth_getBlockByNumber"), 2);
}

#[tokio::test]
async fn queued_requests_obey_new_embargo_and_timeouts_cannot_shorten_it() {
    let mock = Mock::new("a", Behavior::RateLimitOnce);
    let server = serve(mock.clone()).await;
    let config = RpcConfig {
        timeout_ms: 150,
        total_timeout_ms: 400,
        requests_per_second: Some(20),
        max_in_flight_per_endpoint: 2,
        ..config()
    };
    let pool = RpcPool::new(31337, vec![server.url.clone()], config).unwrap();
    // Warm identity validation without consuming the mock's first data response.
    pool.call("eth_chainId", json!([])).await.unwrap();
    let a = pool.clone();
    let b = pool.clone();
    let first = tokio::spawn(async move { a.call("eth_blockNumber", json!([])).await });
    let second = tokio::spawn(async move { b.call("eth_blockNumber", json!([])).await });
    assert_eq!(first.await.unwrap().unwrap_err().category, "timeout");
    assert_eq!(second.await.unwrap().unwrap_err().category, "timeout");
    assert_eq!(mock.count("eth_blockNumber"), 1);
    assert_eq!(pool.health()[0].status, "cooldown");
    assert_eq!(pool.health()[0].in_flight, 0);
}
