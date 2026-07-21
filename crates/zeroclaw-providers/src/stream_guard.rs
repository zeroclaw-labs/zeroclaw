//! Ties a spawned streaming-parser task's lifetime to the stream the consumer
//! holds, so dropping the stream (turn cancel, timeout, client disconnect)
//! aborts the task and releases its socket instead of leaking it.

use ::zeroclaw_api::model_provider::StreamError;
use futures_util::StreamExt;

/// Per-read idle bound shared by every SSE parser. A local runtime (llama.cpp,
/// Ollama, vLLM) or a proxy can accept the request, emit 200 headers, then stall
/// without sending a chunk; the streaming clients carry no total timeout by
/// design, so an idle upstream would leave the parser parked forever and the turn
/// hangs on "working". Bounding each read converts that stall into a retryable
/// StreamError.
pub(crate) const SSE_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// One idle-bounded read from an SSE line reader.
#[derive(Debug)]
pub(crate) enum SseLine {
    Line(String),
    Eof,
    Err(StreamError),
}

/// Wrap an HTTP response body as a line reader. The single place that turns a
/// reqwest byte stream into an `AsyncBufRead`, so every provider frames lines and
/// handles UTF-8 identically.
pub(crate) fn sse_reader(response: reqwest::Response) -> impl tokio::io::AsyncBufRead + Unpin {
    let byte_stream = response
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    tokio::io::BufReader::new(tokio_util::io::StreamReader::new(byte_stream))
}

/// Read the next SSE line under the shared idle bound, mapping read errors and
/// stalls onto `StreamError`. The one copy of the timeout/error/idle arms every
/// provider parser shares.
macro_rules! next_line_or_break {
    ($lines:expr, $tx:expr) => {
        match $crate::stream_guard::next_sse_line(&mut $lines).await {
            $crate::stream_guard::SseLine::Line(line) => line,
            $crate::stream_guard::SseLine::Eof => break,
            $crate::stream_guard::SseLine::Err(e) => {
                let _ = $tx.send(Err(e)).await;
                return;
            }
        }
    };
}
pub(crate) use next_line_or_break;

pub(crate) async fn next_sse_line<R>(lines: &mut tokio::io::Lines<R>) -> SseLine
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    match tokio::time::timeout(SSE_IDLE_TIMEOUT, lines.next_line()).await {
        Ok(Ok(Some(line))) => SseLine::Line(line),
        Ok(Ok(None)) => SseLine::Eof,
        Ok(Err(err)) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_category(::zeroclaw_log::EventCategory::Provider)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({ "error": format!("{err}") })),
                "stream: SSE read error, aborting stream"
            );
            SseLine::Err(StreamError::Http(format!("SSE read error: {err}")))
        }
        Err(_) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_category(::zeroclaw_log::EventCategory::Provider)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({ "idle_secs": SSE_IDLE_TIMEOUT.as_secs() })),
                "stream: SSE idle timeout, connection stalled, aborting stream"
            );
            SseLine::Err(StreamError::Http(format!(
                "SSE stream stalled: no data for {}s",
                SSE_IDLE_TIMEOUT.as_secs()
            )))
        }
    }
}

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

    struct StallAfterReader {
        data: std::io::Cursor<Vec<u8>>,
        drained: bool,
    }

    impl tokio::io::AsyncRead for StallAfterReader {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if self.drained {
                return std::task::Poll::Pending;
            }
            let before = buf.filled().len();
            let inner = std::pin::Pin::new(&mut self.data);
            let res = inner.poll_read(cx, buf);
            if buf.filled().len() == before {
                self.drained = true;
                return std::task::Poll::Pending;
            }
            res
        }
    }

    #[tokio::test]
    async fn next_sse_line_reads_lines_then_eof() {
        use tokio::io::AsyncBufReadExt;
        let reader = tokio::io::BufReader::new(std::io::Cursor::new(b"one\ntwo\n".to_vec()));
        let mut lines = reader.lines();
        assert!(matches!(
            super::next_sse_line(&mut lines).await,
            super::SseLine::Line(ref l) if l == "one"
        ));
        assert!(matches!(
            super::next_sse_line(&mut lines).await,
            super::SseLine::Line(ref l) if l == "two"
        ));
        assert!(matches!(
            super::next_sse_line(&mut lines).await,
            super::SseLine::Eof
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn next_sse_line_times_out_when_idle() {
        use tokio::io::AsyncBufReadExt;
        let reader = tokio::io::BufReader::new(StallAfterReader {
            data: std::io::Cursor::new(b"one\n".to_vec()),
            drained: false,
        });
        let mut lines = reader.lines();
        assert!(matches!(
            super::next_sse_line(&mut lines).await,
            super::SseLine::Line(ref l) if l == "one"
        ));

        let pump = ::zeroclaw_spawn::spawn!(async move { super::next_sse_line(&mut lines).await });
        tokio::task::yield_now().await;
        tokio::time::advance(super::SSE_IDLE_TIMEOUT + std::time::Duration::from_secs(1)).await;

        match pump.await.expect("pump task must finish, not hang") {
            super::SseLine::Err(StreamError::Http(msg)) => {
                assert!(msg.contains("stalled"), "got: {msg}");
            }
            other => panic!("expected stalled Http error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn next_sse_line_surfaces_read_error() {
        use tokio::io::AsyncBufReadExt;
        struct ErrReader;
        impl tokio::io::AsyncRead for ErrReader {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                _buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Err(std::io::Error::other("boom")))
            }
        }
        let reader = tokio::io::BufReader::new(ErrReader);
        let mut lines = reader.lines();
        match super::next_sse_line(&mut lines).await {
            super::SseLine::Err(StreamError::Http(msg)) => {
                assert!(msg.contains("read error"), "got: {msg}");
            }
            other => panic!("expected read-error Http error, got: {other:?}"),
        }
    }

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
