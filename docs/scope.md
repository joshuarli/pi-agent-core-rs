# Milestone 0 scope

This document freezes what the project means by “Pi parity” before the Rust runtime is
implemented. `plan.md` is the authority for this milestone. `V1.md` describes the optional Luau
policy plane and does not widen V0. The pinned upstream checkout and its fixture runner are the
executable specification; prose in this document is a boundary, not a substitute for a fixture.

## Product boundary

V0 is a pure Rust, headless agent execution microkernel plus an explicit default coding profile.
The useful execution path is:

```text
caller-owned model stream
        -> assistant response
        -> tool preparation and execution
        -> tool-result messages
        -> next model turn
```

The embedding owns the executor, model transport, tool authority, workspace, cancellation owner,
and optional event/trace sinks. The core owns state transitions, context history, provider
request construction, tool scheduling and result ordering, queue semantics, event settlement,
failure classification, and cleanup.

Pi is the behavioral oracle, not a source-code port. A behavior enters the V0 contract only when
it has an upstream path and symbol, a deterministic in-process fixture, and a normalized result
comparison. A source file being present in the Pi repository is not permission to implement it.

## V0 contract

The following are deliberately in scope:

| Area | Contract to establish in Milestone 0 | V0 consequence |
| --- | --- | --- |
| Agent state | System prompt, model descriptor, thinking level, messages, tools, stream snapshot, pending tool IDs, last error | Stable Rust state snapshot; no borrowed mutable state |
| Runs | `prompt`, `continue`, exactly one active run, explicit run settlement | A second direct run is rejected while active; steering/follow-up are explicit queues |
| Model boundary | Small provider stream/request protocol independent of `pi-ai` | No provider implementation or provider dependency in the core |
| Messages | User, assistant, tool-result, text/image/tool-call content, usage and stop reason as needed for execution | Explicit tagged protocol types with Miniserde JSON codecs |
| Tools | JSON Schema, argument validation, preparation, sequential/parallel execution, updates, errors, termination hints | Tool authority is supplied by the caller and every default tool is replaceable |
| Events | Agent/turn/message/tool lifecycle and awaited observer settlement | Event order and terminal grammar are fixture-tested |
| Cancellation | Model, preparation, execution, hooks, queue waits, and between-turn cancellation | Terminal cleanup leaves the same agent reusable |
| Hooks and queues | The selected `beforeToolCall`, `afterToolCall`, context, stop, next-turn, steering and follow-up semantics | Rust-only in V0; Luau adapts later |
| Default profile | Pinned prompt template, active tool order, schemas, snippets, guidelines and standard-tool behavior | `PiDefaultCodingProfile` is explicit and sterile profiles remain possible |
| Trace boundary | Optional immutable event consumer, separate from state | The lean recorder is a later V0 milestone; no session tree |

The default profile is a capability bundle, not ambient authority. Its constructor takes an
explicit workspace and operation adapters. It does not discover cwd, `$HOME`, `.pi`, settings,
skills, project instructions, sessions, or credentials. Callers can omit it, replace or remove
any standard tool, wrap an operation with policy, or provide a complete application profile.

## V0 exclusions

These are rejected as V0 implementation targets even when an upstream package exports them:

| Excluded surface | Reason |
| --- | --- |
| `pi-coding-agent` session/UI/application behavior | V0 is a headless loop, not the interactive coding agent |
| `pi-tui`, terminal rendering, commands, keybindings, themes, approval or permission UI | No terminal or human approval authority belongs in the kernel |
| Session manager/repository/storage, session JSONL, `/tree`, `/resume`, branches, labels, names | No ambient persistence or session navigation |
| Resource discovery, prompt templates, skills, extensions, package management, MCP | These are application/resource systems, not the selected execution kernel |
| `AGENTS.md`, `.pi`, `~/.pi`, cwd/home/settings/config discovery | Authority must cross an explicit host boundary |
| Pi provider catalog/authentication/model discovery and a port of `pi-ai` | Provider mechanics stay behind the small model-stream trait |
| OpenTelemetry, Sentry, compaction policy and provider-specific retry/catalog policy | Not needed for the V0 state machine |
| Tokio, Node, TypeScript, `napi-rs`, JavaScript callback bridges, or a scripting runtime | Core is executor-owned pure Rust |
| Swarm framework, world forking, IPC, C ABI, WASM/component, Python bindings | Post-V1 exploration only when a concrete use case exists |

