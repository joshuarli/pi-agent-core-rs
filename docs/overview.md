# pi-agent-core-rs

`pi-agent-core-rs` is a small, headless Rust implementation of a pinned,
useful subset of Pi's agent runtime. It is an execution microkernel, not a
port of Pi's interactive application.

The core reduces this explicit loop:

```text
model stream -> assistant response -> tool execution -> tool results -> next turn
```

It is designed for disposable agents in CI sandboxes, VM worlds, RL
environments, and swarms. Pi is a behavioral oracle through an in-process,
pinned Rust fixture harness; this project never launches the Pi CLI for parity
or runtime behavior.

## Design commitments

- Rust owns state transitions, scheduling, cancellation, event settlement,
  tracing boundaries, and resource ownership.
- The embedding owns the executor, model transport, workspace, tools, policy,
  credentials, and side effects.
- The core has no ambient configuration, session storage, `$HOME` discovery,
  package/plugin discovery, provider implementation, or background runtime.
- The checked-in nightly in `rust-toolchain.toml` is authoritative. Tokio is
  prohibited; applications commonly drive the core with Smol.
- `pi-agent-protocol` uses Miniserde at the JSON boundary. Serde values are not
  part of the public workspace contract.
- The pinned Pi default coding profile is batteries-included but fully
  replaceable. Its workspace and all filesystem/process authority are explicit.
- `pi-agent-luau` is optional. A pure Rust agent neither links nor constructs a
  scripting VM.

## Crate direction

```text
pi-agent-protocol <- pi-agent-core <- pi-agent-luau
                   <- pi-agent-trace
```

Arrows point from a dependency toward its dependent. The protocol provides
stable data and event shapes; the core owns the loop; trace and Luau are
downstream optional layers.

## Documentation map

- [Quickstart](quickstart.md) — build and run a first caller-owned agent.
- [Scope and compatibility boundary](scope.md) — selected Pi subset and hard
  exclusions.
- [Architecture](architecture.md) — ownership, ports, state machine, and
  scheduling.
- [Core terminology](core-terminology.md) — upstream Pi vocabulary, Rust
  names, aliases, and intentional differences for resync work.
- [Glossary](glossary.md) — repository-wide vocabulary for state, boundaries,
  lifecycle, tools, policy, and verification layers.
- [Runtime semantics](semantics.md) — observable lifecycle and cancellation
  contracts.
- [Fixture format](../parity/fixture-format.md) and [Rust parity guide](../parity/README.md)
  — exact behavioral fixtures and verification evidence.
- [Default coding profile](default-coding-profile.md) — captured prompt, tools,
  operation adapters, and update procedure.
- [Verification](verification.md) — required checks and completed V0 evidence.
- [Quality evaluation](quality-evaluation.md) — deterministic trace parity,
  three pinned Express tasks, replay artifacts, and resource diagnostics.
- [Tracing](trace.md) — optional trajectory observer boundary.
- [Terminal host](tui.md) — `pi-agent` ownership boundaries, interaction
  contract, and post-V0 direction.
- [Writing Luau extensions](luau-extensions.md) — closed bundles, capability
  bindings, coroutine-backed tools, limits, and review rules.
- [V1](../V1.md) — delivered optional Luau extension foundation and intentional
  boundaries.

The parity harness and fixtures have their own [guide](../parity/README.md).
The end-to-end coding evaluation controller is documented in
[`evals/README.md`](../evals/README.md).
