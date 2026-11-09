//! A call owns one bounded PCM connection. The backend owns streaming ASR/VAD;
//! live config and the channel adapter own admission at every audio boundary.
use anyhow::{Context, Result, ensure};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use zeroclaw_config::schema::LocalWhisperTranscriptionProviderConfig;

pub(crate) struct LiveTranscription {
    pub audio: mpsc::Sender<Vec<u8>>,
    /// Immutable identity of this physical connection, compared with live selection.
    pub endpoint: String,
    worker: tokio::task::JoinHandle<()>,
}
impl Drop for LiveTranscription {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

impl LiveTranscription {
    pub async fn connect(
        config: &LocalWhisperTranscriptionProviderConfig,
    ) -> Result<(Self, mpsc::Receiver<Value>)> {
        let uri = config
            .stream_uri
            .as_deref()
            .context("live_transcription_not_configured")?;
        let url = url::Url::parse(uri)?;
        ensure!(
            matches!(url.scheme(), "ws" | "wss")
                && matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"))
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none(),
            "live_transcription_requires_loopback_endpoint"
        );
        let mut request = uri.into_client_request()?;
        if let Some(token) = config
            .bearer_token
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            request
                .headers_mut()
                .insert("Authorization", format!("Bearer {}", token.trim()).parse()?);
        }
        let (mut socket, _) = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio_tungstenite::connect_async(request),
        )
        .await
        .context("live_transcription_connect_timeout")?
        .context("live_transcription_connect_failed")?;
        let ready = tokio::time::timeout(std::time::Duration::from_secs(15), socket.next())
            .await
            .context("live_transcription_ready_timeout")?
            .context("live_transcription_closed")??;
        let ready: Value = serde_json::from_str(ready.to_text()?)?;
        ensure!(
            ready["type"] == "ready" && ready["sampleRate"] == 16000 && ready["streaming"] == true,
            "live_transcription_protocol_mismatch"
        );
        let (audio, mut incoming) = mpsc::channel::<Vec<u8>>(32);
        let (events, rx) = mpsc::channel(16);
        let worker = zeroclaw_spawn::spawn!(async move {
            let result = async {
                let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(20));
                loop {
                    tokio::select! {
                        audio = incoming.recv() => {
                            let Some(audio) = audio else { break; };
                            ensure!(!audio.is_empty() && audio.len() <= 8192 && audio.len() % 2 == 0,
                                "live_transcription_invalid_pcm");
                            socket.send(Message::Binary(audio.into())).await?;
                        }
                        event = socket.next() => {
                            let event = event.context("live_transcription_closed")??;
                            match event {
                                Message::Text(text) => {
                                    ensure!(text.len() <= 262144, "live_transcription_frame_limit");
                                    let value: Value = serde_json::from_str(text.as_str())?;
                                    match value["type"].as_str() {
                                        Some("speech_start" | "final") => events.send(value).await.context("live_transcription_consumer_closed")?,
                                        Some("partial" | "pong" | "interrupted") => {},
                                        Some("error") => anyhow::bail!("live_transcription_backend_failed"),
                                        _ => anyhow::bail!("live_transcription_invalid_event"),
                                    }
                                }
                                Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await?,
                                Message::Close(_) => anyhow::bail!("live_transcription_closed"),
                                _ => anyhow::bail!("live_transcription_invalid_frame"),
                            }
                        }
                        _ = heartbeat.tick() => socket.send(Message::Text(json!({"type":"ping"}).to_string().into())).await?,
                    }
                }
                Ok::<(), anyhow::Error>(())
            }.await;
            if result.is_err() {
                let _ = events.send(json!({"type":"error"})).await;
            }
        });
        Ok((
            Self {
                audio,
                worker,
                endpoint: uri.to_string(),
            },
            rx,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn pcm_stream_authenticates_and_emits_final_without_batch_transcription() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("ws://{}/stream", listener.local_addr().unwrap());
        let server = zeroclaw_spawn::spawn!(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            use tokio_tungstenite::tungstenite::handshake::server::{
                Callback, ErrorResponse, Request, Response,
            };
            struct Authenticate;
            impl Callback for Authenticate {
                fn on_request(
                    self,
                    req: &Request,
                    response: Response,
                ) -> std::result::Result<Response, ErrorResponse> {
                    assert_eq!(req.headers()["Authorization"], "Bearer fixture");
                    Ok(response)
                }
            }
            let mut socket = tokio_tungstenite::accept_hdr_async(tcp, Authenticate)
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    json!({"type":"ready","sampleRate":16000,"streaming":true})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            loop {
                match socket.next().await.unwrap().unwrap() {
                    Message::Binary(pcm) => {
                        assert_eq!(pcm.as_ref(), &[0, 1, 2, 3]);
                        break;
                    }
                    Message::Text(_) => socket
                        .send(Message::Text(json!({"type":"pong"}).to_string().into()))
                        .await
                        .unwrap(),
                    _ => {}
                }
            }
            for event in [
                json!({"type":"speech_start","utteranceId":"u1"}),
                json!({"type":"partial","utteranceId":"u1","text":"unfinished"}),
                json!({"type":"final","utteranceId":"u1","text":"Complete speech"}),
            ] {
                socket
                    .send(Message::Text(event.to_string().into()))
                    .await
                    .unwrap();
            }
            // Dropping the call owner closes its physical connection.
            let _ = socket.next().await;
        });
        let (stream, mut events) =
            LiveTranscription::connect(&LocalWhisperTranscriptionProviderConfig {
                stream_uri: Some(uri),
                bearer_token: Some("fixture".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        stream.audio.send(vec![0, 1, 2, 3]).await.unwrap();
        assert_eq!(events.recv().await.unwrap()["type"], "speech_start");
        let final_event = events.recv().await.unwrap();
        assert_eq!(final_event["type"], "final");
        assert_eq!(final_event["text"], "Complete speech");
        drop(stream);
        tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }
    #[tokio::test]
    async fn stream_endpoint_cannot_exfiltrate_pcm_or_credentials_to_remote_hosts() {
        for uri in [
            "ws://example.org/stream",
            "ws://user@127.0.0.1/stream",
            "https://127.0.0.1/stream",
        ] {
            assert!(
                LiveTranscription::connect(&LocalWhisperTranscriptionProviderConfig {
                    stream_uri: Some(uri.into()),
                    ..Default::default()
                })
                .await
                .is_err()
            );
        }
    }
}
