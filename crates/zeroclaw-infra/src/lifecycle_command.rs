//! Bounded external-command projection for content-free lifecycle events.

use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use tokio::task::{AbortHandle, JoinError, JoinSet};
use zeroclaw_api::lifecycle::{
    ApprovalOutcome, LifecycleCorrelation, LifecycleEventKind, LifecycleEventV1, LifecycleMapper,
    LifecycleSignal,
};
use zeroclaw_api::observability_traits::{Observer, ObserverEvent, ObserverMetric};

const TERMINAL_QUEUE_CAPACITY: usize = 8;
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Maximum Unicode scalar values in a configured command name.
pub const LIFECYCLE_COMMAND_NAME_CHARS_MAX: usize = 64;
/// Maximum queued non-terminal events per configured command.
pub const LIFECYCLE_COMMAND_QUEUE_MAX: usize = 1024;
/// Maximum child processes running concurrently per configured command.
pub const LIFECYCLE_COMMAND_CONCURRENCY_MAX: usize = 16;
/// Maximum per-child wall-clock timeout in milliseconds.
pub const LIFECYCLE_COMMAND_TIMEOUT_MS_MAX: u64 = 60_000;
/// Maximum bytes retained independently from stdout and stderr.
pub const LIFECYCLE_COMMAND_OUTPUT_BYTES_MAX: usize = 1_048_576;
/// Maximum executable-plus-argument entries in literal argv.
pub const LIFECYCLE_COMMAND_ARGV_MAX: usize = 64;
/// Maximum bytes in one literal argv entry.
pub const LIFECYCLE_COMMAND_ARG_BYTES_MAX: usize = 4096;

/// Fully validated command-runner configuration independent of config storage.
#[derive(Debug, Clone)]
pub struct LifecycleCommandSpec {
    /// Stable operator-facing identity used only in diagnostics.
    pub name: String,
    /// Absolute executable followed by literal arguments.
    pub argv: Vec<String>,
    /// Explicit event allowlist.
    pub events: HashSet<LifecycleEventKind>,
    /// Maximum queued non-terminal events.
    pub queue_capacity: usize,
    /// Maximum simultaneously running children.
    pub max_concurrency: usize,
    /// Per-child wall-clock timeout.
    pub timeout: Duration,
    /// Maximum retained bytes from each output stream.
    pub max_output_bytes: usize,
}

impl LifecycleCommandSpec {
    /// Validate transport-independent command invariants.
    ///
    /// # Errors
    ///
    /// Returns an error for a blank name, relative/missing executable, empty
    /// event allowlist, or zero queue/concurrency/timeout bounds.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.name.trim().is_empty() {
            anyhow::bail!("lifecycle command name must not be blank");
        }
        if self.name.chars().count() > LIFECYCLE_COMMAND_NAME_CHARS_MAX
            || self.name.chars().any(char::is_control)
        {
            anyhow::bail!(
                "lifecycle command name must contain at most {LIFECYCLE_COMMAND_NAME_CHARS_MAX} non-control characters"
            );
        }
        let Some(executable) = self.argv.first() else {
            anyhow::bail!("lifecycle command argv must not be empty");
        };
        if !std::path::Path::new(executable).is_absolute() {
            anyhow::bail!("lifecycle command executable must be absolute");
        }
        if self.argv.len() > LIFECYCLE_COMMAND_ARGV_MAX
            || self
                .argv
                .iter()
                .any(|arg| arg.len() > LIFECYCLE_COMMAND_ARG_BYTES_MAX)
        {
            anyhow::bail!(
                "lifecycle command argv exceeds the {LIFECYCLE_COMMAND_ARGV_MAX}-entry or {LIFECYCLE_COMMAND_ARG_BYTES_MAX}-byte-per-entry bound"
            );
        }
        if self.events.is_empty() {
            anyhow::bail!("lifecycle command event allowlist must not be empty");
        }
        if !(1..=LIFECYCLE_COMMAND_QUEUE_MAX).contains(&self.queue_capacity) {
            anyhow::bail!(
                "lifecycle command queue capacity must be 1..={LIFECYCLE_COMMAND_QUEUE_MAX}"
            );
        }
        if !(1..=LIFECYCLE_COMMAND_CONCURRENCY_MAX).contains(&self.max_concurrency) {
            anyhow::bail!(
                "lifecycle command concurrency must be 1..={LIFECYCLE_COMMAND_CONCURRENCY_MAX}"
            );
        }
        if self.timeout.is_zero()
            || self.timeout > Duration::from_millis(LIFECYCLE_COMMAND_TIMEOUT_MS_MAX)
        {
            anyhow::bail!(
                "lifecycle command timeout must be 1..={LIFECYCLE_COMMAND_TIMEOUT_MS_MAX} milliseconds"
            );
        }
        if self.max_output_bytes > LIFECYCLE_COMMAND_OUTPUT_BYTES_MAX {
            anyhow::bail!(
                "lifecycle command output bound must be 0..={LIFECYCLE_COMMAND_OUTPUT_BYTES_MAX} bytes per stream"
            );
        }
        Ok(())
    }
}

