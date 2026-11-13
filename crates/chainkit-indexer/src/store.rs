use crate::{IndexerError, Result};
use chainkit_abi::Abi;
use chainkit_evm::{normalize_address, normalize_hash};
use hmac::{Hmac, Mac};
use rusqlite::{params, types::Value as SqlValue, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

type HmacSha256 = Hmac<Sha256>;
const APPLICATION_ID: i64 = 0x434b4958;
const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Query {
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    pub from_block: Option<u64>,
    pub to_block: Option<u64>,
    pub tx_hash: Option<String>,
    pub sender: Option<String>,
    pub status: Option<u64>,
    pub signature: Option<String>,
    pub topic0: Option<String>,
}

#[derive(Clone)]
pub struct Store {
    db: Arc<Mutex<Connection>>,
}
impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Task {
    pub id: i64,
    pub project: String,
    pub chain_id: u64,
    pub address: String,
    pub start_block: u64,
    pub next_block: u64,
    pub mode: String,
    pub paused: bool,
    pub revision: u64,
    pub genesis_hash: Option<String>,
    pub head: Option<u64>,
    pub last_error: Option<String>,
    pub updated_at: Option<String>,
}
const TASK_FIELDS: &str = "id,project,chain_id,address,start_block,next_block,mode,paused,revision,genesis_hash,head,last_error,updated_at";
fn row_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
fn task_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get(0)?,
        project: row.get(1)?,
        chain_id: row_u64(row, 2)?,
        address: row.get(3)?,
        start_block: row_u64(row, 4)?,
        next_block: row_u64(row, 5)?,
        mode: row.get(6)?,
        paused: row.get(7)?,
        revision: row_u64(row, 8)?,
        genesis_hash: row.get(9)?,
        head: row.get::<_, Option<i64>>(10)?.map(|n| n as u64),
        last_error: row.get(11)?,
        updated_at: row.get(12)?,
    })
}
fn invalid(message: &str) -> IndexerError {
    IndexerError::new("invalid_input", message)
}
fn integer(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| invalid("Integer exceeds supported range"))
}
fn address(value: &str) -> Result<String> {
    normalize_address(value).map_err(|_| invalid("Invalid EVM address"))
}
fn hash(value: &str) -> Result<String> {
    normalize_hash(value).map_err(|_| invalid("Invalid EVM hash"))
}
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn random_token() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}
fn abi_json(value: Value) -> Result<(Value, String)> {
    let normalized = if value.is_array() {
        value
    } else {
        value
            .get("abi")
            .cloned()
            .ok_or_else(|| invalid("ABI must be an array or a compiled artifact"))?
    };
    Abi::new(normalized.clone())
        .map_err(|_| IndexerError::new("invalid_abi", "Invalid contract ABI"))?;
    let encoded = serde_json::to_vec(&normalized)?;
    Ok((normalized, digest(&encoded)))
}
fn check_abi_source(source: &str) -> Result<()> {
    if !["manual", "sourcify", "etherscan"].contains(&source) {
        return Err(invalid("ABI source must be manual, sourcify or etherscan"));
    }
    Ok(())
}
fn check_project(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err(invalid(
            "Project ID must contain 1..128 ASCII letters, digits, hyphens or underscores",
        ));
    }
    Ok(())
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )?;
        let txn = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let app: i64 = txn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let version: i64 = txn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if app != 0 && app != APPLICATION_ID {
            return Err(IndexerError::new(
                "storage",
                "Database belongs to another application",
            ));
        }
        if version == 0 {
            let table_count: i64 = txn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'", [], |r| r.get(0))?;
            if table_count != 0 {
                return Err(IndexerError::new(
                    "storage",
                    "Unrecognized database schema; use a separate Rust indexer database",
                ));
            }
            txn.execute_batch(include_str!("schema.sql"))?;
            txn.execute("INSERT INTO meta VALUES('cursor_key',?)", [random_token()])?;
            txn.pragma_update(None, "application_id", APPLICATION_ID)?;
            txn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if app != APPLICATION_ID || version != SCHEMA_VERSION {
            return Err(IndexerError::new(
                "storage",
                "Unsupported database schema version",
            ));
        }
        txn.commit()?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
        })
    }
    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.db
            .lock()
            .map_err(|_| IndexerError::new("storage", "Database lock unavailable"))
    }
    pub fn create_project(&self, id: &str, name: &str) -> Result<Value> {
        check_project(id)?;
        if name.is_empty() || name.len() > 256 {
            return Err(invalid("Project name must contain 1..256 bytes"));
        }
        let token = random_token();
        let db = self.lock()?;
        if db
            .query_row("SELECT 1 FROM projects WHERE id=?", [id], |_| Ok(()))
            .optional()?
            .is_some()
        {
            return Err(IndexerError::new("conflict", "Project already exists"));
        }
        db.execute(
            "INSERT INTO projects(id,name,manage_digest) VALUES(?,?,?)",
            params![id, name, digest(token.as_bytes())],
        )?;
        Ok(json!({"id":id,"name":name,"tokens":{"manage":token}}))
    }
    pub fn projects(&self) -> Result<Value> {
        let db = self.lock()?;
        let mut stmt = db.prepare("SELECT id,name,created_at FROM projects ORDER BY id")?;
        let rows = stmt.query_map([], |r| Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"created_at":r.get::<_,String>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!(rows))
    }
    pub fn authorize(&self, project: &str, token: &str) -> Result<()> {
        if token.len() != 64 {
            return Err(IndexerError::new(
                "forbidden",
                "Project manage token required",
            ));
        }
        let db = self.lock()?;
        let got = digest(token.as_bytes());
        let allowed = db
            .query_row(
                "SELECT 1 FROM projects WHERE id=? AND manage_digest=?",
                params![project, got],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !allowed {
            return Err(IndexerError::new(
                "forbidden",
                "Project manage token required",
            ));
        }
        Ok(())
    }
    pub fn rotate_key(&self, project: &str) -> Result<Value> {
        let token = random_token();
        let db = self.lock()?;
        if db.execute(
            "UPDATE projects SET manage_digest=? WHERE id=?",
            params![digest(token.as_bytes()), project],
        )? == 0
        {
            return Err(IndexerError::new("not_found", "Project not found"));
        }
        Ok(json!({"role":"manage","token":token}))
    }
    pub fn contracts(&self, project: &str) -> Result<Value> {
        let db = self.lock()?;
        Self::require_project(&db, project)?;
        let mut stmt = db.prepare(&format!(
            "SELECT {TASK_FIELDS} FROM contracts WHERE project=? ORDER BY chain_id,address"
        ))?;
        let tasks = stmt
            .query_map([project], task_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(serde_json::to_value(tasks)?)
    }
    pub fn register(
        &self,
        project: &str,
        chain_id: u64,
        contract: &str,
        abi: Value,
        start_block: u64,
        mode: &str,
    ) -> Result<Value> {
        self.register_with_source(
            project,
            chain_id,
            contract,
            abi,
            start_block,
            mode,
            "manual",
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn register_with_source(
        &self,
        project: &str,
        chain_id: u64,
        contract: &str,
        abi: Value,
        start_block: u64,
        mode: &str,
        source: &str,
    ) -> Result<Value> {
        check_abi_source(source)?;
        check_project(project)?;
        if chain_id == 0 || !["basic", "full"].contains(&mode) {
            return Err(invalid("Positive chain ID and basic/full mode required"));
        }
        let chain = integer(chain_id)?;
        let start = integer(start_block)?;
        if start == i64::MAX {
            return Err(invalid("Starting block is too large"));
        }
        let contract = address(contract)?;
        let (abi, abi_hash) = abi_json(abi)?;
        let mut db = self.lock()?;
        let txn = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::require_project(&txn, project)?;
        if Self::task_in(&txn, project, chain_id, &contract).is_ok() {
            return Err(IndexerError::new(
                "conflict",
                "Contract is already registered in this project",
            ));
        }
        txn.execute("INSERT INTO contracts(project,chain_id,address,start_block,next_block,mode) VALUES(?,?,?,?,?,?)", params![project,chain,contract,start,start,mode])?;
        let id = txn.last_insert_rowid();
        txn.execute(
            "INSERT INTO abi_bindings VALUES(?,?,?,?,?)",
            params![id, start, abi_hash, abi.to_string(), source],
        )?;
        txn.execute("INSERT INTO abi_revisions(contract_id,revision,effective_from,abi_hash,abi,source) VALUES(?,0,?,?,?,?)", params![id,start,abi_hash,abi.to_string(),source])?;
        let task = Self::task_in(&txn, project, chain_id, &contract)?;
        txn.commit()?;
        Ok(serde_json::to_value(task)?)
    }
    fn require_project(db: &Connection, project: &str) -> Result<()> {
        if db
            .query_row("SELECT 1 FROM projects WHERE id=?", [project], |_| Ok(()))
            .optional()?
            .is_none()
        {
            return Err(IndexerError::new("not_found", "Project not found"));
        }
        Ok(())
    }
    fn task_in(db: &Connection, project: &str, chain_id: u64, contract: &str) -> Result<Task> {
        db.query_row(
            &format!(
                "SELECT {TASK_FIELDS} FROM contracts WHERE project=? AND chain_id=? AND address=?"
            ),
            params![project, integer(chain_id)?, contract],
            task_row,
        )
        .optional()?
        .ok_or_else(|| IndexerError::new("not_found", "Contract registration not found"))
    }
    pub(crate) fn task(&self, project: &str, chain_id: u64, contract: &str) -> Result<Task> {
        Self::task_in(&*self.lock()?, project, chain_id, &address(contract)?)
    }
    pub(crate) fn tasks(&self, chain_id: u64) -> Result<Vec<Task>> {
        let db = self.lock()?;
        let mut stmt = db.prepare(&format!(
            "SELECT {TASK_FIELDS} FROM contracts WHERE chain_id=? AND paused=0 ORDER BY id"
        ))?;
        let rows = stmt
            .query_map([integer(chain_id)?], task_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    pub fn coverage(&self, project: &str, chain_id: u64, contract: &str) -> Result<Value> {
        let task = self.task(project, chain_id, contract)?;
        Ok(Self::coverage_of(&task))
    }
    fn coverage_of(t: &Task) -> Value {
        let trace = t.mode == "full";
        json!({"project":t.project,"chain_id":t.chain_id,"address":t.address,
            "mode":t.mode,"start_block":t.start_block,"next_block":t.next_block,
            "indexed_range":{"from_block":t.start_block,"to_block":t.next_block},
            "paused":t.paused,"head":t.head,"last_error":t.last_error,
            "revision":t.revision,"genesis_hash":t.genesis_hash,"updated_at":t.updated_at,
            "capabilities":{"emitted_events":true,"direct_transactions":true,"contract_creation":true,"internal_calls":trace,"external_token_transfers":false},
            "trace_discovery":if trace { Some("block_trace_or_complete_transaction_sweep") } else { None::<&str> },
            "limitations":["Coverage depends on RPC completeness and configured historical availability","ABI bindings use block granularity","External token relationships are not indexed automatically"]})
    }
    pub fn set_paused(
        &self,
        project: &str,
        chain_id: u64,
        contract: &str,
        paused: bool,
    ) -> Result<Value> {
        let mut db = self.lock()?;
        let txn = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let task = Self::task_in(&txn, project, chain_id, &address(contract)?)?;
        txn.execute(
            "UPDATE contracts SET paused=?,revision=revision+1 WHERE id=?",
            params![paused, task.id],
        )?;
        let task = Self::task_in(&txn, project, chain_id, &task.address)?;
        txn.commit()?;
        Ok(Self::coverage_of(&task))
    }
    pub fn backfill(
        &self,
        project: &str,
        chain_id: u64,
        contract: &str,
        from_block: u64,
    ) -> Result<Value> {
        let mut db = self.lock()?;
        let txn = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let task = Self::task_in(&txn, project, chain_id, &address(contract)?)?;
        if from_block < task.start_block || from_block > task.next_block {
            return Err(invalid("Backfill block must be within the indexed range"));
        }
        Self::rewind_in(&txn, &task, from_block)?;
        let task = Self::task_in(&txn, project, chain_id, &task.address)?;
        txn.commit()?;
        Ok(Self::coverage_of(&task))
    }
    pub fn abi_history(&self, project: &str, chain_id: u64, contract: &str) -> Result<Value> {
        let db = self.lock()?;
        let task = Self::task_in(&db, project, chain_id, &address(contract)?)?;
        let mut stmt = db.prepare("SELECT revision,effective_from,abi_hash,abi,source,created_at FROM abi_revisions WHERE contract_id=? ORDER BY revision")?;
        let rows = stmt
            .query_map([task.id], |r| {
                Ok((
                    row_u64(r, 0)?,
                    row_u64(r, 1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let entries = rows.into_iter().map(|(revision,effective_from,abi_hash,abi,source,created_at)| Ok(json!({"revision":revision,"effective_from":effective_from,"abi_hash":abi_hash,"abi":serde_json::from_str::<Value>(&abi)?,"created_at":created_at,"source":source}))).collect::<Result<Vec<_>>>()?;
        Ok(json!(entries))
    }
    pub fn update_abi(
        &self,
        project: &str,
        chain_id: u64,
        contract: &str,
        abi: Value,
        effective_from: u64,
    ) -> Result<Value> {
        self.update_abi_with_source(project, chain_id, contract, abi, effective_from, "manual")
    }
    pub fn update_abi_with_source(
        &self,
        project: &str,
        chain_id: u64,
        contract: &str,
        abi: Value,
        effective_from: u64,
        source: &str,
    ) -> Result<Value> {
        check_abi_source(source)?;
        let (abi, abi_hash) = abi_json(abi)?;
        let decoder = Abi::new(abi.clone()).map_err(|_| invalid("Invalid ABI"))?;
        let mut db = self.lock()?;
        let txn = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let task = Self::task_in(&txn, project, chain_id, &address(contract)?)?;
        if effective_from < task.start_block {
            return Err(invalid("ABI effective block precedes registration start"));
        }
        let effective = integer(effective_from)?;
        txn.execute("INSERT INTO abi_bindings VALUES(?,?,?,?,?) ON CONFLICT(contract_id,effective_from) DO UPDATE SET abi_hash=excluded.abi_hash,abi=excluded.abi,source=excluded.source",params![task.id,effective,abi_hash,abi.to_string(),source])?;
        let revision: i64 = txn.query_row(
            "SELECT COALESCE(MAX(revision),0)+1 FROM abi_revisions WHERE contract_id=?",
            [task.id],
            |r| r.get(0),
        )?;
        txn.execute("INSERT INTO abi_revisions(contract_id,revision,effective_from,abi_hash,abi,source) VALUES(?,?,?,?,?,?)",params![task.id,revision,effective,abi_hash,abi.to_string(),source])?;
        let end: i64 = txn.query_row("SELECT COALESCE(MIN(effective_from),9223372036854775807) FROM abi_bindings WHERE contract_id=? AND effective_from>?",params![task.id,effective],|r|r.get(0))?;
        // Redecode retained raw evidence inside the same transaction. No RPC is needed.
        let rows = {
            let mut stmt = txn.prepare("SELECT block_hash,hash,raw,trace FROM transactions WHERE contract_id=? AND block_number>=? AND block_number<?")?;
            let rows = stmt
                .query_map(params![task.id, effective, end], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for (block_hash, tx_hash, raw, trace) in rows {
            let raw: Value = serde_json::from_str(&raw)?;
            let decoded = if raw
                .get("to")
                .and_then(Value::as_str)
                .is_some_and(|v| v.eq_ignore_ascii_case(&task.address))
            {
                decoder.decode_function(raw.get("input").and_then(Value::as_str).unwrap_or("0x"))
            } else {
                unknown("Top-level transaction uses another contract ABI")
            };
            let trace = trace
                .map(|s| -> Result<String> {
                    let v: Value = serde_json::from_str(&s)?;
                    Ok(crate::scanner::annotate_trace(
                        v.get("raw").unwrap_or(&v),
                        &task.address,
                        &decoder,
                    )?
                    .to_string())
                })
                .transpose()?;
            txn.execute("UPDATE transactions SET decoded=?,signature=?,trace=? WHERE contract_id=? AND block_hash=? AND hash=?",params![decoded.to_string(),decoded_signature(&decoded),trace,task.id,block_hash,tx_hash])?;
        }
        let rows = {
            let mut stmt = txn.prepare("SELECT block_hash,log_index,raw FROM events WHERE contract_id=? AND emitter=? AND block_number>=? AND block_number<?")?;
            let rows = stmt
                .query_map(params![task.id, task.address, effective, end], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        row_u64(r, 1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for (block_hash, idx, raw) in rows {
            let raw: Value = serde_json::from_str(&raw)?;
            let decoded = decoder.decode_event(&raw);
            txn.execute("UPDATE events SET decoded=?,signature=? WHERE contract_id=? AND block_hash=? AND log_index=?",params![decoded.to_string(),decoded_signature(&decoded),task.id,block_hash,integer(idx)?])?;
        }
        txn.execute(
            "UPDATE contracts SET revision=revision+1 WHERE id=?",
            [task.id],
        )?;
        txn.commit()?;
        Ok(
            json!({"abi_hash":abi_hash,"effective_from":effective_from,"revision":revision,"redecoded":true,"source":source}),
        )
    }
    pub(crate) fn abi_for(&self, id: i64, block: u64) -> Result<Abi> {
        let db = self.lock()?;
        let raw:String=db.query_row("SELECT abi FROM abi_bindings WHERE contract_id=? AND effective_from<=? ORDER BY effective_from DESC LIMIT 1",params![id,integer(block)?],|r|r.get(0))?;
        Abi::new(serde_json::from_str(&raw)?)
            .map_err(|_| IndexerError::new("storage", "Stored ABI is invalid"))
    }
    pub(crate) fn block_hash(&self, id: i64, number: u64) -> Result<Option<String>> {
        self.lock()?
            .query_row(
                "SELECT hash FROM blocks WHERE contract_id=? AND number=? AND canonical=1",
                params![id, integer(number)?],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
    }
    pub(crate) fn bind_genesis(&self, task: &Task, genesis: &str) -> Result<()> {
        let mut db = self.lock()?;
        let txn = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: Option<String> = txn
            .query_row(
                "SELECT genesis_hash FROM chain_identities WHERE chain_id=?",
                [integer(task.chain_id)?],
                |r| r.get(0),
            )
            .optional()?;
        if existing.as_deref().is_some_and(|old| old != genesis)
            || task
                .genesis_hash
                .as_deref()
                .is_some_and(|old| old != genesis)
        {
            return Err(IndexerError::new(
                "identity",
                "RPC genesis does not match persisted chain evidence",
            ));
        }
        txn.execute(
            "INSERT OR IGNORE INTO chain_identities VALUES(?,?)",
            params![integer(task.chain_id)?, genesis],
        )?;
        if txn.execute("UPDATE contracts SET genesis_hash=? WHERE id=? AND (genesis_hash IS NULL OR genesis_hash=?)",params![genesis,task.id,genesis])?!=1 {
            return Err(IndexerError::new("identity","Registration genesis changed concurrently"));
        }
        txn.commit()?;
        Ok(())
    }
    pub(crate) fn record_error(&self, task: &Task, category: &str) -> Result<()> {
        self.lock()?.execute("UPDATE contracts SET last_error=?,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=? AND revision=? AND next_block=?",params![category,task.id,integer(task.revision)?,integer(task.next_block)?])?;
        Ok(())
    }
    pub(crate) fn clear_error(&self, task: &Task) -> Result<()> {
        self.lock()?.execute("UPDATE contracts SET last_error=NULL,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=? AND revision=? AND next_block=?",params![task.id,integer(task.revision)?,integer(task.next_block)?])?;
        Ok(())
    }
    pub(crate) fn record_head(&self, task: &Task, head: u64) -> Result<()> {
        self.lock()?.execute(
            "UPDATE contracts SET head=? WHERE id=?",
            params![integer(head)?, task.id],
        )?;
        Ok(())
    }
    fn fence(txn: &Connection, task: &Task) -> Result<()> {
        let now = Self::task_in(txn, &task.project, task.chain_id, &task.address)?;
        if now.next_block != task.next_block || now.revision != task.revision || now.paused {
            return Err(IndexerError::new(
                "stale_task",
                "Registration changed while fetching a block",
            ));
        }
        Ok(())
    }
    fn rewind_in(txn: &Connection, task: &Task, next: u64) -> Result<()> {
        txn.execute(
            "UPDATE blocks SET canonical=0 WHERE contract_id=? AND number>=?",
            params![task.id, integer(next)?],
        )?;
        txn.execute(
            "UPDATE contracts SET next_block=?,revision=revision+1,last_error=NULL WHERE id=?",
            params![integer(next)?, task.id],
        )?;
        Ok(())
    }
    pub(crate) fn rewind(&self, task: &Task, next: u64) -> Result<()> {
        let mut db = self.lock()?;
        let txn = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::fence(&txn, task)?;
        Self::rewind_in(&txn, task, next)?;
        txn.commit()?;
        Ok(())
    }
    pub(crate) fn commit_block(
        &self,
        task: &Task,
        block: &Value,
        transactions: &[Value],
        events: &[Value],
        head: u64,
    ) -> Result<()> {
        let mut db = self.lock()?;
        let txn = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::fence(&txn, task)?;
        let number = task.next_block;
        let block_hash = hash(
            block["hash"]
                .as_str()
                .ok_or_else(|| invalid("Block hash missing"))?,
        )?;
        let parent_hash = hash(
            block["parentHash"]
                .as_str()
                .ok_or_else(|| invalid("Block parent hash missing"))?,
        )?;
        if number > task.start_block {
            let parent: Option<String> = txn
                .query_row(
                    "SELECT hash FROM blocks WHERE contract_id=? AND number=? AND canonical=1",
                    params![task.id, integer(number - 1)?],
                    |r| r.get(0),
                )
                .optional()?;
            if parent.as_deref() != Some(parent_hash.as_str()) {
                return Err(IndexerError::new(
                    "reorg",
                    "Block parent changed before commit",
                ));
            }
        }
        txn.execute("INSERT INTO blocks(contract_id,number,hash,parent_hash,raw) VALUES(?,?,?,?,?) ON CONFLICT(contract_id,hash) DO UPDATE SET canonical=1,raw=excluded.raw",params![task.id,integer(number)?,block_hash,parent_hash,block.to_string()])?;
        // A previously orphaned block may be canonical again after another reorg.
        txn.execute(
            "DELETE FROM events WHERE contract_id=? AND block_hash=?",
            params![task.id, block_hash],
        )?;
        txn.execute(
            "DELETE FROM transactions WHERE contract_id=? AND block_hash=?",
            params![task.id, block_hash],
        )?;
        for t in transactions {
            txn.execute(
                "INSERT INTO transactions VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    task.id,
                    block_hash,
                    t["hash"].as_str(),
                    integer(number)?,
                    t["tx_index"].as_u64().map(integer).transpose()?,
                    t["sender"].as_str(),
                    t["recipient"].as_str(),
                    t["status"].as_u64().map(integer).transpose()?,
                    decoded_signature(&t["decoded"]),
                    t["relations"].to_string(),
                    t["raw"].to_string(),
                    t["receipt"].to_string(),
                    t["decoded"].to_string(),
                    t.get("trace")
                        .filter(|v| !v.is_null())
                        .map(Value::to_string)
                ],
            )?;
        }
        for e in events {
            txn.execute(
                "INSERT INTO events VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    task.id,
                    block_hash,
                    e["tx_hash"].as_str(),
                    integer(number)?,
                    e["tx_index"].as_u64().map(integer).transpose()?,
                    e["log_index"].as_u64().map(integer).transpose()?,
                    e["emitter"].as_str(),
                    e["topic0"].as_str(),
                    decoded_signature(&e["decoded"]),
                    e["raw"].to_string(),
                    e["decoded"].to_string()
                ],
            )?;
        }
        txn.execute("UPDATE contracts SET next_block=?,head=?,last_error=NULL,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?",params![integer(number+1)?,integer(head)?,task.id])?;
        txn.commit()?;
        Ok(())
    }
    pub fn query(
        &self,
        project: &str,
        chain_id: u64,
        contract: &str,
        kind: &str,
        query: &Query,
    ) -> Result<Value> {
        if !["transactions", "events"].contains(&kind) {
            return Err(invalid("Unknown query resource"));
        }
        let mut db = self.lock()?;
        let txn = db.transaction()?;
        let task = Self::task_in(&txn, project, chain_id, &address(contract)?)?;
        let result = Self::query_in(&txn, &task, kind, query, true)?;
        txn.commit()?;
        Ok(result)
    }
    pub fn context(
        &self,
        project: &str,
        chain_id: u64,
        contract: &str,
        tx_hash: &str,
        query: &Query,
    ) -> Result<Value> {
        let tx_hash = hash(tx_hash)?;
        let mut db = self.lock()?;
        let txn = db.transaction()?;
        let task = Self::task_in(&txn, project, chain_id, &address(contract)?)?;
        Self::check_consistent(&task)?;
        let transaction=txn.query_row("SELECT t.raw,t.receipt,t.decoded,t.trace,t.relations,t.block_number,t.tx_index,t.hash,t.status,t.block_hash FROM transactions t JOIN blocks b ON b.contract_id=t.contract_id AND b.hash=t.block_hash AND b.canonical=1 WHERE t.contract_id=? AND t.hash=?",params![task.id,tx_hash],transaction_row).optional()?;
        let mut log_query = query.clone();
        log_query.tx_hash = Some(tx_hash);
        let logs = Self::query_in(&txn, &task, "events", &log_query, false)?;
        let data=transaction.map(|transaction|json!({"transaction":transaction,"logs":logs["data"],"next_cursor":logs["next_cursor"]}));
        let result = json!({"schema_version":"1","data":data,"coverage":Self::coverage_of(&task),"warnings":[],"next_cursor":logs["next_cursor"]});
        if serde_json::to_vec(&result)?.len() > 32 * 1024 * 1024 {
            return Err(IndexerError::new(
                "too_large",
                "Transaction context exceeds response budget; reduce the log page limit",
            ));
        }
        txn.commit()?;
        Ok(result)
    }
    fn check_consistent(task: &Task) -> Result<()> {
        if task
            .last_error
            .as_deref()
            .is_some_and(|v| ["deep_reorg", "reorg", "identity", "invalid_response"].contains(&v))
        {
            return Err(IndexerError::new(
                "index_inconsistent",
                "Index consistency must be restored by a successful scan or administrator backfill",
            ));
        }
        Ok(())
    }
    fn query_in(
        db: &Connection,
        task: &Task,
        kind: &str,
        q: &Query,
        only_emitter: bool,
    ) -> Result<Value> {
        Self::check_consistent(task)?;
        let limit = q.limit.unwrap_or(50);
        if !(1..=200).contains(&limit) {
            return Err(invalid("Page limit must be 1..200"));
        }
        if q.from_block
            .zip(q.to_block)
            .is_some_and(|(from, to)| from > to)
        {
            return Err(invalid("Invalid block range"));
        }
        if q.status.is_some_and(|s| s > 1) {
            return Err(invalid("Transaction status must be 0 or 1"));
        }
        if q.signature.as_ref().is_some_and(|s| s.len() > 4096) {
            return Err(invalid("Signature exceeds size limit"));
        }
        let mut filter = serde_json::to_value(q)?;
        filter.as_object_mut().unwrap().remove("cursor");
        filter.as_object_mut().unwrap().remove("limit");
        let fingerprint = digest(format!("{}:{kind}:{only_emitter}:{filter}", task.id).as_bytes());
        let mut upper = task.next_block;
        let mut last: Option<[u64; 3]> = None;
        if let Some(encoded) = &q.cursor {
            let c = Self::decode_cursor(db, encoded)?;
            if c.filter != fingerprint {
                return Err(IndexerError::new(
                    "invalid_cursor",
                    "Cursor does not match query filters",
                ));
            }
            if c.revision != task.revision || c.upper > task.next_block {
                return Err(IndexerError::new(
                    "snapshot_invalidated",
                    "Index changed; restart pagination",
                ));
            }
            upper = c.upper;
            last = Some(c.last);
        }
        let mut clauses = vec![
            "t.contract_id=?".to_string(),
            "t.block_number<?".into(),
            "b.canonical=1".into(),
        ];
        let mut values = vec![
            SqlValue::Integer(task.id),
            SqlValue::Integer(integer(upper)?),
        ];
        if let Some(from) = q.from_block {
            clauses.push("t.block_number>=?".into());
            values.push(SqlValue::Integer(integer(from)?));
        }
        if let Some(to) = q.to_block {
            clauses.push("t.block_number<?".into());
            values.push(SqlValue::Integer(integer(to)?));
        }
        if let Some(tx) = &q.tx_hash {
            clauses.push(
                if kind == "events" {
                    "t.tx_hash=?"
                } else {
                    "t.hash=?"
                }
                .into(),
            );
            values.push(SqlValue::Text(hash(tx)?));
        }
        if let Some(sender) = &q.sender {
            clauses.push(
                if kind == "events" {
                    "x.sender=?"
                } else {
                    "t.sender=?"
                }
                .into(),
            );
            values.push(SqlValue::Text(address(sender)?));
        }
        if let Some(status) = q.status {
            clauses.push(
                if kind == "events" {
                    "x.status=?"
                } else {
                    "t.status=?"
                }
                .into(),
            );
            values.push(SqlValue::Integer(integer(status)?));
        }
        if let Some(signature) = &q.signature {
            clauses.push("t.signature=?".into());
            values.push(SqlValue::Text(signature.clone()));
        }
        if let Some(topic) = &q.topic0 {
            if kind != "events" {
                return Err(invalid("topic0 applies only to event queries"));
            }
            clauses.push("t.topic0=?".into());
            values.push(SqlValue::Text(hash(topic)?));
        }
        if kind == "events" && only_emitter {
            clauses.push("t.emitter=?".into());
            values.push(SqlValue::Text(task.address.clone()));
        }
        let log_column = if kind == "events" {
            "t.log_index"
        } else {
            "CAST(0 AS INTEGER)"
        };
        if let Some(last) = last {
            clauses.push(format!("(t.block_number,t.tx_index,{log_column})>(?,?,?)"));
            for value in last {
                values.push(SqlValue::Integer(integer(value)?));
            }
        }
        let (table, fields, join) = if kind == "transactions" {
            ("transactions","t.raw,t.receipt,t.decoded,t.trace,t.relations,t.block_number,t.tx_index,t.hash,t.status,t.block_hash",String::new())
        } else {
            ("events","t.raw,t.decoded,t.block_number,t.tx_index,t.log_index,t.tx_hash,t.block_hash", " JOIN transactions x ON x.contract_id=t.contract_id AND x.block_hash=t.block_hash AND x.hash=t.tx_hash".into())
        };
        let sql=format!("SELECT {fields} FROM {table} t JOIN blocks b ON b.contract_id=t.contract_id AND b.hash=t.block_hash{join} WHERE {} ORDER BY t.block_number,t.tx_index,{log_column} LIMIT ?",clauses.join(" AND "));
        values.push(SqlValue::Integer((limit + 1) as i64));
        let mut stmt = db.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(values))?;
        let mut data = Vec::new();
        let mut bytes = 0usize;
        let mut byte_truncated = false;
        while let Some(row) = rows.next()? {
            let item = if kind == "transactions" {
                transaction_row(row)?
            } else {
                event_row(row)?
            };
            let size = serde_json::to_vec(&item)?.len();
            if bytes.saturating_add(size) > 16 * 1024 * 1024 {
                if data.is_empty() {
                    return Err(IndexerError::new(
                        "too_large",
                        "Evidence item exceeds query response budget; retrieve a smaller context",
                    ));
                }
                byte_truncated = true;
                break;
            }
            bytes += size;
            data.push(item);
        }
        let more = byte_truncated || data.len() > limit;
        data.truncate(limit);
        let next = if more {
            let item = data.last().expect("positive page limit");
            let cursor = Cursor {
                filter: fingerprint,
                revision: task.revision,
                upper,
                last: [
                    item["block_number"].as_u64().unwrap(),
                    item["transaction_index"].as_u64().unwrap(),
                    item.get("log_index").and_then(Value::as_u64).unwrap_or(0),
                ],
            };
            Some(Self::encode_cursor(db, &cursor)?)
        } else {
            None
        };
        Ok(
            json!({"schema_version":"1","data":data,"coverage":Self::coverage_of(task),"warnings":if byte_truncated { vec!["Page shortened to fit response budget"] } else { Vec::<&str>::new() },"next_cursor":next,"snapshot_to_block":upper}),
        )
    }
    fn cursor_key(db: &Connection) -> Result<Vec<u8>> {
        let key: String =
            db.query_row("SELECT value FROM meta WHERE key='cursor_key'", [], |r| {
                r.get(0)
            })?;
        hex::decode(key).map_err(|_| IndexerError::new("storage", "Cursor signing key is invalid"))
    }
    fn encode_cursor(db: &Connection, cursor: &Cursor) -> Result<String> {
        let payload = serde_json::to_vec(cursor)?;
        let mut mac = HmacSha256::new_from_slice(&Self::cursor_key(db)?)
            .expect("HMAC supports arbitrary key lengths");
        mac.update(&payload);
        Ok(format!(
            "{}.{}",
            hex::encode(&payload),
            hex::encode(mac.finalize().into_bytes())
        ))
    }
    fn decode_cursor(db: &Connection, encoded: &str) -> Result<Cursor> {
        let error = || IndexerError::new("invalid_cursor", "Invalid pagination cursor");
        if encoded.len() > 8192 {
            return Err(error());
        }
        let (payload, signature) = encoded.split_once('.').ok_or_else(error)?;
        let payload = hex::decode(payload).map_err(|_| error())?;
        let signature = hex::decode(signature).map_err(|_| error())?;
        let mut mac = HmacSha256::new_from_slice(&Self::cursor_key(db)?)
            .expect("HMAC supports arbitrary key lengths");
        mac.update(&payload);
        mac.verify_slice(&signature).map_err(|_| error())?;
        serde_json::from_slice(&payload).map_err(|_| error())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    filter: String,
    revision: u64,
    upper: u64,
    last: [u64; 3],
}

pub(crate) fn unknown(reason: &str) -> Value {
    json!({"status":"unknown","reason":reason,"decoder_version":"1"})
}
pub(crate) fn decoded_signature(decoded: &Value) -> Option<&str> {
    decoded.get("signature").and_then(Value::as_str)
}
fn sql_json(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Value> {
    let raw: String = row.get(index)?;
    serde_json::from_str(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}
fn transaction_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let mut receipt = sql_json(row, 1)?;
    let log_count = receipt
        .as_object_mut()
        .and_then(|object| object.remove("logs"))
        .map_or(0, |logs| logs.as_array().map_or(0, Vec::len));
    let trace: Option<String> = row.get(3)?;
    let trace = trace
        .map(|s| serde_json::from_str::<Value>(&s))
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
    Ok(
        json!({"raw":sql_json(row,0)?,"receipt":receipt,"receipt_log_count":log_count,"decoded":sql_json(row,2)?,"trace":trace,"relations":sql_json(row,4)?,"block_number":row_u64(row,5)?,"transaction_index":row_u64(row,6)?,"hash":row.get::<_,String>(7)?,"status":row_u64(row,8)?,"block_hash":row.get::<_,String>(9)?}),
    )
}
fn event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(
        json!({"raw":sql_json(row,0)?,"decoded":sql_json(row,1)?,"block_number":row_u64(row,2)?,"transaction_index":row_u64(row,3)?,"log_index":row_u64(row,4)?,"transaction_hash":row.get::<_,String>(5)?,"block_hash":row.get::<_,String>(6)?}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    const ADDRESS: &str = "0x1111111111111111111111111111111111111111";
    fn setup() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("index.sqlite")).unwrap();
        store.create_project("demo", "Demo").unwrap();
        store
            .register("demo", 31337, ADDRESS, json!([]), 0, "basic")
            .unwrap();
        (dir, store)
    }
    fn block(n: u64) -> Value {
        json!({"number":format!("0x{n:x}"),"hash":format!("0x{:064x}",n+100),"parentHash":format!("0x{:064x}",n+99),"transactions":[]})
    }

    #[test]
    fn stale_fetch_and_error_cannot_overwrite_newer_cursor_or_management_revision() {
        let (_dir, store) = setup();
        let first = store.task("demo", 31337, ADDRESS).unwrap();
        store.commit_block(&first, &block(0), &[], &[], 3).unwrap();
        store.record_error(&first, "deep_reorg").unwrap();
        assert!(store.coverage("demo", 31337, ADDRESS).unwrap()["last_error"].is_null());
        assert_eq!(
            store
                .commit_block(&first, &block(0), &[], &[], 3)
                .unwrap_err()
                .category,
            "stale_task"
        );
        let current = store.task("demo", 31337, ADDRESS).unwrap();
        store.set_paused("demo", 31337, ADDRESS, true).unwrap();
        store.record_error(&current, "deep_reorg").unwrap();
        assert!(store.coverage("demo", 31337, ADDRESS).unwrap()["last_error"].is_null());
        assert_eq!(
            store
                .commit_block(&current, &block(1), &[], &[], 3)
                .unwrap_err()
                .category,
            "stale_task"
        );
    }
    #[test]
    fn canonical_query_snapshot_survives_other_connection_rewind() {
        let (dir, store) = setup();
        let task = store.task("demo", 31337, ADDRESS).unwrap();
        store.commit_block(&task, &block(0), &[], &[], 1).unwrap();
        let other = Store::open(dir.path().join("index.sqlite")).unwrap();
        let mut db = store.lock().unwrap();
        let txn = db.transaction().unwrap();
        let snapshot = Store::task_in(&txn, "demo", 31337, ADDRESS).unwrap();
        other.backfill("demo", 31337, ADDRESS, 0).unwrap();
        let old =
            Store::query_in(&txn, &snapshot, "transactions", &Query::default(), true).unwrap();
        assert_eq!(old["coverage"]["next_block"], 1);
        let canonical: i64 = txn
            .query_row("SELECT COUNT(*) FROM blocks WHERE canonical=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(canonical, 1);
        txn.commit().unwrap();
        drop(db);
        assert_eq!(
            store.coverage("demo", 31337, ADDRESS).unwrap()["next_block"],
            0
        );
    }
    #[test]
    fn unknown_or_newer_schema_is_rejected_instead_of_reinitialized() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("foreign.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute("CREATE TABLE application_data(value TEXT)", [])
            .unwrap();
        drop(db);
        assert_eq!(Store::open(&path).unwrap_err().category, "storage");
        let path = dir.path().join("future.sqlite");
        drop(Store::open(&path).unwrap());
        let db = Connection::open(&path).unwrap();
        db.pragma_update(None, "user_version", 2).unwrap();
        drop(db);
        assert_eq!(Store::open(&path).unwrap_err().category, "storage");
    }
}
