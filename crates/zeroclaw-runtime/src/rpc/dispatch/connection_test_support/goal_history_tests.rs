use super::{
    GoalReattachFixture, GoalReattachPeer, NativeBurstMode, NativeBurstRoute, QUESTION,
    REATTACH_OWNER, REATTACH_SID,
};
use crate::rpc::context::RpcContext;
use crate::rpc::dispatch::{Method, RpcDispatcher};
use crate::rpc::session::SessionStore;
use crate::rpc::transport::TransportKind;
use crate::rpc::types::{SessionGoalUpdate, SessionMessagesResult};
use crate::security::auth_provider::{Credential, PeercredAuthProvider};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use zeroclaw_infra::session_queue::SessionActorQueue;

const HISTORY_ID: u64 = 4;
const BEFORE_SAVE: &str = "goal-before-candidate-save";
const WAIT_SAVE: &str = "goal-wait-candidate-save";
const WAIT_GATE: &str = "goal-wait-attach-gate";
const BEFORE_RESERVE: &str = "goal-before-candidate-reserve";
const HISTORY_FRAME: &str = "messages-frame";
const FILLER: &str = "occupied";

// Issue through native command admission and finish on its actual dispatcher.
// Holding the bounded writer lets policy and connection changes occur before
// the first acknowledgement or a deferred notification is committed.
mod goal_issuer_tests {
    use super::super::{GoalParentAdmissionCase, HeldGoalParent, prepare_held_goal_parent_fixture};
    use super::*;
    use std::sync::atomic::Ordering;
    use zeroclaw_goal::control::{GoalQueuedControl, GoalResponse, GoalSessionKey};

    const ID: u64 = 152;
    type Command = (Value, crate::rpc::goal::RpcGoalCommandSubmission);

    struct Case {
        peer: Peer,
        ctx: Arc<RpcContext>,
        parent: HeldGoalParent,
        registry: Arc<dyn zeroclaw_goal::GoalTaskRegistry>,
        dir: tempfile::TempDir,
    }

    impl Case {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let mut ctx = RpcContext::minimal(
                zeroclaw_config::schema::Config {
                    config_path: dir.path().join("config.toml"),
                    data_dir: dir.path().to_owned(),
                    ..Default::default()
                },
                Arc::new(SessionStore::new(
                    64,
                    Arc::new(SessionActorQueue::new(4, 30, 60)),
                )),
            );
            let (registry, parent) = prepare_held_goal_parent_fixture(
                &mut ctx,
                dir.path(),
                REATTACH_SID,
                REATTACH_OWNER,
                GoalParentAdmissionCase::DirectResume,
            )
            .await;
            let peer = Peer::connect(&ctx, REATTACH_OWNER).await;
            Self {
                peer,
                ctx,
                parent,
                registry,
                dir,
            }
        }

        async fn admit(&self, command: &str) -> Command {
            self.peer
                .dispatcher
                .handle_session_goal(&json!({
                    "session_id": REATTACH_SID, "command": command,
                }))
                .await
                .unwrap()
        }

        fn finish(&self, command: Command) -> JoinHandle<()> {
            let dispatcher = self.peer.dispatcher.spawn_handle();
            zeroclaw_spawn::spawn!(async move {
                dispatcher
                    .finish_session_goal_command(json!(ID), false, Ok(command))
                    .await;
            })
        }

        async fn park(&self, command: Command) -> JoinHandle<()> {
            self.peer.occupy_writer();
            let writers = self.peer.writer.strong_count();
            let finish = self.finish(command);
            self.peer.wait_for_reservation(writers).await;
            finish
        }

        fn drain_fillers(&mut self) {
            for _ in 0..self.peer.writer.max_capacity() {
                assert_eq!(self.peer.frames.try_recv().unwrap(), FILLER);
            }
        }

        fn change_durable_owner(&self) {
            self.ctx
                .session_backend
                .as_ref()
                .unwrap()
                .set_session_principal(&format!("rpc_{REATTACH_SID}"), "fixture-second-owner")
                .unwrap();
        }

        fn assert_no_parent(&self) {
            assert_eq!(self.parent.turn.calls.load(Ordering::SeqCst), 0);
        }

        fn key(&self) -> String {
            GoalSessionKey::zero_code(REATTACH_SID)
                .unwrap()
                .durable_id()
        }

        async fn assert_paused(&self, reason: zeroclaw_goal::GoalPauseReason) {
            let task = self
                .registry
                .current_goal_for_session(&self.key())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(task.status, zeroclaw_api::task::TaskStatus::Paused);
            assert_eq!(
                self.registry
                    .get_goal_task(&task.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .pause_reason,
                Some(reason)
            );
        }
    }

