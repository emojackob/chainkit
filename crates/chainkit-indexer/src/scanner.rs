use crate::{
    store::{unknown, Task},
    IndexerError, Result, Store,
};
use chainkit_abi::Abi;
use chainkit_evm::{normalize_address, normalize_hash, RpcPool};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScanOptions {
    pub confirmations: u64,
    pub max_blocks: u64,
    pub reorg_window: u64,
}
impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            confirmations: 12,
            max_blocks: 20,
            reorg_window: 256,
        }
    }
}
fn malformed(message: &str) -> IndexerError {
    IndexerError::new("invalid_response", message)
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| malformed("Required RPC string field is missing"))
}
fn quantity(value: &str) -> Result<u64> {
    if !value.starts_with("0x") || value.len() <= 2 || value.len() > 18 {
        return Err(malformed("Invalid RPC quantity"));
    }
    let n = u64::from_str_radix(&value[2..], 16).map_err(|_| malformed("Invalid RPC quantity"))?;
    if n > i64::MAX as u64 {
        return Err(malformed("RPC quantity exceeds storage range"));
    }
    Ok(n)
}
fn number(value: &Value, name: &str) -> Result<u64> {
    quantity(field(value, name)?)
}
fn address(value: &str) -> Result<String> {
    normalize_address(value).map_err(|_| malformed("Invalid RPC address"))
}
fn hash(value: &str) -> Result<String> {
    normalize_hash(value).map_err(|_| malformed("Invalid RPC hash"))
}
fn optional_address(value: &Value, name: &str) -> Result<Option<String>> {
    match value.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(address(s)?)),
        _ => Err(malformed("Invalid optional RPC address")),
    }
}
fn valid_bytes(value: &str) -> Result<()> {
    if !value.starts_with("0x")
        || !value.len().is_multiple_of(2)
        || !value[2..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(malformed("Invalid RPC hex bytes"));
    }
    Ok(())
}
fn block_identity(block: &Value, expected: u64) -> Result<(String, String)> {
    if !block.is_object() {
        return Err(IndexerError::new(
            "history_unavailable",
            "Requested block is unavailable",
        ));
    }
    if number(block, "number")? != expected {
        return Err(malformed("RPC returned another block number"));
    }
    Ok((
        hash(field(block, "hash")?)?,
        hash(field(block, "parentHash")?)?,
    ))
}

struct Cache<'a> {
    rpc: &'a RpcPool,
    values: HashMap<String, Value>,
    bytes: usize,
}
impl<'a> Cache<'a> {
    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let key = format!("{method}:{params}");
        if let Some(value) = self.values.get(&key) {
            return Ok(value.clone());
        }
        let value = self.rpc.call(method, params).await?;
        let size = value.to_string().len();
        if self.bytes.saturating_add(size) > 64 * 1024 * 1024 {
            self.values.clear();
            self.bytes = 0;
        }
        if size <= 64 * 1024 * 1024 {
            self.values.insert(key, value.clone());
            self.bytes += size;
        }
        Ok(value)
    }
}

/// Scan a bounded round for every active registration on the pool's chain.
///
/// Errors are recorded per registration and returned in the `errors` array.
/// A failure for one project does not prevent the remaining projects from running.
/// Acquisition is reused within the round; canonicality is revalidated at commit.
pub async fn scan_once(store: &Store, rpc: &RpcPool, options: &ScanOptions) -> Result<Value> {
    if options.max_blocks == 0
        || options.max_blocks > 1000
        || options.reorg_window == 0
        || options.reorg_window > 100_000
        || options.confirmations > 100_000
    {
        return Err(IndexerError::new("invalid_input", "Invalid scan limits"));
    }
    let mut cache = Cache {
        rpc,
        values: HashMap::new(),
        bytes: 0,
    };
    let tasks = store.tasks(rpc.chain_id())?;
    if tasks.is_empty() {
        return Ok(json!({"scanned_blocks":0,"registrations":[],"errors":[]}));
    }
    let anchor: Result<(u64, String)> = async {
        let head_raw = rpc.call("eth_blockNumber", json!([])).await?;
        let head = quantity(
            head_raw
                .as_str()
                .ok_or_else(|| malformed("RPC head is not a quantity"))?,
        )?;
        let genesis = rpc
            .call("eth_getBlockByNumber", json!(["0x0", false]))
            .await?;
        let (genesis_hash, _) = block_identity(&genesis, 0)?;
        Ok((head, genesis_hash))
    }
    .await;
    let (head, genesis_hash) = match anchor {
        Ok(anchor) => anchor,
        Err(error) => {
            for task in &tasks {
                store.record_error(task, &error.category)?;
            }
            return Err(error);
        }
    };
    let mut completed = Vec::new();
    let mut errors = Vec::new();
    let mut total = 0;
    for initial in tasks {
        let identity = json!({"project":initial.project,"chain_id":initial.chain_id,"address":initial.address});
        let result =
            scan_registration(store, &initial, &mut cache, options, head, &genesis_hash).await;
        match result {
            Ok(count) => {
                total += count;
                completed.push(json!({"registration":identity,"scanned_blocks":count,"coverage":store.coverage(&initial.project,initial.chain_id,&initial.address)?}));
            }
            Err(error) => {
                let task = store
                    .task(&initial.project, initial.chain_id, &initial.address)
                    .unwrap_or(initial);
                tracing::warn!(project=%task.project,chain_id=task.chain_id,address=%task.address,next_block=task.next_block,error=%error.category,"Registration scan stopped; incomplete block was not committed");
                errors.push(json!({"registration":identity,"category":error.category,"message":error.message}));
            }
        }
    }
    Ok(json!({"scanned_blocks":total,"registrations":completed,"errors":errors}))
}

