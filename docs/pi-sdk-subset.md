# Pinned Pi SDK subset

This document names the upstream public surface that is a parity target. It does not promise
source/API compatibility with TypeScript. The Rust public contract is the protocol and core API
described here and in `docs/semantics.md`.

## Pin record

The checked-in source currently reports:

| Input | Value |
| --- | --- |
| Repository | `https://github.com/badlogic/pi-mono.git` |
| Checkout | `parity/upstream/source` |
| Commit | `9d2ec7ffabe927bfad2214c1cee25b6632a78dcf` |
| Pi agent package | `@earendil-works/pi-agent-core` `0.84.1` |
| Root package | `pi-monorepo` `0.0.3` |
| Node requirement | `>=22.19.0` from root/package engines |
| Lockfile | `parity/upstream/source/package-lock.json` |
| Lockfile SHA-256 | `01bb5df57ed0de4f308ed1b88b4536e43a0b61ce82b0eff3fcb1b922176c3c4c` |
| Runner | `<fill exact command used by the in-process fixture runner>` |
| Recorded at | `2026-08-13` |

The canonical machine-readable copy belongs in `parity/UPSTREAM_COMMIT`; if it differs from this
table, the machine-readable pin and a fixture diff win. The fixture runner imports the SDK in
process. It must not invoke Pi CLI, `pi-coding-agent`, a shell command supplied by the scenario, or
a real provider.

## Selection rule

Include a public symbol only when it contributes to the headless execution microkernel or pinned
default coding profile. Types from `@earendil-works/pi-ai` are copied only to the degree needed to
describe protocol data; provider implementations, catalogs and auth remain out of scope. Internal
upstream files may explain a selected behavior but cannot expand this list.

## Stateful `Agent` surface

Source: `packages/agent/src/agent.ts`, exported through `packages/agent/src/index.ts`.

| Export/member | Target behavior | Fixture |
| --- | --- | --- |
| `Agent` constructor / `AgentOptions` | Explicit initial state, `streamFn`, model, tools, hooks, queue modes, and stream options | `agent/construct` |
| `subscribe(listener)` | Ordered lifecycle observer; listener receives active abort signal; settlement participates in run | `events/observer-settlement` |
| `state` / `AgentState` | Snapshot of prompt/model/thinking/tools/messages and runtime stream/pending/error fields | `state/snapshot-reset` |
| `steeringMode`, `followUpMode` / `QueueMode` | `all` or `one-at-a-time` independent modes | `queues/modes` |
| `steer(message)` | Queue a message for the next eligible steering drain point; it may be enqueued while idle without starting a run | `queues/steering` |
| `followUp(message)` | Queue a message for the next idle boundary; it may be enqueued while idle without starting a run | `queues/follow-up` |
| `clearSteeringQueue()`, `clearFollowUpQueue()`, `clearAllQueues()` | Remove queued messages explicitly | `queues/clear` |
| `hasQueuedMessages()` | Report either queue non-empty | `queues/basic` |
| `signal` | Expose active run cancellation signal to host callbacks; absent while idle | `cancel/signal` |
| `abort()` | Idempotently cancel active run; idle is a no-op | `cancel/reuse` |
| `waitForIdle()` | Resolve after terminal event listeners settle, not merely after `agent_end` emission | `events/observer-settlement` |
| `reset()` | Idle-only clear of transcript, queues, transient state and last error | `state/snapshot-reset` |
| `prompt(message)` | Start a run from text, one message, or ordered messages; text can include images | `run/prompt-input` |
| `continue()` | Start a run from current transcript with tail validation and queued-message special case | `run/continue-tail` |

Rust names may differ. The observable behavior, field meaning, error classification, and event
ordering are the target; TypeScript overloads and declaration-merging customization are not.

## Low-level loop surface

Source: `packages/agent/src/agent-loop.ts` and its exported types.

| Export/type | Target |
| --- | --- |
| `agentLoop(prompts, context, config, signal, streamFn)` | Event-producing loop for a new prompt |
| `agentLoopContinue(context, config, signal, streamFn)` | Event-producing loop without appending a new prompt |
| `runAgentLoop(...)` / `runAgentLoopContinue(...)` | Deterministic async loop behavior used by the stateful wrapper |
| `AgentEventSink` | Ordered event sink whose async settlement is awaited |
| `AgentContext` | Explicit system prompt, transcript and optional ordered tools |
| `AgentLoopConfig` | Model, conversion/transform, stream options, hooks and queue pollers |
| `StreamFn` | Caller-provided model stream; failure contract is represented in returned stream/final message |
| `AgentEvent` | Lifecycle/event payloads listed below |

