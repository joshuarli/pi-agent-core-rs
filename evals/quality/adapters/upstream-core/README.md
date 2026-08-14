# `upstream-core` quality adapter

This executable is a thin JSON process boundary around the existing pinned
[`parity/upstream/agent-runner.mts`](../../../../parity/upstream/agent-runner.mts).
It does not invoke Pi's CLI or TUI, install packages, contact a provider, or
discover project resources.

`adapter.py` reads one JSON request from stdin:

```json
{"protocol":"pi-agent-quality-adapter/v0","operation":"run","fixture":"parity/fixtures/declarative/single-turn-text.json"}
```

`fixture` is required and is resolved relative to the repository root when it
is not absolute. Both `declarative_parity_fixture` inputs and the quality
suite's `quality_core_case` inputs are accepted. The latter are translated in
memory from `model_script.emit` records to the existing runner's `chunks`
vocabulary (`tool_call_delta` must contain a complete JSON object). Request
role expectations are retained in response metadata but are not asserted by
the older runner. Barrier and scheduler controls are rejected with a precise
error because silently dropping them would change the case's meaning.

The response is one JSON object with `metadata` describing the pinned upstream
source and `result` containing the unchanged canonical parity result emitted
by the existing runner. Runner status `0` is success; status `1` is a valid
error/cancellation result; status `2` means the request or runner contract was
invalid.

The adapter checks the checked-out source is detached at commit
`9d2ec7ffabe927bfad2214c1cee25b6632a78dcf` and has no tracked changes before
running. (The canonical pin is also recorded in `parity/UPSTREAM_COMMIT`.)

Example:

```sh
printf '%s\n' '{"fixture":"parity/fixtures/declarative/single-turn-text.json"}' |
  evals/quality/adapters/upstream-core/adapter.py
```
