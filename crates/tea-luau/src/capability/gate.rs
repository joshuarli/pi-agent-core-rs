//! Authorization gate for explicit capability providers.

use super::domain::{CapabilityError, CapabilityProvider, CapabilityRequest, CapabilityResponse};
use super::manifest::CapabilityManifest;

/// The host-side authorization gate around a capability provider.
pub struct CapabilityGate<P> {
    manifest: CapabilityManifest,
    pub(super) provider: P,
}

impl<P> CapabilityGate<P> {
    /// Bind one provider to one immutable manifest.
    pub fn new(manifest: CapabilityManifest, provider: P) -> Self {
        Self { manifest, provider }
    }

    /// Borrow the immutable authority manifest.
    pub fn manifest(&self) -> &CapabilityManifest {
        &self.manifest
    }
}

impl<P> CapabilityGate<P>
where
    P: CapabilityProvider,
{
    /// Check the manifest before invoking the provider.
    pub fn provide(
        &self,
        request: &CapabilityRequest,
    ) -> Result<CapabilityResponse, CapabilityError> {
        self.manifest.check(request)?;
        self.provider
            .provide(request)
            .map_err(CapabilityError::Provider)
    }
}
