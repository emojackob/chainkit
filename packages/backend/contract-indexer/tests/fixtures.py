from copy import deepcopy
from chainkit_chain.abi import keccak256
from chainkit_chain import RpcError

TARGET = '0x' + '11'*20
OTHER = '0x' + '22'*20
SENDER = '0x' + '33'*20
TOKEN = '0x' + '44'*20
TX = '0x' + 'aa'*32
ABI = [
    {'type': 'function', 'name': 'set', 'inputs': [{'name': 'value', 'type': 'uint256'}]},
    {'type': 'event', 'name': 'Changed', 'inputs': [{'name': 'value', 'type': 'uint256', 'indexed': False}]},
]


def word(n):
    return n.to_bytes(32, 'big').hex()


def h(n):
    return '0x' + f'{n:064x}'


class Chain:
    """Deterministic canonical chain with explicit receipts and callTracer data."""
    def __init__(self):
        self.blocks, self.receipts, self.trace_data = {}, {}, {}
        self.overflow = False
        self.support_trace = True
        self.omit_logs = False
        self.queries = []
        for n in range(5):
            self.blocks[n] = {'number': hex(n), 'hash': h(n+100), 'parentHash': h(n+99),
                              'timestamp': hex(1700000000+n), 'transactions': []}
        self.blocks[0]['parentHash'] = h(0)
        self.add(1, TX, to=TARGET, values=[7, 9])
        self.add(1, h(301), to=TARGET, values=[], status=0)
        self.add(2, h(302), to=OTHER, values=[11], internal=True)
        self.add(3, h(303), to=OTHER, values=[], internal=True)
        self.add(3, h(304), to=TARGET, values=[])

    def add(self, block, txhash, *, to, values, status=1, internal=False):
        b = self.blocks[block]
        i = len(b['transactions'])
        tx = {'hash': txhash, 'blockHash': b['hash'], 'blockNumber': hex(block),
              'transactionIndex': hex(i), 'from': SENDER, 'to': to,
              'input': '0x'+keccak256(b'set(uint256)')[:4].hex()+word(2**255+1),
              'value': '0x0', 'nonce': hex(i)}
        b['transactions'].append(tx)
        logstart = sum(len(self.receipts[t['hash']]['logs']) for t in b['transactions'][:-1])
        logs = [{'address': TARGET, 'transactionHash': txhash, 'blockHash': b['hash'],
                 'blockNumber': hex(block), 'transactionIndex': hex(i), 'logIndex': hex(logstart+j),
                 'topics': ['0x'+keccak256(b'Changed(uint256)').hex()], 'data': '0x'+word(value)}
                for j, value in enumerate(values)]
        self.receipts[txhash] = {'transactionHash': txhash, 'blockHash': b['hash'], 'blockNumber': hex(block),
                                'transactionIndex': hex(i), 'status': hex(status), 'logs': logs, 'gasUsed': '0x5208', 'contractAddress': None}
        self.trace_data[txhash] = {'type': 'CALL', 'from': SENDER, 'to': to, 'input': tx['input'],
                                  **({'calls': [{'type': 'CALL', 'from': OTHER, 'to': TARGET, 'input': '0x'}]} if internal else {})}

    def head(self):
        return max(self.blocks)

    def block(self, number, full=True):
        self.queries.append(('block', number))
        if number not in self.blocks:
            raise RpcError('history_unavailable', 'Block unavailable')
        result = deepcopy(self.blocks[number])
        if not full:
            result['transactions'] = [t['hash'] for t in result['transactions']]
        return result

    def receipt(self, txhash):
        return deepcopy(self.receipts[txhash])

    def logs(self, contract, lo, hi):
        if self.omit_logs:
            return []
        return deepcopy([l for n, b in self.blocks.items() if lo <= n <= hi
                         for t in b['transactions'] for l in self.receipts[t['hash']]['logs'] if l['address'] == contract])

    def call(self, method, params):
        if method == 'debug_traceBlockByNumber':
            if not self.support_trace:
                raise RpcError('unsupported', 'Block tracing unavailable')
            return [{'txHash': t['hash'], 'result': deepcopy(self.trace_data[t['hash']])}
                    for t in self.blocks[int(params[0], 16)]['transactions']]
        raise AssertionError(method)

    def fork(self, after, *, remove=True):
        for n in sorted(self.blocks):
            if n <= after:
                continue
            b = self.blocks[n]
            b['hash'] = h(1000+n)
            b['parentHash'] = self.blocks[n-1]['hash']
            for tx in b['transactions']:
                tx['blockHash'] = b['hash']
                self.receipts[tx['hash']]['blockHash'] = b['hash']
                for log in self.receipts[tx['hash']]['logs']:
                    log['blockHash'] = b['hash']
            if remove:
                b['transactions'] = []