    #[tokio::test]
    async fn pending_reader_receives_command_notices_without_moving_the_active_stream() {
        for still_owned in [true, false] {
            let mut case = Case::new().await;
            let resumed = case
                .peer
                .result(
                    Method::SessionGoal,
                    json!({
                        "session_id":REATTACH_SID, "command":"/goal resume",
                    }),
                    ID,
                )
                .await;
            assert!(matches!(
                serde_json::from_value::<crate::rpc::types::SessionGoalResult>(resumed)
                    .unwrap()
                    .response,
                crate::rpc::types::GoalResponseWire::Resumed(_)
            ));
            arrived(&case.parent.turn.started, "held original parent").await;
            let mut issuer = Peer::connect(&case.ctx, REATTACH_OWNER).await;
            issuer
                .result(
                    Method::SessionNew,
                    json!({
                        "session_id":REATTACH_SID,"agent_alias":"test-agent","chat_mode":"chat",
                    }),
                    20,
                )
                .await;
            let recipient = case
                .ctx
                .sessions
                .goal_session_snapshot(REATTACH_SID)
                .await
                .unwrap()
                .goal_recipient;
            let (active, pending) = recipient.destinations_for_test();
            assert!(
                active
                    .and_then(|d| d.upgrade())
                    .is_some_and(|d| d.matches(&case.peer.dispatcher.rpc))
            );
            assert!(
                pending
                    .and_then(|d| d.upgrade())
                    .is_some_and(|d| d.matches(&issuer.dispatcher.rpc))
            );
            let status = issuer
                .result(
                    Method::SessionGoal,
                    json!({"session_id":REATTACH_SID,"command":"/goal status"}),
                    21,
                )
                .await;
            assert!(matches!(
                serde_json::from_value::<crate::rpc::types::SessionGoalResult>(status)
                    .unwrap()
                    .response,
                crate::rpc::types::GoalResponseWire::Status(_)
            ));
            let (expected, control) = issuer
                .dispatcher
                .handle_session_goal(&json!({
                    "session_id":REATTACH_SID, "command":"/goal pause now",
                }))
                .await
                .unwrap();
            assert!(matches!(
                control.response(),
                GoalResponse::TransitionPending(_)
            ));
            let dispatcher = issuer.dispatcher.spawn_handle();
            let initial = expected.clone();
            let delivery = zeroclaw_spawn::spawn!(async move {
                dispatcher
                    .finish_session_goal_command(json!(22), false, Ok((initial, control)))
                    .await;
            });
            assert_eq!(
                issuer.next_frame().await,
                json!({"jsonrpc":"2.0","id":22,"result":expected})
            );
            issuer.occupy_writer();
            let writers = issuer.writer.strong_count();
            case.parent.turn.release.notify_one();
            issuer.wait_for_reservation(writers).await;
            if !still_owned {
                case.change_durable_owner();
            }
            for _ in 0..issuer.writer.max_capacity() {
                assert_eq!(issuer.frames.try_recv().unwrap(), FILLER);
            }
            join_history(delivery).await;
            if still_owned {
                let frame = issuer.next_frame().await;
                assert!(frame.get("id").is_none());
                assert_eq!(frame["method"], "session/goal_update");
                assert!(
                    matches!(serde_json::from_value::<SessionGoalUpdate>(frame["params"].clone()).unwrap(),
                    SessionGoalUpdate::Lifecycle { response: crate::rpc::types::GoalResponseWire::Paused(_), session_id } if session_id == REATTACH_SID)
                );
            }
            assert!(issuer.frames.try_recv().is_err());
            while let Ok(raw) = case.peer.frames.try_recv() {
                let frame: Value = serde_json::from_str(&raw).unwrap();
                assert!(frame.get("id").is_none());
                assert!(
                    frame["params"].get("lifecycle").is_none(),
                    "command notice moved to the stream"
                );
            }
            case.assert_paused(zeroclaw_goal::GoalPauseReason::OperatorPaused)
                .await;
            assert_eq!(case.parent.turn.calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn authorized_session_without_goal_receives_no_current_goal() {
        let mut case = Case::new().await;
        case.ctx
            .goal_runtime
            .dispose_session(REATTACH_SID)
            .await
            .unwrap();
        let result = case
            .peer
            .result(
                Method::SessionGoal,
                json!({"session_id":REATTACH_SID, "command":"/goal status"}),
                ID,
            )
            .await;
        assert_eq!(
            serde_json::from_value::<crate::rpc::types::SessionGoalResult>(result)
                .unwrap()
                .response,
            crate::rpc::types::GoalResponseWire::NoCurrentGoal
        );
        case.assert_no_parent();
    }

    #[tokio::test]
    async fn only_current_incarnation_can_commit_prepared_projection() {
        let mut case = Case::new().await;
        let command = case.admit("/goal budget").await;
        assert!(matches!(command.1.response(), GoalResponse::Budget(_)));
        let delivery = case.park(command).await;
        assert!(case.ctx.sessions.remove(REATTACH_SID).await);
        super::super::insert_session_with_owner(
            &case.ctx,
            case.dir.path(),
            REATTACH_SID,
            Box::new(super::super::HeldOrdinaryProvider(Default::default())),
            Some(REATTACH_OWNER),
        )
        .await;
        case.drain_fillers();
        join_history(delivery).await;
        let frame = case.peer.next_frame().await;
        assert_eq!(frame["id"], ID);
        assert_eq!(frame["error"]["code"], super::super::super::FORBIDDEN);
        assert!(frame.get("result").is_none());
        case.assert_no_parent();
    }

    #[tokio::test]
    async fn fenced_closure_sends_its_projection_only_while_original_owner_is_current() {
        for still_owned in [true, false] {
            let mut case = Case::new().await;
            let ordinary = case
                .ctx
                .sessions
                .session_queue
                .acquire(REATTACH_SID)
                .await
                .unwrap();
            let command = case.admit("/goal resume").await;
            assert_eq!(
                command.1.response(),
                &GoalResponse::Queued(GoalQueuedControl::Resume)
            );
            let expected = command.0.clone();
            let delivery = case.finish(command);
            assert_eq!(
                case.peer.next_frame().await,
                json!({"jsonrpc":"2.0","id":ID,"result":expected})
            );
            let generation = case
                .ctx
                .sessions
                .get_generation(REATTACH_SID)
                .await
                .unwrap();
            let fence = case
                .ctx
                .sessions
                .lock_goal_lifecycle(REATTACH_SID, generation)
                .await
                .unwrap()
                .fence_for_binding(true);
            let resident = case
                .ctx
                .goal_runtime
                .supervisor(REATTACH_SID)
                .await
                .unwrap();
            case.peer.occupy_writer();
            let writers = case.peer.writer.strong_count();
            assert!(
                resident
                    .quiesce_session_worker(&case.key())
                    .await
                    .unwrap()
                    .is_none()
            );
            case.peer.wait_for_reservation(writers).await;
            if !still_owned {
                case.change_durable_owner();
            }
            case.drain_fillers();
            join_history(delivery).await;
            if still_owned {
                let frame = case.peer.next_frame().await;
                assert!(frame.get("id").is_none());
                assert_eq!(frame["method"], "session/goal_update");
                let update: SessionGoalUpdate =
                    serde_json::from_value(frame["params"].clone()).unwrap();
                assert!(
                    matches!(update, SessionGoalUpdate::Lifecycle {session_id, response:crate::rpc::types::GoalResponseWire::AlreadyPaused(status)}
                    if session_id == REATTACH_SID && status.task_id == case.registry.current_goal_for_session(&case.key()).await.unwrap().unwrap().id)
                );
            }
            assert!(case.peer.frames.try_recv().is_err());
            drop(fence);
            drop(ordinary);
            case.assert_paused(zeroclaw_goal::GoalPauseReason::OperatorPaused)
                .await;
            case.assert_no_parent();
        }
    }

    #[tokio::test]
    async fn command_failure_is_not_published_after_original_owner_changes() {
        let mut case = Case::new().await;
        let ordinary = case
            .ctx
            .sessions
            .session_queue
            .acquire(REATTACH_SID)
            .await
            .unwrap();
        let command = case.admit("/goal resume").await;
        let expected = command.0.clone();
        let delivery = case.finish(command);
        assert_eq!(
            case.peer.next_frame().await,
            json!({"jsonrpc":"2.0","id":ID,"result":expected})
        );
        let entered = Arc::new(Notify::new());
        let resume = Arc::new(Notify::new());
        *case
            .ctx
            .goal_runtime
            .test_deferred_commit_pause
            .lock()
            .unwrap() = Some(crate::rpc::goal::GoalTestDeferredCommitPause {
            session_id: REATTACH_SID.into(),
            entered: Arc::clone(&entered),
            release: Arc::clone(&resume),
        });
        drop(ordinary);
        arrived(&entered, "deferred command admission").await;
        case.parent.restart.quiesce_for_restart().await.unwrap();
        case.peer.occupy_writer();
        let writers = case.peer.writer.strong_count();
        resume.notify_one();
        case.peer.wait_for_reservation(writers).await;
        case.change_durable_owner();
        case.drain_fillers();
        join_history(delivery).await;
        assert!(case.peer.frames.try_recv().is_err());
        case.assert_paused(zeroclaw_goal::GoalPauseReason::OperatorPaused)
            .await;
        case.assert_no_parent();
    }

    #[tokio::test]
    async fn original_registration_receives_exact_status_despite_later_same_tui() {
        let mut case = Case::new().await;
        let command = case.admit("/goal status").await;
        assert!(matches!(command.1.response(), GoalResponse::Status(_)));
        let expected = command.0.clone();
        let delivery = case.park(command).await;
        let mut other = Peer::connect(&case.ctx, REATTACH_OWNER).await;
        case.drain_fillers();
        join_history(delivery).await;
        let frame = case.peer.next_frame().await;
        assert_eq!(frame, json!({"jsonrpc":"2.0", "id":ID, "result":expected}));
        assert!(other.frames.try_recv().is_err());
        case.assert_no_parent();
    }

    #[tokio::test]
    async fn replaced_registration_drops_pending_result_without_waiting_for_writer_capacity() {
        let mut case = Case::new().await;
        let command = case.admit("/goal status").await;
        let delivery = case.park(command).await;
        let replacement = "fixture-new-binding";
        let initialized = case.peer.dispatcher.handle_initialize(&json!({
            "protocol_version":1, "tui_id":replacement, "tui_sig":case.ctx.tui_registry.sign(replacement)
        })).await.unwrap();
        assert_eq!(initialized["tui_id"], replacement);
        join_history(delivery).await;
        case.drain_fillers();
        assert!(case.peer.frames.try_recv().is_err());
        case.assert_no_parent();
    }

    #[tokio::test]
    async fn changed_selected_owner_refuses_queued_ack_and_withdraws_work() {
        let mut case = Case::new().await;
        let ordinary = case
            .ctx
            .sessions
            .session_queue
            .acquire(REATTACH_SID)
            .await
            .unwrap();
        let command = case.admit("/goal resume").await;
        assert_eq!(
            command.1.response(),
            &GoalResponse::Queued(GoalQueuedControl::Resume)
        );
        let delivery = case.park(command).await;
        case.change_durable_owner();
        case.drain_fillers();
        join_history(delivery).await;
        drop(ordinary);
        let frame = case.peer.next_frame().await;
        assert_eq!(frame["id"], ID);
        assert_eq!(frame["error"]["code"], super::super::super::FORBIDDEN);
        assert!(frame.get("result").is_none());
        assert!(case.peer.frames.try_recv().is_err());
        case.assert_paused(zeroclaw_goal::GoalPauseReason::OperatorPaused)
            .await;
        let conn = rusqlite::Connection::open(case.dir.path().join("control_plane.db")).unwrap();
        let disposition: (String, String) = conn
            .query_row(
                "SELECT state, outcome_reason FROM goal_control_intents WHERE session_id = ?1",
                [case.key()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(disposition, ("abandoned".into(), "delivery_failed".into()));
        case.assert_no_parent();
    }

    #[tokio::test]
    async fn refused_deferred_prelaunch_notice_pauses_before_first_parent() {
        let mut case = Case::new().await;
        let ordinary = case
            .ctx
            .sessions
            .session_queue
            .acquire(REATTACH_SID)
            .await
            .unwrap();
        let command = case.admit("/goal resume").await;
        assert_eq!(
            command.1.response(),
            &GoalResponse::Queued(GoalQueuedControl::Resume)
        );
        let expected = command.0.clone();
        let delivery = case.finish(command);
        assert_eq!(
            case.peer.next_frame().await,
            json!({"jsonrpc":"2.0","id":ID,"result":expected})
        );
        case.peer.occupy_writer();
        let writers = case.peer.writer.strong_count();
        drop(ordinary);
        case.peer.wait_for_reservation(writers).await;
        assert_eq!(
            case.registry
                .current_goal_for_session(&case.key())
                .await
                .unwrap()
                .unwrap()
                .status,
            zeroclaw_api::task::TaskStatus::Running
        );
        case.change_durable_owner();
        case.drain_fillers();
        join_history(delivery).await;
        assert!(case.peer.frames.try_recv().is_err());
        case.assert_paused(zeroclaw_goal::GoalPauseReason::ExternalDependency)
            .await;
        case.assert_no_parent();
    }
}

// The frames still come from the production RPC paths. Owning a bounded
// receiver here lets us stop draining it at an exact boundary.
struct Peer {
    dispatcher: RpcDispatcher,
    writer: mpsc::Sender<String>,
    frames: mpsc::Receiver<String>,
    live_reply_chunks: Vec<(String, Option<String>)>,
}

impl GoalReattachPeer for Peer {
    async fn write_request(&mut self, method: Method, params: &Value, id: u64) {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.dispatcher.process_line(&request(method, params, id)),
        )
        .await
        .expect("ordinary RPC request must complete while Goal worker is held");
    }

    async fn read_frame(&mut self) -> Value {
        let frame: Value =
            serde_json::from_str(&self.frames.recv().await.expect("peer writer open")).unwrap();
        let params = &frame["params"];
        if frame["method"] == "session/update"
            && params["type"] == "agent_message_chunk"
            && params["presentation_only"] != true
        {
            self.live_reply_chunks.push((
                params["text"].as_str().unwrap().to_owned(),
                params["reply_occurrence"].as_str().map(str::to_owned),
            ));
        }
        frame
    }
}

fn request(method: Method, params: &Value, id: u64) -> String {
    json!({"jsonrpc": "2.0", "method": method.wire_name(), "params": params, "id": id}).to_string()
}

impl Peer {
    async fn connect(ctx: &Arc<RpcContext>, tui_id: &str) -> Self {
        Self::connect_with_occurrence(ctx, tui_id, false).await
    }

    async fn connect_with_occurrence(
        ctx: &Arc<RpcContext>,
        tui_id: &str,
        reply_occurrence: bool,
    ) -> Self {
        let uid = PeercredAuthProvider::current_process_uid();
        // The native burst emits 72 thoughts even before the final round starts.
        let (writer, frames) = mpsc::channel(128);
        let dispatcher =
            RpcDispatcher::new(Arc::clone(ctx), writer.clone(), format!("unix:uid={uid}"))
                .with_transport(TransportKind::Local, Credential::Peercred { uid });
        let mut peer = Self {
            dispatcher,
            writer,
            frames,
            live_reply_chunks: Vec::new(),
        };
        if reply_occurrence {
            let initialized = peer
                .result(
                    Method::Initialize,
                    json!({
                        "protocol_version": 1,
                        "tui_id": tui_id,
                        "tui_sig": ctx.tui_registry.sign(tui_id),
                        "clientCapabilities": { "replyOccurrence": {} },
                    }),
                    1,
                )
                .await;
            assert_eq!(initialized["tui_id"], tui_id);
        } else {
            peer.initialize(ctx, tui_id, None).await;
        }
        peer
    }

    fn start_history(&self) -> JoinHandle<()> {
        self.start_history_with(json!({"session_id": REATTACH_SID}))
    }

    fn start_history_with(&self, params: Value) -> JoinHandle<()> {
        let mut connection = self.dispatcher.spawn_handle();
        zeroclaw_spawn::spawn!(async move {
            connection
                .process_line(&request(Method::SessionMessages, &params, HISTORY_ID))
                .await;
        })
    }

    async fn history_snapshot(&mut self) -> SessionMessagesResult {
        let response = self.next_frame().await;
        assert_eq!(
            response["id"], HISTORY_ID,
            "notification overtook history: {response}"
        );
        let snapshot: SessionMessagesResult =
            serde_json::from_value(response["result"].clone()).unwrap();
        assert_eq!(snapshot.session_id, REATTACH_SID);
        snapshot
    }

    async fn history(&mut self, saved: Option<&str>) {
        let snapshot = self.history_snapshot().await;
        let mut expected = vec![("user", "Choose a bounded repair.")];
        if let Some(candidate) = saved {
            expected.push(("assistant", candidate));
        }
        assert_eq!(
            snapshot
                .messages
                .iter()
                .map(|m| (m.role.as_str(), m.content.as_str()))
                .collect::<Vec<_>>(),
            expected
        );
    }

    async fn candidate(&mut self, answer: &str) {
        let frame = self.next_frame().await;
        assert_candidate(&frame, answer);
    }

    // A, unlike B, is the starting recipient for ordinary stream frames.
    async fn original_candidate(&mut self, answer: &str) {
        loop {
            let frame = self.next_frame().await;
            if frame["method"] == "session/goal_update" {
                assert_candidate(&frame, answer);
                return;
            }
            assert_parent_stream(&frame);
        }
    }

    fn drain_pre_cutover(&mut self) {
        while let Ok(raw) = self.frames.try_recv() {
            if raw != FILLER {
                assert_parent_stream(&serde_json::from_str(&raw).unwrap());
            }
        }
    }

    async fn notice(&mut self, blocked: bool) {
        let frame = self.next_frame().await;
        assert_eq!(frame["method"], "session/goal_update", "{frame}");
        let update = serde_json::from_value::<SessionGoalUpdate>(frame["params"].clone()).unwrap();
        match update {
            SessionGoalUpdate::PausedForBlocker { session_id } if blocked => {
                assert_eq!(session_id, REATTACH_SID)
            }
            SessionGoalUpdate::Completed { session_id } if !blocked => {
                assert_eq!(session_id, REATTACH_SID)
            }
            other => panic!("unexpected Goal notice: {other:?}"),
        }
        self.result(Method::Status, json!({}), 7).await;
    }

    fn occupy_writer(&self) {
        while self.writer.capacity() > 0 {
            self.writer.try_send(FILLER.to_owned()).unwrap();
        }
    }

    async fn wait_for_reservation(&self, before: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while self.writer.strong_count() <= before {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("reserve_owned must be parked on the full writer");
        assert_eq!(self.writer.capacity(), 0);
    }
}

fn assert_candidate(frame: &Value, expected: &str) {
    assert_eq!(frame["method"], "session/goal_update", "{frame}");
    assert!(
        matches!(serde_json::from_value::<SessionGoalUpdate>(frame["params"].clone()).unwrap(),
        SessionGoalUpdate::VerifiedCandidate { session_id, candidate, .. } if session_id == REATTACH_SID && candidate == expected),
        "{frame}"
    );
}

fn assert_parent_stream(frame: &Value) {
    assert_eq!(frame["method"], "session/update", "{frame}");
    assert_eq!(frame["params"]["session_id"], REATTACH_SID);
    assert!(
        matches!(
            frame["params"]["type"].as_str(),
            Some("agent_message_chunk" | "agent_thought_chunk" | "context_usage")
        ),
        "{frame}"
    );
}

async fn arrived(signal: &Notify, name: &str) {
    tokio::time::timeout(Duration::from_secs(2), signal.notified())
        .await
        .unwrap_or_else(|_| panic!("did not reach {name}"));
}

async fn join_history(read: JoinHandle<()>) {
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("history request must finish")
        .unwrap();
}

struct HistoryCase {
    ctx: Arc<RpcContext>,
    goal: GoalReattachFixture,
    a: Peer,
    b: Peer,
    directory: tempfile::TempDir,
}

impl HistoryCase {
    async fn new(mode: NativeBurstMode) -> Self {
        Self::with_occurrences(mode, NativeBurstRoute::Primary, false, false).await
    }

    async fn with_occurrences(
        mode: NativeBurstMode,
        route: NativeBurstRoute,
        first: bool,
        second: bool,
    ) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: directory.path().to_owned(),
            config_path: directory.path().join("config.toml"),
            ..Default::default()
        };
        let mut ctx = RpcContext::minimal(
            config,
            Arc::new(SessionStore::new(
                64,
                Arc::new(SessionActorQueue::new(4, 30, 60)),
            )),
        );
        let goal = GoalReattachFixture::with_route(&mut ctx, directory.path(), mode, route).await;
        // Here the held parent must be the *only* source of its question: the
        // stream fixture normally seeds the same text from an older turn.
        let key = format!("rpc_{REATTACH_SID}");
        let backend = ctx.session_backend.as_ref().unwrap();
        assert!(backend.remove_last(&key).unwrap());
        let seed = backend.try_load(&key).unwrap();
        assert_eq!(seed.len(), 1);
        assert_eq!(seed[0].content, "Choose a bounded repair.");
        let agent = ctx.sessions.get_agent(REATTACH_SID).await.unwrap();
        {
            let mut agent = agent.lock().await;
            agent.clear_history();
            agent.seed_history(&seed);
        }
        let mut a = Peer::connect_with_occurrence(&ctx, REATTACH_OWNER, first).await;
        goal.start(&mut a).await;
        let b = Peer::connect_with_occurrence(&ctx, "goal-history-reader", second).await;
        Self {
            ctx,
            goal,
            a,
            b,
            directory,
        }
    }

    async fn offer(&mut self) {
        let response = self
            .b
            .result(
                Method::SessionNew,
                json!({"session_id":REATTACH_SID,"agent_alias":"test-agent","chat_mode":"chat"}),
                3,
            )
            .await;
        assert_eq!(response["session_id"], REATTACH_SID);
    }

    async fn unsaved(&self, expected: bool) {
        assert_eq!(
            self.ctx
                .sessions
                .goal_candidate_unsaved_for_test(REATTACH_SID)
                .await,
            Some(expected)
        );
    }

    async fn verifier_started(&self) {
        arrived(&self.goal.verifier.turn.started, "held verifier").await;
    }

    async fn assert_original_recipient(&self) {
        let recipient = self
            .ctx
            .sessions
            .goal_session_snapshot(REATTACH_SID)
            .await
            .unwrap()
            .goal_recipient;
        let (active, pending) = recipient.destinations_for_test();
        assert!(
            active
                .and_then(|connection| connection.upgrade())
                .is_some_and(|connection| connection.matches(&self.a.dispatcher.rpc)),
            "a non-promoting request must keep A active"
        );
        assert!(
            pending
                .and_then(|connection| connection.upgrade())
                .is_some_and(|connection| connection.matches(&self.b.dispatcher.rpc)),
            "a non-promoting request must keep B pending"
        );
    }

    async fn finish(self, expected: &str) {
        let history = self
            .ctx
            .session_backend
            .as_ref()
            .unwrap()
            .try_load(&format!("rpc_{REATTACH_SID}"))
            .unwrap();
        assert_eq!(
            history
                .iter()
                .map(|m| (m.role.as_str(), m.content.as_str()))
                .collect::<Vec<_>>(),
            [
                ("user", "Choose a bounded repair."),
                ("assistant", expected)
            ]
        );
        self.goal.verifier.finish().await;
        self.ctx
            .goal_runtime
            .dispose_session(REATTACH_SID)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn goal_candidate_during_history_commit_follows_response() {
    let mut case = HistoryCase::new(NativeBurstMode::SplitFinal).await;
    case.offer().await;
    let (snapshot, send) = case.ctx.sessions.set_test_effect_pause(HISTORY_FRAME);
    let (candidate_wait, retry) = case.ctx.sessions.set_test_effect_pause(WAIT_GATE);
    let read = case.b.start_history();
    arrived(&snapshot, HISTORY_FRAME).await;
    case.goal.parent.turn.release.notify_one();
    arrived(&candidate_wait, WAIT_GATE).await;
    case.unsaved(false).await;
    case.a.drain_pre_cutover();
    assert!(case.b.frames.is_empty());
    retry.notify_one();
    send.notify_one();
    case.b.history(None).await;
    case.b.candidate(QUESTION).await;
    case.verifier_started().await;
    case.goal.verifier.turn.release.notify_one();
    case.b.notice(true).await;
    case.a.result(Method::Status, json!({}), 7).await;
    join_history(read).await;
    case.finish(QUESTION).await;
}

#[tokio::test]
async fn goal_history_waits_for_published_candidate_to_be_saved() {
    let mut case = HistoryCase::new(NativeBurstMode::SplitFinal).await;
    let (saving, save) = case.ctx.sessions.set_test_effect_pause(BEFORE_SAVE);
    case.goal.parent.turn.release.notify_one();
    case.a.original_candidate(QUESTION).await;
    arrived(&saving, BEFORE_SAVE).await;
    case.offer().await;
    let (waiting, retry) = case.ctx.sessions.set_test_effect_pause(WAIT_SAVE);
    let read = case.b.start_history();
    arrived(&waiting, WAIT_SAVE).await;
    case.unsaved(true).await;
    assert!(!read.is_finished() && case.b.frames.is_empty());
    retry.notify_one();
    save.notify_one();
    case.b.history(Some(QUESTION)).await;
    join_history(read).await;
    case.unsaved(false).await;
    case.verifier_started().await;
    case.goal.verifier.turn.release.notify_one();
    case.b.notice(true).await;
    case.a.result(Method::Status, json!({}), 7).await;
    case.finish(QUESTION).await;
}

#[tokio::test]
async fn goal_parent_occurrence_follows_negotiated_delivery_and_saved_row() {
    for first_opted_in in [true, false] {
        let mut case = HistoryCase::with_occurrences(
            NativeBurstMode::SplitFinal,
            NativeBurstRoute::StreamingFallback,
            first_opted_in,
            !first_opted_in,
        )
        .await;
        let (saving, save) = case.ctx.sessions.set_test_effect_pause(BEFORE_SAVE);
        case.goal.parent.turn.release.notify_one();
        let mut tails = Vec::new();
        let occurrence = loop {
            let frame = case.a.next_frame().await;
            if frame["method"] == "session/goal_update" {
                assert_candidate(&frame, QUESTION);
                let payload = &frame["params"]["verified_candidate"];
                if !first_opted_in {
                    assert!(payload.get("reply_occurrence").is_none(), "{frame}");
                }
                break payload["reply_occurrence"].as_str().map(str::to_owned);
            }
            assert_parent_stream(&frame);
            let params = &frame["params"];
            if params["presentation_only"] == true {
                tails.push(params.get("reply_occurrence").cloned());
            }
        };
        assert_eq!(occurrence.is_some(), first_opted_in);
        assert!(!tails.is_empty(), "fixture must emit a marked display tail");
        let expected_tail = occurrence.as_ref().map(|id| json!(id));
        assert!(tails.iter().all(|tail| tail == &expected_tail));
        let chunks = &case.a.live_reply_chunks;
        let before_tool = chunks
            .iter()
            .find(|(s, _)| s == "draft before child\n")
            .expect("pre-tool narration was delivered");
        if first_opted_in {
            assert!(before_tool.1.is_some());
            assert_ne!(before_tool.1, occurrence);
        } else {
            assert!(before_tool.1.is_none());
        }
        for fragment in ["Which option ", "should I use?"] {
            let (_, id) = chunks
                .iter()
                .find(|(s, _)| s == fragment)
                .expect("both real streamed question fragments were delivered");
            assert_eq!(id.as_deref(), occurrence.as_deref());
        }

        arrived(&saving, BEFORE_SAVE).await;
        let backend = case.ctx.session_backend.as_ref().unwrap();
        let key = format!("rpc_{REATTACH_SID}");
        let before = backend.try_load_transcript_rows(&key).unwrap();
        assert_eq!(before.len(), 1);
        assert!(before[0].reply_occurrence.is_none());
        save.notify_one();
        case.verifier_started().await;
        let after = backend.try_load_transcript_rows(&key).unwrap();
        assert_eq!(after.len(), 2);
        let row = &after[1];
        assert_eq!(row.message.content, QUESTION);
        let saved = row
            .reply_occurrence
            .as_deref()
            .expect("durable candidate identity");
        if first_opted_in {
            assert_eq!(occurrence.as_deref(), Some(saved));
        }

        for (peer, opted_in) in [
            (&mut case.a, first_opted_in),
            (&mut case.b, !first_opted_in),
        ] {
            let read = peer.start_history_with(json!({"session_id": REATTACH_SID, "limit": 1}));
            let response = peer.next_frame().await;
            assert_eq!(response["id"], HISTORY_ID, "{response}");
            let page = response["result"]["messages"].as_array().unwrap();
            assert_eq!(page.len(), 1);
            assert_eq!(page[0]["content"], QUESTION);
            if opted_in {
                assert_eq!(page[0]["reply_occurrence"], saved);
            } else {
                assert!(page[0].get("reply_occurrence").is_none(), "{response}");
            }
            join_history(read).await;
        }

        case.goal.verifier.turn.release.notify_one();
        case.a.notice(true).await;
        case.finish(QUESTION).await;
    }
}

#[tokio::test]
async fn goal_saved_candidate_is_history_only_and_blocker_is_live() {
    let mut case = HistoryCase::new(NativeBurstMode::SplitFinal).await;
    case.goal.parent.turn.release.notify_one();
    case.a.original_candidate(QUESTION).await;
    case.verifier_started().await;
    case.offer().await;
    let read = case.b.start_history();
    case.b.history(Some(QUESTION)).await;
    join_history(read).await;
    case.goal.verifier.turn.release.notify_one();
    case.b.notice(true).await;
    case.a.result(Method::Status, json!({}), 7).await;
    case.finish(QUESTION).await;
}

#[tokio::test]
async fn goal_empty_page_does_not_promote_a_pending_destination() {
    assert_limited_history_keeps_recipient(0).await;
}

#[tokio::test]
async fn goal_single_message_page_does_not_promote_a_pending_destination() {
    assert_limited_history_keeps_recipient(1).await;
}

async fn assert_limited_history_keeps_recipient(limit: usize) {
    let mut case = HistoryCase::new(NativeBurstMode::SplitFinal).await;
    case.goal.parent.turn.release.notify_one();
    case.a.original_candidate(QUESTION).await;
    case.verifier_started().await;
    case.offer().await;
    let read = case
        .b
        .start_history_with(json!({"session_id": REATTACH_SID, "limit": limit}));
    let snapshot = case.b.history_snapshot().await;
    assert_eq!(snapshot.total, 2);
    assert_eq!(snapshot.start, 2 - limit);
    assert_eq!(snapshot.messages.len(), limit);
    if limit == 1 {
        assert_eq!(snapshot.messages[0].content, QUESTION);
    }
    join_history(read).await;
    case.assert_original_recipient().await;
    case.goal.verifier.turn.release.notify_one();
    case.a.notice(true).await;
    case.b.result(Method::Status, json!({}), 7).await;
    case.finish(QUESTION).await;
}

#[tokio::test]
async fn goal_limited_snapshot_does_not_wait_for_the_candidate_save() {
    let mut case = HistoryCase::new(NativeBurstMode::SplitFinal).await;
    let (saving, save) = case.ctx.sessions.set_test_effect_pause(BEFORE_SAVE);
    let (waiting, _retry) = case.ctx.sessions.set_test_effect_pause(WAIT_SAVE);
    case.goal.parent.turn.release.notify_one();
    case.a.original_candidate(QUESTION).await;
    arrived(&saving, BEFORE_SAVE).await;
    case.offer().await;
    let read = case
        .b
        .start_history_with(json!({"session_id": REATTACH_SID, "limit": 100}));
    tokio::select! {
        biased;
        () = waiting.notified() => panic!("a limited request entered the Goal save gate"),
        () = case.b.history(None) => {}
    }
    join_history(read).await;
    case.unsaved(true).await;
    case.assert_original_recipient().await;
    save.notify_one();
    case.verifier_started().await;
    case.goal.verifier.turn.release.notify_one();
    case.a.notice(true).await;
    case.b.result(Method::Status, json!({}), 7).await;
    case.finish(QUESTION).await;
}

#[tokio::test]
async fn goal_null_limit_still_promotes_the_complete_snapshot() {
    let mut case = HistoryCase::new(NativeBurstMode::SplitFinal).await;
    case.goal.parent.turn.release.notify_one();
    case.a.original_candidate(QUESTION).await;
    case.verifier_started().await;
    case.offer().await;
    let read = case
        .b
        .start_history_with(json!({"session_id": REATTACH_SID, "limit": null}));
    case.b.history(Some(QUESTION)).await;
    join_history(read).await;
    case.goal.verifier.turn.release.notify_one();
    case.b.notice(true).await;
    case.a.result(Method::Status, json!({}), 7).await;
    case.finish(QUESTION).await;
}

#[tokio::test]
async fn goal_damaged_snapshot_is_best_effort_without_promoting() {
    use zeroclaw_infra::session_sqlite::SqliteSessionBackend;

    let mut case = HistoryCase::new(NativeBurstMode::SplitFinal).await;
    case.goal.parent.turn.release.notify_one();
    case.a.original_candidate(QUESTION).await;
    case.verifier_started().await;
    case.offer().await;
    let connection =
        rusqlite::Connection::open(SqliteSessionBackend::db_path(case.directory.path())).unwrap();
    let key = format!("rpc_{REATTACH_SID}");
    let row: i64 = connection
        .query_row(
            "SELECT MAX(id) FROM sessions WHERE session_key = ?1",
            [&key],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE sessions SET content = CAST(content AS BLOB) WHERE id = ?1",
                [row],
            )
            .unwrap(),
        1
    );
    let backend = case.ctx.session_backend.as_ref().unwrap();
    assert!(backend.try_load(&key).is_err());
    assert_eq!(backend.load(&key).len(), 1);
    let read = case.b.start_history();
    case.b.history(None).await;
    join_history(read).await;
    case.assert_original_recipient().await;
    case.goal.verifier.turn.release.notify_one();
    case.a.notice(true).await;
    case.b.result(Method::Status, json!({}), 7).await;
    connection
        .execute(
            "UPDATE sessions SET content = CAST(content AS TEXT) WHERE id = ?1",
            [row],
        )
        .unwrap();
    case.finish(QUESTION).await;
}

#[tokio::test]
async fn goal_unreadable_metadata_does_not_promote_an_empty_response() {
    assert_unlocated_goal_history_keeps_recipient(None, false).await;
}

#[tokio::test]
async fn goal_unreadable_metadata_does_not_promote_an_alternate_transcript() {
    assert_unlocated_goal_history_keeps_recipient(Some(&format!("gw_{REATTACH_SID}")), false).await;
    assert_unlocated_goal_history_keeps_recipient(Some(REATTACH_SID), false).await;
}

#[tokio::test]
async fn goal_unlocated_and_unreadable_native_transcript_does_not_promote() {
    assert_unlocated_goal_history_keeps_recipient(None, true).await;
}

async fn assert_unlocated_goal_history_keeps_recipient(
    alternate_key: Option<&str>,
    damage_history: bool,
) {
    use zeroclaw_api::model_provider::ChatMessage;
    use zeroclaw_infra::session_sqlite::SqliteSessionBackend;

    const ALTERNATE: &str = "Ordinary history under the alternate key.";
    let mut case = HistoryCase::new(NativeBurstMode::SplitFinal).await;
    case.goal.parent.turn.release.notify_one();
    case.a.original_candidate(QUESTION).await;
    case.verifier_started().await;
    case.offer().await;

    let backend = case.ctx.session_backend.as_ref().unwrap();
    let key = format!("rpc_{REATTACH_SID}");
    let metadata = backend.get_session_metadata(&key).unwrap();
    if let Some(alternate_key) = alternate_key {
        backend
            .append(alternate_key, &ChatMessage::user(ALTERNATE))
            .unwrap();
        if let Some(owner) = metadata.principal_id.as_deref() {
            backend.set_session_principal(alternate_key, owner).unwrap();
        }
    }
    let connection =
        rusqlite::Connection::open(SqliteSessionBackend::db_path(case.directory.path())).unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE session_metadata SET name = X'00' WHERE session_key = ?1",
                [&key]
            )
            .unwrap(),
        1
    );
    assert!(backend.get_session_metadata(&key).is_none());
    assert_eq!(
        backend
            .try_load(&key)
            .unwrap()
            .iter()
            .map(|message| (message.role.as_str(), message.content.as_str()))
            .collect::<Vec<_>>(),
        [
            ("user", "Choose a bounded repair."),
            ("assistant", QUESTION)
        ]
    );

    if damage_history {
        assert_eq!(connection.execute(
            "UPDATE sessions SET content = CAST(content AS BLOB) WHERE session_key = ?1 AND role = 'assistant'",
            [&key],
        ).unwrap(), 1);
        assert!(backend.try_load(&key).is_err());
    }

    let read = case.b.start_history();
    let snapshot = case.b.history_snapshot().await;
    assert_eq!(snapshot.start, 0);
    let expected = alternate_key
        .map(|_| ("user", ALTERNATE))
        .into_iter()
        .collect::<Vec<_>>();
    assert_eq!(snapshot.total, expected.len());
    assert_eq!(
        snapshot
            .messages
            .iter()
            .map(|message| (message.role.as_str(), message.content.as_str()))
            .collect::<Vec<_>>(),
        expected
    );
    join_history(read).await;
    case.assert_original_recipient().await;
    case.goal.verifier.turn.release.notify_one();
    case.a.notice(true).await;
    case.b.result(Method::Status, json!({}), 7).await;
    assert_eq!(
        connection
            .execute(
                "UPDATE session_metadata SET name = ?1 WHERE session_key = ?2",
                rusqlite::params![metadata.name, key],
            )
            .unwrap(),
        1
    );
    if damage_history {
        assert_eq!(connection.execute(
            "UPDATE sessions SET content = CAST(content AS TEXT) WHERE session_key = ?1 AND role = 'assistant'",
            [&key],
        ).unwrap(), 1);
    }
    case.finish(QUESTION).await;
}

#[tokio::test]
async fn goal_verified_empty_native_history_allows_promotion() {
    use super::{HeldOrdinaryProvider, insert_session_with_owner};
    use zeroclaw_api::model_provider::ChatMessage;
    use zeroclaw_infra::session_sqlite::SqliteSessionBackend;

    for alternate in [None, Some("A legacy history message.")] {
        let directory = tempfile::tempdir().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: directory.path().to_owned(),
            config_path: directory.path().join("config.toml"),
            ..Default::default()
        };
        let mut ctx = RpcContext::minimal(
            config,
            Arc::new(SessionStore::new(
                64,
                Arc::new(SessionActorQueue::new(4, 30, 60)),
            )),
        );
        Arc::get_mut(&mut ctx).unwrap().session_backend = Some(Arc::new(
            SqliteSessionBackend::new(directory.path()).unwrap(),
        ));
        insert_session_with_owner(
            &ctx,
            directory.path(),
            REATTACH_SID,
            Box::new(HeldOrdinaryProvider(Default::default())),
            Some(REATTACH_OWNER),
        )
        .await;
        let backend = ctx.session_backend.as_ref().unwrap();
        if let Some(text) = alternate {
            backend
                .append(&format!("gw_{REATTACH_SID}"), &ChatMessage::user(text))
                .unwrap();
        }
        let mut peer = Peer::connect(&ctx, "goal-history-reader").await;
        peer.result(
            Method::SessionNew,
            json!({"session_id": REATTACH_SID, "agent_alias": "test-agent", "chat_mode": "chat"}),
            3,
        )
        .await;
        let key = format!("rpc_{REATTACH_SID}");
        assert!(backend.get_session_metadata(&key).is_none());
        assert!(backend.try_load(&key).unwrap().is_empty());
        let read = peer.start_history();
        let snapshot = peer.history_snapshot().await;
        assert_eq!(snapshot.start, 0);
        assert_eq!(snapshot.total, usize::from(alternate.is_some()));
        assert_eq!(
            snapshot
                .messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            alternate.into_iter().collect::<Vec<_>>()
        );
        join_history(read).await;
        let recipient = ctx
            .sessions
            .goal_session_snapshot(REATTACH_SID)
            .await
            .unwrap()
            .goal_recipient;
        let (active, pending) = recipient.destinations_for_test();
        assert!(
            active
                .and_then(|connection| connection.upgrade())
                .is_some_and(|connection| connection.matches(&peer.dispatcher.rpc))
        );
        assert!(pending.is_none());
    }
}

#[tokio::test]
async fn goal_checked_history_does_not_override_durable_owner_at_send() {
    use zeroclaw_infra::session_sqlite::SqliteSessionBackend;

    let mut case = HistoryCase::new(NativeBurstMode::SplitFinal).await;
    case.goal.parent.turn.release.notify_one();
    case.a.original_candidate(QUESTION).await;
    case.verifier_started().await;
    case.offer().await;

    let (arrived_at_frame, send) = case.ctx.sessions.set_test_effect_pause(HISTORY_FRAME);
    let read = case.b.start_history();
    arrived(&arrived_at_frame, HISTORY_FRAME).await;
    let backend = case.ctx.session_backend.as_ref().unwrap();
    let key = format!("rpc_{REATTACH_SID}");
    let owner = backend.get_session_metadata(&key).unwrap().principal_id;
    backend
        .set_session_principal(&key, "fixture-other-principal")
        .unwrap();
    send.notify_one();
    let frame = case.b.next_frame().await;
    assert_eq!(frame["id"], HISTORY_ID);
    assert_eq!(frame["error"]["code"], super::super::FORBIDDEN);
    assert!(frame.get("result").is_none());
    join_history(read).await;
    case.assert_original_recipient().await;

    let connection =
        rusqlite::Connection::open(SqliteSessionBackend::db_path(case.directory.path())).unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE session_metadata SET principal_id = ?1 WHERE session_key = ?2",
                rusqlite::params![owner, key],
            )
            .unwrap(),
        1
    );
    case.goal.verifier.turn.release.notify_one();
    case.a.notice(true).await;
    case.b.result(Method::Status, json!({}), 7).await;
    case.finish(QUESTION).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn goal_sqlite_append_keeps_the_only_runtime_worker_responsive() {
    use zeroclaw_infra::session_sqlite::SqliteSessionBackend;

    let mut case = HistoryCase::new(NativeBurstMode::SplitFinal).await;
    let (saving, save) = case.ctx.sessions.set_test_effect_pause(BEFORE_SAVE);
    case.goal.parent.turn.release.notify_one();
    case.a.original_candidate(QUESTION).await;
    arrived(&saving, BEFORE_SAVE).await;
    case.unsaved(true).await;

    let path = SqliteSessionBackend::db_path(case.directory.path());
    let writer = rusqlite::Connection::open(path).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    // Wake the held append from the only worker before enqueuing its probe.
    // Use a wall-clock receive on the test thread: the blocked worker might
    // not be able to drive a Tokio timer.
    zeroclaw_spawn::spawn!(async move { save.notify_one() })
        .await
        .unwrap();
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let probe = zeroclaw_spawn::spawn!(async move { send.send(()).unwrap() });
    let responsive = receive.recv_timeout(Duration::from_secs(2));
    writer.execute_batch("ROLLBACK").unwrap();
    probe.await.unwrap();
    responsive.expect("SQLite writer contention blocked the sole runtime worker");

    case.verifier_started().await;
    let reopened = SqliteSessionBackend::new(case.directory.path()).unwrap();
    let key = format!("rpc_{REATTACH_SID}");
    use zeroclaw_infra::session_backend::SessionBackend;
    let durable = reopened.try_load(&key).unwrap();
    assert_eq!(durable.len(), 2);
    assert_eq!(durable[0].content, "Choose a bounded repair.");
    assert_eq!(durable[1].content, QUESTION);
    let agent = case.ctx.sessions.get_agent(REATTACH_SID).await.unwrap();
    let visible = zeroclaw_api::model_provider::durable_chat_messages(agent.lock().await.history());
    assert_eq!(
        visible
            .iter()
            .filter(|message| message.content == QUESTION)
            .count(),
        1
    );
    case.goal.verifier.turn.release.notify_one();
    case.a.notice(true).await;
    case.finish(QUESTION).await;
}

