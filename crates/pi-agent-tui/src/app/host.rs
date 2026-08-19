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
