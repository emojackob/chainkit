#!/usr/bin/env bash
# Package an already built Linux binary without downloading or compiling code.
set -euo pipefail
if (($# < 2 || $# > 4)); then
  echo 'Usage: build-packages.sh BINARY VERSION [OUTPUT_DIR] [x86_64|aarch64]' >&2
  exit 2
fi
binary=$(realpath -- "$1")
version=$2
output=${3:-dist}
architecture=${4:-$(uname -m)}
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+([.+~][a-zA-Z0-9.-]+)?$ ]] || { echo 'Invalid package version' >&2; exit 2; }
case "$architecture" in
  x86_64) debarch=amd64 ;;
  aarch64) debarch=arm64 ;;
  *) echo 'Only x86_64 and aarch64 packages are supported' >&2; exit 2 ;;
esac
[[ -f $binary && -x $binary ]] || { echo 'Executable binary required' >&2; exit 2; }
command -v readelf >/dev/null
machine=$(readelf --file-header "$binary" | sed -n 's/^[[:space:]]*Machine:[[:space:]]*//p')
case "$architecture:$machine" in
  'x86_64:Advanced Micro Devices X86-64'|'aarch64:AArch64') ;;
  *) echo 'Binary ELF architecture does not match requested package architecture' >&2; exit 2 ;;
esac
versions=$(readelf --version-info "$binary")
glibc_version=$(printf '%s\n' "$versions" | sed -n 's/.*Name: GLIBC_\([0-9][0-9.]*\).*/\1/p' | sort -Vu | tail -n 1)
[[ -n $glibc_version ]] || { echo 'GNU Linux binary with versioned glibc requirements expected' >&2; exit 2; }
# DT_RELR has its own ABI marker rather than a numeric GLIBC symbol version.
# The GNU loader first supported this marker in glibc 2.36.
case "$versions" in
  *GLIBC_ABI_DT_RELR*) glibc_version=$(printf '%s\n' "$glibc_version" '2.36' | sort -Vu | tail -n 1) ;;
esac
epoch=${SOURCE_DATE_EPOCH:-$(date +%s)}
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
mkdir -p -- "$output"
output=$(realpath -- "$output")
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
stage="$work/stage"
install -Dm755 -- "$binary" "$stage/usr/bin/chainkit-indexer"
install -Dm644 -- "$root/packaging/systemd/chainkit-indexer.service" "$stage/usr/lib/systemd/system/chainkit-indexer.service"
install -Dm644 -- "$root/examples/rust-indexer/config.example.toml" "$stage/etc/chainkit-indexer/config.toml"
install -Dm644 -- "$root/examples/rust-indexer/README.md" "$stage/usr/share/doc/chainkit-indexer/README.md"
install -Dm644 -- "$root/packaging/README.md" "$stage/usr/share/doc/chainkit-indexer/PACKAGING.md"
mkdir -p "$work/control"
cat > "$work/control/control" <<CONTROL
Package: chainkit-indexer
Version: $version
Architecture: $debarch
Maintainer: Chainkit maintainers
Section: utils
Priority: optional
Depends: libc6 (>= $glibc_version), libgcc-s1
Description: Multi-project EVM contract indexing and evidence queries
 Standalone Rust CLI, HTTP API, and read-only stdio MCP server.
 SQLite is embedded. No Python or external database server is required.
CONTROL
printf '/etc/chainkit-indexer/config.toml\n' > "$work/control/conffiles"
printf '2.0\n' > "$work/debian-binary"
# Portable Debian format: debian-binary, control.tar.gz, data.tar.gz in ar.
tar --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner -C "$work/control" -cf - . | gzip -n > "$work/control.tar.gz"
tar --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner -C "$stage" -cf - . | gzip -n > "$work/data.tar.gz"
(cd "$work" && ar crD "$output/chainkit-indexer_${version}_${debarch}.deb" debian-binary control.tar.gz data.tar.gz)
size=$(du -sb "$stage" | cut -f1)
cat > "$stage/.PKGINFO" <<PKGINFO
pkgname = chainkit-indexer
pkgbase = chainkit-indexer
pkgver = $version-1
pkgdesc = Multi-project EVM contract indexing and evidence queries
url = https://github.com/emojackob/chainkit
builddate = $epoch
packager = Chainkit maintainers
size = $size
arch = $architecture
depend = glibc>=$glibc_version
depend = gcc-libs
backup = etc/chainkit-indexer/config.toml
PKGINFO
tar --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner -C "$stage" --zstd -cf "$output/chainkit-indexer-${version}-1-${architecture}.pkg.tar.zst" .PKGINFO etc usr
printf 'Packages written to %s\n' "$output"
