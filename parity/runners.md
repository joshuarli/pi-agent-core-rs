# Rust runner boundaries

The checked-in runner is a small executable boundary around the Rust agent library. Keeping the
fixture adapter separate from the library makes it possible to audit fixture I/O without granting
the core filesystem, process, network, or configuration authority.

## Runner contract

The runner accepts exactly one declarative fixture path and emits exactly one canonical result. It
must:

* load only the named fixture;
* use the fixture's scripted model stream and deterministic host responses;
* use an explicit workspace/capability root supplied by the fixture;
* return a settled result for success, model error, tool error, or cancellation;
* normalize before writing output, following normalization.md.

It must not read ambient HOME, cwd, environment configuration, session files, .pi resources,
skills, extensions, prompt templates, clocks, or network services. It must not mutate the fixture
tree. A live provider call is never a substitute for a missing scripted response.

Exit status is deliberately simple: 0 means a canonical result was emitted, 2 means the fixture
or runner contract was invalid, and 1 means the run produced a valid result whose outcome is an
error or cancellation. A model/tool error is data, not a harness failure.

## Rust runner

The pi-agent-parity executable calls the Rust agent library directly and supplies the scripted
model stream, tool definitions, and explicit capabilities. It uses Miniserde to read the fixture
and a caller-owned Smol block_on only as the executable's test driver; the core library owns
neither an executor nor filesystem capability.

Run one fixture from the repository root:

    cargo +nightly-2026-07-24 run -p pi-agent-core --features parity-runner \
      --bin pi-agent-parity -- parity/fixtures/declarative/single-turn-text.json

The runner supports queued actions, text, tool continuation, deterministic
cancel_after: text_delta, post-cancellation reuse, parallel and sequential tool batches,
tool failures, and normalized terminal errors/cancellation. Other unimplemented fixture
semantics exit with status 2. The adapter maps Rust events and errors to the canonical shape;
it must not change agent semantics to force a result.

## Canonical results

The normalizer is a pure transformation from the Rust event/result representation to the
canonical JSON object. It has no model, filesystem, process, network, clock, or environment
authority. It validates first, then applies the rules in normalization.md.

The checked-in expected result is the contract oracle for each fixture. Object key order is
canonicalized for comparison, while array and event order remain significant. A malformed result
is a runner failure; a semantic mismatch requires an intentional fixture and implementation
change.

## Full corpus command

Run the complete corpus from any working directory:

    ./parity/run-rust.sh

The command builds the Rust adapter with the pinned nightly toolchain, runs every JSON fixture
under fixtures/declarative/, canonicalizes both sides with jq -S -c, and compares the result
with fixtures/expected/. Temporary output is removed on exit, and the checked-in fixture tree
is never written.

## Recorded evidence

Recorded fixtures are consumed only by their explicit provider-free replay adapter. They are
useful when an external endpoint is unavailable or a response is intentionally frozen, but they
do not grant the runner network access. Existing captures must remain immutable unless a separate
migration artifact is requested.
