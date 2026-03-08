use async_trait::async_trait;
use futures_util::{stream, StreamExt};

use super::traits::{ChatMessage, StreamChunk, StreamOptions, StreamResult};

pub type BoxStream = stream::BoxStream<'static, StreamResult<StreamChunk>>;

#[async_trait]
pub trait ProviderStreaming: Send + Sync {
    fn supports_streaming(&self) -> bool;

    fn stream_chat(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
        options: StreamOptions,
    ) -> BoxStream;

    fn stream_chat_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
        options: StreamOptions,
    ) -> BoxStream {
        let _ = (messages, model, temperature, options);
        let chunk = StreamChunk::error(
            "streaming with history not supported for this provider".to_string(),
        );
        stream::once(async move { Ok(chunk) }).boxed()
    }
}

pub fn not_supported_stream() -> BoxStream {
    let chunk = StreamChunk::error("provider does not support streaming");
    stream::once(async move { Ok(chunk) }).boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockStreamer;

    #[async_trait]
    impl ProviderStreaming for MockStreamer {
        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _system_prompt: Option<&str>,
            message: &str,
            _model: &str,
            _temperature: f64,
            _options: StreamOptions,
        ) -> BoxStream {
            let text = format!("echo: {message}");
            let chunk = StreamChunk::delta(text);
            let final_chunk = StreamChunk::final_chunk();
            stream::iter(vec![Ok(chunk), Ok(final_chunk)]).boxed()
        }
    }

    #[test]
    fn mock_streamer_supports_streaming() {
        let s = MockStreamer;
        assert!(s.supports_streaming());
    }

    #[tokio::test]
    async fn mock_streamer_produces_chunks() {
        let s = MockStreamer;
        let stream = s.stream_chat(None, "hello", "test-model", 0.7, StreamOptions::new(true));
        let chunks: Vec<_> = stream.collect().await;
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].as_ref().unwrap().delta.contains("echo: hello"));
        assert!(chunks[1].as_ref().unwrap().is_final);
    }

    #[tokio::test]
    async fn not_supported_returns_error_chunk() {
        let stream = not_supported_stream();
        let chunks: Vec<_> = stream.collect().await;
        assert_eq!(chunks.len(), 1);
        let chunk = chunks[0].as_ref().unwrap();
        assert!(chunk.is_final);
        assert!(chunk.delta.contains("does not support"));
    }

    #[tokio::test]
    async fn default_stream_chat_history_returns_error() {
        let s = MockStreamer;
        let messages = vec![ChatMessage::user("test")];
        let stream = s.stream_chat_history(&messages, "model", 0.7, StreamOptions::new(true));
        let chunks: Vec<_> = stream.collect().await;
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].as_ref().unwrap().delta.contains("not supported"));
    }
}
