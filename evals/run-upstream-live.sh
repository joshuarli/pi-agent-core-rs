#!/usr/bin/env bash
set -euo pipefail

eval_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source_root="$eval_root/parity/upstream/source"

# The source checkout and its tsx executable are pin material. The adapter imports the source
# directly and never resolves a host `pi` command.
cd "$source_root"
exec ./node_modules/.bin/tsx "$eval_root/evals/upstream-live-adapter.mts" "$@"
