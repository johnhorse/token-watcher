#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "==> [1/4] Checking Rust code format (cargo fmt)..."
(cd "$ROOT_DIR/src-tauri" && cargo fmt --all -- --check)

echo "==> [2/4] Checking Clippy lints (cargo clippy -D warnings)..."
(cd "$ROOT_DIR/src-tauri" && cargo clippy --all-targets --locked -- -D warnings)

echo "==> [3/4] Running test suite (cargo test --locked)..."
(cd "$ROOT_DIR/src-tauri" && cargo test --locked)

echo "==> [4/4] Checking UI production build (npm run build)..."
(cd "$ROOT_DIR/ui" && npm run build)

echo "🎉 All local CI parity checks passed cleanly!"
