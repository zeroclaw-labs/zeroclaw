//! Provider-neutral text-to-speech contract.

use anyhow::Result;

use crate::attribution::Attributable;

/// A text-to-speech provider implementation.
#[async_trait::async_trait]
pub trait TtsProvider: Send + Sync + Attributable {
    /// Stable provider identifier used for attribution and diagnostics.
    fn name(&self) -> &str;

    /// Synthesize text into an audio container selected by the provider.
    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>>;

    /// Container format returned by [`Self::synthesize`].
    fn output_format(&self) -> &str;

    /// Voices supported by this provider.
    fn supported_voices(&self) -> Vec<String>;

    /// Audio formats supported by this provider.
    fn supported_formats(&self) -> Vec<String>;
}
