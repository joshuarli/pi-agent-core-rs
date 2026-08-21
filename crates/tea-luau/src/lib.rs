//! Hermetic Luau policy support for tea-core.
//!
//! A policy declares prompt additions, prompt-facing tool definitions, and a
//! narrow pre-tool decision. It cannot acquire ambient process, network, file,
//! or MCP authority; a host binds each declared capability explicitly.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Caller-driven coroutine support for explicit asynchronous capabilities.
pub mod async_runtime;
/// Closed, deterministic source bundles and their manifests.
pub mod bundle;
/// Per-VM execution of closed bundle-local Luau modules.
pub mod bundle_runtime;
/// Versioned, capability-scoped extension ABI values and host gates.
pub mod capability;
/// Coroutine-backed Luau tool handlers adapted to the core tool scheduler.
pub mod tool_handler;

mod policy;
pub use policy::{LuaPolicy, LuaPolicyHookSet, PolicyError, PolicyLimits, PolicyTool};

#[cfg(test)]
mod tests {
    use super::{LuaPolicy, LuaPolicyHookSet, PolicyError, PolicyLimits};
    use crate::bundle::{Bundle, BundleManifest, BUNDLE_ABI_VERSION};
    use tea_core::error::HookError;
    use tea_core::hooks::{AfterToolCall, BeforeToolCall, ContextEnvelope, HookSet};
    use tea_core::state::{SerializedJson, ToolCallId};
    use tea_core::tool::{ToolCall, ToolResult};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const GAME_POLICY: &str = r#"
        return {
            system_prompt_append = "Use game tools deliberately.",
            tools = {
                {
                    name = "execute_code",
                    description = "Execute a game script.",
                    capability = "rs-agent",
                    execution_mode = "sequential",
                    schema_json = '{"type":"object","required":["code"]}',
                },
            },
            before_tool_call = function(call)
                if call.name == "execute_code" then
                    return "allow"
                end
                return { action = "block", reason = "not granted by game policy" }
            end,
        }
    "#;

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(format!("call-{name}")).expect("test IDs are non-empty"),
            name: name.to_owned(),
            arguments: SerializedJson::new("{}"),
        }
    }

    #[test]
    fn policy_declares_prompt_tools_and_pre_tool_boundary() {
        let policy = LuaPolicy::load(GAME_POLICY).expect("policy should load");

        assert_eq!(
            policy.system_prompt_append(),
            "Use game tools deliberately."
        );
        assert_eq!(policy.tools().len(), 1);
        assert_eq!(policy.tools()[0].name, "execute_code");
        assert_eq!(policy.tools()[0].capability, "rs-agent");
        assert_eq!(
            policy.before_tool_call(&call("execute_code")),
            Ok(BeforeToolCall::Allow)
        );
        assert_eq!(
            policy.before_tool_call(&call("read_resource")),
            Ok(BeforeToolCall::Block {
                reason: "not granted by game policy".to_owned(),
            })
        );
    }

    #[test]
    fn luau_syntax_runs_in_the_jit_enabled_policy_vm() {
        let policy = LuaPolicy::load(
            r#"
                local permitted: boolean = true
                return {
                    system_prompt_append = if permitted then "Luau policy" else "unreachable",
                    before_tool_call = function(_) return "allow" end,
                }
            "#,
        )
        .expect("Luau type annotations and if-expressions should compile");

        assert_eq!(policy.system_prompt_append(), "Luau policy");
        assert_eq!(
            policy.before_tool_call(&call("execute_code")),
            Ok(BeforeToolCall::Allow)
        );
    }

    #[test]
    fn policy_bundle_resolves_only_its_closed_relative_module_graph() {
        let bundle = Bundle::from_sources(
            BundleManifest::new(BUNDLE_ABI_VERSION, "main.luau", std::iter::empty::<&str>())
                .expect("manifest is valid"),
            [
                (
                    "main.luau",
                    r#"
                        local prompt = require("./parts/prompt.luau")
                        return {
                            system_prompt_append = prompt,
                            before_tool_call = function(_) return "allow" end,
                        }
                    "#,
                ),
                ("parts/prompt.luau", "return 'closed bundle policy'"),
            ],
        )
        .expect("closed bundle is valid");

        let policy = LuaPolicy::load_bundle(bundle).expect("closed bundle should load");
        assert_eq!(policy.system_prompt_append(), "closed bundle policy");
        assert_eq!(
            policy.before_tool_call(&call("tool")),
            Ok(BeforeToolCall::Allow)
        );
    }

    #[test]
    fn policy_bundle_applies_source_limit_to_every_module() {
        let bundle = Bundle::from_sources(
            BundleManifest::new(BUNDLE_ABI_VERSION, "main.luau", std::iter::empty::<&str>())
                .expect("manifest is valid"),
            [
                ("main.luau", "return require('./prompt.luau')"),
                (
                    "prompt.luau",
                    "return { system_prompt_append = 'large enough to exceed limit' }",
                ),
            ],
        )
        .expect("closed bundle is valid");
        let result = LuaPolicy::load_bundle_with_limits(
            bundle,
            PolicyLimits {
                max_source_bytes: 8,
                ..PolicyLimits::default()
            },
        );
        let error = match result {
            Ok(_) => panic!("a non-entrypoint source cannot evade the aggregate bound"),
            Err(error) => error,
        };
        assert!(matches!(error, PolicyError::SourceTooLarge { .. }));
    }

    #[test]
    fn sandbox_has_no_ambient_os_or_module_loader() {
        for source in [
            "return { system_prompt_append = os.time() }",
            "return { system_prompt_append = require('filesystem') }",
        ] {
            assert!(
                LuaPolicy::load(source).is_err(),
                "source should be rejected: {source}"
            );
        }
    }

    #[test]
    fn interrupt_budget_terminates_an_unbounded_hook() {
        let policy = LuaPolicy::load_with_limits(
            r#"
                return {
                    system_prompt_append = "",
                    before_tool_call = function(_) while true do end end,
                }
            "#,
            PolicyLimits {
                max_source_bytes: 4 * 1024,
                max_memory_bytes: 1024 * 1024,
                max_interrupt_checks: 2,
            },
        )
        .expect("policy source should load without executing the hook");

        let error = policy
            .before_tool_call(&call("execute_code"))
            .expect_err("unbounded hook must be interrupted");
        assert!(matches!(error, PolicyError::Runtime { .. }));
    }

    #[test]
    fn source_limit_is_checked_before_vm_evaluation() {
        let error = LuaPolicy::load_with_limits(
            GAME_POLICY,
            PolicyLimits {
                max_source_bytes: 8,
                ..PolicyLimits::default()
            },
        )
        .err()
        .expect("oversized source should not enter the VM");

        assert!(matches!(error, PolicyError::SourceTooLarge { .. }));
    }

    #[test]
    fn duplicate_tool_names_are_rejected_before_host_binding() {
        let error = LuaPolicy::load(
            r#"
                return {
                    system_prompt_append = "",
                    tools = {
                        {
                            name = "inspect",
                            description = "First declaration.",
                            capability = "world",
                            execution_mode = "parallel",
                            schema_json = "{}",
                        },
                        {
                            name = "inspect",
                            description = "Second declaration.",
                            capability = "world",
                            execution_mode = "parallel",
                            schema_json = "{}",
                        },
                    },
                }
            "#,
        )
        .err()
        .expect("a policy must not shadow a tool binding");

        assert!(matches!(error, PolicyError::Contract { .. }));
    }

    #[test]
    fn policy_retains_optional_tool_handler_source_without_granting_authority() {
        let policy = LuaPolicy::load(
            r#"
                return {
                    system_prompt_append = "",
                    tools = {
                        {
                            name = "world_echo",
                            description = "Echo through an explicit host capability.",
                            capability = "world",
                            execution_mode = "sequential",
                            schema_json = "{}",
                            handler_source = "return function(call) return call.arguments_json end",
                        },
                    },
                }
            "#,
        )
        .expect("a declaration may retain handler source");

        assert_eq!(
            policy.tools()[0].handler_source.as_deref(),
            Some("return function(call) return call.arguments_json end")
        );
        let error = match LuaPolicy::load(
            r#"
                return {
                    system_prompt_append = "",
                    tools = {{
                        name = "empty_handler",
                        description = "Invalid handler.",
                        capability = "world",
                        execution_mode = "sequential",
                        schema_json = "{}",
                        handler_source = "  ",
                    }},
                }
            "#,
        ) {
            Ok(_) => panic!("an explicitly empty handler has no executable contract"),
            Err(error) => error,
        };
        assert!(matches!(error, PolicyError::Contract { .. }));
    }

    #[test]
    fn policy_denial_does_not_reach_the_host_hook() {
        let policy = Arc::new(LuaPolicy::load(GAME_POLICY).expect("policy should load"));
        let calls = Arc::new(AtomicUsize::new(0));
        let hooks = LuaPolicyHookSet::new(
            policy,
            Arc::new(CountingHooks {
                before_calls: Arc::clone(&calls),
            }),
        );

        assert!(matches!(
            hooks.before_tool_call(&call("read_resource")),
            Ok(BeforeToolCall::Block { .. })
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(
            hooks.before_tool_call(&call("execute_code")),
            Ok(BeforeToolCall::Allow)
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    struct CountingHooks {
        before_calls: Arc<AtomicUsize>,
    }

    impl HookSet for CountingHooks {
        fn before_tool_call(&self, _call: &ToolCall) -> Result<BeforeToolCall, HookError> {
            self.before_calls.fetch_add(1, Ordering::Relaxed);
            Ok(BeforeToolCall::Allow)
        }

        fn after_tool_call(
            &self,
            _call: &ToolCall,
            _result: &ToolResult,
        ) -> Result<AfterToolCall, HookError> {
            Ok(AfterToolCall::default())
        }

        fn transform_context(
            &self,
            context: ContextEnvelope,
        ) -> Result<ContextEnvelope, HookError> {
            Ok(context)
        }

        fn convert_to_llm(&self, _context: ContextEnvelope) -> Result<String, HookError> {
            Ok("[]".to_owned())
        }
    }
}
