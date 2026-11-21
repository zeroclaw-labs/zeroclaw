//! Provider-neutral speech-to-text contract.

use anyhow::Result;

use crate::attribution::Attributable;

/// A speech-to-text provider implementation.
#[async_trait::async_trait]
pub trait AsrProvider: Send + Sync + Attributable {
    /// Stable provider identifier used for attribution and diagnostics.
    fn name(&self) -> &str;

    /// Transcribe an audio payload. The filename supplies its format.
    async fn transcribe(&self, audio_data: &[u8], file_name: &str) -> Result<String>;

    /// File extensions accepted by this provider.
    fn supported_formats(&self) -> Vec<String> {
        [
            "flac", "mp3", "mpeg", "mpga", "mp4", "m4a", "ogg", "oga", "opus", "wav", "webm",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }
}
