from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import tempfile
import threading
import unittest
import urllib.request
from urllib.parse import urlsplit
from unittest.mock import patch
from io import BytesIO
from types import SimpleNamespace
from urllib.error import URLError
from urllib.error import HTTPError

from chainkit_chain import Rpc, RpcError
from chainkit_indexer import Store, Scanner
from chainkit_indexer.api import handler_class, openapi
from fixtures import Chain, ABI, TARGET, OTHER, TX, h


class WireSocket:
    def __init__(self, raw):
        self.input = BytesIO(raw)
        self.output = BytesIO()

    def makefile(self, *args):
        return self.input

    def settimeout(self, timeout):
        pass

    def sendall(self, raw):
        self.output.write(raw)


class WireResponse(BytesIO):
    def __init__(self, status, body):
        super().__init__(body)
        self.status = status


@contextmanager
def running(handler):
    """Actual stdlib HTTP request parsing over byte streams, no OS sockets.

    This tests HTTP and JSON-RPC adapters without claiming TCP/TLS verification.
    """
    def transport(request, timeout=None):
        if not request.full_url.startswith('http://fixture.invalid'):
            raise URLError('simulated unavailable endpoint containing secret-credential')
        url = urlsplit(request.full_url)
        body = request.data or b''
        headers = dict(request.header_items())
        headers['Content-Length'] = str(len(body))
        headers['Host'] = 'fixture.invalid'
        raw = (request.get_method()+' '+(url.path or '/')+('?' + url.query if url.query else '')+' HTTP/1.0\r\n').encode()
        raw += ''.join(k+': '+v+'\r\n' for k,v in headers.items()).encode()+b'\r\n'+body
        sock = WireSocket(raw)
        handler(sock, ('127.0.0.1', 1234), SimpleNamespace(server_name='fixture', server_port=80))
        head, result = sock.output.getvalue().split(b'\r\n\r\n', 1)
        status = int(head.split(b' ')[1])
        if status >= 400:
            raise HTTPError(request.full_url, status, 'HTTP error', {}, BytesIO(result))
        return WireResponse(status, result)
    with patch('urllib.request.urlopen', transport):
        yield 'http://fixture.invalid'


