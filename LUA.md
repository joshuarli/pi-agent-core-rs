# Writing Luau extensions

`pi-agent-luau` is the optional policy plane for `pi-agent-core`. An extension
is a `.luau` source file that returns one declaration table. It can shape the
system prompt, declare model-visible tools, and make an explicit allow/block/
terminate decision before a tool runs.

It is deliberately not a second agent runtime. Rust owns model transport,
state transitions, tool scheduling, MCP/process/filesystem effects,
cancellation, and tracing. A policy never receives an ambient capability.

## Runtime and boundaries

Policies run in an isolated, JIT-enabled Luau VM (`mlua`'s `luau-jit`
backend). The VM exposes only table, string, UTF-8, math, and coroutine helper
libraries. `os`, `io`, `require`, package paths, debug APIs, host environment
variables, networking, the current directory, and wall-clock access are not
available.

The default limits are 64 KiB source, 1 MiB VM memory, and 10,000 Luau
interrupt checks for initial loading and each policy hook invocation. A host
may reduce or raise finite limits with `PolicyLimits`. An exhausted budget is a
typed policy failure; it does not grant a fallback capability.

Policy hooks in this first slice are synchronous. Do not use a coroutine to
perform work or expect `require`/host modules to appear. Asynchronous host
capabilities are a later V1 addition and will receive their own explicit ABI.

## Minimal policy

```luau
-- runebench-policy.luau
return {
    system_prompt_append = [[
Use the game tools deliberately. Inspect state before starting a long action.
]],

    tools = {
        {
            name = "execute_code",
            description = "Execute a JavaScript game script for a named bot.",
            capability = "rs-agent",
            execution_mode = "sequential",
            schema_json = [[
                {"type":"object","required":["bot_name","code"],"properties":{
                    "bot_name":{"type":"string"},
                    "code":{"type":"string"}
                },"additionalProperties":false}
            ]],
        },
    },

    before_tool_call = function(call)
        if call.name == "execute_code" then
            return "allow"
        end
        return {
            action = "block",
            reason = "this policy does not grant that game operation",
        }
    end,
}
```

The returned value must be a table with a string `system_prompt_append`.
`tools` and `before_tool_call` are optional. Tool declaration order is retained
for the model-facing registry.

## Tool declarations do not grant authority

Each entry in `tools` has these required fields:

| Field | Meaning |
| --- | --- |
| `name` | Model-visible, unique tool name. |
| `description` | Model-facing explanation. |
| `schema_json` | A JSON Schema encoded as a string. It is parsed at the Rust boundary. |
| `capability` | A host-owned capability name, such as `rs-agent`. |
| `execution_mode` | Exactly `"sequential"` or `"parallel"`. |

The host must explicitly bind a declaration's `capability` to an ordinary Rust
`AgentTool`. If it does not bind it, the tool must not be registered. This
prevents an extension from obtaining MCP, process, filesystem, or network
authority by naming it in a table.

Use `"sequential"` whenever calls have side effects on shared state, including
game bots. Use `"parallel"` only when the host capability's contract says
overlap is safe.

## Pre-tool decisions

When present, `before_tool_call` receives a table:

```luau
function(call)
    -- call.id: opaque call ID
    -- call.name: registered tool name
    -- call.arguments_json: exact JSON arguments from the model
end
```

It must return exactly one of:

```luau
"allow"
{ action = "block", reason = "explain the denial" }
{ action = "terminate", reason = "explain why this run must end" }
```

An allow decision then proceeds to the embedding host's normal hook chain. A
block or terminate decision stops there: the host hook and tool implementation
are not called. Policies cannot rewrite arguments, fabricate a tool result, or
call a tool directly.

## Rust host integration

The host reads a policy, binds only known capabilities, appends the policy
prompt after the pinned core system prompt, and composes the policy hook before
its provider hook:

```rust
use std::sync::Arc;

use pi_agent_luau::{LuaPolicy, LuaPolicyHookSet};

let policy = Arc::new(LuaPolicy::load(&source)?);
for tool in policy.tools() {
    if tool.capability == "rs-agent" {
        // Register a Rust AgentTool that implements exactly this declaration.
        // Do not create a capability for an unrecognized name.
    }
}
let hooks = Arc::new(LuaPolicyHookSet::new(policy, provider_hooks));
```

The wrapper delegates provider-context conversion and all hooks other than
`before_tool_call` to `provider_hooks`. This permits a Lua policy to constrain
effects without embedding provider-specific protocol code in the policy VM.

## Extension review checklist

- Keep `system_prompt_append` task-specific and avoid embedding credentials.
- Declare the smallest set of tools and use schemas that reject unexpected
  fields when the underlying capability has a narrow contract.
- Give every denial a model-actionable reason.
- Treat `call.arguments_json` as untrusted model text; never parse it by
  string matching to grant an effect.
- Add Rust integration coverage for each new capability binding and for its
  allow, block, and error paths.
- Expanding this surface—new hooks, host modules, async calls, or tool
  handlers—requires a versioned Rust contract, limit/cancellation behavior,
  documentation here, and tests. It is not an extension-only change.
