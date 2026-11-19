//! Wire protocol implementations shared by provider vendors.
//!
//! These modules do not select a provider kind or inspect the config registry.
//! Capability factories resolve vendor policy before invoking a protocol.

pub(crate) mod openai;

/// Retain a bounded, sanitized upstream explanation without buffering an
/// arbitrary error response. Successful audio/vector bodies use their own path.
pub(crate) async fn error_body(mut response: reqwest::Response) -> String {
    const MAX_BYTES: usize = 8192;
    let mut body = Vec::new();
    while body.len() < MAX_BYTES {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let remaining = MAX_BYTES - body.len();
                body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            }
            Ok(None) | Err(_) => break,
        }
    }
    crate::sanitize_api_error(&String::from_utf8_lossy(&body))
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn capability_errors_keep_status_and_bounded_upstream_explanation() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_string(format!(
                "unknown synthetic model sk-test-secret {}",
                "x".repeat(20_000)
            )))
            .expect(3)
            .mount(&server)
            .await;
        for capability in ["embedding", "asr", "tts"] {
            let response = reqwest::Client::new()
                .post(server.uri())
                .send()
                .await
                .unwrap();
            let error = match capability {
                "embedding" => super::openai::embedding::vectors(response, 1, Some(3))
                    .await
                    .unwrap_err(),
                "asr" => super::openai::audio::transcription_text(response)
                    .await
                    .unwrap_err(),
                _ => super::openai::audio::speech_audio(response)
                    .await
                    .unwrap_err(),
            }
            .to_string();
            assert!(error.contains("400"), "{error}");
            assert!(error.contains("unknown synthetic model"), "{error}");
            assert!(!error.contains("sk-test-secret"), "{error}");
            assert!(error.len() < 1024, "{}", error.len());
        }
    }
}
