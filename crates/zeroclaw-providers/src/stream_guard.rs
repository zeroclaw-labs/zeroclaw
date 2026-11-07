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

/// Parser futures are inert until the stream is first polled. Provider
/// dispatch commits admission before polling, so creating or dropping an
/// unpolled stream cannot send a request. The guard still aborts a running
/// parser immediately when its consumer is dropped.
pub(crate) fn parser_stream<T, F>(
    parser: F,
    rx: tokio::sync::mpsc::Receiver<T>,
) -> futures_util::stream::BoxStream<'static, T>
where
    T: Send + 'static,
    F: std::future::Future<Output = ()> + Send + 'static,
{
    use futures_util::StreamExt;
    futures_util::stream::unfold(
        (rx, Some(parser), None),
        |(mut rx, mut parser, mut guard)| async move {
            if let Some(parser) = parser.take() {
                let handle = ::zeroclaw_spawn::spawn!(parser);
                guard = Some(AbortOnDrop::new(handle.abort_handle()));
            }
            rx.recv().await.map(|event| (event, (rx, parser, guard)))
        },
    )
    .boxed()
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
        let _ = tx.send(Ok(::zeroclaw_api::model_provider::StreamEvent::ProtocolMetadata(
            serde_json::json!({"kind":"completion_signal","observed":"required_completion_signal","signal":completion_signal}),
        ))).await;
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
        .send(Err(
            ::zeroclaw_api::model_provider::StreamError::MissingFinal(completion_signal.into()),
        ))
        .await;
}

#[cfg(test)]
mod tests {
    use ::zeroclaw_api::model_provider::{StreamError, StreamEvent, StreamResult};

    #[tokio::test]
    async fn unpolled_parser_is_inert_and_polled_parser_is_aborted_on_drop() {
        use futures_util::StreamExt;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let started = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));
        struct Stop(Arc<AtomicBool>);
        impl Drop for Stop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let running = started.clone();
        let stop = stopped.clone();
        let mut stream = super::parser_stream(
            async move {
                running.store(true, Ordering::SeqCst);
                let _stop = Stop(stop);
                tx.send(1).await.expect("receiver");
                std::future::pending::<()>().await;
            },
            rx,
        );
        tokio::task::yield_now().await;
        assert!(!started.load(Ordering::SeqCst));
        assert_eq!(stream.next().await, Some(1));
        assert!(started.load(Ordering::SeqCst));
        drop(stream);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !stopped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("parser aborted");
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let running = started.clone();
        started.store(false, Ordering::SeqCst);
        drop(super::parser_stream(
            async move {
                running.store(true, Ordering::SeqCst);
                tx.send(1).await.expect("receiver");
            },
            rx,
        ));
        tokio::task::yield_now().await;
        assert!(!started.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn finish_emits_final_when_completion_seen() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(4);
        super::finish_sse_stream(&tx, true, "message_stop").await;
        assert!(matches!(
            rx.recv().await,
            Some(Ok(StreamEvent::ProtocolMetadata(_)))
        ));
        assert!(matches!(rx.recv().await, Some(Ok(StreamEvent::Final))));
    }

    #[tokio::test]
    async fn finish_emits_truncation_error_without_completion() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(4);
        super::finish_sse_stream(&tx, false, "message_stop").await;
        match rx.recv().await {
            Some(Err(StreamError::MissingFinal(msg))) => {
                assert_eq!(msg, "message_stop");
            }
            other => panic!("expected truncation StreamError, got {other:?}"),
        }
    }
}