#[derive(Debug)]
struct QueuedEvent(LifecycleEventV1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnqueueError {
    Full,
    Closed,
}

struct LifecycleCommandDispatcher {
    name: String,
    events: HashSet<LifecycleEventKind>,
    normal: mpsc::Sender<QueuedEvent>,
    terminal: mpsc::Sender<QueuedEvent>,
    closed: AtomicBool,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    drain_done: Mutex<Option<std_mpsc::Receiver<()>>>,
}

impl LifecycleCommandDispatcher {
    fn new(spec: LifecycleCommandSpec) -> anyhow::Result<Self> {
        spec.validate()?;
        let (normal_tx, normal_rx) = mpsc::channel(spec.queue_capacity);
        let (terminal_tx, terminal_rx) = mpsc::channel(TERMINAL_QUEUE_CAPACITY);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (drain_done_tx, drain_done_rx) = std_mpsc::sync_channel(1);
        let worker_spec = Arc::new(spec.clone());
        std::thread::Builder::new()
            .name(format!("zc-lifecycle-{}", spec.name))
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    let _ = drain_done_tx.send(());
                    return;
                };
                runtime.block_on(run_dispatcher(
                    normal_rx,
                    terminal_rx,
                    shutdown_rx,
                    Arc::clone(&worker_spec),
                    move |event| {
                        let spec = Arc::clone(&worker_spec);
                        async move { deliver_and_record(&spec, event).await }
                    },
                ));
                let _ = drain_done_tx.send(());
            })
            .map_err(|error| {
                anyhow::Error::msg(format!("failed to start lifecycle command worker: {error}"))
            })?;
        Ok(Self {
            name: spec.name,
            events: spec.events,
            normal: normal_tx,
            terminal: terminal_tx,
            closed: AtomicBool::new(false),
            shutdown: Mutex::new(Some(shutdown_tx)),
            drain_done: Mutex::new(Some(drain_done_rx)),
        })
    }

    fn enqueue(&self, event: LifecycleEventV1) {
        if !self.events.contains(&event.event) {
            return;
        }
        let event_kind = event.event;
        if self.closed.load(Ordering::Acquire) {
            record_delivery_issue(
                &self.name,
                event_kind,
                "lifecycle command worker is closed; event dropped",
            );
            return;
        }
        let result = try_enqueue_event(&self.normal, &self.terminal, event);
        match result {
            Ok(()) => {}
            Err(EnqueueError::Full) => record_delivery_issue(
                &self.name,
                event_kind,
                "lifecycle command queue is full; event dropped",
            ),
            Err(EnqueueError::Closed) => record_delivery_issue(
                &self.name,
                event_kind,
                "lifecycle command worker is closed; event dropped",
            ),
        }
    }

    fn begin_shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(shutdown) = self.shutdown.lock().take() {
            let _ = shutdown.send(());
        }
    }

    fn await_shutdown(&self, timeout: Duration) {
        if let Some(receiver) = self.drain_done.lock().take() {
            let _ = receiver.recv_timeout(timeout);
        }
    }
}

fn try_enqueue_event(
    normal: &mpsc::Sender<QueuedEvent>,
    terminal: &mpsc::Sender<QueuedEvent>,
    event: LifecycleEventV1,
) -> Result<(), EnqueueError> {
    let result = if matches!(
        event.event,
        LifecycleEventKind::TurnCompleted | LifecycleEventKind::SessionEnded
    ) {
        terminal.try_send(QueuedEvent(event))
    } else {
        normal.try_send(QueuedEvent(event))
    };
    result.map_err(|error| match error {
        mpsc::error::TrySendError::Full(_) => EnqueueError::Full,
        mpsc::error::TrySendError::Closed(_) => EnqueueError::Closed,
    })
}

/// Observer that maps core events once and fans content-free payloads to
/// configured command dispatchers.
pub struct LifecycleCommandObserver {
    mapper: Mutex<LifecycleMapper>,
    dispatchers: Vec<LifecycleCommandDispatcher>,
}

