"""Bounded JSON-RPC client with explicit node identity and adaptive log ranges."""
from __future__ import annotations

import itertools
import json
import time
import urllib.error
import urllib.request
import urllib.parse
from typing import Any

from .abi import address, hash32


class RpcError(RuntimeError):
    def __init__(self, category: str, message: str):
        self.category = category
        super().__init__(message)


def quantity(value: str) -> int:
    if not isinstance(value, str) or not value.startswith('0x') or len(value) > 18:
        raise ValueError('invalid RPC quantity')
    result = int(value[2:], 16)
    if result < 0 or result > 2**63-1:
        raise ValueError('RPC quantity exceeds database range')
    return result


class Rpc:
    """Endpoints are operator configuration, never accepted from project HTTP inputs.

    Each operation validates chain ID and genesis on its selected endpoint. Only
    transient transport faults fail over. Provider error text/URLs never escape.
    """
    def __init__(self, chain_id: int, urls: list[str], *, timeout: float = 15,
                 max_response_bytes: int = 32*1024*1024):
        if not isinstance(urls, list) or not urls or len(urls) > 8 or type(chain_id) is not int or not 1 <= chain_id <= 2**63-1:
            raise ValueError('chain_id and 1..8 RPC endpoints required')
        if any(urllib.parse.urlsplit(u).scheme not in ('http', 'https') for u in urls):
            raise ValueError('RPC endpoints must use HTTP(S)')
        self.chain_id, self.urls = chain_id, list(urls)
        self.timeout, self.max_response_bytes = timeout, max_response_bytes
        self._ids = itertools.count(1)
        self.genesis_hash = None
        self._selected = 0

    def _request(self, url: str, method: str, params: list) -> Any:
        req_id = next(self._ids)
        body = json.dumps({'jsonrpc': '2.0', 'id': req_id, 'method': method, 'params': params}).encode()
        request = urllib.request.Request(url, body, {'Content-Type': 'application/json'})
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                raw = response.read(self.max_response_bytes+1)
            if len(raw) > self.max_response_bytes:
                raise RpcError('too_large', 'RPC response exceeds configured limit')
            payload = json.loads(raw)
        except urllib.error.HTTPError as exc:
            exc.close()
            raise RpcError('transient' if exc.code in (408, 429) or exc.code >= 500 else 'http',
                           f'RPC HTTP status {exc.code}') from None
        except (urllib.error.URLError, TimeoutError, OSError):
            raise RpcError('transient', 'RPC transport failed') from None
        except (ValueError, UnicodeError, RecursionError):
            raise RpcError('invalid_response', 'RPC returned invalid JSON') from None
        if not isinstance(payload, dict) or payload.get('id') != req_id or payload.get('jsonrpc') != '2.0':
            raise RpcError('invalid_response', 'RPC envelope mismatch')
        if 'error' in payload:
            error = payload['error']
            code = error.get('code') if isinstance(error, dict) else None
            text = str(error.get('message', '')).lower() if isinstance(error, dict) else ''
            if code == -32601:
                category = 'unsupported'
            elif code == -32005 or any(s in text for s in ('too many results', 'limit exceeded',
                                                          'response size', 'block range', 'more than')):
                category = 'too_large'
            elif any(s in text for s in ('rate limit', 'timeout', 'temporarily unavailable')):
                category = 'transient'
            else:
                category = 'rpc'
            raise RpcError(category, f'RPC {category} error (code={code})')
        if 'result' not in payload:
            raise RpcError('invalid_response', 'RPC result missing')
        return payload['result']

    def call(self, method: str, params: list) -> Any:
        for attempt in range(len(self.urls)):
            index = (self._selected + attempt) % len(self.urls)
            url = self.urls[index]
            try:
                if quantity(self._request(url, 'eth_chainId', [])) != self.chain_id:
                    raise RpcError('identity', 'RPC chain ID mismatch')
                genesis = self._request(url, 'eth_getBlockByNumber', ['0x0', False])
                if not isinstance(genesis, dict):
                    raise RpcError('history_unavailable', 'Genesis block unavailable')
                got = hash32(genesis['hash'])
                if self.genesis_hash is not None and self.genesis_hash != got:
                    raise RpcError('identity', 'RPC genesis hash mismatch')
                self.genesis_hash = got
                result = self._request(url, method, params)
                self._selected = index
                return result
            except RpcError as exc:
                if exc.category != 'transient' or attempt+1 == len(self.urls):
                    raise
                time.sleep(min(0.1 * 2**attempt, 1))
        raise RpcError('transient', 'No RPC endpoint available')

    def head(self) -> int:
        return quantity(self.call('eth_blockNumber', []))

    def block(self, number: int, full: bool = True) -> dict:
        result = self.call('eth_getBlockByNumber', [hex(number), full])
        if not isinstance(result, dict):
            raise RpcError('history_unavailable', 'Requested block unavailable')
        if quantity(result['number']) != number:
            raise RpcError('invalid_response', 'RPC block number mismatch')
        hash32(result['hash'])
        hash32(result['parentHash'])
        quantity(result['timestamp'])
        if not isinstance(result.get('transactions'), list):
            raise RpcError('invalid_response', 'RPC block transactions missing')
        return result

    def receipt(self, tx_hash: str) -> dict:
        result = self.call('eth_getTransactionReceipt', [hash32(tx_hash)])
        if not isinstance(result, dict):
            raise RpcError('history_unavailable', 'Requested receipt unavailable')
        if hash32(result['transactionHash']) != tx_hash.lower() or not isinstance(result.get('logs'), list):
            raise RpcError('invalid_response', 'Receipt identity mismatch')
        return result

    def logs(self, contract: str, start: int, end: int) -> list[dict]:
        """Inclusive range, splits overflow; single-block fallback never skips."""
        contract = address(contract)
        ranges, out = [(start, end)], []
        while ranges:
            lo, hi = ranges.pop()
            try:
                result = self.call('eth_getLogs', [{'address': contract,
                                   'fromBlock': hex(lo), 'toBlock': hex(hi)}])
                if not isinstance(result, list):
                    raise RpcError('invalid_response', 'RPC logs must be an array')
                for log in result:
                    if address(log['address']) != contract or not lo <= quantity(log['blockNumber']) <= hi:
                        raise RpcError('invalid_response', 'RPC returned logs outside filter')
                out.extend(result)
            except RpcError as exc:
                if exc.category != 'too_large':
                    raise
                if lo < hi:
                    mid = (lo+hi)//2
                    ranges.extend([(mid+1, hi), (lo, mid)])
                else:
                    block = self.block(lo)
                    for tx in block['transactions']:
                        receipt = self.receipt(tx['hash'])
                        if receipt['blockHash'].lower() != block['hash'].lower():
                            raise RpcError('reorg', 'Block changed during receipt fallback')
                        out.extend(l for l in receipt['logs'] if address(l['address']) == contract)
        return out
