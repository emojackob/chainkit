#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
export PYTHONPATH="$root/packages/chain/src:$root/packages/backend/contract-indexer/src${PYTHONPATH:+:$PYTHONPATH}"
jobdir="${CHAINKIT_JOB_DIR:?Set CHAINKIT_JOB_DIR to a local directory containing job.json and rpc-config.json}"
jobdir=$(cd -- "$jobdir" && pwd)
command=${1:-coverage}
if (($#)); then shift; fi
mapfile -t identity < <(python3 - "$jobdir/job.json" <<'PY'
import json
import sys
from pathlib import Path

job = json.loads(Path(sys.argv[1]).read_text())
for key in ('project', 'chain_id', 'address'):
    print(job[key])
print(Path(sys.argv[1]).resolve().parent / job['database'])
PY
)
if ((${#identity[@]} != 4)); then
  echo 'Invalid job.json: expected project, chain_id, address and database' >&2
  exit 1
fi
case "$command" in
  scan)
    exec python3 -m chainkit_indexer --database "${identity[3]}" scan \
      --config "$jobdir/rpc-config.json" --project "${identity[0]}" --chain-id "${identity[1]}" \
      --address "${identity[2]}" "$@"
    ;;
  serve)
    exec python3 -m chainkit_indexer --database "${identity[3]}" serve \
      --config "$jobdir/rpc-config.json" "$@"
    ;;
  *)
    exec python3 "$root/examples/contract-indexer/local-query.py" \
      --job "$jobdir/job.json" "$command" "$@"
    ;;
esac
