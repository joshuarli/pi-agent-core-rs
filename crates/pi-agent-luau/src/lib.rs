//! Hermetic Luau policy support for `pi-agent-core`.
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

use crate::bundle::Bundle;
use crate::bundle_runtime::BundleRuntime;
use mlua::{Function, Lua, LuaOptions, StdLib, Table, Value, VmState};
use pi_agent_core::error::HookError;
use pi_agent_core::hooks::{
    AfterToolCall, BeforeToolCall, ContextEnvelope, HookFuture, HookSet, NextTurn,
};
use pi_agent_core::scheduler::CancellationToken;
use pi_agent_core::tool::{ToolCall, ToolExecutionMode, ToolResult};
use pi_agent_protocol::JsonValue;
use std::collections::BTreeSet;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const POLICY_CHUNK_NAME: &str = "pi-agent-policy.luau";

/// Resource limits applied to one Lua policy virtual machine.
///
/// `max_interrupt_checks` bounds initial policy evaluation and each hook
/// invocation separately. Luau invokes the interrupt handler at loop and
/// function-call boundaries, so the value is a deterministic host budget
/// rather than an exact instruction count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolicyLimits {
    /// Largest accepted policy source in bytes.
    pub max_source_bytes: usize,
    /// Largest Luau VM allocation total in bytes.
    pub max_memory_bytes: usize,
    /// Largest number of Luau interrupt checks permitted per evaluation.
    pub max_interrupt_checks: usize,
}

impl Default for PolicyLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 64 * 1024,
            max_memory_bytes: 1024 * 1024,
            max_interrupt_checks: 10_000,
        }
    }
}

/// A prompt-facing tool declared by a policy but not granted any authority.
#[derive(Clone, Debug, PartialEq)]
pub struct PolicyTool {
    /// Stable tool name sent to the model.
    pub name: String,
    /// Prompt-facing explanation of the tool.
    pub description: String,
    /// JSON Schema for the tool arguments.
    pub schema: JsonValue,
    /// Host-owned capability name that must be explicitly bound by an embedder.
    pub capability: String,
    /// Whether the core may overlap calls to this tool.
    pub execution_mode: ToolExecutionMode,
    /// Optional self-contained Luau source for this tool's coroutine handler.
    ///
    /// The source must evaluate to a function accepted by
    /// [`tool_handler::LuaToolHandler`]. It remains inert until an embedding
    /// deliberately adapts it into an explicit Rust capability; declaring a
    /// handler never grants a world effect by itself.
    pub handler_source: Option<String>,
}

/// A loaded, sandboxed Luau policy.
pub struct LuaPolicy {
    runtime: Mutex<PolicyRuntime>,
    system_prompt_append: String,
    tools: Vec<PolicyTool>,
}

struct PolicyRuntime {
    lua: Lua,
    before_tool_call: Option<Function>,
    interrupt_budget: Arc<AtomicUsize>,
    max_interrupt_checks: usize,
}

/// A policy loading or evaluation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyError {
    /// The source exceeds the configured boundary before entering the VM.
    SourceTooLarge {
        /// Source length in bytes.
        actual: usize,
        /// Configured maximum length in bytes.
        limit: usize,
    },
    /// A configured VM resource limit is zero.
    InvalidLimit {
        /// Stable configuration field name.
        field: &'static str,
    },
    /// The extension failed to meet the policy-table contract.
    Contract {
        /// Searchable explanation of the mismatch.
        message: String,
    },
    /// The Luau VM rejected or interrupted evaluation.
    Runtime {
        /// Host-safe diagnostic from the Luau VM.
        message: String,
    },
}

impl fmt::Display for PolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceTooLarge { actual, limit } => {
                write!(
                    formatter,
                    "policy source is {actual} bytes, exceeding {limit} bytes"
                )
            }
            Self::InvalidLimit { field } => {
                write!(formatter, "policy limit {field} must be non-zero")
            }
            Self::Contract { message } => write!(formatter, "invalid policy contract: {message}"),
            Self::Runtime { message } => write!(formatter, "Luau policy failed: {message}"),
        }
    }
}

