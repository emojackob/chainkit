CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS projects(
 id TEXT PRIMARY KEY, name TEXT NOT NULL, paused INTEGER NOT NULL DEFAULT 0,
 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE TABLE IF NOT EXISTS api_keys(
 digest TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
 role TEXT NOT NULL CHECK(role IN ('read','manage')), revoked INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS contracts(
 id INTEGER PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
 chain_id INTEGER NOT NULL CHECK(chain_id>0), address TEXT NOT NULL,
 mode TEXT NOT NULL CHECK(mode IN ('basic','full')),
 start_block INTEGER NOT NULL CHECK(start_block>=0), next_block INTEGER NOT NULL,
 revision INTEGER NOT NULL DEFAULT 0, abi_revision INTEGER NOT NULL DEFAULT 0,
 paused INTEGER NOT NULL DEFAULT 0, head INTEGER, genesis_hash TEXT,
 last_error TEXT, updated_at TEXT, UNIQUE(project_id,chain_id,address)
);
CREATE TABLE IF NOT EXISTS abis(
 contract_id INTEGER NOT NULL REFERENCES contracts(id), effective_from INTEGER NOT NULL,
 abi_hash TEXT NOT NULL, abi_json TEXT NOT NULL,
 PRIMARY KEY(contract_id,effective_from)
);
CREATE TABLE IF NOT EXISTS blocks(
 contract_id INTEGER NOT NULL REFERENCES contracts(id), number INTEGER NOT NULL,
 hash TEXT NOT NULL, parent_hash TEXT NOT NULL, canonical INTEGER NOT NULL DEFAULT 1,
 raw TEXT NOT NULL, fetched_at TEXT NOT NULL DEFAULT(strftime('%Y-%m-%dT%H:%M:%fZ','now')),
 PRIMARY KEY(contract_id,hash)
);
CREATE UNIQUE INDEX IF NOT EXISTS blocks_canonical_height ON blocks(contract_id,number) WHERE canonical=1;
CREATE TABLE IF NOT EXISTS transactions(
 contract_id INTEGER NOT NULL, block_hash TEXT NOT NULL, hash TEXT NOT NULL,
 block_number INTEGER NOT NULL, tx_index INTEGER NOT NULL, sender TEXT NOT NULL,
 recipient TEXT, status INTEGER NOT NULL, signature TEXT, relations TEXT NOT NULL,
 raw TEXT NOT NULL, receipt TEXT NOT NULL, decoded TEXT NOT NULL, trace TEXT,
 PRIMARY KEY(contract_id,block_hash,hash),
 FOREIGN KEY(contract_id,block_hash) REFERENCES blocks(contract_id,hash)
);
CREATE INDEX IF NOT EXISTS tx_order ON transactions(contract_id,block_number,tx_index);
CREATE INDEX IF NOT EXISTS tx_hash ON transactions(contract_id,hash);
CREATE INDEX IF NOT EXISTS tx_sender ON transactions(contract_id,sender,block_number,tx_index);
CREATE INDEX IF NOT EXISTS tx_signature ON transactions(contract_id,signature,block_number,tx_index);
CREATE TABLE IF NOT EXISTS events(
 contract_id INTEGER NOT NULL, block_hash TEXT NOT NULL, tx_hash TEXT NOT NULL,
 block_number INTEGER NOT NULL, tx_index INTEGER NOT NULL, log_index INTEGER NOT NULL,
 emitter TEXT NOT NULL, topic0 TEXT, signature TEXT,
 raw TEXT NOT NULL, decoded TEXT NOT NULL,
 PRIMARY KEY(contract_id,block_hash,log_index),
 FOREIGN KEY(contract_id,block_hash,tx_hash) REFERENCES transactions(contract_id,block_hash,hash)
);
CREATE INDEX IF NOT EXISTS events_order ON events(contract_id,block_number,tx_index,log_index);
CREATE INDEX IF NOT EXISTS events_topic ON events(contract_id,emitter,topic0,block_number,tx_index,log_index);
CREATE INDEX IF NOT EXISTS events_signature ON events(contract_id,emitter,signature,block_number,tx_index,log_index);
CREATE INDEX IF NOT EXISTS events_tx ON events(contract_id,tx_hash,log_index);
CREATE TABLE IF NOT EXISTS arguments(
 contract_id INTEGER NOT NULL, block_hash TEXT NOT NULL, tx_hash TEXT NOT NULL,
 log_index INTEGER NOT NULL, path TEXT NOT NULL, abi_type TEXT NOT NULL, value TEXT NOT NULL,
 PRIMARY KEY(contract_id,block_hash,tx_hash,log_index,path),
 FOREIGN KEY(contract_id,block_hash,tx_hash) REFERENCES transactions(contract_id,block_hash,hash)
);
CREATE INDEX IF NOT EXISTS argument_value ON arguments(contract_id,path,abi_type,value,block_hash,tx_hash,log_index);
CREATE TABLE IF NOT EXISTS abi_history(
 contract_id INTEGER NOT NULL REFERENCES contracts(id), effective_from INTEGER NOT NULL,
 revision INTEGER NOT NULL, abi_hash TEXT NOT NULL, abi_json TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT(strftime('%Y-%m-%dT%H:%M:%fZ','now')),
 PRIMARY KEY(contract_id,revision)
);
