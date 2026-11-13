"""Operator CLI. Local database access is an administrative trust boundary."""
import argparse
import json
import logging
import signal
import sys
import threading

from chainkit_chain import Rpc, RpcError
from .api import make_server
from .openapi import openapi
from .errors import IndexerError
from .scanner import Scanner
from .store import Store


def main():
    p = argparse.ArgumentParser(description='Project-scoped EVM contract indexer')
    p.add_argument('--database', default='chainkit.sqlite')
    sub = p.add_subparsers(dest='command', required=True)
    create = sub.add_parser('create-project')
    create.add_argument('project')
    create.add_argument('--name', required=True)
    sub.add_parser('list-projects')
    sub.add_parser('openapi')
    rotate = sub.add_parser('rotate-key')
    rotate.add_argument('project')
    rotate.add_argument('--role', choices=['read', 'manage'], required=True)
    pause = sub.add_parser('pause-project')
    pause.add_argument('project')
    pause.add_argument('--resume', action='store_true')
    for name in ('serve', 'scan'):
        cmd = sub.add_parser(name)
        cmd.add_argument('--config', required=True, help='Operator-owned JSON RPC configuration')
        cmd.add_argument('--log-level', choices=['DEBUG', 'INFO', 'WARNING', 'ERROR'], default='INFO')
        if name == 'serve':
            cmd.add_argument('--host', default='127.0.0.1')
            cmd.add_argument('--port', type=int, default=8080)
        else:
            cmd.add_argument('--project', required=True)
            cmd.add_argument('--chain-id', required=True, type=int)
            cmd.add_argument('--address', required=True)
            cmd.add_argument('--max-blocks', type=int, default=20)
    args = p.parse_args()
    logging.basicConfig(level=getattr(logging, getattr(args, 'log_level', 'WARNING')),
                        format='%(asctime)s %(levelname)s %(name)s %(message)s')
    if args.command == 'openapi':
        print(json.dumps(openapi(), indent=2))
        return
    store = Store(args.database)
    stop, worker = threading.Event(), None
    try:
        if args.command == 'create-project':
            result = store.create_project(args.project, args.name)
        elif args.command == 'list-projects':
            result = store.projects()
        elif args.command == 'rotate-key':
            result = {'token': store.rotate_key(args.project, args.role), 'role': args.role}
        elif args.command == 'pause-project':
            result = store.pause_project(args.project, not args.resume)
        else:
            with open(args.config) as f:
                config = json.load(f)
            poll = config.get('poll_interval', 5)
            max_blocks = config.get('max_blocks_per_round', 20)
            if not isinstance(poll, (int, float)) or isinstance(poll, bool) or not 0.1 <= poll <= 3600 or type(max_blocks) is not int or not 1 <= max_blocks <= 1000:
                raise IndexerError('invalid_config', 'Invalid poll interval or block batch limit')
            chains = {int(k): Rpc(int(k), v) for k, v in config['chains'].items()}
            scanner = Scanner(store, chains, confirmations=config.get('confirmations', 12),
                              reorg_window=config.get('reorg_window', 256))
            if args.command == 'scan':
                result = scanner.scan(args.project, args.chain_id, args.address, max_blocks=args.max_blocks)
            else:
                server = make_server(store, set(chains), args.host, args.port)
                worker = threading.Thread(target=scanner.run, args=(stop,), kwargs={
                    'poll_interval': poll, 'max_blocks': max_blocks}, name='chainkit-scanner')
                worker.start()
                def shutdown(signum, frame):
                    stop.set()
                    threading.Thread(target=server.shutdown, daemon=True).start()
                signal.signal(signal.SIGTERM, shutdown)
                signal.signal(signal.SIGINT, shutdown)
                print(f'Chainkit listening on {args.host}:{server.server_port}', file=sys.stderr)
                try:
                    server.serve_forever()
                finally:
                    stop.set()
                    server.server_close()
                return
        print(json.dumps(result, ensure_ascii=False, indent=2))
    except (IndexerError, RpcError) as exc:
        print(json.dumps({'error': getattr(exc, 'code', getattr(exc, 'category', 'error')), 'message': str(exc)}), file=sys.stderr)
        raise SystemExit(1)
    except (OSError, ValueError, TypeError, KeyError):
        print(json.dumps({'error': 'invalid_configuration', 'message': 'Check command inputs, configuration file and local runtime availability'}), file=sys.stderr)
        raise SystemExit(1)
    finally:
        stop.set()
        if worker:
            worker.join()
        store.close()


if __name__ == '__main__':
    main()