impl std::error::Error for PolicyError {}

impl LuaPolicy {
    /// Load a policy with the conservative default VM limits.
    pub fn load(source: &str) -> Result<Self, PolicyError> {
        Self::load_with_limits(source, PolicyLimits::default())
    }

    /// Load a policy with host-selected, finite resource limits.
    pub fn load_with_limits(source: &str, limits: PolicyLimits) -> Result<Self, PolicyError> {
        validate_limits(limits)?;
        if source.len() > limits.max_source_bytes {
            return Err(PolicyError::SourceTooLarge {
                actual: source.len(),
                limit: limits.max_source_bytes,
            });
        }

        let lua = Lua::new_with(
            StdLib::COROUTINE | StdLib::TABLE | StdLib::STRING | StdLib::UTF8 | StdLib::MATH,
            LuaOptions::new(),
        )
        .map_err(runtime_error)?;
        lua.set_memory_limit(limits.max_memory_bytes)
            .map_err(runtime_error)?;
        lua.enable_jit(true);
        // Luau makes global tables read-only and isolates script globals. This is
        // in addition to omitting ambient I/O, OS, package, and debug libraries.
        lua.sandbox(true).map_err(runtime_error)?;

        let interrupt_budget = Arc::new(AtomicUsize::new(limits.max_interrupt_checks));
        let interrupt_counter = Arc::clone(&interrupt_budget);
        lua.set_interrupt(move |_| {
            if interrupt_counter
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_err()
            {
                return Err(mlua::Error::RuntimeError(
                    "Luau policy interrupt budget exhausted".to_owned(),
                ));
            }
            Ok(VmState::Continue)
        });

        let declaration: Table = lua
            .load(source)
            .set_name(POLICY_CHUNK_NAME)
            .eval()
            .map_err(runtime_error)?;
        let (system_prompt_append, tools, before_tool_call) = parse_declaration(&declaration)?;

        Ok(Self {
            runtime: Mutex::new(PolicyRuntime {
                lua,
                before_tool_call,
                interrupt_budget,
                max_interrupt_checks: limits.max_interrupt_checks,
            }),
            system_prompt_append,
            tools,
        })
    }

    /// Load a closed multi-module policy bundle with the default VM limits.
    ///
    /// The bundle entrypoint must return the same declaration table accepted
    /// by [`Self::load`]. Its `require` function can resolve only explicit
    /// bundle-local `./` and `../` imports; it cannot load virtual modules,
    /// host files, packages, or network resources.
    pub fn load_bundle(bundle: Bundle) -> Result<Self, PolicyError> {
        Self::load_bundle_with_limits(bundle, PolicyLimits::default())
    }

