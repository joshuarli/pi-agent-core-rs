# Deterministic core quality cases

Each directory in this folder is one provider-free core case. Its `manifest.json`
is an input to the quality-suite lowering boundary. Cases include provider
stream failures, partial tool-call streams, and structured cancellation
barriers that are not part of the closed fixture format.

The manifests use the following contract:

```text
format_version: 1
kind: quality_core_case
id: stable case id
description: scenario summary
source: { repository, issue?, role, historical_behavior_is_oracle }
scope: core
gate: strict | informational
contract: rust_fixture_runner
setup: exact system prompt, fixture model, and registered tool schemas
actions: deterministic prompts and control points
model_script: ordered requests and exact provider stream chunks
host: deterministic tool responses and barriers
observations: fields and invariants to record, not historical expected output
```

`source.issue` records historical scenario inspiration only. Issue reports are
never an expected-output oracle. The `contract` names the Rust fixture runner
that executes the lowered case; the manifest does not claim an external
implementation result.

Model chunks use `type` values such as `text_delta`, `tool_call_start`,
`tool_call_delta`, `tool_call_end`, `done`, and `stream_error`. Tool schedules use
named barriers rather than wall-clock sleeps. This keeps the script replayable
by the Rust fixture runner while still making start, completion, ordering, and
cancellation boundaries observable.

`orphan-tool-result/manifest.json` is retained as an excluded probe. The public
agent API does not permit constructing an initial orphan `tool_result` message, so
it has no executable script and cannot enter the strict fixture gate until a
supported public construction exists.
