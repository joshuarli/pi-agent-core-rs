//! Explicit, feature-gated provider and model metadata.
//!
//! [`ProviderRegistry`] is intentionally only a view over checked-in Rust data. Constructing it
//! does not read the process environment, a credential file, a workspace, or a remote catalog.
//! Hosts resolve credentials and other authority themselves, then pass an owned
//! [`ProviderConfiguration`] to [`ProviderRegistry::build`].

use crate::scheduler::ModelProvider;
use std::fmt;
use std::sync::Arc;

/// Version of the checked-in picker metadata format.
pub const MODEL_CATALOG_VERSION: u32 = 1;

/// One picker-visible model in a provider's checked-in catalog.
///
/// The catalog is deliberately a small, versioned list of identifiers already present in this
/// repository. `context_window` is `None` when this repository does not provide an authoritative
/// context-capacity source; the registry does not infer one from a model name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelDescriptor {
    /// Stable provider-local model identifier.
    pub id: &'static str,
    /// Human-readable name for a host picker.
    pub display_name: &'static str,
    /// Known context capacity in tokens, if supplied by repository source data.
    pub context_window: Option<u64>,
}

/// Provider capabilities that are safe for a host to advertise.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderCapabilities {
    /// Whether this adapter can expose provider-reported monetary cost.
    pub provider_reported_cost: bool,
    /// Whether this adapter currently has a concrete provider-backed compactor.
    ///
    /// Both built-in adapters are `false` until the core compactor port and a documented provider
    /// compaction policy exist. This flag is metadata only; it never creates an implicit fallback.
    pub concrete_compactor: bool,
}

impl ProviderCapabilities {
    /// Whether provider-reported cost is available.
    pub const fn supports_provider_reported_cost(self) -> bool {
        self.provider_reported_cost
    }

    /// Whether a concrete provider-backed compactor is available.
    pub const fn supports_compaction(self) -> bool {
        self.concrete_compactor
    }
}

/// The explicit configuration family required by one compiled adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderConfigurationKind {
    /// Command Code's caller-owned API key and host context configuration.
    #[cfg(feature = "provider-commandcode")]
    CommandCode,
    /// OpenRouter's caller-owned API key and model configuration.
    #[cfg(feature = "provider-openrouter")]
    OpenRouter,
    /// Local OpenAI-compatible endpoint and model configuration.
    #[cfg(feature = "provider-local")]
    Local,
}

/// Metadata for one adapter compiled into this crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderEntry {
    /// Stable provider identifier used in model descriptors.
    pub id: &'static str,
    /// Human-readable provider name for a host picker.
    pub display_name: &'static str,
    /// Version of the static model list below.
    pub model_catalog_version: u32,
    /// Picker-visible models. This list is not a promise of a vendor's complete catalog.
    pub models: &'static [ModelDescriptor],
    /// Whether a caller may supply a model identifier outside `models`.
    pub allows_custom_models: bool,
    /// The explicit adapter configuration family accepted by [`ProviderRegistry::build`].
    pub configuration: ProviderConfigurationKind,
    /// Capabilities available from this adapter.
    pub capabilities: ProviderCapabilities,
}

impl ProviderEntry {
    /// Find one static catalog model by exact identifier.
    pub fn model(&self, model_id: &str) -> Option<&'static ModelDescriptor> {
        self.models.iter().find(|model| model.id == model_id)
    }

    /// Whether a model identifier may be supplied through the custom-model path.
    pub const fn allows_custom_model(&self) -> bool {
        self.allows_custom_models
    }
}

/// A model selected from the static catalog or through the explicit custom-model path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelSelection {
    /// Provider-independent descriptor ready for an agent configuration.
    pub descriptor: crate::state::ModelDescriptor,
    /// Whether this selection was outside the checked-in catalog.
    pub custom: bool,
}

impl ModelSelection {
    /// Borrow the provider-independent descriptor.
    pub const fn descriptor(&self) -> &crate::state::ModelDescriptor {
        &self.descriptor
    }

    /// Consume the selection and return its provider-independent descriptor.
    pub fn into_descriptor(self) -> crate::state::ModelDescriptor {
        self.descriptor
    }
}

