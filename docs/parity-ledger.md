# Milestone 0 parity ledger

This ledger is the index of intentional upstream behavior. `plan.md` requires every row to point
to an upstream path and symbol plus a deterministic fixture. Status values are deliberately closed:
`supported`, `deferred-to-v1`, `rejected`, and `investigating`. A row marked `supported` is a V0
target even when its fixture is still to be checked in; it is not permission to infer missing
semantics from source alone.

The selected upstream checkout is `parity/upstream/source`; the pin metadata belongs in
`parity/UPSTREAM_COMMIT`. Paths in this document are relative to that checkout unless noted.

## Ledger entry schema

Use one row per externally observable contract, not one row per implementation helper.

| Field | Required content |
| --- | --- |
| ID | Stable `PL-<domain>-<number>` identifier |
| Status | Exactly one of the four values above |
| Upstream path/symbol | File and exported method/type/event/factory; internal helpers may explain but cannot widen target |
| Observable behavior | Ordering, payload, state, error, or settlement guarantee |
| Fixture | Deterministic in-process fixture path and scenario ID |
| Evidence | Canonical result field/assertion expected from both runners |
| Normalization | `none`, `timestamp`, `generated-id`, or `duration` only; semantic differences are never normalized |
| Exit criterion | Required for `investigating`; otherwise the parity assertion |

Copy this exact template for new rows:

```text
ID: PL-<domain>-<number>
Status: supported | deferred-to-v1 | rejected | investigating
Upstream path: packages/<package>/<path>:<line>
Upstream symbol/export: <name>
Observable behavior: <one observable contract>
V0/V1 rationale: <boundary reason>
Fixture: parity/fixtures/<scenario>/<runner>.json
Evidence: <canonical-result field/assertion>
Normalization: none | timestamp | generated-id | duration (explain)
Exit criterion: <required when investigating>
```

## Agent API and loop

| ID | Status | Upstream path / symbol | Observable target | Fixture / evidence |
| --- | --- | --- | --- | --- |
| PL-AGENT-001 | supported | `packages/agent/src/agent.ts` / `Agent` constructor, `AgentOptions` | Construct explicit state, stream function, hooks, queues, model/tool config; no ambient setup | `agent/construct` / state snapshot and first request |
| PL-AGENT-002 | supported | `agent.ts` / `prompt` and `normalizePromptInput` | Accept text, one message, or ordered messages; append prompt messages and emit their lifecycle before assistant work | `events/plain-prompt` / messages and event order |
| PL-AGENT-003 | supported | `agent.ts` / `continue`, `runContinuation`; `agent-loop.ts` / `agentLoopContinue` | Continue only from non-empty transcript whose last message is user/tool-result; assistant tail is rejected or follows pinned queued-message special case | `run/continue-tail` / typed outcome |
| PL-AGENT-004 | supported | `agent.ts` / `steer`, `followUp`, `clear*Queue`, `hasQueuedMessages` | Explicit steering/follow-up queues with independent modes and documented drain points | `queues/basic-and-mixed` |
| PL-AGENT-005 | supported | `agent.ts` / `subscribe`, `waitForIdle` | Listener registration order, active signal, and awaited `agent_end` settlement are observable | `events/observer-settlement` |
| PL-AGENT-006 | supported | `agent.ts` / `abort`, `signal`, `runWithLifecycle`, `finishRun` | Idempotent cancellation; terminal state clears stream/pending IDs and permits reuse | `cancel/reuse` |
| PL-AGENT-007 | supported | `agent.ts` / `state`, `reset`; `types.ts` / `AgentState` | State snapshot fields and top-level copy-on-assignment semantics | `state/snapshot-reset` |
| PL-AGENT-008 | supported | `agent-loop.ts` / `runAgentLoop`, `runAgentLoopContinue`, `runLoop` | Multi-turn progression, normal completion, tool continuation, and terminal agent event | `events/plain`, `events/tool` |
| PL-AGENT-009 | supported | `types.ts` / `AgentContext`, `AgentMessage`, `AgentToolCall`, `AgentToolResult` | Selected protocol shape sufficient for model requests, context, tool results, and transcript | `protocol/round-trip` |
| PL-AGENT-010 | supported | `types.ts` / `AgentLoopConfig` | `transformContext` precedes `convertToLlm`; next-turn replacement affects subsequent request only | `hooks/context-and-next-turn` |
| PL-AGENT-011 | investigating | `agent.ts` / `ActiveRun`; `types.ts` / `AgentEvent` (no public run/turn/message IDs) | Stable V0 correlation ID assignment and adapter normalization | `identity/plain-and-tool-run`; exit: add fixture proving ID generation/mapping and no reuse after cancellation |
| PL-AGENT-012 | investigating | `agent.ts` / `processEvents`; `subscribe` | Observer failure, unsubscribe/reentrancy, and non-blocking subscription capacity adaptation | `events/observer-edge-cases`; exit: record a single terminal result and explicit Rust observer contract |
| PL-AGENT-013 | rejected | `packages/agent/src/harness/**` / session and harness exports | Session tree, compaction, skills, resource loading, persisted session behavior | `scope/rejection-matrix` |

