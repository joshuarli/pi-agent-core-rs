#!/usr/bin/env bash

# Run the complete provider-free declarative corpus through both pinned SDK
# adapters and compare every result with its checked-in canonical expectation.
#
# This script deliberately invokes the TypeScript SDK runner in-process from
# the pinned source checkout. It never invokes `pi`, another Pi CLI, npm, vault,
# or a provider. The upstream checkout and its locked node_modules are setup
# prerequisites; this command has no installation or network fallback.

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
fixture_dir="$repo_root/parity/fixtures/declarative"
expected_dir="$repo_root/parity/fixtures/expected"
upstream_dir="$repo_root/parity/upstream/source"
upstream_runner="$repo_root/parity/upstream/agent-runner.mts"
rust_toolchain="nightly-2026-07-24"

die() {
	echo "parity corpus: $*" >&2
	exit 2
}

command -v jq >/dev/null 2>&1 || die "jq is required to compare canonical JSON (no installation is attempted)"
command -v cargo >/dev/null 2>&1 || die "cargo is required"
command -v git >/dev/null 2>&1 || die "git is required"

[[ -d "$fixture_dir" ]] || die "missing declarative fixture directory: $fixture_dir"
[[ -d "$expected_dir" ]] || die "missing expected-result directory: $expected_dir"
[[ -d "$upstream_dir/.git" ]] || die "missing pinned upstream checkout: $upstream_dir"
[[ -x "$upstream_dir/node_modules/.bin/tsx" ]] || die "missing pinned upstream tsx; run the documented offline dependency setup first"
[[ -f "$upstream_runner" ]] || die "missing upstream runner: $upstream_runner"

pinned_commit=$(sed -nE 's/^Commit: `([^`]+)`.*/\1/p' "$repo_root/parity/UPSTREAM_COMMIT")
[[ -n "$pinned_commit" ]] || die "could not read the upstream commit from parity/UPSTREAM_COMMIT"
actual_commit=$(git -C "$upstream_dir" rev-parse HEAD 2>/dev/null) || die "upstream checkout is not a git repository"
[[ "$actual_commit" == "$pinned_commit" ]] || die "upstream checkout is $actual_commit, expected pinned commit $pinned_commit"
git -C "$upstream_dir" diff --quiet || die "pinned upstream checkout has tracked changes"
git -C "$upstream_dir" diff --cached --quiet || die "pinned upstream checkout has staged changes"

rust_cmd=(cargo "+$rust_toolchain")
"${rust_cmd[@]}" build -q -p pi-agent-core --features parity-runner --bin pi-agent-parity
rust_runner="$repo_root/target/debug/pi-agent-parity"
[[ -x "$rust_runner" ]] || die "Rust parity runner was not built: $rust_runner"

temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/pi-parity.XXXXXX")
trap 'rm -rf "$temp_dir"' EXIT

run_and_accept_status() {
	local label=$1
	local output=$2
	shift 2
	set +e
	"$@" >"$output"
	local status=$?
	set -e
	case "$status" in
		0|1) return 0 ;;
		*)
			echo "FAIL $label runner exited with status $status" >&2
			return 1
			;;
	esac
}

run_upstream() {
	local fixture=$1
	(
		cd "$upstream_dir"
		./node_modules/.bin/tsx ../agent-runner.mts "$fixture"
	)
}

canonicalize() {
	local source=$1
	local destination=$2
	jq -e -cS . "$source" >"$destination"
	[[ $(wc -l <"$destination" | tr -d ' ') == 1 ]] || {
		echo "runner emitted more than one JSON document: $source" >&2
		return 1
	}
}

compare_json() {
	local label=$1
	local left=$2
	local right=$3
	local diff_file=$4
	if diff -u --label "$label (expected)" --label "$label (actual)" "$left" "$right" >"$diff_file"; then
		return 0
	fi
	echo "FAIL $label" >&2
	sed -n '1,160p' "$diff_file" >&2
	return 1
}

fixtures=()
while IFS= read -r fixture; do
	fixtures+=("$fixture")
done < <(find "$fixture_dir" -type f -name '*.json' -print | sort)
[[ ${#fixtures[@]} -gt 0 ]] || die "no declarative fixtures found in $fixture_dir"

passed=0
failed=0
for fixture in "${fixtures[@]}"; do
	relative_fixture=${fixture#"$fixture_dir"/}
	name=${relative_fixture%.json}
	# Keep temporary paths flat even when a fixture uses the format's supported
	# slash-separated identifier or is stored in a nested fixture directory.
	temp_stem=${name//\//__}
	expected="$expected_dir/$relative_fixture"
	if [[ ! -f "$expected" ]]; then
		echo "FAIL $name: missing expected result $expected" >&2
		failed=$((failed + 1))
		continue
	fi

	upstream_raw="$temp_dir/$temp_stem.upstream.json"
	rust_raw="$temp_dir/$temp_stem.rust.json"
	expected_canonical="$temp_dir/$temp_stem.expected.canonical.json"
	upstream_canonical="$temp_dir/$temp_stem.upstream.canonical.json"
	rust_canonical="$temp_dir/$temp_stem.rust.canonical.json"

	fixture_failed=0
	run_and_accept_status "$name/upstream" "$upstream_raw" run_upstream "$fixture" || fixture_failed=1
	run_and_accept_status "$name/rust" "$rust_raw" "$rust_runner" "$fixture" || fixture_failed=1
	canonicalize "$expected" "$expected_canonical" || fixture_failed=1
	canonicalize "$upstream_raw" "$upstream_canonical" || fixture_failed=1
	canonicalize "$rust_raw" "$rust_canonical" || fixture_failed=1

	if [[ "$fixture_failed" == 0 ]]; then
		compare_json "$name/upstream-vs-expected" "$expected_canonical" "$upstream_canonical" "$temp_dir/$temp_stem.upstream.diff" || fixture_failed=1
		compare_json "$name/rust-vs-expected" "$expected_canonical" "$rust_canonical" "$temp_dir/$temp_stem.rust.diff" || fixture_failed=1
		compare_json "$name/upstream-vs-rust" "$upstream_canonical" "$rust_canonical" "$temp_dir/$temp_stem.adapters.diff" || fixture_failed=1
	fi

	if [[ "$fixture_failed" == 0 ]]; then
		passed=$((passed + 1))
		echo "ok $name"
	else
		failed=$((failed + 1))
	fi
done

echo "parity corpus: $passed passed, $failed failed"
if [[ "$failed" != 0 ]]; then
	exit 1
fi