/// Explicit caller-owned configuration for one compiled adapter.
///
/// The enum is empty in a default provider-free build. No default constructor or environment
/// lookup can manufacture credentials or host context.
#[derive(Clone, Debug)]
pub enum ProviderConfiguration {
    /// Fully configured Command Code adapter.
    #[cfg(feature = "provider-commandcode")]
    CommandCode(crate::provider::commandcode::CommandCodeConfig),
    /// Fully configured OpenRouter adapter.
    #[cfg(feature = "provider-openrouter")]
    OpenRouter(crate::provider::openrouter::OpenRouterConfig),
    /// Fully configured local OpenAI-compatible adapter.
    #[cfg(feature = "provider-local")]
    Local(crate::provider::local::LocalConfig),
}

/// A provider and the exact model descriptor it was configured to serve.
pub struct ConfiguredProvider {
    /// Descriptor selected by the host and validated against the registry.
    pub descriptor: crate::state::ModelDescriptor,
    /// Explicitly constructed provider adapter.
    pub provider: Arc<dyn ModelProvider>,
}

impl fmt::Debug for ConfiguredProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfiguredProvider")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

/// Errors from model resolution or explicit adapter construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistryError {
    /// The requested provider is not compiled into this crate.
    UnknownProvider {
        /// Provider ID supplied by the caller.
        provider: String,
    },
    /// A required model identifier was empty.
    EmptyModel {
        /// Provider ID whose model was empty.
        provider: String,
    },
    /// The model is not in the static catalog and custom IDs are unavailable.
    UnknownModel {
        /// Provider ID selected by the caller.
        provider: String,
        /// Model ID rejected by the catalog.
        model: String,
    },
    /// The explicit configuration belongs to a different provider family.
    ConfigurationProviderMismatch {
        /// Provider selected by the caller.
        expected: String,
        /// Provider family represented by the supplied configuration.
        actual: &'static str,
    },
    /// The explicit configuration's model does not match the selected descriptor.
    ConfigurationModelMismatch {
        /// Model selected by the caller.
        expected: String,
        /// Model represented by the supplied configuration.
        actual: String,
    },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownProvider { provider } => {
                write!(formatter, "unknown provider {provider:?}")
            }
            Self::EmptyModel { provider } => {
                write!(
                    formatter,
                    "model for provider {provider:?} must not be empty"
                )
            }
            Self::UnknownModel { provider, model } => {
                write!(formatter, "unknown model {provider}/{model}")
            }
            Self::ConfigurationProviderMismatch { expected, actual } => write!(
                formatter,
                "provider configuration is for {actual}, selected provider is {expected}"
            ),
            Self::ConfigurationModelMismatch { expected, actual } => write!(
                formatter,
                "provider configuration model {actual:?} does not match selected model {expected:?}"
            ),
        }
    }
}

impl std::error::Error for RegistryError {}

// Source/update evidence for these lists is intentionally local and reviewable: identifiers are
// copied from adapter tests/configuration and recorded OpenRouter fixtures in this repository.
// No context capacity is claimed because those sources do not provide one.
#[cfg(feature = "provider-commandcode")]
static COMMAND_CODE_MODELS: &[ModelDescriptor] = &[ModelDescriptor {
    id: "deepseek/deepseek-v4-flash",
    display_name: "DeepSeek V4 Flash",
    context_window: None,
}];

#[cfg(feature = "provider-openrouter")]
static OPENROUTER_MODELS: &[ModelDescriptor] = &[
    ModelDescriptor {
        id: "deepseek/deepseek-v4-flash-0731",
        display_name: "DeepSeek V4 Flash 0731",
        context_window: None,
    },
    ModelDescriptor {
        id: "inclusionai/ling-3.0-tiny:free",
        display_name: "InclusionAI Ling 3.0 Tiny (Free)",
        context_window: None,
    },
    ModelDescriptor {
        id: "openai/gpt-5.6-luna",
        display_name: "OpenAI GPT 5.6 Luna",
        context_window: None,
    },
    ModelDescriptor {
        id: "poolside/laguna-xs-2.1",
        display_name: "Poolside Laguna XS 2.1",
        context_window: None,
    },
    ModelDescriptor {
        id: "poolside/laguna-xs-2.1:free",
        display_name: "Poolside Laguna XS 2.1 (Free)",
        context_window: None,
    },
];

