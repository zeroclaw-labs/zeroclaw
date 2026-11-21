//! Provider-neutral speech-to-text contract.

use anyhow::Result;
use async_trait::async_trait;

/// Trait for speech-to-text transcription_provider implementations.
#[async_trait]
pub trait AsrProvider: Send + Sync + crate::attribution::Attributable {
    /// Human-readable transcription_provider name (e.g. "groq", "openai").
    fn name(&self) -> &str;

    /// Transcribe raw audio bytes. `file_name` includes the extension for
    /// format detection (e.g. "voice.ogg").
    async fn transcribe(&self, audio_data: &[u8], file_name: &str) -> Result<String>;

    /// List of supported audio file extensions.
    fn supported_formats(&self) -> Vec<String> {
        vec![
            "flac", "mp3", "mpeg", "mpga", "mp4", "m4a", "ogg", "oga", "opus", "wav", "webm",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }
}
