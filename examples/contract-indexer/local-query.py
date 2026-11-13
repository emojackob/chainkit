"""Query a locally registered contract through Chainkit's public Store API."""
import argparse
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path[:0] = [str(ROOT / 'packages/chain/src'),
                str(ROOT / 'packages/backend/contract-indexer/src')]

from chainkit_indexer import Store
from chainkit_indexer.errors import IndexerError


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--job', type=Path, required=True)
    parser.add_argument('kind', choices=['coverage', 'abis', 'transactions', 'events', 'context'])
    parser.add_argument('--tx-hash')
    parser.add_argument('--sender')
    parser.add_argument('--status', type=int, choices=[0, 1])
    parser.add_argument('--signature')
    parser.add_argument('--topic0')
    parser.add_argument('--from-block', type=int)
    parser.add_argument('--to-block', type=int)
    parser.add_argument('--limit', type=int, default=50)
    parser.add_argument('--cursor')
    parser.add_argument('--argument-path')
    parser.add_argument('--argument-type')
    parser.add_argument('--argument-value')
    args = parser.parse_args()
    if args.kind == 'context' and not args.tx_hash:
        parser.error('context requires --tx-hash')
    job = json.loads(args.job.read_text())
    database = args.job.resolve().parent / job['database']
    if not database.is_file():
        parser.error('registered database does not exist')
    store = Store(str(database))
    identity = (job['project'], job['chain_id'], job['address'])
    try:
        if args.kind == 'coverage':
            result = store.coverage(*identity)
        elif args.kind == 'abis':
            result = store.abi_history(*identity)
        elif args.kind == 'context':
            result = store.context(*identity, args.tx_hash, limit=args.limit, cursor=args.cursor)
        else:
            result = store.query(*identity, args.kind, limit=args.limit, cursor=args.cursor,
                from_block=args.from_block, to_block=args.to_block, tx_hash=args.tx_hash,
                sender=args.sender, status=args.status, event_signature=args.signature,
                topic0=args.topic0, argument_path=args.argument_path,
                argument_type=args.argument_type, argument_value=args.argument_value)
        print(json.dumps(result, ensure_ascii=False, indent=2))
    except IndexerError as exc:
        print(json.dumps({'error': exc.code, 'message': str(exc)}), file=sys.stderr)
        return 1
    finally:
        store.close()
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
