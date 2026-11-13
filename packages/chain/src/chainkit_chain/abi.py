"""Bounded Solidity ABI decoding. Values retain types; integers are strings.

No signing or encoding. Unknown/ambiguous data is preserved by callers.
"""
from __future__ import annotations

import json
import re
from dataclasses import dataclass
from typing import Any

_MASK = (1 << 64) - 1
_RC = (0x1, 0x8082, 0x800000000000808A, 0x8000000080008000,
       0x808B, 0x80000001, 0x8000000080008081, 0x8000000000008009,
       0x8A, 0x88, 0x80008009, 0x8000000A, 0x8000808B,
       0x800000000000008B, 0x8000000000008089, 0x8000000000008003,
       0x8000000000008002, 0x8000000000000080, 0x800A,
       0x800000008000000A, 0x8000000080008081, 0x8000000000008080,
       0x80000001, 0x8000000080008008)
_ROT = ((0, 36, 3, 41, 18), (1, 44, 10, 45, 2), (62, 6, 43, 15, 61),
        (28, 55, 25, 21, 56), (27, 20, 39, 8, 14))


def keccak256(data: bytes) -> bytes:
    """Ethereum Keccak-256, intentionally not FIPS SHA3-256."""
    def rot(v: int, n: int) -> int:
        return ((v << n) | (v >> (64 - n))) & _MASK if n else v

    padded = bytearray(data)
    pad_length = 136 - len(padded) % 136
    padded.extend(b'\x81' if pad_length == 1 else b'\x01' + b'\0' * (pad_length-2) + b'\x80')
    state = [0] * 25
    for offset in range(0, len(padded), 136):
        for i in range(17):
            state[i] ^= int.from_bytes(padded[offset+i*8:offset+i*8+8], 'little')
        for rc in _RC:
            c = [state[x] ^ state[x+5] ^ state[x+10] ^ state[x+15] ^ state[x+20]
                 for x in range(5)]
            d = [c[(x-1) % 5] ^ rot(c[(x+1) % 5], 1) for x in range(5)]
            b = [0] * 25
            for x in range(5):
                for y in range(5):
                    b[y + 5*((2*x+3*y) % 5)] = rot(state[x+5*y] ^ d[x], _ROT[x][y])
            for x in range(5):
                for y in range(5):
                    state[x+5*y] = b[x+5*y] ^ ((~b[(x+1) % 5+5*y]) & b[(x+2) % 5+5*y])
            state[0] ^= rc
    return b''.join(v.to_bytes(8, 'little') for v in state)[:32]


def hex_bytes(value: str) -> bytes:
    if not isinstance(value, str) or not re.fullmatch(r'0x(?:[0-9a-fA-F]{2})*', value):
        raise ValueError('expected even-length 0x-prefixed hex bytes')
    return bytes.fromhex(value[2:])


def address(value: str) -> str:
    if len(hex_bytes(value)) != 20:
        raise ValueError('expected a 20-byte EVM address')
    return value.lower()


def hash32(value: str) -> str:
    if len(hex_bytes(value)) != 32:
        raise ValueError('expected a 32-byte hash')
    return value.lower()


@dataclass(frozen=True)
class Type:
    kind: str
    bits: int = 0
    child: Type | None = None
    length: int | None = None
    components: tuple[tuple[str, Type], ...] = ()

    @property
    def dynamic(self) -> bool:
        return (self.kind in ('bytes', 'string') or
                self.kind == 'array' and (self.length is None or self.child.dynamic) or
                self.kind == 'tuple' and any(t.dynamic for _, t in self.components))

    @property
    def size(self) -> int:
        if self.dynamic:
            return 32
        if self.kind == 'array':
            return self.length * self.child.size
        if self.kind == 'tuple':
            return sum(t.size for _, t in self.components)
        return 32

    @property
    def canonical(self) -> str:
        if self.kind == 'array':
            return f'{self.child.canonical}[{self.length if self.length is not None else ""}]'
        if self.kind == 'tuple':
            return '(' + ','.join(t.canonical for _, t in self.components) + ')'
        if self.kind in ('uint', 'int', 'fixedbytes'):
            return ('bytes' if self.kind == 'fixedbytes' else self.kind) + str(self.bits)
        return self.kind