    /// Load a closed multi-module policy bundle with host-selected limits.
    ///
    /// `max_source_bytes` applies to the aggregate UTF-8 bytes of every
    /// bundle module, not only the entrypoint. This prevents dormant modules
    /// from evading the source-size boundary.
    pub fn load_bundle_with_limits(
        bundle: Bundle,
        limits: PolicyLimits,
    ) -> Result<Self, PolicyError> {
        validate_limits(limits)?;
        let source_bytes = bundle.modules().values().try_fold(0usize, |total, source| {
            total.checked_add(source.len()).ok_or(())
        });
        let source_bytes = source_bytes.unwrap_or(usize::MAX);
        if source_bytes > limits.max_source_bytes {
            return Err(PolicyError::SourceTooLarge {
                actual: source_bytes,
                limit: limits.max_source_bytes,
            });
        }

        let lua = Lua::new_with(
            StdLib::COROUTINE | StdLib::TABLE | StdLib::STRING | StdLib::UTF8 | StdLib::MATH,
            LuaOptions::new(),
        )
        .map_err(runtime_error)?;
        lua.set_memory_limit(limits.max_memory_bytes)
            .map_err(runtime_error)?;
        lua.enable_jit(true);

        let bundle_runtime = BundleRuntime::new(bundle);
        bundle_runtime
            .install(&lua)
            .map_err(|error| PolicyError::Runtime {
                message: error.to_string(),
            })?;
        // Luau makes global tables read-only and isolates script globals. This is
        // in addition to omitting ambient I/O, OS, package, and debug libraries.
        lua.sandbox(true).map_err(runtime_error)?;

        let interrupt_budget = Arc::new(AtomicUsize::new(limits.max_interrupt_checks));
        let interrupt_counter = Arc::clone(&interrupt_budget);
        lua.set_interrupt(move |_| {
            if interrupt_counter
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_err()
            {
                return Err(mlua::Error::RuntimeError(
                    "Luau policy interrupt budget exhausted".to_owned(),
                ));
            }
            Ok(VmState::Continue)
        });

        let declaration =
            match bundle_runtime
                .eval_entrypoint(&lua)
                .map_err(|error| PolicyError::Runtime {
                    message: error.to_string(),
                })? {
                Value::Table(declaration) => declaration,
                _ => {
                    return Err(PolicyError::Contract {
                        message: "bundle entrypoint must return a policy declaration table"
                            .to_owned(),
                    });
                }
            };
        let (system_prompt_append, tools, before_tool_call) = parse_declaration(&declaration)?;

        Ok(Self {
            runtime: Mutex::new(PolicyRuntime {
                lua,
                before_tool_call,
                interrupt_budget,
                max_interrupt_checks: limits.max_interrupt_checks,
            }),
            system_prompt_append,
            tools,
        })
    }

    /// Return text the host may append after its pinned system prompt.
    pub fn system_prompt_append(&self) -> &str {
        &self.system_prompt_append
    }

    /// Return the ordered, authority-free tool declarations.
    pub fn tools(&self) -> &[PolicyTool] {
        &self.tools
    }

    /// Evaluate the optional pre-tool decision without granting the policy an effect.
    pub fn before_tool_call(&self, call: &ToolCall) -> Result<BeforeToolCall, PolicyError> {
        let runtime = self.runtime.lock().map_err(|_| PolicyError::Runtime {
            message: "policy VM lock was poisoned".to_owned(),
        })?;
        let Some(function) = runtime.before_tool_call.as_ref() else {
            return Ok(BeforeToolCall::Allow);
        };
        runtime
            .interrupt_budget
            .store(runtime.max_interrupt_checks, Ordering::Relaxed);
        let call_table = runtime.lua.create_table().map_err(runtime_error)?;
        call_table
            .set("id", call.id.as_str())
            .map_err(runtime_error)?;
        call_table
            .set("name", call.name.as_str())
            .map_err(runtime_error)?;
        call_table
            .set("arguments_json", call.arguments.as_str())
            .map_err(runtime_error)?;
        let decision = function.call::<Value>(call_table).map_err(runtime_error)?;
        parse_decision(decision)
    }
}

/// A hook adapter that gives a Lua policy the first, narrow pre-tool decision.
///
/// All other hook methods—including provider-context conversion—remain owned
/// by the embedding host. A denied call never reaches the wrapped hook set.
#[derive(Clone)]
pub struct LuaPolicyHookSet {
    policy: Arc<LuaPolicy>,
    inner: Arc<dyn HookSet>,
}

impl LuaPolicyHookSet {
    /// Compose a loaded policy with the host's provider and lifecycle hooks.
    pub fn new(policy: Arc<LuaPolicy>, inner: Arc<dyn HookSet>) -> Self {
        Self { policy, inner }
    }
}

impl HookSet for LuaPolicyHookSet {
    fn before_tool_call(&self, call: &ToolCall) -> Result<BeforeToolCall, HookError> {
        match self
            .policy
            .before_tool_call(call)
            .map_err(before_hook_error)?
        {
            BeforeToolCall::Allow => self.inner.before_tool_call(call),
            decision => Ok(decision),
        }
    }

