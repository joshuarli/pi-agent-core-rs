//! Optional concrete model-provider adapters.
//!
//! The core loop depends only on [`crate::scheduler::ModelProvider`]. These adapters are behind
//! explicit Cargo features so transport processes and provider wire formats never become part of
//! the default state-machine build.

#[cfg(feature = "provider-commandcode")]
pub mod commandcode;
#[cfg(feature = "provider-openrouter")]
pub mod openrouter;
