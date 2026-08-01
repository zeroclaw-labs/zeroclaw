//! Ties a spawned streaming-parser task's lifetime to the stream the consumer
//! holds, so dropping the stream (turn cancel, timeout, client disconnect)
//! aborts the task and releases its socket instead of leaking it.

/// Aborts the wrapped task when dropped. Carry it inside the returned stream's
/// `unfold` state so the abort fires exactly when the consumer drops the
/// stream. `AbortHandle::abort` is a no-op once the task has finished, so the
/// happy path is unaffected.
pub(crate) struct AbortOnDrop(tokio::task::AbortHandle);

impl AbortOnDrop {
    pub(crate) fn new(handle: tokio::task::AbortHandle) -> Self {
        Self(handle)
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if self.0.is_finished() {
            return;
        }
        self.0.abort();
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Kill)
                .with_category(::zeroclaw_log::EventCategory::Provider)
                .with_outcome(::zeroclaw_log::EventOutcome::Success),
            "stream: consumer dropped — aborting detached parser task to release socket"
        );
    }
}

pub(crate) async fn finish_sse_stream(
    tx: &tokio::sync::mpsc::Sender<
        ::zeroclaw_api::model_provider::StreamResult<::zeroclaw_api::model_provider::StreamEvent>,
    >,
    saw_completion: bool,
    completion_signal: &str,
) {
    if saw_completion {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                .with_category(::zeroclaw_log::EventCategory::Provider)
                .with_outcome(::zeroclaw_log::EventOutcome::Success),
            "stream: SSE parser reached end of stream, emitting Final"
        );
        let _ = tx
            .send(Ok(::zeroclaw_api::model_provider::StreamEvent::Final))
            .await;
        return;
    }
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_category(::zeroclaw_log::EventCategory::Provider)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "completion_signal": completion_signal,
            })),
        "stream: SSE connection closed before completion signal — truncated response, surfacing error"
    );
    let _ = tx
        .send(Err(::zeroclaw_api::model_provider::StreamError::Http(
            format!("SSE stream closed before {completion_signal}: response truncated"),
        )))
        .await;
}

#[cfg(test)]
mod tests {
    use ::zeroclaw_api::model_provider::{StreamError, StreamEvent, StreamResult};

    #[tokio::test]
    async fn finish_emits_final_when_completion_seen() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(4);
        super::finish_sse_stream(&tx, true, "message_stop").await;
        assert!(matches!(rx.recv().await, Some(Ok(StreamEvent::Final))));
    }

    #[tokio::test]
    async fn finish_emits_truncation_error_without_completion() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(4);
        super::finish_sse_stream(&tx, false, "message_stop").await;
        match rx.recv().await {
            Some(Err(StreamError::Http(msg))) => {
                assert!(msg.contains("truncated"), "got: {msg}");
                assert!(msg.contains("message_stop"), "got: {msg}");
            }
            other => panic!("expected truncation StreamError, got {other:?}"),
        }
    }
}
