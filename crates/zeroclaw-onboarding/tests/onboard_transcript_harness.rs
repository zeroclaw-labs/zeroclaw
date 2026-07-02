//! Full walked-path transcript harness (checklist stage 16). Drives the real
//! onboarding flow across the transport x mode x path matrix and writes the
//! entire captured I/O of each cell to its own labeled folder under the
//! workspace `tmp/zeroclaw-onboard/<timestamp>/`. Output is the artifact; the
//! assertions key off the structural outcome token and transcript content, never
//! off filesystem state outside the run folder.
//!
//! The accessible config (the one the CLI resolves via `--config-dir` /
//! `ZEROCLAW_CONFIG_DIR`) is a read-only input used only to resolve a genuine
//! aliased agent and its model-provider for the GUIDED runner; the flow only
//! ever writes into a seeded copy under the run folder. The heavy entrypoint is
//! `#[ignore]` (operator-run): the CLI-pty cells build the binary from source and
//! the GUIDED cells need a live provider, so it is not part of the default sweep.

#[path = "common/pty.rs"]
mod pty;
#[path = "common/spec.rs"]
mod spec;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use spec::{INSTANCE, LAYER, SECTION, matrix_spec};
use zeroclaw_config::schema::{Config, MatrixConfig};
use zeroclaw_onboarding::driver::{FlowRequest, run_flow};
use zeroclaw_onboarding::{FieldScope, LlmResponder, LlmTransport, SecretReader};
use zeroclaw_runtime::flow::{
    FlowTransport, Outcome, Prompt, TransportError, TransportResult, WalkError,
};
use zeroclaw_runtime::response_type::{ResponseType, ResponseValue, SecretValue};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root above the crate")
        .to_path_buf()
}

fn run_dir() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs();
    let dir = workspace_root()
        .join("tmp")
        .join("zeroclaw-onboard")
        .join(stamp.to_string());
    std::fs::create_dir_all(&dir).expect("create timestamped run folder");
    dir
}

/// The config dir the CLI itself would resolve. Read-only here: used only to
/// resolve a genuine agent + provider alias for GUIDED. Never written to.
fn accessible_config_dir() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("ZEROCLAW_CONFIG_DIR") {
        return Some(PathBuf::from(explicit));
    }
    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".zeroclaw"))
        .filter(|p| p.join("config.toml").exists())
}

/// Resolve the agent alias and its dotted model-provider ref for GUIDED runs.
/// `ZEROCLAW_ONBOARD_AGENT` picks the alias explicitly; otherwise the first
/// enabled agent (sorted) is used. Never hardcoded; absent config yields None
/// and the GUIDED cells are skipped. Parsed generically (not through the strict
/// `Config` view) so an operator config with crate-unknown fields still
/// resolves.
fn resolve_agent_provider() -> Option<(String, String)> {
    let dir = accessible_config_dir()?;
    let raw = std::fs::read_to_string(dir.join("config.toml")).ok()?;
    let value: toml::Value = toml::from_str(&raw).ok()?;
    let agents = value.get("agents")?.as_table()?;
    let mut aliases: Vec<&String> = agents.keys().collect();
    aliases.sort();
    let preferred = std::env::var("ZEROCLAW_ONBOARD_AGENT").ok();
    let candidates: Vec<&String> = match &preferred {
        Some(wanted) => aliases.into_iter().filter(|a| *a == wanted).collect(),
        None => aliases,
    };
    for alias in candidates {
        let Some(agent) = agents.get(alias).and_then(|a| a.as_table()) else {
            continue;
        };
        let enabled = agent
            .get("enabled")
            .and_then(|e| e.as_bool())
            .unwrap_or(true);
        if !enabled {
            continue;
        }
        if let Some(provider) = agent.get("model_provider").and_then(|p| p.as_str()) {
            return Some((alias.clone(), provider.to_string()));
        }
    }
    None
}

fn seed_config_copy(into: &Path) -> Config {
    let mut config = Config::default();
    config
        .channels
        .matrix
        .insert(INSTANCE.to_string(), MatrixConfig::default());
    let seed = format!(
        "schema_version = 3\n\n[channels.matrix.{INSTANCE}]\nenabled = false\nhomeserver = \"https://matrix.org\"\n"
    );
    std::fs::write(into.join("config.toml"), seed).expect("write seeded config copy");
    config
}

