CREATE TABLE chain_identities (
 chain_id INTEGER PRIMARY KEY, genesis_hash TEXT NOT NULL
);
CREATE TABLE projects (
 id TEXT PRIMARY KEY, name TEXT NOT NULL, manage_digest TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE TABLE contracts (
 id INTEGER PRIMARY KEY, project TEXT NOT NULL REFERENCES projects(id),
 chain_id INTEGER NOT NULL, address TEXT NOT NULL,
 start_block INTEGER NOT NULL, next_block INTEGER NOT NULL,
 mode TEXT NOT NULL CHECK(mode IN ('basic','full')),
 paused INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 0,
 genesis_hash TEXT, head INTEGER, last_error TEXT, updated_at TEXT,
 UNIQUE(project,chain_id,address)
);
CREATE TABLE abi_bindings (
 contract_id INTEGER NOT NULL REFERENCES contracts(id),
 effective_from INTEGER NOT NULL, abi_hash TEXT NOT NULL, abi TEXT NOT NULL,
 source TEXT NOT NULL CHECK(source IN ('manual','sourcify','etherscan')),
 PRIMARY KEY(contract_id,effective_from)
);
CREATE TABLE abi_revisions (
 contract_id INTEGER NOT NULL REFERENCES contracts(id), revision INTEGER NOT NULL,
 effective_from INTEGER NOT NULL, abi_hash TEXT NOT NULL, abi TEXT NOT NULL,
 source TEXT NOT NULL CHECK(source IN ('manual','sourcify','etherscan')),
 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
 PRIMARY KEY(contract_id,revision)
);
CREATE TABLE blocks (
 contract_id INTEGER NOT NULL REFERENCES contracts(id), number INTEGER NOT NULL,
 hash TEXT NOT NULL, parent_hash TEXT NOT NULL, canonical INTEGER NOT NULL DEFAULT 1,
 raw TEXT NOT NULL,
 PRIMARY KEY(contract_id,hash)
);
CREATE UNIQUE INDEX canonical_height ON blocks(contract_id,number) WHERE canonical=1;
CREATE TABLE transactions (
 contract_id INTEGER NOT NULL, block_hash TEXT NOT NULL, hash TEXT NOT NULL,
 block_number INTEGER NOT NULL, tx_index INTEGER NOT NULL, sender TEXT NOT NULL,
 recipient TEXT, status INTEGER NOT NULL, signature TEXT,
 relations TEXT NOT NULL, raw TEXT NOT NULL, receipt TEXT NOT NULL,
 decoded TEXT NOT NULL, trace TEXT,
 PRIMARY KEY(contract_id,block_hash,hash),
 FOREIGN KEY(contract_id,block_hash) REFERENCES blocks(contract_id,hash)
);
CREATE INDEX transaction_order ON transactions(contract_id,block_number,tx_index,hash);
CREATE INDEX transaction_sender ON transactions(contract_id,sender,block_number,tx_index);
CREATE INDEX transaction_signature ON transactions(contract_id,signature,block_number,tx_index);
CREATE TABLE events (
 contract_id INTEGER NOT NULL, block_hash TEXT NOT NULL, tx_hash TEXT NOT NULL,
 block_number INTEGER NOT NULL, tx_index INTEGER NOT NULL, log_index INTEGER NOT NULL,
 emitter TEXT NOT NULL, topic0 TEXT, signature TEXT, raw TEXT NOT NULL, decoded TEXT NOT NULL,
 PRIMARY KEY(contract_id,block_hash,log_index),
 FOREIGN KEY(contract_id,block_hash,tx_hash) REFERENCES transactions(contract_id,block_hash,hash)
);
CREATE INDEX event_order ON events(contract_id,block_number,tx_index,log_index);
CREATE INDEX event_signature ON events(contract_id,emitter,signature,block_number,tx_index,log_index);
CREATE INDEX event_topic ON events(contract_id,emitter,topic0,block_number,tx_index,log_index);
CREATE INDEX event_transaction ON events(contract_id,tx_hash,log_index);
CREATE TABLE meta (key TEXT PRIMARY KEY,value TEXT NOT NULL);
