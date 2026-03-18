//! Unified voice pipeline for ZeroClaw channels.
//!
//! This module provides a single [`VoicePipeline`] facade that combines:
//! - **STT** — speech-to-text via the providers in [`crate::channels::transcription`]
//! - **TTS** — text-to-speech via the providers in [`crate::channels::tts`]
//!
//! Channels that handle audio (e.g. Telegram voice notes) can build a
//! `VoicePipeline` from the active [`crate::config::Config`] and call
//! [`VoicePipeline::transcribe`] / [`VoicePipeline::synthesize`] without
//! importing both sub-systems individually.
//!
//! # Example
//!
//! ```rust,ignore
//! use zeroclaw::voice::VoicePipeline;
//!
//! let pipeline = VoicePipeline::from_config(&config)?;
//! let text = pipeline.transcribe(audio_bytes, "voice.ogg").await?;
//! let audio = pipeline.synthesize(&text).await?;
//! ```
//!
//! # Configuration
//!
//! The pipeline reads the existing `[transcription]` and `[tts]` sections;
//! no additional config keys are required.  Both halves are optional — a
//! pipeline with only STT configured will fail on `synthesize()` calls, and
//! vice versa.

use anyhow::{Context as _, Result};

use crate::channels::transcription::TranscriptionManager;
use crate::channels::tts::TtsManager;
use crate::config::Config;

// ── VoicePipeline ─────────────────────────────────────────────────────────────

/// Combined STT + TTS voice pipeline.
///
/// Build with [`VoicePipeline::from_config`]; both halves are optional.
pub struct VoicePipeline {
    stt: Option<TranscriptionManager>,
    tts: Option<TtsManager>,
    /// Default voice used by [`synthesize`](Self::synthesize).
    pub default_voice: String,
    /// Default audio format returned by TTS.
    pub default_format: String,
    /// Default TTS provider name.
    pub default_tts_provider: String,
}

impl VoicePipeline {
    /// Construct a [`VoicePipeline`] from the active configuration.
    ///
    /// Returns `Ok` even when neither STT nor TTS is enabled — callers can
    /// query [`is_stt_available`](Self::is_stt_available) /
    /// [`is_tts_available`](Self::is_tts_available) before invoking the
    /// respective halves.
    pub fn from_config(config: &Config) -> Result<Self> {
        let stt = if config.transcription.enabled {
            Some(
                TranscriptionManager::new(&config.transcription)
                    .context("Failed to initialise STT transcriber")?,
            )
        } else {
            None
        };

        let tts = if config.tts.enabled {
            Some(TtsManager::new(&config.tts).context("Failed to initialise TTS manager")?)
        } else {
            None
        };

        Ok(Self {
            stt,
            tts,
            default_voice: config.tts.default_voice.clone(),
            default_format: config.tts.default_format.clone(),
            default_tts_provider: config.tts.default_provider.clone(),
        })
    }

    /// Returns `true` when the STT half is configured and enabled.
    pub fn is_stt_available(&self) -> bool {
        self.stt.is_some()
    }

    /// Returns `true` when the TTS half is configured and enabled.
    pub fn is_tts_available(&self) -> bool {
        self.tts.is_some()
    }

    /// Returns `true` when both STT and TTS are available.
    pub fn is_full_duplex(&self) -> bool {
        self.is_stt_available() && self.is_tts_available()
    }

    // ── STT ──────────────────────────────────────────────────────────────────

    /// Transcribe `audio_data` to text using the default STT provider.
    ///
    /// `file_name` is used for format detection (e.g. `"voice.ogg"`).
    pub async fn transcribe(&self, audio_data: &[u8], file_name: &str) -> Result<String> {
        let stt = self.stt.as_ref().context(
            "STT is not configured — enable [transcription] in config.toml",
        )?;
        stt.transcribe(audio_data, file_name).await
    }

    /// Transcribe `audio_data` using a specific named STT provider.
    pub async fn transcribe_with_provider(
        &self,
        audio_data: &[u8],
        file_name: &str,
        provider: &str,
    ) -> Result<String> {
        let stt = self.stt.as_ref().context(
            "STT is not configured — enable [transcription] in config.toml",
        )?;
        stt.transcribe_with_provider(audio_data, file_name, provider)
            .await
    }

