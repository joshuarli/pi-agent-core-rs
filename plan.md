# pi-agent-core-rs

Build a small, auditable, headless Rust implementation of the useful runtime semantics of
`@earendil-works/pi-agent-core`, including a version-pinned default Pi coding profile. An optional
embedded Luau policy plane adds programmability without becoming part of every agent instance.

This project is **not** a port of Pi's interactive application, session system, or ambient
configuration. It does reproduce the selected default coding profile—its system prompt, active
tools, and tool-local prompt material—because those are observable inputs to useful headless
coding-agent behavior.

It is a behavioral-parity implementation of the minimal stateful agent kernel:

> model stream → assistant response → tool execution → tool results → next model turn

Everything related to terminal UI, interactive session navigation, project configuration, persisted Pi sessions, resource discovery, skills, extensions, themes, commands, and coding-agent UX is explicitly out of scope.

The resulting library should be suitable for running very large numbers of disposable agents inside RL environments, CI sandboxes, VM worlds, and agent swarms.

---

# 1. Primary objectives

The planned repository layout is:

```text
pi-agent-core-rs/
├── crates/
│   ├── pi-agent-core/        # pure Rust agent runtime
│   ├── pi-agent-protocol/    # stable data/event types
│   ├── pi-agent-trace/       # optional lean trajectory recorder (V0)
│   └── pi-agent-luau/        # optional embedded policy runtime (V1)
│
├── parity/
│   ├── upstream/             # in-process runner importing the Pi agent SDK
│   ├── rust/                 # runner using our implementation
│   ├── fixtures/
│   └── compare/
│
├── examples/
│   ├── rust/
│   └── luau/                 # V1
│
└── docs/
    ├── scope.md
    ├── semantics.md
    ├── parity-ledger.md
    └── architecture.md
```

The Rust library must be independently useful without any external language runtime. A pure
Rust agent pays no Luau startup or memory cost. `V1.md` defines the optional Luau policy layer;
it is downstream of and cannot alter the core state-machine contract.

The runtime must have:

* no TUI dependency;
* no filesystem configuration discovery;
* no Pi session files;
* no global configuration;
* no implicit `$HOME` access;
* no package/plugin discovery;
* no Node, TypeScript, napi-rs, or JavaScript runtime;
* no built-in model-provider dependency;
* no requirement for `pi-ai`;
* no runtime, thread pool, or background task created by the core; the application owns its
  Smol executor and any spawned work;
* no ambient persistence.

---

# 2. Pin the upstream SDK subset

Before implementing runtime behavior:

1. Identify the current public Pi agent SDK exports that are within this project's scope.
2. Record the upstream repository URL, exact git commit SHA, package version, Node version,
   package-manager lockfile input, and runner command in:

```text
parity/UPSTREAM_COMMIT
```

3. Build the upstream fixture runner by importing that SDK in-process. It must exercise the
   library API directly with deterministic host callbacks; it must not execute Pi, the coding
   agent, or any other Pi CLI.
4. Inspect only the relevant parts of:

```text
packages/agent/              # kernel behavior
packages/ai/                 # only for protocol/type understanding
packages/coding-agent/src/core/system-prompt.ts
packages/coding-agent/src/core/tools/  # default profile only
```

5. Do **not** treat the rest of `packages/coding-agent`, Pi's interactive application, or any
   session/UI package as an implementation target.

The listed coding-agent system-prompt and tool modules are an explicit profile specification, not
permission to import session, UI, configuration, resource-discovery, or extension behavior.

Create:

```text
docs/parity-ledger.md
docs/pi-sdk-subset.md
```

containing every upstream semantic that we intentionally:

* support;
* defer;
* reject as out of scope.

Every parity behavior should point to:

* upstream file;
* upstream symbol;
* test fixture exercising it.

`docs/pi-sdk-subset.md` names the public SDK method, type, event behavior, and default coding
profile artifacts that are the parity target. The profile ledger records upstream path, exported
factory/symbol, byte hash of generated prompt text, active-tool order, schemas, prompt snippets,
prompt guidelines, and a fixture. Internal upstream files may explain behavior, but they do not
expand the target.
The ledger uses only `supported`, `deferred-to-v1`, `rejected`, and `investigating` statuses.

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
* streaming partial tool updates
* thrown tool errors converted into tool-result errors
* dynamic tool-result metadata required for parity
* the pinned Pi default coding tools and their prompt-local guidance
* caller replacement, removal, addition, and capability wrapping of every default tool