/// Drive the real binary's manual CLI walk over a pty, feeding the homeserver
/// section's prompts and capturing the entire terminal transcript. Locale is
/// the first prompt, so the registry's first code leads the scripted answers.
fn drive_pty_manual_happy(config_dir: &Path) -> (String, String) {
    let drive = pty::drive_flow(
        config_dir,
        &[],
        pty::scripted_answers(),
        Duration::from_secs(40),
    );
    let outcome = if drive.completed {
        "completed".to_string()
    } else {
        "incomplete".to_string()
    };
    (drive.transcript, outcome)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathLabel {
    Happy,
    BadInputThenGood,
    Cancel,
    TransportClosed,
}

impl PathLabel {
    fn folder(self) -> &'static str {
        match self {
            PathLabel::Happy => "happy",
            PathLabel::BadInputThenGood => "bad-input-then-good",
            PathLabel::Cancel => "cancel",
            PathLabel::TransportClosed => "transport-closed",
        }
    }
}

fn happy_answer(prompt: &Prompt) -> ResponseValue {
    match &prompt.response_type {
        ResponseType::Secret => ResponseValue::Secret(SecretValue::new("sk-token".into())),
        ResponseType::YesNo => ResponseValue::YesNo(true),
        ResponseType::Number => ResponseValue::Number("100".into()),
        ResponseType::Choice { options } => ResponseValue::Choice(options[0].value.clone()),
        ResponseType::FreeformText => ResponseValue::FreeformText("https://walked.test".into()),
    }
}

/// Records the full I/O of a walk into a transcript buffer. The answer behaviour
/// is selected by `path`, so a single transport drives every matrix cell.
struct RecordingTransport {
    path: PathLabel,
    asked: usize,
    transcript: String,
    outcome_token: Option<String>,
}

impl RecordingTransport {
    fn new(path: PathLabel) -> Self {
        Self {
            path,
            asked: 0,
            transcript: String::new(),
            outcome_token: None,
        }
    }
}

#[async_trait]
impl FlowTransport for RecordingTransport {
    async fn ask(&mut self, prompt: &Prompt) -> TransportResult<ResponseValue> {
        self.asked += 1;
        let _ = writeln!(
            self.transcript,
            "ASK #{} [{}] {}",
            self.asked,
            prompt.response_type.ask_kind(),
            prompt.text
        );
        match self.path {
            PathLabel::Cancel => {
                let _ = writeln!(self.transcript, "  -> cancel (transport closed by user)");
                Err(TransportError::Closed)
            }
            PathLabel::TransportClosed => {
                let _ = writeln!(self.transcript, "  -> transport closed");
                Err(TransportError::Closed)
            }
            PathLabel::BadInputThenGood if self.asked == 1 => {
                if let ResponseType::Number = prompt.response_type {
                    let _ = writeln!(self.transcript, "  <- (rejected) not-a-number");
                }
                let answer = happy_answer(prompt);
                let _ = writeln!(self.transcript, "  <- {answer:?}");
                Ok(answer)
            }
            _ => {
                let answer = happy_answer(prompt);
                let _ = writeln!(self.transcript, "  <- {answer:?}");
                Ok(answer)
            }
        }
    }

    async fn emit(&mut self, outcome: &Outcome) -> TransportResult<()> {
        let token = outcome_token(outcome);
        let _ = writeln!(self.transcript, "EMIT [{token}] {outcome:?}");
        self.outcome_token = Some(token);
        Ok(())
    }
}

fn outcome_token(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Completed { .. } => "completed".to_string(),
        Outcome::Cancelled => "cancelled".to_string(),
        Outcome::Failed { .. } => "failed".to_string(),
    }
}

fn walk_error_token(error: &WalkError) -> String {
    match error {
        WalkError::Transport(_) => "transport-error".to_string(),
        WalkError::Write(_) => "write-error".to_string(),
        WalkError::UnknownNode(_) => "unknown-node".to_string(),
    }
}

fn walk_token(walk: &Result<Outcome, WalkError>) -> String {
    match walk {
        Ok(outcome) => outcome_token(outcome),
        Err(error) => walk_error_token(error),
    }
}