## Event lifecycle

| ID | Status | Upstream path / symbol | Observable target | Fixture / evidence |
| --- | --- | --- | --- | --- |
| PL-EVENT-001 | supported | `types.ts` / `AgentEvent`; `agent-loop.ts` / `runAgentLoop` | `agent_start` and initial `turn_start` precede prompt message events | `events/plain` |
| PL-EVENT-002 | supported | `agent-loop.ts` / `streamAssistantResponse` | Assistant start/update/end behavior, including no-start final response fallback | `events/stream-shapes` |
| PL-EVENT-003 | supported | `agent-loop.ts` / `executeToolCalls*` | Tool start/preparation order, update flushing, end order, and result-message source order | `tools/parallel-reverse-order` |
| PL-EVENT-004 | supported | `agent-loop.ts` / `emitToolResultMessage` | Every tool result has message start/end after finalized tool execution | `events/tool` |
| PL-EVENT-005 | supported | `agent-loop.ts` / `runLoop` | `turn_end` contains assistant message and ordered tool results; next turn starts only after its settlement | `events/multi-turn` |
| PL-EVENT-006 | supported | `agent.ts` / `processEvents` | Event reducer updates streaming/pending/error state before observers run | `events/observer-state-view` |
| PL-EVENT-007 | supported | `agent.ts` / `finishRun` | `agent_end` is last emitted event; idle follows awaited terminal observers | `events/observer-settlement` |
| PL-EVENT-008 | investigating | `types.ts` / `AgentEvent` | Stable run/turn/message IDs and terminal-event grammar for Rust protocol | `identity/*`, `events/grammar`; exit: fixture must prove generated IDs and exactly-one terminal event |

## Tools and hooks

| ID | Status | Upstream path / symbol | Observable target | Fixture / evidence |
| --- | --- | --- | --- | --- |
| PL-TOOL-001 | supported | `types.ts` / `AgentTool`, `ToolExecutionMode` | Name, description, raw JSON Schema-compatible parameters, execute callback, optional per-tool mode | `tools/definitions` |
| PL-TOOL-002 | supported | `agent-loop.ts` / `prepareToolCall` | Unknown tool, argument preparation, schema validation, before-hook ordering and blocked errors | `tools/validation-and-before` |
| PL-TOOL-003 | supported | `agent-loop.ts` / `executeToolCallsSequential` | Source-ordered sequential prepare/execute/finalize/end/result insertion | `tools/sequential` |
| PL-TOOL-004 | supported | `agent-loop.ts` / `executeToolCallsParallel` | Sequential preparation, concurrent allowed execution, completion-order ends, source-order result messages | `tools/parallel-reverse-order` |
| PL-TOOL-005 | supported | `agent-loop.ts` / `executePreparedToolCall` | Partial updates are emitted/settled before end; post-settlement updates ignored | `tools/partial-updates` |
| PL-TOOL-006 | supported | `agent-loop.ts` / `finalizeExecutedToolCall` | `afterToolCall` replacement is field-by-field; callback failure becomes error result | `tools/after-replacement` |
| PL-TOOL-007 | supported | `agent-loop.ts` / `shouldTerminateToolBatch` | A batch terminates only when every finalized result carries `terminate: true` | `tools/termination-hint` |
| PL-TOOL-008 | supported | `types.ts` / `BeforeToolCallResult`, `AfterToolCallResult` | Block reason, error flag/content/details/usage/termination behavior | `tools/hooks` |
| PL-TOOL-009 | investigating | `agent-loop.ts` / per-tool sequential override | Mixed sequential/parallel batch semantics at pinned commit | `tools/mixed-execution`; exit: fixture must pin whether one sequential call serializes entire batch |
| PL-TOOL-010 | supported | `types.ts` / `AgentToolUpdateCallback` | Update callback scoped to invocation and late-call suppression | `tools/partial-updates` |
| PL-TOOL-011 | rejected | `packages/agent/src/harness/tools/**` | Interactive renderers, session file mutation queue, harness tool UX | `scope/rejection-matrix` |

## Cancellation, errors, and queues

