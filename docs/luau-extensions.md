# Writing Luau extensions

`pi-agent-luau` is an optional, hermetic policy plane for
`pi-agent-core-rs`. It is for task- and world-specific policy, not a second
agent runtime: Rust retains control of model transport, the state machine,
tool scheduler, cancellations, tracing, resource ownership, and every side
effect.

Use the checked-in nightly. The embedded engine is `mlua`'s `luau-jit`
backend; it is intentionally not LuaJIT 5.2. An embedding normally drives its
agent and any Luau futures using Smol. Tokio is unsupported.

## What a policy can do

A policy source or bundle entrypoint returns a declaration table. It can:

- append task-specific text to a host-owned system prompt;
- describe model-visible tools and their JSON schemas;
- allow, block, or terminate before a model tool call; and
- provide coroutine handler source for a host to adapt to an ordinary Rust
  `AgentTool`.

A policy cannot discover files, read environment variables, run processes,
open a network connection, load packages, use a wall clock, modify core
state, schedule an agent, or acquire a capability by naming it.

## Minimal declaration

```luau
return {
    system_prompt_append = [[
Inspect the world before acting. Keep world calls narrow and deliberate.
]],

    tools = {
        {
            name = "inspect_world",
            description = "Read a small host-provided world snapshot.",
            capability = "world",
            execution_mode = "sequential",
            schema_json = [[{"type":"object","additionalProperties":false}]],
        },
    },

    before_tool_call = function(call)
        if call.name == "inspect_world" then
            return "allow"
        end
        return { action = "block", reason = "this policy did not grant that tool" }
    end,
}
```

`system_prompt_append` is required. `tools` and `before_tool_call` are
optional. Each tool needs a unique non-empty `name`, `description`,
`capability`, `schema_json`, and `execution_mode` (`"sequential"` or
`"parallel"`). `schema_json` is parsed and validated by Rust before a model
can call the tool.

`before_tool_call` receives opaque `id`, model-facing `name`, and exact
`arguments_json`. It must return one of:

```luau
"allow"
{ action = "block", reason = "model-actionable explanation" }
{ action = "terminate", reason = "explain why the run must stop" }
```

The decision is made before the host hook and tool implementation. It cannot
rewrite arguments, fabricate a result, or invoke an effect directly.

## Write a capability-backed tool handler

Add `handler_source` when the model-facing tool should invoke an explicit host
capability. It is a string whose value evaluates to a function. The function
runs in a fresh VM for each tool invocation and receives:

```luau
{ id = "opaque-call-id", name = "tool-name", arguments_json = "{...}" }
```

The only suspension protocol is a yielded capability request:

```luau
local world_handler = [[
return function(call)
    local result = coroutine.yield({
        kind = "capability",
        capability = "world",
        method = "inspect",
        arguments_json = call.arguments_json,
    })
    return {
        content = result.content,
        details_json = result.details_json,
        is_error = result.is_error,
    }
end
]]

return {
    system_prompt_append = "",
    tools = {
        {
            name = "inspect_world",
            description = "Read a small host-provided world snapshot.",
            capability = "world",
            execution_mode = "sequential",
            schema_json = [[{"type":"object","additionalProperties":false}]],
            handler_source = world_handler,
        },
    },
}
```

The Rust embedding must construct `LuaToolHandler` with matching
`ToolHandlerSpec` and an explicit `CapabilityBindings` entry. The handler
rejects a yielded capability other than the declared one. The capability
implementation must validate `method` and parsed JSON itself; a shared
capability should additionally bind it to the outer model-visible tool name.
Runebench demonstrates that pattern with an MCP manifest scoped to exact
server/method/target triples.

On success, return either a string or a result table containing `content` and
optional `details_json`, `is_error`, and `terminate`. `details_json` must be
valid JSON. A handler may make at most `HandlerLimits::max_capability_calls`
host calls (64 by default).

## Bundle-local modules

For a multi-file policy, build `bundle::Bundle` in the embedding from explicit
source records and call `LuaPolicy::load_bundle`. There is deliberately no
filesystem bundle loader in the crate.

```luau
-- main.luau
local prompt = require("./parts/prompt.luau")
return { system_prompt_append = prompt }
```

Only `./...` and `../...` imports are accepted, and they must stay inside the
declared bundle. Bare names, absolute paths, drive paths, package registries,
and virtual modules are denied. Each VM has its own module cache; bundle
source hashes are deterministic identities, not cryptographic digests.

## Host capability manifests

`capability::CapabilityManifest` is the host-facing, serializable ABI-v1
authority description. Its typed modules are `@agent`, `@world`, `@trace`,
`@task`, `@json`, and `@time`; an MCP permission can be scoped to an exact
server, method, and tool/resource target. Use `CapabilityGate` before an
effectful provider. A manifest does not install globals or effects into Luau;
the embedding still chooses a concrete `LuauCapability` binding.

This separation is intentional. Do not invent `require("@world")` or other
ambient capability modules in a policy unless its embedding documents and
installs that exact interface. The baseline bundle loader rejects it.

## Async work and cancellation

`async_runtime` is available to an embedding that needs a generic Luau
coroutine outside the normal tool scheduler. It installs
`await(capability, arguments_json)` and returns a caller-polled `LuauTask`.
The host's `HostAwaiter` owns the future and uses the supplied
`CancellationToken`; cancellation drops a pending host future and settles the
task as a typed cancellation. It neither starts an executor nor spawns a
thread.

Tool handlers already use the core scheduler and should normally be preferred
for model-visible effects.

## Limits and review checklist

Policies and handlers have host-selected finite source, memory, and Luau
interrupt budgets. Handler calls also have a finite host-call budget. The
current defaults are 64 KiB source, 1 MiB VM memory, 10,000 interrupt checks,
and 64 capability calls per handler invocation. A fresh VM per handler call
means a handler cannot leak a coroutine or mutable global into another call.

- Treat `arguments_json` as hostile model input and validate it structurally.
- Grant the smallest model tool set and exact host methods/targets.
- Make block reasons useful to the model but free of secrets.
- Test an allowed call, denied method, invalid arguments, host error, and
  cancellation for every new binding.
- Keep host effects in Rust. A policy declaration or manifest string is never
  authority.
- Do not place credentials in policy text, a prompt suffix, handler source, or
  tool environment.

For crate ownership and benchmark/test evidence see
[architecture](architecture.md), [verification](verification.md), and
[V1](../V1.md).