async fn scan_registration(
    store: &Store,
    initial: &Task,
    cache: &mut Cache<'_>,
    options: &ScanOptions,
    head: u64,
    genesis: &str,
) -> Result<u64> {
    if let Err(error) = store.bind_genesis(initial, genesis) {
        store.record_error(initial, &error.category)?;
        return Err(error);
    }
    if let Err(error) = recover(store, initial, cache.rpc, options.reorg_window).await {
        if error.category != "stale_task" {
            store.record_error(initial, &error.category)?;
        }
        return Err(error);
    }
    let mut task = store.task(&initial.project, initial.chain_id, &initial.address)?;
    store.record_head(&task, head)?;
    let safe = head.checked_sub(options.confirmations);
    let mut count = 0;
    while safe.is_some_and(|safe| task.next_block <= safe) && count < options.max_blocks {
        if task.paused {
            break;
        }
        if let Err(error) = scan_block(store, &task, cache, head).await {
            if error.category != "stale_task" {
                store.record_error(&task, &error.category)?;
            }
            return Err(error);
        }
        count += 1;
        tracing::info!(project=%task.project,chain_id=task.chain_id,address=%task.address,committed_block=task.next_block,batch_blocks=count,safe_block=safe,"Block indexed");
        task = store.task(&task.project, task.chain_id, &task.address)?;
    }
    // Clear transient failures after a successful verification even when caught up.
    store.clear_error(&task)?;
    Ok(count)
}
async fn recover(store: &Store, task: &Task, rpc: &RpcPool, window: u64) -> Result<()> {
    if task.next_block <= task.start_block {
        return Ok(());
    }
    let end = task.next_block - 1;
    let remote = rpc
        .call("eth_getBlockByNumber", json!([format!("0x{end:x}"), false]))
        .await?;
    let (remote_hash, _) = block_identity(&remote, end)?;
    if store.block_hash(task.id, end)?.as_deref() == Some(&remote_hash) {
        return Ok(());
    }
    let lower = task.start_block.max(end.saturating_sub(window - 1));
    for number in (lower..end).rev() {
        let block = rpc
            .call(
                "eth_getBlockByNumber",
                json!([format!("0x{number:x}"), false]),
            )
            .await?;
        let (remote_hash, _) = block_identity(&block, number)?;
        if store.block_hash(task.id, number)?.as_deref() == Some(&remote_hash) {
            store.rewind(task, number + 1)?;
            return Ok(());
        }
    }
    if lower == task.start_block {
        store.rewind(task, lower)?;
        Ok(())
    } else {
        Err(IndexerError::new(
            "deep_reorg",
            "No common ancestor in recovery window; administrator backfill required",
        ))
    }
}

