# Quality evaluation

The quality suite adds a small regression layer above the existing declarative
parity corpus. Its aim is information per run: exact core state-machine
behavior first, then three realistic coding fixes. It is not a general agent
benchmark.

The deterministic tier runs the pinned upstream `pi-agent-core` source and the
Rust candidate on the same scripted model/tool fixture. It records normalized
requests, tool lifecycle/order, final state, request semantic hashes, first
trace divergence, and paired process peak RSS. Ten strict cases currently
cover malformed/unknown tools, error recovery, stream settlement, empty
tool-use, partial calls, ordering, and single/parallel cancellation.

The live tier is manual and provider-opt-in. It uses a clean pinned Express
worktree for `express-4744-easy`, `express-3936-medium`, and
`express-4205-hard`, then applies a deterministic fast or full test validator.
Upstream runs via the headless pinned Pi SDK; Rust uses the default coding
profile and Smol-owned evaluation adapter. A model answer alone never passes a
case.

See [`evals/quality/README.md`](../evals/quality/README.md) for commands,
cache setup, artifacts, replay, and memory-measurement interpretation. The
durable scope, tiers, and evaluation contract are in
[`evals/README.md`](../evals/README.md).