impl LifecycleCommandObserver {
    /// Construct an observer only when at least one valid command is present.
    ///
    /// # Errors
    ///
    /// Returns an error when any command specification is invalid or its worker
    /// thread cannot start.
    pub fn from_specs(specs: Vec<LifecycleCommandSpec>) -> anyhow::Result<Option<Self>> {
        if specs.is_empty() {
            return Ok(None);
        }
        let dispatchers = specs
            .into_iter()
            .map(LifecycleCommandDispatcher::new)
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Some(Self {
            mapper: Mutex::new(LifecycleMapper::default()),
            dispatchers,
        }))
    }

    fn project(&self, event: &ObserverEvent) -> Vec<LifecycleEventV1> {
        let Some(signals) = lifecycle_signals(event) else {
            return Vec::new();
        };
        let timestamp = unix_time_ms();
        let mut mapper = self.mapper.lock();
        signals
            .into_iter()
            .filter_map(|(signal, correlation)| {
                match mapper.project(signal, correlation, timestamp) {
                    Ok(projected) => projected,
                    Err(error) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Skip
                            )
                            .with_attrs(::serde_json::json!({"error": error.to_string()})),
                            "lifecycle event skipped because correlation was invalid"
                        );
                        None
                    }
                }
            })
            .collect()
    }

    fn flush_with_timeout(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        for dispatcher in &self.dispatchers {
            dispatcher.begin_shutdown();
        }
        for dispatcher in &self.dispatchers {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            dispatcher.await_shutdown(remaining);
        }
    }
}

impl Observer for LifecycleCommandObserver {
    fn record_event(&self, event: &ObserverEvent) {
        for projected in self.project(event) {
            for dispatcher in &self.dispatchers {
                dispatcher.enqueue(projected.clone());
            }
        }
    }

    fn record_metric(&self, _metric: &ObserverMetric) {}

    fn flush(&self) {
        self.flush_with_timeout(SHUTDOWN_DRAIN_TIMEOUT);
    }

