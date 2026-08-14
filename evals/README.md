# Comparative coding evaluations

This directory is the final V0 quality gate. It is an end-to-end comparison of the pinned
upstream headless Pi SDK profile and `PiDefaultCodingProfile`; it is not another semantic parity
runner and it is never ordinary CI.

The scaffold is deliberately inert by default. The controller validates task contracts and can
print a deterministic paired-run plan without starting a model, opening a network connection, or
reading a credential. A live attempt requires an explicit `run --allow-provider` and a local
baseline manifest. Provider adapters are external commands supplied by the caller; the controller
does not discover Pi, models, credentials, workspaces, or capabilities.

## What came from `localswarm`

The useful shape is retained without importing its TypeScript SDK machinery:

- every attempt gets a fresh workspace;
- the task exposes the smallest explicit capability manifest it needs;
- workspace paths are checked before materialization;
- a controller-owned oracle decides success after the agent settles;
- `READY` is a no-tool control for separating model/server overhead from coding behavior;
- coding and READY concurrency waves are reported separately.

The Rust runtime and the pinned upstream runner remain the semantic oracle. This harness only asks
whether both profiles can complete the same small programming contract under the same externally
selected model settings.

The manifest performs one paired `READY` control and one lightweight coding task for both
baselines. `interval-merge-v0` asks the model to write a single standard-library `intervals.py`
file once; it does not run a model-driven test process. The controller's hidden oracle imports and
checks that file after settlement, so assistant final text remains non-authoritative. Its task cap
is 60 seconds; the Rust adapter separately limits each OpenRouter HTTP request to 30 seconds. The local
provider-specific JSON report is ignored by Git; [`../docs/v0-completion-audit.md`](../docs/v0-completion-audit.md)
records the redacted summary and the exact rerun command below.

`interval-merge-v0` declares the exact captured schemas of the active `read`, `bash`, `edit`, and
`write` profile tools. The controller test compares that declaration to `default-profile.json`,
which the pinned-source profile verifier in turn checks against the upstream factories.

## Files

```text
evals/
├── controller.py                 # stdlib-only validate/plan/run controller
├── mock_adapter.py               # provider-free adapter for controller smoke evidence
├── test_controller.py            # deterministic controller tests; no provider execution
├── tasks/
│   ├── ready-v0.json             # no-tool overhead control
│   └── interval-merge-v0.json    # small multi-file coding task
├── baselines.example.json         # shape of an explicit local adapter manifest
└── baselines.mock.json             # runnable provider-free controller smoke manifest
```

`baselines.deepseek-v0.json` is the checked-in opt-in live manifest. It binds both adapters to
`deepseek/deepseek-v4-flash-0731`, invokes each through `vault OPENROUTER_API_KEY -- …`, and
names neither a host Pi CLI nor a core provider dependency. `run-upstream-live.sh` imports pinned
TypeScript sources directly. `run-rust-live.sh` runs the eval-only `pi-agent-eval` binary with
the repository's pinned nightly and Smol feature; the Rust library remains transport-free.

Validate the checked-in contracts with the pinned host Python:

```bash
python3 evals/controller.py validate
python3 -m unittest evals.test_controller
```

The checked-in mock manifest exercises the complete controller boundary without a model, network,
or credential. It is harness evidence only: both baseline entries call the deterministic
`mock_adapter.py` stand-in, so its success does not establish upstream/Rust behavioral parity.

```bash
python3 evals/controller.py run \
  --baselines evals/baselines.mock.json \
  --allow-provider \
  --out /tmp/pi-eval-mock-report.json
```

Run the explicit DeepSeek gate (after validating it) with:

```bash
python3 evals/controller.py validate --baselines evals/baselines.deepseek-v0.json
python3 evals/controller.py run \
  --baselines evals/baselines.deepseek-v0.json \
  --allow-provider \
  --out evals/results/deepseek-v0.json
```

