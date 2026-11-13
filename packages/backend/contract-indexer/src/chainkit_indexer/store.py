"""SQLite persistence. Every public data query resolves a project-owned contract.

Each contract registration owns its evidence and cursor. This deliberately trades
storage duplication for independent project history, ABI and lifecycle semantics.
"""
from __future__ import annotations

import base64
from contextlib import contextmanager
import hashlib
import hmac
import json
from pathlib import Path
import re
import secrets
import sqlite3
import threading

from chainkit_chain import ABI, address, hash32
from .errors import IndexerError


def dump(value) -> str:
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True)


def signature(decoded: dict) -> str | None:
    return decoded['candidates'][0]['signature'] if decoded['status'] == 'decoded' else None


def indexed_arguments(decoded):
    if decoded['status'] != 'decoded':
        return []
    result = []
    def visit(path, typ, value):
        if isinstance(value, list):
            for i, element in enumerate(value):
                if isinstance(element, dict) and 'position' in element:
                    visit(path+'/'+str(element['position']), element['type'], element['value'])
                else:
                    visit(path+'/'+str(i), re.sub(r'\[[0-9]*\]$', '', typ), element)
        elif isinstance(value, dict) and value.get('recoverable') is False:
            result.append((path, 'indexed_hash:'+typ, value['hash']))
        else:
            result.append((path, typ, 'true' if value is True else 'false' if value is False else str(value)))
    for arg in decoded['candidates'][0]['arguments']:
        visit('/'+str(arg['position']), arg['type'], arg['value'])
    return result


