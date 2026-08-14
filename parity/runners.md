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
scripted text/tool-call turns, default-parallel and explicitly sequential deterministic host tools
(including tool errors), and a normal final turn. It intentionally rejects cancellation, provider
errors, and other unimplemented grammar with exit status `2`; extend it together with the next
upstream/Rust differential case. The separate
[`upstream/profile-runner.mts`](upstream/profile-runner.mts) emits the pinned default coding
profile and is not an agent lifecycle fixture runner.

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

It supports the same queued-action, text, and tool-continuation slice, including default-parallel
and explicitly sequential tool batches plus tool failures, as the upstream runner. It currently
rejects cancellation, provider errors, and other unimplemented fixture semantics with exit status
`2`. The Rust runner's adapter is responsible
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

## Recorded evidence

Recorded fixtures are consumed only by an explicit recorded adapter. They are useful when a
provider endpoint is unavailable or its response is intentionally frozen, but they do not grant
the runner network access. Existing captures—including the OpenRouter unavailable-model capture—
must be preserved byte-for-byte unless a separate migration artifact is requested.
