//! Deterministic prompt-cacheability measurements at the model request boundary.
//!
//! A provider cache hit is provider-specific and must be reported from provider usage. The
//! measurements here are deliberately narrower: they describe how much of two adjacent
//! [`ModelRequest`] values is byte-identical before transport serialization. Hosts can use this
//! as a cacheability proxy and pair it with `Usage::cache_read_tokens` when a provider reports
//! real cache accounting.

use crate::scheduler::ModelRequest;
use tea_protocol::JsonValue;

/// Byte-oriented comparison of one request with its predecessor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptCacheMeasurement {
    /// Stable fingerprint for system prompt, ordered tools, model, and thinking level.
    pub cache_domain_fingerprint: u64,
    /// Whether the predecessor belongs to the same prompt/cache domain.
    pub cache_domain_changed: bool,
    /// System-prompt byte length for the current request.
    pub system_prompt_bytes: usize,
    /// Deterministic ordered tool-definition byte length for the current request.
    pub tool_definition_bytes: usize,
    /// Converted provider-context byte length for the current request.
    pub context_bytes: usize,
    /// Approximate full prompt bytes before a provider-specific envelope is added.
    pub prompt_bytes: usize,
    /// Longest common byte prefix of adjacent converted provider contexts.
    ///
    /// This is a deterministic cacheability proxy, not a provider cache hit.
    pub common_context_prefix_bytes: usize,
    /// Longest common prefix as millionths of the predecessor context length.
    pub common_context_prefix_ratio_millionths: u32,
    /// Stable fingerprint of the current converted provider context.
    pub context_fingerprint: u64,
}

/// Compare a request with an optional immediately preceding request.
pub fn measure_prompt_cacheability(
    previous: Option<&ModelRequest>,
    current: &ModelRequest,
) -> PromptCacheMeasurement {
    let current_tools = tool_definition_bytes(current);
    let current_domain = cache_domain_fingerprint(current, &current_tools);
    let same_domain = previous
        .map(|request| {
            cache_domain_fingerprint(request, &tool_definition_bytes(request)) == current_domain
        })
        .unwrap_or(false);
    let common_context_prefix_bytes = previous
        .filter(|_| same_domain)
        .map(|request| common_prefix_len(request.context.as_bytes(), current.context.as_bytes()))
        .unwrap_or(0);
    let previous_context_bytes = previous.map_or(0, |request| request.context.len());
    let common_context_prefix_ratio_millionths = if previous_context_bytes == 0 {
        0
    } else {
        ((common_context_prefix_bytes as u128 * 1_000_000) / previous_context_bytes as u128)
            .min(u32::MAX as u128) as u32
    };
    PromptCacheMeasurement {
        cache_domain_fingerprint: current_domain,
        cache_domain_changed: previous.is_some() && !same_domain,
        system_prompt_bytes: current.system_prompt.len(),
        tool_definition_bytes: current_tools.len(),
        context_bytes: current.context.len(),
        prompt_bytes: current
            .system_prompt
            .len()
            .saturating_add(current_tools.len())
            .saturating_add(current.context.len()),
        common_context_prefix_bytes,
        common_context_prefix_ratio_millionths,
        context_fingerprint: stable_fingerprint(current.context.as_bytes()),
    }
}

fn cache_domain_fingerprint(request: &ModelRequest, tools: &[u8]) -> u64 {
    let mut bytes = Vec::with_capacity(
        request
            .system_prompt
            .len()
            .saturating_add(tools.len())
            .saturating_add(64),
    );
    bytes.extend_from_slice(request.system_prompt.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(tools);
    bytes.push(0);
    if let Some(model) = &request.model {
        bytes.extend_from_slice(model.provider.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(model.model.as_bytes());
        bytes.push(0);
        if let Some(revision) = &model.revision {
            bytes.extend_from_slice(revision.as_bytes());
        }
    }
    bytes.push(0);
    bytes.extend_from_slice(format!("{:?}", request.thinking_level).as_bytes());
    stable_fingerprint(&bytes)
}

fn tool_definition_bytes(request: &ModelRequest) -> Vec<u8> {
    let definitions = request
        .tools
        .iter()
        .map(|tool| {
            JsonValue::object([
                ("name", JsonValue::from(tool.name.clone())),
                ("description", JsonValue::from(tool.description.clone())),
                ("schema", tool.schema.clone()),
                (
                    "execution_mode",
                    JsonValue::from(format!("{:?}", tool.execution_mode)),
                ),
            ])
        })
        .collect::<Vec<_>>();
    JsonValue::Array(definitions)
        .to_json_string()
        .unwrap_or_default()
        .into_bytes()
}

fn common_prefix_len(left: &[u8], right: &[u8]) -> usize {
    left.iter()
        .zip(right)
        .take_while(|(left, right)| left == right)
        .count()
}

fn stable_fingerprint(bytes: &[u8]) -> u64 {
    // FNV-1a is small, deterministic, and sufficient for a diagnostic fingerprint. It is not
    // used as an identity, authorization token, or cryptographic digest.
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::ModelRequest;
    use crate::state::{ModelDescriptor, ThinkingLevel};

    fn request(context: &str) -> ModelRequest {
        ModelRequest {
            system_prompt: "system".into(),
            context: context.into(),
            tools: Vec::new(),
            model: Some(ModelDescriptor {
                provider: "fixture".into(),
                model: "model".into(),
                revision: None,
            }),
            thinking_level: ThinkingLevel::Off,
        }
    }

    #[test]
    fn reports_common_context_prefix_without_calling_it_a_hit() {
        let previous = request("[one]");
        let current = request("[one]{\"role\":\"user\"}");
        let measurement = measure_prompt_cacheability(Some(&previous), &current);
        assert_eq!(
            measurement.common_context_prefix_bytes,
            previous.context.len()
        );
        assert_eq!(
            measurement.common_context_prefix_ratio_millionths,
            1_000_000
        );
        assert!(!measurement.cache_domain_changed);
    }

    #[test]
    fn domain_changes_zero_the_reusable_prefix() {
        let previous = request("[one]");
        let mut current = request("[one]");
        current.system_prompt = "changed".into();
        let measurement = measure_prompt_cacheability(Some(&previous), &current);
        assert_eq!(measurement.common_context_prefix_bytes, 0);
        assert!(measurement.cache_domain_changed);
    }
}