#[tokio::test]
async fn goal_sqlite_metadata_failure_rolls_back_candidate_and_reports_the_original_error() {
    use crate::rpc::types::{GoalFailureReasonWire, SessionUpdateEvent, TurnCompletionOutcome};
    use std::sync::atomic::Ordering;
    use zeroclaw_infra::session_backend::SessionBackend;
    use zeroclaw_infra::session_sqlite::SqliteSessionBackend;

    let mut case = HistoryCase::new(NativeBurstMode::SplitFinal).await;
    let (saving, save) = case.ctx.sessions.set_test_effect_pause(BEFORE_SAVE);
    case.goal.parent.turn.release.notify_one();
    case.a.original_candidate(QUESTION).await;
    arrived(&saving, BEFORE_SAVE).await;
    case.unsaved(true).await;

    let path = SqliteSessionBackend::db_path(case.directory.path());
    let writer = rusqlite::Connection::open(path).unwrap();
    writer
        .execute_batch(
            "CREATE TRIGGER fail_goal_metadata_update BEFORE UPDATE ON session_metadata
             BEGIN SELECT RAISE(ABORT, 'injected Goal history metadata failure'); END;",
        )
        .unwrap();
    drop(writer);
    save.notify_one();

    let error = case.a.next_frame().await;
    assert_eq!(error["method"], "session/update", "{error}");
    let event: SessionUpdateEvent = serde_json::from_value(error["params"].clone()).unwrap();
    assert!(
        matches!(event, SessionUpdateEvent::TurnComplete {
            session_id,
            outcome: TurnCompletionOutcome::Failed,
            content,
            goal_owned: true,
            ..
        } if session_id == REATTACH_SID && content.contains("injected Goal history metadata failure")),
        "{error}"
    );

    let failure = case.a.next_frame().await;
    assert_eq!(failure["method"], "session/goal_update", "{failure}");
    let update: SessionGoalUpdate = serde_json::from_value(failure["params"].clone()).unwrap();
    assert!(
        matches!(update, SessionGoalUpdate::Failed {
            session_id,
            reason: GoalFailureReasonWire::CandidatePersistenceFailed
        } if session_id == REATTACH_SID),
        "{failure}"
    );
    case.unsaved(false).await;
    assert_eq!(case.goal.verifier.turn.calls.load(Ordering::Acquire), 0);

    let key = format!("rpc_{REATTACH_SID}");
    let reopened = SqliteSessionBackend::new(case.directory.path()).unwrap();
    for history in [
        case.ctx
            .session_backend
            .as_ref()
            .unwrap()
            .try_load(&key)
            .unwrap(),
        reopened.try_load(&key).unwrap(),
    ] {
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[0].content, "Choose a bounded repair.");
    }
    let agent = case.ctx.sessions.get_agent(REATTACH_SID).await.unwrap();
    let visible = zeroclaw_api::model_provider::durable_chat_messages(agent.lock().await.history());
    assert!(!visible.iter().any(|message| message.content == QUESTION));

    case.ctx
        .goal_runtime
        .dispose_session(REATTACH_SID)
        .await
        .unwrap();
    case.a.result(Method::Status, json!({}), 7).await;
    case.goal.verifier.finish().await;
}

