"""HTTP adapter. Public reads, project-scoped bearer auth for writes."""
from __future__ import annotations

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import logging
import sqlite3
from urllib.parse import parse_qs, urlsplit

from .errors import IndexerError
from .store import Store

_LOG = logging.getLogger(__name__)
from .openapi import openapi


def handler_class(store: Store, chain_ids: set[int]):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.0'
        server_version = 'Chainkit/0.1'

        def setup(self):
            super().setup()
            self.connection.settimeout(15)

        def log_message(self, format, *args):
            # No URL/query/token logging (filters may contain application data).
            pass

        def do_GET(self):
            self._handle()

        def do_POST(self):
            self._handle()

        def _send(self, status, payload):
            raw = json.dumps(payload, ensure_ascii=True, separators=(',', ':')).encode()
            if len(raw) > 32*1024*1024:
                status = 413
                raw = b'{"schema_version":"1","error":{"code":"response_too_large","message":"Reduce query page size or use the embedded API for this evidence"}}'
            self.send_response(status)
            self.send_header('Content-Type', 'application/json; charset=utf-8')
            self.send_header('Content-Length', str(len(raw)))
            self.send_header('Cache-Control', 'no-store')
            self.end_headers()
            self.wfile.write(raw)

        def _handle(self):
            try:
                payload = self._dispatch()
                self._send(200, payload)
            except IndexerError as exc:
                self._send(exc.status, {'schema_version': '1', 'error': {'code': exc.code, 'message': exc.message}})
            except (ValueError, TypeError, KeyError, UnicodeError, RecursionError):
                self._send(400, {'schema_version': '1', 'error': {'code': 'invalid_input', 'message': 'Invalid request fields or JSON'}})
            except sqlite3.OperationalError:
                self._send(503, {'schema_version': '1', 'error': {'code': 'storage_unavailable', 'message': 'Storage is busy or unavailable; retry later'}})
            except (BrokenPipeError, ConnectionResetError):
                pass
            except Exception:
                _LOG.exception('Indexer request failed')
                self._send(500, {'schema_version': '1', 'error': {'code': 'internal_error', 'message': 'Internal server error'}})

        def _body(self):
            if self.headers.get('Transfer-Encoding'):
                raise IndexerError('invalid_input', 'Transfer-Encoding is not supported')
            n = int(self.headers.get('Content-Length', '0'))
            if not 1 <= n <= 2*1024*1024:
                raise IndexerError('body_limit', 'JSON body must be 1 byte..2 MiB', 413)
            if self.headers.get_content_type() != 'application/json':
                raise IndexerError('invalid_input', 'Content-Type must be application/json', 415)
            body = self.rfile.read(n)
            if len(body) != n:
                raise IndexerError('invalid_input', 'Incomplete request body')
            value = json.loads(body)
            if not isinstance(value, dict):
                raise IndexerError('invalid_input', 'Expected JSON object')
            return value

        def _dispatch(self):
            if len(self.path) > 8192:
                raise IndexerError('invalid_input', 'URL too long', 414)
            url = urlsplit(self.path)
            if url.path == '/health' and self.command == 'GET':
                return {'schema_version': '1', 'status': 'ok'}
            if url.path == '/openapi.json' and self.command == 'GET':
                return openapi()
            parts = url.path.strip('/').split('/')
            if len(parts) < 3 or parts[:2] != ['v1', 'projects']:
                raise IndexerError('not_found', 'Unknown resource', 404)
            project = parts[2]
            if self.command == 'POST':
                auth = self.headers.get('Authorization', '')
                store.authorize(project, auth[7:] if auth.startswith('Bearer ') else '', manage=True)
            query = parse_qs(url.query, keep_blank_values=True, max_num_fields=20)
            if any(len(v) != 1 for v in query.values()):
                raise IndexerError('invalid_query', 'Duplicate query parameters')
            q = {k: v[0] for k, v in query.items()}
            body = self._body() if self.command == 'POST' else {}
            envelope = lambda value: {'schema_version': '1', 'data': value}
            if len(parts) == 3 and self.command == 'GET':
                return envelope(store.project(project))
            if parts[3:] == ['pause'] and self.command == 'POST':
                return envelope(store.pause_project(project, body['paused']))
            if parts[3:] == ['contracts']:
                if self.command == 'GET':
                    return envelope(store.contracts(project))
                chain = body['chain_id']
                if type(chain) is not int or chain not in chain_ids:
                    raise IndexerError('chain_unconfigured', 'Chain is not configured by the service operator', 409)
                return envelope(store.register(project, chain, body['address'], body['abi'], body['start_block'], body.get('mode', 'full')))
            if len(parts) < 8 or parts[3] != 'chains' or parts[5] != 'contracts':
                raise IndexerError('not_found', 'Unknown resource', 404)
            chain, contract, resource = int(parts[4]), parts[6], parts[7]
            if self.command == 'GET':
                if len(parts) == 8 and resource == 'abis':
                    return envelope(store.abi_history(project, chain, contract))
                if len(parts) == 8 and resource == 'coverage':
                    return envelope(store.coverage(project, chain, contract))
                if len(parts) == 8 and resource in ('transactions', 'events'):
                    allowed = {'limit', 'cursor', 'from_block', 'to_block', 'tx_hash', 'signature', 'argument_path', 'argument_type', 'argument_value'}
                    allowed |= {'sender', 'status'} if resource == 'transactions' else {'topic0'}
                    if set(q)-allowed:
                        raise IndexerError('invalid_query', 'Unknown filter')
                    args = {('event_signature' if k == 'signature' else k): int(v) if k in ('limit', 'from_block', 'to_block', 'status') else v for k, v in q.items()}
                    return store.query(project, chain, contract, resource, **args)
                if len(parts) == 9 and resource == 'context':
                    if set(q)-{'limit', 'cursor'}:
                        raise IndexerError('invalid_query', 'Unknown context parameter')
                    return store.context(project, chain, contract, parts[8], limit=int(q.get('limit', 50)), cursor=q.get('cursor'))
            if self.command == 'POST' and len(parts) == 8:
                if resource == 'pause':
                    return envelope(store.pause_contract(project, chain, contract, body['paused']))
                if resource == 'abis':
                    return envelope(store.add_abi(project, chain, contract, body['abi'], body['effective_from'], replace=body.get('replace', False)))
                if resource == 'backfill':
                    return envelope(store.backfill(project, chain, contract, body['from_block']))
            raise IndexerError('not_found', 'Unknown resource', 404)

    return Handler


def make_server(store: Store, chain_ids: set[int], host='127.0.0.1', port=8080) -> ThreadingHTTPServer:
    server = ThreadingHTTPServer((host, port), handler_class(store, chain_ids))
    server.daemon_threads = False
    return server
