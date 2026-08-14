#!/usr/bin/env bash
set -euo pipefail

eval_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

# This is an opt-in executable integration, not a core runtime dependency. Pin the same nightly
# declared by rust-toolchain.toml; do not fall back to stable Rust.
cd "$eval_root"
exec cargo +nightly-2026-07-24 run --quiet -p pi-agent-core --features eval-runner --bin pi-agent-eval -- "$@"