async fn scan_block(store: &Store, task: &Task, cache: &mut Cache<'_>, head: u64) -> Result<()> {
    let number = task.next_block;
    let block = cache
        .call(
            "eth_getBlockByNumber",
            json!([format!("0x{number:x}"), true]),
        )
        .await?;
    let (block_hash, parent_hash) = block_identity(&block, number)?;
    if number > task.start_block
        && store.block_hash(task.id, number - 1)?.as_deref() != Some(&parent_hash)
    {
        return Err(IndexerError::new(
            "reorg",
            "Block parent differs from the indexed chain",
        ));
    }
    let transactions = block
        .get("transactions")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed("Full block transactions are missing"))?;
    let mut positions = HashMap::new();
    for (index, tx) in transactions.iter().enumerate() {
        if number_of(tx, "transactionIndex")? != index as u64
            || number_of(tx, "blockNumber")? != number
            || hash(field(tx, "blockHash")?)? != block_hash
        {
            return Err(malformed("Transaction inclusion does not match block"));
        }
        let tx_hash = hash(field(tx, "hash")?)?;
        if positions.insert(tx_hash, index as u64).is_some() {
            return Err(malformed("Duplicate transaction in block"));
        }
        address(field(tx, "from")?)?;
        optional_address(tx, "to")?;
        valid_bytes(field(tx, "input")?)?;
    }
    // Fetch block-local logs; on provider overflow use receipt evidence instead.
    let fetched=cache.call("eth_getLogs",json!([{"address":task.address,"fromBlock":format!("0x{number:x}"),"toBlock":format!("0x{number:x}")}])).await;
    let receipt_fallback = matches!(&fetched,Err(error) if error.category=="too_large");
    let found = match fetched {
        Ok(Value::Array(logs)) => logs,
        Ok(_) => return Err(malformed("RPC logs are not an array")),
        Err(_) if receipt_fallback => Vec::new(),
        Err(error) => return Err(error),
    };
    let mut expected = HashMap::new();
    let mut event_transactions = HashSet::new();
    for log in &found {
        let tx_hash = hash(field(log, "transactionHash")?)?;
        let index = *positions
            .get(&tx_hash)
            .ok_or_else(|| malformed("Log refers to a transaction outside the block"))?;
        validate_log(log, &tx_hash, &block_hash, number, index)?;
        if address(field(log, "address")?)? != task.address {
            return Err(malformed("RPC log is outside the requested address filter"));
        }
        let key = (tx_hash.clone(), number_of(log, "logIndex")?);
        if expected.insert(key, log.clone()).is_some() {
            return Err(malformed("Duplicate getLogs entry"));
        }
        event_transactions.insert(tx_hash);
    }
    let mut traces = HashMap::new();
    if task.mode == "full" && !transactions.is_empty() {
        let response = match cache
            .call(
                "debug_traceBlockByNumber",
                json!([format!("0x{number:x}"),{"tracer":"callTracer","timeout":"10s"}]),
            )
            .await
        {
            Ok(response) => response,
            Err(error) if error.category == "unsupported" => {
                // A complete per-transaction sweep has the same discovery
                // scope as block tracing. Never restrict this to candidates
                // already found through events or direct calls.
                let mut items = Vec::with_capacity(transactions.len());
                for tx in transactions {
                    let tx_hash = hash(field(tx, "hash")?)?;
                    let trace = cache
                        .call(
                            "debug_traceTransaction",
                            json!([tx_hash,{"tracer":"callTracer","timeout":"10s"}]),
                        )
                        .await?;
                    items.push(json!({"txHash":tx_hash,"result":trace}));
                }
                Value::Array(items)
            }
            Err(error) => return Err(error),
        };
        let items = response
            .as_array()
            .ok_or_else(|| malformed("Block traces are not an array"))?;
        if items.len() != transactions.len() {
            return Err(malformed("Block trace coverage is incomplete"));
        }
        for (tx, item) in transactions.iter().zip(items) {
            if item.get("error").is_some() {
                return Err(malformed("A transaction could not be traced"));
            }
            let tx_hash = hash(field(tx, "hash")?)?;
            if item
                .get("txHash")
                .and_then(Value::as_str)
                .map(hash)
                .transpose()?
                .is_some_and(|h| h != tx_hash)
            {
                return Err(malformed("Trace transaction identity differs from block"));
            }
            let trace = item
                .get("result")
                .filter(|v| v.is_object())
                .ok_or_else(|| malformed("callTracer result missing"))?;
            if address(field(trace, "from")?)? != address(field(tx, "from")?)?
                || !field(trace, "input")?.eq_ignore_ascii_case(field(tx, "input")?)
                || optional_address(tx, "to")?.is_some_and(|to| {
                    optional_address(trace, "to").ok().flatten().as_deref() != Some(&to)
                })
            {
                return Err(malformed("Trace root differs from block transaction"));
            }
            related_trace(trace, &task.address)?;
            traces.insert(tx_hash, trace.clone());
        }
    }
    let abi = store.abi_for(task.id, number)?;
    let mut tx_rows = Vec::new();
    let mut event_rows = Vec::new();
    let mut seen_logs = HashSet::new();
    for (index, tx) in transactions.iter().enumerate() {
        let tx_hash = hash(field(tx, "hash")?)?;
        let sender = address(field(tx, "from")?)?;
        let recipient = optional_address(tx, "to")?;
        let mut relations = Vec::new();
        if recipient.as_deref() == Some(&task.address) {
            relations.push("direct_call");
        }
        if sender == task.address {
            relations.push("sent_transaction");
        }
        if event_transactions.contains(&tx_hash) {
            relations.push("emitted_event");
        }
        let trace = traces.get(&tx_hash);
        if let Some(trace) = trace {
            if related_trace(trace, &task.address)? {
                relations.push("call_trace");
            }
        }
        if relations.is_empty() && recipient.is_some() && !receipt_fallback {
            continue;
        }
        let receipt = cache
            .call("eth_getTransactionReceipt", json!([tx_hash]))
            .await?;
        if hash(field(&receipt, "transactionHash")?)? != tx_hash
            || hash(field(&receipt, "blockHash")?)? != block_hash
            || number_of(&receipt, "blockNumber")? != number
            || number_of(&receipt, "transactionIndex")? != index as u64
        {
            return Err(IndexerError::new(
                "reorg",
                "Receipt inclusion differs from scanned block",
            ));
        }
        if optional_address(&receipt, "contractAddress")?.as_deref() == Some(&task.address) {
            relations.push("creation");
        }
        let logs = receipt
            .get("logs")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed("Receipt logs missing"))?;
        if receipt_fallback
            && logs.iter().any(|log| {
                log.get("address")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.eq_ignore_ascii_case(&task.address))
            })
            && !relations.contains(&"emitted_event")
        {
            relations.push("emitted_event");
        }
        if relations.is_empty() {
            continue;
        }
        let status = number_of(&receipt, "status")?;
        if status > 1 || status == 0 && !logs.is_empty() {
            return Err(malformed("Receipt status/log combination is invalid"));
        }
        let decoded = if recipient.as_deref() == Some(&task.address) {
            abi.decode_function(field(tx, "input")?)
        } else {
            unknown("Top-level transaction uses another contract ABI")
        };
        for log in logs {
            validate_log(log, &tx_hash, &block_hash, number, index as u64)?;
            let log_index = number_of(log, "logIndex")?;
            if !seen_logs.insert(log_index) {
                return Err(malformed("Duplicate receipt log position"));
            }
            let emitter = address(field(log, "address")?)?;
            let topics = log["topics"]
                .as_array()
                .ok_or_else(|| malformed("Log topics missing"))?;
            let decoded = if emitter == task.address {
                if !relations.contains(&"emitted_event") {
                    relations.push("emitted_event");
                }
                if let Some(expected) = expected.remove(&(tx_hash.clone(), log_index)) {
                    if !field(&expected, "data")?.eq_ignore_ascii_case(field(log, "data")?)
                        || expected["topics"].to_string().to_lowercase()
                            != log["topics"].to_string().to_lowercase()
                    {
                        return Err(malformed("getLogs and receipt disagree"));
                    }
                }
                abi.decode_event(log)
            } else {
                unknown("Log emitter ABI is not bound")
            };
            event_rows.push(json!({"tx_hash":tx_hash,"tx_index":index,"log_index":log_index,"emitter":emitter,"topic0":topics.first().map(|t|hash(t.as_str().unwrap())).transpose()?,"raw":log,"decoded":decoded}));
        }
        let trace = trace
            .map(|value| annotate_trace(value, &task.address, &abi))
            .transpose()?;
        relations.sort_unstable();
        relations.dedup();
        tx_rows.push(json!({"hash":tx_hash,"tx_index":index,"sender":sender,"recipient":recipient,"status":status,"relations":relations,"raw":tx,"receipt":receipt,"decoded":decoded,"trace":trace}));
    }
    if !expected.is_empty() {
        return Err(malformed("getLogs returned events absent from receipts"));
    }
    let confirmed = cache
        .rpc
        .call(
            "eth_getBlockByNumber",
            json!([format!("0x{number:x}"), false]),
        )
        .await?;
    if block_identity(&confirmed, number)?.0 != block_hash {
        return Err(IndexerError::new("reorg", "Block changed before commit"));
    }
    store.commit_block(task, &block, &tx_rows, &event_rows, head)
}
fn number_of(value: &Value, name: &str) -> Result<u64> {
    number(value, name)
}
fn validate_log(
    log: &Value,
    tx_hash: &str,
    block_hash: &str,
    number: u64,
    tx_index: u64,
) -> Result<()> {
    if hash(field(log, "transactionHash")?)? != tx_hash
        || hash(field(log, "blockHash")?)? != block_hash
        || number_of(log, "blockNumber")? != number
        || number_of(log, "transactionIndex")? != tx_index
        || log.get("removed").and_then(Value::as_bool).unwrap_or(false)
    {
        return Err(malformed("Log inclusion does not match receipt/block"));
    }
    number_of(log, "logIndex")?;
    address(field(log, "address")?)?;
    let topics = log
        .get("topics")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed("Log topics missing"))?;
    if topics.len() > 4 {
        return Err(malformed("Log has more than four topics"));
    }
    for topic in topics {
        hash(
            topic
                .as_str()
                .ok_or_else(|| malformed("Invalid log topic"))?,
        )?;
    }
    valid_bytes(field(log, "data")?)
}

