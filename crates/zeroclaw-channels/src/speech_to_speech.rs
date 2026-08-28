//! Speech-to-speech broker channel.
//!
//! Bridges a hosted bidirectional voice model (e.g. Gemini Live) into
//! ZeroClaw as a broker channel: audio in, transcript/audio out, with a
//! broker persona steering how the model mediates the call. This module
//! currently holds only the `Channel` skeleton — the audio seam and session
//! handle land in a later task.

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;

use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::SpeechToSpeechConfig;

/// Speech-to-speech broker channel — bridges a hosted bidirectional voice
/// model into ZeroClaw. `send`/`listen` are minimal stubs for now; broker
/// session logic is filled in by a later task.
pub struct SpeechToSpeechChannel {
    /// The alias key under `[channels.speech_to_speech.<alias>]` this
    /// handle is bound to. Used for attribution.
    alias: String,
    /// Precomputed `name()` key (`speech_to_speech.<alias>`), joined once at
    /// construction so `name()` can return a borrowed `&str`.
    name: String,
    /// Reserved for the audio seam + session handle added in a later task.
    #[allow(dead_code)]
    config: SpeechToSpeechConfig,
}

impl SpeechToSpeechChannel {
    pub fn new(alias: impl Into<String>, config: SpeechToSpeechConfig) -> Self {
        let alias = alias.into();
        let name = format!("speech_to_speech.{alias}");
        Self {
            alias,
            name,
            config,
        }
    }
}

impl ::zeroclaw_api::attribution::Attributable for SpeechToSpeechChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::VoiceBroker,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for SpeechToSpeechChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, _message: &SendMessage) -> Result<()> {
        // Session/audio delivery lands in a later task.
        Ok(())
    }

    async fn listen(&self, _tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Broker session loop lands in a later task.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::channel::Channel;

    fn cfg() -> SpeechToSpeechConfig {
        SpeechToSpeechConfig::default()
    }

    #[test]
    fn channel_name_is_alias_scoped() {
        let ch = SpeechToSpeechChannel::new("desk".to_string(), cfg());
        assert_eq!(ch.name(), "speech_to_speech.desk");
    }
}
