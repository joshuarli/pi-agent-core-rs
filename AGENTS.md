# pi-agent-core-rs

This repository is a headless Rust agent execution microkernel with a pinned
Pi default coding profile. It is not Pi's CLI, session system, or an ambient
project configuration layer; it also contains a small, repository-owned
terminal host in `crates/pi-agent-tui`. Rust owns mechanism; optional Luau owns
policy; embeddings own model transport and world capabilities.

Start with [docs/overview.md](docs/overview.md). The main routes are:

- [Quickstart](docs/quickstart.md) for an application integration.
- [Scope](docs/scope.md), [architecture](docs/architecture.md), and
  [semantics](docs/semantics.md) for the durable core contract.
- [Default coding profile](docs/default-coding-profile.md) and
  [provider adapters](docs/provider-adapters.md) for optional runtime layers.
- [Tracing](docs/trace.md) and [Luau extensions](docs/luau-extensions.md) for
  optional observability and policy layers.
- [Terminal host](docs/tui.md) for the repository-owned `pi-agent` TUI.
- [Quality evaluation](docs/quality-evaluation.md) and
  [verification](docs/verification.md) for compatibility and quality evidence.
- [fixture format](parity/fixture-format.md) and [parity guide](parity/README.md)
  for fixture-based compatibility work.
- [`V1.md`](V1.md) for remaining programmable-policy work.

## Working contract

- Establish behavior, boundaries, callers, tests, and documentation before
  changing a contract. Make the smallest reversible assumption.
- Use precise types and explicit capability boundaries. Do not add a dependency
  without user approval or hide a fallback that changes semantics.
- For bugs, add the smallest isolated failing regression test first, then fix
  the root cause and retain the test.
- Start with focused evidence, then broaden checks. Use the pinned nightly in
  `rust-toolchain.toml`; stable Rust and Tokio are not supported.
- Keep core executor- and provider-agnostic. No Pi CLI, sessions, ambient
  discovery, `$HOME` access, or world authority belongs in the core.
- Do not run pre-commit hooks or push. Preserve unrelated worktree changes.
