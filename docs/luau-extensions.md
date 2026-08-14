# Writing Luau extensions

`pi-agent-luau` is the optional policy plane for `pi-agent-core`. An extension
is a `.luau` file returning one declaration table. It can append task-specific
system instructions, declare model-visible tools, and make an explicit
allow/block/terminate decision before a tool runs.

It is not a second agent runtime. Rust owns model transport, state
transitions, scheduling, filesystem/process/MCP effects, cancellation, and
tracing. A policy receives no ambient authority.

## Runtime boundary

Policies run in an isolated JIT-enabled Luau VM through `mlua`'s `luau-jit`
backend. The exposed environment is limited to table, string, UTF-8, math,
and coroutine helpers. There is no `os`, `io`, `require`, package path, debug
API, environment variable, network, current-directory, or wall-clock access.

The current policy ABI is synchronous and declarative. Its default limits are
64 KiB source, 1 MiB VM memory, and 10,000 interrupt checks during loading and
each hook. A host may choose finite `PolicyLimits`; exhaustion is a typed
failure and never grants fallback authority. Async host modules, richer hooks,
and module resolution are future V1 work described in [`V1.md`](../V1.md).

## Minimal policy

```luau
return {
    system_prompt_append = [[
Use the world tools deliberately. Inspect state before long actions.
]],

    tools = {
        {
            name = "execute_code",
            description = "Execute a game script for a named bot.",
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
        return { action = "block", reason = "tool is not granted by this policy" }
    end,
}
```

The returned value must be a table with a string `system_prompt_append`.
`tools` and `before_tool_call` are optional. Declaration order is retained in
the model-facing registry.

## Declaring a tool does not grant it

Every declaration supplies a unique model-visible `name`, `description`, JSON
Schema string in `schema_json`, host-owned `capability`, and either
`"sequential"` or `"parallel"` execution mode. The embedding must explicitly
bind that capability to a Rust `AgentTool`. An unbound declaration is rejected;
naming `rs-agent`, a shell, filesystem, MCP server, or network operation in
Luau never creates that authority.

Use sequential execution for shared side effects. Only choose parallel when
the Rust capability explicitly permits overlapping calls.

## Pre-tool decisions

`before_tool_call` receives a table with opaque `id`, registered `name`, and
the exact model-provided `arguments_json`. It must return exactly one of:

```luau
"allow"
{ action = "block", reason = "model-actionable explanation" }
{ action = "terminate", reason = "explain why the run must stop" }
```

Block and terminate decisions prevent both the following host hook and the
tool implementation from running. Policies cannot rewrite arguments, fabricate
tool results, call a tool directly, or mutate agent state.

## Host integration

Load a policy, bind a closed capability set, append its prompt after the
pinned core prompt, and compose its hook before the provider hook:

```rust,no_run
use std::sync::Arc;

use pi_agent_luau::{LuaPolicy, LuaPolicyHookSet};

# fn example(source: String, provider_hooks: Arc<dyn pi_agent_core::hooks::HookSet>) -> Result<(), Box<dyn std::error::Error>> {
let policy = Arc::new(LuaPolicy::load(&source)?);
for tool in policy.tools() {
    if tool.capability == "rs-agent" {
        // Register only a Rust AgentTool with this exact declared authority.
    }
}
let hooks = Arc::new(LuaPolicyHookSet::new(policy, provider_hooks));
# let _ = hooks;
# Ok(())
# }
```

The wrapper delegates provider-context conversion and every unsupported hook to
the supplied Rust hook set. This keeps provider protocol code and effectful
world capabilities outside the policy VM.

## Review checklist

- Keep prompt additions task-specific and credential-free.
- Grant the smallest tool set and reject unexpected schema fields.
- Make denials actionable for the model.
- Treat `arguments_json` as untrusted model text; do not grant effects through
  string matching.
- Add integration coverage for every capability binding and its allow, block,
  cancellation, and error paths.
- Any new host module, async call, hook, or tool-handler behavior needs a
  versioned Rust contract, limits/cancellation rule, tests, and documentation.
