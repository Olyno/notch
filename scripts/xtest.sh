#!/usr/bin/env bash
# Cross-language test runner: builds the real Rust binaries, then drives
# them from Dart. Exits non-zero on any failure.
set -euo pipefail
cd "$(dirname "$0")/.."

(cd rust && cargo build --release -p notch-relay -p notch-mcp -q)
(cd libs/dart && dart pub get >/dev/null && dart test -t xlang "$@")
echo "xtest: all green"
