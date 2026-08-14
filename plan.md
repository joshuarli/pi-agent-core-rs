# pi-agent-core-rs

Build a small, auditable, headless Rust implementation of the useful runtime semantics of `@earendil-works/pi-agent-core`, with a first-class handwritten TypeScript SDK.

This project is **not** a port of Pi Coding Agent.

It is a behavioral-parity implementation of the minimal stateful agent kernel:

> model stream → assistant response → tool execution → tool results → next model turn

Everything related to terminal UI, interactive session navigation, project configuration, persisted Pi sessions, resource discovery, skills, extensions, themes, commands, and coding-agent UX is explicitly out of scope.

The resulting library should be suitable for running very large numbers of disposable agents inside RL environments, CI sandboxes, VM worlds, and agent swarms.

---

# 1. Primary objectives

Produce:

```text
pi-agent-core-rs/
├── crates/
│   ├── pi-agent-core/        # pure Rust agent runtime
│   ├── pi-agent-protocol/    # stable data/event types
│   └── pi-agent-trace/       # optional lean trajectory recorder
│
├── bindings/
│   └── node/                 # thin napi-rs bridge
│
├── packages/
│   └── sdk/                  # handwritten TypeScript API
│
├── parity/
│   ├── upstream/             # runner using real pi-agent-core
│   ├── rust/                 # runner using our implementation
│   ├── fixtures/
│   └── compare/
│
├── examples/
│   ├── rust/
│   └── typescript/
│
└── docs/
    ├── scope.md
    ├── semantics.md
    ├── parity-ledger.md
    └── architecture.md
```

The Rust library must be independently useful without JavaScript.

The TypeScript SDK must feel like a native TypeScript library rather than generated Rust bindings.

The runtime must have:

* no TUI dependency;
* no filesystem configuration discovery;
* no Pi session files;
* no global configuration;
* no implicit `$HOME` access;
* no package/plugin discovery;
* no built-in coding tools;
* no built-in model-provider dependency;
* no requirement for `pi-ai`;
* no background threads/processes other than explicitly created async runtime work;
* no ambient persistence.

---

# 2. Pin the upstream specification

Before implementing runtime behavior:

1. Clone the current upstream Pi repository.
2. Record the exact git commit SHA in:

```text
parity/UPSTREAM_COMMIT
```

3. Inspect only the relevant parts of:

```text
packages/agent/
packages/ai/                 # only for protocol/type understanding
```

4. Do **not** treat `packages/coding-agent` as an implementation target.

It can be consulted only when necessary to understand how `pi-agent-core` is consumed.

Create:

```text
docs/parity-ledger.md
```

containing every upstream semantic that we intentionally:

* support;
* defer;
* reject as out of scope.

Every parity behavior should point to:

* upstream file;
* upstream symbol;
* test fixture exercising it.

Never silently implement something merely because upstream contains it.

---

# 3. Hard scope boundary

## In scope

Implement behavioral equivalents of the useful `Agent` / agent-loop semantics:

### Agent state

* system prompt
* active model descriptor
* thinking level
* messages
* available tools
* streaming state
* current partial assistant response
* pending tool-call IDs
* last runtime error

### Agent execution

* `prompt(...)`
* `continue()`
* provider streaming
* multi-turn execution
* assistant tool calls
* tool execution
* tool-result injection
* continued inference after tools
* normal completion
* errors
* cancellation

### Tool semantics

* tool schemas
* argument validation
* sequential execution
* parallel execution
* per-tool execution override
* `beforeToolCall`
* `afterToolCall`
* streaming partial tool updates
* thrown tool errors converted into tool-result errors
* blocking tools
* early-termination hint
* dynamic tool-result metadata required for parity

### Context policy

* `convertToLlm`
* `transformContext`
* `shouldStopAfterTurn`
* `prepareNextTurn`

### Queue semantics

