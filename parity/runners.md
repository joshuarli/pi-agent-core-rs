# Runner boundaries

The harness has three boundaries: a fixture runner, a normalizer, and a comparator. Keeping them
separate makes it possible to audit whether a mismatch came from the implementation or from the
harness.

## Common runner contract

A runner accepts exactly one declarative fixture path and emits exactly one canonical result. It
may write diagnostics to stderr, but stdout is reserved for the result. It must:

* load only the named fixture and its checked-in expected data;
* use the fixture's scripted model stream and deterministic host responses;
* use an explicit workspace/capability root supplied by the harness;
* return a settled result for success, model error, tool error, or cancellation;
* normalize before writing output, following [`normalization.md`](normalization.md).

A runner must not read ambient HOME, cwd, environment configuration, session files, `.pi`
resources, skills, extensions, prompt templates, clocks, or network services. It must not mutate
the fixture tree. A live provider call is never a substitute for a missing scripted response.

Exit status is deliberately simple: `0` means a canonical result was emitted, `2` means the
fixture or runner contract was invalid, and `1` means the run produced a valid result whose
`outcome` is an error or cancellation. A model/tool error is data, not a harness failure.

## Upstream runner

The upstream runner imports the pinned `pi-agent-core` SDK in-process and adapts the declarative
model and host scripts to its public API. It may inspect only the pinned SDK subset recorded by
the repository's upstream ledger. It must never execute `pi`, `pi-coding-agent`, a shell command,
or another Pi CLI. The network is disabled; recorded provider captures are read as immutable data.

Its output is passed through the shared canonical normalizer. SDK-specific event names, IDs,
timestamps, and provider envelopes must not leak into canonical output.

The V0 closed-slice implementation is
[`upstream/agent-runner.mts`](upstream/agent-runner.mts). From `parity/upstream/source`, run it
against a seeded text or deterministic-tool fixture with:

```text
  ./node_modules/.bin/tsx ../agent-runner.mts ../../fixtures/declarative/tool-continue.json
```

It supports ordered queued `steer`/`follow_up`, text `prompt`, and `continue` fixture actions,
scripted text/tool-call turns, deterministic `cancel_after: "text_delta"` model checkpoints,
default-parallel and explicitly sequential deterministic host tools (including tool errors), and
reuse after a cancelled prompt. Provider errors remain stream data; other unimplemented grammar
still exits with status `2`. The separate
[`upstream/profile-runner.mts`](upstream/profile-runner.mts) emits the pinned default coding
profile and is not an agent lifecycle fixture runner. Its companion profile gate is
[`run-profile.sh`](run-profile.sh): it invokes the pinned factories in-process, never Pi. The
`grep` factory's source-owned `rg` subprocess is the sole documented exception to virtual
operation isolation; it is run only in a disposable workspace with an empty
`PI_CODING_AGENT_DIR`, and it must resolve PATH `rg` rather than a host-managed Pi binary.

## Rust runner

The Rust runner calls the Rust agent library directly and supplies the same scripted model stream,
tool definitions, and explicit capabilities. It does not launch a binary, discover a workspace,
or instantiate an optional policy runtime merely to run a V0 parity case. It should remain usable
with the core crate's declared dependencies only; the scaffold deliberately adds no crate or root
Cargo manifest.

The initial closed-slice adapter is the `pi-agent-parity` executable in the core package. It uses
Miniserde to read the explicit fixture and a Smol `block_on` only as the executable's caller-owned
test driver; the `pi-agent-core` library itself owns neither executor nor filesystem capability.
Run it from the repository root:

```text
cargo +nightly-2026-07-24 run -p pi-agent-core --features parity-runner \
  --bin pi-agent-parity -- parity/fixtures/declarative/single-turn-text.json
```

It supports the same queued-action, text, tool-continuation, deterministic
`cancel_after: "text_delta"`, and post-cancellation reuse slice, including default-parallel and
explicitly sequential tool batches plus tool failures, as the upstream runner. Provider/model
errors and cancellation are normalized as settled terminal data; other unimplemented fixture
semantics still exit with status `2`. The Rust runner's adapter is responsible
for mapping Rust events and errors to the canonical shape. It must not change agent semantics to
make a result match upstream. If the two adapters need different setup, that setup belongs in the
runner boundary and must be documented.

## Normalizer

The normalizer is a pure transformation from one runner's event/result representation to the
canonical JSON object. It has no model, filesystem, process, network, clock, or environment
authority. It validates first, then applies the rules in [`normalization.md`](normalization.md).
Normalization failures are reported as exit status `2`.

## Comparator

The comparator accepts two canonical result paths (or canonical JSON streams), validates both, and
compares the complete shape. It does not rerun a fixture or apply a second set of semantic
exceptions. A mismatch report should identify the JSON path, expected value, and actual value;
array order and event order are significant. Exit status `0` means equal and `1` means mismatch;
malformed input is `2`.

## Full corpus command

The repeatable corpus check is [`run-declarative.sh`](run-declarative.sh). It first verifies that
`parity/upstream/source` is clean and detached at the commit recorded in [`UPSTREAM_COMMIT`](UPSTREAM_COMMIT),
then builds the Rust adapter with the pinned nightly toolchain. For every JSON fixture under
`fixtures/declarative/`, it runs both adapters, canonicalizes their JSON with `jq -S -c`, and compares
each result against `fixtures/expected/` and against the other adapter. Temporary output is removed
on exit; the checked-in fixture tree is never written.

Run it from any working directory with:

```text
./parity/run-declarative.sh
```

The upstream `tsx` executable must already exist under the pinned checkout's `node_modules`; the
script does not install dependencies or contact the network. Runner exit status `1` (a fixture's
model/tool error or cancellation outcome) is accepted as data, while status `2` or a malformed
canonical result fails the corpus check. The command does not execute `pi` or any Pi CLI.

## Recorded evidence

Recorded fixtures are consumed only by an explicit recorded adapter. They are useful when a
provider endpoint is unavailable or its response is intentionally frozen, but they do not grant
the runner network access. Existing captures—including the OpenRouter unavailable-model capture—
must be preserved byte-for-byte unless a separate migration artifact is requested.
