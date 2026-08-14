//! Optional concrete model-provider adapters.
//!
//! The core loop depends only on [`crate::scheduler::ModelProvider`]. These adapters are behind
//! explicit Cargo features so transport processes and provider wire formats never become part of
//! the default state-machine build.

mod registry;
mod retry;

#[cfg(any(feature = "provider-commandcode", feature = "provider-openrouter"))]
pub mod openai;

pub use registry::{
    ConfiguredProvider, ModelDescriptor, ModelSelection, ProviderCapabilities,
    ProviderConfiguration, ProviderConfigurationKind, ProviderEntry, ProviderRegistry,
    RegistryError, MODEL_CATALOG_VERSION,
};
pub use retry::RetryPolicy;

#[cfg(feature = "provider-commandcode")]
pub mod commandcode;
#[cfg(feature = "provider-openrouter")]
pub mod openrouter;