    fn name(&self) -> &str {
        "lifecycle-command"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn lifecycle_signals(
    event: &ObserverEvent,
) -> Option<Vec<(LifecycleSignal, LifecycleCorrelation)>> {
    match event {
        ObserverEvent::AgentStart {
            agent_alias,
            turn_id,
            ..
        } => {
            let turn = turn_id.as_deref()?;
            let turn_correlation = correlation(agent_alias.as_deref(), turn, true)?;
            let mut session_correlation = turn_correlation.clone();
            session_correlation.turn_id = None;
            Some(vec![
                (LifecycleSignal::SessionStarted, session_correlation),
                (LifecycleSignal::TurnStarted, turn_correlation),
            ])
        }
        ObserverEvent::ToolCallStart {
            tool,
            agent_alias,
            turn_id,
            ..
        } => Some(vec![(
            LifecycleSignal::ToolStarted {
                tool_name: Some(tool.clone()),
            },
            correlation(agent_alias.as_deref(), turn_id.as_deref()?, true)?,
        )]),
        ObserverEvent::ToolCall {
            tool,
            success,
            agent_alias,
            turn_id,
            ..
        } => Some(vec![(
            LifecycleSignal::ToolCompleted {
                tool_name: Some(tool.clone()),
                success: *success,
            },
            correlation(agent_alias.as_deref(), turn_id.as_deref()?, true)?,
        )]),
        ObserverEvent::AuthorizationRequested {
            tool_name,
            agent_alias,
            turn_id,
            ..
        } => Some(vec![(
            LifecycleSignal::ApprovalRequested {
                tool_name: Some(tool_name.clone()),
            },
            correlation(agent_alias.as_deref(), turn_id.as_deref()?, true)?,
        )]),
        ObserverEvent::AuthorizationResponded {
            tool_name,
            granted,
            agent_alias,
            turn_id,
            ..
        } => Some(vec![(
            LifecycleSignal::ApprovalResponded {
                tool_name: Some(tool_name.clone()),
                outcome: if *granted {
                    ApprovalOutcome::Granted
                } else {
                    ApprovalOutcome::Denied
                },
            },
            correlation(agent_alias.as_deref(), turn_id.as_deref()?, true)?,
        )]),
        ObserverEvent::AgentEnd {
            agent_alias,
            turn_id,
            ..
        } => {
            let turn = turn_id.as_deref()?;
            let turn_correlation = correlation(agent_alias.as_deref(), turn, true)?;
            let mut session_correlation = turn_correlation.clone();
            session_correlation.turn_id = None;
            Some(vec![
                (LifecycleSignal::TurnCompleted, turn_correlation),
                (LifecycleSignal::SessionEnded, session_correlation),
            ])
        }
        _ => None,
    }
}

fn correlation(
    agent_alias: Option<&str>,
    turn_id: &str,
    include_turn: bool,
) -> Option<LifecycleCorrelation> {
    let session_source = zeroclaw_api::TOOL_LOOP_SESSION_KEY
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .filter(|session| !session.trim().is_empty())
        .unwrap_or_else(|| turn_id.to_string());
    LifecycleCorrelation::new(
        agent_alias.unwrap_or("default"),
        opaque_correlation_id("session", &session_source),
        include_turn.then(|| opaque_correlation_id("turn", turn_id)),
    )
    .ok()
}

fn opaque_correlation_id(domain: &str, source: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"zeroclaw-lifecycle-v1\0");
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(source.as_bytes());
    let digest = hasher.finalize();
    let mut id = String::with_capacity(35);
    id.push_str(if domain == "session" { "ls_" } else { "lt_" });
    for byte in &digest[..16] {
        let _ = write!(id, "{byte:02x}");
    }
    id
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

async fn run_dispatcher<F, Fut>(
    mut normal_rx: mpsc::Receiver<QueuedEvent>,
    mut terminal_rx: mpsc::Receiver<QueuedEvent>,
    mut shutdown_rx: oneshot::Receiver<()>,
    spec: Arc<LifecycleCommandSpec>,
    deliver: F,
) where
    F: Fn(LifecycleEventV1) -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let mut jobs = JoinSet::new();
    let mut in_flight: HashMap<String, Vec<AbortHandle>> = HashMap::new();
    let mut terminal_sequence: HashMap<String, u64> = HashMap::new();
    loop {
        tokio::select! {
            biased;
            Some(QueuedEvent(event)) = terminal_rx.recv() => {
                let session_id = event.correlation.session_id.clone();
                let aborted = in_flight.remove(&session_id).unwrap_or_default();
                for handle in &aborted {
                    handle.abort();
                }
                while aborted.iter().any(|handle| !handle.is_finished()) {
                    let Some(result) = jobs.join_next().await else {
                        break;
                    };
                    record_join_result(&spec.name, result);
                }
                prune_finished(&mut in_flight);
                while jobs.len() >= spec.max_concurrency {
                    let Some(result) = jobs.join_next().await else {
                        break;
                    };
                    record_join_result(&spec.name, result);
                    prune_finished(&mut in_flight);
                }
                if event.event == LifecycleEventKind::SessionEnded
                    || (event.event == LifecycleEventKind::TurnCompleted
                        && event.state == zeroclaw_api::lifecycle::LifecycleState::Idle)
                {
                    terminal_sequence.insert(session_id, event.sequence);
                }
                deliver.clone()(event).await;
            }
            _ = &mut shutdown_rx => {
                jobs.abort_all();
                while jobs.join_next().await.is_some() {}
                in_flight.clear();
                while let Ok(QueuedEvent(event)) = terminal_rx.try_recv() {
                    terminal_sequence.insert(event.correlation.session_id.clone(), event.sequence);
                    deliver.clone()(event).await;
                }
                break;
            }
            Some(result) = jobs.join_next(), if !jobs.is_empty() => {
                record_join_result(&spec.name, result);
                prune_finished(&mut in_flight);
            }
            maybe_event = normal_rx.recv(), if jobs.len() < spec.max_concurrency => {
                let Some(QueuedEvent(event)) = maybe_event else {
                    if jobs.is_empty() && terminal_rx.is_closed() {
                        break;
                    }
                    continue;
                };
                let stale = terminal_sequence
                    .get(&event.correlation.session_id)
                    .is_some_and(|terminal| event.sequence <= *terminal);
                if !stale {
                    let deliver = deliver.clone();
                    let session_id = event.correlation.session_id.clone();
                    let completed_session = session_id.clone();
                    let handle = jobs.spawn(async move {
                        deliver(event).await;
                        completed_session
                    });
                    in_flight.entry(session_id).or_default().push(handle);
                }
            }
        }
    }
}

fn prune_finished(in_flight: &mut HashMap<String, Vec<AbortHandle>>) {
    in_flight.retain(|_, handles| {
        handles.retain(|handle| !handle.is_finished());
        !handles.is_empty()
    });
}

fn record_join_result(name: &str, result: Result<String, JoinError>) {
    if let Err(error) = result
        && !error.is_cancelled()
    {
        record_delivery_issue(
            name,
            LifecycleEventKind::TurnStarted,
            "lifecycle command delivery task failed",
        );
    }
}

async fn deliver_and_record(spec: &LifecycleCommandSpec, event: LifecycleEventV1) {
    let kind = event.event;
    match execute_command(spec, &event).await {
        Ok(output) if output.success => {}
        Ok(output) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "hook": spec.name,
                        "event": kind.as_str(),
                        "exit_code": output.exit_code,
                        "stdout_bytes": output.stdout.bytes.len(),
                        "stderr_bytes": output.stderr.bytes.len(),
                        "stdout_truncated": output.stdout.truncated,
                        "stderr_truncated": output.stderr.truncated,
                    })),
                "lifecycle command exited unsuccessfully"
            );
        }
        Err(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "hook": spec.name,
                        "event": kind.as_str(),
                        "error": error.to_string(),
                    })),
                "lifecycle command delivery failed"
            );
        }
    }
}

#[derive(Debug)]
struct BoundedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

#[derive(Debug)]
struct CommandOutput {
    success: bool,
    exit_code: Option<i32>,
    stdout: BoundedOutput,
    stderr: BoundedOutput,
}