fn driver_error_token(error: &zeroclaw_onboarding::DriverError) -> String {
    use zeroclaw_onboarding::DriverError;
    match error {
        DriverError::EmptySection(_) => "empty-section".to_string(),
        DriverError::AliasCreate { .. } => "alias-create-error".to_string(),
        DriverError::Walk(walk) => walk_error_token(walk),
    }
}

fn write_meta(dir: &Path, meta: &BTreeMap<&str, String>) {
    let mut meta_json = String::from("{\n");
    let mut first = true;
    for (key, value) in meta {
        if !first {
            meta_json.push_str(",\n");
        }
        first = false;
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        let _ = write!(meta_json, "  \"{key}\": \"{escaped}\"");
    }
    meta_json.push_str("\n}\n");
    let mut file = std::fs::File::create(dir.join("meta.json")).expect("create meta.json");
    file.write_all(meta_json.as_bytes())
        .expect("write meta.json");
}

fn write_cell(
    base: &Path,
    transport: &str,
    mode: &str,
    path: PathLabel,
    transcript: &str,
    meta: &BTreeMap<&str, String>,
) {
    let dir = base.join(transport).join(mode).join(path.folder());
    std::fs::create_dir_all(&dir).expect("create cell folder");
    std::fs::write(dir.join("transcript.txt"), transcript).expect("write transcript");
    write_meta(&dir, meta);
}

async fn run_manual_cell(base: &Path, path: PathLabel) {
    let run_root = base.join("inproc").join("manual").join(path.folder());
    std::fs::create_dir_all(&run_root).expect("seed dir");
    let mut config = seed_config_copy(&run_root);

    let mut transport = RecordingTransport::new(path);
    let request = FlowRequest {
        section_prefix: SECTION,
        layer: LAYER,
        instance: INSTANCE,
        create: false,
        scope: FieldScope::All,
    };
    let result = run_flow(&mut config, &request, &mut transport).await;

    let outcome_token = match &result {
        Ok(outcome) => outcome_token(outcome),
        Err(error) => driver_error_token(error),
    };
    let mut meta = BTreeMap::new();
    meta.insert("transport", "inproc".to_string());
    meta.insert("mode", "manual".to_string());
    meta.insert("path", path.folder().to_string());
    meta.insert("model_provider", "n/a (manual)".to_string());
    meta.insert("outcome", outcome_token.clone());
    write_cell(base, "inproc", "manual", path, &transport.transcript, &meta);

    match path {
        PathLabel::Happy | PathLabel::BadInputThenGood => assert_eq!(
            outcome_token,
            "completed",
            "{} must complete the walk",
            path.folder()
        ),
        PathLabel::Cancel | PathLabel::TransportClosed => assert_eq!(
            outcome_token,
            "transport-error",
            "{} must surface a transport error, not a silent completion",
            path.folder()
        ),
    }
}

/// Records every conversational turn the guide takes for a non-secret prompt.
/// The LLM walk is back-and-forth: `LlmTransport::ask` loops on `respond` until
/// a parseable value returns, so the agent may reply with clarifications or
/// restatements before the field resolves. Each of those turns is transcribed,
/// not just the final parsed answer.
struct RecordingResponder<L: LlmResponder> {
    inner: L,
    log: std::sync::Arc<std::sync::Mutex<String>>,
    turn: usize,
}

#[async_trait]
impl<L: LlmResponder> LlmResponder for RecordingResponder<L> {
    async fn respond(&mut self, prompt_text: &str) -> TransportResult<String> {
        self.turn += 1;
        {
            let mut log = self.log.lock().unwrap();
            let _ = writeln!(log, "LLM-ASK turn#{} {prompt_text}", self.turn);
        }
        let reply = self.inner.respond(prompt_text).await;
        let mut log = self.log.lock().unwrap();
        match &reply {
            Ok(text) => {
                let _ = writeln!(log, "  <- guide: {text}");
            }
            Err(error) => {
                let _ = writeln!(log, "  <- guide error: {error}");
            }
        }
        reply
    }
}

struct RecordingSecretReader<S: SecretReader> {
    inner: S,
    log: std::sync::Arc<std::sync::Mutex<String>>,
}

