import json
import sqlite3
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

from chainkit_chain import ABI as ParsedABI, RpcError
from chainkit_chain.abi import Decoder, keccak256, parse_type
from chainkit_indexer import Store, Scanner, IndexerError
from fixtures import Chain, TARGET, OTHER, TX, ABI, h, word


class AbiTests(unittest.TestCase):
    def test_ethereum_keccak_vectors(self):
        self.assertEqual(keccak256(b'').hex(), 'c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470')
        self.assertEqual(keccak256(b'abc').hex(), '4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45')
        self.assertEqual(keccak256(b'Transfer(address,address,uint256)').hex(), 'ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef')
        self.assertEqual(keccak256(b'transfer(address,uint256)')[:4].hex(), 'a9059cbb')

    def test_dynamic_tuple_array_and_integer_precision(self):
        types = [parse_type({'type': 'tuple', 'components': [{'name': 'a', 'type': 'uint256'}, {'name': 'b', 'type': 'string'}]}), parse_type({'type': 'uint256[]'})]
        # Head: tuple@64, array@192. Tuple: uint, string@64, length, data.
        data = bytes.fromhex(word(64)+word(192)+word(2**255+1)+word(64)+word(3)+'616263'+'00'*29+word(2)+word(1)+word(2))
        result = Decoder(data).sequence(types, 0)
        self.assertEqual(result[0][0]['value'], str(2**255+1))
        self.assertEqual(result[0][1]['value'], 'abc')
        self.assertEqual(result[1], ['1', '2'])

    def test_indexed_dynamic_is_hash_and_anonymous(self):
        abi = ParsedABI([{'type': 'event', 'name': 'Note', 'anonymous': True, 'inputs': [{'name': 's', 'type': 'string', 'indexed': True}]}])
        decoded = abi.decode('event', '0x', [h(123)])
        self.assertEqual(decoded['status'], 'decoded')
        self.assertEqual(decoded['candidates'][0]['arguments'][0]['value'], {'hash': h(123), 'recoverable': False})

    def test_malformed_offsets_width_and_budget(self):
        with self.assertRaises(ValueError):
            Decoder(bytes.fromhex(word(0))).sequence([parse_type({'type': 'string'})], 0)
        with self.assertRaises(ValueError):
            Decoder(bytes.fromhex(word(256))).value(parse_type({'type': 'uint8'}), 0)
        with self.assertRaises(ValueError):
            Decoder(bytes.fromhex(word(5000))).value(parse_type({'type': 'uint256[]'}), 0)
        self.assertEqual(Decoder(b'\xff'*32).value(parse_type({'type': 'int8'}), 0), '-1')
        with self.assertRaises(ValueError):
            parse_type({'type': 'fixed128x18'})

    def test_ambiguous_event_candidates_preserved(self):
        a = ParsedABI([ABI[1], ABI[1]])
        result = a.decode('event', '0x'+word(1), ['0x'+keccak256(b'Changed(uint256)').hex()])
        self.assertEqual(result['status'], 'ambiguous')


class IndexerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.path = str(Path(self.temp.name)/'index.sqlite')
        self.store = Store(self.path)
        self.keys_a = self.store.create_project('alpha', 'Alpha')['tokens']
        self.store.create_project('beta', 'Beta')
        self.chain = Chain()
        self.scanner = Scanner(self.store, {1: self.chain}, confirmations=0)
        self.store.register('alpha', 1, TARGET, ABI, 1, 'full')

    def tearDown(self):
        self.store.close()
        self.temp.cleanup()

    def scan(self, **kwargs):
        return self.scanner.scan('alpha', 1, TARGET, **kwargs)

    def query(self, kind='transactions', **kwargs):
        return self.store.query('alpha', 1, TARGET, kind, **kwargs)

    def test_progress_logs_report_committed_counts_and_caught_up_state(self):
        with patch('chainkit_indexer.scanner.time.monotonic', return_value=100):
            with self.assertLogs('chainkit_indexer.scanner', level='INFO') as logs:
                self.scan()
        output = '\n'.join(logs.output)
        self.assertIn('next_block=1; checking RPC', output)
        self.assertIn('head=4 safe_block=4 pending_blocks=4', output)
        self.assertIn('committed_block=1 next_block=2', output)
        self.assertEqual(output.count('Scan progress'), 1)
        self.assertIn('state=caught_up next_block=5 pending_blocks=0', output)
        self.assertIn('batch_blocks=4 batch_transactions=5 batch_events=3 batch_receipt_logs=3', output)
        with self.assertLogs('chainkit_indexer.scanner', level='INFO') as idle:
            self.assertEqual(self.scan()['scanned_blocks'], 0)
        self.assertIn('batch_blocks=0 batch_transactions=0 batch_events=0', '\n'.join(idle.output))

    def test_failed_scan_logs_category_without_provider_secret_or_progress(self):
        secret = 'https://private-provider.example/secret-key'
        with patch.object(self.chain, 'call', side_effect=RpcError('unsupported', secret)):
            with self.assertLogs('chainkit_indexer.scanner', level='INFO') as logs:
                with self.assertRaises(RpcError):
                    self.scan()
        output = '\n'.join(logs.output)
        self.assertIn('next_block=1 mode=full error=unsupported', output)
        self.assertIn('debug_traceBlockByNumber', output)
        self.assertNotIn(secret, output)
        self.assertNotIn('Scan progress', output)
        self.assertNotIn('Scan round complete', output)
        self.assertEqual(self.store.coverage('alpha', 1, TARGET)['next_block'], 1)

    def test_full_index_direct_failure_no_event_indirect_and_context(self):
        self.assertEqual(self.scan()['scanned_blocks'], 4)
        self.assertEqual(len(self.query()['items']), 5)
        self.assertEqual(len(self.query('events')['items']), 3)
        self.assertEqual(len(self.query(status=0)['items']), 1)
        context = self.store.context('alpha', 1, TARGET, TX, limit=1)
        self.assertEqual(len(context['logs']['items']), 1)
        self.assertIsNotNone(context['logs']['next_cursor'])
        self.assertEqual(context['data']['decoded']['candidates'][0]['arguments'][0]['value'], str(2**255+1))
        self.assertNotIn('logs', context['data']['receipt'])
        self.assertEqual(context['data']['trace']['status'], 'available')

    def test_project_chain_abi_isolation_and_keys(self):
        self.store.register('beta', 1, TARGET, [], 2, 'basic')
        self.scan()
        self.assertEqual(self.store.query('beta', 1, TARGET, 'events')['items'], [])
        self.scanner.scan('beta', 1, TARGET)
        events = self.store.query('beta', 1, TARGET, 'events')['items']
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]['decoded']['status'], 'unknown')
        self.assertEqual(self.query('events')['items'][0]['decoded']['status'], 'decoded')
        with self.assertRaises(IndexerError) as exc:
            self.store.authorize('beta', self.keys_a['manage'])
        self.assertEqual(exc.exception.status, 401)
        with self.assertRaises(IndexerError) as exc:
            self.store.authorize('alpha', self.keys_a['read'], manage=True)
        self.assertEqual(exc.exception.status, 403)
        self.store.register('alpha', 2, TARGET, [], 0, 'basic')
        self.assertEqual(self.store.coverage('alpha', 2, TARGET)['next_block'], 0)

    def test_idempotent_restart_and_persistent_cursor(self):
        self.scan(max_blocks=1)
        self.store.close()
        self.store = Store(self.path)
        self.scanner = Scanner(self.store, {1: self.chain}, confirmations=0)
        self.scan()
        self.assertEqual(self.scan()['scanned_blocks'], 0)
        self.store.backfill('alpha', 1, TARGET, 1)
        self.scan()
        self.assertEqual(len(self.query()['items']), 5)
        self.assertEqual(len(self.query('events')['items']), 3)

    def test_reorg_removes_old_branch_and_invalidates_page(self):
        self.scan()
        page = self.query(limit=1)
        self.chain.fork(1)
        self.scan()
        self.assertEqual(len(self.query()['items']), 2)
        with self.assertRaises(IndexerError) as exc:
            self.query(limit=1, cursor=page['next_cursor'])
        self.assertEqual(exc.exception.code, 'snapshot_invalidated')
        self.assertEqual(self.store.db.execute('SELECT count(*) FROM blocks WHERE canonical=0').fetchone()[0], 3)

    def test_same_transaction_reincluded_preserves_old_receipt(self):
        self.scan()
        self.chain.fork(0, remove=False)
        self.scan()
        self.assertEqual(len(self.query()['items']), 5)
        self.assertEqual(self.store.context('alpha', 1, TARGET, TX)['data']['block_hash'], self.chain.blocks[1]['hash'])
        self.assertEqual(self.store.db.execute('SELECT count(*) FROM transactions WHERE hash=?', (TX,)).fetchone()[0], 2)

    def test_deep_reorg_stops_and_does_not_claim_recovery(self):
        self.scan()
        self.chain.fork(0)
        self.scanner.reorg_window = 2
        with self.assertRaises(IndexerError) as exc:
            self.scan()
        self.assertEqual(exc.exception.code, 'deep_reorg')
        self.assertEqual(self.store.coverage('alpha', 1, TARGET)['last_error'], 'deep_reorg')
        with self.assertRaises(IndexerError) as exc:
            self.query()
        self.assertEqual(exc.exception.code, 'index_inconsistent')

    def test_failure_rolls_back_block_and_keeps_cursor(self):
        self.chain.receipts[TX]['blockHash'] = h(999)
        with self.assertRaises(RpcError):
            self.scan()
        self.assertEqual(self.store.coverage('alpha', 1, TARGET)['next_block'], 1)
        with self.assertRaises(IndexerError) as exc:
            self.query()
        self.assertEqual(exc.exception.code, 'index_inconsistent')

    def test_database_failure_is_atomic(self):
        self.store.db.executescript("CREATE TRIGGER fail_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'injected'); END;")
        with self.assertRaises(sqlite3.IntegrityError):
            self.scan()
        self.assertEqual(self.store.coverage('alpha', 1, TARGET)['next_block'], 1)
        self.assertEqual(self.store.db.execute('SELECT count(*) FROM blocks').fetchone()[0], 0)
        self.assertEqual(self.query()['items'], [])

    def test_full_mode_does_not_silently_degrade(self):
        self.chain.support_trace = False
        with self.assertRaises(RpcError):
            self.scan()
        self.assertEqual(self.store.coverage('alpha', 1, TARGET)['next_block'], 1)
        self.assertEqual(self.store.coverage('alpha', 1, TARGET)['last_error'], 'unsupported')

    def test_basic_mode_declares_missing_internal_calls(self):
        self.store.register('beta', 1, TARGET, ABI, 1, 'basic')
        self.chain.support_trace = False
        self.scanner.scan('beta', 1, TARGET)
        page = self.store.query('beta', 1, TARGET, 'transactions')
        self.assertEqual(len(page['items']), 4)
        self.assertEqual(page['coverage']['capabilities']['internal_calls'], 'not_supported')

    def test_cursor_snapshot_stable_during_append_and_filter_bound(self):
        self.scan(max_blocks=1)
        page = self.query(limit=1)
        self.scan()
        next_page = self.query(limit=1, cursor=page['next_cursor'])
        self.assertIsNone(next_page['next_cursor'])
        self.assertEqual(next_page['coverage']['query_snapshot_to'], 2)
        with self.assertRaises(IndexerError):
            self.query(limit=1, status=0, cursor=page['next_cursor'])
        with self.assertRaises(IndexerError):
            self.query(cursor=page['next_cursor']+'X')

    def test_pause_revokes_stale_worker_and_rotating_keys(self):
        stale = self.store.contract('alpha', 1, TARGET)
        self.store.pause_project('alpha', True)
        self.assertTrue(self.scan()['paused'])
        with self.assertRaises(IndexerError):
            self.store.bind_genesis(stale, h(100))
        new = self.store.rotate_key('alpha', 'read')
        self.store.authorize('alpha', new)
        with self.assertRaises(IndexerError):
            self.store.authorize('alpha', self.keys_a['read'])

    def test_independent_connections_fence_duplicate_worker(self):
        stale = self.store.contract('alpha', 1, TARGET)
        other = Store(self.path)
        try:
            self.scan(max_blocks=1)
            with self.assertRaises(IndexerError) as exc:
                other.bind_genesis(stale, h(100))
            self.assertEqual(exc.exception.code, 'stale_task')
        finally:
            other.close()

    def test_abi_binding_rescan_and_unknown_event_preservation(self):
        self.scan()
        self.store.add_abi('alpha', 1, TARGET, [], 2)
        self.assertEqual(self.store.coverage('alpha', 1, TARGET)['next_block'], 2)
        self.scan()
        events = self.query('events')['items']
        self.assertEqual([e['decoded']['status'] for e in events], ['decoded', 'decoded', 'unknown'])
        self.assertTrue(events[-1]['raw']['data'])
        with self.assertRaises(IndexerError):
            self.store.add_abi('alpha', 1, TARGET, ABI, 2)

    def test_coverage_and_missing_transaction_do_not_assert_absence(self):
        context = self.store.context('alpha', 1, TARGET, TX)
        self.assertIsNone(context['data'])
        self.assertIn('does not prove', context['warnings'][0])
        self.scan()
        self.assertTrue(self.query(from_block=1, to_block=5)['coverage']['request_fully_covered'])
        self.assertFalse(self.query(from_block=0, to_block=5)['coverage']['request_fully_covered'])

    def test_receipt_recovers_silently_omitted_direct_events(self):
        self.chain.omit_logs = True
        self.scan()
        # Full-block traces discover the indirect event despite omitted getLogs.
        self.assertEqual(len(self.query('events')['items']), 3)

    def test_typed_argument_index(self):
        self.scan()
        page = self.query('events', argument_path='/0', argument_type='uint256', argument_value='9')
        self.assertEqual(len(page['items']), 1)
        self.assertEqual(page['items'][0]['log_index'], 1)
        self.assertEqual(len(self.query(argument_path='/0', argument_type='uint256', argument_value=str(2**255+1))['items']), 3)
        with self.assertRaises(IndexerError):
            self.query('events', argument_path='/0')

    def test_trace_revert_decoding_and_delegatecall_context(self):
        txh = h(301)
        root = self.chain.trace_data[txh]
        root['error'] = 'execution reverted'
        root['output'] = '0x'+keccak256(b'Panic(uint256)')[:4].hex()+word(17)
        root['calls'] = [{'type': 'DELEGATECALL', 'from': TARGET, 'to': OTHER,
                          'input': self.chain.blocks[1]['transactions'][1]['input']}]
        self.scan()
        ctx = self.store.context('alpha', 1, TARGET, txh)
        frames = ctx['data']['trace']['data']['related_frames']
        self.assertEqual(frames[0]['decoded_error']['candidates'][0]['signature'], 'Panic(uint256)')
        self.assertEqual(frames[1]['context_address'], TARGET)
        self.assertEqual(frames[1]['code_address'], OTHER)
        self.assertTrue(frames[1]['reverted'])

    def test_contract_creation_without_events(self):
        self.chain.add(4, h(400), to=None, values=[])
        self.chain.receipts[h(400)]['contractAddress'] = TARGET
        self.chain.trace_data[h(400)] = {'type': 'CREATE', 'from': self.chain.blocks[4]['transactions'][0]['from'],
                                         'to': TARGET, 'input': self.chain.blocks[4]['transactions'][0]['input']}
        self.scan()
        ctx = self.store.context('alpha', 1, TARGET, h(400))
        self.assertIn('creation', ctx['data']['relations'])

    def test_stopping_worker_does_not_advance_cursor(self):
        import threading
        stop = threading.Event()
        stop.set()
        self.assertEqual(self.scanner.scan('alpha', 1, TARGET, cancel=stop)['scanned_blocks'], 0)
        self.assertEqual(self.store.coverage('alpha', 1, TARGET)['next_block'], 1)

    def test_abi_correction_preserves_revision_history(self):
        self.scan()
        page = self.query('events', limit=1)
        self.store.add_abi('alpha', 1, TARGET, [], 1, replace=True)
        self.assertEqual(len(self.store.abi_history('alpha', 1, TARGET)), 2)
        self.scan()
        self.assertTrue(all(e['decoded']['status'] == 'unknown' for e in self.query('events')['items']))
        with self.assertRaises(IndexerError):
            self.query('events', limit=1, cursor=page['next_cursor'])
        self.store.add_abi('alpha', 1, TARGET, ABI, 1, replace=True)
        self.scan()
        self.assertEqual(self.query('events')['items'][0]['decoded']['status'], 'decoded')
        self.assertEqual(len(self.store.abi_history('alpha', 1, TARGET)), 3)
