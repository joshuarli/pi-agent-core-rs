use pi_agent_core::provider::{openai::OpenAiContextHook, ProviderRegistry};
use pi_agent_core::{Agent, AgentConfiguration, DefaultCodingTools, ThinkingLevel};
use pi_agent_luau::LuaPolicyHookSet;
use std::sync::Arc;
use std::path::Path;

use super::error::AppError;
use super::phi::{PhiDeclaredTool, PhiExtensionFilesTool, PhiExtensionHandbookTool, PhiExtensions};

const PHI_AUTHORING_PROMPT: &str = r#"
Phi extensions

When the user asks to create or modify a Phi extension, call `phi_extension_handbook` before
writing source. Use `phi_extension_files` only for Phi extension files. Its writes are drafts:
they cannot change the extension registry, activate a new extension, or grant authority. Phi
extensions are reloaded only after a run has settled, so the current run keeps its original tools,
prompt, and hooks.
"#;

/// Build the agent shared by the interactive host and its headless tests.
///
/// Provider adapters consume the standard OpenAI-compatible context produced by the host
/// policy hook. Keeping this assembly in one function makes a headless provider probe exercise
/// the same boundary as the terminal application.
pub fn build_host_agent(
    tools: DefaultCodingTools,
) -> Result<pi_agent_core::AgentBuilder, AppError> {
    build_host_agent_with_thinking(tools, ThinkingLevel::Off)
}

pub(super) fn build_host_agent_with_thinking(
    tools: DefaultCodingTools,
    thinking_level: ThinkingLevel,
) -> Result<pi_agent_core::AgentBuilder, AppError> {
    Agent::builder()
        .hooks(Arc::new(OpenAiContextHook))
        .thinking_level(thinking_level)
        .pinned_default_coding_profile(tools)
        .map_err(|error| AppError::Setup(error.to_string()))
}

/// Compose the TUI's trusted host configuration with loaded Phi declarations.
///
/// This operation is intentionally separate from `AgentBuilder`: it can be applied with the
/// core's idle-only `Agent::replace_configuration` API when a host reloads Phi files. No
/// capability binding is created for a declaration or handler source.
pub(super) fn compose_phi_configuration(
    mut configuration: AgentConfiguration,
    phi: &PhiExtensions,
    phi_home: &Path,
) -> Result<AgentConfiguration, AppError> {
    let mut seen = configuration
        .tools
        .names()
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    let mut prompt = configuration.system_prompt;
    for extension in phi.extensions() {
        let append = extension.policy().system_prompt_append();
        if !append.is_empty() {
            if !prompt.is_empty() {
                prompt.push('\n');
            }
            prompt.push_str(append);
        }
        for tool in extension.policy().tools() {
            if !seen.insert(tool.name.clone()) {
                return Err(AppError::Setup(format!(
                    "Phi extension {:?} duplicates tool {:?}",
                    extension.name(),
                    tool.name
                )));
            }
            configuration
                .tools
                .insert(Arc::new(PhiDeclaredTool::from_policy(extension.name(), tool)));
        }
    }
    for reserved in ["phi_extension_handbook", "phi_extension_files"] {
        if !seen.insert(reserved.to_owned()) {
            return Err(AppError::Setup(format!(
                "Phi extension tool name is reserved: {reserved:?}"
            )));
        }
    }
    if !prompt.is_empty() {
        prompt.push('\n');
    }
    prompt.push_str(PHI_AUTHORING_PROMPT.trim());
    configuration
        .tools
        .insert(Arc::new(PhiExtensionHandbookTool));
    configuration
        .tools
        .insert(Arc::new(PhiExtensionFilesTool::new(phi_home)));

    // Wrapping in reverse preserves registry order: the first extension's decision runs first.
    let mut hooks = configuration.hooks;
    for extension in phi.extensions().iter().rev() {
        hooks = Arc::new(LuaPolicyHookSet::new(
            Arc::clone(extension.policy()),
            hooks,
        ));
    }
    configuration.system_prompt = prompt;
    configuration.hooks = hooks;
    Ok(configuration)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ModelCandidate {
    pub(super) provider: &'static str,
    pub(super) provider_name: &'static str,
    pub(super) model: Option<pi_agent_core::provider::ModelDescriptor>,
}

impl ModelCandidate {
    pub(super) fn label(self) -> String {
        match self.model {
            Some(model) => format!("{} · {}", self.provider_name, model.id),
            None => format!("{} · custom model…", self.provider_name),
        }
    }

    pub(super) fn model_id(self) -> Option<&'static str> {
        self.model.map(|model| model.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_agent_core::hooks::NoHooks;
    use pi_agent_core::tool::ToolRegistry;

    #[test]
    fn phi_authoring_tools_and_guidance_are_present_without_an_extension_registry() {
        let configuration = compose_phi_configuration(
            AgentConfiguration::new("base prompt", ToolRegistry::default(), Arc::new(NoHooks)),
            &PhiExtensions::default(),
            Path::new("/fixture/phi"),
        )
        .expect("empty Phi registry composes with the host configuration");

        assert!(configuration
            .tools
            .names()
            .eq(["phi_extension_handbook", "phi_extension_files"]));
        assert!(configuration
            .system_prompt
            .contains("call `phi_extension_handbook` before"));
    }
}

pub(super) fn model_candidates(registry: &ProviderRegistry, filter: &str) -> Vec<ModelCandidate> {
    let filter = filter.to_ascii_lowercase();
    let mut candidates = Vec::new();
    for entry in registry.providers() {
        for model in entry.models {
            if model.id.to_ascii_lowercase().contains(&filter)
                || model.display_name.to_ascii_lowercase().contains(&filter)
                || entry.id.to_ascii_lowercase().contains(&filter)
                || entry.display_name.to_ascii_lowercase().contains(&filter)
            {
                candidates.push(ModelCandidate {
                    provider: entry.id,
                    provider_name: entry.display_name,
                    model: Some(*model),
                });
            }
        }
        if entry.allows_custom_model()
            && ("custom model".contains(&filter)
                || entry.id.to_ascii_lowercase().contains(&filter)
                || entry.display_name.to_ascii_lowercase().contains(&filter))
        {
            candidates.push(ModelCandidate {
                provider: entry.id,
                provider_name: entry.display_name,
                model: None,
            });
        }
    }
    candidates
}

pub(super) fn overlay_lines(
    title: &str,
    filter: &str,
    candidates: &[String],
    selected: usize,
    max_rows: usize,
) -> Vec<String> {
    let mut lines = vec![if filter.is_empty() {
        format!("{title} {} · Type to filter", candidates.len())
    } else {
        format!("{title} {} · {filter}", candidates.len())
    }];
    if candidates.is_empty() {
        lines.push("  No matching models".into());
    } else {
        let visible = max_rows.saturating_sub(2).max(1).min(candidates.len());
        let start = selected
            .saturating_sub(visible.saturating_sub(1))
            .min(candidates.len().saturating_sub(visible));
        lines.extend(candidates[start..start + visible].iter().enumerate().map(
            |(offset, candidate)| {
                let index = start + offset;
                format!("{} {candidate}", if index == selected { '❯' } else { ' ' })
            },
        ));
    }
    lines.push("↑/↓ navigate · Enter select · Esc close".into());
    lines
}