#[cfg(feature = "provider-local")]
static LOCAL_MODELS: &[ModelDescriptor] = &[ModelDescriptor {
    id: crate::provider::local::LAGUNA_XS_2_1_MODEL,
    display_name: "Laguna XS 2.1 5-bit (oMLX)",
    context_window: Some(32_768),
}];

static COMPILED_PROVIDERS: &[ProviderEntry] = &[
    #[cfg(feature = "provider-commandcode")]
    ProviderEntry {
        id: "command-code",
        display_name: "Command Code",
        model_catalog_version: MODEL_CATALOG_VERSION,
        models: COMMAND_CODE_MODELS,
        allows_custom_models: true,
        configuration: ProviderConfigurationKind::CommandCode,
        capabilities: ProviderCapabilities {
            provider_reported_cost: false,
            concrete_compactor: false,
        },
    },
    #[cfg(feature = "provider-openrouter")]
    ProviderEntry {
        id: "openrouter",
        display_name: "OpenRouter",
        model_catalog_version: MODEL_CATALOG_VERSION,
        models: OPENROUTER_MODELS,
        allows_custom_models: true,
        configuration: ProviderConfigurationKind::OpenRouter,
        capabilities: ProviderCapabilities {
            provider_reported_cost: true,
            concrete_compactor: false,
        },
    },
    #[cfg(feature = "provider-local")]
    ProviderEntry {
        id: "local",
        display_name: "Local OpenAI-compatible server",
        model_catalog_version: MODEL_CATALOG_VERSION,
        models: LOCAL_MODELS,
        allows_custom_models: true,
        configuration: ProviderConfigurationKind::Local,
        capabilities: ProviderCapabilities {
            provider_reported_cost: false,
            concrete_compactor: false,
        },
    },
];

/// Explicit registry of adapters selected by Cargo features.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProviderRegistry {
    entries: &'static [ProviderEntry],
}

impl ProviderRegistry {
    /// Construct a registry from this build's static feature-selected entries.
    pub const fn new() -> Self {
        Self {
            entries: COMPILED_PROVIDERS,
        }
    }

