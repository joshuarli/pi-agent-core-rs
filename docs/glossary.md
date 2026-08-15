# Glossary

This is the repository-wide vocabulary for pi-agent-core-rs. Terms in this
document describe ownership and observable contracts, not implementation
convenience. The exact upstream name mapping for the low-level agent package is
in [core terminology](core-terminology.md).

## Runtime

**Agent** — The durable state owner. An Agent owns the transcript, model
configuration, registered tools, explicit queues, and one-or-zero active runs.
The Rust target is pi_agent_core::Agent.

**Run** — One prompt or continue invocation together with all model turns, tool
work, queue drains, observers, cancellation, and terminal settlement caused by
that invocation. A run owns its child cancellation scope. The Rust targets are
RunHandle, RunState, and RunSnapshot.

**Turn** — One assistant response and its tool calls/results, from turn_start
through turn_end. A run may contain multiple turns when steering or follow-up
input continues the loop.

**Idle** — The Agent has no active run and all terminal observers have settled.
An emitted agent_end is not necessarily idle yet: awaited observers may still
be finishing.

**Settlement** — The single terminal path that clears streaming state,
partial-response state, pending tool calls, cancellation ownership, and active
run state before making the Agent reusable.

**Cancellation** — A request for the active run's child cancellation scope to
stop model, tool, hook, and observer work. Cancellation is cooperative at every
caller-owned async boundary; it is not an implicit executor shutdown.

## Transcript and model boundary

**AgentMessage** — A value retained in the canonical transcript. The standard
variants are user input, assistant output, and tool results. The Rust type is
state::AgentMessage; state::Message is a compatibility alias.

**Assistant message** — A model response. It may contain text and zero or more
AgentToolCall values, and it records a terminal StopReason when finalized.

**AgentToolCall** — A tool-call content block requested by an assistant
message. Its provider-supplied ToolCallId must remain stable through validation,
execution, events, and the resulting transcript message.

**Tool result** — The finalized output associated with one tool call. It is
inserted into model context in assistant source order even when parallel tool
execution completes in another order. The Rust target is
tool::AgentToolResult, with tool::ToolResult retained as the compatibility
spelling.

**Model provider** — A caller-owned implementation of the model transport port.
It receives a ModelRequest and returns a caller-driven ModelStream; the core
does not own HTTP, credentials, retries outside an adapter, or an executor.

**Model request** — The explicit provider boundary containing the selected
ModelDescriptor, system prompt, converted messages, ordered tool definitions,
thinking level, and cancellation scope.

**Model stream event** — One provider response event consumed by the run loop.
The core reduces text deltas, tool calls, usage, and terminal stop reasons into
state and lifecycle events.

**Thinking level** — The requested reasoning budget: Off, Minimal, Low, Medium,
High, XHigh, or Max. Default is retained only as a compatibility associated
constant for Off; upstream Pi's default vocabulary is "off".

**Stop reason** — The terminal outcome reported by a model turn. The normal
upstream outcome is stop; Rust uses StopReason::Stop and retains
StopReason::EndTurn as a compatibility associated constant.

## Tools and policy

**Agent tool** — An explicit executable capability with a stable name,
description, parameter schema, execution mode, and caller-owned implementation.
The Rust trait is tool::AgentTool.

**Tool definition** — The prompt-facing description of a tool without its
authority or implementation. Rust stores its JSON Schema as schema and also
exposes parameters() for upstream vocabulary.

**Tool execution mode** — Whether calls from one assistant message execute
Sequentially or may execute in Parallel.

**Tool update** — A partial result emitted while a tool is still running. It is
observable to event consumers but is not a finalized transcript result.

**Hook** — A typed policy port around a lifecycle boundary. HookSet can approve
or block a tool call, replace finalized result fields, transform context,
convert context to provider messages, stop after a turn, or return an
AgentLoopTurnUpdate.

**Context envelope** — The versioned Rust policy boundary containing canonical
messages and explicit host-only messages. ContextEnvelope is deliberately not
the upstream application/session AgentContext.