The upstream adapter removes provider credentials from the default bash tool's child environment.
The Rust profile's default shell environment is already explicit and empty. Both therefore retain
the pinned tool schemas/prompts without giving model-produced commands access to the injected key.

Print the paired evaluation matrix without running anything:

```bash
python3 evals/controller.py plan --baselines evals/baselines.example.json
```

`baselines.example.json` contains placeholders and cannot run. For a real evaluation, copy it to
an ignored local file and replace both commands with explicit adapters. Every baseline must declare
`adapter.protocol` as `pi-coding-eval-adapter/v0` and `adapter.result_schema` as
`pi-coding-eval-result/v0`. Each command receives these required substitutions as argv values:
`{task_json}`, `{workspace}`, `{capabilities_json}`, `{result_json}`, `{attempt_id}`, and
`{baseline_id}`. `{controller_root}` and `{controller_python}` are optional controller-owned
substitutions for checked-in helper adapters. Each required token must occur exactly once; this
prevents an adapter from guessing identity from a temporary filename or accidentally writing the
result to an ambiguous path. Commands are never passed through a shell, and the controller rejects
the host-installed `pi`/`pi-agent` executables. The adapters must write the typed result JSON
described below. Keep credentials in the adapter wrapper (for example, a caller-owned `vault`
command), never in a task, result, or controller environment.

```bash
python3 evals/controller.py run \
  --baselines evals/baselines.local.json \
  --allow-provider \
  --out evals/results/local.json
```

The explicit opt-in is intentionally hard to bypass. Without `--allow-provider`, `run` exits
before creating a workspace or launching an adapter. The controller passes a small sanitized
environment and never forwards the parent process's secret variables. A provider adapter may
still use network access after the caller has explicitly opted in; live runs are therefore
manual and remain outside parity CI. Every adapter starts in a controller-owned process group;
on task timeout the controller terminates and reaps that whole group, including a secret injector,
wrapper, and any spawned model process.

## Contract boundary

Task JSON is public and versioned. It contains the prompt, initial workspace, timeout, capability
schemas, and an oracle identifier, but not the hidden assertions. The oracle lives in the
controller and runs only after the adapter reports terminal settlement. An agent's final text is
never sufficient for a coding success: `interval-merge-v0` imports the submitted implementation
in a fresh Python process and exercises additional edge cases that are not in the task's test
prompt.

The adapter result must have this shape:

```json
{
  "schema_version": "pi-coding-eval-result/v0",
  "attempt_id": "...",
  "baseline_id": "upstream",
  "terminal": {"status": "completed"},
  "final_text": "...",
  "turns": 3,
  "tool_calls": 4,
  "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0},
  "trace": []
}
```

`trace` is retained as typed, redacted adapter data. The controller records task and capability
hashes, baseline/profile/revision metadata, workspace-input hash, terminal status, oracle result,
elapsed time, turns, calls, usage, timeout/cancellation, and exit status. Provider responses and
secrets do not belong in the result artifact.

The controller validates the result schema and the explicit `attempt_id`/`baseline_id` identity
against the current invocation before invoking an oracle. A result with a valid shape but the
wrong attempt or baseline is a contract failure, not a scored attempt.

## Baseline and wave rules

The local manifest must include both `upstream` and `rust`, pin both baseline commands,
upstream/profile revisions, model/provider revision, sampling settings, and timeout. The controller pairs `upstream` and `rust` attempts by
task and repeat, then derives a deterministic order from the manifest seed. It executes each
baseline/wave group separately with bounded local concurrency:
`min(concurrency, admission_concurrency)`. Stagger applies before each later adapter launch;
stop-on-failure prevents new launches while already admitted attempts finish. Each report records
logical concurrency, admission limit, observed active peak, planned/attempted counts, and whether
a wave stopped early. Coding and READY waves use separate policies.

Do not interpret one successful run as a claim of parity. The final gate needs reproducible,
controller-scored results for both baselines, paired attempt records, and success/median/p95
reports across the selected repeats.
