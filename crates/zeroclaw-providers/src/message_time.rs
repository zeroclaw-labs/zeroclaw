//! Request-only rendering of structured message time. Never parses body text.

use std::borrow::Cow;

use crate::ChatMessage;

/// Composite adapters must pass metadata through until physical dispatch.
/// Consuming the timestamp on the temporary copy makes nested dispatch safe.
pub(crate) fn project(messages: &[ChatMessage], composite: bool) -> Cow<'_, [ChatMessage]> {
    if composite || !messages.iter().any(|message| message.timestamp.is_some()) {
        return Cow::Borrowed(messages);
    }
    let mut projected = messages.to_vec();
    for message in &mut projected {
        let timestamp = message.timestamp.take();
        if message.role == "user"
            && let Some(timestamp) = timestamp
            && let Some(time) = chrono::DateTime::from_timestamp_millis(timestamp)
        {
            let time = time.with_timezone(&chrono::Local);
            message.content = format!(
                "[CURRENT DATE & TIME: {}]\n\n{}",
                time.format("%Y-%m-%d %H:%M:%S %Z (%A)"),
                message.content,
            );
        }
    }
    Cow::Owned(projected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatRequest, ModelProvider, ProviderDispatch};
    use futures_util::{StreamExt, stream};
    use std::sync::Arc;
    use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use zeroclaw_api::model_provider::{StreamChunk, StreamEvent, StreamOptions, StreamResult};

    struct Echo;

    impl Attributable for Echo {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }
        fn alias(&self) -> &str {
            "timestamp-fixture"
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for Echo {
        async fn chat_with_system(
            &self,
            _system: Option<&str>,
            message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(message.to_owned())
        }

        fn stream_chat_with_system(
            &self,
            _system: Option<&str>,
            message: &str,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
            stream::iter([
                Ok(StreamChunk::delta(message)),
                Ok(StreamChunk::final_chunk()),
            ])
            .boxed()
        }
    }

    #[tokio::test]
    async fn dispatch_projects_time_for_owned_borrowed_chat_tools_and_streams() {
        let history = vec![ChatMessage::user("literal input").with_timestamp(1_800_000_000_000)];
        let restored: Vec<ChatMessage> =
            serde_json::from_slice(&serde_json::to_vec(&history).unwrap()).unwrap();
        let expected = project(&restored, false)[0].content.clone();
        let request = ChatRequest {
            messages: &restored,
            tools: None,
            thinking: None,
        };
        let provider: Arc<dyn ModelProvider> = Arc::new(Echo);
        let owned = ProviderDispatch::new(provider.clone());
        let borrowed = ProviderDispatch::from_ref(provider.as_ref());
        let responses = [
            owned
                .chat(request, "model", None)
                .await
                .unwrap()
                .text
                .unwrap(),
            borrowed
                .chat(request, "model", None)
                .await
                .unwrap()
                .text
                .unwrap(),
            owned
                .chat_with_history(&restored, "model", None)
                .await
                .unwrap(),
            borrowed
                .chat_with_history(&restored, "model", None)
                .await
                .unwrap(),
            owned
                .chat_with_tools(&restored, &[], "model", None)
                .await
                .unwrap()
                .text
                .unwrap(),
            borrowed
                .chat_with_tools(&restored, &[], "model", None)
                .await
                .unwrap()
                .text
                .unwrap(),
        ];
        assert!(responses.iter().all(|response| response == &expected));
        let options = StreamOptions::new(true);
        for events in [
            owned.stream_chat(request, "model", None, options),
            borrowed.stream_chat(request, "model", None, options),
        ] {
            let chunks = events.collect::<Vec<_>>().await;
            assert!(
                matches!(&chunks[0], Ok(StreamEvent::TextDelta(chunk)) if chunk.delta == expected)
            );
        }
        for chunks in [
            owned.stream_chat_with_history(&restored, "model", None, options),
            borrowed.stream_chat_with_history(&restored, "model", None, options),
        ] {
            let chunks = chunks.collect::<Vec<_>>().await;
            assert_eq!(chunks[0].as_ref().unwrap().delta, expected);
        }
        assert_eq!(restored[0].content, "literal input");
        assert_eq!(restored[0].timestamp, history[0].timestamp);
    }

    #[test]
    fn request_projection_preserves_history_and_is_not_reapplied() {
        let messages = vec![
            ChatMessage::user("original body").with_timestamp(1_800_000_000_000),
            ChatMessage::assistant("answer").with_timestamp(1_800_000_000_001),
            ChatMessage::user("[2020-01-01 00:00:00 UTC] literal legacy body"),
        ];
        let encoded = serde_json::to_vec(&messages).unwrap();
        let restored: Vec<ChatMessage> = serde_json::from_slice(&encoded).unwrap();
        let projected = project(&restored, false);
        assert!(projected[0].content.starts_with("[CURRENT DATE & TIME: "));
        assert!(projected[0].content.ends_with("\n\noriginal body"));
        assert_eq!(projected[0].timestamp, None);
        assert_eq!(projected[1].content, "answer");
        assert_eq!(projected[2].content, messages[2].content);
        assert_eq!(serde_json::to_vec(&messages).unwrap(), encoded);
        assert_eq!(serde_json::to_vec(&restored).unwrap(), encoded);
        assert!(matches!(project(&projected, false), Cow::Borrowed(_)));
        assert!(matches!(project(&messages, true), Cow::Borrowed(_)));
    }
}
