"""One-block atomic scanning with cursor fencing and bounded reorg recovery."""
from __future__ import annotations

import threading
import logging
import sqlite3
import time
from chainkit_chain import ABI, Rpc, RpcError, address, hash32
from chainkit_chain.abi import hex_bytes
from chainkit_chain.rpc import quantity
from .errors import IndexerError
from .store import Store


_LOG = logging.getLogger(__name__)


def unknown(reason):
    return {'status': 'unknown', 'reason': reason, 'candidates': [], 'decoder_version': '1'}


def related_trace(root: dict, target: str) -> bool:
    """CallTracer frames include reverted calls; preserve the original tree."""
    pending = [(root, None)]
    budget = 100_000
    matched = False
    while pending:
        budget -= 1
        if budget < 0:
            raise RpcError('too_large', 'Trace frame budget exceeded')
        frame, parent_context = pending.pop()
        if not isinstance(frame, dict) or not isinstance(frame.get('type'), str):
            raise RpcError('invalid_response', 'Invalid callTracer frame')
        sender = address(frame['from']) if frame.get('from') else None
        to = address(frame['to']) if frame.get('to') else None
        context = parent_context if frame['type'] in ('DELEGATECALL', 'CALLCODE') else to
        if target in (sender, to, context):
            matched = True
        children = frame.get('calls', [])
        if not isinstance(children, list):
            raise RpcError('invalid_response', 'Invalid trace child frames')
        pending.extend((child, context) for child in children)
    return matched


def decode_error(data, abi, builtin):
    result = abi.decode('error', data)
    return builtin.decode('error', data) if result['status'] == 'unknown' else result


def annotate_trace(root, target, abi):
    error_abi = ABI([
        {'type': 'error', 'name': 'Error', 'inputs': [{'name': 'message', 'type': 'string'}]},
        {'type': 'error', 'name': 'Panic', 'inputs': [{'name': 'code', 'type': 'uint256'}]},
    ])
    frames = []
    pending = [(root, None, [], False)]
    while pending:
        frame, parent_context, path, parent_reverted = pending.pop()
        to = address(frame['to']) if frame.get('to') else None
        sender = address(frame['from']) if frame.get('from') else None
        context = parent_context if frame['type'] in ('DELEGATECALL', 'CALLCODE') else to
        reverted = parent_reverted or bool(frame.get('error'))
        if target in (sender, to, context):
            uses_abi = context == target or to == target
            frames.append({'trace_path': path, 'type': frame['type'], 'context_address': context,
                'code_address': to, 'reverted': reverted, 'error': frame.get('error'),
                'decoded_input': abi.decode('function', frame.get('input', '0x')) if uses_abi else unknown('another_contract_ABI'),
                'decoded_error': decode_error(frame.get('output', '0x'), abi, error_abi) if uses_abi and frame.get('error') else None})
        pending.extend((child, context, path+[i], reverted) for i, child in reversed(list(enumerate(frame.get('calls', [])))))
    return {'raw': root, 'related_frames': frames, 'abi_binding': 'operator_supplied_block_level'}


