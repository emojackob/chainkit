# Linux packaging and repository publication

The Rust binary embeds SQLite and does not require Python or an external database server. Build each artifact on its target architecture. Release binaries currently target the GNU Linux environment and require glibc; they are not static musl artifacts. A binary built on a newer distribution may require newer glibc than a target system provides. Package generation validates the ELF architecture and derives its minimum glibc dependency from the binary, rather than claiming an older baseline. The release workflow builds on Ubuntu 22.04 to establish its supported baseline.

## Build installable packages

```sh
bash packaging/build-release.sh
bash packaging/build-packages.sh target/release/chainkit-indexer 0.1.0 dist
```

The release build script remaps home, Cargo cache, and worktree paths in Rust diagnostics so public binaries do not embed builder-specific paths. It accepts additional Cargo build arguments, including `--target`.

The package script needs `ar`, `readelf`, `tar`, `gzip`, and `zstd` and creates both a `.deb` and an Arch `.pkg.tar.zst`. It does not run the binary, contact providers, install packages, start services, or publish releases. Keep the same version across binary metadata and package names. For another architecture, supply `x86_64` or `aarch64` as the fourth argument using a binary built for that architecture.

Install the appropriate local package:

```sh
sudo apt install ./dist/chainkit-indexer_0.1.0_amd64.deb
# On Arch Linux:
sudo pacman -U ./dist/chainkit-indexer-0.1.0-1-x86_64.pkg.tar.zst
```

Configure `/etc/chainkit-indexer/config.toml` before use. Packages do not enable or start the service automatically. The optional service uses a dynamic unprivileged user and keeps its database under `/var/lib/chainkit-indexer`.

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now chainkit-indexer
```

Local administration must target the service database explicitly. Stop the service while performing administrative writes if you need an exclusive maintenance window. Management tokens are returned only on project creation; store them securely. To pass the optional ABI-provider key to systemd, use a root-readable `/etc/chainkit-indexer/environment` containing `CHAINKIT_ETHERSCAN_API_KEY=...`. Do not commit credentials.

`arch/PKGBUILD` is a release template. Replace its `SKIP` checksum with the actual source release checksum before building or publishing it; its prepare step deliberately rejects an unset checksum. No AUR submission is performed by these scripts.

## APT repository

A downloadable `.deb` does not by itself enable `apt install chainkit-indexer`. Generate signed repository metadata on a machine with `apt-utils` and GnuPG:

```sh
bash packaging/publish-apt.sh dist /path/to/apt-repository YOUR_GPG_KEY_ID
```

Upload the output atomically to your own HTTPS host. Distribute the public signing key through a trusted channel. Clients should scope trust with `signed-by`, not the global trusted keyring:

```text
deb [signed-by=/etc/apt/keyrings/chainkit.asc] https://YOUR_PACKAGE_HOST/apt stable main
```

After adding the trusted key and source, clients can run `apt update` and `apt install chainkit-indexer`. `YOUR_PACKAGE_HOST` is an operator-supplied destination; this repository does not claim a hosted package service exists.

## pacman repository

Generate package signatures and a signed database using Arch's `repo-add` and GnuPG:

```sh
bash packaging/publish-pacman.sh dist /path/to/pacman-repository YOUR_GPG_KEY_ID
```

Upload the output atomically to your own HTTPS host. Clients verify and trust the public key and configure a repository such as:

```ini
[chainkit]
SigLevel = Required DatabaseRequired
Server = https://YOUR_PACKAGE_HOST/pacman/$arch
```

They can then install with `pacman -Syu chainkit-indexer`. AUR is a separate source-build distribution channel and is not the same as a pacman binary repository.

Signing keys, hosted repository credentials, and publication destinations are operator-managed. No private key belongs in source control. GitHub release assets and software-repository publication are separate steps.