### Default coding profile

Ship `PiDefaultCodingProfile`, enabled by the ergonomic Rust builder and fully replaceable by a
sterile or application profile. At the pinned upstream commit, its active set is derived from
Pi's default `selectedTools` and its tool definitions—not a Rust-maintained approximation. It
constructs the default system prompt from the same ordered prompt template, active tool snippets,
tool guidelines, and explicit workspace value as Pi.

The profile contains concrete local implementations for the pinned standard tools, currently
identified by upstream factories as `read`, `bash`, `edit`, `write`, `grep`, `find`, and `ls`.
Which are active by default is captured from upstream rather than assumed. A caller supplies an
explicit workspace root and may replace each filesystem/process operation or omit the profile
entirely. No tool discovers a cwd, home directory, settings, or capability implicitly.

There is no permission or approval UI. The profile exposes a programmatic capability/policy
boundary so an embedding can permit, deny, log, sandbox, or replace an operation without the
agent loop knowing how the decision was made.

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
* between turns.

Cancellation must leave the `Agent` in a valid idle state.

Per-tool execution overrides; `beforeToolCall`; `afterToolCall`; blocking tools;
early-termination hints; `convertToLlm`; `transformContext`; `shouldStopAfterTurn`;
`prepareNextTurn`; steering; follow-up; and queue modes are V0 scope. Their exact behavior is
established by the pinned SDK fixtures before implementation; no undocumented escape hatch may
invent a competing contract.

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
compaction policy
permission UI
approval UI
provider authentication
provider catalogs
model discovery
OpenTelemetry
Sentry
```

Do not port Pi's harness/session infrastructure simply because it lives in `packages/agent` or
`packages/coding-agent`.

The design target is an **agent execution microkernel plus an explicit default coding profile**,
not everything exported by the npm package.

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

The loop itself must not know:

* HTTP;
* Anthropic;
* OpenAI;
* Qwen;
* vLLM;
* SGLang;
* filesystem access;
* shell execution;
* VM execution.

Those mechanisms belong behind tools and world capabilities. `PiDefaultCodingProfile` supplies
the pinned local filesystem/process tools as one explicit profile; a sterile embedding can omit
them, and a world embedding can replace them without changing the loop.

---

# 7. Model abstraction

Do **not** port `pi-ai`.

Define a small provider boundary.

## Async runtime policy

Use Smol for examples, tests, and host integration. Tokio is prohibited as a direct dependency
or enabled feature in every workspace crate. The core must be executor-owned, not
executor-owning: it may compose `Send` futures but must not call `smol::block_on`, create an
executor, spawn background work, or expose Tokio types. Applications create and drive the Smol
executor. Parallel tool work is composed within the run rather than detached from it.

Cancellation is a structured, executor-agnostic protocol concern. V0 uses its small in-core
`CancellationToken`, whose `cancelled()` future wakes provider/tool/hook adapters without a Tokio
type or global runtime.

Compile and test against the checked-in nightly toolchain in `rust-toolchain.toml`. There is no
stable-Rust or MSRV compatibility target.

For example:

```rust
type ModelFuture<'a> = Pin<Box<dyn Future<Output = Result<Box<dyn ModelEventStream>, SchedulerError>> + Send + 'a>>;

pub trait ModelEventStream: Send {
    fn next_event<'a>(
        &'a mut self,
        cancel: CancellationToken,
    ) -> ModelEventFuture<'a>;
}

pub trait ModelProvider: Send + Sync {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> ModelFuture<'a>;
}
```

The provider future resolves once the response source exists; the core reduces one event before
polling for the next. `ModelStream` remains a finite deterministic replay adapter, not the
production streaming contract.

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
    pub metadata: JsonValue,
}
```

Do not encode provider-specific assumptions into the agent core.

An embedding, world runtime, or optional Luau policy adapter must be able to implement
`ModelStream` through the same Rust boundary.

This permits:

```text
pi-agent-core-rs
       │
       ├── custom HTTP provider
       ├── SGLang provider
       ├── vLLM provider
       ├── native Rust provider
       └── caller-owned stream adapter
                │
                └── custom gateway, world runtime, or native provider
```

