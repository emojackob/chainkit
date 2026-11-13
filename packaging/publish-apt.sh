#!/usr/bin/env bash
# Generate a signed APT repository in an operator-provided output directory.
set -euo pipefail
if (($# != 3)); then
  echo 'Usage: publish-apt.sh PACKAGE_DIR OUTPUT_DIR GPG_SIGNING_KEY' >&2
  exit 2
fi
packages=$(realpath -- "$1")
mkdir -p -- "$2"
output=$(realpath -- "$2")
key=$3
command -v apt-ftparchive >/dev/null
command -v gpg >/dev/null
mkdir -p "$output/pool/main" "$output/dists/stable/main"
shopt -s nullglob
artifacts=("$packages"/*.deb)
((${#artifacts[@]})) || { echo 'No Debian packages found' >&2; exit 1; }
cp -- "${artifacts[@]}" "$output/pool/main/"
cd "$output"
for arch in amd64 arm64; do
  mkdir -p "dists/stable/main/binary-$arch"
  apt-ftparchive --arch "$arch" packages pool/main > "dists/stable/main/binary-$arch/Packages"
  gzip -n -c "dists/stable/main/binary-$arch/Packages" > "dists/stable/main/binary-$arch/Packages.gz"
done
# Scan only fresh package indexes. Existing Release/signature files are excluded,
# including on repeated runs, and output is written outside the scanned tree.
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work/indexes"
cp -a dists/stable/main "$work/indexes/main"
apt-ftparchive -o APT::FTPArchive::Release::Origin=Chainkit \
  -o APT::FTPArchive::Release::Label=Chainkit \
  -o APT::FTPArchive::Release::Suite=stable \
  -o APT::FTPArchive::Release::Codename=stable \
  -o 'APT::FTPArchive::Release::Architectures=amd64 arm64' \
  -o APT::FTPArchive::Release::Components=main release "$work/indexes" > "$work/Release"
gpg --batch --yes --local-user "$key" --clearsign --output "$work/InRelease" "$work/Release"
gpg --batch --yes --local-user "$key" --armor --detach-sign --output "$work/Release.gpg" "$work/Release"
gpg --batch --armor --export "$key" > "$work/chainkit.asc"
mv -- "$work/Release" "$work/InRelease" "$work/Release.gpg" dists/stable/
mv -- "$work/chainkit.asc" chainkit.asc
printf 'Signed APT metadata generated in %s; upload atomically to your HTTPS host.\n' "$output"
