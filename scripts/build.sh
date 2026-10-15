#!/bin/sh
# Builds Monitor Input Switcher into target/release/ (or target/debug/ with --debug). Run
# ./scripts/install.sh afterwards to install it for the current user.
set -eu

APP=monitor-input-switcher
ROOT=$(cd "$(dirname "$0")/.." && pwd)

case "${1:-}" in
    "") PROFILE=release; FLAGS=--release ;;
    --debug) PROFILE=debug; FLAGS= ;;
    *) echo "Usage: $0 [--debug]"; exit 2 ;;
esac

command -v cargo >/dev/null 2>&1 || { echo "cargo not found: install Rust from https://rustup.rs"; exit 1; }

# $FLAGS is empty for a debug build, so it's left unquoted on purpose.
cargo build $FLAGS --manifest-path "$ROOT/Cargo.toml"

echo "Built $ROOT/target/$PROFILE/$APP"
if [ "$PROFILE" = debug ]; then
    echo "Install it with: $ROOT/scripts/install.sh --debug"
else
    echo "Install it with: $ROOT/scripts/install.sh"
fi
