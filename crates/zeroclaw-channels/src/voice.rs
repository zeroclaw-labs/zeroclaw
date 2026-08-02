//! Unified voice pipeline facade for channel code.
//! This module keeps speech-to-text and text-to-speech wiring behind one
//! small API without changing the existing managers or channel call sites.

use anyhow::{Context as _, Result};

use zeroclaw_config::schema::Config;

use crate::transcription::TranscriptionManager;
use crate::tts::TtsManager;

pub struct VoicePipeline {
    stt: Option<TranscriptionManager>,
    tts: Option<TtsManager>,
}

impl VoicePipeline {
    /// Build a pipeline for the runtime-active agent, when one can be resolved.
    pub fn from_config(config: &Config) -> Result<Self> {
        Self::from_config_for_agent(config, None)
    }

    pub fn from_config_for_agent(config: &Config, agent_alias: Option<&str>) -> Result<Self> {
        let stt = if config.transcription.enabled {
            Some(
                TranscriptionManager::from_config_for_agent(config, agent_alias)
                    .context("failed to initialize transcription manager")?,
            )
        } else {
            None
        };

        let tts = if config.tts.enabled {
            Some(
                TtsManager::from_config_for_agent(config, agent_alias)
                    .context("failed to initialize TTS manager")?,
            )
        } else {
            None
        };

        Ok(Self { stt, tts })
    }

    /// Returns true when the transcription half is enabled.
    pub fn is_stt_available(&self) -> bool {
        self.stt.is_some()
    }

    /// Returns true when the TTS half is enabled.
    pub fn is_tts_available(&self) -> bool {
        self.tts.is_some()
    }

    /// Returns true when both voice directions are enabled.
    pub fn is_full_duplex(&self) -> bool {
        self.is_stt_available() && self.is_tts_available()
    }

    /// List registered transcription provider aliases.
    pub fn stt_providers(&self) -> Vec<String> {
        self.stt
            .as_ref()
            .map(|manager| {
                let mut providers: Vec<_> = manager
                    .available_providers()
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                providers.sort();
                providers
            })
            .unwrap_or_default()
    }

    /// List registered TTS provider aliases.
    pub fn tts_providers(&self) -> Vec<String> {
        self.tts
            .as_ref()
            .map(TtsManager::available_providers)
            .unwrap_or_default()
    }

    /// Transcribe audio with the bound agent's transcription provider.
    pub async fn transcribe(&self, audio_data: &[u8], file_name: &str) -> Result<String> {
        let stt = self.stt.as_ref().context(
            "STT is not configured; enable [transcription] before calling VoicePipeline::transcribe",
        )?;
        stt.transcribe(audio_data, file_name).await
    }

    /// Transcribe audio with an explicit transcription provider alias.
    pub async fn transcribe_with_provider(
        &self,
        audio_data: &[u8],
        file_name: &str,
        provider_alias: &str,
    ) -> Result<String> {
        let stt = self.stt.as_ref().context(
            "STT is not configured; enable [transcription] before calling VoicePipeline::transcribe_with_provider",
        )?;
        stt.transcribe_with_provider(audio_data, file_name, provider_alias)
            .await
    }

    /// Synthesize speech with the bound agent's TTS provider.
    pub async fn synthesize(&self, text: &str) -> Result<Vec<u8>> {
        let tts = self.tts.as_ref().context(
            "TTS is not configured; enable [tts] before calling VoicePipeline::synthesize",
        )?;
        tts.synthesize(text).await
    }

    /// Synthesize speech with the bound agent's TTS provider and an explicit voice.
    pub async fn synthesize_with_voice(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        let tts = self.tts.as_ref().context(
            "TTS is not configured; enable [tts] before calling VoicePipeline::synthesize_with_voice",
        )?;
        tts.synthesize_with_voice(text, voice).await
    }

