# `rust-core` quality adapter

This executable is a thin JSON process boundary around the existing
[`pi-agent-parity`](../../../../crates/pi-agent-core/src/bin/pi-agent-parity.rs)
binary. It uses the repository's pinned `nightly-2026-07-24` toolchain and
does not instantiate a TUI, provider, policy runtime, or ambient workspace
capability.

`adapter.py` reads one JSON request from stdin:

```json
{"protocol":"pi-agent-quality-adapter/v0","operation":"run","fixture":"parity/fixtures/declarative/single-turn-text.json"}
```

`fixture` is required and is resolved relative to the repository root when it
is not absolute. The adapter accepts both `declarative_parity_fixture` inputs
and the quality suite's `quality_core_case` inputs. For the latter it imports
only the sibling adapter's pure translation helper to convert
`model_script.emit` to the existing Rust runner's `chunks` vocabulary; it does
not invoke upstream. Request role expectations are retained in metadata, and
barrier/scheduler controls are rejected rather than silently ignored because
the existing Rust parity binary has no barrier scheduler.

The response is one JSON object with `metadata` describing the Rust runner and
`result` containing the unchanged canonical parity result emitted by the
binary. Runner status `0` is success; status `1` is a valid error/cancellation
result; status `2` means the request or runner contract was invalid.

Example:

```sh
printf '%s\n' '{"fixture":"parity/fixtures/declarative/single-turn-text.json"}' |
  evals/quality/adapters/rust-core/adapter.py
```
