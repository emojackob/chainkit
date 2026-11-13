#!/usr/bin/env bash
# Generate a signed pacman repository. Signing keys stay in the operator's keyring.
set -euo pipefail
if (($# != 3)); then
  echo 'Usage: publish-pacman.sh PACKAGE_DIR OUTPUT_DIR GPG_SIGNING_KEY' >&2
  exit 2
fi
packages=$(realpath -- "$1")
mkdir -p -- "$2"
output=$(realpath -- "$2")
key=$3
command -v repo-add >/dev/null
command -v gpg >/dev/null
shopt -s nullglob
found=0
for arch in x86_64 aarch64; do
  artifacts=("$packages"/*-"$arch".pkg.tar.zst)
  ((${#artifacts[@]})) || continue
  found=1
  mkdir -p "$output/$arch"
  cp -- "${artifacts[@]}" "$output/$arch/"
  for artifact in "$output/$arch/"*.pkg.tar.zst; do
    gpg --batch --yes --local-user "$key" --detach-sign "$artifact"
  done
  (cd "$output/$arch" && repo-add --prevent-downgrade --sign --key "$key" chainkit.db.tar.gz ./*.pkg.tar.zst)
done
((found)) || { echo 'No supported Arch packages found' >&2; exit 1; }
gpg --batch --armor --export "$key" > "$output/chainkit.asc"
printf 'Signed pacman metadata generated in %s; upload atomically to your HTTPS host.\n' "$output"