class Scanner:
    def __init__(self, store: Store, chains: dict[int, Rpc], *, confirmations: int = 12,
                 reorg_window: int = 256):
        if type(confirmations) is not int or not 0 <= confirmations <= 100_000 or not 1 <= reorg_window <= 100_000:
            raise ValueError('Invalid confirmations or reorg window')
        self.store, self.chains = store, dict(chains)
        self.confirmations, self.reorg_window = confirmations, reorg_window
        self._scan_lock = threading.Lock()

    def scan(self, project: str, chain_id: int, contract: str, *, max_blocks=20, cancel=None):
        if type(max_blocks) is not int or not 1 <= max_blocks <= 1000:
            raise IndexerError('invalid_input', 'max_blocks must be 1..1000')
        # This instance serializes RPC use; cursor CAS also fences other processes.
        with self._scan_lock:
            task = self.store.contract(project, chain_id, contract)
            if task['paused'] or self.store.project(project)['paused']:
                return {'scanned_blocks': 0, 'paused': True}
            rpc = self.chains.get(chain_id)
            if rpc is None:
                self.store.record_error(task, 'chain_unconfigured')
                raise IndexerError('chain_unconfigured', 'Chain is not configured by the service operator', 409)
            count = 0
            tx_count = event_count = receipt_log_count = 0
            started = last_progress = time.monotonic()
            label = f'project={project} chain={chain_id} contract={task["address"]}'
            _LOG.info('Scan starting %s mode=%s next_block=%s; checking RPC head and chain identity',
                      label, task['mode'], task['next_block'])
            try:
                head = rpc.head()
                genesis = rpc.block(0, False)['hash']
                self.store.bind_genesis(task, genesis)
                self._recover(task, rpc)
                task = self.store.contract(project, chain_id, contract)
                self.store.record_head(task, head)
                safe = head - self.confirmations
                _LOG.info('Scan target %s head=%s safe_block=%s pending_blocks=%s batch_limit=%s',
                          label, head, safe, max(0, safe-task['next_block']+1), max_blocks)
                while task['next_block'] <= safe and count < max_blocks:
                    if cancel is not None and cancel.is_set():
                        break
                    stats = self._block(task, rpc, head, cancel)
                    count += 1
                    tx_count += stats['transactions']
                    event_count += stats['events']
                    receipt_log_count += stats['receipt_logs']
                    task = self.store.contract(project, chain_id, contract)
                    now = time.monotonic()
                    if count == 1 or now-last_progress >= 10:
                        _LOG.info('Scan progress %s committed_block=%s next_block=%s pending_blocks=%s '
                                  'batch_blocks=%s batch_transactions=%s batch_events=%s batch_receipt_logs=%s '
                                  'blocks_per_second=%.2f', label, task['next_block']-1, task['next_block'],
                                  max(0, safe-task['next_block']+1), count, tx_count, event_count,
                                  receipt_log_count, count/max(now-started, 0.001))
                        last_progress = now
                _LOG.info('Scan round complete %s state=%s next_block=%s pending_blocks=%s '
                          'batch_blocks=%s batch_transactions=%s batch_events=%s batch_receipt_logs=%s elapsed_seconds=%.1f',
                          label, 'cancelled' if cancel is not None and cancel.is_set() else
                          ('caught_up' if task['next_block'] > safe else 'backfilling'),
                          task['next_block'], max(0, safe-task['next_block']+1), count,
                          tx_count, event_count, receipt_log_count, time.monotonic()-started)
                return {'scanned_blocks': count, 'coverage': self.store.coverage(project, chain_id, contract),
                        'safe_block': safe, 'confirmations': self.confirmations}
            except (RpcError, IndexerError) as exc:
                code = exc.category if isinstance(exc, RpcError) else exc.code
                self.store.record_error(task, code)
                # Provider exception messages may contain credentials or URLs.
                _LOG.warning('Scan failed %s next_block=%s mode=%s error=%s; '
                             'failed block was not committed.',
                             label, task['next_block'], task['mode'], code)
                if code == 'unsupported' and task['mode'] == 'full':
                    _LOG.warning('full mode requires historical debug_traceBlockByNumber with callTracer; '
                                 'no automatic downgrade. %s', label)
                raise
            except (KeyError, ValueError, TypeError):
                self.store.record_error(task, 'invalid_response')
                _LOG.warning('Scan failed %s next_block=%s error=invalid_response; failed block was not committed',
                             label, task['next_block'])
                raise RpcError('invalid_response', 'Malformed RPC data; cursor was not advanced for this block') from None

    def _recover(self, task, rpc):
        end = task['next_block']-1
        if end < task['start_block']:
            return
        stored = self.store.block_hash(task['id'], end)
        if stored == rpc.block(end, False)['hash'].lower():
            return
        lower = max(task['start_block'], end-self.reorg_window+1)
        for number in range(end-1, lower-1, -1):
            if self.store.block_hash(task['id'], number) == rpc.block(number, False)['hash'].lower():
                self.store.rewind(task, number+1)
                return
        if lower == task['start_block']:
            self.store.rewind(task, lower)
        else:
            raise IndexerError('deep_reorg', 'No common ancestor in recovery window; operator backfill required', 409)

    def _traces(self, rpc, number, transactions):
        result = rpc.call('debug_traceBlockByNumber', [hex(number), {'tracer': 'callTracer', 'timeout': '10s'}])
        if not isinstance(result, list) or len(result) != len(transactions):
            raise RpcError('invalid_response', 'Full block traces missing or incomplete')
        traces = {}
        for tx, item in zip(transactions, result):
            if (not isinstance(item, dict) or item.get('error') or not isinstance(item.get('result'), dict)
                    or item.get('txHash', tx['hash']).lower() != tx['hash'].lower()):
                raise RpcError('invalid_response', 'Trace execution failed or transaction identity mismatch')
            root = item['result']
            if address(root['from']) != address(tx['from']) or (tx.get('to') and address(root['to']) != address(tx['to'])) or root.get('input', '0x').lower() != tx.get('input', '0x').lower():
                raise RpcError('invalid_response', 'Trace root disagrees with block transaction')
            traces[tx['hash'].lower()] = root
        return traces

    def _block(self, task, rpc, head, cancel=None):
        number = task['next_block']
        _LOG.debug('Fetching block project=%s chain=%s contract=%s block=%s mode=%s',
                   task['project_id'], task['chain_id'], task['address'], number, task['mode'])
        raw_block = rpc.block(number)
        bh = hash32(raw_block['hash'])
        parent = hash32(raw_block['parentHash'])
        if number > task['start_block'] and self.store.block_hash(task['id'], number-1) != parent:
            raise RpcError('reorg', 'Block parent no longer matches indexed chain')
        block = {'number_int': number, 'hash': bh, 'parentHash': parent, 'raw': {k: v for k, v in raw_block.items() if k != 'transactions'}}
        transactions = raw_block['transactions']
        positions = {}
        for i, tx in enumerate(transactions):
            if not isinstance(tx, dict) or quantity(tx['transactionIndex']) != i or hash32(tx['blockHash']) != bh or quantity(tx['blockNumber']) != number:
                raise RpcError('invalid_response', 'Full transaction identity mismatch')
            txh = hash32(tx['hash'])
            if txh in positions:
                raise RpcError('invalid_response', 'Duplicate transaction in block')
            positions[txh] = i
        found_logs = rpc.logs(task['address'], number, number)
        event_txs = set()
        expected_logs = {}
        for log in found_logs:
            txh = hash32(log['transactionHash'])
            if (txh not in positions or hash32(log['blockHash']) != bh or quantity(log['blockNumber']) != number
                    or quantity(log['transactionIndex']) != positions[txh] or log.get('removed', False)):
                raise RpcError('reorg', 'Log does not belong to scanned block')
            key = (txh, quantity(log['logIndex']))
            if key in expected_logs and expected_logs[key] != log:
                raise RpcError('invalid_response', 'Conflicting log identities')
            expected_logs[key] = log
            event_txs.add(txh)
        traces = self._traces(rpc, number, transactions) if task['mode'] == 'full' else {}
        abi = self.store.abi_for(task['id'], number)
        tx_rows, log_rows, seen_logs = [], [], set()
        for i, tx in enumerate(transactions):
            if cancel is not None and cancel.is_set():
                raise IndexerError('cancelled', 'Scan cancelled before commit', 409)
            txh = hash32(tx['hash'])
            to = address(tx['to']) if tx.get('to') else None
            sender = address(tx['from'])
            trace = traces.get(txh)
            relations = []
            if to == task['address']:
                relations.append('direct_call')
            if txh in event_txs:
                relations.append('emitted_event')
            if trace is not None and related_trace(trace, task['address']):
                relations.append('call_trace')
            # Creation receipts are checked even if no log was emitted.
            if not relations and to is not None:
                continue
            receipt = rpc.receipt(txh)
            if (hash32(receipt['blockHash']) != bh or quantity(receipt['blockNumber']) != number
                    or quantity(receipt['transactionIndex']) != i):
                raise RpcError('reorg', 'Receipt moved during scan')
            if receipt.get('contractAddress') and address(receipt['contractAddress']) == task['address']:
                relations.append('creation')
            if not relations:
                continue
            status = quantity(receipt['status'])
            if status not in (0, 1) or status == 0 and receipt['logs']:
                raise RpcError('invalid_response', 'Invalid receipt status/log combination')
            decoded = abi.decode('function', tx.get('input', '0x')) if to == task['address'] else unknown('top_level_target_uses_another_ABI')
            hex_bytes(tx['input'])
            # Preserve arbitrary precision RPC values in their original hex form.
            tx_rows.append({'hash': txh, 'tx_index': i, 'sender': sender, 'recipient': to, 'status': status,
                            'relations': sorted(set(relations)), 'raw': tx, 'receipt': receipt,
                            'decoded': decoded, 'trace': annotate_trace(trace, task['address'], abi) if trace is not None else None})
            for log in receipt['logs']:
                idx = quantity(log['logIndex'])
                if (hash32(log['transactionHash']) != txh or hash32(log['blockHash']) != bh
                        or quantity(log['blockNumber']) != number or quantity(log['transactionIndex']) != i
                        or idx in seen_logs or log.get('removed', False)):
                    raise RpcError('invalid_response', 'Invalid or duplicate receipt log')
                seen_logs.add(idx)
                emitter = address(log['address'])
                topics = [hash32(t) for t in log['topics']]
                if len(topics) > 4:
                    raise RpcError('invalid_response', 'Too many log topics')
                hex_bytes(log['data'])
                if emitter == task['address']:
                    # Receipt evidence can recover omissions in getLogs for discovered transactions.
                    if 'emitted_event' not in tx_rows[-1]['relations']:
                        tx_rows[-1]['relations'].append('emitted_event')
                    key = (txh, idx)
                    if key in expected_logs:
                        expected = expected_logs.pop(key)
                        if (expected['data'].lower() != log['data'].lower() or
                                [t.lower() for t in expected['topics']] != topics):
                            raise RpcError('invalid_response', 'getLogs and receipt disagree')
                log_rows.append({'tx_hash': txh, 'tx_index': i, 'log_index': idx, 'emitter': emitter,
                                 'topic0': topics[0] if topics else None, 'raw': log,
                                 'decoded': abi.decode('event', log['data'], topics) if emitter == task['address'] else unknown('emitter_ABI_not_bound')})
        if expected_logs:
            raise RpcError('invalid_response', 'getLogs returned events absent from receipts')
        if hash32(rpc.block(number, False)['hash']) != bh:
            raise RpcError('reorg', 'Block changed before commit')
        self.store.commit_block(task, block, tx_rows, log_rows, head)
        return {'transactions': len(tx_rows), 'events': sum(l['emitter'] == task['address'] for l in log_rows),
                'receipt_logs': len(log_rows)}

    def run(self, stop: threading.Event, *, poll_interval=5, max_blocks=20):
        if type(max_blocks) is not int or not 1 <= max_blocks <= 1000:
            raise ValueError('max_blocks must be 1..1000')
        if poll_interval <= 0:
            raise ValueError('poll_interval must be positive')
        _LOG.info('Scanner worker started poll_interval=%ss max_blocks_per_round=%s', poll_interval, max_blocks)
        while not stop.is_set():
            behind = False
            try:
                tasks = self.store.tasks()
            except sqlite3.Error:
                logging.getLogger(__name__).exception('Failed to load scanner tasks')
                stop.wait(poll_interval)
                continue
            for task in tasks:
                if stop.is_set():
                    break
                try:
                    result = self.scan(task['project_id'], task['chain_id'], task['address'], max_blocks=max_blocks, cancel=stop)
                    behind |= result.get('scanned_blocks', 0) == max_blocks
                except (RpcError, IndexerError):
                    # Error is persisted per contract; one project cannot stop the rest.
                    continue
                except sqlite3.Error:
                    logging.getLogger(__name__).exception('Storage error while scanning contract id=%s', task['id'])
                    continue
            stop.wait(0.05 if behind else poll_interval)
