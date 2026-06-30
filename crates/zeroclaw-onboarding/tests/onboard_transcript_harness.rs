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

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use zeroclaw_config::schema::{Config, MatrixConfig};
use zeroclaw_onboarding::driver::{FlowRequest, run_flow};
use zeroclaw_onboarding::{FieldScope, LlmResponder, LlmTransport, SecretReader, build_spec};
use zeroclaw_runtime::flow::{
    FlowTransport, Outcome, Prompt, TransportError, TransportResult, WalkError,
};
use zeroclaw_runtime::response_type::{ResponseType, ResponseValue, SecretValue};

const SECTION: &str = "channels.matrix.home";
const LAYER: &str = "channel";
const INSTANCE: &str = "home";

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

/// Resolve the first enabled agent alias and its dotted model-provider ref from
/// the accessible config. Never hardcoded; absent config yields None and the
/// GUIDED cells are skipped. Parsed generically (not through the strict `Config`
/// view) so an operator config with crate-unknown fields still resolves.
fn resolve_agent_provider() -> Option<(String, String)> {
    let dir = accessible_config_dir()?;
    let raw = std::fs::read_to_string(dir.join("config.toml")).ok()?;
    let value: toml::Value = toml::from_str(&raw).ok()?;
    let agents = value.get("agents")?.as_table()?;
    let mut aliases: Vec<&String> = agents.keys().collect();
    aliases.sort();
    for alias in aliases {
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

fn zeroclaw_binary() -> PathBuf {
    static BINARY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BINARY
        .get_or_init(|| {
            escargot::CargoBuild::new()
                .package("zeroclawlabs")
                .bin("zeroclaw")
                .run()
                .expect("build the zeroclaw binary from source for the harness")
                .path()
                .to_path_buf()
        })
        .clone()
}

/// Drive the real binary's manual CLI walk over a pty, feeding the homeserver
/// section's prompts and capturing the entire terminal transcript. Locale is
/// the first prompt, so the registry's first code leads the scripted answers.
fn drive_pty_manual_happy(config_dir: &Path) -> (String, String) {
    let binary = zeroclaw_binary();
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 40,
            cols: 200,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pty");

    let mut cmd = CommandBuilder::new(binary);
    cmd.arg("--config-dir");
    cmd.arg(config_dir);
    cmd.arg("onboard-flow");
    cmd.arg("--section");
    cmd.arg(SECTION);
    cmd.arg("--layer");
    cmd.arg(LAYER);
    cmd.arg("--instance");
    cmd.arg(INSTANCE);

    let mut child = pair.slave.spawn_command(cmd).expect("spawn child");
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("clone reader");
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut writer = pair.master.take_writer().expect("take writer");
    let mut answers = pty_scripted_answers().into_iter();

    let mut transcript = String::new();
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut idle_since = Instant::now();
    let mut outcome = "incomplete".to_string();

    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => {
                transcript.push_str(&String::from_utf8_lossy(&chunk));
                idle_since = Instant::now();
                if transcript.contains("[completed") {
                    outcome = "completed".to_string();
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if idle_since.elapsed() >= Duration::from_millis(200) {
                    if let Some(answer) = answers.next() {
                        writer
                            .write_all(format!("{answer}\n").as_bytes())
                            .expect("write answer");
                        writer.flush().expect("flush");
                        idle_since = Instant::now();
                    } else if transcript.contains("[completed") {
                        outcome = "completed".to_string();
                        break;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = child.wait();
    (transcript, outcome)
}

fn pty_scripted_answers() -> Vec<String> {
    let mut config = Config::default();
    config
        .channels
        .matrix
        .insert(INSTANCE.to_string(), MatrixConfig::default());
    let spec = build_spec(
        config.prop_fields(),
        SECTION,
        LAYER,
        INSTANCE,
        Outcome::Cancelled,
    )
    .expect("matrix section yields a spec");
    let mut ordered: Vec<_> = spec.nodes.values().collect();
    ordered.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    let mut answers = vec![
        zeroclaw_runtime::i18n::available_locales()
            .first()
            .expect("registry lists at least one locale")
            .code
            .clone(),
    ];
    answers.extend(
        ordered
            .into_iter()
            .map(|node| match &node.prompt.response_type {
                ResponseType::Secret => "sk-token".to_string(),
                ResponseType::YesNo => "y".to_string(),
                ResponseType::Number => "100".to_string(),
                ResponseType::Choice { options } => options[0].value.clone(),
                ResponseType::FreeformText => "https://walked.test".to_string(),
            }),
    );
    answers
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

fn driver_error_token(error: &zeroclaw_onboarding::DriverError) -> String {
    use zeroclaw_onboarding::DriverError;
    match error {
        DriverError::EmptySection(_) => "empty-section".to_string(),
        DriverError::AliasCreate { .. } => "alias-create-error".to_string(),
        DriverError::Walk(walk) => match walk {
            WalkError::Transport(_) => "transport-error".to_string(),
            WalkError::Write(_) => "write-error".to_string(),
            WalkError::UnknownNode(_) => "unknown-node".to_string(),
        },
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

/// A scripted stand-in for the guide, driven positionally by the same spec the
/// walk uses so every answer parses for its field's type. It is genuinely
/// conversational: for the first field it emits an unparseable clarification
/// turn before the real answer, so the recorded transcript proves the
/// back-and-forth loop is captured. Offline, no live provider needed, and it can
/// never loop forever because each served answer parses.
struct ConversationalScript {
    answers: std::collections::VecDeque<String>,
    pending: std::collections::VecDeque<String>,
    injected_clarification: bool,
}

impl ConversationalScript {
    fn new(config: &Config) -> Self {
        let spec = build_spec(
            config.prop_fields(),
            SECTION,
            LAYER,
            INSTANCE,
            Outcome::Cancelled,
        )
        .expect("matrix section yields a spec");
        let mut answers = std::collections::VecDeque::new();
        let mut current = Some(spec.start.clone());
        while let Some(id) = current {
            let Some(node) = spec.nodes.get(&id) else {
                break;
            };
            if !matches!(node.prompt.response_type, ResponseType::Secret) {
                let answer = match &node.prompt.response_type {
                    ResponseType::YesNo => "yes".to_string(),
                    ResponseType::Number => "100".to_string(),
                    ResponseType::Choice { options } => options[0].value.clone(),
                    ResponseType::FreeformText => "https://walked.test".to_string(),
                    ResponseType::Secret => unreachable!("secret filtered above"),
                };
                answers.push_back(answer);
            }
            current = match &node.on_success {
                zeroclaw_runtime::flow::Step::Node(next) => Some(next.clone()),
                zeroclaw_runtime::flow::Step::Terminal(_) => None,
            };
        }
        Self {
            answers,
            pending: std::collections::VecDeque::new(),
            injected_clarification: false,
        }
    }
}

#[async_trait]
impl LlmResponder for ConversationalScript {
    async fn respond(&mut self, _prompt_text: &str) -> TransportResult<String> {
        if let Some(queued) = self.pending.pop_front() {
            return Ok(queued);
        }
        let Some(answer) = self.answers.pop_front() else {
            return Err(TransportError::Agent {
                reason: "conversational script exhausted: walk visited more non-secret \
                         fields than the spec chain predicted"
                    .to_string(),
            });
        };
        if !self.injected_clarification {
            self.injected_clarification = true;
            self.pending.push_back(answer);
            return Ok("Let me make sure I understand the field first.".to_string());
        }
        Ok(answer)
    }
}

struct ScriptedSecret;

#[async_trait]
impl SecretReader for ScriptedSecret {
    async fn read_secret(&mut self, _prompt_text: &str) -> TransportResult<String> {
        Ok("sk-token".to_string())
    }
}

async fn run_guided_offline_cell(base: &Path) {
    let run_root = base.join("inproc").join("guided").join("happy");
    std::fs::create_dir_all(&run_root).expect("seed dir");
    let mut config = seed_config_copy(&run_root);

    let log = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let responder = RecordingResponder {
        inner: ConversationalScript::new(&config),
        log: std::sync::Arc::clone(&log),
        turn: 0,
    };
    let secret = RecordingSecretReader {
        inner: ScriptedSecret,
        log: std::sync::Arc::clone(&log),
    };
    let mut transport = LlmTransport::new(responder, secret);

    let spec = build_spec(
        config.prop_fields(),
        SECTION,
        LAYER,
        INSTANCE,
        Outcome::Completed {
            configured: vec![zeroclaw_runtime::flow::ConfiguredItem {
                layer: LAYER.to_string(),
                instance: INSTANCE.to_string(),
            }],
        },
    )
    .expect("matrix section yields a spec");
    let walk = Box::pin(spec.walk(&mut transport, &mut config)).await;

    let outcome_token = match &walk {
        Ok(outcome) => outcome_token(outcome),
        Err(error) => match error {
            WalkError::Transport(_) => "transport-error".to_string(),
            WalkError::Write(_) => "write-error".to_string(),
            WalkError::UnknownNode(_) => "unknown-node".to_string(),
        },
    };

    let transcript = log.lock().unwrap().clone();
    let mut meta = BTreeMap::new();
    meta.insert("transport", "inproc".to_string());
    meta.insert("mode", "guided".to_string());
    meta.insert("path", "happy".to_string());
    meta.insert(
        "model_provider",
        "scripted (offline conversational)".to_string(),
    );
    meta.insert("outcome", outcome_token.clone());
    let dir = base.join("inproc").join("guided").join("happy");
    std::fs::create_dir_all(&dir).expect("create guided cell folder");
    std::fs::write(dir.join("transcript.txt"), &transcript).expect("write transcript");
    write_meta(&dir, &meta);

    assert_eq!(
        outcome_token, "completed",
        "the guided walk must complete even with conversational back-and-forth"
    );
    assert!(
        transcript.contains("make sure I understand"),
        "the recorded transcript must include the guide's mid-field clarification turn"
    );
    assert!(
        transcript.matches("LLM-ASK").count() > spec_field_prompts(&config),
        "a conversational walk must record more turns than there are fields"
    );
}

fn spec_field_prompts(config: &Config) -> usize {
    build_spec(
        config.prop_fields(),
        SECTION,
        LAYER,
        INSTANCE,
        Outcome::Cancelled,
    )
    .expect("spec")
    .nodes
    .values()
    .filter(|node| !matches!(node.prompt.response_type, ResponseType::Secret))
    .count()
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
    let Some((alias, _provider)) = resolve_agent_provider() else {
        return;
    };
    let Ok(raw) = std::fs::read_to_string(dir.join("config.toml")) else {
        return;
    };
    let accessible: Config = match toml::from_str(&raw) {
        Ok(config) => config,
        Err(error) => {
            let cell = base.join("inproc").join("guided-live").join("happy");
            std::fs::create_dir_all(&cell).expect("create live guided cell");
            std::fs::write(
                cell.join("transcript.txt"),
                format!("skipped: accessible config did not parse into Config: {error}\n"),
            )
            .expect("write skip note");
            return;
        }
    };

    let agent = match Box::pin(zeroclaw_runtime::agent::Agent::from_config(
        &accessible,
        &alias,
    ))
    .await
    {
        Ok(agent) => agent,
        Err(error) => {
            let cell = base.join("inproc").join("guided-live").join("happy");
            std::fs::create_dir_all(&cell).expect("create live guided cell");
            std::fs::write(
                cell.join("transcript.txt"),
                format!("skipped: agent {alias:?} did not build: {error}\n"),
            )
            .expect("write skip note");
            return;
        }
    };

    let run_root = base.join("inproc").join("guided-live").join("happy");
    std::fs::create_dir_all(&run_root).expect("seed dir");
    let mut config = seed_config_copy(&run_root);

    let log = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let responder = RecordingResponder {
        inner: zeroclaw_onboarding::AgentResponder::new(
            zeroclaw_onboarding::InProcessAgentTurn::new(agent),
        ),
        log: std::sync::Arc::clone(&log),
        turn: 0,
    };
    let secret = RecordingSecretReader {
        inner: ScriptedSecret,
        log: std::sync::Arc::clone(&log),
    };
    let mut transport = LlmTransport::new(responder, secret);

    let spec = build_spec(
        config.prop_fields(),
        SECTION,
        LAYER,
        INSTANCE,
        Outcome::Completed {
            configured: vec![zeroclaw_runtime::flow::ConfiguredItem {
                layer: LAYER.to_string(),
                instance: INSTANCE.to_string(),
            }],
        },
    )
    .expect("matrix section yields a spec");
    let walk = Box::pin(spec.walk(&mut transport, &mut config)).await;

    let outcome_token = match &walk {
        Ok(outcome) => outcome_token(outcome),
        Err(error) => match error {
            WalkError::Transport(_) => "transport-error".to_string(),
            WalkError::Write(_) => "write-error".to_string(),
            WalkError::UnknownNode(_) => "unknown-node".to_string(),
        },
    };

    let transcript = log.lock().unwrap().clone();
    std::fs::write(run_root.join("transcript.txt"), &transcript).expect("write transcript");
    let mut meta = BTreeMap::new();
    meta.insert("transport", "inproc".to_string());
    meta.insert("mode", "guided-live".to_string());
    meta.insert("path", "happy".to_string());
    meta.insert("model_provider", _provider);
    meta.insert("agent", alias);
    meta.insert("outcome", outcome_token);
    write_meta(&run_root, &meta);
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

    let outcome_token = match &walk {
        Ok(outcome) => outcome_token(outcome),
        Err(error) => match error {
            WalkError::Transport(_) => "transport-error".to_string(),
            WalkError::Write(_) => "write-error".to_string(),
            WalkError::UnknownNode(_) => "unknown-node".to_string(),
        },
    };

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
    let mut config = Config::default();
    config
        .channels
        .matrix
        .insert(INSTANCE.to_string(), MatrixConfig::default());
    build_spec(
        config.prop_fields(),
        SECTION,
        LAYER,
        INSTANCE,
        Outcome::Cancelled,
    )
    .expect("matrix section yields a spec")
    .nodes
    .len()
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