**Agent loop turn update** — Request-scoped replacements for context, model, or
thinking level after a completed turn and before the next provider request.
The Rust type is hooks::AgentLoopTurnUpdate; hooks::NextTurn is its
compatibility alias.

**Capability** — An explicit authority boundary exposed by a host, such as a
filesystem operation, process operation, model transport, or Luau binding.
Capabilities are not ambient access and are never discovered by the core.

**Policy** — A caller-owned decision layer that constrains or transforms
explicit operations. Rust hooks and optional Luau policy may decide; they do
not own the core Agent FSM or scheduler.

## Queues and lifecycle events

**Steering** — Input injected at an eligible point while a run is still active.
Rust exposes Agent::steer and the compatibility Agent::enqueue_steering.

**Follow-up** — Input held until the run would otherwise become idle. Rust
exposes Agent::follow_up and the compatibility Agent::enqueue_follow_up.

**Queue mode** — The drain policy for either queue: OneAtATime or All. Queueing
never implicitly starts a run.

**Agent event** — The lifecycle observation emitted by the core. Rust's
event::AgentEvent is an envelope containing RunId, EventSequence, and an
AgentEventKind payload. This explicit identity envelope is a deliberate Rust
extension of upstream's discriminated event union.

**Event observer** — An awaited lifecycle consumer. Observers run after the
state reducer and in registration order; terminal observers participate in run
settlement.

**Nonblocking subscription** — A bounded, lossy event channel that never
participates in settlement and never starts a background task. It is distinct
from an awaited observer and must not be silently substituted for one.

## State and maintenance operations

**Agent state** — Mutable durable and runtime-owned state behind the Agent
lock. It includes transcript/configuration plus phase, streaming, pending tool,
error, host-message, and accounting fields.

**Agent snapshot** — An owned read-only inspection view of Agent state. A
snapshot never exposes a mutable borrow into the loop.

**Compaction** — An idle-only transaction that validates and atomically
replaces retained transcript context using a caller-owned Compactor.
Compaction is not a UI summary feature in the core and cannot partially mutate
history.

**Accounting** — Provider-reported model usage and exact cost text retained
per turn and in aggregate. The core never estimates cost from token counts.

**Serialized JSON** — An owned JSON text boundary used where the core must
preserve provider or host data without exposing a serializer dependency in the
public contract. The Rust type is state::SerializedJson.

## Repository layers

**Core** — crates/pi-agent-core: state machine, scheduling, cancellation,
hooks, queues, tool validation, provider port, and optional adapter modules.

**Protocol** — crates/pi-agent-protocol: stable JSON-native values and
wire-level data types. It does not own Agent state or scheduling.

**Default coding tools** — The explicit tools/ implementation under
crates/pi-agent-core/src, exposed through the compatibility facade
default_tools.rs. Workspace/process authority arrives through operation traits.

**Provider adapter** — An opt-in concrete transport under
crates/pi-agent-core/src/provider. It must preserve the generic ModelProvider
contract and must not move provider authority into the core.

**Luau policy layer** — crates/pi-agent-luau: optional hermetic policy,
capability manifests, and tool handlers. It is downstream of the core.

**Trace layer** — crates/pi-agent-trace: an optional immutable event consumer
that records redacted trajectory data. It does not mutate Agent state.

**Terminal host** — crates/pi-agent-tui: the repository-owned pi-agent host.
It is an application layer, not part of the headless core.

**Parity fixture** — A deterministic scenario describing setup, provider
events, host actions, and expected normalized observations. Fixtures are
behavioral evidence, not a second runtime contract.

**Quality evaluation** — The deterministic evaluation harness that exercises
the same explicit ports with pinned coding tasks and records evidence beyond
unit-test success.

## Naming rule

Use upstream Pi names when the concepts are semantically equivalent:
AgentMessage, AgentToolCall, AgentToolResult, AgentLoopTurnUpdate, steer, and
follow-up. Use Rust-specific names when they carry an explicit contract absent
upstream, such as AgentEvent's identity envelope, ContextEnvelope, RunHandle,
AgentSnapshot, and SerializedJson. Preserve compatibility aliases when a
rename would unnecessarily break an existing embedding, and record the mapping
in [core terminology](core-terminology.md).