def parse_type(param: dict, depth: int = 0) -> Type:
    if depth > 16:
        raise ValueError('ABI nesting exceeds 16')
    typ = param['type']
    m = re.fullmatch(r'(.+)\[([0-9]*)\]', typ)
    if m:
        n = int(m[2]) if m[2] else None
        if n is not None and not 1 <= n <= 4096:
            raise ValueError('ABI array length outside 1..4096')
        return Type('array', child=parse_type({**param, 'type': m[1]}, depth+1), length=n)
    if typ == 'tuple':
        parts = param.get('components')
        if not isinstance(parts, list) or len(parts) > 256:
            raise ValueError('invalid tuple components')
        return Type('tuple', components=tuple((p.get('name', ''), parse_type(p, depth+1)) for p in parts))
    if typ in ('address', 'bool', 'bytes', 'string', 'function'):
        return Type(typ)
    m = re.fullmatch(r'(uint|int)([0-9]*)', typ)
    if m:
        bits = int(m[2] or 256)
        if bits not in range(8, 257, 8):
            raise ValueError('invalid integer width')
        return Type(m[1], bits=bits)
    m = re.fullmatch(r'bytes([0-9]+)', typ)
    if m and 1 <= int(m[1]) <= 32:
        return Type('fixedbytes', bits=int(m[1]))
    raise ValueError(f'unsupported ABI type: {typ}')


