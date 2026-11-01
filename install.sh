#!/usr/bin/env bash
# Builds the web assets and installs the lia binary.
# Usage: ./install.sh [--build-only] [--skip-web]
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

build_only=0
skip_web=0
for arg in "$@"; do
  case "$arg" in
    --build-only) build_only=1 ;;
    --skip-web) skip_web=1 ;;
    -h|--help)
      sed -n '2,3p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: '$1' is required but not installed. $2" >&2
    exit 1
  }
}

need cargo "Install Rust from https://rustup.rs"

if [ "$skip_web" -eq 0 ]; then
  need npm "Install Node.js (needed to build the web assets), or pass --skip-web."
  echo "==> Installing web dependencies"
  npm install
  echo "==> Building web assets"
  npm run build:web
fi

if [ "$build_only" -eq 1 ]; then
  echo "==> Building release binary"
  cargo build --release
  echo "Done: ./target/release/lia"
else
  echo "==> Installing lia"
  cargo install --path . --locked
  echo "Done: run 'lia --help' (ensure ~/.cargo/bin is on your PATH)"
fi

if [ ! -f lia.toml ] && [ -f lia.example.toml ]; then
  echo "Next: copy lia.example.toml and edit it (see docs/configure.md)"
fi
