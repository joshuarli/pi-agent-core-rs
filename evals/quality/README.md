# Quality evaluation suite

This directory contains provider-free deterministic fixture checks and an
opt-in ecological coding check. The core gate executes the Rust fixture
runner; it does not require an upstream checkout, provider credentials, or a
recorded replay artifact.

## Deterministic core gate

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m evals.quality fast --out /tmp/pi-quality-fast
```

Each case manifest is lowered to the closed fixture vocabulary and run by
`crates/tea-core/src/bin/tea-fixtures.rs`. Artifacts retain the
manifest, fixture, Rust response, canonical trace, metrics, and process
diagnostics. Source fixtures live under
`crates/tea-core/fixtures/declarative/`.

## Resource diagnostics

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m evals.quality resources --out /tmp/pi-quality-resources.json
```

The resource probe uses the Rust-only
`rustybench::AllocProfiler` benchmark from
`crates/tea-core/benches/quality_memory.rs`. Allocation and timing values
are diagnostic and do not gate fixture results.

## Live coding gate

The coding tier is an explicit provider-opt-in check for three pinned
`pi-bench` Express tasks. It runs the Rust coding adapter and the selected
validator from a fresh detached worktree; no upstream comparison or ambient
repository discovery is performed.

Populate the exact bare-repository cache first:

```sh
python3 -m evals.quality prepare-cache --cache-root /tmp/pi-quality-cache
```

Then provide an explicit model and env file:

```sh
python3 -m evals.quality coding --allow-provider \
  --model poolside/laguna-xs-2.1:free \
  --env-file .env \
  --cache-root /tmp/pi-quality-cache \
  --workspace-root /tmp/pi-quality-workspaces \
  --out /tmp/pi-quality-coding \
  --validator fast
```

The Rust profile is captured at
`crates/tea-core/profile/default-profile.json`. Worktrees are removed
after each attempt and provider credentials are sourced only at the final
adapter process boundary.