The default profile may use local filesystem/process implementations, but those implementations
are explicit profile adapters. The core never learns filesystem, shell, network, VM, or world
semantics.

## V0 versus V1

| Concern | V0 | V1 (`V1.md`) |
| --- | --- | --- |
| Mechanism | Rust agent FSM, context, stream handling, scheduling, queues, cancellation, settlement | Unchanged; Rust remains authoritative |
| Policy | Statically supplied Rust hooks/tools and caller-owned adapters | Optional hermetic Luau policy attached downstream |
| Runtime cost | No VM; pure Rust agent has no scripting cost | `pi-agent-luau` with `mlua`/Luau only when selected |
| Capabilities | Rust traits and explicit profile adapters | Host-controlled `@agent`, `@world`, `@trace`, `@task`, `@json`, `@time` modules |
| Scheduling | Caller-owned Smol executor; no core-owned runtime/tasks | Luau coroutines yield to Rust futures on the same caller-owned executor |
| Isolation | Rust API and explicit host capability boundary | Capability manifest, closed module resolver, VM/resource limits |
| Tracing | Optional linear typed event recorder | Luau may annotate, never alter replay semantics |
| Interfaces | No external language binding | No general external binding; future IPC/WASM/etc. remain post-V1 |

Luau cannot redefine V0 events, ordering, state transitions, cancellation, usage, failure
classification, resource ownership, or run lifecycle. If a feature requires a VM, module
resolution, policy bundle, world capability, or script ABI, it is deferred to V1. If it requires
interactive sessions or ambient discovery, it remains rejected.

## Decision rules

When upstream behavior is encountered, classify it in `docs/parity-ledger.md` using exactly one of
these statuses:

* `supported`: part of the V0 target and covered by a fixture row;
* `deferred-to-v1`: explicitly a Luau policy-plane concern from `V1.md`;
* `rejected`: outside the headless kernel/profile boundary;
* `investigating`: the selected public behavior or its settlement detail is not yet pinned by a
  deterministic fixture.

`investigating` is a temporary specification status, not permission to implement a guess. Every
such row has a fixture ID and a concrete exit condition. A fixture may normalize timestamps,
generated UUIDs, and durations only; it may not normalize semantic ordering, queue behavior,
message content, tool results, state cleanup, or terminal outcomes.

## Milestone 0 exit checklist

Milestone 0 is complete only when all of the following have evidence:

- `parity/UPSTREAM_COMMIT` records repository URL, exact commit, package version, Node version,
  lockfile hash, and runner command.
- `docs/pi-sdk-subset.md` names every selected public method, type, event, and profile symbol.
- `docs/parity-ledger.md` has one row for every selected or excluded upstream semantic, with a
  status from the four-value vocabulary and a fixture reference.
- `docs/semantics.md` has a fixture-resolved active-run contract, event grammar, cancellation
  checkpoints, observer settlement rule, queue drain points, and terminal-state invariant.
- `docs/default-coding-profile.md` records active order, canonical schemas, prompt snippets,
  guidelines, generated prompt bytes/hash, operation adapters, and behavior fixtures.
- The deterministic upstream fixtures run in-process against the SDK; they do not invoke Pi CLI,
  `pi-coding-agent`, or a real provider.
- No Rust loop implementation is needed for this milestone; no dependency is added to produce the
  specification.

## Exact new-ledger entry template

Copy this block when adding a row to any Milestone 0 ledger. Keep the status vocabulary exact.

```text
ID: PL-<domain>-<number>
Status: supported | deferred-to-v1 | rejected | investigating
Upstream path: packages/<package>/<path>:<line>
Upstream symbol/export: <name>
Observable behavior: <one sentence; include ordering or settlement>
V0/V1 rationale: <why this boundary is correct>
Fixture: parity/fixtures/<scenario>/<runner>.json
Expected evidence: <canonical field or assertion>
Normalization: none | timestamp | generated-id | duration (explain)
Open question / exit criterion: <required only for investigating>
```
