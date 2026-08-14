Revise the `pi-agent-core-rs` implementation plan substantially.

The previous plan proposed a handwritten TypeScript SDK layered over a thin `napi-rs` binding. Remove that entire direction.

The preferred architecture is now:

```text
pi-agent-core-rs
├── pure Rust agent kernel
│   ├── Agent FSM
│   ├── model-stream abstraction
│   ├── tool scheduler
│   ├── hooks
│   ├── steering/follow-up queues
│   ├── structured cancellation
│   ├── event stream
│   └── lean trajectory tracing
│
├── embedded Luau runtime via mlua
│   ├── agent policy
│   ├── hooks
│   ├── lightweight tool definitions/wrappers
│   ├── orchestration logic
│   └── explicit world capabilities
│
└── optional external interfaces later
    ├── IPC
    ├── C ABI
    ├── WASM/component interface
    ├── Python
    └── other bindings only if justified
```

The guiding principle is:

> **Rust owns mechanism. Luau owns policy. The world runtime supplies capabilities.**

The Rust implementation remains the authoritative execution substrate. Luau is an embedded extension language, not another implementation layer.

The goal is to retain the ergonomics and programmability that motivated a TypeScript SDK without paying for Node/V8 or allowing a heavyweight host runtime to become part of every agent instance.

---

# Why Luau/mlua

Use **Luau through `mlua`**, rather than stock Lua if practical.

Luau is preferred because it preserves Lua's desirable embedding model while adding:

* gradual typing;
* better tooling;
* a more modern language surface;
* strong suitability for generated code;
* an intentionally constrained embedding environment;
* a mature VM;
* good coroutine semantics;
* good fit for capability-oriented sandboxing.

The fact that Lua/Luau is deliberately small is a feature here, not a deficiency.

Do not attempt to compensate for Luau's small standard environment by rebuilding Node, Python, or a general-purpose operating environment inside it.

The scripting environment should remain sparse and capability-driven.

The rich object in this architecture is the **world**, not the scripting language.

---

# Remove TypeScript/napi-rs completely

Delete from the plan:

```text
bindings/node/
packages/sdk/
napi-rs
handwritten TypeScript SDK
JS Promise ↔ Rust Future bridge
AbortSignal bridge
TypeScript callback providers
TypeScript callback tools
TypeScript stress tests
```

Do not retain these as transitional implementation milestones.

External language bindings may be added much later if there is a demonstrated use case.

They are not part of the first architecture.

---

# Preserve the existing Rust kernel scope

None of this changes the core parity objective.

The Rust kernel should still implement behavioral parity with the useful subset of `pi-agent-core.Agent`:

```text
Agent state
prompt()
continue()
model streaming
tool calls
tool results
parallel/sequential tool execution
beforeToolCall
afterToolCall
transformContext
convertToLlm
shouldStopAfterTurn
prepareNextTurn
steering queue
follow-up queue
structured cancellation
event streaming
state settlement semantics
```

The same hard exclusions remain:

```text
no pi-coding-agent
no TUI
no /tree
no SessionManager
no Pi session JSONL
no session labels
no session navigation
no ~/.pi discovery
no project resource discovery
no skills
no extensions system
no package manager
no settings
no themes
no coding-agent UI state
```

The behavioral parity suite against a pinned upstream `pi-agent-core` remains the main correctness oracle.

Do not allow the Luau integration to contaminate or complicate the core parity implementation.

Ideally:

```text
pi-agent-core
```

should compile and function entirely without:

```text
mlua
Luau
scripting
world APIs
```

The scripting layer is downstream.

---

# Treat Luau as an optional policy runtime

Do not require every Rust `Agent` to instantiate a Luau VM.

A simple agent with statically supplied Rust tools and policy should incur **zero scripting runtime cost**.

The architecture should support both:

```text
Rust Agent
    │
    └── pure Rust policy/tools
```

and:

```text
Rust Agent
    │
    └── Luau policy runtime
```

This matters for swarm density.

Many extremely narrow workers may not need programmable policy at all.

For example, an evaluator worker whose behavior is fully defined by:

```text
fixed prompt
fixed model
two tools
fixed termination condition
```

should not pay for a Luau state unnecessarily.

Treat scripting as an attachable capability.

