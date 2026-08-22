//! Policy VM loading and hook evaluation.

use super::parsing::{parse_decision, parse_declaration, runtime_error};
use super::types::{PolicyRuntime, PolicyTool};
use super::{LuaPolicy, PolicyError, PolicyLimits};
use crate::bundle::Bundle;
use crate::bundle_runtime::BundleRuntime;
use mlua::{Lua, LuaOptions, StdLib, Table, Value, VmState};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tea_core::hooks::BeforeToolCall;
use tea_core::tool::ToolCall;

const POLICY_CHUNK_NAME: &str = "tea-policy.luau";

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
