#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
fixtures="$script_dir/fixtures/recorded/openrouter"
python3 "$script_dir/recorded/replay.py" --check
python3 "$script_dir/recorded/replay.py" \
  "$fixtures/poolside-laguna-xs-2.1-privacy-restricted.json" \
  --expected "$fixtures/poolside-laguna-xs-2.1-privacy-restricted.canonical.json" \
  --check
python3 "$script_dir/recorded/replay.py" \
  "$fixtures/deepseek-deepseek-v4-flash-0731-success.json" \
  --expected "$fixtures/deepseek-deepseek-v4-flash-0731-success.canonical.json" \
  --check