A future provider crate may be added, but provider implementation is not part of the initial parity milestone.

---

# 8. Protocol types

Create a small stable protocol crate.

Use explicit Rust protocol types plus Miniserde JSON codecs for:

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

Tools are explicit capabilities. The runtime bundles the pinned Pi default coding profile, but
callers own its authority: they choose the workspace and local operations, replace or remove any
standard tool, wrap it with policy, and add domain tools.

The scheduler should understand:

```text
name
description
JSON Schema
execution mode
execute callback
```

It should not contain tool-specific scheduling paths.

Use a standard-future boundary; do not add `async-trait` merely to spell this trait:

```rust
type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>>;

pub trait AgentTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> &JsonValue;

    fn execution_mode(&self) -> Option<ToolExecutionMode>;

    fn execute<'a>(
        &'a self,
        call: ToolCall,
        cancel: CancellationToken,
        updates: ToolUpdateSink,
    ) -> ToolFuture<'a>;
}
```

Use JSON Schema-compatible values at the boundary.

Do not introduce a Rust-specific schema DSL into the public kernel API.

Rust callers, Luau policy tools, and future external interfaces must all pass raw JSON Schema or
an equivalent serialized schema value at this boundary.

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

* failed tools;
* successful tools;
* sequential-only tools inside an otherwise parallel batch.

---

# 11. Hooks

Support host-provided Rust callbacks corresponding to Pi's useful loop hooks. They are kernel
extension points; the optional Luau layer adapts to them later and does not redefine their
semantics.

`before_tool_call` may allow execution, block with a reason, or request termination.
`after_tool_call` may replace content, details, error status, usage, or a termination hint. Its
changes use Pi-compatible replacement semantics, never an undocumented recursive merge.

`transform_context` runs before `convert_to_llm`. The persisted context supports an explicitly
versioned host-message envelope so applications can add non-LLM messages without the core
inventing UI-message concepts. `convert_to_llm` can convert, filter, or normalize that envelope
into `LlmMessage`s. `should_stop_after_turn` and `prepare_next_turn` are implemented exactly as
established by the pinned SDK fixtures.

Hook errors, cancellation, observer settlement, and replacement precedence are typed contracts.
They must not bypass agent state transitions or tool-result ordering.

---

# 12. Steering and follow-up queues

Implement explicit `steering` and `follow_up` queues, not a general actor/mailbox framework.
Steering drains at the Pi-compatible points of an active run. Follow-up messages drain only when
the run would otherwise become idle. The M0 state table fixes the behavior of `prompt`,
`continue`, cancellation, and explicit queue methods while active; no input is silently assigned
a queue class without that contract.

Fixtures cover one and many steering messages, arrival during tools, one and many follow-ups,
mixed queue ordering, queue modes present at the pin, and cancellation with queued messages.

---

# 13. Agent ownership model

Prefer one owned `Agent` with internally synchronized state rather than passing mutable state
throughout application code. V0 permits exactly one active `Run` per `Agent`.

Suggested API direction:

```rust
let agent = Agent::builder()
    .model(model)
    .system_prompt(prompt)
    .model_stream(provider)
    .tools(tools)
    .build();

let run = agent.start_prompt(task)?;
```

The run handle must allow an observer to register before the first lifecycle event is emitted
and must provide the final result. The exact Rust ergonomics remain provisional, but these
state transitions are not:

* direct `prompt`, `continue`, steering, and follow-up behavior while active is fixed by the
  Milestone 0 SDK fixtures and represented explicitly in the state table;
* `abort` is idempotent;
* terminal success, failure, and cancellation clear streaming and pending-tool state before the
  agent becomes idle;
* state inspection returns a documented snapshot, never a borrowed mutable view;
* dropping an unfinished run cancels it or is explicitly disallowed—choose one during Milestone
  0 and test it.

---

# 14. Event settlement semantics

This is easy to get subtly wrong.

Determine exactly which upstream event consumers are awaited and when the agent becomes idle.

Reproduce externally observable behavior.

Do not overload one mechanism with two incompatible meanings. The API distinguishes:

* an `EventObserver`, an explicitly registered async callback whose settlement behavior follows
  the pinned Pi SDK; and
* an observational subscription, which has documented capacity, overflow, and dropped-consumer
  behavior and cannot hold a run open indefinitely.