    fn after_tool_call(
        &self,
        call: &ToolCall,
        result: &ToolResult,
    ) -> Result<AfterToolCall, HookError> {
        self.inner.after_tool_call(call, result)
    }

    fn transform_context(&self, context: ContextEnvelope) -> Result<ContextEnvelope, HookError> {
        self.inner.transform_context(context)
    }

    fn convert_to_llm(&self, context: ContextEnvelope) -> Result<String, HookError> {
        self.inner.convert_to_llm(context)
    }

    fn should_stop_after_turn(&self, context: &ContextEnvelope) -> Result<bool, HookError> {
        self.inner.should_stop_after_turn(context)
    }

    fn prepare_next_turn(&self, context: ContextEnvelope) -> Result<NextTurn, HookError> {
        self.inner.prepare_next_turn(context)
    }

    fn before_tool_call_async<'a>(
        &'a self,
        call: &'a ToolCall,
        context: ContextEnvelope,
        cancellation: CancellationToken,
    ) -> HookFuture<'a, BeforeToolCall> {
        match self
            .policy
            .before_tool_call(call)
            .map_err(before_hook_error)
        {
            Ok(BeforeToolCall::Allow) => {
                self.inner
                    .before_tool_call_async(call, context, cancellation)
            }
            Ok(decision) => Box::pin(std::future::ready(Ok(decision))),
            Err(error) => Box::pin(std::future::ready(Err(error))),
        }
    }

    fn after_tool_call_async<'a>(
        &'a self,
        call: &'a ToolCall,
        result: &'a ToolResult,
        context: ContextEnvelope,
        cancellation: CancellationToken,
    ) -> HookFuture<'a, AfterToolCall> {
        self.inner
            .after_tool_call_async(call, result, context, cancellation)
    }

    fn transform_context_async<'a>(
        &'a self,
        context: ContextEnvelope,
        cancellation: CancellationToken,
    ) -> HookFuture<'a, ContextEnvelope> {
        self.inner.transform_context_async(context, cancellation)
    }

    fn convert_to_llm_async<'a>(
        &'a self,
        context: ContextEnvelope,
        cancellation: CancellationToken,
    ) -> HookFuture<'a, String> {
        self.inner.convert_to_llm_async(context, cancellation)
    }

    fn should_stop_after_turn_async<'a>(
        &'a self,
        context: &'a ContextEnvelope,
        cancellation: CancellationToken,
    ) -> HookFuture<'a, bool> {
        self.inner
            .should_stop_after_turn_async(context, cancellation)
    }

    fn prepare_next_turn_async<'a>(
        &'a self,
        context: ContextEnvelope,
        cancellation: CancellationToken,
    ) -> HookFuture<'a, NextTurn> {
        self.inner.prepare_next_turn_async(context, cancellation)
    }
}

fn validate_limits(limits: PolicyLimits) -> Result<(), PolicyError> {
    for (field, value) in [
        ("max_source_bytes", limits.max_source_bytes),
        ("max_memory_bytes", limits.max_memory_bytes),
        ("max_interrupt_checks", limits.max_interrupt_checks),
    ] {
        if value == 0 {
            return Err(PolicyError::InvalidLimit { field });
        }
    }
    Ok(())
}

fn parse_declaration(
    declaration: &Table,
) -> Result<(String, Vec<PolicyTool>, Option<Function>), PolicyError> {
    let system_prompt_append = declaration
        .get::<String>("system_prompt_append")
        .map_err(contract_error)?;
    let before_tool_call = declaration
        .get::<Option<Function>>("before_tool_call")
        .map_err(contract_error)?;
    let Some(declared_tools) = declaration
        .get::<Option<Table>>("tools")
        .map_err(contract_error)?
    else {
        return Ok((system_prompt_append, Vec::new(), before_tool_call));
    };

    let mut names = BTreeSet::new();
    let mut tools = Vec::new();
    for declared_tool in declared_tools.sequence_values::<Table>() {
        let declared_tool = declared_tool.map_err(contract_error)?;
        let tool = parse_tool(&declared_tool)?;
        if !names.insert(tool.name.clone()) {
            return Err(PolicyError::Contract {
                message: format!("tools contains duplicate name {:?}", tool.name),
            });
        }
        tools.push(tool);
    }
    Ok((system_prompt_append, tools, before_tool_call))
}