fn related_trace(root: &Value, target: &str) -> Result<bool> {
    let mut stack = vec![(root, None::<String>, 0usize)];
    let mut matched = false;
    let mut budget = 100_000;
    while let Some((frame, parent_context, depth)) = stack.pop() {
        if depth > 256 {
            return Err(IndexerError::new(
                "too_large",
                "Trace depth budget exceeded",
            ));
        }
        if budget == 0 {
            return Err(IndexerError::new(
                "too_large",
                "Trace frame budget exceeded",
            ));
        }
        budget -= 1;
        let kind = field(frame, "type")?;
        let sender = optional_address(frame, "from")?;
        let to = optional_address(frame, "to")?;
        let context = if ["DELEGATECALL", "CALLCODE"].contains(&kind) {
            parent_context
        } else {
            to.clone()
        };
        matched |= [sender.as_deref(), to.as_deref(), context.as_deref()].contains(&Some(target));
        if let Some(calls) = frame.get("calls") {
            let calls = calls
                .as_array()
                .ok_or_else(|| malformed("Trace calls must be an array"))?;
            stack.extend(
                calls
                    .iter()
                    .map(|child| (child, context.clone(), depth + 1)),
            );
        }
    }
    Ok(matched)
}
pub(crate) fn annotate_trace(root: &Value, target: &str, abi: &Abi) -> Result<Value> {
    let mut stack = vec![(root, None::<String>, Vec::<usize>::new(), false)];
    let mut frames = Vec::new();
    let mut budget = 100_000;
    while let Some((frame, parent_context, path, parent_reverted)) = stack.pop() {
        if path.len() > 256 {
            return Err(IndexerError::new(
                "too_large",
                "Trace depth budget exceeded",
            ));
        }
        if budget == 0 {
            return Err(IndexerError::new(
                "too_large",
                "Trace frame budget exceeded",
            ));
        }
        budget -= 1;
        let kind = field(frame, "type")?;
        let sender = optional_address(frame, "from")?;
        let to = optional_address(frame, "to")?;
        let context = if ["DELEGATECALL", "CALLCODE"].contains(&kind) {
            parent_context
        } else {
            to.clone()
        };
        let reverted = parent_reverted || frame.get("error").is_some();
        if [sender.as_deref(), to.as_deref(), context.as_deref()].contains(&Some(target)) {
            let uses_abi = context.as_deref() == Some(target) || to.as_deref() == Some(target);
            let decoded = if uses_abi {
                abi.decode_function(frame.get("input").and_then(Value::as_str).unwrap_or("0x"))
            } else {
                unknown("Call uses another contract ABI")
            };
            let decoded_error = if uses_abi && frame.get("error").is_some() {
                Some(abi.decode_error(frame.get("output").and_then(Value::as_str).unwrap_or("0x")))
            } else {
                None
            };
            frames.push(json!({"trace_path":path,"type":kind,"context_address":context,"code_address":to,"reverted":reverted,"error":frame.get("error"),"decoded_input":decoded,"decoded_error":decoded_error}));
        }
        if let Some(calls) = frame.get("calls") {
            let calls = calls
                .as_array()
                .ok_or_else(|| malformed("Trace calls must be an array"))?;
            for (index, child) in calls.iter().enumerate().rev() {
                let mut child_path = path.clone();
                child_path.push(index);
                stack.push((child, context.clone(), child_path, reverted));
            }
        }
    }
    Ok(json!({"raw":root,"related_frames":frames,"abi_binding":"operator_supplied_block_level"}))
}