class Store:
    def __init__(self, path: str):
        self._lock = threading.RLock()
        self.db = sqlite3.connect(path, timeout=10, check_same_thread=False, isolation_level=None)
        self.db.row_factory = sqlite3.Row
        self.db.execute('PRAGMA foreign_keys=ON')
        self.db.execute('PRAGMA journal_mode=WAL')
        version = self.db.execute('PRAGMA user_version').fetchone()[0]
        if version not in (0, 1):
            raise RuntimeError('Unsupported database schema version')
        with self._lock:
            self.db.executescript(Path(__file__).with_name('schema.sql').read_text())
            self.db.execute('PRAGMA user_version=1')
            self.db.execute("INSERT OR IGNORE INTO meta VALUES('cursor_key',?)", (secrets.token_hex(32),))
            self._cursor_key = bytes.fromhex(self.db.execute("SELECT value FROM meta WHERE key='cursor_key'").fetchone()[0])

    def close(self):
        with self._lock:
            self.db.close()

    @contextmanager
    def transaction(self, *, write=True):
        with self._lock:
            self.db.execute('BEGIN IMMEDIATE' if write else 'BEGIN')
            try:
                yield self.db
                self.db.execute('COMMIT')
            except BaseException:
                self.db.execute('ROLLBACK')
                raise

    def create_project(self, project_id: str, name: str) -> dict:
        if not re.fullmatch(r'[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}', project_id) or not 1 <= len(name) <= 200:
            raise IndexerError('invalid_project', 'Use a 1..64 character project ID and 1..200 character name')
        with self.transaction() as db:
            if db.execute('SELECT 1 FROM projects WHERE id=?', (project_id,)).fetchone():
                raise IndexerError('already_exists', 'Project already exists', 409)
            db.execute('INSERT INTO projects(id,name) VALUES(?,?)', (project_id, name))
            keys = {role: self._issue_key(db, project_id, role) for role in ('read', 'manage')}
        return {'id': project_id, 'name': name, 'tokens': keys}

    @staticmethod
    def _issue_key(db, project: str, role: str) -> str:
        token = secrets.token_urlsafe(32)
        db.execute('INSERT INTO api_keys(digest,project_id,role) VALUES(?,?,?)',
                   (hashlib.sha256(token.encode()).hexdigest(), project, role))
        return token

    def rotate_key(self, project: str, role: str) -> str:
        if role not in ('read', 'manage'):
            raise IndexerError('invalid_role', 'Role must be read or manage')
        with self.transaction() as db:
            self._project(db, project)
            db.execute('UPDATE api_keys SET revoked=1 WHERE project_id=? AND role=?', (project, role))
            return self._issue_key(db, project, role)

    def authorize(self, project: str, token: str, *, manage=False):
        with self._lock:
            row = self.db.execute('SELECT role FROM api_keys WHERE digest=? AND project_id=? AND revoked=0',
                                  (hashlib.sha256(token.encode()).hexdigest(), project)).fetchone()
        if not row:
            raise IndexerError('unauthorized', 'Invalid project credentials', 401)
        if manage and row['role'] != 'manage':
            raise IndexerError('forbidden', 'Project manage token required', 403)

    @staticmethod
    def _project(db, project):
        row = db.execute('SELECT id,name,paused,created_at FROM projects WHERE id=?', (project,)).fetchone()
        if not row:
            raise IndexerError('not_found', 'Project not found', 404)
        return dict(row)

    def project(self, project):
        with self._lock:
            return self._project(self.db, project)

    def projects(self):
        with self._lock:
            return [dict(r) for r in self.db.execute('SELECT id,name,paused,created_at FROM projects ORDER BY id')]

    def pause_project(self, project: str, paused: bool):
        if type(paused) is not bool:
            raise IndexerError('invalid_input', 'paused must be a boolean')
        with self.transaction() as db:
            self._project(db, project)
            db.execute('UPDATE projects SET paused=? WHERE id=?', (paused, project))
            db.execute('UPDATE contracts SET revision=revision+1 WHERE project_id=?', (project,))
        return self.project(project)

    def register(self, project: str, chain_id: int, contract: str, abi, start_block: int, mode: str = 'full') -> dict:
        contract, parsed = address(contract), ABI(abi)
        if mode not in ('basic', 'full'):
            raise IndexerError('invalid_input', 'mode must be basic or full')
        if type(chain_id) is not int or not 1 <= chain_id <= 2**63-1 or type(start_block) is not int or not 0 <= start_block <= 2**63-2:
            raise IndexerError('invalid_input', 'chain_id and explicit nonnegative start_block required')
        with self.transaction() as db:
            self._project(db, project)
            if db.execute('SELECT 1 FROM contracts WHERE project_id=? AND chain_id=? AND address=?',
                          (project, chain_id, contract)).fetchone():
                raise IndexerError('already_exists', 'Contract already registered in this project', 409)
            cid = db.execute('INSERT INTO contracts(project_id,chain_id,address,start_block,next_block,mode) VALUES(?,?,?,?,?,?)',
                             (project, chain_id, contract, start_block, start_block, mode)).lastrowid
            db.execute('INSERT INTO abis VALUES(?,?,?,?)', (cid, start_block, parsed.hash, parsed.json))
            db.execute('INSERT INTO abi_history(contract_id,effective_from,revision,abi_hash,abi_json) VALUES(?,?,?,?,?)', (cid, start_block, 0, parsed.hash, parsed.json))
        return self.contract(project, chain_id, contract)

    @staticmethod
    def _contract(db, project: str, chain_id: int, contract: str):
        if type(chain_id) is not int or not 1 <= chain_id <= 2**63-1:
            raise IndexerError('invalid_input', 'Invalid chain ID')
        row = db.execute('SELECT c.* FROM contracts c WHERE project_id=? AND chain_id=? AND address=?',
                         (project, chain_id, address(contract))).fetchone()
        if not row:
            raise IndexerError('not_found', 'Contract is not registered in this project', 404)
        return dict(row)

    def contract(self, project: str, chain_id: int, contract: str):
        with self._lock:
            return self._contract(self.db, project, chain_id, contract)

    def contracts(self, project: str):
        with self._lock:
            self._project(self.db, project)
            return [dict(r) for r in self.db.execute('SELECT * FROM contracts WHERE project_id=? ORDER BY chain_id,address', (project,))]

    def tasks(self):
        with self._lock:
            return [dict(r) for r in self.db.execute('SELECT c.* FROM contracts c JOIN projects p ON p.id=c.project_id WHERE c.paused=0 AND p.paused=0 ORDER BY c.id')]

    def pause_contract(self, project, chain_id, contract, paused):
        if type(paused) is not bool:
            raise IndexerError('invalid_input', 'paused must be a boolean')
        with self.transaction() as db:
            c = self._contract(db, project, chain_id, contract)
            db.execute('UPDATE contracts SET paused=?,revision=revision+1 WHERE id=?', (paused, c['id']))
        return self.contract(project, chain_id, contract)

    def abi_for(self, contract_id: int, number: int) -> ABI:
        with self._lock:
            row = self.db.execute('SELECT abi_json FROM abis WHERE contract_id=? AND effective_from<=? ORDER BY effective_from DESC LIMIT 1',
                                  (contract_id, number)).fetchone()
        if not row:
            raise IndexerError('abi_unavailable', 'No ABI binding for this block', 409)
        return ABI(json.loads(row[0]))

    @staticmethod
    def _check_task(db, expected):
        row = db.execute('SELECT c.*,p.paused AS project_paused FROM contracts c JOIN projects p ON p.id=c.project_id WHERE c.id=?', (expected['id'],)).fetchone()
        if (not row or row['revision'] != expected['revision'] or row['next_block'] != expected['next_block']
                or row['paused'] or row['project_paused']):
            raise IndexerError('stale_task', 'Task was paused, reset or advanced by another worker', 409)
        return row

    def bind_genesis(self, expected, genesis_hash):
        genesis_hash = hash32(genesis_hash)
        with self.transaction() as db:
            row = self._check_task(db, expected)
            if row['genesis_hash'] and row['genesis_hash'] != genesis_hash:
                raise IndexerError('chain_identity', 'Configured chain genesis differs from indexed history', 409)
            db.execute('UPDATE contracts SET genesis_hash=? WHERE id=?', (genesis_hash, expected['id']))

    def block_hash(self, cid: int, number: int) -> str | None:
        with self._lock:
            row = self.db.execute('SELECT hash FROM blocks WHERE contract_id=? AND number=? AND canonical=1', (cid, number)).fetchone()
            return row[0] if row else None

    def commit_block(self, expected, block, transactions, logs, head):
        with self.transaction() as db:
            self._check_task(db, expected)
            if expected['next_block'] != block['number_int']:
                raise IndexerError('invalid_batch', 'Block does not match task cursor', 409)
            db.execute('INSERT INTO blocks(contract_id,number,hash,parent_hash,raw) VALUES(?,?,?,?,?) ON CONFLICT(contract_id,hash) DO UPDATE SET canonical=1',
                       (expected['id'], block['number_int'], block['hash'], block['parentHash'], dump(block['raw'])))
            for t in transactions:
                db.execute('INSERT OR REPLACE INTO transactions VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)',
                           (expected['id'], block['hash'], t['hash'], block['number_int'], t['tx_index'],
                            t['sender'], t['recipient'], t['status'], signature(t['decoded']), dump(t['relations']),
                            dump(t['raw']), dump(t['receipt']), dump(t['decoded']), dump(t['trace']) if t['trace'] is not None else None))
            for t in transactions:
                db.execute('DELETE FROM arguments WHERE contract_id=? AND block_hash=? AND tx_hash=? AND log_index=-1', (expected['id'], block['hash'], t['hash']))
                db.executemany('INSERT INTO arguments VALUES(?,?,?,?,?,?,?)', [(expected['id'], block['hash'], t['hash'], -1, *arg) for arg in indexed_arguments(t['decoded'])])
            for log in logs:
                db.execute('INSERT OR REPLACE INTO events VALUES(?,?,?,?,?,?,?,?,?,?,?)',
                           (expected['id'], block['hash'], log['tx_hash'], block['number_int'], log['tx_index'],
                            log['log_index'], log['emitter'], log['topic0'], signature(log['decoded']),
                            dump(log['raw']), dump(log['decoded'])))
            for log in logs:
                db.execute('DELETE FROM arguments WHERE contract_id=? AND block_hash=? AND tx_hash=? AND log_index=?', (expected['id'], block['hash'], log['tx_hash'], log['log_index']))
                db.executemany('INSERT INTO arguments VALUES(?,?,?,?,?,?,?)', [(expected['id'], block['hash'], log['tx_hash'], log['log_index'], *arg) for arg in indexed_arguments(log['decoded'])])
            db.execute("UPDATE contracts SET next_block=?,head=?,last_error=NULL,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?",
                       (block['number_int']+1, head, expected['id']))

    def rewind(self, expected, next_block: int):
        with self.transaction() as db:
            self._check_task(db, expected)
            db.execute('UPDATE blocks SET canonical=0 WHERE contract_id=? AND canonical=1 AND number>=?', (expected['id'], next_block))
            db.execute('UPDATE contracts SET next_block=?,revision=revision+1,last_error=NULL WHERE id=?', (next_block, expected['id']))

    def record_error(self, expected, code):
        with self.transaction() as db:
            db.execute('UPDATE contracts SET last_error=? WHERE id=? AND revision=? AND next_block=?',
                       (code, expected['id'], expected['revision'], expected['next_block']))

    def record_head(self, expected, head):
        with self.transaction() as db:
            self._check_task(db, expected)
            db.execute("UPDATE contracts SET head=?,last_error=NULL,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?", (head, expected['id']))

    def coverage(self, project, chain_id, contract):
        with self.transaction(write=False):
            c = self._contract(self.db, project, chain_id, contract)
            return self._coverage(c)

    def _coverage(self, c):
        anchor = self.block_hash(c['id'], c['next_block']-1)
        return {'start_block': c['start_block'], 'next_block': c['next_block'],
                'indexed_range': [c['start_block'], c['next_block']], 'range_semantics': '[from,to)',
                'snapshot_block_hash': anchor, 'observed_head': c['head'], 'revision': c['revision'],
                'abi_revision': c['abi_revision'], 'last_error': c['last_error'],
                'paused': bool(c['paused']), 'project_paused': bool(self._project(self.db, c['project_id'])['paused']), 'mode': c['mode'], 'updated_at': c['updated_at'],
                'capabilities': {'emitted_events': 'indexed_range', 'direct_transactions': 'indexed_range',
                                 'contract_creation': 'indexed_range', 'internal_calls': 'indexed_call_traces' if c['mode'] == 'full' else 'not_supported',
                                 'external_token_transfers': 'not_supported'},
                'integrity': 'recovery_required' if c['last_error'] in ('deep_reorg', 'reorg', 'chain_identity', 'identity') else 'last_verified_snapshot',
                'warnings': ['Coverage is relative to configured RPC responses; omitted or incorrect provider data cannot always be detected.',
                             'ABI bindings are operator supplied; proxy implementation history is not automatically verified.',
                             'Confirmation depth is not a finalized-chain guarantee.']}

    def _encode_cursor(self, payload):
        raw = dump(payload).encode()
        mac = hmac.new(self._cursor_key, raw, hashlib.sha256).digest()
        return base64.urlsafe_b64encode(mac+raw).decode().rstrip('=')

    def _decode_cursor(self, value):
        try:
            if len(value) > 4096:
                raise ValueError()
            raw = base64.urlsafe_b64decode(value + '=' * (-len(value) % 4))
            if not hmac.compare_digest(raw[:32], hmac.new(self._cursor_key, raw[32:], hashlib.sha256).digest()):
                raise ValueError()
            payload = json.loads(raw[32:])
            if not isinstance(payload, dict):
                raise ValueError()
            return payload
        except (ValueError, TypeError):
            raise IndexerError('invalid_cursor', 'Invalid pagination cursor') from None

    def query(self, project, chain_id, contract, kind, *, limit=50, cursor=None,
              from_block=None, to_block=None, tx_hash=None, sender=None, status=None,
              event_signature=None, topic0=None, all_logs=False, argument_path=None, argument_type=None, argument_value=None):
        if kind not in ('transactions', 'events') or type(limit) is not int or not 1 <= limit <= 200:
            raise IndexerError('invalid_query', 'Invalid resource or limit (1..200)')
        if all_logs and (kind != 'events' or tx_hash is None):
            raise IndexerError('invalid_query', 'Context logs require a transaction hash')
        if kind == 'events' and (sender is not None or status is not None):
            raise IndexerError('invalid_query', 'sender/status filters apply only to transactions')
        if kind == 'transactions' and topic0 is not None:
            raise IndexerError('invalid_query', 'topic0 applies only to events')
        for value in (from_block, to_block):
            if value is not None and (type(value) is not int or not 0 <= value <= 2**63-2):
                raise IndexerError('invalid_query', 'Block bounds must be nonnegative integers')
        if from_block is not None and to_block is not None and from_block > to_block:
            raise IndexerError('invalid_query', 'from_block exceeds to_block')
        if status is not None and (type(status) is not int or status not in (0, 1)):
            raise IndexerError('invalid_query', 'status must be 0 or 1')
        if event_signature is not None and (not isinstance(event_signature, str) or len(event_signature) > 2048):
            raise IndexerError('invalid_query', 'Invalid signature')
        if any(v is not None for v in (argument_path, argument_type, argument_value)):
            if not all(isinstance(v, str) for v in (argument_path, argument_type, argument_value)) or not re.fullmatch(r'(?:/[0-9]+)+', argument_path) or len(argument_path) > 200 or len(argument_type) > 200 or len(argument_value) > 4096:
                raise IndexerError('invalid_query', 'Argument filters require positional path, ABI type and string value')
        tx_hash = hash32(tx_hash) if tx_hash else None
        sender = address(sender) if sender else None
        topic0 = hash32(topic0) if topic0 else None
        fingerprint = hashlib.sha256(dump([project, chain_id, address(contract), kind, from_block,
                                            to_block, tx_hash, sender, status, event_signature, topic0, all_logs, argument_path, argument_type, argument_value]).encode()).hexdigest()
        with self._lock:
            # One snapshot even if a different process scans/reorganizes this file.
            own_snapshot = not self.db.in_transaction
            if own_snapshot:
                self.db.execute('BEGIN')
            try:
                c = self._contract(self.db, project, chain_id, contract)
                if c['last_error'] in ('deep_reorg', 'reorg', 'chain_identity', 'identity'):
                    raise IndexerError('index_inconsistent', 'Chain consistency requires recovery; inspect coverage', 409)
                upper, last = c['next_block'], [-1, -1, -1]
                if cursor:
                    token = self._decode_cursor(cursor)
                    if token.get('filter') != fingerprint:
                        raise IndexerError('invalid_cursor', 'Cursor belongs to a different query')
                    upper, last = token['upper'], token['last']
                    if token['revision'] != c['revision'] or token['abi_revision'] != c['abi_revision'] or upper > c['next_block']:
                        raise IndexerError('snapshot_invalidated', 'Snapshot changed; restart pagination', 409)
                table = 'transactions' if kind == 'transactions' else 'events'
                hash_col = 't.hash' if kind == 'transactions' else 't.tx_hash'
                log_col = 'CAST(-1 AS INTEGER)' if kind == 'transactions' else 't.log_index'
                sql = f'SELECT t.* FROM {table} t JOIN blocks b ON b.contract_id=t.contract_id AND b.hash=t.block_hash WHERE t.contract_id=? AND b.canonical=1 AND t.block_number<? AND (t.block_number,t.tx_index,{log_col})>(?,?,?)'
                params = [c['id'], upper, *last]
                if kind == 'events' and not all_logs:
                    sql += ' AND t.emitter=?'
                    params.append(c['address'])
                for col, val, op in [('t.block_number', from_block, '>='), ('t.block_number', to_block, '<'),
                                     (hash_col, tx_hash, '='), ('t.sender', sender, '='),
                                     ('t.status', status, '='), ('t.signature', event_signature, '='), ('t.topic0', topic0, '=')]:
                    if val is not None:
                        sql += f' AND {col}{op}?'
                        params.append(val)
                if argument_path is not None:
                    sql += f' AND EXISTS(SELECT 1 FROM arguments a WHERE a.contract_id=t.contract_id AND a.block_hash=t.block_hash AND a.tx_hash={hash_col} AND a.log_index={log_col} AND a.path=? AND a.abi_type=? AND a.value=?)'
                    params.extend([argument_path, argument_type, argument_value])
                sql += f' ORDER BY t.block_number,t.tx_index,{log_col} LIMIT ?'
                rows = self.db.execute(sql, [*params, limit+1]).fetchall()
                items = [self._item(r, kind) for r in rows[:limit]]
                next_cursor = None
                if len(rows) > limit:
                    r = rows[limit-1]
                    next_cursor = self._encode_cursor({'filter': fingerprint, 'upper': upper,
                        'revision': c['revision'], 'abi_revision': c['abi_revision'],
                        'last': [r['block_number'], r['tx_index'], r['log_index'] if kind == 'events' else -1]})
                coverage = self._coverage(c)
                coverage['query_snapshot_to'] = upper
                coverage['query_snapshot_hash'] = self.block_hash(c['id'], upper-1)
                coverage['requested_range'] = [from_block, to_block]
                coverage['request_fully_covered'] = (from_block is not None and to_block is not None
                                                       and c['start_block'] <= from_block <= to_block <= upper)
                result = {'schema_version': '1', 'items': items, 'next_cursor': next_cursor, 'coverage': coverage}
                if own_snapshot:
                    self.db.execute('COMMIT')
                return result
            except BaseException:
                if own_snapshot:
                    self.db.execute('ROLLBACK')
                raise

    @staticmethod
    def _item(row, kind):
        if kind == 'transactions':
            return {'tx_hash': row['hash'], 'block_hash': row['block_hash'], 'block_number': row['block_number'],
                    'transaction_index': row['tx_index'], 'from': row['sender'], 'to': row['recipient'],
                    'status': row['status'], 'relations': json.loads(row['relations']), 'decoded': json.loads(row['decoded'])}
        return {'tx_hash': row['tx_hash'], 'block_hash': row['block_hash'], 'block_number': row['block_number'],
                'transaction_index': row['tx_index'], 'log_index': row['log_index'], 'emitter': row['emitter'],
                'raw': json.loads(row['raw']), 'decoded': json.loads(row['decoded'])}

    def context(self, project, chain_id, contract, tx_hash, *, limit=50, cursor=None):
        tx_hash = hash32(tx_hash)
        # Serialize context and logs into one database read snapshot.
        with self.transaction(write=False):
            page = self.query(project, chain_id, contract, 'events', tx_hash=tx_hash,
                              all_logs=True, limit=limit, cursor=cursor)
            c = self._contract(self.db, project, chain_id, contract)
            row = self.db.execute('SELECT t.*,b.fetched_at FROM transactions t JOIN blocks b ON b.contract_id=t.contract_id AND b.hash=t.block_hash WHERE t.contract_id=? AND t.hash=? AND b.canonical=1', (c['id'], tx_hash)).fetchone()
            if not row:
                return {'schema_version': '1', 'data': None, 'logs': page,
                        'warnings': ['No indexed relation found. This does not prove the transaction does not exist.']}
            receipt = json.loads(row['receipt'])
            receipt.pop('logs', None)
            return {'schema_version': '1', 'data': {**self._item(row, 'transactions'),
                    'raw': json.loads(row['raw']), 'receipt': receipt, 'fetched_at': row['fetched_at'],
                    'source': 'configured_rpc', 'trace': {'status': 'available', 'data': json.loads(row['trace'])} if row['trace'] else {'status': 'not_supported'}}, 'logs': page,
                    'warnings': ['Receipt failure status alone does not identify a revert reason.',
                                 'Other-contract events are raw unless a matching ABI is registered for decoding.']}

    def add_abi(self, project, chain_id, contract, abi, effective_from, *, replace=False):
        parsed = ABI(abi)
        if type(replace) is not bool:
            raise IndexerError('invalid_input', 'replace must be a boolean')
        if type(effective_from) is not int or not 0 <= effective_from <= 2**63-2:
            raise IndexerError('invalid_input', 'effective_from must be a block number')
        with self.transaction() as db:
            c = self._contract(db, project, chain_id, contract)
            if effective_from < c['start_block']:
                raise IndexerError('invalid_input', 'ABI binding precedes scan start')
            existing = db.execute('SELECT abi_hash FROM abis WHERE contract_id=? AND effective_from=?', (c['id'], effective_from)).fetchone()
            if existing:
                if existing[0] == parsed.hash:
                    return {'abi_hash': parsed.hash, 'effective_from': effective_from}
                if not replace:
                    raise IndexerError('already_exists', 'ABI binding already exists; explicit replace required', 409)
            # Invalidate affected evidence and rescan; preserves historical bindings.
            db.execute('INSERT INTO abis VALUES(?,?,?,?) ON CONFLICT(contract_id,effective_from) DO UPDATE SET abi_hash=excluded.abi_hash,abi_json=excluded.abi_json', (c['id'], effective_from, parsed.hash, parsed.json))
            db.execute('INSERT INTO abi_history(contract_id,effective_from,revision,abi_hash,abi_json) VALUES(?,?,?,?,?)', (c['id'], effective_from, c['abi_revision']+1, parsed.hash, parsed.json))
            db.execute('UPDATE blocks SET canonical=0 WHERE contract_id=? AND number>=?', (c['id'], effective_from))
            db.execute('UPDATE contracts SET next_block=min(next_block,?),revision=revision+1,abi_revision=abi_revision+1 WHERE id=?', (effective_from, c['id']))
        return {'abi_hash': parsed.hash, 'effective_from': effective_from}

    def backfill(self, project, chain_id, contract, from_block):
        with self.transaction() as db:
            c = self._contract(db, project, chain_id, contract)
            if type(from_block) is not int or not c['start_block'] <= from_block <= c['next_block']:
                raise IndexerError('invalid_input', 'Backfill must be inside the configured history range')
            db.execute('UPDATE blocks SET canonical=0 WHERE contract_id=? AND number>=?', (c['id'], from_block))
            db.execute('UPDATE contracts SET next_block=?,revision=revision+1,last_error=NULL WHERE id=?', (from_block, c['id']))
        return self.coverage(project, chain_id, contract)

    def abi_history(self, project, chain_id, contract):
        with self.transaction(write=False) as db:
            c = self._contract(db, project, chain_id, contract)
            return [dict(row) for row in db.execute('SELECT effective_from,revision,abi_hash,created_at FROM abi_history WHERE contract_id=? ORDER BY revision', (c['id'],))]
