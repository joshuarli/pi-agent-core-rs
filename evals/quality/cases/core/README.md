# Deterministic core quality cases

Each directory in this folder is one provider-free core case. Its `manifest.json`
is an input to the quality-suite adapters; it is deliberately separate from the
older `parity/fixtures/declarative/` corpus because these cases include provider
stream failures, partial tool-call streams, and structured cancellation barriers
that are not part of the V0 parity-fixture format.

The manifests use the following contract:

```text
format_version: 1
kind: quality_core_case
id: stable case id
description: scenario summary
source: { repository, issue?, upstream_commit_tested, role, historical_behavior_is_oracle }
scope: core
parity: strict | informational
oracle: current_pinned_upstream_capture
setup: exact system prompt, fixture model, and registered tool schemas
actions: deterministic prompts and control points
model_script: ordered requests and exact provider stream chunks
host: deterministic tool responses and barriers
observations: fields and invariants to record, not historical expected output
```

`source.issue` records issue inspiration only. Issue reports are never an oracle:
the adapters must capture behavior from the pinned upstream commit named in each
manifest and compare the Rust candidate with that capture. In particular, these
manifests intentionally do not contain a claimed historical stop reason, event
sequence, or “fixed” outcome.

Model chunks use `type` values such as `text_delta`, `tool_call_start`,
`tool_call_delta`, `tool_call_end`, `done`, and `stream_error`. Tool schedules use
named barriers rather than wall-clock sleeps. This keeps the same script replayable
by both adapters while still making start, completion, ordering, and cancellation
boundaries observable.

`orphan-tool-result/manifest.json` is retained as an excluded probe. The public
agent API does not permit constructing an initial orphan `tool_result` message, so
it has no executable script and cannot gate core parity until a supported public
construction exists.