* steering messages
* follow-up messages
* queue mode if applicable at the pinned upstream commit
* exact ordering guarantees

### Events

Implement the meaningful Pi event lifecycle:

```text
agent_start
turn_start

message_start
message_update*
message_end

tool_execution_start
tool_execution_update*
tool_execution_end

turn_end

...

agent_end
```

Preserve ordering and settlement semantics.

### Cancellation

Support cancellation during:

* model streaming;
* tool preparation;
* tool execution;
* hook execution where possible;
* between turns.

Cancellation must leave the `Agent` in a valid idle state.

---

# 4. Explicitly out of scope

Do not implement:

```text
pi-coding-agent
pi-tui
SessionManager
SessionRepo
SessionStorage
/tree
/resume
branch navigation
session labels
session names
session JSONL
extension persistence
extension UI state
skills
prompt templates
resource loaders
AGENTS.md discovery
.pi discovery
~/.pi discovery
settings
themes
keybindings
interactive commands
package management
MCP
shell tools
filesystem tools
grep
edit
read
write
compaction policy
coding-agent system prompts
permission UI
approval UI
provider authentication
provider catalogs
model discovery
OpenTelemetry
Sentry
```

Do not port the newer Pi harness/session infrastructure simply because it lives in `packages/agent`.

The design target is the **agent state machine**, not everything exported by the npm package.

---

# 5. Architectural principle: Pi is the executable specification

Do not translate TypeScript source mechanically into Rust.

Instead:

1. identify observable behavior;
2. build a deterministic upstream fixture;
3. capture its event/state result;
4. implement the equivalent Rust behavior;
5. compare normalized results.

The project should converge by differential testing.

Use the upstream implementation as an oracle.

---

# 6. Core Rust architecture

The core runtime should depend only on abstract host capabilities.

Conceptually:

```text
                  ┌──────────────────┐
                  │   Agent<State>   │
                  └────────┬─────────┘
                           │
                           ▼
                  ┌──────────────────┐
                  │    Agent Loop    │
                  │                  │
                  │ finite state     │
                  │ machine          │
                  └───┬──────────┬───┘
                      │          │
              model   │          │ tools
                      ▼          ▼
              ModelStream    ToolExecutor
                 trait        callbacks
```

The loop must not know:

* HTTP;
* Anthropic;
* OpenAI;
* Qwen;
* vLLM;
* SGLang;
* filesystem access;
* shell execution;
* VM execution.

Those belong outside the kernel.

---

# 7. Model abstraction

Do **not** port `pi-ai`.

Define a small provider boundary.

For example:

```rust
#[async_trait]
pub trait ModelStream: Send + Sync {
    async fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<AssistantStream, ModelTransportError>;
}
```

`ModelRequest` should contain only what the agent loop actually needs:

```rust
pub struct ModelRequest {
    pub model: ModelDescriptor,
    pub system_prompt: String,
    pub messages: Vec<LlmMessage>,
    pub tools: Vec<ToolDefinition>,
    pub thinking_level: ThinkingLevel,
    pub options: StreamOptions,
}
```

Keep `ModelDescriptor` deliberately lightweight.

Something like:

```rust
pub struct ModelDescriptor {
    pub provider: String,
    pub id: String,
    pub metadata: serde_json::Value,
}
```

Do not encode provider-specific assumptions into the agent core.

The TypeScript SDK must be able to implement `ModelStream` with a callback.

This permits:

```text
pi-agent-core-rs
       │
       ├── custom HTTP provider
       ├── SGLang provider
       ├── vLLM provider
       ├── native Rust provider
       └── TypeScript callback
                │
                └── pi-ai, AI SDK, custom gateway, etc.
```

A future provider crate may be added, but provider implementation is not part of the initial parity milestone.

---

# 8. Protocol types

Create a small stable protocol crate.

Use `serde` types for:

```text
ModelDescriptor
ThinkingLevel

UserMessage
AssistantMessage
ToolResultMessage

TextContent
ImageContent
ToolCallContent

ToolDefinition
ToolResult

AgentEvent
AssistantStreamEvent
Usage
StopReason
```

Preserve enough shape compatibility to compare against Pi.

Avoid unnecessary Pi-specific fields.

When Pi exposes a field that is irrelevant to execution, classify it explicitly in the parity ledger instead of copying it automatically.

Prefer tagged enums.

For example:

```rust
enum AgentEvent {
    AgentStart,
    TurnStart,

    MessageStart { message: Message },
    MessageUpdate { ... },
    MessageEnd { message: Message },

    ToolExecutionStart { ... },
    ToolExecutionUpdate { ... },
    ToolExecutionEnd { ... },

    TurnEnd { ... },
    AgentEnd { messages: Vec<Message> },
}
```

Keep these serializable.

---

# 9. Tool abstraction

Tools are caller-owned capabilities.

The core should understand:

```text
name
description
JSON Schema
execution mode
execute callback
```

It should not understand what the tool does.

Suggested abstraction:

```rust
#[async_trait]
pub trait AgentTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> &serde_json::Value;

    fn execution_mode(&self) -> Option<ToolExecutionMode>;

    async fn execute(
        &self,
        call: ToolCall,
        cancel: CancellationToken,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError>;
}
```

Use JSON Schema-compatible values at the boundary.

Do not introduce a Rust-specific schema DSL into the public kernel API.

TypeScript users should be able to pass:

* TypeBox;
* Zod converted to JSON Schema;
* raw JSON Schema;
* generated schemas.

Choose one mature Rust JSON Schema validator and isolate it behind an internal adapter so it can be replaced.

---

# 10. Parallel tool execution semantics

This requires dedicated parity tests.

Pi has nontrivial ordering semantics.

For a single assistant message:

```text
tool A
tool B
tool C
```

under parallel execution:

* tool preparation happens in source order;
* allowed tools may execute concurrently;
* `tool_execution_end` occurs in actual completion order;
* resulting tool-result messages are inserted into model context in assistant/source order.

Do not simplify this.

Construct deterministic tests where completion order is deliberately:

```text
C → A → B
```

and verify that:

```text
events:
C → A → B

context tool results:
A → B → C
```

Also test mixtures of:

* blocked tools;
* failed tools;
* successful tools;
* terminate flags;
* sequential-only tools inside an otherwise parallel batch.

---

# 11. Hooks

Support host-provided callbacks corresponding to Pi's useful loop hooks.

### Before tool call

```text
before_tool_call
```

May:

* allow execution;
* block execution;
* provide reason;
* optionally request termination.

### After tool call

```text
after_tool_call
```

May override:

* content;
* details;
* error status;
* usage;
* termination hint.

Use replacement semantics matching Pi rather than recursive merge semantics.

### Context transform

```text
transform_context
```

Called before provider conversion.

### Provider conversion

```text
convert_to_llm
```

Allows application-specific messages to be:

* converted;
* filtered;
* normalized.

Do not build UI-message concepts into Rust.

The generic mechanism should simply allow arbitrary application messages to disappear or become model messages.

### Turn hooks

Implement:

```text
should_stop_after_turn
prepare_next_turn
```

according to parity tests.

---

# 12. Steering and follow-up queues

Implement these as explicit agent-runtime concepts because they materially affect execution semantics.

The agent owns separate queues:

```text
steering
follow_up
```

Steering is consumed at Pi-compatible drain points during an active run.

Follow-up messages are consumed only when the run would otherwise become idle.

Write deterministic tests around:

* one steering message;
* many steering messages;
* steering arriving while tools execute;
* follow-up after normal completion;
* multiple follow-ups;
* steering plus follow-up;
* cancellation with queued messages;
* queue drain ordering.

Do not introduce general actor/mailbox infrastructure.

Implement only what parity requires.

---

