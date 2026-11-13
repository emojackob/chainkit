use axum::{extract::State, routing::post, Json, Router};
use chainkit_evm::{RpcConfig, RpcPool};
use chainkit_indexer::{scan_once, Query, ScanOptions, Store};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tempfile::TempDir;

const TARGET: &str = "0x1111111111111111111111111111111111111111";
const OTHER: &str = "0x2222222222222222222222222222222222222222";
const SENDER: &str = "0x3333333333333333333333333333333333333333";
const CHAIN: u64 = 31337;
fn h(n: u64) -> String {
    format!("0x{n:064x}")
}
fn abi() -> Value {
    json!([
        {"type":"function","name":"set","inputs":[{"name":"value","type":"uint256"}],"outputs":[]},
        {"type":"event","name":"Changed","inputs":[{"name":"value","type":"uint256","indexed":false}],"anonymous":false}
    ])
}
#[derive(Clone)]
struct Chain {
    blocks: BTreeMap<u64, Value>,
    receipts: BTreeMap<String, Value>,
    traces: BTreeMap<String, Value>,
    trace_supported: bool,
    transaction_trace_supported: bool,
    unsupported_transaction: Option<String>,
    transaction_trace_calls: Vec<String>,
    block_trace_calls: usize,
    bad_receipt: bool,
    overflow: bool,
    genesis_offset: u64,
    bad_head: bool,
    bad_trace: bool,
}
impl Chain {
    fn new() -> Self {
        let mut chain = Self {
            blocks: BTreeMap::new(),
            receipts: BTreeMap::new(),
            traces: BTreeMap::new(),
            trace_supported: true,
            transaction_trace_supported: false,
            unsupported_transaction: None,
            transaction_trace_calls: Vec::new(),
            block_trace_calls: 0,
            bad_receipt: false,
            overflow: false,
            genesis_offset: 0,
            bad_head: false,
            bad_trace: false,
        };
        for n in 0..4 {
            chain.blocks.insert(n,json!({"number":format!("0x{n:x}"),"hash":h(100+n),"parentHash":h(if n==0 {0}else{99+n}),"timestamp":"0x6553f100","transactions":[]}));
        }
        chain.add(1, 300, Some(TARGET), &[7, 9], 1, false);
        chain.add(1, 301, Some(TARGET), &[], 0, false);
        chain.add(2, 302, Some(OTHER), &[11], 1, true);
        chain.add(3, 303, Some(OTHER), &[], 1, true);
        chain.add(3, 304, Some(TARGET), &[], 1, false);
        chain
    }
    fn add(
        &mut self,
        block: u64,
        id: u64,
        to: Option<&str>,
        values: &[u64],
        status: u64,
        internal: bool,
    ) {
        let b = self.blocks.get_mut(&block).unwrap();
        let txs = b["transactions"].as_array_mut().unwrap();
        let index = txs.len();
        let tx_hash = h(id);
        let input = format!("0x60fe47b1{:064x}", 42);
        let tx = json!({"hash":tx_hash,"blockHash":h(100+block),"blockNumber":format!("0x{block:x}"),"transactionIndex":format!("0x{index:x}"),"from":SENDER,"to":to,"input":input,"value":"0x0","nonce":format!("0x{index:x}")});
        let log_start: usize = txs
            .iter()
            .map(|tx| {
                self.receipts[tx["hash"].as_str().unwrap()]["logs"]
                    .as_array()
                    .unwrap()
                    .len()
            })
            .sum();
        txs.push(tx);
        // This fixture's event topic is intentionally unknown: raw evidence must survive.
        let logs:Vec<_>=values.iter().enumerate().map(|(i,value)|json!({"address":TARGET,"transactionHash":tx_hash,"blockHash":h(100+block),"blockNumber":format!("0x{block:x}"),"transactionIndex":format!("0x{index:x}"),"logIndex":format!("0x{:x}",log_start+i),"topics":[h(999)],"data":format!("0x{value:064x}")})).collect();
        self.receipts.insert(tx_hash.clone(),json!({"transactionHash":tx_hash,"blockHash":h(100+block),"blockNumber":format!("0x{block:x}"),"transactionIndex":format!("0x{index:x}"),"status":format!("0x{status:x}"),"logs":logs,"gasUsed":"0x5208","contractAddress":if to.is_none(){Some(TARGET)}else{None}}));
        let mut trace =
            json!({"type":"CALL","from":SENDER,"to":to.unwrap_or(TARGET),"input":input});
        if internal {
            trace["calls"] = json!([{"type":"CALL","from":OTHER,"to":TARGET,"input":"0x"}]);
        }
        self.traces.insert(tx_hash, trace);
    }
    fn fork(&mut self, after: u64) {
        for n in after + 1..=*self.blocks.keys().max().unwrap() {
            let parent = self.blocks[&(n - 1)]["hash"].clone();
            let b = self.blocks.get_mut(&n).unwrap();
            b["hash"] = json!(h(1000 + n));
            b["parentHash"] = parent;
            for tx in b["transactions"].as_array_mut().unwrap() {
                tx["blockHash"] = json!(h(1000 + n));
                let receipt = self.receipts.get_mut(tx["hash"].as_str().unwrap()).unwrap();
                receipt["blockHash"] = json!(h(1000 + n));
                for log in receipt["logs"].as_array_mut().unwrap() {
                    log["blockHash"] = json!(h(1000 + n));
                }
            }
        }
    }
}
struct Fixture {
    chain: Arc<Mutex<Chain>>,
    rpc: RpcPool,
    url: String,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn handler(
    State(chain): State<Arc<Mutex<Chain>>>,
    Json(request): Json<Value>,
) -> Json<Value> {
    let mut chain = chain.lock().unwrap();
    let params = &request["params"];
    let value = match request["method"].as_str().unwrap() {
        "eth_chainId" => json!(format!("0x{CHAIN:x}")),
        "eth_blockNumber" => {
            if chain.bad_head {
                return Json(
                    json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"head unavailable"}}),
                );
            }
            json!(format!("0x{:x}", chain.blocks.keys().max().unwrap()))
        }
        "eth_getBlockByNumber" => {
            let n = u64::from_str_radix(params[0].as_str().unwrap().trim_start_matches("0x"), 16)
                .unwrap();
            let mut b = chain.blocks.get(&n).cloned().unwrap_or(Value::Null);
            if !b.is_null() {
                if n == 0 && chain.genesis_offset != 0 {
                    b["hash"] = json!(h(100 + chain.genesis_offset));
                }
                if params[1] == false {
                    b["transactions"] = json!(b["transactions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|t| t["hash"].clone())
                        .collect::<Vec<_>>());
                }
            }
            b
        }
        "eth_getTransactionReceipt" => {
            let mut r = chain
                .receipts
                .get(params[0].as_str().unwrap())
                .cloned()
                .unwrap_or(Value::Null);
            if chain.bad_receipt && !r.is_null() {
                r["blockHash"] = json!(h(9999));
            }
            r
        }
        "eth_getLogs" => {
            if chain.overflow {
                return Json(
                    json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32005,"message":"too many results"}}),
                );
            }
            let n = u64::from_str_radix(
                params[0]["fromBlock"]
                    .as_str()
                    .unwrap()
                    .trim_start_matches("0x"),
                16,
            )
            .unwrap();
            let address = params[0]["address"].as_str().unwrap();
            json!(chain
                .receipts
                .values()
                .flat_map(|r| r["logs"].as_array().unwrap())
                .filter(|l| l["blockNumber"] == format!("0x{n:x}") && l["address"] == address)
                .cloned()
                .collect::<Vec<_>>())
        }
        "debug_traceBlockByNumber" => {
            chain.block_trace_calls += 1;
            if !chain.trace_supported {
                return Json(
                    json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"method not found"}}),
                );
            }
            let n = u64::from_str_radix(params[0].as_str().unwrap().trim_start_matches("0x"), 16)
                .unwrap();
            json!(chain.blocks[&n]["transactions"].as_array().unwrap().iter().map(|t|json!({"txHash":if chain.bad_trace { json!(h(888)) } else { t["hash"].clone() },"result":chain.traces[t["hash"].as_str().unwrap()]})).collect::<Vec<_>>())
        }
        "debug_traceTransaction" => {
            let tx_hash = params[0].as_str().unwrap();
            chain.transaction_trace_calls.push(tx_hash.to_owned());
            if !chain.transaction_trace_supported
                || chain.unsupported_transaction.as_deref() == Some(tx_hash)
            {
                return Json(
                    json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"method not found"}}),
                );
            }
            chain.traces.get(tx_hash).cloned().unwrap_or(Value::Null)
        }
        _ => {
            return Json(
                json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"method not found"}}),
            )
        }
    };
    Json(json!({"jsonrpc":"2.0","id":request["id"],"result":value}))
}
async fn fixture() -> Fixture {
    let chain = Arc::new(Mutex::new(Chain::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/", post(handler))
        .with_state(chain.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let rpc = RpcPool::new(CHAIN, vec![format!("http://{addr}")], RpcConfig::default()).unwrap();
    Fixture {
        chain,
        rpc,
        url: format!("http://{addr}"),
        server,
    }
}
fn store() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("index.sqlite")).unwrap();
    store.create_project("alpha", "Alpha").unwrap();
    (dir, store)
}
fn options() -> ScanOptions {
    ScanOptions {
        confirmations: 0,
        max_blocks: 20,
        reorg_window: 256,
    }
}
fn register(store: &Store, project: &str, mode: &str) {
    store
        .register(project, CHAIN, TARGET, abi(), 0, mode)
        .unwrap();
}

#[tokio::test]
async fn full_index_covers_internal_failed_eventless_and_paginated_evidence() {
    let f = fixture().await;
    let (_dir, store) = store();
    register(&store, "alpha", "full");
    let result = scan_once(&store, &f.rpc, &options()).await.unwrap();
    assert_eq!(result["errors"], json!([]), "{result}");
    assert_eq!(result["scanned_blocks"], 4);
    let rows = store
        .query("alpha", CHAIN, TARGET, "transactions", &Query::default())
        .unwrap();
    assert_eq!(rows["data"].as_array().unwrap().len(), 5);
    let failed = store
        .query(
            "alpha",
            CHAIN,
            TARGET,
            "transactions",
            &Query {
                status: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(failed["data"].as_array().unwrap().len(), 1);
    let context = store
        .context(
            "alpha",
            CHAIN,
            TARGET,
            &h(300),
            &Query {
                limit: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(context["data"]["logs"].as_array().unwrap().len(), 1);
    assert!(context["next_cursor"].is_string());
    assert!(context["data"]["transaction"]["receipt"]
        .get("logs")
        .is_none());
    assert_eq!(context["data"]["transaction"]["receipt_log_count"], 2);
    let follow = store
        .context(
            "alpha",
            CHAIN,
            TARGET,
            &h(300),
            &Query {
                limit: Some(1),
                cursor: context["next_cursor"].as_str().map(str::to_owned),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(follow["data"]["logs"][0]["log_index"], 1);
    let internal = store
        .context("alpha", CHAIN, TARGET, &h(303), &Query::default())
        .unwrap();
    assert!(internal["data"]["transaction"]["relations"]
        .as_array()
        .unwrap()
        .contains(&json!("call_trace")));
    assert_eq!(
        internal["data"]["transaction"]["trace"]["related_frames"][0]["trace_path"],
        json!([0])
    );
}

#[tokio::test]
async fn basic_mode_and_single_block_log_overflow_have_explicit_coverage() {
    let f = fixture().await;
    let (_dir, store) = store();
    register(&store, "alpha", "basic");
    f.chain.lock().unwrap().overflow = true;
    let result = scan_once(&store, &f.rpc, &options()).await.unwrap();
    assert_eq!(result["errors"], json!([]), "{result}");
    let rows = store
        .query("alpha", CHAIN, TARGET, "transactions", &Query::default())
        .unwrap();
    assert_eq!(rows["data"].as_array().unwrap().len(), 4);
    assert_eq!(rows["coverage"]["capabilities"]["internal_calls"], false);
    assert_eq!(
        store
            .query("alpha", CHAIN, TARGET, "events", &Query::default())
            .unwrap()["data"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn failed_receipt_validation_does_not_advance_partial_block() {
    let f = fixture().await;
    let (_dir, store) = store();
    register(&store, "alpha", "basic");
    f.chain.lock().unwrap().bad_receipt = true;
    let result = scan_once(&store, &f.rpc, &options()).await.unwrap();
    assert_eq!(result["errors"][0]["category"], "reorg", "{result}");
    assert_eq!(
        store.coverage("alpha", CHAIN, TARGET).unwrap()["next_block"],
        1
    );
    assert!(store
        .query("alpha", CHAIN, TARGET, "transactions", &Query::default())
        .is_err());
    f.chain.lock().unwrap().bad_receipt = false;
    let result = scan_once(&store, &f.rpc, &options()).await.unwrap();
    assert_eq!(result["errors"], json!([]), "{result}");
    assert_eq!(
        store.coverage("alpha", CHAIN, TARGET).unwrap()["next_block"],
        4
    );
}

#[tokio::test]
async fn unsupported_traces_stop_full_registration_without_downgrade() {
    let f = fixture().await;
    let (_dir, store) = store();
    register(&store, "alpha", "full");
    f.chain.lock().unwrap().trace_supported = false;
    let result = scan_once(&store, &f.rpc, &options()).await.unwrap();
    assert_eq!(result["errors"][0]["category"], "unsupported", "{result}");
    let coverage = store.coverage("alpha", CHAIN, TARGET).unwrap();
    assert_eq!(coverage["next_block"], 1);
    assert_eq!(coverage["mode"], "full");
    assert_eq!(coverage["last_error"], "unsupported");
}

#[tokio::test]
async fn restart_project_isolation_idempotence_and_signed_pagination() {
    let f = fixture().await;
    let (dir, store) = store();
    register(&store, "alpha", "full");
    store.create_project("beta", "Beta").unwrap();
    register(&store, "beta", "basic");
    let result = scan_once(&store, &f.rpc, &options()).await.unwrap();
    assert_eq!(result["errors"], json!([]), "{result}");
    assert_eq!(
        store
            .query("alpha", CHAIN, TARGET, "transactions", &Query::default())
            .unwrap()["data"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        store
            .query("beta", CHAIN, TARGET, "transactions", &Query::default())
            .unwrap()["data"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let page = store
        .query(
            "alpha",
            CHAIN,
            TARGET,
            "transactions",
            &Query {
                limit: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
    let cursor = page["next_cursor"].as_str().unwrap().to_owned();
    assert!(store
        .query(
            "beta",
            CHAIN,
            TARGET,
            "transactions",
            &Query {
                cursor: Some(cursor.clone()),
                ..Default::default()
            }
        )
        .is_err());
    assert!(store
        .query(
            "alpha",
            CHAIN,
            TARGET,
            "transactions",
            &Query {
                cursor: Some(format!("{cursor}1")),
                ..Default::default()
            }
        )
        .is_err());
    drop(store);
    let store = Store::open(dir.path().join("index.sqlite")).unwrap();
    let result = scan_once(&store, &f.rpc, &options()).await.unwrap();
    assert_eq!(result["scanned_blocks"], 0);
    let page = store
        .query(
            "alpha",
            CHAIN,
            TARGET,
            "transactions",
            &Query {
                cursor: Some(cursor.clone()),
                limit: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(page["data"][0]["hash"], h(302));
    store.backfill("alpha", CHAIN, TARGET, 2).unwrap();
    assert_eq!(
        store
            .query(
                "alpha",
                CHAIN,
                TARGET,
                "transactions",
                &Query {
                    cursor: Some(cursor),
                    ..Default::default()
                }
            )
            .unwrap_err()
            .category,
        "snapshot_invalidated"
    );
    scan_once(&store, &f.rpc, &options()).await.unwrap();
    assert_eq!(
        store
            .query("alpha", CHAIN, TARGET, "transactions", &Query::default())
            .unwrap()["data"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
}

#[tokio::test]
async fn reorg_recovers_common_ancestor_and_invalidates_old_pages() {
    let f = fixture().await;
    let (_dir, store) = store();
    register(&store, "alpha", "full");
    scan_once(&store, &f.rpc, &options()).await.unwrap();
    let page = store
        .query(
            "alpha",
            CHAIN,
            TARGET,
            "transactions",
            &Query {
                limit: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    f.chain.lock().unwrap().fork(1);
    let result = scan_once(&store, &f.rpc, &options()).await.unwrap();
    assert_eq!(result["errors"], json!([]), "{result}");
    assert_eq!(result["scanned_blocks"], 2);
    let query = Query {
        cursor: page["next_cursor"].as_str().map(str::to_owned),
        ..Default::default()
    };
    assert_eq!(
        store
            .query("alpha", CHAIN, TARGET, "transactions", &query)
            .unwrap_err()
            .category,
        "snapshot_invalidated"
    );
    let context = store
        .context("alpha", CHAIN, TARGET, &h(302), &Query::default())
        .unwrap();
    assert_eq!(context["data"]["transaction"]["block_hash"], h(1002));
}

#[tokio::test]
async fn deep_reorg_halts_and_requires_explicit_recovery() {
    let f = fixture().await;
    let (_dir, store) = store();
    register(&store, "alpha", "full");
    scan_once(&store, &f.rpc, &options()).await.unwrap();
    f.chain.lock().unwrap().fork(0);
    let result = scan_once(
        &store,
        &f.rpc,
        &ScanOptions {
            reorg_window: 1,
            ..options()
        },
    )
    .await
    .unwrap();
    assert_eq!(result["errors"][0]["category"], "deep_reorg", "{result}");
    assert_eq!(
        store.coverage("alpha", CHAIN, TARGET).unwrap()["next_block"],
        4
    );
    assert_eq!(
        store
            .query("alpha", CHAIN, TARGET, "events", &Query::default())
            .unwrap_err()
            .category,
        "index_inconsistent"
    );
    store.backfill("alpha", CHAIN, TARGET, 0).unwrap();
    let result = scan_once(&store, &f.rpc, &options()).await.unwrap();
    assert_eq!(result["errors"], json!([]), "{result}");
}

#[tokio::test]
async fn abi_update_redecodes_without_rpc_and_retains_history() {
    let f = fixture().await;
    let (_dir, store) = store();
    store
        .register("alpha", CHAIN, TARGET, json!([]), 0, "basic")
        .unwrap();
    scan_once(&store, &f.rpc, &options()).await.unwrap();
    let page = store
        .query(
            "alpha",
            CHAIN,
            TARGET,
            "transactions",
            &Query {
                limit: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(page["data"][0]["decoded"]["status"], "unknown");
    let updated = store.update_abi("alpha", CHAIN, TARGET, abi(), 0).unwrap();
    assert_eq!(updated["redecoded"], true);
    let rows = store
        .query("alpha", CHAIN, TARGET, "transactions", &Query::default())
        .unwrap();
    assert_eq!(rows["data"][0]["decoded"]["signature"], "set(uint256)");
    assert_eq!(
        store.coverage("alpha", CHAIN, TARGET).unwrap()["next_block"],
        4
    );
    assert_eq!(
        store
            .abi_history("alpha", CHAIN, TARGET)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        store
            .query(
                "alpha",
                CHAIN,
                TARGET,
                "transactions",
                &Query {
                    cursor: page["next_cursor"].as_str().map(str::to_owned),
                    ..Default::default()
                }
            )
            .unwrap_err()
            .category,
        "snapshot_invalidated"
    );
}

#[test]
fn tokens_are_scoped_and_rotated_without_plaintext_storage() {
    let (_dir, store) = store();
    let created = store.create_project("beta", "Beta").unwrap();
    let token = created["tokens"]["manage"].as_str().unwrap();
    store.authorize("beta", token).unwrap();
    assert!(store.authorize("alpha", token).is_err());
    let rotated = store.rotate_key("beta").unwrap();
    store
        .authorize("beta", rotated["token"].as_str().unwrap())
        .unwrap();
    assert!(store.authorize("beta", token).is_err());
    assert!(store.projects().unwrap().to_string().find(token).is_none());
    assert!(store.create_project("../invalid", "Invalid").is_err());
}

#[tokio::test]
async fn persisted_chain_identity_protects_new_projects_after_restart() {
    let mut f = fixture().await;
    let (dir, store) = store();
    register(&store, "alpha", "basic");
    scan_once(&store, &f.rpc, &options()).await.unwrap();
    store.create_project("beta", "Beta").unwrap();
    register(&store, "beta", "basic");
    drop(store);
    let store = Store::open(dir.path().join("index.sqlite")).unwrap();
    f.chain.lock().unwrap().genesis_offset = 500;
    f.rpc = RpcPool::new(CHAIN, vec![f.url.clone()], RpcConfig::default()).unwrap();
    let result = scan_once(&store, &f.rpc, &options()).await.unwrap();
    assert_eq!(result["errors"].as_array().unwrap().len(), 2, "{result}");
    for error in result["errors"].as_array().unwrap() {
        assert_eq!(error["category"], "identity");
    }
    assert_eq!(
        store.coverage("beta", CHAIN, TARGET).unwrap()["next_block"],
        0
    );
}
#[tokio::test]
async fn chain_head_errors_are_persisted_for_each_registration() {
    let f = fixture().await;
    let (_dir, store) = store();
    register(&store, "alpha", "basic");
    store.create_project("beta", "Beta").unwrap();
    register(&store, "beta", "basic");
    f.chain.lock().unwrap().bad_head = true;
    let error = scan_once(&store, &f.rpc, &options()).await.unwrap_err();
    for project in ["alpha", "beta"] {
        assert_eq!(
            store.coverage(project, CHAIN, TARGET).unwrap()["last_error"],
            error.category
        );
    }
}
#[tokio::test]
async fn trace_misalignment_stops_before_block_commit() {
    let f = fixture().await;
    let (_dir, store) = store();
    register(&store, "alpha", "full");
    f.chain.lock().unwrap().bad_trace = true;
    let result = scan_once(&store, &f.rpc, &options()).await.unwrap();
    assert_eq!(
        result["errors"][0]["category"], "invalid_response",
        "{result}"
    );
    assert_eq!(
        store.coverage("alpha", CHAIN, TARGET).unwrap()["next_block"],
        1
    );
}
#[tokio::test]
async fn top_level_creation_without_events_is_indexed() {
    let f = fixture().await;
    let (_dir, store) = store();
    register(&store, "alpha", "basic");
    f.chain.lock().unwrap().add(1, 390, None, &[], 1, false);
    let result = scan_once(&store, &f.rpc, &options()).await.unwrap();
    assert_eq!(result["errors"], json!([]), "{result}");
    let context = store
        .context("alpha", CHAIN, TARGET, &h(390), &Query::default())
        .unwrap();
    assert!(context["data"]["transaction"]["relations"]
        .as_array()
        .unwrap()
        .contains(&json!("creation")));
}

#[test]
fn abi_provenance_survives_restart_and_revisions_preserve_each_source() {
    let (dir, store) = store();
    store
        .register_with_source("alpha", CHAIN, TARGET, abi(), 0, "basic", "sourcify")
        .unwrap();
    let history = store.abi_history("alpha", CHAIN, TARGET).unwrap();
    assert_eq!(history[0]["source"], "sourcify");
    store
        .update_abi_with_source("alpha", CHAIN, TARGET, json!([]), 1, "etherscan")
        .unwrap();
    store.update_abi("alpha", CHAIN, TARGET, abi(), 2).unwrap();
    assert_eq!(
        store
            .update_abi_with_source("alpha", CHAIN, TARGET, abi(), 3, "untrusted-provider")
            .unwrap_err()
            .category,
        "invalid_input"
    );
    drop(store);
    let store = Store::open(dir.path().join("index.sqlite")).unwrap();
    let history = store.abi_history("alpha", CHAIN, TARGET).unwrap();
    assert_eq!(history.as_array().unwrap().len(), 3);
    assert_eq!(history[0]["source"], "sourcify");
    assert_eq!(history[1]["source"], "etherscan");
    assert_eq!(history[2]["source"], "manual");
}

#[tokio::test]
async fn complete_transaction_trace_fallback_discovers_indirect_eventless_calls() {
    let f = fixture().await;
    let (_dir, store) = store();
    register(&store, "alpha", "full");
    {
        let mut chain = f.chain.lock().unwrap();
        chain.trace_supported = false;
        chain.transaction_trace_supported = true;
    }
    let result = scan_once(&store, &f.rpc, &options()).await.unwrap();
    assert_eq!(result["errors"], json!([]), "{result}");
    assert_eq!(result["scanned_blocks"], 4);
    let chain = f.chain.lock().unwrap();
    assert_eq!(
        chain.transaction_trace_calls,
        vec![h(300), h(301), h(302), h(303), h(304)]
    );
    drop(chain);
    let context = store
        .context("alpha", CHAIN, TARGET, &h(303), &Query::default())
        .unwrap();
    assert!(context["data"]["transaction"]["relations"]
        .as_array()
        .unwrap()
        .contains(&json!("call_trace")));
    assert_eq!(
        store.coverage("alpha", CHAIN, TARGET).unwrap()["trace_discovery"],
        "block_trace_or_complete_transaction_sweep"
    );
}

#[tokio::test]
async fn incomplete_transaction_trace_sweep_never_commits_partial_block() {
    let f = fixture().await;
    let (_dir, store) = store();
    register(&store, "alpha", "full");
    {
        let mut chain = f.chain.lock().unwrap();
        chain.trace_supported = false;
        chain.transaction_trace_supported = true;
        chain.unsupported_transaction = Some(h(304));
    }
    let result = scan_once(&store, &f.rpc, &options()).await.unwrap();
    assert_eq!(result["errors"][0]["category"], "unsupported", "{result}");
    assert_eq!(
        store.coverage("alpha", CHAIN, TARGET).unwrap()["next_block"],
        3
    );
    assert_eq!(
        store.coverage("alpha", CHAIN, TARGET).unwrap()["mode"],
        "full"
    );
    // The internal call was traced successfully, but its whole block must remain unpublished.
    assert!(store
        .context("alpha", CHAIN, TARGET, &h(303), &Query::default())
        .unwrap()["data"]
        .is_null());
    assert_eq!(
        f.chain.lock().unwrap().transaction_trace_calls,
        vec![h(300), h(301), h(302), h(303), h(304)]
    );
}

#[tokio::test]
async fn empty_full_blocks_require_no_trace_method() {
    let f = fixture().await;
    let (_dir, store) = store();
    register(&store, "alpha", "full");
    {
        let mut chain = f.chain.lock().unwrap();
        chain.blocks.retain(|number, _| *number == 0);
        chain.trace_supported = false;
        chain.transaction_trace_supported = false;
    }
    let result = scan_once(&store, &f.rpc, &options()).await.unwrap();
    assert_eq!(result["errors"], json!([]), "{result}");
    assert_eq!(result["scanned_blocks"], 1);
    let chain = f.chain.lock().unwrap();
    assert_eq!(chain.block_trace_calls, 0);
    assert!(chain.transaction_trace_calls.is_empty());
}
