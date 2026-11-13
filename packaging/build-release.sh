#!/usr/bin/env bash
# Build a release executable without embedding the builder's home/worktree paths.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cargo_cache=$(realpath -m -- "${CARGO_HOME:-$HOME/.cargo}")
delimiter=$'\x1f'
encoded=${CARGO_ENCODED_RUSTFLAGS:-}
if [[ -z $encoded && -n ${RUSTFLAGS:-} ]]; then
  # Cargo itself splits RUSTFLAGS on whitespace. Encoded flags retain paths
  # containing spaces when appending the remapping options below.
  read -r -a inherited <<< "$RUSTFLAGS"
  for flag in "${inherited[@]}"; do
    encoded+="${encoded:+$delimiter}$flag"
  done
fi
for mapping in "$HOME=/build/home" "$root=/build/chainkit" "$cargo_cache=/build/cargo"; do
  encoded+="${encoded:+$delimiter}--remap-path-prefix=$mapping"
done
cd -- "$root"
CARGO_ENCODED_RUSTFLAGS="$encoded" cargo build --locked --release -p chainkit-indexer-cli "$@"
