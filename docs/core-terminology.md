# Core terminology

This repository follows the public vocabulary of the upstream low-level agent
package at packages/agent/src, while keeping Rust-specific names where the
contract is intentionally stronger or more explicit. This page is the resync
map: when upstream changes a type or event, search for the Rust target listed
here before changing behavior.

## Directly aligned names

| Upstream Pi | Rust core | Notes |
| --- | --- | --- |
| Agent | Agent | Owns durable state and one active run. |
| AgentMessage | state::AgentMessage | Canonical transcript union. state::Message remains a compatibility alias. |
| AgentState | state::AgentState | Rust adds explicit ownership, accounting, and host-message state. |
| AgentTool | tool::AgentTool | Explicit executable capability. |
| AgentToolCall | state::AgentToolCall | Assistant tool-call content block; AssistantToolCall is retained as a compatibility alias. |
| AgentToolResult | tool::AgentToolResult | Rust result also carries scheduler correlation and finalization fields. |
| ToolCall | tool::ToolCall | Provider tool invocation passed to a capability. |
| ToolDefinition | tool::ToolDefinition | Rust calls the schema field schema; parameters() is the upstream vocabulary. |
| ToolExecutionMode | tool::ToolExecutionMode | Sequential and Parallel. |
| ThinkingLevel | state::ThinkingLevel | Canonical levels are Off, Minimal, Low, Medium, High, XHigh, Max. |
| stop | state::StopReason::Stop | Rust retains StopReason::EndTurn as a compatibility associated constant. |
| QueueMode | queue::QueueMode | OneAtATime and All correspond to upstream "one-at-a-time" and "all". |
| steer() | Agent::steer() | enqueue_steering() remains as the original Rust spelling. |
| followUp() | Agent::follow_up() | enqueue_follow_up() remains as the original Rust spelling. |
| AgentLoopTurnUpdate | hooks::AgentLoopTurnUpdate | NextTurn remains as a compatibility alias. |

## Rust envelope names

The upstream package emits a discriminated AgentEvent union. Rust wraps the
same lifecycle payload in event::AgentEvent so every event has a RunId and
per-run EventSequence; the payload is event::AgentEventKind (also available
as AgentEventPayload). This is deliberate: generated identity and ordering
are part of the Rust state-machine contract.

Likewise, Rust's ContextEnvelope is not a rename of upstream AgentContext.
It is a versioned policy boundary containing canonical messages and explicit
host-only messages. The Rust provider request is built from it by a hook, so
provider conversion and host policy cannot be smuggled into the durable agent
state.

AgentSnapshot, RunHandle, RunState, RunSnapshot, AgentPhase, and
RunPhase are Rust lifecycle/inspection types without direct upstream peers.
They make ownership and cancellation settlement observable in a caller-owned
executor, which is a deliberate expansion of the low-level contract.

## Compatibility and intentional differences

- ThinkingLevel::Default is retained as an associated-constant compatibility
  spelling for ThinkingLevel::Off; upstream's default is "off".
- Rust messages and tool results use owned serialized protocol values at the
  integration boundary. Upstream uses richer pi-ai message/content types.
- Rust's AgentTool receives a ToolCall and returns a finalized
  AgentToolResult; upstream separates the tool-call block from the result
  returned by execute(). The Rust scheduler needs the correlation and error
  fields to enforce result placement and settlement.
- AgentSession, session events, compaction UI, extensions, and application
  tools belong to packages/coding-agent/src/core, not this microkernel. They
  must not be copied into tea-core; the TUI and Luau crates are explicit
  downstream hosts.

When resyncing from upstream, first compare packages/agent/src/types.ts,
agent.ts, and agent-loop.ts; then compare the corresponding Rust targets
above. Treat a name absent from the direct-alignment table as a contract review,
not an automatic rename.
