# Rust contract fixtures

The parity directory is a deterministic, provider-free contract corpus for the Rust agent
kernel. Each declarative fixture is run by the checked-in Rust adapter and compared with its
checked-in canonical result. There is no upstream checkout, differential runner, or live provider
in this verification path.

## Layout

    parity/
    ├── fixtures/
    │   ├── declarative/       # deterministic, provider-free test inputs
    │   ├── expected/          # checked-in canonical Rust results
    │   ├── normalized/        # generated results, not source fixtures
    │   └── recorded/          # immutable external/provider captures
    ├── rust/                  # Rust runner documentation/auxiliary boundary
    ├── compare/               # reserved canonical-result comparison boundary
    ├── fixture-format.md      # declarative input contract
    ├── normalization.md       # canonicalization and redaction rules
    ├── runners.md             # Rust runner I/O and scope boundaries
    └── run-rust.sh            # full Rust fixture check

The corpus covers text turns, cancellation and reuse, tool success/error, parallel completion
ordering, partial updates, hooks, queues, continuation, and the default profile contract.
Recorded provider responses remain immutable evidence and are checked only by their explicit
provider-free replay adapter.

## Workflow

1. Add a provider-free JSON fixture under fixtures/declarative/.
2. Add its canonical result under fixtures/expected/.
3. Run ./parity/run-rust.sh.
4. Treat a mismatch as a contract or fixture change; do not weaken normalization to hide it.

The runner uses the pinned nightly toolchain and jq for canonical JSON comparison. It never
starts a Pi CLI, installs packages, contacts a provider, reads ambient configuration, or mutates
the checked-in fixture tree. Fixture outcomes that represent model/tool errors or cancellation
are valid data; malformed fixtures and runner failures are verification failures.

The default profile has checked-in captured prompt/definition data under profile/. Rust tests
validate that capture and the concrete explicit capability boundary. See profile/README.md.
