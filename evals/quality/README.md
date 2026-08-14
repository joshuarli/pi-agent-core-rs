# Quality evaluation suite

This directory implements the quality-suite contract in
[`evals/README.md`](../README.md). It has two intentionally different tiers:

- `fast` is provider-free, strict deterministic parity against the pinned
  upstream `pi-agent-core` source. It runs ten current-upstream scenarios for
  validation/recovery, stream errors, tool ordering, and cancellation.
- `coding` is an explicit, Vault-backed ecological check for exactly three
  pinned `pi-bench` Express tasks. It compares upstream Pi's headless SDK
  session with the Rust default coding profile using test results, never an LLM
  judge.

The suite does not invoke a host `pi` executable or test Pi TUI/session UI
behavior. Rust uses the repository's `nightly-2026-07-24`; there is no stable
fallback and no Tokio runtime.

## Routine deterministic gate

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m evals.quality fast --out /tmp/pi-quality-fast
PYTHONDONTWRITEBYTECODE=1 python3 -m evals.quality replay /tmp/pi-quality-fast/unknown-tool/report.json
```

`fast` writes the input manifest and lowered fixture, both raw adapter results,
both canonical traces, a JSON/text first-divergence report, metrics, request
fingerprints, and per-adapter process peak RSS. `replay` is offline: it
rechecks every stored normalized request fingerprint and rebuilds the stored
trace report without launching an adapter, reading a credential, or using the
network.

## Resource diagnostics

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m evals.quality resources --out /tmp/pi-quality-resources.json
```

Every core adapter run is wrapped with the platform `time` utility when it can
report peak RSS. That gives a paired upstream/Rust process-memory diagnostic on
the same host and fixture. The Rust-only `resources` probe uses the
`rustybench::AllocProfiler` global allocator from
[`quality_memory.rs`](../../crates/pi-agent-core/benches/quality_memory.rs) for
one provider-free text turn. It reports allocation count/bytes and peak live
allocation bytes. Node/Pi has no equivalent allocator instrumentation here, so
allocation counts are **not** compared across languages; only RSS is paired.
Neither timing nor resource values gate semantic parity because they vary by
machine and instrumentation changes the Rust path.

## Live coding gate

First populate the exact pinned bare-repository cache. This is deliberately a
separate maintenance step which may use the network; a scoring run never
fetches a repository while a model attempt is active.

```sh
python3 -m evals.quality prepare-cache --cache-root /tmp/pi-quality-cache
```

Then run the ordinary fast regression validator. The adapters themselves are
launched through `vault OPENROUTER_API_KEY -- bash …`; the evaluator never
reads or forwards the key.

```sh
python3 -m evals.quality coding --allow-provider \
  --model deepseek/deepseek-v4-flash-0731 \
  --cache-root /tmp/pi-quality-cache \
  --workspace-root /tmp/pi-quality-workspaces \
  --out /tmp/pi-quality-coding \
  --validator fast
```

Use `full` for the release/audit validator (`npm install` then `npm test`) plus
the deterministic core tier:

```sh
python3 -m evals.quality full --allow-provider \
  --model deepseek/deepseek-v4-flash-0731 \
  --cache-root /tmp/pi-quality-cache \
  --workspace-root /tmp/pi-quality-workspaces \
  --out /tmp/pi-quality-full
```

Each attempt starts from a detached exact Express commit cloned without local
hardlinks, records the resulting source patch and its hash, runs the selected
validator after settlement, then removes only that evaluator-created
worktree. NPM's content cache may be shared by lockfile hash; `node_modules`
is never reused. The three fast validator manifests retain baseline/fixed
evidence and the original full audit command.

## Hermetic upstream coding-profile adapter

[`../upstream-live-adapter.mts`](../upstream-live-adapter.mts) uses the pinned
source checkout's programmatic `Agent` together with its own
`buildSystemPrompt`, `createCodingToolDefinitions`, and `createCodingTools`
factories. That supplies Pi's normal built-in coding prompt and `read`,
`bash`, `edit`, `write` tool surface without TUI, sessions, extensions, skills,
prompts, themes, context files, package discovery, or user settings. After
copying the Vault key into a model callback, it clears inherited environment
variables so model-produced bash commands cannot read that key or ambient user
settings.

The profile lifts only Pi-installation documentation paths to the captured
profile's fixed virtual paths. That makes both baselines receive the same
prompt bytes while retaining the pinned source template, tool ordering,
snippets, guidelines, and supplied workspace unchanged.

The pinned shallow source intentionally omits a generated Bedrock catalog JSON
that its `createAgentSession` factory imports before it can honor explicit
in-memory options. Hydrating that catalog would be a hidden network-dependent
mutation of the upstream pin, so this adapter intentionally uses the focused
public profile factories instead. The default system prompt/tool bytes are
still produced by the same pinned coding-agent source.

The result is a headless SDK oracle, not a claim that this repository ports
Pi's broader coding-agent product or TUI.

“Hermetic” here means no Pi resource/session/settings discovery and no secret
inheritance. It does **not** make Pi's exact default bash/read/edit/write tools
filesystem-safe: their normal authority is part of the upstream surface being
compared. Run the live tier only in a disposable sandbox/VM with a dedicated
cache/workspace location, never against sensitive host files.