async fn execute_command(
    spec: &LifecycleCommandSpec,
    event: &LifecycleEventV1,
) -> anyhow::Result<CommandOutput> {
    let payload = serde_json::to_vec(event)?;
    let executable = spec
        .argv
        .first()
        .ok_or_else(|| anyhow::Error::msg("lifecycle command argv is empty"))?;
    let mut command = Command::new(executable);
    command
        .args(&spec.argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let timeout = spec.timeout;
    let max_output_bytes = spec.max_output_bytes;
    tokio::time::timeout(timeout, async move {
        let mut child = command.spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::Error::msg("lifecycle command stdin was not piped"))?;
        stdin.write_all(&payload).await?;
        stdin.shutdown().await?;
        drop(stdin);
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::Error::msg("lifecycle command stdout was not piped"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::Error::msg("lifecycle command stderr was not piped"))?;
        let (status, stdout, stderr) = tokio::join!(
            child.wait(),
            read_bounded(stdout, max_output_bytes),
            read_bounded(stderr, max_output_bytes),
        );
        let status = status?;
        Ok(CommandOutput {
            success: status.success(),
            exit_code: status.code(),
            stdout: stdout?,
            stderr: stderr?,
        })
    })
    .await
    .map_err(|_| anyhow::Error::msg(format!("lifecycle command timed out after {timeout:?}")))?
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    max_bytes: usize,
) -> io::Result<BoundedOutput> {
    let mut bytes = Vec::with_capacity(max_bytes.min(4096));
    let mut truncated = false;
    let mut buffer = [0_u8; 4096];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = max_bytes.saturating_sub(bytes.len());
        let kept = remaining.min(read);
        bytes.extend_from_slice(&buffer[..kept]);
        truncated |= kept < read;
    }
    Ok(BoundedOutput { bytes, truncated })
}