    /// Synthesize speech with an explicit TTS provider and voice.
    pub async fn synthesize_with_provider(
        &self,
        text: &str,
        provider_alias: &str,
        voice: &str,
    ) -> Result<Vec<u8>> {
        let tts = self.tts.as_ref().context(
            "TTS is not configured; enable [tts] before calling VoicePipeline::synthesize_with_provider",
        )?;
        tts.synthesize_with_provider(text, provider_alias, voice)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::{
        AliasedAgentConfig, EdgeTtsProviderConfig, GroqTranscriptionProviderConfig,
        TranscriptionConfig, TranscriptionProviderConfig, TtsProviderConfig,
    };

    fn config_with_edge_tts() -> Config {
        let mut config = Config::default();
        config.tts.enabled = true;
        config.agents.insert(
            "default".to_string(),
            AliasedAgentConfig {
                tts_provider: "edge.default".into(),
                ..AliasedAgentConfig::default()
            },
        );
        config.providers.tts.edge.insert(
            "default".to_string(),
            EdgeTtsProviderConfig {
                base: TtsProviderConfig {
                    binary_path: Some("edge-tts".to_string()),
                    ..TtsProviderConfig::default()
                },
            },
        );
        config
    }

    fn config_with_groq_stt() -> Config {
        let mut config = Config {
            transcription: TranscriptionConfig {
                enabled: true,
                ..TranscriptionConfig::default()
            },
            ..Config::default()
        };
        config.providers.transcription.groq.insert(
            "default".to_string(),
            GroqTranscriptionProviderConfig {
                base: TranscriptionProviderConfig {
                    api_key: Some("test-groq-key".to_string()),
                    ..TranscriptionProviderConfig::default()
                },
                ..GroqTranscriptionProviderConfig::default()
            },
        );
        config.agents.insert(
            "default".to_string(),
            AliasedAgentConfig {
                transcription_provider: "groq.default".into(),
                ..AliasedAgentConfig::default()
            },
        );
        config
    }

    #[test]
    fn pipeline_is_empty_when_voice_features_are_disabled() {
        let pipeline = VoicePipeline::from_config(&Config::default()).unwrap();
        assert!(!pipeline.is_stt_available());
        assert!(!pipeline.is_tts_available());
        assert!(!pipeline.is_full_duplex());
        assert!(pipeline.stt_providers().is_empty());
        assert!(pipeline.tts_providers().is_empty());
    }

    #[test]
    fn pipeline_reports_enabled_halves_and_providers() {
        let mut config = config_with_groq_stt();
        let tts_config = config_with_edge_tts();
        config.tts = tts_config.tts;
        config.providers.tts = tts_config.providers.tts;
        config.agents.get_mut("default").unwrap().tts_provider = "edge.default".into();

        let pipeline = VoicePipeline::from_config(&config).unwrap();

        assert!(pipeline.is_stt_available());
        assert!(pipeline.is_tts_available());
        assert!(pipeline.is_full_duplex());
        assert_eq!(pipeline.stt_providers(), vec!["groq.default"]);
        assert_eq!(pipeline.tts_providers(), vec!["edge.default"]);
    }

    #[tokio::test]
    async fn pipeline_binds_requested_agent_tts_provider() {
        let mut config = config_with_edge_tts();
        config.agents.clear();
        config.agents.insert(
            "background".to_string(),
            AliasedAgentConfig {
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "primary".to_string(),
            AliasedAgentConfig {
                tts_provider: "edge.default".into(),
                ..AliasedAgentConfig::default()
            },
        );

        let owner = VoicePipeline::from_config_for_agent(&config, Some("primary")).unwrap();
        let background = VoicePipeline::from_config_for_agent(&config, Some("background")).unwrap();

        let owner_err = owner
            .synthesize_with_voice("", "en-US-AriaNeural")
            .await
            .unwrap_err();
        assert!(
            owner_err.to_string().contains("must not be empty"),
            "unexpected error: {owner_err}"
        );
        assert!(
            background
                .synthesize_with_voice("hello", "en-US-AriaNeural")
                .await
                .unwrap_err()
                .to_string()
                .contains("no tts_provider configured")
        );
    }

    #[tokio::test]
    async fn transcribe_errors_when_stt_disabled() {
        let pipeline = VoicePipeline::from_config(&Config::default()).unwrap();
        let err = pipeline
            .transcribe(b"audio", "voice.ogg")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("STT is not configured"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn synthesize_errors_when_tts_disabled() {
        let pipeline = VoicePipeline::from_config(&Config::default()).unwrap();
        let err = pipeline.synthesize("hello").await.unwrap_err();
        assert!(
            err.to_string().contains("TTS is not configured"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn transcribe_uses_bound_agent_provider_before_network_dispatch() {
        let pipeline = VoicePipeline::from_config(&config_with_groq_stt()).unwrap();
        let err = pipeline
            .transcribe(b"audio", "voice.unsupported")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("Unsupported audio format"),
            "expected configured provider to validate the audio format, got: {err}"
        );
    }

    #[tokio::test]
    async fn synthesize_with_voice_requires_agent_tts_provider() {
        let mut config = config_with_edge_tts();
        config.agents.get_mut("default").unwrap().tts_provider = "".into();

        let pipeline = VoicePipeline::from_config(&config).unwrap();
        let err = pipeline
            .synthesize_with_voice("hello", "en-US-AriaNeural")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("no tts_provider configured"),
            "unexpected error: {err}"
        );
    }
}
