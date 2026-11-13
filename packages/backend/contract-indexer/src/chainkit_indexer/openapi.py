"""Machine-readable contract for project-scoped evidence tools."""
import re

BASE = '/v1/projects/{project}/chains/{chain_id}/contracts/{address}'


def ref(name):
    return {'$ref': '#/components/schemas/'+name}


def obj(properties, required=()):
    return {'type': 'object', 'properties': properties, 'required': list(required)}


def envelope(schema):
    return obj({'schema_version': {'const': '1'}, 'data': schema}, ('schema_version', 'data'))


def page(item):
    return obj({'schema_version': {'const': '1'}, 'items': {'type': 'array', 'items': ref(item)},
                'next_cursor': {'type': ['string', 'null'], 'description': 'Opaque cursor; retain filters and restart on snapshot_invalidated.'},
                'coverage': ref('Coverage')}, ('schema_version', 'items', 'next_cursor', 'coverage'))


def openapi():
    text = {'type': 'string'}
    integer = {'type': 'integer', 'minimum': 0}
    address = {'type': 'string', 'pattern': '^0x[0-9a-fA-F]{40}$'}
    hash32 = {'type': 'string', 'pattern': '^0x[0-9a-fA-F]{64}$'}
    abi = {'oneOf': [{'type': 'array', 'items': {'type': 'object'}},
                     obj({'abi': {'type': 'array', 'items': {'type': 'object'}}}, ('abi',))]}
    evidence = {'tx_hash': hash32, 'block_hash': hash32, 'block_number': integer, 'transaction_index': integer}
    schemas = {
        'Argument': obj({'position': integer, 'name': text, 'type': text, 'indexed': {'type': 'boolean'},
            'value': {'description': 'Integer = decimal string, address/bytes = hex. Tuples retain typed positional fields. Indexed dynamic values are hashes, not recoverable plaintext.'}}, ('position', 'type', 'value')),
        'Decoding': obj({'status': {'enum': ['decoded', 'unknown', 'invalid', 'ambiguous']},
            'abi_hash': hash32, 'decoder_version': text, 'reason': text,
            'candidates': {'type': 'array', 'items': obj({'signature': text, 'arguments': {'type': 'array', 'items': ref('Argument')}}, ('signature', 'arguments'))},
            'errors': {'type': 'array', 'items': {'type': 'object'}}}, ('status', 'candidates', 'decoder_version')),
        'Coverage': obj({'start_block': integer, 'next_block': integer,
            'indexed_range': {'type': 'array', 'items': integer, 'minItems': 2, 'maxItems': 2, 'description': '[from,to), never a claim about unscanned history.'},
            'range_semantics': {'const': '[from,to)'}, 'snapshot_block_hash': {'type': ['string', 'null']},
            'query_snapshot_to': integer, 'query_snapshot_hash': {'type': ['string', 'null']},
            'requested_range': {'type': 'array', 'items': {'type': ['integer', 'null']}},
            'request_fully_covered': {'type': 'boolean', 'description': 'Only true for an explicit bounded request fully inside the snapshot. Check capabilities separately.'},
            'observed_head': {'type': ['integer', 'null']}, 'revision': integer, 'abi_revision': integer,
            'mode': {'enum': ['basic', 'full']}, 'paused': {'type': 'boolean'}, 'project_paused': {'type': 'boolean'},
            'last_error': {'type': ['string', 'null']}, 'updated_at': {'type': ['string', 'null']},
            'integrity': {'enum': ['last_verified_snapshot', 'recovery_required']},
            'capabilities': obj({k: text for k in ('emitted_events', 'direct_transactions', 'contract_creation', 'internal_calls', 'external_token_transfers')}),
            'warnings': {'type': 'array', 'items': text}}, ('start_block', 'next_block', 'indexed_range', 'capabilities', 'integrity')),
        'Transaction': obj({**evidence, 'from': address, 'to': {'type': ['string', 'null']},
            'status': {'type': 'integer', 'enum': [0, 1], 'description': 'Execution status, not a diagnosis of cause.'},
            'relations': {'type': 'array', 'items': {'enum': ['direct_call', 'emitted_event', 'creation', 'call_trace']}},
            'decoded': ref('Decoding')}, ('tx_hash', 'block_hash', 'block_number', 'status', 'relations', 'decoded')),
        'Event': obj({**evidence, 'log_index': integer, 'emitter': address, 'raw': {'type': 'object'}, 'decoded': ref('Decoding')},
                     ('tx_hash', 'block_hash', 'log_index', 'emitter', 'raw', 'decoded')),
        'TransactionPage': page('Transaction'), 'EventPage': page('Event'),
        'Context': obj({'schema_version': {'const': '1'},
            'data': {'oneOf': [{'type': 'null'}, {'allOf': [ref('Transaction'), obj({
                'raw': {'type': 'object', 'description': 'Original RPC transaction; amounts remain hex strings.'},
                'receipt': {'type': 'object', 'description': 'Original receipt without logs; logs are paginated separately.'},
                'fetched_at': text, 'source': {'const': 'configured_rpc'},
                'trace': obj({'status': {'enum': ['available', 'not_supported']},
                              'data': obj({'raw': {'type': 'object'}, 'related_frames': {'type': 'array', 'items': {'type': 'object'}}, 'abi_binding': text})})})]}]},
            'logs': ref('EventPage'), 'warnings': {'type': 'array', 'items': text}}, ('schema_version', 'data', 'logs', 'warnings')),
        'Project': obj({'id': text, 'name': text, 'paused': {'type': 'integer', 'enum': [0, 1]}, 'created_at': text}, ('id', 'name', 'paused')),
        'Contract': obj({'project_id': text, 'chain_id': integer, 'address': address, 'mode': {'enum': ['basic', 'full']},
                         'start_block': integer, 'next_block': integer, 'paused': {'type': 'integer'}, 'revision': integer}),
        'Error': obj({'schema_version': {'const': '1'}, 'error': obj({'code': text, 'message': text}, ('code', 'message'))}, ('schema_version', 'error')),
    }
    register = obj({'chain_id': {'type': 'integer', 'minimum': 1}, 'address': address, 'abi': abi,
                    'start_block': integer, 'mode': {'enum': ['basic', 'full'], 'default': 'full'}}, ('chain_id', 'address', 'abi', 'start_block'))
    pause = obj({'paused': {'type': 'boolean'}}, ('paused',))
    bindings = obj({'abi': abi, 'effective_from': integer, 'replace': {'type': 'boolean', 'default': False}}, ('abi', 'effective_from'))
    entries = [
        ('/v1/projects/{project}', 'get', 'Get project metadata', ref('Project'), None),
        ('/v1/projects/{project}/pause', 'post', 'Pause/resume all project indexing; manage token required', ref('Project'), pause),
        ('/v1/projects/{project}/contracts', 'get', 'List this project contract registrations', {'type': 'array', 'items': ref('Contract')}, None),
        ('/v1/projects/{project}/contracts', 'post', 'Register address and ABI with explicit historical start; manage token required', ref('Contract'), register),
        (BASE+'/coverage', 'get', 'Check range, integrity, mode and freshness before interpreting empty results', ref('Coverage'), None),
        (BASE+'/transactions', 'get', 'List related transactions, including failed/no-event calls; internal relations require full mode', ref('TransactionPage'), None),
        (BASE+'/events', 'get', 'List all events emitted by target; unknown topics remain raw', ref('EventPage'), None),
        (BASE+'/context/{tx_hash}', 'get', 'Get evidence, trace error data and paginated receipt logs; null does not prove absence', ref('Context'), None),
        (BASE+'/pause', 'post', 'Pause/resume this registration; manage token required', ref('Contract'), pause),
        (BASE+'/abis', 'get', 'List ABI revision history; bindings are operator supplied', {'type': 'array', 'items': obj({'effective_from': integer, 'revision': integer, 'abi_hash': hash32, 'created_at': text})}, None),
        (BASE+'/abis', 'post', 'Append/correct ABI binding, preserve revisions and requeue tail scan; manage token required', obj({'abi_hash': hash32, 'effective_from': integer}), bindings),
        (BASE+'/backfill', 'post', 'Invalidate indexed tail and queue rescan; manage token required', ref('Coverage'), obj({'from_block': integer}, ('from_block',))),
    ]
    descriptions = {
        'limit': 'Page size (default 50, maximum 200).', 'cursor': 'Pass next_cursor with unchanged filters.',
        'from_block': 'Inclusive lower block bound.', 'to_block': 'Exclusive upper block bound.',
        'signature': 'Full canonical function/event signature, including argument types.',
        'sender': 'Top-level transaction sender.', 'status': '0 failed, 1 succeeded.',
        'argument_path': 'Positional path such as /0 or /0/1. Requires argument_type and argument_value.',
        'argument_type': 'Canonical ABI type, or indexed_hash:string etc. Requires path and value.',
        'argument_value': 'Exact string; decimal integer, true/false, or lowercase address/bytes hex.',
    }
    paths = {}
    for path, method, summary, output, body in entries:
        params = [{'name': p, 'in': 'path', 'required': True, 'schema': address if p == 'address' else hash32 if p == 'tx_hash' else integer if p == 'chain_id' else text}
                  for p in re.findall(r'\{(\w+)\}', path)]
        is_page = path.endswith(('/events', '/transactions'))
        is_context = '/context/' in path
        names = []
        if method == 'get' and (is_page or is_context):
            names = ['limit', 'cursor']
            if is_page:
                names += ['from_block', 'to_block', 'tx_hash', 'signature', 'argument_path', 'argument_type', 'argument_value']
                names += ['sender', 'status'] if path.endswith('/transactions') else ['topic0']
        for name in names:
            schema = integer if name in ('from_block', 'to_block', 'status') else hash32 if name in ('tx_hash', 'topic0') else address if name == 'sender' else text
            if name == 'limit':
                schema = {'type': 'integer', 'minimum': 1, 'maximum': 200, 'default': 50}
            params.append({'name': name, 'in': 'query', 'schema': schema, 'description': descriptions.get(name, name)})
        op = {'summary': summary, 'operationId': method+'_'+re.sub(r'[^a-zA-Z0-9]+', '_', path).strip('_'),
              'parameters': params, 'security': [{'ProjectToken': []}] if method == 'post' else [], 'responses': {}}
        actual_output = output if is_page or is_context else envelope(output)
        for code, desc in [(200, 'Success; inspect coverage and warnings'), (400, 'Invalid input'), (401, 'Invalid project credentials'),
                           (403, 'Manage token required'), (404, 'Registration not found'), (409, 'Conflict, invalidated snapshot or chain consistency failure'),
                           (413, 'Body/response too large'), (415, 'JSON required'), (503, 'Storage unavailable; retry later')]:
            if method == 'get' and code in (401, 403):
                continue
            op['responses'][str(code)] = {'description': desc, 'content': {'application/json': {'schema': actual_output if code == 200 else ref('Error')}}}
        if body is not None:
            op['requestBody'] = {'required': True, 'content': {'application/json': {'schema': body}}}
        paths.setdefault(path, {})[method] = op
    return {'openapi': '3.1.0', 'info': {'title': 'Chainkit project contract indexer', 'version': '0.1.0',
            'description': 'GET queries are public. POST operations require a project manage token. Read coverage first. Ranges are [from,to). Missing indexed data never proves chain absence. Full mode requires historical block call traces. ABI bindings are operator supplied.'},
            'paths': paths, 'components': {'securitySchemes': {'ProjectToken': {'type': 'http', 'scheme': 'bearer'}}, 'schemas': schemas}}
