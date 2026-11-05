#!/usr/bin/env bash
set -euo pipefail
kind_version=v0.33.0
kind_sha256=aee6151561422756b764a4ae28e7f44cda5af5a9eead3cc9985112b1de8d8e0d
kind_directory=$(mktemp -d)
trap 'rm -rf "$kind_directory"' EXIT
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
  "https://github.com/kubernetes-sigs/kind/releases/download/$kind_version/kind-linux-amd64" \
  --output "$kind_directory/kind-linux-amd64"
(cd "$kind_directory" && printf '%s  kind-linux-amd64\n' "$kind_sha256" | sha256sum --check)
mkdir -p "$HOME/.local/bin"
install -m 0755 "$kind_directory/kind-linux-amd64" "$HOME/.local/bin/kind"
printf '%s\n' "$HOME/.local/bin" >> "$GITHUB_PATH"