fn record_delivery_issue(name: &str, event: LifecycleEventKind, message: &'static str) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Skip)
            .with_attrs(::serde_json::json!({"hook": name, "event": event.as_str()})),
        message
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ActiveDelivery(Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for ActiveDelivery {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn event(kind: LifecycleEventKind, sequence: u64) -> LifecycleEventV1 {
        event_for_session(kind, sequence, "session")
    }

    fn event_for_session(
        kind: LifecycleEventKind,
        sequence: u64,
        session_id: &str,
    ) -> LifecycleEventV1 {
        LifecycleEventV1 {
            schema_version: zeroclaw_api::lifecycle::LIFECYCLE_SCHEMA_VERSION,
            sequence,
            timestamp_unix_ms: 1,
            event: kind,
            state: if kind == LifecycleEventKind::SessionEnded {
                zeroclaw_api::lifecycle::LifecycleState::Done
            } else {
                zeroclaw_api::lifecycle::LifecycleState::Working
            },
            correlation: LifecycleCorrelation::new(
                "agent",
                session_id,
                (kind != LifecycleEventKind::SessionEnded).then(|| "turn".into()),
            )
            .unwrap(),
            tool_name: None,
            tool_success: None,
            approval_outcome: None,
        }
    }

    #[test]
    fn empty_specs_create_no_observer_or_worker() {
        assert!(
            LifecycleCommandObserver::from_specs(Vec::new())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn shutdown_signals_every_dispatcher_before_waiting_on_any_one() {
        fn dispatcher() -> (
            LifecycleCommandDispatcher,
            oneshot::Receiver<()>,
            std_mpsc::SyncSender<()>,
        ) {
            let (normal, _normal_rx) = mpsc::channel(1);
            let (terminal, _terminal_rx) = mpsc::channel(1);
            let (shutdown, shutdown_rx) = oneshot::channel();
            let (drain_done, drain_done_rx) = std_mpsc::sync_channel(1);
            (
                LifecycleCommandDispatcher {
                    name: "test".into(),
                    events: HashSet::new(),
                    normal,
                    terminal,
                    closed: AtomicBool::new(false),
                    shutdown: Mutex::new(Some(shutdown)),
                    drain_done: Mutex::new(Some(drain_done_rx)),
                },
                shutdown_rx,
                drain_done,
            )
        }

        let (first, mut first_shutdown, _first_drain) = dispatcher();
        let (second, mut second_shutdown, _second_drain) = dispatcher();
        let observer = LifecycleCommandObserver {
            mapper: Mutex::new(LifecycleMapper::default()),
            dispatchers: vec![first, second],
        };

        observer.flush_with_timeout(Duration::from_millis(5));

        assert_eq!(first_shutdown.try_recv(), Ok(()));
        assert_eq!(second_shutdown.try_recv(), Ok(()));
    }

    #[test]
    fn spec_rejects_relative_executable_and_zero_bounds() {
        let spec = LifecycleCommandSpec {
            name: "test".into(),
            argv: vec!["relative".into()],
            events: [LifecycleEventKind::TurnStarted].into_iter().collect(),
            queue_capacity: 0,
            max_concurrency: 0,
            timeout: Duration::ZERO,
            max_output_bytes: 0,
        };
        assert!(spec.validate().is_err());
        let invalid_name = LifecycleCommandSpec {
            name: "invalid\0name".into(),
            argv: vec!["/bin/echo".into()],
            events: [LifecycleEventKind::TurnStarted].into_iter().collect(),
            queue_capacity: 1,
            max_concurrency: 1,
            timeout: Duration::from_secs(1),
            max_output_bytes: 0,
        };
        assert!(invalid_name.validate().is_err());

        let valid = LifecycleCommandSpec {
            name: "valid".into(),
            argv: vec!["/bin/echo".into()],
            events: [LifecycleEventKind::TurnStarted].into_iter().collect(),
            queue_capacity: 1,
            max_concurrency: 1,
            timeout: Duration::from_millis(1),
            max_output_bytes: 0,
        };
        for invalid in [
            LifecycleCommandSpec {
                queue_capacity: LIFECYCLE_COMMAND_QUEUE_MAX + 1,
                ..valid.clone()
            },
            LifecycleCommandSpec {
                max_concurrency: LIFECYCLE_COMMAND_CONCURRENCY_MAX + 1,
                ..valid.clone()
            },
            LifecycleCommandSpec {
                timeout: Duration::from_millis(LIFECYCLE_COMMAND_TIMEOUT_MS_MAX + 1),
                ..valid.clone()
            },
            LifecycleCommandSpec {
                max_output_bytes: LIFECYCLE_COMMAND_OUTPUT_BYTES_MAX + 1,
                ..valid.clone()
            },
            LifecycleCommandSpec {
                argv: vec!["/bin/echo".into(); LIFECYCLE_COMMAND_ARGV_MAX + 1],
                ..valid.clone()
            },
            LifecycleCommandSpec {
                argv: vec![
                    "/bin/echo".into(),
                    "x".repeat(LIFECYCLE_COMMAND_ARG_BYTES_MAX + 1),
                ],
                ..valid
            },
        ] {
            assert!(invalid.validate().is_err());
        }
    }

    #[tokio::test]
    async fn bounded_reader_drains_without_retaining_excess() {
        let input = std::io::Cursor::new(vec![b'x'; 16_384]);
        let output = read_bounded(input, 17).await.unwrap();
        assert_eq!(output.bytes.len(), 17);
        assert!(output.truncated);
    }

    #[tokio::test]
    async fn enqueue_is_bounded_nonblocking_and_reserves_terminal_capacity() {
        let (normal_tx, mut normal_rx) = mpsc::channel(1);
        let (terminal_tx, mut terminal_rx) = mpsc::channel(1);

        assert!(
            try_enqueue_event(
                &normal_tx,
                &terminal_tx,
                event(LifecycleEventKind::TurnStarted, 1),
            )
            .is_ok()
        );
        assert!(matches!(
            try_enqueue_event(
                &normal_tx,
                &terminal_tx,
                event(LifecycleEventKind::ToolStarted, 2),
            ),
            Err(EnqueueError::Full)
        ));
        assert!(
            try_enqueue_event(
                &normal_tx,
                &terminal_tx,
                event(LifecycleEventKind::SessionEnded, 3),
            )
            .is_ok()
        );

        assert_eq!(normal_rx.recv().await.unwrap().0.sequence, 1);
        assert_eq!(terminal_rx.recv().await.unwrap().0.sequence, 3);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_receives_exact_json_on_stdin() {
        let spec = LifecycleCommandSpec {
            name: "cat".into(),
            argv: vec!["/bin/cat".into()],
            events: [LifecycleEventKind::TurnStarted].into_iter().collect(),
            queue_capacity: 1,
            max_concurrency: 1,
            timeout: Duration::from_secs(1),
            max_output_bytes: 4096,
        };
        let event = event(LifecycleEventKind::TurnStarted, 1);
        let output = execute_command(&spec, &event).await.unwrap();
        assert!(output.success);
        assert_eq!(output.stdout.bytes, serde_json::to_vec(&event).unwrap());
        assert!(output.stderr.bytes.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn argv_is_literal_and_never_shell_interpolated() {
        let marker = "$(printf should-not-run)";
        let spec = LifecycleCommandSpec {
            name: "echo".into(),
            argv: vec!["/bin/echo".into(), marker.into()],
            events: [LifecycleEventKind::TurnStarted].into_iter().collect(),
            queue_capacity: 1,
            max_concurrency: 1,
            timeout: Duration::from_secs(1),
            max_output_bytes: 4096,
        };
        let output = execute_command(&spec, &event(LifecycleEventKind::TurnStarted, 1))
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(output.stdout.bytes).unwrap().trim(),
            marker
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdout_and_stderr_are_drained_concurrently_and_capped() {
        let script = "i=0; while [ \"$i\" -lt 2048 ]; do printf 0123456789abcdef0123456789abcdef; printf fedcba9876543210fedcba9876543210 >&2; i=$((i + 1)); done";
        let spec = LifecycleCommandSpec {
            name: "dual-output".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            events: [LifecycleEventKind::TurnStarted].into_iter().collect(),
            queue_capacity: 1,
            max_concurrency: 1,
            timeout: Duration::from_secs(2),
            max_output_bytes: 17,
        };

        let output = execute_command(&spec, &event(LifecycleEventKind::TurnStarted, 1))
            .await
            .unwrap();
        assert!(output.success);
        assert_eq!(output.stdout.bytes.len(), 17);
        assert_eq!(output.stderr.bytes.len(), 17);
        assert!(output.stdout.truncated);
        assert!(output.stderr.truncated);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_failure_nonzero_exit_and_timeout_are_isolated() {
        let base = LifecycleCommandSpec {
            name: "failure".into(),
            argv: vec!["/path/that/does/not/exist".into()],
            events: [LifecycleEventKind::TurnStarted].into_iter().collect(),
            queue_capacity: 1,
            max_concurrency: 1,
            timeout: Duration::from_millis(100),
            max_output_bytes: 8,
        };
        let projected = event(LifecycleEventKind::TurnStarted, 1);
        assert!(execute_command(&base, &projected).await.is_err());

        let mut nonzero = base.clone();
        nonzero.argv = vec!["/usr/bin/false".into()];
        assert!(!execute_command(&nonzero, &projected).await.unwrap().success);

        let mut stalled = base;
        let temporary = tempfile::tempdir().unwrap();
        let pid_file = temporary.path().join("child.pid");
        stalled.argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf '%s' \"$$\" > \"$1\"; while :; do :; done".into(),
            "lifecycle-command-test".into(),
            pid_file.to_string_lossy().into_owned(),
        ];
        assert!(execute_command(&stalled, &projected).await.is_err());

        let pid = std::fs::read_to_string(&pid_file).unwrap();
        let mut child_is_gone = false;
        for _ in 0..100 {
            let alive = std::process::Command::new("/bin/kill")
                .args(["-0", pid.as_str()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if !alive {
                child_is_gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(child_is_gone, "timed-out lifecycle child remained alive");

        let recovered = LifecycleCommandSpec {
            argv: vec!["/bin/cat".into()],
            timeout: Duration::from_secs(1),
            ..stalled
        };
        assert!(
            execute_command(&recovered, &projected)
                .await
                .unwrap()
                .success
        );
    }

    #[test]
    fn observer_coalesces_repeated_working_start_for_the_same_turn() {
        let observer = LifecycleCommandObserver {
            mapper: Mutex::new(LifecycleMapper::default()),
            dispatchers: Vec::new(),
        };
        let start = ObserverEvent::AgentStart {
            model_provider: "provider".into(),
            model: "model".into(),
            channel: Some("channel".into()),
            agent_alias: Some("agent".into()),
            turn_id: Some("turn".into()),
        };

        let first = observer.project(&start);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].event, LifecycleEventKind::SessionStarted);
        assert_eq!(first[1].event, LifecycleEventKind::TurnStarted);
        assert_eq!(
            first[1].state,
            zeroclaw_api::lifecycle::LifecycleState::Working
        );
        assert!(observer.project(&start).is_empty());
    }

    #[tokio::test]
    async fn correlation_is_stable_across_scope_and_exposes_no_source_identifiers() {
        let observer = LifecycleCommandObserver {
            mapper: Mutex::new(LifecycleMapper::default()),
            dispatchers: Vec::new(),
        };
        let raw_turn = "turn-containing-private-source-text";
        let raw_session = "channel_sender_private_session_key";
        let start = ObserverEvent::AgentStart {
            model_provider: "provider".into(),
            model: "model".into(),
            channel: Some("channel".into()),
            agent_alias: Some("agent".into()),
            turn_id: Some(raw_turn.into()),
        };
        let started = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some(raw_session.into()), async { observer.project(&start) })
            .await;
        let tool = ObserverEvent::ToolCallStart {
            tool: "shell".into(),
            tool_call_id: None,
            arguments: Some("arguments-must-not-cross".into()),
            channel: Some("channel".into()),
            agent_alias: Some("agent".into()),
            parent_agent_alias: None,
            turn_id: Some(raw_turn.into()),
        };
        let projected_tool = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some(raw_session.into()), async { observer.project(&tool) })
            .await;

        assert_eq!(started.len(), 2);
        assert_eq!(projected_tool.len(), 1);
        assert_eq!(
            started[0].correlation.session_id,
            projected_tool[0].correlation.session_id
        );
        let serialized = serde_json::to_string(&(started, projected_tool)).unwrap();
        for private in [raw_session, raw_turn, "arguments-must-not-cross"] {
            assert!(!serialized.contains(private));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatcher_bounds_concurrency_and_prioritizes_terminal_event() {
        let (normal_tx, normal_rx) = mpsc::channel(8);
        let (terminal_tx, terminal_rx) = mpsc::channel(2);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let spec = Arc::new(LifecycleCommandSpec {
            name: "scripted".into(),
            argv: vec!["/bin/echo".into()],
            events: LifecycleEventKind::ALL.into_iter().collect(),
            queue_capacity: 8,
            max_concurrency: 2,
            timeout: Duration::from_secs(1),
            max_output_bytes: 0,
        });
        let worker = {
            let active = Arc::clone(&active);
            let max_seen = Arc::clone(&max_seen);
            let release = Arc::clone(&release);
            let delivered = Arc::clone(&delivered);
            zeroclaw_spawn::spawn!(run_dispatcher(
                normal_rx,
                terminal_rx,
                shutdown_rx,
                Arc::clone(&spec),
                move |event| {
                    let active = Arc::clone(&active);
                    let max_seen = Arc::clone(&max_seen);
                    let release = Arc::clone(&release);
                    let delivered = Arc::clone(&delivered);
                    async move {
                        let now = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                        let _active_delivery = ActiveDelivery(Arc::clone(&active));
                        max_seen.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                        if event.event != LifecycleEventKind::SessionEnded {
                            release.notified().await;
                        }
                        delivered.lock().push(event.event);
                    }
                }
            ))
        };

        normal_tx
            .send(QueuedEvent(event(LifecycleEventKind::TurnStarted, 1)))
            .await
            .unwrap();
        normal_tx
            .send(QueuedEvent(event(LifecycleEventKind::ToolStarted, 2)))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while active.load(std::sync::atomic::Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        terminal_tx
            .send(QueuedEvent(event(LifecycleEventKind::SessionEnded, 3)))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if delivered.lock().contains(&LifecycleEventKind::SessionEnded) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(max_seen.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(
            delivered.lock().as_slice(),
            [LifecycleEventKind::SessionEnded]
        );
        release.notify_waiters();
        let _ = shutdown_tx.send(());
        worker.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn terminal_event_cancels_only_its_session_within_global_concurrency() {
        let (normal_tx, normal_rx) = mpsc::channel(8);
        let (terminal_tx, terminal_rx) = mpsc::channel(2);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let spec = Arc::new(LifecycleCommandSpec {
            name: "scripted".into(),
            argv: vec!["/bin/echo".into()],
            events: LifecycleEventKind::ALL.into_iter().collect(),
            queue_capacity: 8,
            max_concurrency: 2,
            timeout: Duration::from_secs(1),
            max_output_bytes: 0,
        });
        let worker = {
            let active = Arc::clone(&active);
            let max_seen = Arc::clone(&max_seen);
            let release = Arc::clone(&release);
            let delivered = Arc::clone(&delivered);
            zeroclaw_spawn::spawn!(run_dispatcher(
                normal_rx,
                terminal_rx,
                shutdown_rx,
                Arc::clone(&spec),
                move |event| {
                    let active = Arc::clone(&active);
                    let max_seen = Arc::clone(&max_seen);
                    let release = Arc::clone(&release);
                    let delivered = Arc::clone(&delivered);
                    async move {
                        let now = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                        let _active_delivery = ActiveDelivery(Arc::clone(&active));
                        max_seen.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                        if event.event != LifecycleEventKind::SessionEnded {
                            release.notified().await;
                        }
                        delivered
                            .lock()
                            .push((event.correlation.session_id, event.event));
                    }
                }
            ))
        };

        normal_tx
            .send(QueuedEvent(event_for_session(
                LifecycleEventKind::TurnStarted,
                1,
                "session-a",
            )))
            .await
            .unwrap();
        normal_tx
            .send(QueuedEvent(event_for_session(
                LifecycleEventKind::TurnStarted,
                2,
                "session-b",
            )))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while active.load(std::sync::atomic::Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        terminal_tx
            .send(QueuedEvent(event_for_session(
                LifecycleEventKind::SessionEnded,
                3,
                "session-a",
            )))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !delivered.lock().iter().any(|(session, event)| {
                session == "session-a" && *event == LifecycleEventKind::SessionEnded
            }) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(max_seen.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(
            !delivered
                .lock()
                .iter()
                .any(|(session, _)| session == "session-b")
        );

        release.notify_waiters();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !delivered.lock().iter().any(|(session, event)| {
                session == "session-b" && *event == LifecycleEventKind::TurnStarted
            }) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let _ = shutdown_tx.send(());
        worker.await.unwrap();
    }
}
