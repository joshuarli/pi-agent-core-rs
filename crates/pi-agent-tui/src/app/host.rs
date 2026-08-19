use pi_agent_core::provider::{openai::OpenAiContextHook, ProviderRegistry};
use pi_agent_core::{Agent, DefaultCodingTools, ThinkingLevel};
use std::sync::Arc;

use super::error::AppError;

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

pub(super) fn provider_candidates(registry: &ProviderRegistry, filter: &str) -> Vec<String> {
    let filter = filter.to_ascii_lowercase();
    registry
        .providers()
        .iter()
        .filter(|entry| {
            entry.id.to_ascii_lowercase().contains(&filter)
                || entry.display_name.to_ascii_lowercase().contains(&filter)
        })
        .map(|entry| entry.id.to_owned())
        .collect()
}

pub(super) fn missing_credential(provider: &str) -> Option<String> {
    let variable = match provider {
        "openrouter" => "OPENROUTER_API_KEY",
        "command-code" => "COMMANDCODE_API_KEY",
        // Local OpenAI-compatible servers are reached through an explicit host URL and do not
        // have a credential boundary in this TUI.
        "local" => return None,
        _ => return Some("provider is not compiled in".into()),
    };
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .is_none()
        .then(|| format!("{variable} is unavailable"))
}

pub(super) fn model_candidates(
    registry: &ProviderRegistry,
    provider: &str,
    filter: &str,
) -> Vec<String> {
    let filter = filter.to_ascii_lowercase();
    let mut candidates = registry
        .provider(provider)
        .into_iter()
        .flat_map(|entry| entry.models.iter())
        .filter(|model| {
            model.id.to_ascii_lowercase().contains(&filter)
                || model.display_name.to_ascii_lowercase().contains(&filter)
        })
        .map(|model| model.id.to_owned())
        .collect::<Vec<_>>();
    if registry
        .provider(provider)
        .is_some_and(|entry| entry.allows_custom_model())
        && "custom model".contains(&filter)
    {
        candidates.push("<custom model>".into());
    }
    candidates
}

pub(super) fn overlay_lines(
    title: &str,
    filter: &str,
    candidates: &[String],
    selected: usize,
) -> Vec<String> {
    let mut lines = vec![format!("{title} picker: {filter}")];
    if candidates.is_empty() {
        lines.push("(no matching compiled entries)".into());
    } else {
        lines.extend(candidates.iter().enumerate().map(|(index, candidate)| {
            format!("{} {candidate}", if index == selected { '>' } else { ' ' })
        }));
    }
    lines.push("Enter selects; Esc cancels".into());
    lines
}
