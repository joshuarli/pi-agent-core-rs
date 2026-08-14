#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source_dir="$root/parity/upstream/source"

cd "$source_dir"
./node_modules/.bin/tsx ../../profile/verify-profile.mts
./node_modules/.bin/tsx ../../profile/verify-grep-process.mts