# 13. Agent ownership model

Prefer one owned `Agent` with internally synchronized state rather than passing mutable state throughout application code.

Suggested API direction:

```rust
let agent = Agent::builder()
    .model(model)
    .system_prompt(prompt)
    .model_stream(provider)
    .tools(tools)
    .build();

let run = agent.prompt(task).await?;
```

Support event subscription independently:

```rust
let mut events = agent.subscribe();

while let Some(event) = events.recv().await {
    // consume event
}
```

Or return a `Run` handle with both event stream and final result.

Do not force the exact API before the state machine is correct.

Correctness first, API ergonomics second.

---

# 14. Event settlement semantics

This is easy to get subtly wrong.

Determine exactly which upstream subscribers are awaited and when the agent becomes idle.

Reproduce externally observable behavior.

Tests should prove:

```text
agent_end emitted
      ↓
async subscriber still running
      ↓
agent remains logically busy
      ↓
subscriber resolves
      ↓
run resolves / agent becomes idle
```

If reproducing this literally harms Rust API design, preserve equivalent externally visible semantics and document the adaptation.

---

# 15. Cancellation

Use structured cancellation.

Recommended primitive:

```text
tokio_util::sync::CancellationToken
```

A run owns a child cancellation scope.

Every operation should receive it:

```text
provider stream
tool execution
hooks
queue wait points
```

Required tests:

```text
abort before first model token
abort during model streaming
abort between streamed chunks
abort before tool execution
abort while one parallel tool is running
abort while multiple tools are running
abort after tool completion before next model turn
abort while hook is pending
```

Verify:

* no orphaned tasks;
* no continued event emission after terminal settlement;
* pending tool-call state is cleared;
* agent returns to idle;
* next prompt can run normally.

Run tests under Tokio's deterministic/paused-time facilities where useful.

---

# 16. Failure semantics

Distinguish:

```text
transport failure
model-returned error
model abort
tool failure
tool panic
hook failure
protocol violation
schema violation
caller cancellation
internal invariant violation
```

Do not collapse everything into `anyhow::Error`.

Use a small typed error hierarchy.

At FFI boundaries, errors may be serialized into stable TypeScript error classes.

Rust panics must never be used for expected model/tool/runtime failures.

Use `catch_unwind` around foreign callbacks if required to protect the runtime boundary.

---

# 17. Lean tracing

Tracing must be separate from runtime state.

Do **not** recreate `session.jsonl`.

The core publishes typed execution events.

An optional trace crate consumes them.

Conceptually:

```text
Agent
  │
  ├── application subscriber
  │
  └── TraceSink
```

A trace is a **linear episode**, not a branchable session database.

Initial logical schema:

```text
EpisodeHeader
  format_version
  run_id
  model
  system_prompt_hash
  metadata

ModelTurn
  sequence
  input/reference
  assistant output
  usage
  stop reason

ToolExecution
  sequence
  call id
  tool
  arguments
  result
  status
  duration

EpisodeEnd
  outcome
  aggregate usage
```

Avoid repeating invariant run-level metadata on every record.

Do not include:

```text
parentId
tree node IDs
bookmarks
labels
display metadata
UI flags
session names
branch summaries
```

Implement at least two sinks:

```text
JsonLinesTraceSink      # debugging/interchange
CborTraceSink           # compact production representation
```

The in-memory Rust data model is canonical; serialization is a replaceable concern.

Tracing must be optional and must not influence agent behavior.

---

# 18. TypeScript binding

Use `napi-rs` for the first native Node-compatible binding.

But maintain this strict layering:

```text
Rust core
    ↓
thin napi-rs bridge
    ↓
handwritten TypeScript SDK
```

Do not expose generated binding objects as the public SDK.

The Rust core must never depend on napi-rs.

This preserves the option to later add:

```text
WASM
Deno FFI
Bun-specific binding
IPC/service binding
Python
C ABI
```