| ID | Status | Upstream path / symbol | Observable target | Fixture / evidence |
| --- | --- | --- | --- | --- |
| PL-RUN-001 | supported | `agent.ts` / `runWithLifecycle`, `handleRunFailure` | Normal/error/aborted terminal cleanup and failure message/event shape | `cancel/failure-shapes` |
| PL-RUN-002 | supported | `types.ts` / callbacks with `AbortSignal` | Cancellation reaches stream, tools, and hooks at deterministic checkpoints | `cancel/checkpoints` |
| PL-RUN-003 | supported | `agent.ts` / `PendingMessageQueue`; `agent-loop.ts` / queue polling | `all` vs `one-at-a-time` drain behavior and queue order | `queues/modes` |
| PL-RUN-004 | supported | `agent-loop.ts` / `shouldStopAfterTurn` | Graceful stop after `turn_end` without queue polling or another model request | `queues/graceful-stop` |
| PL-RUN-005 | supported | `agent-loop.ts` / `prepareNextTurn` | Context/model/thinking replacement before next request | `hooks/context-and-next-turn` |
| PL-RUN-006 | investigating | `agent.ts` / `runWithLifecycle` | Dropping unfinished Rust `Run` and observer cancellation behavior | `cancel/drop-and-observer`; exit: choose cancel-and-settle or explicit drop prohibition and test it |
| PL-RUN-007 | supported | `types.ts` / `StreamFn` contract; `agent-loop.ts` | Provider errors are stream/final-message outcomes, not implicit provider-specific exceptions | `failure/provider-error` |
| PL-RUN-008 | rejected | `packages/agent/src/harness/compaction/**` | Compaction policy, branch summarization, session context management | `scope/rejection-matrix` |

## Default coding profile

| ID | Status | Upstream path / symbol | Observable target | Fixture / evidence |
| --- | --- | --- | --- | --- |
| PL-PROFILE-001 | supported | `packages/coding-agent/src/core/system-prompt.ts` / `buildSystemPrompt` | Ordered default prompt template, workspace substitution, visible tools, snippets, guidelines | `profile/default-prompt` |
| PL-PROFILE-002 | supported | `packages/coding-agent/src/core/tools/index.ts` / `createCodingTools`, `createCodingToolDefinitions` | Default active order is captured from upstream factory output, not a Rust approximation | `profile/active-tool-order` |
| PL-PROFILE-003 | supported | `core/tools/{read,bash,edit,write,grep,find,ls}.ts` / `create*ToolDefinition` | Seven standard factories, raw schemas, descriptions and tool metadata | `profile/tool-definitions` |
| PL-PROFILE-004 | supported | `core/tools/{read,bash,edit,write,grep,find,ls}.ts` / `*ToolSystemPromptContribution` | Prompt snippets and guideline arrays included in ordered profile ledger | `profile/prompt-contributions` |
| PL-PROFILE-005 | supported | same tool files / `create*Tool` | Standard operation behavior through explicit adapters, with successful/invalid/host-error cases | `profile/tool-behavior` |
| PL-PROFILE-006 | supported | `system-prompt.ts` / `BuildSystemPromptOptions` | Caller can select/remove tools, replace prompt, append text, and supply workspace/context explicitly | `profile/sterile-and-composed` |
| PL-PROFILE-007 | rejected | `core/resource-loader.ts`, `core/skills.ts`, session/config modules | Resource discovery, skills, `.pi`/home config, sessions and interactive profile state | `scope/rejection-matrix` |

## V1 and excluded upstream surfaces

| ID | Status | Upstream path / symbol | Observable target | Fixture / evidence |
| --- | --- | --- | --- | --- |
| PL-V1-001 | deferred-to-v1 | `V1.md` / `pi-agent-luau` design (not upstream Pi) | Optional policy VM, capability modules, typed script errors, coroutine bridging, limits | `v1/abi-and-limits` (post-V0) |
| PL-V1-002 | deferred-to-v1 | `V1.md` / Luau hook/tool adapter | Adapt Rust hooks and tools without moving lifecycle semantics into scripts | `v1/hook-adapter` (post-V0) |
| PL-V1-003 | deferred-to-v1 | `V1.md` / world/task/trace capabilities | Explicit host capability manifest and closed module resolver | `v1/capability-manifest` (post-V0) |
| PL-V1-004 | rejected | Pi coding-agent/TUI/session exports | UI, persistent sessions, package/resource systems and ambient config remain outside V0 and V1 core | `scope/rejection-matrix` |
| PL-V1-005 | rejected | `packages/ai/**` provider implementations/catalogs | No `pi-ai` port or provider catalog in either V0 kernel or V1 policy plane | `scope/rejection-matrix` |

## Ledger maintenance

When the pinned commit changes, update `parity/UPSTREAM_COMMIT`, re-run the selected export and
profile inventory, classify every changed row, add a declarative fixture, run both runners, and
only then update the Rust target. Never silently import an upstream helper or change a `supported`
row's semantics without a fixture diff.