Milestone 0 defines stable run, turn, message, and tool-call IDs; whether update payloads are
snapshots or patches; and the terminal-event grammar. Tests should prove, where the pinned SDK
does await an observer:

```text
agent_end emitted
      ↓
async observer still running
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

Use structured, executor-agnostic cancellation. No Tokio type may appear in the core's public
or private API.

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

Use scripted providers, tools, and a controllable test clock to make cancellation boundaries
deterministic under Smol; do not depend on Tokio paused-time facilities.

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

Rust panics must never be used for expected model/tool/runtime failures.

The optional Luau layer adds typed script syntax, typecheck, runtime, forbidden-capability, and
resource-limit failures at its own boundary. Neither Rust hooks nor scripts can corrupt agent
invariants.

---

# 17. Lean tracing

Tracing is separate from runtime state. `pi-agent-trace` consumes immutable typed events and
records a linear episode, never a Pi session tree. Its canonical in-memory model contains an
episode header, model turns, tool executions, and an episode end; invariant run metadata is not
repeated per record. JSON Lines and CBOR are explicit caller-selected sinks. No trace, JSONL,
and CBOR runs must have identical agent behavior; sink failure is reported separately and cannot
change the agent outcome. Prompts and tool content require a caller-selected redaction policy.

---

# 18. Optional Luau policy plane

`pi-agent-luau` is an optional downstream crate built with `mlua` and Luau. Rust owns mechanism;
Luau owns policy; the embedding world supplies capabilities. `pi-agent-core` compiles and runs
without `mlua`, Luau, world APIs, or a scripting VM, and no `mlua` type appears in core APIs.

Luau never owns state transitions, message history, stream handling, tool scheduling/order,
queues, cancellation, event settlement, tracing, usage aggregation, failure classification, or
resource ownership. It may use the Rust hook surface to permit/transform a tool call, stop after
a turn, expose a tool, emit a trace annotation, or express orchestration intent. The Rust state
machine validates every resulting action.

The V1 specification in `V1.md` defines hermetic VM construction, capability manifests, module
resolution, static Luau declarations, coroutine/future bridging on the caller-owned Smol
executor, cancellation, limits, error translation, testing, and performance gates.

---

# 19. Capability-oriented policy ABI

Do not mirror Rust structs or the internal object graph into scripts. Provide a small stable ABI
of host-controlled capability modules such as `@agent`, `@world`, `@trace`, `@task`, `@json`, and
`@time`. A capability manifest states exactly which operations each worker receives. Scripts have
no implicit filesystem, process, environment, network, home, current-directory, clock, FFI, or
package authority.

The host controls `require`: virtual capability modules and optional bundle-local relative modules
form a closed dependency graph. Absolute imports, global package paths, LuaRocks/npm-like
resolution, native plugin loading, and network package fetches are excluded.

---

# 20. External interfaces

There is no Node, TypeScript, napi-rs, JavaScript callback bridge, or generated binding plan.
Potential IPC, C ABI, WASM/component, Python, or other interfaces are post-V1 work only if a
demonstrated use case justifies them. Any interface must preserve the same explicit capability
and core-state boundaries.

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
upstream in-process Pi agent SDK runner
Rust implementation
```

Each runner emits a canonical result containing provider requests, tool invocations, ordered
events, final state, and terminal outcome:

```text
canonical-result.json
```

Normalize only genuinely nondeterministic fields:

* wall-clock timestamps;
* generated UUIDs;
* measured durations.

Do not normalize semantic ordering differences away.

The scenario language includes a controllable clock, tool completion delays, and external
actions such as cancellation. It is declarative: fixtures cannot contain arbitrary Rust or
runner-specific callbacks that make the two implementations behave differently.

Then compare structurally.

## Recorded provider corpus

In addition to synthetic deterministic scripts, keep a small, redacted corpus of real Pi SDK
provider streams. Capture it through Pi only to observe a provider integration; do not invoke
Pi as part of the parity test itself. Both the in-process upstream SDK runner and the Rust
runner replay the recorded stream through their deterministic fake-provider adapters.

Each recording includes the normalized request, streamed response events, final response, Pi
and model versions, capture date, and a redaction manifest. It must never include an API key,
authorization header, account identifier, or ambient Pi configuration. Recordings use harmless
fixed prompts and are a supplemental regression corpus, not the normative source of behavior.
Recorded terminal transport errors use the same manifest and contain the normalized terminal
outcome when no provider stream exists.