---

# Rust remains authoritative

Do not move lifecycle semantics into Lua/Luau.

The following must remain entirely owned by Rust:

```text
agent state transitions
message history
tool-call sequencing
parallel tool scheduling
tool-result ordering
provider streaming
steering queue semantics
follow-up queue semantics
cancellation
event settlement
pending tool tracking
run ownership
trace sequencing
usage aggregation
failure classification
resource ownership
```

Luau should never decide *how* the agent loop works.

Luau may decide things such as:

```text
whether a tool call is permitted
how a tool call should be transformed
what tool should be exposed
whether to stop after a turn
what orchestration action to request
how to react to an event
what world operation to invoke
```

Think of it as a policy/control plane over a Rust state machine.

A bug in Luau policy should not be able to violate Rust agent invariants.

---

# Capability-oriented scripting API

Do not expose Rust structs mechanically into Luau.

Do not mirror the entire internal Rust object graph.

Design a deliberately small, stable scripting ABI.

Conceptually:

```text
Rust kernel
   │
   ├── Agent capability
   ├── World capability
   ├── Trace capability
   ├── Task capability
   └── small utility capabilities
             │
             ▼
           Luau
```

Scripts should see capabilities, not implementation details.

Initial built-in namespaces/modules should be approximately:

```text
agent
world
trace
task
json
time
```

Avoid growing this casually.

Every new host API should answer:

> Is this an actual agent/world primitive, or are we accidentally rebuilding a general application runtime?

Prefer explicit, narrow functions over giant mutable objects.

Example style:

```lua
local agent = require("@agent")
local world = require("@world")
local trace = require("@trace")

agent.on("before_tool_call", function(ctx, call)
    trace.emit("tool_requested", {
        name = call.name,
    })

    return call
end)

agent.tool({
    name = "exec",
    description = "Execute a command inside this world",
    schema = {
        type = "object",
        properties = {
            command = { type = "string" },
        },
        required = { "command" },
    },

    run = function(ctx, args)
        return world.exec(args.command)
    end,
})
```

Do not make this API overly object-oriented.

Simple tables, functions, discriminated values, and capability objects are preferable.

---

# No ambient authority

The embedded language must have **no ambient host authority**.

A Luau script should not automatically be able to access:

```text
host filesystem
host process spawning
host environment variables
host network
HOME
current host working directory
system clock beyond explicitly exposed APIs
dynamic native libraries
arbitrary FFI
package registries
OS commands
```

All meaningful external effects must arrive through explicit host capabilities.

For example:

```text
world.exec
world.fs.read
world.fs.write
world.fork
world.snapshot
world.send
```

may exist because the host deliberately injected them.

This should create a capability-secure model such as:

```text
worker A:
    agent
    world.fs.read
    world.fs.write
    world.exec

evaluator:
    agent
    trace

orchestrator:
    agent.spawn
    world.fork
    world.inspect
    trace
```

An agent should receive exactly the authority appropriate for its role.

Do not give every script a god object.

---

# Host-controlled module resolution

Do not support arbitrary Lua filesystem/package discovery.

The module model should be deterministic and host-controlled.

Support something like:

```lua
require("@agent")
require("@world")
require("@trace")
require("./local-policy")
```

where:

```text
@agent
@world
@trace
```

are host-provided virtual modules.

Bundle-local modules may optionally be supported.

Explicitly reject or avoid:

```text
arbitrary absolute filesystem imports
LuaRocks
npm-like package resolution
searching HOME
searching current working directory implicitly
global package paths
native plugin loading
runtime network package fetches
```

A policy bundle should have a closed dependency graph.

This is critical for reproducibility, hermeticity, startup performance, and RL-environment determinism.

---

# Luau typing

Use Luau's gradual type system as part of the intended developer experience.

Provide type declarations/stubs for the host capability API.

For example:

```lua
export type ToolCall = {
    id: string,
    name: string,
    arguments: {[string]: any},
}

export type ToolResult = {
    content: any,
    error: boolean?,
}

export type AgentContext = {
    run_id: string,
    turn: number,
}
```

Generate or maintain Luau definitions for:

```text
agent
world
trace
task
json
time
```

where appropriate.

The type checker should be usable independently of runtime execution.