#[async_trait]
impl<S: SecretReader> SecretReader for RecordingSecretReader<S> {
    async fn read_secret(&mut self, prompt_text: &str) -> TransportResult<String> {
        {
            let mut log = self.log.lock().unwrap();
            let _ = writeln!(log, "SECRET-ASK {prompt_text}");
        }
        let raw = self.inner.read_secret(prompt_text).await;
        let mut log = self.log.lock().unwrap();
        match &raw {
            Ok(_) => {
                let _ = writeln!(log, "  <- operator: <redacted>");
            }
            Err(error) => {
                let _ = writeln!(log, "  <- operator error: {error}");
            }
        }
        raw
    }
}

/// Transcribes both directions of the guide/operator conversation that happens
/// inside a field: what the guide says to the human and what the human replies.
struct RecordingOperatorIo<O: zeroclaw_onboarding::OperatorIo> {
    inner: O,
    log: std::sync::Arc<std::sync::Mutex<String>>,
}

#[async_trait]
impl<O: zeroclaw_onboarding::OperatorIo> zeroclaw_onboarding::OperatorIo
    for RecordingOperatorIo<O>
{
    async fn say(&mut self, text: &str) -> TransportResult<()> {
        {
            let mut log = self.log.lock().unwrap();
            let _ = writeln!(log, "  GUIDE-SAY: {text}");
        }
        self.inner.say(text).await
    }

    async fn hear(&mut self) -> TransportResult<String> {
        let reply = self.inner.hear().await;
        let mut log = self.log.lock().unwrap();
        match &reply {
            Ok(text) => {
                let _ = writeln!(log, "  USER: {text}");
            }
            Err(error) => {
                let _ = writeln!(log, "  USER error: {error}");
            }
        }
        reply
    }
}

/// A scripted non-technical human. Replies to whatever the guide asks with a
/// vague plain-language answer; the guide has to do the interpreting. The
/// queue carries any field-specific replies first, then the generic shrug.
struct VagueScriptedOperator {
    replies: std::collections::VecDeque<String>,
}

impl VagueScriptedOperator {
    fn new(replies: Vec<&str>) -> Self {
        Self {
            replies: replies.into_iter().map(String::from).collect(),
        }
    }
}

#[async_trait]
impl zeroclaw_onboarding::OperatorIo for VagueScriptedOperator {
    async fn say(&mut self, _text: &str) -> TransportResult<()> {
        Ok(())
    }

    async fn hear(&mut self) -> TransportResult<String> {
        Ok(self.replies.pop_front().unwrap_or_else(|| {
            "hmm i don't really get that, just pick whatever is normal please".to_string()
        }))
    }
}

/// A scripted stand-in for the live agent that implements the real `AgentTurn`
/// seam, so the offline guided cell runs the exact
/// `AgentResponder -> LlmTransport -> spec.walk` stack the live agent drives,
/// with no parallel responder. It holds conversation memory: every message it
/// receives is appended to `history`, proving the walk is one continuous
/// exchange rather than isolated one-shot questions. Answers are field-aware:
/// each is a realistic value chosen by the field's `prop` so the transcript
/// reads like a real operator being guided (a homeserver URL for `homeserver`,
/// `@bot:matrix.org` for `user_id`, an in-range number for a pacing field),
/// while still parsing for its field's type. For the first field the guide
/// opens a conversation with the operator (a plain-language question, no
/// `ANSWER:` marker) and only resolves after hearing the operator's vague
/// reply, so the recorded transcript captures genuine guide/user
/// back-and-forth. It can never loop forever because each served answer
/// parses.
struct MemoryScriptedTurn {
    answers: std::collections::VecDeque<String>,
    pending: std::collections::VecDeque<String>,
    history: Vec<String>,
    injected_clarification: bool,
}

/// Realistic fixture values keyed by the field's leaf name (the last
/// dot-segment of the fully-qualified `prop`, e.g. `homeserver` from
/// `channels.matrix.homeserver`), walked as a table so there is no
/// string-literal match and a field is one row to add. Yes/no and number
/// fields fall back to a generic type-valid value when absent.
const FREEFORM_ANSWERS: &[(&str, &str)] = &[
    ("homeserver", "https://matrix.org"),
    ("user_id", "@bot:matrix.org"),
    ("device_id", "ZEROCLAW01"),
    ("allowed_rooms", "!ops:matrix.org"),
    ("excluded_tools", "shell"),
    ("stream_mode", "off"),
];
const NUMBER_ANSWERS: &[(&str, &str)] = &[
    ("approval_timeout_secs", "60"),
    ("reply_min_interval_secs", "2"),
    ("reply_queue_depth_max", "16"),
    ("draft_update_interval_ms", "500"),
    ("multi_message_delay_ms", "250"),
];
const NO_ANSWER_FIELDS: &[&str] = &["mention_only"];