---

# 22. Required V0 parity corpus

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

## Hooks and context

22. before hook: allow, block, block with reason, and terminate
23. after hook: change content, error state, metadata, and termination hint
24. transform context and filter a host-only message
25. convert a host message to an LLM message
26. replace model, context, and thinking level between turns
27. graceful stop after a turn

## Queues

28. one and many steering messages
29. steering while tools execute
30. one and many follow-up messages
31. mixed steering/follow-up ordering and queue modes
32. cancellation with queued messages

## Cancellation

33. cancel during streaming
34. cancel during tool
35. cancel during parallel tools
36. cancel while a hook is pending
37. cancel then reuse same agent

## Event semantics

38. exact plain-run lifecycle
39. exact tool-run lifecycle
40. observer registration ordering
41. awaited observer settlement, if present in the pinned subset
42. pending-tool state
43. streaming-message state

## Default coding profile

44. byte-for-byte default system prompt after normalizing only explicit workspace values
45. active-tool order, schemas, snippets, and guidelines
46. each standard tool's successful, invalid-input, and host-error behavior
47. replacement/removal of a standard tool and sterile-profile construction

Do not call the parity milestone complete until these pass.

---

# 23. Concurrency and race testing

After semantic parity, run focused tests plus `cargo fmt`, `cargo clippy`, `cargo test`,
coverage, and Miri where practical. Consider `loom` only for the narrow synchronization boundary
where deterministic interleaving exploration adds value. The core contains no unsafe Rust and
compiles with warnings denied. Do not add a test-runner dependency merely to replace `cargo test`.

Stress the pure Rust runtime with 100 and 1,000 deterministic agents, parallel tool batches,
high cancellation churn, repeated reuse after failure, slow and dropped event observers, and
default-profile workspace isolation. Run the standard coding profile only in explicit temporary
workspaces; it must not create sessions or consult host configuration.

---

# 24. Fuzz/property testing

Use deterministic property-style matrices—permuted completion orders, bounded cancellation
checkpoints, and profile compositions—for source-ordered tool-result insertion; terminal-state
cleanup (`is_streaming == false`, no pending calls); cancellation settlement and reuse; balanced
message/event lifecycles; and profile composition where active tools determine the generated
prompt. Every run starts and ends exactly once; every tool start has at most one terminal end.
Do not add a property-testing dependency unless it is separately approved.

---

# 25. Benchmarking

Benchmark fake-provider/tool overhead, never LLM latency: agent/profile construction, idle
memory, prompt-loop and event throughput, tool scheduling, cancellation settlement, trace
serialization, and 1 through 10,000 idle / 100 through 1,000 active deterministic agents. Record
RSS, CPU, allocations where practical, and p50/p95/p99 lifecycle overhead. Measure the optional
Luau layer separately in V1; never attribute provider latency to the core.

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
2. diff the selected public SDK exports and relevant `packages/agent` files;
3. classify changed semantics and update `pi-sdk-subset.md` if the target changes;
4. add or modify a declarative fixture;
5. run the in-process upstream SDK fixture;
6. update Rust implementation;
7. update the parity ledger.

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

The V0 public contract is the Rust protocol, core APIs, default coding profile, and typed events.
The optional Luau capability ABI is versioned independently in V1. There is no source-compatible
TypeScript API or Node binding target.

---

# 28. Milestones

## Milestone 0 — Specification

Deliver:

```text
UPSTREAM_COMMIT
scope.md
semantics.md
parity-ledger.md
pi-sdk-subset.md
default-coding-profile.md
first deterministic in-process upstream fixtures
```

No substantial Rust loop implementation yet.

Exit criterion:

We can describe the state machine without referring vaguely to “whatever Pi does,” including
the active-run, observer, cancellation, stream-event, and recorded-provider contracts.
The default prompt/tool profile is captured from the pinned upstream symbols with fixtures and
hashes, not copied from memory.

---

## Milestone 1 — Minimal Rust Agent

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
one-active-run ownership
state snapshots
```

Exit criterion:

Plain-generation parity fixtures pass through a usable stateful `Agent` and `Run` API on a
caller-owned Smol executor.

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
default Pi coding profile
```

Exit criterion:

All tool ordering, standard-profile, cancellation, and event-state fixtures pass. The default
profile reproduces the pinned prompt text and active standard-tool definitions while permitting
a caller to replace, remove, or sandbox every tool.

---

## Milestone 3 — Policy hooks and queues

Support `beforeToolCall`, `afterToolCall`, `transformContext`, `convertToLlm`,
`shouldStopAfterTurn`, `prepareNextTurn`, steering, follow-up, and pinned queue modes.

Exit criterion:

All hook, context, queue, cancellation, and settlement fixtures pass in Rust without a scripting
runtime.

---

## Milestone 4 — Lean tracing

Implement the optional linear trajectory recorder with JSONL and CBOR sinks, explicit redaction,
and trace-failure isolation.

Exit criterion:

No-trace, JSONL, and CBOR executions have identical observable agent behavior.

---

## Milestone 5 — Hardening

Run race, property, cancellation, profile-isolation, and scale suites plus the focused quality
checks.

Exit criterion:

The pure Rust runtime is reusable after every terminal outcome and completes the declared
deterministic scale suites without ambient state.

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

V0 is successful when all of the following are true:

1. A Rust program can use `pi-agent-core-rs` without installing or invoking Pi.
2. The runtime performs no ambient configuration or filesystem discovery.
3. No Pi session files exist.
4. No interactive/TUI concept exists in the core.
5. Core agent-loop behavior matches the pinned in-process Pi SDK subset across the V0 parity
   corpus.
6. Parallel tool ordering, hooks, context policies, and queues match the selected Pi semantics.
7. The default coding profile reproduces the pinned prompt, active-tool set, schemas, snippets,
   guidelines, and standard-tool behavior, while every capability remains replaceable.
8. Cancellation is structured, clears transient state, and leaves the same agent reusable.
9. Tool and provider implementations are explicit capabilities; no profile discovers authority.
10. The model-provider abstraction does not require porting `pi-ai`.
11. The core is driven by a caller-owned Smol executor and contains no Tokio, Node, TypeScript,
    napi-rs, or scripting dependency.
12. The core contains no unsafe Rust.
13. Recorded provider streams are redacted, replayable, and supplementary to the deterministic
    corpus.
14. Upstream Pi SDK and default-profile updates can be evaluated through a repeatable
    parity-diff procedure.

V0 should finish as an **agent execution microkernel with an explicit default coding profile**,
not gradually become an interactive application. `V1.md` adds optional Luau policy without
changing this boundary.

---

# 33. Final V0 gate — comparative coding-task evaluation

Add an `evals/` harness only after all preceding V0 gates are green. It compares the pinned
upstream headless Pi default profile with the Rust `PiDefaultCodingProfile` under the same task,
model endpoint and revision, sampling settings, timeout, initial workspace, active tool set, and
capability adapter. It is an end-to-end quality and operating comparison, not a replacement for
the differential semantic suite.

Every task is a versioned contract: fixed task prompt and initial workspace; minimal explicit
tool schemas; timeout; capability manifest; and a controller-owned hidden oracle. The evaluator
creates a fresh workspace for each attempt, rejects path and symlink escapes, and runs hidden
verification after settlement. The agent's final text never determines success.

Start with small deterministic programming tasks in the style of `localswarm`: a stated
multi-file implementation contract, constrained write/edit/test tools, and hidden edge cases.
Include a no-tool `READY` control to separate runtime overhead from coding behavior. Add tasks
only after controller scoring is reproducible.

Record task and capability-adapter versions, upstream/profile version, runtime version, model and
provider revision, sampling parameters, workspace-input hash, terminal outcome, controller-test
result, elapsed time, turns, tool calls, usage where available, timeout/cancellation, and a
redacted typed trace. Report attempt counts and confidence intervals with success rate plus
median/p95 elapsed time, turns, calls, and tokens. Use paired randomized single-agent runs.

Run concurrency waves separately for each baseline against one shared model replica, using the
same fresh-workspace policy, admission limit, stagger, timeout, and stop-on-failure rule. Report
logical concurrency and observed active peak. Do not blame model-server saturation on the agent
runtime without the `READY` control.

Live-provider evaluations are opt-in and never ordinary parity CI. Synthetic and recorded
fixtures remain the semantic oracle. V0 is not complete until this suite has reproducible,
controller-scored results for both baselines.
