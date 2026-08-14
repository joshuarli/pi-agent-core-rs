# pi-agent-core-rs

This repository is a headless Rust port of a pinned, useful subset of Pi's agent runtime. It
never launches the Pi CLI. The core is driven by the embedding application's Smol executor and
uses explicit model, tool, policy, and queue capabilities rather than ambient project/session
state.

The current V0 slice implements lifecycle settlement, text/tool turns, schema validation,
parallel and sequential tool scheduling, partial updates, before/after tool hooks, steering and
follow-up queues, `continue`, cancellation, reset, and a deterministic in-process parity harness.
The remaining V0 corpus and executable default coding-tool capability layer are tracked in
[`plan.md`](plan.md); V0 is not declared complete until those gates and the final coding evaluation
are satisfied.

## Crate dependency direction

```text
pi-agent-core  →  pi-agent-protocol
pi-agent-trace →  pi-agent-protocol
pi-agent-luau  →  pi-agent-core  (planned V1 crate; not a V0 workspace member)
```

Arrows mean "depends on". The trace crate consumes immutable protocol events,
and the Luau crate is downstream of the core; neither is required by the layers
above it.

`pi-agent-protocol` is the stable data/event layer. `pi-agent-core` depends on
those types and owns the agent loop. `pi-agent-trace` is optional and downstream
of the observable protocol contract; the core must not require tracing.
`pi-agent-luau` is a planned optional policy layer downstream of the core and
must not place scripting types in core APIs. No optional layer is a dependency
of the protocol or core layers.

The pinned `rust-toolchain.toml` nightly toolchain is authoritative. Core APIs
remain executor-agnostic: Smol drives the parity runner and host integration,
while Tokio is prohibited throughout the workspace. The protocol uses
Miniserde for its JSON text codec; no Serde type is exposed by the workspace.