class Decoder:
    def __init__(self, data: bytes):
        if len(data) > 1_048_576:
            raise ValueError('ABI payload exceeds 1 MiB')
        self.data = data
        self.budget = 16384

    def take(self, offset: int, size: int) -> bytes:
        if offset < 0 or size < 0 or offset + size > len(self.data):
            raise ValueError('ABI data out of bounds')
        return self.data[offset:offset+size]

    def word(self, offset: int) -> int:
        return int.from_bytes(self.take(offset, 32), 'big')

    def sequence(self, types: list[Type], base: int) -> list[Any]:
        head_size = sum(t.size for t in types)
        self.take(base, head_size)
        values, offset = [], base
        for typ in types:
            if typ.dynamic:
                relative = self.word(offset)
                if relative % 32 or relative < head_size:
                    raise ValueError('invalid ABI offset')
                pos = base + relative
            else:
                pos = offset
            values.append(self.value(typ, pos))
            offset += typ.size
        return values

    def value(self, typ: Type, pos: int) -> Any:
        self.budget -= 1
        if self.budget < 0:
            raise ValueError('ABI element budget exceeded')
        if typ.kind == 'array':
            n = self.word(pos) if typ.length is None else typ.length
            if n > 4096:
                raise ValueError('ABI array exceeds 4096 elements')
            return self.sequence([typ.child] * n, pos+32 if typ.length is None else pos)
        if typ.kind == 'tuple':
            vals = self.sequence([t for _, t in typ.components], pos)
            return [{'position': i, 'name': name, 'type': t.canonical, 'value': v}
                    for i, ((name, t), v) in enumerate(zip(typ.components, vals))]
        if typ.kind in ('bytes', 'string'):
            n = self.word(pos)
            raw = self.take(pos+32, n)
            padded = self.take(pos+32, (n+31)//32*32)
            if any(padded[n:]):
                raise ValueError('nonzero ABI padding')
            return raw.decode('utf-8', errors='strict') if typ.kind == 'string' else '0x' + raw.hex()
        raw = self.take(pos, 32)
        num = int.from_bytes(raw, 'big')
        if typ.kind == 'uint':
            if num >= 1 << typ.bits:
                raise ValueError('integer exceeds ABI width')
            return str(num)
        if typ.kind == 'int':
            signed = int.from_bytes(raw, 'big', signed=True)
            if not -(1 << (typ.bits-1)) <= signed < 1 << (typ.bits-1):
                raise ValueError('invalid signed ABI extension')
            return str(signed)
        if typ.kind == 'bool':
            if num not in (0, 1):
                raise ValueError('invalid ABI bool')
            return bool(num)
        if typ.kind == 'address':
            if any(raw[:12]):
                raise ValueError('invalid ABI address padding')
            return '0x' + raw[12:].hex()
        n = 24 if typ.kind == 'function' else typ.bits
        if any(raw[n:]):
            raise ValueError('invalid fixed bytes padding')
        return '0x' + raw[:n].hex()


class ABI:
    def __init__(self, value: list | dict):
        items = value.get('abi') if isinstance(value, dict) else value
        if not isinstance(items, list) or len(items) > 2048:
            raise ValueError('expected ABI array with at most 2048 items')
        self.items = items
        self.entries = []
        for item in items:
            if not isinstance(item, dict):
                raise ValueError('invalid ABI item')
            kind = item.get('type')
            if kind not in ('function', 'event', 'error', 'constructor', 'fallback', 'receive'):
                raise ValueError('invalid ABI item type')
            if kind in ('function', 'event', 'error', 'constructor'):
                inputs = item.get('inputs', [])
                if not isinstance(inputs, list) or len(inputs) > 256:
                    raise ValueError('invalid ABI inputs')
                types = [parse_type(p) for p in inputs]
                if kind == 'constructor':
                    continue
                if not re.fullmatch(r'[A-Za-z_$][A-Za-z0-9_$]*', item.get('name', '')):
                    raise ValueError('invalid ABI name')
                sig = item['name'] + '(' + ','.join(t.canonical for t in types) + ')'
                self.entries.append(({**item, 'inputs': inputs}, types, sig, keccak256(sig.encode())))
        self.json = json.dumps(items, sort_keys=True, separators=(',', ':'), ensure_ascii=True)
        self.hash = '0x' + keccak256(self.json.encode()).hex()

    def decode(self, kind: str, data: str, topics: list[str] | None = None) -> dict:
        candidates, errors = [], []
        try:
            raw = hex_bytes(data)
            ts = [hex_bytes(hash32(t)) for t in topics or []]
            for item, types, signature, digest in self.entries:
                if item['type'] != kind:
                    continue
                if kind == 'event':
                    anonymous = item.get('anonymous', False)
                    if not anonymous and (not ts or ts[0] != digest):
                        continue
                elif raw[:4] != digest[:4]:
                    continue
                try:
                    if kind == 'event':
                        indexed = [bool(p.get('indexed', False)) for p in item['inputs']]
                        topic_values = ts if anonymous else ts[1:]
                        if len(topic_values) != sum(indexed):
                            raise ValueError('event topic count mismatch')
                        plain = iter(Decoder(raw).sequence([t for t, idx in zip(types, indexed) if not idx], 0))
                        topic_iter = iter(topic_values)
                        vals = []
                        for t, idx in zip(types, indexed):
                            if idx:
                                word = next(topic_iter)
                                vals.append({'hash': '0x'+word.hex(), 'recoverable': False}
                                            if t.kind in ('array', 'tuple', 'bytes', 'string')
                                            else Decoder(word).value(t, 0))
                            else:
                                vals.append(next(plain))
                    else:
                        indexed = [False] * len(types)
                        vals = Decoder(raw[4:]).sequence(types, 0)
                    candidates.append({'signature': signature, 'arguments': [
                        {'position': i, 'name': p.get('name', ''), 'type': t.canonical,
                         'indexed': idx, 'value': v}
                        for i, (p, t, idx, v) in enumerate(zip(item['inputs'], types, indexed, vals))]})
                except (ValueError, UnicodeError) as exc:
                    errors.append({'signature': signature, 'error': str(exc)})
            return {'status': 'decoded' if len(candidates) == 1 else 'ambiguous' if candidates else
                    'invalid' if errors else 'unknown', 'abi_hash': self.hash,
                    'candidates': candidates, 'errors': errors, 'decoder_version': '1'}
        except ValueError as exc:
            return {'status': 'invalid', 'abi_hash': self.hash, 'candidates': [],
                    'errors': [{'error': str(exc)}], 'decoder_version': '1'}
