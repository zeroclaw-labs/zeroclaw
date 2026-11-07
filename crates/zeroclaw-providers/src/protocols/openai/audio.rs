//! Standard OpenAI multipart transcription and JSON speech protocols.

use anyhow::{Context, Result, bail};
use reqwest::multipart::{Form, Part};
use serde::Serialize;

pub(crate) struct TranscriptionRequest<'a> {
    pub(crate) model: &'a str,
    pub(crate) language: Option<&'a str>,
    pub(crate) prompt: Option<&'a str>,
    pub(crate) file_name: String,
    pub(crate) mime: &'a str,
    pub(crate) audio: &'a [u8],
}

impl TranscriptionRequest<'_> {
    pub(crate) fn form(self) -> Result<Form> {
        let file = Part::bytes(self.audio.to_vec())
            .file_name(self.file_name)
            .mime_str(self.mime)?;
        let mut form = Form::new()
            .part("file", file)
            .text("model", self.model.to_owned())
            .text("response_format", "json");
        if let Some(language) = self.language {
            form = form.text("language", language.to_owned());
        }
        if let Some(prompt) = self.prompt {
            form = form.text("prompt", prompt.to_owned());
        }
        Ok(form)
    }
}

#[derive(Serialize)]
pub(crate) struct SpeechRequest<'a> {
    pub(crate) model: &'a str,
    pub(crate) input: &'a str,
    pub(crate) voice: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) response_format: Option<&'a str>,
}

pub(crate) async fn speech_audio(response: reqwest::Response) -> Result<Vec<u8>> {
    let status = response.status();
    if !status.is_success() {
        bail!("speech API returned HTTP {status}");
    }
    Ok(response
        .bytes()
        .await
        .context("reading speech API audio response")?
        .to_vec())
}

/// Resolved adapter for the multipart transcription protocol. Vendor factories
/// supply defaults; the protocol never selects a provider or reads config.
pub(crate) struct TranscriptionClient {
    pub(crate) transport: crate::transport::HttpTransportConfig,
    /// Proxy/telemetry service identity. The multipart wire format is shared
    /// by OpenAI-compatible vendors, but their configured proxy routes are
    /// intentionally distinct.
    pub(crate) service_key: &'static str,
    pub(crate) endpoint: String,
    pub(crate) model: String,
    pub(crate) language: Option<String>,
    pub(crate) initial_prompt: Option<String>,
}

impl TranscriptionClient {
    pub(crate) async fn transcribe(&self, audio: &[u8], file_name: &str) -> Result<String> {
        let (file_name, mime) = crate::transcription::validate_audio(audio, file_name)?;
        let client = self
            .transport
            .client(std::time::Duration::from_secs(120), self.service_key)?;
        let form = TranscriptionRequest {
            model: &self.model,
            language: self.language.as_deref(),
            prompt: self.initial_prompt.as_deref(),
            file_name,
            mime,
            audio,
        }
        .form()?;
        let response = self
            .transport
            .bearer(client.post(&self.endpoint))
            .multipart(form)
            .send()
            .await
            .context("sending transcription request")?;
        transcription_text(response).await
    }
}

pub(crate) async fn transcription_text(response: reqwest::Response) -> Result<String> {
    let status = response.status();
    if !status.is_success() {
        bail!("Transcription API error ({status})");
    }
    let body: serde_json::Value = response
        .json()
        .await
        .context("parsing transcription response")?;
    body["text"]
        .as_str()
        .map(ToOwned::to_owned)
        .context("Transcription response missing 'text' field")
}

pub(crate) struct SpeechClient {
    pub(crate) transport: crate::transport::HttpTransportConfig,
    pub(crate) endpoint: String,
    pub(crate) model: String,
    pub(crate) speed: Option<f64>,
    pub(crate) response_format: Option<String>,
}

impl SpeechClient {
    pub(crate) async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        let client = self
            .transport
            .client(std::time::Duration::from_secs(60), "channel.tts.openai")?;
        let body = SpeechRequest {
            model: &self.model,
            input: text,
            voice,
            speed: self.speed,
            response_format: self.response_format.as_deref(),
        };
        let response = self
            .transport
            .bearer(client.post(&self.endpoint))
            .json(&body)
            .send()
            .await
            .context("sending speech request")?;
        speech_audio(response).await
    }
}