    /// Names of available STT providers (empty if STT is disabled).
    pub fn stt_providers(&self) -> Vec<&str> {
        self.stt
            .as_ref()
            .map(|s: &TranscriptionManager| s.available_providers())
            .unwrap_or_default()
    }

    // ── TTS ──────────────────────────────────────────────────────────────────

    /// Synthesize `text` to audio using the default TTS provider and voice.
    ///
    /// Returns raw audio bytes in the configured default format.
    pub async fn synthesize(&self, text: &str) -> Result<Vec<u8>> {
        let tts = self
            .tts
            .as_ref()
            .context("TTS is not configured — enable [tts] in config.toml")?;
        tts.synthesize(text).await
    }

    /// Synthesize `text` to audio using the default TTS provider and a
    /// specific `voice`.
    pub async fn synthesize_with_voice(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        let provider = self.default_tts_provider.clone();
        self.synthesize_with_provider(text, &provider, voice).await
    }

    /// Synthesize `text` using a specific named TTS `provider` and `voice`.
    pub async fn synthesize_with_provider(
        &self,
        text: &str,
        provider: &str,
        voice: &str,
    ) -> Result<Vec<u8>> {
        let tts = self
            .tts
            .as_ref()
            .context("TTS is not configured — enable [tts] in config.toml")?;
        tts.synthesize_with_provider(text, provider, voice).await
    }

    /// Names of available TTS providers (empty if TTS is disabled).
    pub fn tts_providers(&self) -> Vec<String> {
        self.tts
            .as_ref()
            .map(|t| t.available_providers())
            .unwrap_or_default()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn base_config() -> Config {
        Config::default()
    }

    // ── Availability flags ────────────────────────────────────────────────────

    #[test]
    fn both_halves_absent_when_disabled() {
        let pipeline = VoicePipeline::from_config(&base_config()).unwrap();
        assert!(!pipeline.is_stt_available());
        assert!(!pipeline.is_tts_available());
        assert!(!pipeline.is_full_duplex());
    }

    #[test]
    fn full_duplex_requires_both_halves() {
        // Only STT enabled — not full duplex
        let mut config = base_config();
        config.transcription.enabled = false;
        config.tts.enabled = false;
        let pipeline = VoicePipeline::from_config(&config).unwrap();
        assert!(!pipeline.is_full_duplex());
    }

    // ── Provider lists ────────────────────────────────────────────────────────

    #[test]
    fn stt_providers_empty_when_disabled() {
        let pipeline = VoicePipeline::from_config(&base_config()).unwrap();
        assert!(pipeline.stt_providers().is_empty());
    }

    #[test]
    fn tts_providers_empty_when_disabled() {
        let pipeline = VoicePipeline::from_config(&base_config()).unwrap();
        assert!(pipeline.tts_providers().is_empty());
    }

    // ── Error messages when halves are absent ────────────────────────────────

    #[tokio::test]
    async fn transcribe_errors_when_stt_disabled() {
        let pipeline = VoicePipeline::from_config(&base_config()).unwrap();
        let err = pipeline.transcribe(b"audio", "voice.ogg").await.unwrap_err();
        assert!(
            err.to_string().contains("STT is not configured"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn synthesize_errors_when_tts_disabled() {
        let pipeline = VoicePipeline::from_config(&base_config()).unwrap();
        let err = pipeline.synthesize("hello").await.unwrap_err();
        assert!(
            err.to_string().contains("TTS is not configured"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn synthesize_with_provider_errors_when_tts_disabled() {
        let pipeline = VoicePipeline::from_config(&base_config()).unwrap();
        let err = pipeline
            .synthesize_with_provider("hello", "openai", "alloy")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("TTS is not configured"));
    }

    #[tokio::test]
    async fn transcribe_with_provider_errors_when_stt_disabled() {
        let pipeline = VoicePipeline::from_config(&base_config()).unwrap();
        let err = pipeline
            .transcribe_with_provider(b"audio", "voice.ogg", "groq")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("STT is not configured"));
    }

    // ── Default voice/format from config ────────────────────────────────────

    #[test]
    fn default_voice_and_format_from_config() {
        let mut config = base_config();
        config.tts.default_voice = "nova".to_string();
        config.tts.default_format = "opus".to_string();
        let pipeline = VoicePipeline::from_config(&config).unwrap();
        assert_eq!(pipeline.default_voice, "nova");
        assert_eq!(pipeline.default_format, "opus");
    }
}