def rpc_server(chain):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_POST(self):
            req = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            method, params = req['method'], req['params']
            out = {'jsonrpc': '2.0', 'id': req['id']}
            if getattr(chain, 'wrong_id', False):
                out['id'] = -1
            try:
                if method == 'eth_chainId':
                    value = hex(getattr(chain, 'chain_id', 1))
                elif method == 'eth_blockNumber':
                    value = hex(chain.head())
                elif method == 'eth_getBlockByNumber':
                    value = chain.block(int(params[0], 16), params[1])
                elif method == 'eth_getTransactionReceipt':
                    value = chain.receipt(params[0])
                elif method == 'eth_getLogs':
                    f = params[0]
                    if chain.overflow:
                        raise RpcError('too_large', 'limit exceeded')
                    value = chain.logs(f['address'], int(f['fromBlock'], 16), int(f['toBlock'], 16))
                else:
                    value = chain.call(method, params)
                out['result'] = value
            except RpcError as exc:
                out['error'] = {'code': -32005 if exc.category == 'too_large' else -32601, 'message': str(exc)}
            raw = json.dumps(out).encode()
            self.send_response(200)
            self.send_header('Content-Length', str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
    return Handler


class RpcTests(unittest.TestCase):
    def test_json_rpc_wire_scan_single_block_overflow_receipt_fallback(self):
        chain = Chain()
        chain.overflow = True
        with running(rpc_server(chain)) as url:
            store = Store(':memory:')
            try:
                store.create_project('p', 'Project')
                store.register('p', 1, TARGET, ABI, 1, 'full')
                scanner = Scanner(store, {1: Rpc(1, [url])}, confirmations=0)
                self.assertEqual(scanner.scan('p', 1, TARGET)['scanned_blocks'], 4)
                self.assertEqual(len(store.query('p', 1, TARGET, 'transactions')['items']), 5)
                self.assertEqual(len(store.query('p', 1, TARGET, 'events')['items']), 3)
            finally:
                store.close()

    def test_identity_envelope_and_historical_trace_capability(self):
        chain = Chain()
        with running(rpc_server(chain)) as url:
            rpc = Rpc(1, [url])
            chain.chain_id = 2
            with self.assertRaises(RpcError) as exc:
                rpc.head()
            self.assertEqual(exc.exception.category, 'identity')
            chain.chain_id = 1
            chain.wrong_id = True
            with self.assertRaises(RpcError) as exc:
                rpc.head()
            self.assertEqual(exc.exception.category, 'invalid_response')
            chain.wrong_id = False
            chain.support_trace = False
            with self.assertRaises(RpcError) as exc:
                rpc.call('debug_traceBlockByNumber', ['0x1', {'tracer': 'callTracer'}])
            self.assertEqual(exc.exception.category, 'unsupported')

    def test_adaptive_range_splitting(self):
        chain = Chain()
        original = chain.logs
        calls = []
        def limited(contract, lo, hi):
            calls.append((lo, hi))
            if hi > lo:
                raise RpcError('too_large', 'limit exceeded')
            return original(contract, lo, hi)
        chain.logs = limited
        with running(rpc_server(chain)) as url:
            self.assertEqual(len(Rpc(1, [url]).logs(TARGET, 1, 3)), 3)
        self.assertIn((1, 3), calls)
        self.assertIn((1, 1), calls)
        self.assertIn((3, 3), calls)

    def test_transient_failover_redacts_endpoint(self):
        chain = Chain()
        with running(rpc_server(chain)) as url:
            unavailable = 'http://unavailable.invalid/secret-credential'
            self.assertEqual(Rpc(1, [unavailable, url], timeout=0.2).head(), 4)
            with self.assertRaises(RpcError) as exc:
                Rpc(1, [unavailable], timeout=0.2).head()
            self.assertNotIn('secret', str(exc.exception))


class HttpTests(unittest.TestCase):
    def setUp(self):
        self.store = Store(':memory:')
        self.alpha = self.store.create_project('alpha', 'Alpha')['tokens']
        self.beta = self.store.create_project('beta', 'Beta')['tokens']
        self.server = handler_class(self.store, {1})
        self.running = running(self.server)
        self.url = self.running.__enter__()

    def tearDown(self):
        self.running.__exit__(None, None, None)
        self.store.close()

    def request(self, path, *, token=None, body=None):
        headers = {'Content-Type': 'application/json'}
        if token:
            headers['Authorization'] = 'Bearer '+token
        request = urllib.request.Request(self.url+path, data=json.dumps(body).encode() if body is not None else None, headers=headers)
        try:
            with urllib.request.urlopen(request) as response:
                return response.status, json.load(response)
        except HTTPError as exc:
            with exc:
                return exc.code, json.load(exc)

    def register(self):
        return self.request('/v1/projects/alpha/contracts', token=self.alpha['manage'],
                            body={'chain_id': 1, 'address': TARGET, 'abi': ABI, 'start_block': 1, 'mode': 'full'})

    def test_public_reads_and_project_scoped_manage_auth(self):
        self.assertEqual(self.register()[0], 200)
        path = '/v1/projects/alpha/contracts'
        self.assertEqual(self.request(path)[0], 200)
        self.assertEqual(self.request(path, token=self.beta['manage'])[0], 200)
        self.assertEqual(self.request(path, token=self.alpha['read'])[0], 200)
        self.assertEqual(self.request('/v1/projects', token=self.alpha['manage'])[0], 404)
        self.assertEqual(self.request(path, token=self.alpha['read'], body={})[0], 403)
        self.assertEqual(self.request('/v1/projects/beta/contracts', token=self.beta['read'])[1]['data'], [])

    def test_all_get_resources_are_public_and_posts_still_require_manage(self):
        self.register()
        Scanner(self.store, {1: Chain()}, confirmations=0).scan('alpha', 1, TARGET)
        base = '/v1/projects/alpha/chains/1/contracts/'+TARGET
        for path in ['/v1/projects/alpha', '/v1/projects/alpha/contracts', base+'/coverage',
                     base+'/transactions', base+'/events', base+'/abis', base+'/context/'+TX]:
            with self.subTest(path=path):
                self.assertEqual(self.request(path)[0], 200)
                self.assertEqual(self.request(path, token='invalid-token')[0], 200)
        mutations = [('/v1/projects/alpha/contracts', {'chain_id': 1, 'address': TARGET, 'abi': ABI, 'start_block': 1}),
                     ('/v1/projects/alpha/pause', {'paused': True}),
                     (base+'/pause', {'paused': True}),
                     (base+'/abis', {'abi': [], 'effective_from': 2}),
                     (base+'/backfill', {'from_block': 1})]
        for path, body in mutations:
            with self.subTest(path=path):
                self.assertEqual(self.request(path, body=body)[0], 401)
                self.assertEqual(self.request(path, token='invalid-token', body=body)[0], 401)
                self.assertEqual(self.request(path, token=self.beta['manage'], body=body)[0], 401)
                self.assertEqual(self.request(path, token=self.alpha['read'], body=body)[0], 403)
        self.assertFalse(self.store.project('alpha')['paused'])
        self.assertEqual(self.store.coverage('alpha', 1, TARGET)['next_block'], 5)
        self.assertEqual(self.request('/v1/projects/missing')[0], 404)

    def test_registration_requires_explicit_history_and_configured_chain(self):
        path = '/v1/projects/alpha/contracts'
        self.assertEqual(self.request(path, token=self.alpha['manage'], body={'chain_id': 1, 'address': TARGET, 'abi': ABI})[0], 400)
        self.assertEqual(self.request(path, token=self.alpha['manage'], body={'chain_id': 2, 'address': TARGET, 'abi': ABI, 'start_block': 0})[0], 409)
        self.assertEqual(self.register()[0], 200)
        self.assertEqual(self.register()[0], 409)

    def test_coverage_context_pagination_filters_and_abi_management(self):
        self.register()
        Scanner(self.store, {1: Chain()}, confirmations=0).scan('alpha', 1, TARGET)
        base = '/v1/projects/alpha/chains/1/contracts/'+TARGET
        code, payload = self.request(base+'/context/'+TX+'?limit=1', token=self.alpha['read'])
        self.assertEqual(code, 200)
        self.assertTrue(payload['logs']['next_cursor'])
        self.assertEqual(self.request(base+'/transactions?limit=201', token=self.alpha['read'])[0], 400)
        self.assertEqual(self.request(base+'/events?unknown=1', token=self.alpha['read'])[0], 400)
        self.assertEqual(self.request(base+'/events?limit=1&limit=2', token=self.alpha['read'])[0], 400)
        code, payload = self.request(base+'/events?argument_path=/0&argument_type=uint256&argument_value=9', token=self.alpha['read'])
        self.assertEqual(code, 200)
        self.assertEqual(len(payload['items']), 1)
        self.assertEqual(self.request(base+'/abis', token=self.alpha['manage'], body={'abi': [], 'effective_from': 2})[0], 200)
        self.assertEqual(self.request(base+'/coverage', token=self.alpha['read'])[1]['data']['next_block'], 2)
        self.assertEqual(self.request(base+'/pause', token=self.alpha['manage'], body={'paused': True})[0], 200)
        self.assertEqual(self.request(base+'/backfill', token=self.alpha['manage'], body={'from_block': 1})[0], 200)

    def test_openapi_exposes_all_operations_without_secrets(self):
        code, spec = self.request('/openapi.json')
        self.assertEqual(code, 200)
        self.assertEqual(spec['openapi'], '3.1.0')
        self.assertEqual(len(spec['paths']), 10)
        self.assertNotIn(self.alpha['manage'], json.dumps(spec))
        for operations in spec['paths'].values():
            for method, operation in operations.items():
                self.assertEqual(operation['security'], [] if method == 'get' else [{'ProjectToken': []}])
                if method == 'get':
                    self.assertNotIn('401', operation['responses'])
                    self.assertNotIn('403', operation['responses'])
