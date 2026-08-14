# Verification and completed V0 evidence

V0 is complete. Its implementation is the pure Rust agent kernel, pinned
default coding profile, deterministic in-process Pi subset parity corpus,
structured cancellation, optional trace observer, and comparative coding
evaluation. The remaining programmable-policy expansion belongs to
[`V1.md`](../V1.md).

## Required local checks

Run the repository's pinned nightly toolchain:

```bash
cargo +nightly-2026-07-24 test --workspace
cargo +nightly-2026-07-24 fmt --check
git diff --check
```

For a profile or upstream-pin change, also run the documented fixture and
source-profile gates in [`parity/README.md`](../parity/README.md) and
[`parity/profile/README.md`](../parity/profile/README.md). A new supported
behavior requires a ledger row and deterministic fixture before implementation.

## Completion evidence

- The parity corpus compares the pinned upstream SDK, Rust runner, and checked
  goldens in process. It covers streams, tool updates/errors, hooks, queues,
  observer settlement, cancellation, reuse, default profile bytes and tool
  behavior.
- The core has no Pi CLI/runtime dependency, no ambient configuration/session
  behavior, no Tokio, and no unsafe Rust. Providers and world side effects are
  explicit host ports.
- Deterministic hardening covers lifecycle balance and cleanup, completion
  ordering, profile composition, concurrent run claims, workspace isolation,
  non-blocking observer overflow, and one thousand isolated agents.
- The final comparative coding gate ran on 2026-08-14 with the explicit,
  Vault-injected DeepSeek manifest. Both upstream and Rust baselines passed two
  of two light standard-library Python tasks and their `READY` controls, with
  no attempt timeout. The provider-specific report is intentionally ignored;
  the controller contract lives in [`evals/README.md`](../evals/README.md).

## Runebench integration evidence

Runebench is an embedding, not part of the transport-free core. Its hard
cutover uses the Rust host, pinned default profile, explicit OpenRouter
adapter, capability-scoped rs-agent MCP bridge, and LuauJIT policy.

On 2026-08-14, the Vault-backed `tasks/woodcutting-xp-5m` acceptance completed
cleanly with `completed=1`, `errored=0`, peak **162 XP/min**, **63,750 XP**,
and Woodcutting level **59**. The host owns a 390-second structured deadline,
leaving cleanup margin before Harbor's 420-second task limit; foreground shell,
provider, and MCP children are cancellation-aware while intentionally detached
world workers are not reaped by the agent host.

Re-run this acceptance after changing the Runebench host, profile binding,
world policy, or process-cancellation boundary. It is not a substitute for the
deterministic core parity suite.