fn parse_tool(declaration: &Table) -> Result<PolicyTool, PolicyError> {
    let name = required_field(declaration, "name")?;
    let description = required_field(declaration, "description")?;
    let capability = required_field(declaration, "capability")?;
    let schema_json = required_field(declaration, "schema_json")?;
    let handler_source = declaration
        .get::<Option<String>>("handler_source")
        .map_err(contract_error)?;
    for (field, value) in [
        ("name", name.as_str()),
        ("description", description.as_str()),
        ("capability", capability.as_str()),
        ("schema_json", schema_json.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(PolicyError::Contract {
                message: format!("tool field {field:?} must not be empty"),
            });
        }
    }
    let execution_mode = match required_field(declaration, "execution_mode")?.as_str() {
        "sequential" => ToolExecutionMode::Sequential,
        "parallel" => ToolExecutionMode::Parallel,
        value => {
            return Err(PolicyError::Contract {
                message: format!(
                    "tool {name:?} has invalid execution_mode {value:?}; expected sequential or parallel"
                ),
            });
        }
    };
    let schema = JsonValue::parse(&schema_json).map_err(|error| PolicyError::Contract {
        message: format!("tool {name:?} schema_json is invalid: {error}"),
    })?;
    if handler_source
        .as_deref()
        .is_some_and(|source| source.trim().is_empty())
    {
        return Err(PolicyError::Contract {
            message: format!("tool {name:?} handler_source must not be empty when declared"),
        });
    }
    Ok(PolicyTool {
        name,
        description,
        schema,
        capability,
        execution_mode,
        handler_source,
    })
}

fn required_field(declaration: &Table, name: &str) -> Result<String, PolicyError> {
    declaration
        .get::<String>(name)
        .map_err(|error| PolicyError::Contract {
            message: format!("tool field {name:?} is required and must be a string: {error}"),
        })
}

fn parse_decision(value: Value) -> Result<BeforeToolCall, PolicyError> {
    match value {
        Value::String(value) if value.to_str().map_err(runtime_error)?.as_ref() == "allow" => {
            Ok(BeforeToolCall::Allow)
        }
        Value::Table(value) => {
            let action: String = value.get("action").map_err(contract_error)?;
            let reason: String = value.get("reason").map_err(contract_error)?;
            if reason.trim().is_empty() {
                return Err(PolicyError::Contract {
                    message: "before_tool_call denial reason must not be empty".to_owned(),
                });
            }
            match action.as_str() {
                "block" => Ok(BeforeToolCall::Block { reason }),
                "terminate" => Ok(BeforeToolCall::Terminate { reason }),
                _ => Err(PolicyError::Contract {
                    message: format!(
                        "before_tool_call action {action:?} must be block or terminate"
                    ),
                }),
            }
        }
        _ => Err(PolicyError::Contract {
            message: "before_tool_call must return \"allow\" or { action, reason }".to_owned(),
        }),
    }
}

fn runtime_error(error: mlua::Error) -> PolicyError {
    PolicyError::Runtime {
        message: error.to_string(),
    }
}

fn contract_error(error: mlua::Error) -> PolicyError {
    PolicyError::Contract {
        message: error.to_string(),
    }
}

fn before_hook_error(error: PolicyError) -> HookError {
    HookError::new("before_tool_call", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{LuaPolicy, LuaPolicyHookSet, PolicyError, PolicyLimits};
    use crate::bundle::{Bundle, BundleManifest, BUNDLE_ABI_VERSION};
    use pi_agent_core::error::HookError;
    use pi_agent_core::hooks::{AfterToolCall, BeforeToolCall, ContextEnvelope, HookSet};
    use pi_agent_core::state::{SerializedJson, ToolCallId};
    use pi_agent_core::tool::{ToolCall, ToolResult};
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
