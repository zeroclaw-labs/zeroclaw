use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{
    connect_async, tungstenite::client::IntoClientRequest, tungstenite::http::HeaderValue,
    tungstenite::Message, MaybeTlsStream, WebSocketStream,
};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct WsClient {
    url: String,
    token: Option<String>,
    stream: Option<WsStream>,
    max_retries: u32,
    base_backoff_ms: u64,
}

impl WsClient {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            token: None,
            stream: None,
            max_retries: 5,
            base_backoff_ms: 1000,
        }
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    pub async fn connect(&mut self) -> Result<()> {
        let mut request = self.url.as_str().into_client_request()?;

        if let Some(token) = &self.token {
            let auth_value = HeaderValue::from_str(&format!("Bearer {}", token))?;
            request.headers_mut().insert("Authorization", auth_value);
        }

        let (ws_stream, _) = connect_async(request)
            .await
            .context("Failed to connect to WebSocket")?;

        self.stream = Some(ws_stream);
        Ok(())
    }

    pub async fn send(&mut self, text: impl Into<String>) -> Result<()> {
        let stream = self.stream.as_mut().context("Not connected")?;
        stream.send(Message::Text(text.into().into())).await?;
        Ok(())
    }

    pub async fn receive(&mut self) -> Result<Option<String>> {
        let stream = self.stream.as_mut().context("Not connected")?;

        loop {
            match stream.next().await {
                Some(Ok(Message::Text(text))) => return Ok(Some(text.to_string())),
                Some(Ok(Message::Close(_))) => return Ok(None),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(None),
            }
        }
    }

    pub async fn connect_with_retry(&mut self) -> Result<()> {
        for attempt in 0..self.max_retries {
            match self.connect().await {
                Ok(_) => return Ok(()),
                Err(e) if attempt == self.max_retries - 1 => return Err(e),
                Err(_) => {
                    let backoff = self.base_backoff_ms * 2_u64.pow(attempt);
                    sleep(Duration::from_millis(backoff)).await;
                }
            }
        }
        unreachable!()
    }

    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_client_creation() {
        let client = WsClient::new("ws://localhost:8080");
        assert!(!client.is_connected());
        assert_eq!(client.url, "ws://localhost:8080");
    }

    #[tokio::test]
    async fn test_client_with_token() {
        let client = WsClient::new("ws://localhost:8080").with_token("test-token");
        assert_eq!(client.token, Some("test-token".to_string()));
    }

    #[tokio::test]
    async fn test_send_without_connection() {
        let mut client = WsClient::new("ws://localhost:8080");
        let result = client.send("test").await;
        assert!(result.is_err());
    }
}
