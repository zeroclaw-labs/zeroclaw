use super::{
    GoalReattachFixture, GoalReattachPeer, NativeBurstMode, QUESTION, REATTACH_OWNER, REATTACH_SID,
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

// The frames still come from the production RPC paths. Owning a bounded
// receiver here lets us stop draining it at an exact boundary.
struct Peer {
    dispatcher: RpcDispatcher,
    writer: mpsc::Sender<String>,
    frames: mpsc::Receiver<String>,
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
        serde_json::from_str(&self.frames.recv().await.expect("peer writer open")).unwrap()
    }
}

fn request(method: Method, params: &Value, id: u64) -> String {
    json!({"jsonrpc": "2.0", "method": method.wire_name(), "params": params, "id": id}).to_string()
}

impl Peer {
    async fn connect(ctx: &Arc<RpcContext>, tui_id: &str) -> Self {
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
        };
        peer.initialize(ctx, tui_id, None).await;
        peer
    }

    fn start_history(&self) -> JoinHandle<()> {
        let mut connection = self.dispatcher.spawn_handle();
        zeroclaw_spawn::spawn!(async move {
            connection
                .process_line(&request(
                    Method::SessionMessages,
                    &json!({"session_id":REATTACH_SID}),
                    HISTORY_ID,
                ))
                .await;
        })
    }

    async fn history(&mut self, saved: Option<&str>) {
        let response = self.next_frame().await;
        assert_eq!(
            response["id"], HISTORY_ID,
            "notification overtook history: {response}"
        );
        let snapshot: SessionMessagesResult =
            serde_json::from_value(response["result"].clone()).unwrap();
        assert_eq!(snapshot.session_id, REATTACH_SID);
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
        SessionGoalUpdate::VerifiedCandidate { session_id, candidate } if session_id == REATTACH_SID && candidate == expected),
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
    _directory: tempfile::TempDir,
}

impl HistoryCase {
    async fn new(mode: NativeBurstMode) -> Self {
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
        let goal = GoalReattachFixture::new(&mut ctx, directory.path(), mode).await;
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
        let mut a = Peer::connect(&ctx, REATTACH_OWNER).await;
        goal.start(&mut a).await;
        let b = Peer::connect(&ctx, "goal-history-reader").await;
        Self {
            ctx,
            goal,
            a,
            b,
            _directory: directory,
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
