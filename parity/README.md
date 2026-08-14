# Parity harness

The parity harness is a small, deterministic contract between two runners:

```text
declarative fixture ──┬── upstream runner ──┐
                      └── Rust runner ──────┴── canonical result ── comparator
```

The runners execute the same fixture and emit the same JSON result shape. The comparator only
compares canonical results; it never starts a Pi CLI, contacts a model provider, or interprets
runner-specific output. The harness itself has no runtime dependency. A future runner may be
implemented in any language, provided it obeys the boundaries in [`runners.md`](runners.md).

## Layout

```text
parity/
├── fixtures/
│   ├── declarative/       # deterministic, provider-free test inputs
│   ├── expected/          # optional checked-in canonical results
│   ├── normalized/        # generated runner results (not source fixtures)
│   └── recorded/          # immutable captures of external/provider behavior
├── rust/                  # Rust runner documentation/auxiliary boundary
├── compare/               # canonical-result comparison boundary (scaffold)
├── fixture-format.md      # declarative input contract
├── normalization.md       # canonicalization and redaction rules
└── runners.md             # runner I/O and scope boundaries
```

`upstream/` is the separately pinned upstream SDK runner. The checked-in V0 corpus currently
covers text, tool success/error, reverse parallel completion ordering, partial updates, before/after
tool policy, queue modes, and continuation. Likewise, files under `fixtures/recorded/` are
evidence, not live tests: a recorded provider response must remain usable when the provider is
unavailable.

## Workflow

1. Add a provider-free JSON fixture under `fixtures/declarative/`.
2. If the case has a complete expected output, add its canonical result under `fixtures/expected/`.
3. Run each adapter with the fixture and write one canonical result under `fixtures/normalized/`.
4. Compare the two canonical results. A mismatch is a contract failure or an intentionally
   documented fixture change; it is not resolved by weakening normalization.

The samples [`single-turn-text.json`](fixtures/declarative/single-turn-text.json) and
[`tool-continue.json`](fixtures/declarative/tool-continue.json) execute through both in-process
SDK adapters without a model, network, or Pi CLI. The latter pins assistant tool-call settlement,
host execution, source-ordered tool-result insertion, and model continuation; see
[`runners.md`](runners.md) for exact commands.

[`tool-error-continue.json`](fixtures/declarative/tool-error-continue.json) additionally proves
that a tool execution failure is transcript data—not a failed agent run—and that the subsequent
model turn can recover.
