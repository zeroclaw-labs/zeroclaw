//! Provider-neutral text-to-speech contract.

use anyhow::Result;

/// Trait for pluggable TTS backends.
#[async_trait::async_trait]
pub trait TtsProvider: Send + Sync + crate::attribution::Attributable {
    /// ModelProvider identifier (e.g. `"openai"`, `"elevenlabs"`).
    fn name(&self) -> &str;

    /// Synthesize `text` using the given `voice`, returning raw audio bytes.
    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>>;

    /// The audio container/format of the bytes returned by
    /// [`synthesize`](Self::synthesize) (e.g. `"opus"`, `"wav"`, `"mp3"`).
    /// Channels use this to pick the correct upload MIME type and Telegram
    /// send method — only `opus`/`ogg` is a true voice note.
    fn output_format(&self) -> &str;

    /// Voices supported by this model_provider.
    fn supported_voices(&self) -> Vec<String>;

    /// Audio output formats supported by this model_provider.
    fn supported_formats(&self) -> Vec<String>;
}