This matters because LLMs themselves are likely to generate many policies and tools.

The desired development loop is:

```text
LLM generates Luau
       ↓
Luau static analysis
       ↓
precise diagnostics
       ↓
LLM repairs
       ↓
runtime execution
```

instead of relying entirely on dynamic failures.

Include typechecking in examples and CI.

---

# Async semantics

Async behavior is central.

Agent environments frequently involve:

```text
model inference
tool execution
VM operations
world RPC
filesystem operations
network services
other agents
sandbox startup
test execution
```

Use `mlua`'s async facilities to bridge Rust futures to Luau coroutines.

The preferred scripting ergonomics are that many host operations appear naturally synchronous:

```lua
local result = world.exec("cargo test")
```

while internally:

```text
Luau coroutine
      ↓
calls Rust async host function
      ↓
coroutine yields
      ↓
Tokio schedules other work
      ↓
future resolves
      ↓
Luau coroutine resumes
```

Avoid introducing an unnecessary JavaScript-style Promise abstraction unless Luau/mlua forces it.

The scripting language should feel simple.

The concurrency implementation remains Rust/Tokio.

---

# Do not create a second scheduler in Luau

Luau coroutines are a suspension mechanism.

They must not become the primary scheduler.

Tokio remains the scheduler for:

```text
model streams
tools
world operations
timers
cancellation
agent concurrency
```

Luau coroutines are resumed by the Rust host as futures become ready.

Do not build:

```text
custom event loop in Luau
custom thread scheduler in Luau
agent scheduler in Luau
tool scheduler in Luau
```

This is important both for performance and for preserving a single concurrency model.

---

# Structured cancellation must cross the Luau boundary

Cancellation remains owned by Rust.

When a Rust `CancellationToken` is cancelled while Luau is suspended on:

```lua
world.exec(...)
agent.prompt(...)
task.sleep(...)
```

the corresponding operation must terminate cleanly.

The Luau coroutine must not remain stranded.

Required tests:

```text
cancel while Luau waits on model
cancel while Luau waits on tool
cancel while Luau waits on world RPC
cancel during nested policy callback
cancel during hook
cancel after coroutine yield but before resume
destroy VM with pending operations
reuse surrounding Agent after cancelled script
```

No orphaned futures.

No leaked coroutine state.

No post-cancellation event emission beyond defined terminal events.

---

# Resource limits

The embedded policy runtime must not be allowed to monopolize a worker.

Investigate and implement practical limits for:

```text
instruction count / execution budget
memory
recursion depth
table/object growth where feasible
coroutine count
host calls
wall-clock execution
```

The Rust host must be able to terminate runaway scripts.

Examples:

```lua
while true do end
```

and recursive pathological code must not wedge an agent process indefinitely.

Treat resource accounting as a first-class runtime concern.

The exact enforcement mechanism may depend on `mlua`/Luau capabilities, but the architecture should expose clear limits even if some are initially best-effort.

Add adversarial tests.

---

# Error model

Translate script failures into typed runtime failures.

Differentiate:

```text
Luau syntax error
Luau typecheck error
Luau runtime error
host capability error
cancelled host operation
resource-limit exceeded
forbidden capability/module
Rust internal invariant failure
```

Do not collapse all scripting errors into a generic string.

Luau errors must never corrupt Rust agent state.

After a failed policy callback, behavior should be explicit:

```text
abort run
block tool
ignore hook
return structured error
```

depending on hook semantics.

Document this.

---

# Policy hooks

Expose the useful Rust kernel hooks to Luau.

Examples:

```lua
agent.on("before_tool_call", function(ctx, call)
    ...
end)

agent.on("after_tool_call", function(ctx, call, result)
    ...
end)

agent.on("before_turn", function(ctx)
    ...
end)

agent.on("after_turn", function(ctx, turn)
    ...
end)
```

Where possible, align these conceptually with the upstream Pi parity semantics:

```text
beforeToolCall
afterToolCall
transformContext
shouldStopAfterTurn
prepareNextTurn
```

But do not expose awkward Pi-specific structures merely for naming parity.

The Rust kernel owns semantic compatibility.

The Luau API can be cleaner.

---

# Tool definition from Luau

Allow Luau to define lightweight tools.

For example:

```lua
agent.tool({
    name = "inspect_repo",

    description = "Inspect repository state",

    schema = {
        type = "object",
        properties = {
            path = { type = "string" },
        },
    },

    run = function(ctx, args)
        local files = world.fs.list(args.path)
        return {
            content = files,
        }
    end,
})
```

Internally this should register a normal Rust-side tool adapter.

The agent loop must not special-case "Lua tools".

To the Rust scheduler, all tools follow the same lifecycle:

```text
validate
prepare
execute
emit updates
complete
insert result
```

Luau-defined tools are simply one possible implementation backend.

---

# Low-level tools should remain Rust/world capabilities

Do not implement fundamental system operations in Luau.

Operations such as:

```text
spawn VM
fork world
execute process
mount filesystem
read/write host-backed storage
network transport
sandbox control
snapshot/restore
```

belong in Rust or in external world services.

Luau should compose these operations.

This distinction is essential:

```text
Rust/world:
    mechanism

Luau:
    composition and policy
```

For example:

```lua
agent.tool({
    name = "run_tests",
    run = function(ctx, args)
        local result = world.exec({
            argv = {"cargo", "test"},
            timeout = 120,
        })

        return summarize_test_result(result)
    end,
})
```

Luau defines the policy/tool interface.

Rust implements process execution and isolation.

---

# Agent composition

Luau should eventually be capable of expressing lightweight agent composition.

Do not immediately build a swarm framework into the language.

But design the capability boundary so future APIs can support something like:

```lua
local child = agent.spawn({
    role = "reviewer",
    model = "qwen3",
    prompt = "...",
})

local result = child.join()
```

or, in a world-aware orchestrator:

```lua
local branch = world.fork()

local child = agent.spawn({
    world = branch,
    task = "try alternative implementation",
})
```

The actual scheduling, lifecycle, and world ownership remain Rust/orchestrator responsibilities.

Luau expresses orchestration intent.

This is an important long-term use case and should influence ABI design even if `agent.spawn` is not implemented in the first milestone.

---

# Lean tracing remains independent

Keep the earlier trace design.

Do not make Luau's state or VM representation part of the canonical trajectory.

The canonical execution trace belongs to the Rust runtime.

A policy event may optionally add structured annotations:

```lua
trace.emit("candidate_rejected", {
    reason = "test regression",
})
```

but canonical runtime events remain produced by Rust:

```text
episode_start
model_turn
tool_execution
tool_result
agent_event
episode_end
```

The trace remains:

```text
linear
append-only
compact
independent of session/UI semantics
```

No Pi-style tree metadata.

Luau annotations must not affect replay semantics unless explicitly designed to do so later.

---

# VM lifecycle and swarm density

Benchmark the cost of the Luau layer separately.

Measure:

```text
Luau VM creation latency
idle VM RSS
loaded host capability API cost
policy bundle load time
coroutine creation cost
Rust ↔ Luau call overhead
async yield/resume overhead
VM teardown latency
```

Test:

```text
1 VM
100 VMs
1,000 VMs
10,000 VMs if feasible
```

Also test strategies such as:

```text
one Luau state per agent
one Luau state per worker process
one Luau state shared across many isolated environments
preinitialized VM templates
cheap state recreation
```

Do not choose the sharing model prematurely.

The priority is semantic isolation.

If sharing one VM complicates capability isolation or creates mutable cross-agent state, prefer independent states despite some memory cost.

The system should make the tradeoff measurable.

---

# Hermetic VM construction

Every Luau state should begin from a known minimal environment.

Do not inherit:

```text
host globals
working directory
filesystem search paths
environment variables
process state
arbitrary native modules
```

VM initialization should be deterministic.

Define exactly:

```text
which standard Luau functions exist
which built-ins are removed/replaced
which capability modules are injected
which bundle modules are loaded
which limits apply
```

Record this in architecture documentation.

A policy run should be reproducible from:

```text
runtime version
policy bundle hash
capability manifest
task/world state
model configuration
```

not from whatever happens to exist on the host.

---

# Capability manifest

Introduce an explicit capability descriptor for every script environment.

Conceptually:

```rust
CapabilitySet {
    agent: ...,
    world: ...,
    trace: ...,
    task: ...,
}
```

Potentially expose a serializable manifest:

```json
{
  "agent": [
    "events",
    "tools",
    "stop"
  ],
  "world": [
    "fs.read",
    "fs.write",
    "exec"
  ],
  "trace": [
    "emit"
  ]
}
```

This gives:

* auditable authority;
* deterministic environments;
* easier testing;
* clearer RL task specifications;
* safer generated scripts.

Treat capabilities as part of the worker definition.

---

# Potential future WASM extension plane

Do not implement this now, but preserve room for a second extension mechanism later:

```text
pi-agent-core-rs
       │
 capability ABI
       │
 ┌─────┴─────────┐
 ▼               ▼
Luau            WASM
policy          components
```

The intended division would be:

### Luau

Use for:

```text
agent policy
hooks
orchestration
small custom tools
evaluation logic
glue
generated code
short-lived dynamic behavior
```

### WASM

Potentially use later for:

```text
large reusable extensions
third-party components
performance-sensitive tools
cross-language plugins
stronger module isolation
portable compiled code
```

Do not force Luau to become a large plugin ecosystem.

Do not force WASM into the initial milestone.

The common abstraction should remain explicit host capabilities.

---

# Why not Rhai

Do not switch to Rhai merely because it is implemented in Rust.

The primary concern is the async execution model.

This runtime is fundamentally asynchronous:

```text
model calls
tool calls
world RPC
sandboxes
other agents
```

A scripting language that naturally integrates through yielding/coroutines is a better fit than one that encourages blocking execution.

Rust purity is less important than preserving a clean async architecture.

The important invariant is:

> concurrency, resource ownership, and the agent state machine remain Rust-native.

An embedded Luau VM is acceptable.

---

# Why not QuickJS

QuickJS remains a credible alternative and should be mentioned in architecture notes, but do not choose it for the initial implementation.

Its advantages are:

```text
native JavaScript syntax
Promises
async/await
excellent model familiarity
small runtime relative to V8
```

However Luau better matches the desired architecture because:

```text
smaller language
simpler host surface
better capability-oriented embedding model
gradual typing
fewer expectations of Node/browser APIs
less temptation to recreate an application ecosystem
```

The objective is not merely:

> JavaScript without V8.

The objective is:

> an intentionally small embedded policy language.

---

# Rust ↔ Luau API design

Create a dedicated crate/layer, for example:

```text
crates/
├── pi-agent-core/
├── pi-agent-protocol/
├── pi-agent-trace/
├── pi-agent-luau/
└── pi-agent-world-api/      # only if useful
```

`pi-agent-luau` is responsible for:

```text
VM construction
sandbox initialization
capability injection
host module registration
async future/coroutine bridging
error translation
resource limits
module resolution
policy bundle loading
type declarations
```

It may depend on:

```text
pi-agent-core
mlua
tokio
```

but:

```text
pi-agent-core
```

must not depend on it.

Keep mlua types out of core public APIs.

---

# Testing strategy

Maintain the existing upstream differential parity suite for Rust core semantics.

Add a separate Luau integration suite.

Do **not** use Luau to define the parity oracle itself.

Core parity must remain testable without scripting.

Luau tests should cover:

## Basic embedding

```text
load script
call function
return structured value
script error
syntax error
module resolution
forbidden module
```

## Agent hooks

```text
before-tool hook
after-tool hook
turn hook
tool blocking
tool result transformation
stop policy
```

## Async

```text
yield on world operation
resume after completion
multiple sequential async operations
concurrent Rust operations
nested async host calls
```

## Cancellation

```text
cancel while yielded
cancel while host future runs
cancel nested callback
destroy VM with pending operations
```

## Isolation

```text
two agents cannot see each other's globals
two agents cannot see each other's capability state
no ambient HOME access
no ambient filesystem access
no environment variable access
```

## Resource limits

```text
infinite loop
deep recursion
large allocation
coroutine explosion
excessive host calls
```

## Error recovery

```text
script hook throws
tool script throws
host operation errors
subsequent agent run remains valid
```

## Swarm stress

```text
100 Luau-backed agents
1,000 Luau-backed agents
many simultaneous coroutine suspensions
high cancellation churn
rapid VM create/destroy
```

---

# Benchmarks

Separate benchmarks into:

```text
pure Rust agent overhead
Luau extension overhead
world operation overhead
model/provider overhead
```

For the Luau layer measure at least:

```text
VM startup
idle memory
policy load
hook call latency
Rust → Luau call
Luau → Rust capability call
async yield/resume
structured table serialization/conversion
VM teardown
```

Compare:

```text
pure Rust worker
Rust + Luau worker
```

Do not compare against Node initially unless useful as a sanity benchmark.

The important question is:

> how much does programmable policy cost relative to a pure native agent?

---

# Updated examples

Replace TypeScript examples with:

```text
examples/
├── rust/
│   ├── basic_agent.rs
│   ├── custom_tool.rs
│   └── swarm.rs
│
└── luau/
    ├── basic_policy.luau
    ├── custom_tool.luau
    ├── hooks.luau
    ├── orchestrator.luau
    └── swarm_policy.luau
```

Provide a small Rust launcher that loads a Luau policy bundle.

Example:

```text
pi-agent run policy.luau
```

This CLI is only a development/debugging convenience.

Do not build a TUI.

---

# First Luau end-to-end demonstration

Create a Luau policy that:

1. registers a small tool;
2. uses an injected world capability;
3. observes agent events;
4. modifies one tool call through policy;
5. runs a multi-turn agent task;
6. emits a custom trace annotation.

For example:

```lua
local agent = require("@agent")
local world = require("@world")
local trace = require("@trace")

agent.tool({
    name = "run_tests",

    schema = {
        type = "object",
        properties = {},
    },

    run = function()
        return world.exec({
            argv = {"cargo", "test"},
        })
    end,
})

agent.on("before_tool_call", function(ctx, call)
    trace.emit("tool_requested", {
        name = call.name,
    })

    return call
end)
```

The agent should execute without:

```text
Node
Deno
Bun
Python
Pi
filesystem configuration discovery
Pi session files
```

---

# Second demonstration: isolated swarm workers

Create a Rust example that instantiates:

```text
N = 100
```

agents.

A configurable subset uses Luau policy.

Each worker receives:

```text
independent Agent state
independent capability manifest
independent trace metadata
independent cancellation token
independent Luau state where applicable
```

Verify explicitly that there is no:

```text
global cwd
global settings
global session
global scripting state
implicit shared filesystem state
ambient process authority
```

This example should demonstrate the intended deployment model:

> extremely cheap, disposable, programmable native agent workers.

---

# Updated definition of done

The project is complete when:

1. The useful `pi-agent-core` runtime semantics are implemented in Rust and validated against the pinned upstream parity corpus.
2. The Rust core has no dependency on Node, TypeScript, Pi Coding Agent, or Pi's session machinery.
3. A pure Rust agent can run with no scripting VM at all.
4. Luau can be attached as an optional policy layer through `mlua`.
5. Luau uses explicit capabilities rather than ambient host access.
6. The host module surface is small and intentionally designed.
7. Model/tool/world operations can suspend Luau coroutines over asynchronous Rust futures.
8. Tokio remains the authoritative scheduler.
9. Cancellation propagates cleanly across Rust/Luau boundaries.
10. Luau scripts cannot directly violate agent-state invariants.
11. Luau tools become ordinary Rust-scheduled agent tools.
12. Resource limits prevent runaway policy scripts from monopolizing the worker.
13. Module resolution is hermetic and host-controlled.
14. No package-manager or home-directory discovery exists.
15. The trajectory recorder remains independent from scripting and agent state.
16. Capability manifests make worker authority explicit.
17. Large numbers of independent Rust/Luau-backed agents can coexist without accidental shared state.
18. Benchmarks quantify the incremental cost of adding Luau to a pure Rust worker.
19. The architecture leaves room for a future WASM extension mechanism without requiring it now.
20. No TUI or interactive coding-agent application machinery is introduced.

The intended final identity of the project is:

```text
pi-agent-core-rs
=
small proven agent state machine
+
Rust-native concurrency/resource ownership
+
explicit world capabilities
+
optional embedded Luau policy plane
+
lean native trajectory tracing
```

Do not let it evolve into a general-purpose application framework.

The target is an **agent execution microkernel** suitable for RL environments, speculative agent swarms, disposable VM worlds, and very high agent multiplicity.