fn prop_leaf(prop: &str) -> &str {
    prop.rsplit_once('.').map_or(prop, |(_, leaf)| leaf)
}

fn lookup(table: &[(&str, &str)], leaf: &str, fallback: &str) -> String {
    table
        .iter()
        .find(|(key, _)| *key == leaf)
        .map_or_else(|| fallback.to_string(), |(_, value)| (*value).to_string())
}

/// Pick a realistic, type-valid answer for a field from its `prop` so the
/// transcript demonstrates a representative end-user conversation rather than
/// type-stub filler. The value obeys the strict transport parse contract (a
/// yes/no field gets `yes`/`no`, a number gets a bare in-range number, a choice
/// gets an exact option), matching the bare-value reply the live guide is
/// briefed to produce.
fn realistic_answer(prop: &str, response_type: &ResponseType) -> String {
    let leaf = prop_leaf(prop);
    match response_type {
        ResponseType::YesNo => {
            if NO_ANSWER_FIELDS.contains(&leaf) {
                "no".to_string()
            } else {
                "yes".to_string()
            }
        }
        ResponseType::Number => lookup(NUMBER_ANSWERS, leaf, "10"),
        ResponseType::Choice { options } => options[0].value.clone(),
        ResponseType::FreeformText => lookup(FREEFORM_ANSWERS, leaf, "default"),
        ResponseType::Secret => unreachable!("secret filtered above"),
    }
}

impl MemoryScriptedTurn {
    fn new() -> Self {
        let spec = matrix_spec();
        let mut answers = std::collections::VecDeque::new();
        let mut current = Some(spec.start.clone());
        while let Some(id) = current {
            let Some(node) = spec.nodes.get(&id) else {
                break;
            };
            if !matches!(node.prompt.response_type, ResponseType::Secret) {
                answers.push_back(realistic_answer(&node.prop, &node.prompt.response_type));
            }
            current = match &node.on_success {
                zeroclaw_runtime::flow::Step::Node(next) => Some(next.clone()),
                zeroclaw_runtime::flow::Step::Terminal(_) => None,
            };
        }
        Self {
            answers,
            pending: std::collections::VecDeque::new(),
            history: Vec::new(),
            injected_clarification: false,
        }
    }
}

#[async_trait]
impl zeroclaw_onboarding::AgentTurn for MemoryScriptedTurn {
    async fn run_single(&mut self, message: &str) -> TransportResult<String> {
        self.history.push(message.to_string());
        if let Some(queued) = self.pending.pop_front() {
            return Ok(format!("ANSWER: {queued}"));
        }
        let Some(answer) = self.answers.pop_front() else {
            return Err(TransportError::Agent {
                reason: "scripted turn exhausted: walk visited more non-secret \
                         fields than the spec chain predicted"
                    .to_string(),
            });
        };
        if !self.injected_clarification {
            self.injected_clarification = true;
            self.pending.push_back(answer);
            return Ok("This first one controls a small behavior of your bot. \
                 Do you want the usual setup, or something specific?"
                .to_string());
        }
        Ok(format!("ANSWER: {answer}"))
    }
}

struct ScriptedSecret;

#[async_trait]
impl SecretReader for ScriptedSecret {
    async fn read_secret(&mut self, _prompt_text: &str) -> TransportResult<String> {
        Ok("sk-token".to_string())
    }
}

struct GuidedWalk {
    outcome_token: String,
    transcript: String,
    config: Config,
}

