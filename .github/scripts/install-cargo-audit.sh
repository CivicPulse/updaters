#!/usr/bin/env bash
# Official RustSec release, with its GitHub release asset SHA-256 pinned here.
set -euo pipefail
archive="$(mktemp)"
directory="$(mktemp -d)"
trap 'rm -f "$archive"; rm -rf "$directory"' EXIT
curl --fail --silent --show-error --location --max-time 120 \
  https://github.com/rustsec/rustsec/releases/download/cargo-audit/v0.22.2/cargo-audit-x86_64-unknown-linux-gnu-v0.22.2.tgz \
  --output "$archive"
printf '%s  %s\n' ab28a1bdb54db4d5d8ad5981cf1f959410370b3d28250dbd35f6a44248620e39 "$archive" | sha256sum --check --status
tar -xzf "$archive" -C "$directory" --strip-components=1
mkdir -p "$HOME/.cargo/bin"
install -m 0755 "$directory/cargo-audit" "$HOME/.cargo/bin/cargo-audit"