#[tokio::test]
async fn goal_completed_during_history_commit_follows_response_once() {
    let mut case = HistoryCase::new(NativeBurstMode::Reattach).await;
    case.goal.parent.turn.release.notify_one();
    case.a.original_candidate("ordinary answer").await;
    case.verifier_started().await;
    case.offer().await;
    let (snapshot, send) = case.ctx.sessions.set_test_effect_pause(HISTORY_FRAME);
    let (notice_wait, retry) = case.ctx.sessions.set_test_effect_pause(WAIT_GATE);
    let read = case.b.start_history();
    arrived(&snapshot, HISTORY_FRAME).await;
    case.goal.verifier.turn.release.notify_one();
    arrived(&notice_wait, WAIT_GATE).await;
    assert!(case.a.frames.is_empty() && case.b.frames.is_empty());
    retry.notify_one();
    send.notify_one();
    case.b.history(Some("ordinary answer")).await;
    case.b.notice(false).await;
    join_history(read).await;
    case.a.result(Method::Status, json!({}), 7).await;
    case.finish("ordinary answer").await;
}

#[tokio::test]
async fn goal_disconnected_history_writer_releases_waiting_candidate() {
    let mut case = HistoryCase::new(NativeBurstMode::SplitFinal).await;
    case.offer().await;
    let (snapshot, send) = case.ctx.sessions.set_test_effect_pause(HISTORY_FRAME);
    let (candidate_wait, retry) = case.ctx.sessions.set_test_effect_pause(WAIT_GATE);
    let read = case.b.start_history();
    arrived(&snapshot, HISTORY_FRAME).await;
    case.b.occupy_writer();
    let before = case.b.writer.strong_count();
    send.notify_one();
    case.b.wait_for_reservation(before).await;
    case.goal.parent.turn.release.notify_one();
    arrived(&candidate_wait, WAIT_GATE).await;
    retry.notify_one();
    case.b.frames.close();
    join_history(read).await;
    case.a.original_candidate(QUESTION).await;
    case.verifier_started().await;
    case.goal.verifier.turn.release.notify_one();
    case.a.notice(true).await;
    while let Ok(frame) = case.b.frames.try_recv() {
        assert_eq!(frame, FILLER);
    }
    case.finish(QUESTION).await;
}

#[tokio::test]
async fn goal_candidate_parked_on_old_writer_follows_promoted_history() {
    let mut case = HistoryCase::new(NativeBurstMode::SplitFinal).await;
    let (candidate, reserve) = case.ctx.sessions.set_test_effect_pause(BEFORE_RESERVE);
    case.goal.parent.turn.release.notify_one();
    arrived(&candidate, BEFORE_RESERVE).await;
    case.a.occupy_writer();
    let before = case.a.writer.strong_count();
    reserve.notify_one();
    case.a.wait_for_reservation(before).await;
    case.unsaved(false).await;
    case.offer().await;
    let read = case.b.start_history();
    case.b.history(None).await;
    join_history(read).await;
    case.b.candidate(QUESTION).await;
    case.verifier_started().await;
    case.goal.verifier.turn.release.notify_one();
    case.b.notice(true).await;
    case.a.drain_pre_cutover();
    case.a.result(Method::Status, json!({}), 7).await;
    case.finish(QUESTION).await;
}