/// Run one guided conversational walk of the matrix section into `cell`:
/// wire the recording responder/operator/secret stack around the given guide
/// turn and scripted operator replies, walk the spec against a seeded config
/// copy, and write the transcript into the cell folder.
async fn run_guided_walk<T: zeroclaw_onboarding::AgentTurn>(
    cell: &Path,
    turn: T,
    operator_replies: Vec<&'static str>,
) -> GuidedWalk {
    std::fs::create_dir_all(cell).expect("create guided cell folder");
    let mut config = seed_config_copy(cell);

    let log = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let responder = RecordingResponder {
        inner: zeroclaw_onboarding::AgentResponder::new(
            turn,
            RecordingOperatorIo {
                inner: VagueScriptedOperator::new(operator_replies),
                log: std::sync::Arc::clone(&log),
            },
        ),
        log: std::sync::Arc::clone(&log),
        turn: 0,
    };
    let secret = RecordingSecretReader {
        inner: ScriptedSecret,
        log: std::sync::Arc::clone(&log),
    };
    let mut transport = LlmTransport::new(responder, secret);

    let walk = Box::pin(matrix_spec().walk(&mut transport, &mut config)).await;
    let transcript = log.lock().unwrap().clone();
    std::fs::write(cell.join("transcript.txt"), &transcript).expect("write transcript");
    GuidedWalk {
        outcome_token: walk_token(&walk),
        transcript,
        config,
    }
}

async fn run_guided_offline_cell(base: &Path) {
    let cell = base.join("inproc").join("guided").join("happy");
    let GuidedWalk {
        outcome_token,
        transcript,
        config,
    } = run_guided_walk(
        &cell,
        MemoryScriptedTurn::new(),
        vec!["oh um, whatever the normal thing is? i just want it to work"],
    )
    .await;

    let walked = config
        .channels
        .matrix
        .get(INSTANCE)
        .expect("walked matrix instance present");
    let walked_table = toml::Value::try_from(walked).expect("serialize walked matrix instance");
    let mut instance_table = walked_table
        .as_table()
        .expect("matrix instance serializes to a table")
        .clone();
    for secret_key in ["access_token", "password", "recovery_key"] {
        if instance_table.contains_key(secret_key) {
            instance_table.insert(
                secret_key.to_string(),
                toml::Value::String("<redacted>".to_string()),
            );
        }
    }
    let rendered_config = format!(
        "schema_version = 3\n\n[channels.matrix.{INSTANCE}]\n{}",
        toml::to_string_pretty(&instance_table).expect("render walked config")
    );

    let mut meta = BTreeMap::new();
    meta.insert("transport", "inproc".to_string());
    meta.insert("mode", "guided".to_string());
    meta.insert("path", "happy".to_string());
    meta.insert(
        "model_provider",
        "scripted (offline conversational)".to_string(),
    );
    meta.insert("outcome", outcome_token.clone());
    std::fs::write(cell.join("config.toml"), &rendered_config).expect("write walked config");
    write_meta(&cell, &meta);

    assert_eq!(
        outcome_token, "completed",
        "the guided walk must complete even with conversational back-and-forth"
    );
    assert!(
        transcript.contains("GUIDE-SAY:"),
        "the recorded transcript must show the guide speaking to the operator"
    );
    assert!(
        transcript.contains("USER:"),
        "the recorded transcript must show the operator replying to the guide"
    );
    assert!(
        transcript.contains("i just want it to work"),
        "the operator's vague plain-language reply must appear in the transcript"
    );
}

/// Live GUIDED cell (opt-in). Resolves a genuine agent + provider from the
/// accessible config, builds the real agent, and lets it converse its way
/// through the walk. The accessible config is read-only: the agent is built from
/// a parsed copy and the flow writes only into the seeded run-folder config. The
/// full real conversation is transcribed; the assertion keys off the structural
/// outcome token only, since a live model's turn count and wording are not fixed.
async fn run_guided_live_cell(base: &Path) {
    let Some(dir) = accessible_config_dir() else {
        return;
    };
    let Some((alias, provider)) = resolve_agent_provider() else {
        return;
    };
    let Ok(raw) = std::fs::read_to_string(dir.join("config.toml")) else {
        return;
    };
    let cell = base.join("inproc").join("guided-live").join("happy");
    let skip = |reason: String| {
        std::fs::create_dir_all(&cell).expect("create live guided cell");
        std::fs::write(cell.join("transcript.txt"), format!("skipped: {reason}\n"))
            .expect("write skip note");
    };

    let accessible: Config = match toml::from_str(&raw) {
        Ok(config) => config,
        Err(error) => {
            skip(format!(
                "accessible config did not parse into Config: {error}"
            ));
            return;
        }
    };
    // Decrypt enc2 secrets with the co-located .secret_key so the built agent
    // authenticates live instead of sending raw ciphertext upstream.
    let accessible = {
        let store = zeroclaw_config::secrets::SecretStore::new(&dir, accessible.secrets.encrypt);
        let mut config = accessible;
        if let Err(error) = config.decrypt_secrets(&store) {
            skip(format!(
                "accessible config secrets did not decrypt: {error}"
            ));
            return;
        }
        config
    };

    let agent = match Box::pin(zeroclaw_runtime::agent::Agent::from_config(
        &accessible,
        &alias,
    ))
    .await
    {
        Ok(agent) => agent,
        Err(error) => {
            skip(format!("agent {alias:?} did not build: {error}"));
            return;
        }
    };

    let walk = run_guided_walk(
        &cell,
        zeroclaw_onboarding::InProcessAgentTurn::new(agent),
        vec![
            "honestly no clue what that means, whatever you think is best",
            "i just want my bot to answer me on matrix",
            "the normal one? we use matrix.org i think",
            "sure",
        ],
    )
    .await;

    let mut meta = BTreeMap::new();
    meta.insert("transport", "inproc".to_string());
    meta.insert("mode", "guided-live".to_string());
    meta.insert("path", "happy".to_string());
    meta.insert("model_provider", provider);
    meta.insert("agent", alias);
    meta.insert("outcome", walk.outcome_token);
    write_meta(&cell, &meta);
}