without disturbing the runtime.

---

# 19. TypeScript SDK design

Target an API approximately like:

```ts
import { Agent } from "@pi-agent-core-rs/sdk";

const agent = new Agent({
  model: {
    provider: "local",
    id: "qwen3-1.7b",
  },

  systemPrompt: "...",

  stream: async (request, signal) => {
    return myProvider.stream(request, signal);
  },

  tools: {
    exec: {
      description: "...",
      schema: ExecSchema,

      execute: async (args, ctx) => {
        return {
          content: [{ type: "text", text: "..." }],
        };
      },
    },
  },
});

const run = agent.prompt("do the task");

for await (const event of run.events) {
  console.log(event);
}

const result = await run.result;
```

The public TypeScript package should provide:

```text
Agent
AgentRun
AgentEvent
AgentState
Tool
ModelStream
TraceSink
AbortSignal integration
```

Use normal JavaScript conventions:

* `AbortSignal`
* async functions
* async iterators
* plain objects
* discriminated unions

Do not leak:

* Rust handles;
* channels;
* Arc;
* mutexes;
* opaque generated enums;
* napi implementation details.

---

# 20. FFI callback model

Exercise caution here.

TypeScript callbacks may provide:

```text
model streaming
tool execution
hooks
event consumers
```

All callbacks must cross the native boundary safely.

Build one isolated bridge layer responsible for:

```text
JS Promise → Rust Future
AbortSignal → CancellationToken
Rust event → JS object
JS exception → typed Rust failure
```

Avoid scattering napi callback code throughout the agent core.

Stress test:

```text
100 agents
1,000 agents
many concurrent tool callbacks
rapid cancellation
JS callback rejection
consumer dropping event streams
```

FFI correctness is a separate test dimension from agent parity.

---

# 21. Differential parity harness

This is the most important part of the project.

Create a completely deterministic fake provider.

Example script:

```json
[
  {
    "expect": "initial request",
    "emit": [
      "text delta",
      "tool call A",
      "tool call B",
      "finish"
    ]
  },
  {
    "expect": "tool results",
    "emit": [
      "final answer",
      "finish"
    ]
  }
]
```

Use exactly the same scenario definition for:

```text
upstream TypeScript Pi
Rust implementation
```

Each runner emits:

```text
canonical-result.json
```

Normalize only genuinely nondeterministic fields:

* wall-clock timestamps;
* generated UUIDs;
* measured durations.

Do not normalize semantic ordering differences away.

Then compare structurally.

---

# 22. Required initial parity corpus

Implement at least the following scenarios.

## Plain generation

1. simple prompt
2. streamed text
3. multiple text deltas
4. reasoning + text if represented
5. normal stop
6. length stop
7. model error
8. aborted model response

## Tools

9. one tool
10. multiple sequential tools
11. multiple parallel tools
12. parallel tools finishing reverse order
13. mixed completion ordering
14. unknown tool
15. malformed arguments
16. schema validation failure
17. tool throws
18. tool returns error result
19. partial tool update
20. multiple partial updates
21. sequential-only tool in parallel batch

## Hooks

22. before hook allow
23. before hook block
24. before hook block with reason
25. before hook terminate
26. after hook modify content
27. after hook modify error state
28. after hook modify metadata
29. after hook terminate
30. mixed terminate flags

## Context

31. transform context
32. filter application-only message
33. convert custom message
34. replace model between turns
35. replace context between turns
36. change thinking level between turns
37. graceful stop after turn

## Queues

38. steering message
39. multiple steering messages
40. follow-up message
41. multiple follow-up messages
42. steering followed by follow-up
43. queue interaction with tools

## Cancellation

44. cancel during streaming
45. cancel during tool
46. cancel during parallel tools
47. cancel while hook pending
48. cancel then reuse same agent

## Event semantics