    /// Return all adapters compiled into this build, in stable provider-ID order.
    pub const fn providers(&self) -> &'static [ProviderEntry] {
        self.entries
    }

    /// Find one compiled provider by stable ID.
    pub fn provider(&self, provider_id: &str) -> Option<&ProviderEntry> {
        self.entries.iter().find(|entry| entry.id == provider_id)
    }

    /// Resolve a static model or an allowed custom model ID.
    pub fn resolve_model(
        &self,
        provider_id: &str,
        model_id: impl Into<String>,
    ) -> Result<ModelSelection, RegistryError> {
        let model_id = model_id.into();
        let entry = self
            .provider(provider_id)
            .ok_or_else(|| RegistryError::UnknownProvider {
                provider: provider_id.to_owned(),
            })?;
        if model_id.trim().is_empty() {
            return Err(RegistryError::EmptyModel {
                provider: provider_id.to_owned(),
            });
        }
        let custom = entry.model(&model_id).is_none();
        if custom && !entry.allows_custom_models {
            return Err(RegistryError::UnknownModel {
                provider: provider_id.to_owned(),
                model: model_id,
            });
        }
        Ok(ModelSelection {
            descriptor: crate::state::ModelDescriptor {
                provider: provider_id.to_owned(),
                model: model_id,
                revision: None,
            },
            custom,
        })
    }

    /// Resolve a caller-supplied model that is intentionally outside the static catalog.
    pub fn custom_model(
        &self,
        provider_id: &str,
        model_id: impl Into<String>,
    ) -> Result<ModelSelection, RegistryError> {
        let selection = self.resolve_model(provider_id, model_id)?;
        if !selection.custom {
            return Err(RegistryError::UnknownModel {
                provider: provider_id.to_owned(),
                model: selection.descriptor.model,
            });
        }
        Ok(selection)
    }

    /// Build an adapter from explicit owned configuration and a resolved model descriptor.
    pub fn build(
        &self,
        descriptor: crate::state::ModelDescriptor,
        configuration: ProviderConfiguration,
    ) -> Result<ConfiguredProvider, RegistryError> {
        self.resolve_model(&descriptor.provider, descriptor.model.clone())?;
        match configuration {
            #[cfg(feature = "provider-commandcode")]
            ProviderConfiguration::CommandCode(configuration) => {
                if descriptor.provider != "command-code" {
                    return Err(RegistryError::ConfigurationProviderMismatch {
                        expected: descriptor.provider.clone(),
                        actual: "command-code",
                    });
                }
                if configuration.model() != descriptor.model {
                    return Err(RegistryError::ConfigurationModelMismatch {
                        expected: descriptor.model,
                        actual: configuration.model().to_owned(),
                    });
                }
                Ok(ConfiguredProvider {
                    descriptor,
                    provider: Arc::new(crate::provider::commandcode::CommandCodeProvider::new(
                        configuration,
                    )),
                })
            }
            #[cfg(feature = "provider-openrouter")]
            ProviderConfiguration::OpenRouter(configuration) => {
                if descriptor.provider != "openrouter" {
                    return Err(RegistryError::ConfigurationProviderMismatch {
                        expected: descriptor.provider.clone(),
                        actual: "openrouter",
                    });
                }
                if configuration.model() != descriptor.model {
                    return Err(RegistryError::ConfigurationModelMismatch {
                        expected: descriptor.model,
                        actual: configuration.model().to_owned(),
                    });
                }
                Ok(ConfiguredProvider {
                    descriptor,
                    provider: Arc::new(crate::provider::openrouter::OpenRouterProvider::new(
                        configuration,
                    )),
                })
            }
            #[cfg(feature = "provider-local")]
            ProviderConfiguration::Local(configuration) => {
                if descriptor.provider != "local" {
                    return Err(RegistryError::ConfigurationProviderMismatch {
                        expected: descriptor.provider.clone(),
                        actual: "local",
                    });
                }
                if configuration.model() != descriptor.model {
                    return Err(RegistryError::ConfigurationModelMismatch {
                        expected: descriptor.model,
                        actual: configuration.model().to_owned(),
                    });
                }
                Ok(ConfiguredProvider {
                    descriptor,
                    provider: Arc::new(crate::provider::local::LocalProvider::new(configuration)),
                })
            }
        }
    }

    /// Build an adapter from a [`ModelSelection`] returned by this registry.
    pub fn build_selection(
        &self,
        selection: ModelSelection,
        configuration: ProviderConfiguration,
    ) -> Result<ConfiguredProvider, RegistryError> {
        self.build(selection.descriptor, configuration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_static_and_has_no_ambient_configuration_path() {
        let first = ProviderRegistry::new();
        let second = ProviderRegistry::new();
        assert_eq!(first.providers(), second.providers());
        assert!(first
            .providers()
            .iter()
            .all(|entry| entry.model_catalog_version == MODEL_CATALOG_VERSION));
    }

    #[cfg(not(any(
        feature = "provider-commandcode",
        feature = "provider-openrouter",
        feature = "provider-local"
    )))]
    #[test]
    fn default_build_remains_provider_free() {
        assert!(ProviderRegistry::new().providers().is_empty());
    }

    #[cfg(feature = "provider-commandcode")]
    #[test]
    fn command_code_feature_contributes_only_command_code() {
        let registry = ProviderRegistry::new();
        let provider = registry
            .provider("command-code")
            .expect("compiled provider");
        assert_eq!(provider.display_name, "Command Code");
        assert_eq!(
            provider.configuration,
            ProviderConfigurationKind::CommandCode
        );
        assert!(!provider.capabilities.provider_reported_cost);
        assert!(!provider.capabilities.concrete_compactor);
        assert!(provider.model("deepseek/deepseek-v4-flash").is_some());
        #[cfg(not(feature = "provider-openrouter"))]
        assert!(registry.provider("openrouter").is_none());
    }

    #[cfg(feature = "provider-openrouter")]
    #[test]
    fn openrouter_feature_exposes_reported_cost_and_known_ids() {
        let registry = ProviderRegistry::new();
        let provider = registry.provider("openrouter").expect("compiled provider");
        assert_eq!(provider.display_name, "OpenRouter");
        assert_eq!(
            provider.configuration,
            ProviderConfigurationKind::OpenRouter
        );
        assert!(provider.capabilities.supports_provider_reported_cost());
        assert!(!provider.capabilities.supports_compaction());
        assert!(provider.model("poolside/laguna-xs-2.1:free").is_some());
        #[cfg(not(feature = "provider-commandcode"))]
        assert!(registry.provider("command-code").is_none());
    }

    #[cfg(feature = "provider-openrouter")]
    #[test]
    fn custom_model_path_is_explicit_and_does_not_change_catalog() {
        let registry = ProviderRegistry::new();
        let before = registry.providers()[0].models;
        let selection = registry
            .custom_model("openrouter", "caller/private-model")
            .expect("custom IDs are allowed");
        assert!(selection.custom);
        assert_eq!(selection.descriptor.provider, "openrouter");
        assert_eq!(selection.descriptor.model, "caller/private-model");
        assert_eq!(registry.providers()[0].models, before);
    }

    #[cfg(feature = "provider-openrouter")]
    #[test]
    fn explicit_openrouter_configuration_builds_without_transport() {
        let registry = ProviderRegistry::new();
        let selection = registry
            .resolve_model("openrouter", "openai/gpt-5.6-luna")
            .expect("checked-in model");
        let configured = registry
            .build(
                selection.into_descriptor(),
                ProviderConfiguration::OpenRouter(
                    crate::provider::openrouter::OpenRouterConfig::try_new(
                        "test-key",
                        "openai/gpt-5.6-luna",
                    )
                    .expect("valid explicit config"),
                ),
            )
            .expect("matching explicit config");
        assert_eq!(configured.descriptor.provider, "openrouter");
        assert_eq!(configured.descriptor.model, "openai/gpt-5.6-luna");
    }

    #[cfg(feature = "provider-commandcode")]
    #[test]
    fn mismatched_explicit_configuration_is_rejected_before_transport() {
        let registry = ProviderRegistry::new();
        let selection = registry
            .resolve_model("command-code", "deepseek/deepseek-v4-flash")
            .expect("checked-in model");
        let host = crate::provider::commandcode::CommandCodeHostContext::new(
            "/sandbox/project",
            "2026-08-14",
            "linux",
        )
        .expect("explicit host context");
        let error = registry
            .build(
                selection.into_descriptor(),
                ProviderConfiguration::CommandCode(
                    crate::provider::commandcode::CommandCodeConfig::new(
                        "test-key",
                        "other-model",
                        host,
                    )
                    .expect("valid explicit config"),
                ),
            )
            .expect_err("model mismatch must fail before adapter use");
        assert!(matches!(
            error,
            RegistryError::ConfigurationModelMismatch { .. }
        ));
    }

    #[cfg(feature = "provider-local")]
    #[test]
    fn local_feature_exposes_laguna_and_builds_without_transport() {
        let registry = ProviderRegistry::new();
        let provider = registry.provider("local").expect("compiled provider");
        assert_eq!(provider.display_name, "Local OpenAI-compatible server");
        assert_eq!(provider.configuration, ProviderConfigurationKind::Local);
        assert!(!provider.capabilities.supports_provider_reported_cost());
        assert_eq!(provider.models[0].context_window, Some(32_768));

        let selection = registry
            .resolve_model("local", crate::provider::local::LAGUNA_XS_2_1_MODEL)
            .expect("Laguna should be in the local catalog");
        let configured = registry
            .build(
                selection.into_descriptor(),
                ProviderConfiguration::Local(crate::provider::local::LocalConfig::laguna_xs_2_1(
                    crate::provider::local::DEFAULT_BASE_URL,
                )),
            )
            .expect("matching local config");
        assert_eq!(configured.descriptor.provider, "local");
    }
}