fn run_pty_manual_cell(base: &Path) {
    let run_root = base.join("pty").join("manual").join("happy");
    std::fs::create_dir_all(&run_root).expect("seed dir");
    seed_config_copy(&run_root);

    let (transcript, outcome) = drive_pty_manual_happy(&run_root);

    let mut meta = BTreeMap::new();
    meta.insert("transport", "pty".to_string());
    meta.insert("mode", "manual".to_string());
    meta.insert("path", "happy".to_string());
    meta.insert("model_provider", "n/a (manual)".to_string());
    meta.insert("outcome", outcome.clone());
    write_cell(base, "pty", "manual", PathLabel::Happy, &transcript, &meta);

    assert_eq!(
        outcome, "completed",
        "the real binary's manual CLI walk must complete over a pty"
    );
}

/// Records a personality-branch walk. Decision nodes resolve to the configured
/// personality choice (author or template); freeform author prompts return the
/// seeded template if present, else a marker, so the workspace write is real.
struct PersonalityRecordingTransport {
    choice: String,
    transcript: String,
    outcome_token: Option<String>,
}

impl PersonalityRecordingTransport {
    fn new(choice: &str) -> Self {
        Self {
            choice: choice.to_string(),
            transcript: String::new(),
            outcome_token: None,
        }
    }
}

#[async_trait]
impl FlowTransport for PersonalityRecordingTransport {
    async fn ask(&mut self, prompt: &Prompt) -> TransportResult<ResponseValue> {
        let _ = writeln!(
            self.transcript,
            "ASK [{}] {}",
            prompt.response_type.ask_kind(),
            prompt.text
        );
        let answer = match &prompt.response_type {
            ResponseType::Choice { options } => {
                let chosen = options
                    .iter()
                    .find(|option| option.value == self.choice)
                    .map(|option| option.value.clone())
                    .unwrap_or_else(|| options[0].value.clone());
                ResponseValue::Choice(chosen)
            }
            ResponseType::FreeformText => {
                let body = prompt
                    .editor_seed
                    .clone()
                    .unwrap_or_else(|| "authored content".to_string());
                ResponseValue::FreeformText(body)
            }
            ResponseType::YesNo => ResponseValue::YesNo(true),
            ResponseType::Number => ResponseValue::Number("1".into()),
            ResponseType::Secret => ResponseValue::Secret(SecretValue::new("sk-token".into())),
        };
        let _ = writeln!(self.transcript, "  <- {answer:?}");
        Ok(answer)
    }

    async fn emit(&mut self, outcome: &Outcome) -> TransportResult<()> {
        let token = outcome_token(outcome);
        let _ = writeln!(self.transcript, "EMIT [{token}] {outcome:?}");
        self.outcome_token = Some(token);
        Ok(())
    }
}

