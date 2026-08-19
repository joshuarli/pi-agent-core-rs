# Verification and completed V0 evidence

V0 is complete. Its implementation is the pure Rust agent kernel, pinned
default coding profile, deterministic in-process Pi subset parity corpus,
structured cancellation, optional trace observer, and comparative coding
evaluation. V1 adds an optional Luau extension foundation without changing the
core's provider-, executor-, or world-agnostic boundary.

## Required local checks

Run the repository's pinned nightly toolchain:

```bash
cargo +nightly-2026-07-24 test --workspace
cargo +nightly-2026-07-24 fmt --check
git diff --check
```

For a profile or contract change, also run the Rust fixture check in
[`parity/README.md`](../parity/README.md) and the profile tests described in
[`parity/profile/README.md`](../parity/profile/README.md). A new supported
behavior requires a deterministic fixture before implementation.

For the trace-first quality gate, run:

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -m evals.quality fast
PYTHONDONTWRITEBYTECODE=1 python3 -m evals.quality resources
```

The live three-Express-task check is manual and requires explicit provider
authorization. Its hermetic adapter, cache procedure, full validator, replay,
and resource interpretation are documented in
[`evals/quality/README.md`](../evals/quality/README.md).

For prompt-cacheability and compaction-prefix evidence, run the deterministic fixture described
in [`docs/cache-friendliness.md`](cache-friendliness.md). Its common-prefix values are a proxy;
provider-reported cache usage remains the authoritative hit/write measurement.

## Completion evidence

- The parity corpus runs the Rust runner against checked-in canonical goldens.
  It covers streams, tool updates/errors, hooks, queues, observer settlement,
  cancellation, reuse, default profile bytes and tool behavior.
- The core has no Pi CLI/runtime dependency, no ambient configuration/session
  behavior, no Tokio, and no unsafe Rust. Providers and world side effects are
  explicit host ports.
- Deterministic hardening covers lifecycle balance and cleanup, completion
  ordering, profile composition, concurrent run claims, workspace isolation,
  non-blocking observer overflow, and one thousand isolated agents.
- Automatic policy regression coverage (`tests/automatic_policy.rs`) pins
  threshold ordering, zero-usage checkpoint retention, overflow compact/retry,
  and transactional failure behavior. `tests/circuit_breaker.rs` pins fatal and
  repeated retryable capability failures, while `tests/tool_projection.rs`
  pins raw-versus-model-facing result curation. These cases are also the local
  compaction parity surface to compare against Pi's compaction split and
  overflow-recovery fixtures without importing Pi session storage semantics.
- The final coding gate ran on 2026-08-14 with the explicit, credential-injected
  provider manifest. The provider-specific report is intentionally ignored; the
  controller contract lives in [`evals/README.md`](../evals/README.md).

## V1 extension evidence

- `pi-agent-luau` has unit and integration coverage for deterministic bundle
  paths/hashes, closed relative imports and per-VM caches, typed capability
  manifests/gates, raw coroutine request validation, cancellation/drop of
  pending host futures, handler host-call limits, and policy-bundle loading.
- The adversarial suite verifies the absence of ambient OS/file/package/debug
  authority, immutable globals, source/memory/interrupt containment, loop and
  recursion termination, failure recovery, deterministic declarations, and
  two-policy isolation.
- `cargo +nightly-2026-07-24 run -p pi-agent-luau --example
  v1_luau_benchmark --release` records startup/teardown, hook, and 256-policy
  isolation costs without brittle timing thresholds.
- The exact end-user and host contracts are in
  [`docs/luau-extensions.md`](luau-extensions.md). The lower-level source
  modules are `bundle`, `bundle_runtime`, `capability`, `async_runtime`, and
  `tool_handler` in `pi-agent-luau`.

## Runebench integration evidence

Runebench is an embedding, not part of the transport-free core. Its hard
cutover uses the Rust host, pinned default profile, explicit OpenRouter
adapter, capability-scoped Rust `rs-agent` MCP client, and LuauJIT policy.

On 2026-08-14, the credential-injected `tasks/woodcutting-xp-5m` acceptance completed
cleanly with `completed=1`, `errored=0`, peak **228 XP/min**, **88,750 XP**,
and Woodcutting level **64**. The Rust MCP client loaded its API documentation
and the Luau policy loaded all five declared `rs-agent` tools. The trajectory
had 17 balanced tool starts/ends and one terminal `agent_end`; the only failed
tool result was the expected `Operation aborted` when the host's 390-second
deadline cancelled a still-running game loop. The host owns that structured
deadline, leaving cleanup margin before Harbor's 420-second task limit;
foreground shell, provider, and MCP children are cancellation-aware while
intentionally detached world workers are not reaped by the agent host.

Re-run this acceptance after changing the Runebench host, profile binding,
world policy, or process-cancellation boundary. It is not a substitute for the
deterministic core parity suite.