49. exact plain-run lifecycle
50. exact tool-run lifecycle
51. subscriber registration ordering
52. awaited subscriber settlement
53. pending-tool state
54. streaming-message state

Do not call the parity milestone complete until these pass.

---

# 23. Concurrency and race testing

After semantic parity:

Use:

```text
cargo test
cargo clippy
cargo fmt
cargo nextest
cargo miri where practical
cargo llvm-cov
```

Consider `loom` for the small subset of synchronization machinery where deterministic concurrency exploration adds value.

No `unsafe` code unless absolutely necessary.

If a binding dependency internally uses unsafe, that is fine; our own crates should default to:

```rust
#![forbid(unsafe_code)]
```

The pure core should compile with warnings denied.

---

# 24. Fuzz/property testing

Use property-based tests for:

### Message ordering

For arbitrary tool batches:

```text
context result ordering == source ordering
```

regardless of execution completion order.

### State settlement

After every terminal outcome:

```text
is_streaming == false
pending_tool_calls == empty
```

### Cancellation

For cancellation injected at arbitrary state-machine transition points:

```text
run eventually settles
agent returns to valid reusable state
```

### Event structure

Every run:

```text
starts exactly once
ends exactly once
```

Every tool start has at most one terminal tool end.

Every message lifecycle is balanced.

Use `proptest`.

---

# 25. Benchmarking

Do not benchmark LLM latency.

Benchmark runtime overhead only with fake providers/tools.

Measure:

```text
agent creation latency
idle Agent memory
prompt-loop overhead
events/sec
tool scheduling overhead
N parallel tool calls
cancellation settlement latency
TypeScript↔Rust callback overhead
trace serialization throughput
```

Important scenarios:

```text
1 agent
100 agents
1,000 agents
10,000 idle agents

100 active deterministic agents
1,000 active deterministic agents
```

Record:

```text
RSS
allocations if practical
CPU time
event throughput
p50/p95/p99 lifecycle overhead
```

The design goal is not to win synthetic benchmarks at the expense of simplicity.

The goal is to establish whether the runtime is cheap enough to become invisible relative to model inference and environment execution.

---

# 26. Upstream compatibility process

Pi will evolve.

Do not continuously chase `main`.

Maintain:

```text
UPSTREAM_COMMIT
PARITY_VERSION
```

Updating upstream is an explicit operation:

1. advance `UPSTREAM_COMMIT`;
2. diff relevant `packages/agent` files;
3. classify changed semantics;
4. add or modify fixture;
5. run upstream fixture;
6. update Rust implementation;
7. update parity ledger.

Never auto-import upstream code.

A CI job may periodically report drift, but upgrades remain deliberate.

---

# 27. API stability policy

Distinguish:

```text
behavioral parity
```

from:

```text
source/API compatibility
```

We require the former.

We explicitly do not require the latter.

Do not make the TypeScript API look worse merely to reproduce Pi's TypeScript API exactly.

Maintain an optional compatibility package later if there is demand:

```text
@pi-agent-core-rs/pi-compat
```

but do not include it in the initial implementation.

---

# 28. Milestones

## Milestone 0 — Specification

Deliver:

```text
UPSTREAM_COMMIT
scope.md
semantics.md
parity-ledger.md
first deterministic upstream fixtures
```

No substantial Rust loop implementation yet.

Exit criterion:

We can describe the state machine without referring vaguely to “whatever Pi does.”

---

## Milestone 1 — Minimal Rust loop

Support:

```text
user prompt
model stream
assistant response
no tools
events
normal termination
model failure
cancellation
```

Exit criterion:

Plain-generation parity fixtures pass.

---

## Milestone 2 — Tools

Support:

```text
schemas
validation
tool calls
tool results
sequential execution
parallel execution
partial updates
tool failures
```

Exit criterion:

All tool ordering parity fixtures pass.

---

## Milestone 3 — Policy hooks and queues

Support:

```text
beforeToolCall
afterToolCall
transformContext
convertToLlm
shouldStopAfterTurn
prepareNextTurn
steering
follow-up
```

Exit criterion:

Corresponding differential fixtures pass.

---

## Milestone 4 — Agent wrapper

Implement the ergonomic stateful Rust `Agent`.

Support:

```text
prompt
continue
state inspection
subscribe
abort
queue steering
queue follow-up
```

Exit criterion:

All core parity scenarios pass in Rust.

---

## Milestone 5 — TypeScript binding

Build isolated napi-rs bridge.

Exit criterion:

TypeScript can provide:

```text
model provider callback
tool callbacks
hooks
AbortSignal
event consumers
```

without requiring any Pi npm package.

---

## Milestone 6 — Handwritten TypeScript SDK

Ship ergonomic API.

Exit criterion:

A TypeScript example can run an entire multi-tool agent without exposing generated native-binding objects.

---

## Milestone 7 — Lean tracing

Implement optional linear trajectory recorder.

Exit criterion:

The same agent can run:

```text
with no trace
with JSONL trace
with compact CBOR trace
```

with identical behavioral results.

---

## Milestone 8 — Stress and fuzz testing

Exit criterion:

* parity corpus passes;
* property tests pass;
* cancellation produces no leaked tasks;
* 1,000 concurrent deterministic agents complete reliably;
* the same Agent can be reused after cancellation/failure;
* TS callbacks survive stress runs.

---

# 29. First end-to-end demonstration

Build a small TypeScript example with:

```text
fake/OpenAI-compatible model endpoint
four caller-owned tools
no persistence
no Pi dependency
lean tracing enabled
```

Suggested tools:

```text
read
write
exec
list
```

The tools may operate against a temporary directory.

Demonstrate:

```text
const agent = new Agent(...)

await agent.prompt(
  "Create a small program, run its tests, and repair any failures."
)
```

Verify:

* multiple model turns;
* multiple tool executions;
* exact event trajectory;
* compact trace;
* clean process exit.

Do not build a TUI.

---

# 30. Second demonstration: swarm suitability

Create:

```text
examples/typescript/swarm.ts
```

Launch a configurable number of isolated agents:

```text
N = 100
```

Each gets:

* independent message state;
* independent tools;
* independent cancellation;
* independent trace metadata.

They may share one model provider object.

Prove that no agent has:

```text
global cwd
global settings
global session
global filesystem state
global message history
```

This example should make the intended deployment model obvious.

---

# 31. Quality constraints

Prefer:

```text
boring code
small modules
explicit ownership
typed state transitions
few dependencies
deterministic tests
```

over abstractions.

Avoid:

```text
generic workflow engines
actor frameworks
plugin systems
dependency injection frameworks
proc-macro-heavy APIs
custom async runtimes
home-grown serialization
global registries
hidden caches
```

Every dependency should earn its place.

---

# 32. Definition of done

The project is successful when all of the following are true:

1. A TypeScript program can use `pi-agent-core-rs` without installing Pi.
2. The runtime performs no ambient configuration or filesystem discovery.
3. No Pi session files exist.
4. No interactive/TUI concept exists in the core.
5. Core agent-loop behavior matches the pinned Pi implementation across the parity corpus.
6. Parallel tool ordering matches Pi exactly.
7. Steering/follow-up behavior matches the selected Pi semantics.
8. Cancellation is structured and leak-free.
9. Tool and provider implementations remain caller-owned.
10. The model-provider abstraction does not require porting `pi-ai`.
11. A linear compact trace can be recorded independently of agent state.
12. The TypeScript SDK is handwritten and ergonomic.
13. The pure Rust core contains no napi-rs code.
14. The core contains no unsafe Rust.
15. Large numbers of independent agents can coexist without ambient shared state.
16. Upstream Pi updates can be evaluated through a repeatable parity-diff procedure.

The project should finish as an **agent kernel**, not gradually become another coding-agent application.