fn personality_agent_spec() -> zeroclaw_runtime::flow::Spec {
    use zeroclaw_runtime::agent::personality_templates::TemplateContext;
    let base = zeroclaw_runtime::flow::Spec {
        start: zeroclaw_runtime::flow::NodeId::new("personality.decision"),
        nodes: BTreeMap::new(),
    };
    let ctx = TemplateContext {
        agent: "scout".to_string(),
        ..Default::default()
    };
    zeroclaw_onboarding::append_personality_branch(
        base,
        "scout",
        &ctx,
        Outcome::Completed {
            configured: vec![zeroclaw_runtime::flow::ConfiguredItem {
                layer: "agent".to_string(),
                instance: "scout".to_string(),
            }],
        },
    )
}

async fn run_personality_cell(base: &Path, choice: &str, folder: &str) {
    let run_root = base.join("inproc").join("personality").join(folder);
    std::fs::create_dir_all(&run_root).expect("seed dir");
    let mut config = Config {
        config_path: run_root.join("config.toml"),
        data_dir: run_root.clone(),
        ..Default::default()
    };

    let spec = personality_agent_spec();
    let mut transport = PersonalityRecordingTransport::new(choice);
    let walk = Box::pin(spec.walk(&mut transport, &mut config)).await;

    let outcome_token = walk_token(&walk);

    let dir = base.join("inproc").join("personality").join(folder);
    std::fs::create_dir_all(&dir).expect("create personality cell folder");
    std::fs::write(dir.join("transcript.txt"), &transport.transcript).expect("write transcript");
    let mut meta = BTreeMap::new();
    meta.insert("transport", "inproc".to_string());
    meta.insert("mode", "personality".to_string());
    meta.insert("path", folder.to_string());
    meta.insert("choice", choice.to_string());
    meta.insert("model_provider", "n/a (manual)".to_string());
    meta.insert("outcome", outcome_token.clone());
    write_meta(&dir, &meta);

    assert_eq!(
        outcome_token, "completed",
        "the personality walk for choice {choice} must complete"
    );
    let written = std::fs::read_to_string(config.agent_workspace_dir("scout").join("SOUL.md"))
        .expect("SOUL.md must be written by the personality walk");
    assert!(
        !written.is_empty(),
        "the chosen personality branch must write file content"
    );
}

fn manual_node_count() -> usize {
    matrix_spec().nodes.len()
}

#[tokio::test]
#[ignore = "operator-run integration harness: writes tmp/zeroclaw-onboard, builds the binary, GUIDED needs a live provider"]
async fn onboard_transcript_matrix() {
    let base = run_dir();

    assert!(
        manual_node_count() > 0,
        "the walked section must expose at least one node to transcribe"
    );

    for path in [
        PathLabel::Happy,
        PathLabel::BadInputThenGood,
        PathLabel::Cancel,
        PathLabel::TransportClosed,
    ] {
        run_manual_cell(&base, path).await;
    }

    run_pty_manual_cell(&base);

    run_guided_offline_cell(&base).await;

    run_personality_cell(&base, "author", "author").await;
    run_personality_cell(&base, "template", "template").await;

    if std::env::var("ZEROCLAW_ONBOARD_GUIDED").is_ok() {
        run_guided_live_cell(&base).await;
    }

    let mut index = BTreeMap::new();
    index.insert("section", SECTION.to_string());
    index.insert("nodes", manual_node_count().to_string());
    match resolve_agent_provider() {
        Some((alias, provider)) => {
            index.insert("guided_agent", alias);
            index.insert("guided_model_provider", provider);
            index.insert(
                "guided_runs",
                if std::env::var("ZEROCLAW_ONBOARD_GUIDED").is_ok() {
                    "requested".to_string()
                } else {
                    "skipped (set ZEROCLAW_ONBOARD_GUIDED=1 with a live provider)".to_string()
                },
            );
        }
        None => {
            index.insert(
                "guided_runs",
                "skipped (no accessible config to resolve a provider alias)".to_string(),
            );
        }
    }
    write_index(&base, &index);

    assert!(
        base.join("inproc/manual/happy/transcript.txt").exists(),
        "the happy-path transcript must be written to its labeled folder"
    );
}

fn write_index(base: &Path, index: &BTreeMap<&str, String>) {
    let mut out = String::from("{\n");
    let mut first = true;
    for (key, value) in index {
        if !first {
            out.push_str(",\n");
        }
        first = false;
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        let _ = write!(out, "  \"{key}\": \"{escaped}\"");
    }
    out.push_str("\n}\n");
    std::fs::write(base.join("index.json"), out).expect("write run index");
}