## Protocol types

The loop uses selected types imported from `@earendil-works/pi-ai`; V0 defines equivalent
serializable Rust types rather than porting the provider package.

| Upstream type family | Required selected shape |
| --- | --- |
| Model | Provider/name/API/id descriptor, thinking support metadata only where request construction needs it |
| User message | `role: user`, text/image content, timestamp as a normalized nondeterministic field |
| Assistant message | `role: assistant`, text/thinking/tool-call content, stop reason, usage, provider/model metadata and error text as needed |
| Tool-result message | `role: toolResult`, tool call/name, text/image content, details, usage, error flag, optional added tool names |
| Content | Text, image and tool-call discriminated variants; tool-call arguments and provider ID preserved |
| Tool definition | Name, description, raw JSON Schema-compatible parameters |
| Usage | Input/output/cache/token/cost totals where exposed by the selected stream |
| Stop reason | Normal, length, error, aborted and any additional value proven necessary by fixture |
| Assistant stream event | Start, text/thinking/tool-call partial updates, done/error; payload snapshots preserved enough for event parity |

Do not copy irrelevant provider fields. Add a field only with a ledger row and a fixture proving
that it changes an execution result, request, event, or state invariant.

## Event behavior

The selected event union is:

```text
agent_start
agent_end { messages }
turn_start
turn_end { message, tool_results }
message_start { message }
message_update { message, assistant_message_event }
message_end { message }
tool_execution_start { tool_call_id, tool_name, args }
tool_execution_update { tool_call_id, tool_name, args, partial_result }
tool_execution_end { tool_call_id, tool_name, result, is_error }
```

The Rust protocol may add stable run/turn/message correlation IDs as specified in
`docs/semantics.md`; adapter-generated IDs are normalized only when upstream carries none. Event
ordering, partial snapshot/patch semantics, observer settlement, and terminal-state cleanup are
part of the target, not implementation conveniences.

## Hook, queue and tool contracts

| Upstream symbol | Target |
| --- | --- |
| `transformContext` | Runs before `convertToLlm` for every request |
| `convertToLlm` | Converts/filters host messages at the model boundary |
| `beforeToolCall` | Runs after argument preparation/validation; may block with reason/termination hint |
| `afterToolCall` | Runs after execute and before tool end/result message; field replacement has no deep merge |
| `shouldStopAfterTurn` | Runs after `turn_end`; true emits `agent_end` without queue poll or next model request |
| `prepareNextTurn` / `prepareNextTurnWithContext` | Can replace next context/model/thinking state |
| `steeringMode` / `getSteeringMessages` | Drain after applicable turn points, mode controls one/all |
| `followUpMode` / `getFollowUpMessages` | Drain only when no tool/steering work remains |
| `AgentTool` | Schema, optional argument preparation, async execute, updates, per-tool execution mode |
| `AgentToolResult` | Content/details/usage, added tools, terminate hint |
| `ToolExecutionMode` | Default parallel or sequential; mixed per-tool behavior is fixture-pinned |

## Explicit non-target exports

The package's root export also exposes harness/session/compaction/search/skills/system-prompt and
tool rendering surfaces. These are not part of this subset:

```text
packages/agent/src/harness/session/**
packages/agent/src/harness/compaction/**
packages/agent/src/harness/skills.ts
packages/agent/src/harness/tools/**
packages/agent/src/harness/system-prompt.ts
packages/agent/src/search/**
packages/agent/src/node.ts and proxy/provider integration helpers
```

They are classified in `docs/parity-ledger.md` as rejected unless a row explicitly says
`deferred-to-v1`. No Rust dependency is added to reproduce them.

## Exact subset inventory template

Run this inventory whenever the upstream pin changes:

```text
Pin:
  repository: <URL>
  commit: <40-hex SHA>
  package: <name>@<version>
  node: <exact version or declared engine>
  lockfile: <path + SHA-256>
  runner: <exact in-process command>

Public export:
  package path: packages/<package>/src/<file>.ts
  symbol: <exported name>
  kind: class | function | type | interface | event
  target status: supported | deferred-to-v1 | rejected | investigating
  observable contract: <one sentence>
  fixture: parity/fixtures/<scenario>/<runner>.json
  normalized fields: <none or explicit nondeterministic fields>
```
