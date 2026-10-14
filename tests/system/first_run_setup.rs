//! First-run setup coverage of the shared quickstart apply core and the real
//! config loader.
//!
//! # What this pins
//!
//! A first run is the one path where a user has nothing and ends up with a
//! config the runtime must be able to start from. That path can produce a
//! config that *looks* plausible — `doctor` counts a channel,
//! `Config::validate()` is happy — while the agent's channel binding points at
//! a block that never received the values the user typed, and the only person
//! who finds out is the user whose first launch fails. Nothing below the
//! system level catches that, because every layer in isolation is correct.
//!
//! # What is and is not covered
//!
//! **Covered.** The shared Quickstart apply core
//! (`zeroclaw_runtime::quickstart::apply_with_surface`) driven with a
//! hand-built [`BuilderSubmission`] into a hermetic temp install root; the
//! persisted `config.toml`; and — in
//! [`first_run_config_loads_through_the_real_binary`] — the real
//! `Config::load_or_init()` loader, reached by spawning the actual `zeroclaw`
//! binary as a child process against that install root.
//!
//! **Not covered.** The adapters that *build* a submission are each their own
//! surface and none of them run here: the zerocode TUI form
//! (`apps/zerocode/src/quickstart_pane.rs::to_submission`), the interactive CLI
//! quickstart (`src/main.rs`), and the gateway HTTP handler
//! (`crates/zeroclaw-gateway/src/api_quickstart.rs`). A bug that lives purely
//! in one of those adapters — a field the form never collects, a key the HTTP
//! layer renames — is invisible to this file. Those are follow-up matrix rows,
//! not something to fake here with a hand-built submission that would only
//! restate what the core already guarantees.
//!
//! In-process reloads use `migration::migrate_to_current`, which is the exact
//! parse `load_or_init` performs on the file body but *not* the whole loader:
//! it skips salvage bookkeeping (`degraded_sections`), runtime path stamping,
//! and env-var overrides. The child-process test is what covers those.
//!
//! No network, no real credentials, no operator config: every value is a
//! neutral placeholder and every byte written lands under a `TempDir`. The
//! child process gets a scrubbed, child-scoped environment; the test process
//! never mutates its own env.
//!
//! # How to add another first-run scenario
//!
//! 1. Build a [`BuilderSubmission`] with [`submission`] and, for channels,
//!    [`fresh_channel`]. Only keys advertised by
//!    `quickstart::field_shape(FieldSection::Channel, <type>)` are accepted by
//!    the apply path — that allowlist is deliberate, so a new scenario that
//!    needs a new field must first surface it in the schema.
//! 2. `let run = FirstRun::quickstart(submission).await;` — this persists into
//!    a fresh temp dir and hands back both the applied and reloaded config.
//! 3. Assert with the shared harness checks:
//!    [`FirstRun::assert_config_validates`],
//!    [`FirstRun::assert_agent_channel_aliases_resolve_to_populated_blocks`],
//!    and [`FirstRun::assert_submitted_channel_fields_persisted`]. Add
//!    scenario-specific typed assertions on `run.reloaded()` afterwards.
//! 4. To assert against the real loader instead of the in-process parse, call
//!    [`run_zeroclaw`] with a read-only subcommand and check its output.
//! 5. Any new harness check needs a matching guard test proving it fails on the
//!    shape it claims to catch — see the guard section at the bottom.
//!
//! Live, credential-backed channel connectivity stays out of this file — that
//! belongs in `tests/live/`, ignored by default.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tempfile::TempDir;
use zeroclaw_config::presets::{
    AgentIdentity, BuilderSubmission, ChannelQuickStart, MemoryChoice, ModelProviderChoice,
    SelectorChoice,
};
use zeroclaw_config::schema::{Config, DEFAULT_WEBHOOK_CHANNEL_PORT};
use zeroclaw_config::secrets::SecretStore;
use zeroclaw_runtime::quickstart::{self, FieldSection, Surface};

// ═════════════════════════════════════════════════════════════════════════════
// Harness
// ═════════════════════════════════════════════════════════════════════════════

/// One complete first run: a clean install root, the submission that was
/// applied to it, and the config as it reads back off disk.
struct FirstRun {
    dir: TempDir,
    submission: BuilderSubmission,
    applied: Config,
    reloaded: Config,
    raw: toml::Value,
}

impl FirstRun {
    /// Drive the shared Quickstart apply path into a clean temp install root,
    /// then reload the persisted `config.toml`.
    ///
    /// `Surface::Test` is the enum's own test origin, so the apply path stamps
    /// events exactly as it does in production without pulling the CLI-only
    /// Fluent error rendering into a library test.
    async fn quickstart(submission: BuilderSubmission) -> Self {
        let dir = tempfile::tempdir().expect("temp install root");
        let mut config = Config {
            config_path: dir.path().join("config.toml"),
            data_dir: dir.path().join("data"),
            ..Default::default()
        };
        // A fresh install writes its skeleton before the user answers anything.
        config.save().await.expect("seed a clean first-run config");

        quickstart::apply_with_surface(submission.clone(), &mut config, Surface::Test)
            .await
            .expect("quickstart apply must succeed on a clean first run");

        let raw_text = std::fs::read_to_string(dir.path().join("config.toml"))
            .expect("quickstart must persist config.toml");
        // The daemon's loader (`Config::load_or_init`) parses through the
        // migration chain rather than plain serde; use the same entry point so
        // this test sees what a real second launch would see.
        let reloaded = zeroclaw_config::migration::migrate_to_current(&raw_text)
            .expect("persisted config must load through the normal loader");
        let raw: toml::Value = toml::from_str(&raw_text).expect("persisted config must be TOML");

        Self {
            dir,
            submission,
            applied: config,
            reloaded,
            raw,
        }
    }

    fn install_root(&self) -> &Path {
        self.dir.path()
    }

    fn reloaded(&self) -> &Config {
        &self.reloaded
    }

    /// The reloaded config with the runtime-only path fields restored, the way
    /// `Config::load_or_init` stamps them after parsing. Needed by any consumer
    /// that touches the install root (doctor's workspace/daemon checks) so the
    /// test never reaches outside its temp dir.
    fn reloaded_with_paths(&self) -> Config {
        Config {
            config_path: self.applied.config_path.clone(),
            data_dir: self.applied.data_dir.clone(),
            ..self.reloaded.clone()
        }
    }

    /// Rewrite the persisted config on disk, then re-read it exactly as
    /// [`FirstRun::quickstart`] does.
    ///
    /// Used only by the guard tests at the bottom of this file, which corrupt
    /// a first-run config into a known-bad shape to prove the checks above
    /// actually fire. A check that stays green against its own failure mode is
    /// decoration, not coverage — and a decorative check is exactly what let
    /// the broken first-run config ship in the first place.
    ///
    /// The corrupted document is written back to `config.toml` so guards that
    /// spawn the real binary see it too; comment formatting from the config
    /// writer is lost in the round-trip, which does not matter for a fixture.
    fn with_corrupted_disk_view(mut self, mutate: impl FnOnce(&mut toml::Table)) -> Self {
        let mut doc = self
            .raw
            .as_table()
            .expect("persisted config is a TOML table")
            .clone();
        mutate(&mut doc);
        let raw_text = toml::to_string(&doc).expect("corrupted config must re-serialize");
        std::fs::write(self.dir.path().join("config.toml"), &raw_text)
            .expect("corrupted config must be writable");
        self.reloaded = zeroclaw_config::migration::migrate_to_current(&raw_text)
            .expect("corrupted config must still parse");
        self.raw = toml::Value::Table(doc);
        self
    }

    /// (1) The produced config passes the same validation the daemon runs.
    fn assert_config_validates(&self) {
        if let Err(err) = self.reloaded.validate() {
            panic!("first-run config failed Config::validate(): {err:#}");
        }
    }

    /// (2) **The binding check.** Every channel alias an agent is bound to must
    /// resolve to a channel block that actually exists on disk and actually
    /// carries content.
    ///
    /// `Config::validate()` only proves the alias *key* is present in the map.
    /// The failure this file exists for is a config where the key was reachable
    /// but the block behind it was empty of everything the user typed, so the
    /// channel runtime had nothing to start from. This check therefore reads
    /// the persisted TOML directly and demands a non-empty table.
    fn assert_agent_channel_aliases_resolve_to_populated_blocks(&self) {
        let cfg = &self.reloaded;
        assert!(
            !cfg.agents.is_empty(),
            "a first run must leave at least one agent configured"
        );

        let mut agent_aliases: Vec<&String> = cfg.agents.keys().collect();
        agent_aliases.sort();
        for agent_alias in agent_aliases {
            let agent = &cfg.agents[agent_alias];
            for (i, reference) in agent.channels.iter().enumerate() {
                let site = format!("agents.{agent_alias}.channels[{i}]");
                let reference = reference.as_str().trim();
                let (channel_type, alias) = reference.split_once('.').unwrap_or_else(|| {
                    panic!("{site} = {reference:?} is not a `<type>.<alias>` reference")
                });

                // The typed view: the alias key exists under `channels.<type>`.
                let keys = cfg
                    .get_map_keys(&format!("channels.{channel_type}"))
                    .unwrap_or_else(|| {
                        panic!("{site} = {reference:?} but `channels.{channel_type}` is not a known channel section")
                    });
                assert!(
                    keys.iter().any(|key| key == alias),
                    "{site} = {reference:?} but `channels.{channel_type}` has no `{alias}` entry (configured aliases: {keys:?})",
                );

                // The on-disk view: the block exists and is populated. An
                // alias that resolves to an empty table is the whole failure
                // shape this file guards.
                let block = toml_table(&self.raw, &format!("channels.{channel_type}.{alias}"))
                    .unwrap_or_else(|| {
                        panic!(
                            "{site} = {reference:?} but `[channels.{channel_type}.{alias}]` is missing from the persisted config"
                        )
                    });
                assert!(
                    !block.is_empty(),
                    "{site} = {reference:?} resolves to an EMPTY `[channels.{channel_type}.{alias}]` block — \
                     the agent is bound to a channel the runtime cannot start",
                );
            }
        }
    }

    /// (3) Every value the user typed at first run survives the round-trip
    /// under its canonical schema key.
    ///
    /// The Quickstart submission once carried a single `token` that was always
    /// written to `bot_token`, so any channel whose required field had a
    /// different name silently lost it while the block still looked
    /// configured. Secret fields are compared after decryption because the
    /// apply path encrypts them on the way in.
    fn assert_submitted_channel_fields_persisted(&self) {
        let store = SecretStore::new(self.install_root(), true);
        for choice in &self.submission.channels {
            // `Existing` entries carry no fields to check; scenarios that
            // exercise reuse assert against the pre-existing block instead.
            let SelectorChoice::Fresh(entry) = choice else {
                continue;
            };
            let secret_keys: HashSet<String> =
                quickstart::field_shape(FieldSection::Channel, &entry.channel_type)
                    .into_iter()
                    .filter(|field| field.is_secret)
                    .map(|field| field.key)
                    .collect();

            let prefix = format!("channels.{}.{}", entry.channel_type, entry.alias);
            let mut keys: Vec<&String> = entry.fields.keys().collect();
            keys.sort();
            for key in keys {
                let want = entry.fields[key].trim();
                let path = format!("{prefix}.{key}");
                let persisted = toml_scalar(&self.raw, &path).unwrap_or_else(|| {
                    panic!(
                        "first run submitted `{path}` = {want:?} but the persisted config has no \
                         scalar at that path — the value was dropped or stored under another key"
                    )
                });
                let persisted = if secret_keys.contains(key) {
                    store.decrypt(&persisted).unwrap_or_else(|err| {
                        panic!("`{path}` persisted as an undecryptable secret: {err:#}")
                    })
                } else {
                    persisted
                };
                assert_eq!(
                    persisted, want,
                    "`{path}` did not survive the first-run round-trip",
                );
            }

            // A freshly built channel is always materialized as enabled — an
            // agent bound to a disabled block is another way to look
            // configured while never connecting.
            let enabled = toml_scalar(&self.raw, &format!("{prefix}.enabled"));
            assert_eq!(
                enabled.as_deref(),
                Some("true"),
                "`{prefix}.enabled` must be persisted as true for a freshly built channel",
            );
        }
    }

    /// (4a) `zeroclaw agents list` through the real loader reports exactly
    /// these aliases, in order.
    ///
    /// Compared as a whole list rather than by substring: `contains("bot")` is
    /// also true of `bot_shadow`, so a near-miss alias would slip through and
    /// the assertion would stop distinguishing the config it claims to check.
    fn assert_loader_lists_agents(&self, expected: &[&str]) {
        let stdout = stdout_of(
            &run_zeroclaw(self.install_root(), &["agents", "list"]),
            "agents list",
        );
        let listed: Vec<&str> = stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        assert_eq!(
            listed, expected,
            "the real loader must report exactly these agent aliases; got:\n{stdout}"
        );
    }

    /// (4b) `zeroclaw config get <agent>.channels` through the real loader
    /// reports exactly these channel refs, in order.
    ///
    /// The CLI renders a `StringArray` prop as a TOML array literal inside the
    /// JSON envelope's `value` string. That literal is re-parsed and compared
    /// element by element, so the check is exact without being hostage to the
    /// renderer's spacing.
    fn assert_loader_reports_agent_channels(&self, agent: &str, expected: &[&str]) {
        let path = format!("agents.{agent}.channels");
        let stdout = stdout_of(
            &run_zeroclaw(self.install_root(), &["config", "get", &path, "--json"]),
            &format!("config get {path}"),
        );
        let envelope: serde_json::Value =
            serde_json::from_str(&stdout).expect("`config get --json` must emit a JSON envelope");
        assert_eq!(envelope["path"], serde_json::Value::String(path.clone()));

        let rendered = envelope["value"].as_str().unwrap_or_else(|| {
            panic!("`{path}` envelope must carry a string `value`; got {envelope}")
        });
        let parsed: toml::Value =
            toml::from_str(&format!("value = {rendered}")).unwrap_or_else(|err| {
                panic!("`{path}` rendered as {rendered:?}, not a TOML array: {err}")
            });
        let listed: Vec<&str> = parsed["value"]
            .as_array()
            .unwrap_or_else(|| panic!("`{path}` rendered as {rendered:?}, not an array"))
            .iter()
            .map(|item| {
                item.as_str()
                    .unwrap_or_else(|| panic!("`{path}` holds a non-string element: {item}"))
            })
            .collect();
        assert_eq!(
            listed, expected,
            "the real loader must report exactly these channel refs for `{agent}`; got {rendered}"
        );
    }
}

/// Read a dotted TOML path as a table.
fn toml_table<'a>(root: &'a toml::Value, path: &str) -> Option<&'a toml::Table> {
    let mut cursor = root;
    for segment in path.split('.') {
        cursor = cursor.as_table()?.get(segment)?;
    }
    cursor.as_table()
}

/// Read a dotted TOML path as a scalar rendered the way the config surfaces
/// render it (so an integer port compares equal to the `"9137"` that was typed).
fn toml_scalar(root: &toml::Value, path: &str) -> Option<String> {
    let mut cursor = root;
    for segment in path.split('.') {
        cursor = cursor.as_table()?.get(segment)?;
    }
    match cursor {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Integer(i) => Some(i.to_string()),
        toml::Value::Float(f) => Some(f.to_string()),
        toml::Value::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

/// A minimal, credential-shaped first-run submission: one remote provider, the
/// default presets, SQLite memory, one agent, and whatever channels the caller
/// adds. Mirrors what every Quickstart surface builds for a clean install.
fn submission(agent: &str, channels: Vec<SelectorChoice<ChannelQuickStart>>) -> BuilderSubmission {
    BuilderSubmission {
        model_provider: SelectorChoice::Fresh(ModelProviderChoice {
            provider_type: "anthropic".into(),
            alias: "anthropic".into(),
            model: "claude-sonnet-4-5".into(),
            fields: HashMap::from([("api_key".to_string(), "placeholder-api-key".to_string())]),
        }),
        risk_profile: SelectorChoice::Fresh("balanced".into()),
        runtime_profile: SelectorChoice::Fresh("balanced".into()),
        memory: SelectorChoice::Fresh(MemoryChoice::Sqlite),
        channels,
        peer_groups: vec![],
        agent: AgentIdentity {
            name: agent.into(),
            system_prompt: "You are helpful.".into(),
            personality_file: None,
            personality_files: vec![],
        },
    }
}

/// Run the real `zeroclaw` binary against a first-run install root.
///
/// Every `ZEROCLAW_*` variable inherited from the developer's shell is stripped
/// from the child before `ZEROCLAW_CONFIG_DIR` is set, so an operator env var
/// (the `ZEROCLAW_*` config-override grammar included) cannot leak into the
/// result. The parent's environment is only read, never mutated, so this stays
/// safe under parallel test execution — which is exactly why the loader is
/// exercised in a child process rather than in-process.
fn run_zeroclaw(install_root: &Path, args: &[&str]) -> std::process::Output {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_zeroclaw"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("ZEROCLAW_") {
            command.env_remove(&key);
        }
    }
    command
        .args(args)
        .env("ZEROCLAW_CONFIG_DIR", install_root)
        .output()
        .expect("failed to spawn the zeroclaw binary")
}

/// Assert the child exited cleanly and return its stdout.
fn stdout_of(output: &std::process::Output, what: &str) -> String {
    assert!(
        output.status.success(),
        "`zeroclaw {what}` exited with {:?} against a first-run config\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// One freshly built channel. `fields` keys must be schema-canonical and
/// advertised by `quickstart::field_shape`.
fn fresh_channel(
    channel_type: &str,
    alias: &str,
    fields: &[(&str, &str)],
) -> SelectorChoice<ChannelQuickStart> {
    SelectorChoice::Fresh(ChannelQuickStart {
        channel_type: channel_type.into(),
        alias: alias.into(),
        fields: fields
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
    })
}

// ═════════════════════════════════════════════════════════════════════════════
// Scenarios
// ═════════════════════════════════════════════════════════════════════════════

/// The single most common first run: one provider, one chat channel, one agent.
/// Proves the produced config is loadable and valid, and that the agent is
/// actually wired to the channel the user set up.
#[tokio::test]
async fn first_run_with_one_channel_produces_a_valid_loadable_config() {
    let run = FirstRun::quickstart(submission(
        "bot",
        vec![fresh_channel(
            "telegram",
            "ops",
            &[("bot_token", "111111:placeholder-bot-token")],
        )],
    ))
    .await;

    run.assert_config_validates();
    run.assert_agent_channel_aliases_resolve_to_populated_blocks();
    run.assert_submitted_channel_fields_persisted();

    let agent = run
        .reloaded()
        .agents
        .get("bot")
        .expect("the agent the user named must be persisted");
    assert_eq!(
        agent
            .channels
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec!["telegram.ops".to_string()],
        "the agent must be bound to exactly the channel the user configured",
    );
    assert_eq!(
        agent.model_provider.as_str(),
        "anthropic.anthropic",
        "the agent must be bound to the provider the user configured",
    );
}

/// **The core regression gate.** Three channel families with three different
/// required-field shapes in one submission: every alias the agent ends up bound
/// to must resolve to a populated block, and no alias may go missing.
#[tokio::test]
async fn first_run_agent_channel_aliases_all_resolve_to_populated_blocks() {
    let run = FirstRun::quickstart(submission(
        "bot",
        vec![
            fresh_channel(
                "telegram",
                "tg",
                &[("bot_token", "111111:placeholder-telegram-token")],
            ),
            fresh_channel(
                "discord",
                "dc",
                &[("bot_token", "placeholder-discord-token")],
            ),
            // Webhook's required shape is a port + a shared secret, not a bot
            // token — the family a bot-token-shaped write path silently
            // emptied.
            fresh_channel(
                "webhook",
                "hooks",
                &[("port", "9137"), ("secret", "placeholder-webhook-secret")],
            ),
        ],
    ))
    .await;

    run.assert_config_validates();
    run.assert_agent_channel_aliases_resolve_to_populated_blocks();
    run.assert_submitted_channel_fields_persisted();

    let agent = run.reloaded().agents.get("bot").expect("agent persisted");
    let mut bound: Vec<String> = agent.channels.iter().map(ToString::to_string).collect();
    bound.sort();
    assert_eq!(
        bound,
        vec![
            "discord.dc".to_string(),
            "telegram.tg".to_string(),
            "webhook.hooks".to_string(),
        ],
        "every channel the user configured must stay bound to the agent",
    );
}

/// **The dropped-field regression, at system level.** A non-default,
/// non-secret channel field the user typed must reach disk under its canonical
/// schema key and
/// survive the reload — not be replaced by the schema default, and not be
/// dropped in favour of a bot-token-shaped write.
#[tokio::test]
async fn first_run_non_default_channel_field_survives_the_round_trip() {
    const CHOSEN_PORT: u16 = 9137;
    assert_ne!(
        CHOSEN_PORT, DEFAULT_WEBHOOK_CHANNEL_PORT,
        "the scenario is only meaningful with a non-default port",
    );

    let run = FirstRun::quickstart(submission(
        "bot",
        vec![fresh_channel(
            "webhook",
            "hooks",
            &[
                ("port", &CHOSEN_PORT.to_string()),
                ("secret", "placeholder-webhook-secret"),
            ],
        )],
    ))
    .await;

    run.assert_config_validates();
    run.assert_agent_channel_aliases_resolve_to_populated_blocks();
    run.assert_submitted_channel_fields_persisted();

    let webhook = run
        .reloaded()
        .channels
        .webhook
        .get("hooks")
        .expect("the webhook block the agent points at must exist after reload");
    assert_eq!(
        webhook.port, CHOSEN_PORT,
        "the port the user typed must survive reload instead of reverting to the schema default",
    );
    assert!(
        webhook.enabled,
        "a channel built during first run must come back enabled",
    );

    let store = SecretStore::new(run.install_root(), true);
    let secret = webhook
        .secret
        .as_deref()
        .expect("the webhook secret the user typed must be persisted");
    assert_eq!(
        store.decrypt(secret).expect("secret must decrypt"),
        "placeholder-webhook-secret",
        "the webhook secret must survive the round-trip under its canonical key",
    );
}

/// A first run that configures no channel at all (a delegate-only or CLI-only
/// agent) is a legitimate outcome — it must validate, and it must not leave the
/// agent bound to a channel nobody configured.
#[tokio::test]
async fn first_run_without_channels_validates_and_binds_nothing() {
    let run = FirstRun::quickstart(submission("bot", vec![])).await;

    run.assert_config_validates();
    // Vacuously true here, but running it keeps the invariant wired to the
    // no-channel scenario too: zero bindings is fine, a dangling one is not.
    run.assert_agent_channel_aliases_resolve_to_populated_blocks();

    let agent = run.reloaded().agents.get("bot").expect("agent persisted");
    assert!(
        agent.channels.is_empty(),
        "no channel was configured, so the agent must not be bound to one; got {:?}",
        agent
            .channels
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
    );
}

/// **The real loader.** Everything above reloads in-process through
/// `migrate_to_current`, which is the parse but not the whole of
/// `Config::load_or_init()`. Here the actual `zeroclaw` binary is spawned
/// against the first-run install root, so the production loader runs in full —
/// directory resolution, filesystem migration checks, salvage bookkeeping,
/// runtime path stamping, secret-store wiring — and the surfaces a user reads
/// on their second launch must agree with what the first run wrote.
///
/// A config that only *parses* is not the bar: the binary has to exit clean and
/// report the channel and the binding.
#[tokio::test]
async fn first_run_config_loads_through_the_real_binary() {
    let run = FirstRun::quickstart(submission(
        "bot",
        vec![fresh_channel(
            "telegram",
            "ops",
            &[("bot_token", "111111:placeholder-bot-token")],
        )],
    ))
    .await;
    let root = run.install_root();

    // `channel list` is the surface that disagreed in the motivating failure:
    // it must mark Telegram configured, not just count something.
    let listing = stdout_of(&run_zeroclaw(root, &["channel", "list"]), "channel list");
    assert!(
        listing.contains("✅ Telegram"),
        "the real loader must see the channel the first run configured; got:\n{listing}"
    );

    // The agent alias survived the loader, and it is the only one.
    run.assert_loader_lists_agents(&["bot"]);

    // The binding itself, read back through the loader rather than off disk.
    run.assert_loader_reports_agent_channels("bot", &["telegram.ops"]);

    // The secret reached the loader's secret store as a populated value — the
    // in-process reload cannot prove this, because it never builds one.
    let token = stdout_of(
        &run_zeroclaw(
            root,
            &["config", "get", "channels.telegram.ops.bot_token", "--json"],
        ),
        "config get channels.telegram.ops.bot_token",
    );
    let token: serde_json::Value =
        serde_json::from_str(&token).expect("`config get --json` must emit a JSON envelope");
    assert_eq!(
        token["populated"],
        serde_json::Value::Bool(true),
        "the credential the user typed must read back as populated through the real loader; got {token}"
    );
}

/// The surfaces a user actually reads must agree with the config that was
/// written: `zeroclaw doctor` must see the channel as configured and must not
/// report it as credential-less. The motivating failure had these disagree —
/// doctor counted a channel while the channel runtime had nothing usable.
///
/// Ignored by default. `diagnose()` is the only public sync entry point and it
/// bundles host probing with the config view: it shells out to git/curl and
/// runs `<tool> --version` for every CLI tool on PATH through a
/// `Command::output()` call with no timeout, so a single wedged binary on the
/// host hangs the suite. The assertions below are the ones worth keeping the
/// moment a config-only doctor entry point exists; until then they are opt-in.
#[ignore = "needs a config-only doctor entry point; diagnose() probes host PATH unbounded"]
#[tokio::test]
async fn first_run_doctor_agrees_the_channel_is_configured() {
    let run = FirstRun::quickstart(submission(
        "bot",
        vec![fresh_channel(
            "telegram",
            "ops",
            &[("bot_token", "111111:placeholder-bot-token")],
        )],
    ))
    .await;

    let report = zeroclaw_runtime::doctor::diagnose(&run.reloaded_with_paths());
    let config_items: Vec<&str> = report
        .iter()
        .filter(|item| item.category == "config")
        .map(|item| item.message.as_str())
        .collect();

    assert!(
        config_items
            .iter()
            .any(|message| message.contains("at least one channel configured")),
        "doctor must see the channel the first run configured; config items: {config_items:?}",
    );
    assert!(
        !config_items
            .iter()
            .any(|message| message.contains("bot_token is unset")),
        "doctor must not report the freshly configured channel as credential-less; \
         config items: {config_items:?}",
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Guard checks — prove each harness assertion fires on its own failure mode
//
// The broken first-run config shipped because every surface agreed while being
// wrong. A green harness is only evidence if it goes red on the shape it claims
// to catch, so each check above is re-run here against a config corrupted into
// that shape and is required to fail.
// ═════════════════════════════════════════════════════════════════════════════

/// Run `check` and return the panic message it produced, or fail if it passed.
fn panic_message_from(check: impl FnOnce()) -> String {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(check));
    let payload = result.err().unwrap_or_else(|| {
        panic!("the harness check passed against a corrupted config — the check is decorative")
    });
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else {
        panic!("harness check panicked with a non-string payload")
    }
}

/// Reach a channel alias's table inside a parsed config document.
fn channel_block<'a>(
    doc: &'a mut toml::Table,
    channel_type: &str,
    alias: &str,
) -> &'a mut toml::Table {
    doc.get_mut("channels")
        .and_then(toml::Value::as_table_mut)
        .and_then(|channels| channels.get_mut(channel_type))
        .and_then(toml::Value::as_table_mut)
        .and_then(|family| family.get_mut(alias))
        .and_then(toml::Value::as_table_mut)
        .expect("the corrupted-shape fixture must have this channel block")
}

/// Guard for `assert_config_validates`: a dangling agent→channel binding must
/// be rejected.
#[tokio::test]
async fn guard_config_validation_rejects_a_dangling_channel_binding() {
    let run = FirstRun::quickstart(submission(
        "bot",
        vec![fresh_channel(
            "telegram",
            "ops",
            &[("bot_token", "111111:placeholder-bot-token")],
        )],
    ))
    .await
    .with_corrupted_disk_view(|doc| {
        let agent = doc
            .get_mut("agents")
            .and_then(toml::Value::as_table_mut)
            .and_then(|agents| agents.get_mut("bot"))
            .and_then(toml::Value::as_table_mut)
            .expect("agent block");
        agent.insert(
            "channels".into(),
            toml::Value::Array(vec![toml::Value::String("telegram.ghost".into())]),
        );
    });

    // Pinned to the specific dangling-reference rejection: a guard that accepts
    // any validation failure would stay green if validate() started failing for
    // an unrelated reason, and would stop proving anything about bindings.
    let message = panic_message_from(|| run.assert_config_validates());
    assert!(
        message.contains("Config::validate()"),
        "unexpected failure message: {message}"
    );
    assert!(
        message.contains("agents.bot.channels[0]") && message.contains("telegram.ghost"),
        "the failure must name the dangling binding, not some other validation error: {message}"
    );
    assert!(
        message.contains("is not configured"),
        "the failure must be the dangling-reference rejection: {message}"
    );
}

/// Guard for `assert_agent_channel_aliases_resolve_to_populated_blocks`, and
/// the sharpest statement of the failure it guards: the alias key still
/// resolves and `Config::validate()` is still happy, but the block behind it is
/// empty.
#[tokio::test]
async fn guard_alias_resolution_rejects_an_empty_channel_block() {
    let run = FirstRun::quickstart(submission(
        "bot",
        vec![fresh_channel(
            "telegram",
            "ops",
            &[("bot_token", "111111:placeholder-bot-token")],
        )],
    ))
    .await
    .with_corrupted_disk_view(|doc| {
        let block = channel_block(doc, "telegram", "ops");
        block.clear();
    });

    // The point of the check: validation still passes on this config.
    run.assert_config_validates();

    let message =
        panic_message_from(|| run.assert_agent_channel_aliases_resolve_to_populated_blocks());
    assert!(
        message.contains("EMPTY") && message.contains("channels.telegram.ops"),
        "unexpected failure message: {message}"
    );
}

/// Guard for the same check against a block that is gone entirely.
#[tokio::test]
async fn guard_alias_resolution_rejects_a_missing_channel_block() {
    let run = FirstRun::quickstart(submission(
        "bot",
        vec![fresh_channel(
            "telegram",
            "ops",
            &[("bot_token", "111111:placeholder-bot-token")],
        )],
    ))
    .await
    .with_corrupted_disk_view(|doc| {
        doc.get_mut("channels")
            .and_then(toml::Value::as_table_mut)
            .and_then(|channels| channels.get_mut("telegram"))
            .and_then(toml::Value::as_table_mut)
            .expect("telegram family")
            .remove("ops");
    });

    let message =
        panic_message_from(|| run.assert_agent_channel_aliases_resolve_to_populated_blocks());
    assert!(
        message.contains("channels.telegram") && message.contains("ops"),
        "unexpected failure message: {message}"
    );
}

/// Guard for [`first_run_config_loads_through_the_real_binary`]: the strings it
/// matches on must actually discriminate. Remove the channel from the persisted
/// config and the real binary has to say so — otherwise `✅ Telegram` is just a
/// substring that happens to be present whatever the config holds.
#[tokio::test]
async fn guard_real_binary_reports_a_channel_that_is_no_longer_configured() {
    let run = FirstRun::quickstart(submission(
        "bot",
        vec![fresh_channel(
            "telegram",
            "ops",
            &[("bot_token", "111111:placeholder-bot-token")],
        )],
    ))
    .await
    .with_corrupted_disk_view(|doc| {
        doc.get_mut("channels")
            .and_then(toml::Value::as_table_mut)
            .and_then(|channels| channels.get_mut("telegram"))
            .and_then(toml::Value::as_table_mut)
            .expect("telegram family")
            .remove("ops");
    });

    let listing = stdout_of(
        &run_zeroclaw(run.install_root(), &["channel", "list"]),
        "channel list",
    );
    assert!(
        listing.contains("❌ Telegram"),
        "the real binary must report Telegram unconfigured once the block is gone; got:\n{listing}"
    );
    assert!(
        !listing.contains("✅ Telegram"),
        "the marker the positive test matches on must not survive the block's removal; got:\n{listing}"
    );
}

/// Guard for the two exact-match loader assertions: a *near* match must fail
/// them.
///
/// Both corruptions here are names that contain the expected name as a prefix,
/// so a substring check would stay green on a config that binds the agent to a
/// different channel than the one the first run built. That is the whole point
/// of comparing lists rather than searching text.
#[tokio::test]
async fn guard_loader_assertions_reject_a_near_match_alias() {
    let run = FirstRun::quickstart(submission(
        "bot",
        vec![fresh_channel(
            "telegram",
            "ops",
            &[("bot_token", "111111:placeholder-bot-token")],
        )],
    ))
    .await
    .with_corrupted_disk_view(|doc| {
        let agents = doc
            .get_mut("agents")
            .and_then(toml::Value::as_table_mut)
            .expect("agents table");
        let mut agent = agents.remove("bot").expect("agent block");
        agent
            .as_table_mut()
            .expect("agent block is a table")
            .insert(
                "channels".into(),
                toml::Value::Array(vec![toml::Value::String("telegram.ops-shadow".into())]),
            );
        // `bot_shadow` contains `bot`; `telegram.ops-shadow` contains
        // `telegram.ops`. Neither is the config the first run wrote.
        agents.insert("bot_shadow".into(), agent);
    });

    let message = panic_message_from(|| run.assert_loader_lists_agents(&["bot"]));
    assert!(
        message.contains("bot_shadow"),
        "the agent-list assertion must reject a near-match alias: {message}"
    );

    let message = panic_message_from(|| {
        run.assert_loader_reports_agent_channels("bot_shadow", &["telegram.ops"])
    });
    assert!(
        message.contains("telegram.ops-shadow"),
        "the binding assertion must reject a near-match channel ref: {message}"
    );
}

/// Guard for `assert_submitted_channel_fields_persisted`: a submitted plain
/// field that never reached disk must be caught. This is the dropped-field
/// shape — the block is populated and validates, it just lost what the user
/// typed.
#[tokio::test]
async fn guard_field_persistence_rejects_a_dropped_plain_field() {
    let run = FirstRun::quickstart(submission(
        "bot",
        vec![fresh_channel(
            "webhook",
            "hooks",
            &[("port", "9137"), ("secret", "placeholder-webhook-secret")],
        )],
    ))
    .await
    .with_corrupted_disk_view(|doc| {
        channel_block(doc, "webhook", "hooks").remove("port");
    });

    // Everything else about this config still looks fine.
    run.assert_config_validates();
    run.assert_agent_channel_aliases_resolve_to_populated_blocks();

    let message = panic_message_from(|| run.assert_submitted_channel_fields_persisted());
    assert!(
        message.contains("channels.webhook.hooks.port"),
        "unexpected failure message: {message}"
    );
}

/// Guard for the same check against a secret field stored with the wrong value
/// — the decrypt-then-compare arm.
#[tokio::test]
async fn guard_field_persistence_rejects_a_corrupted_secret_field() {
    let run = FirstRun::quickstart(submission(
        "bot",
        vec![fresh_channel(
            "webhook",
            "hooks",
            &[("port", "9137"), ("secret", "placeholder-webhook-secret")],
        )],
    ))
    .await
    .with_corrupted_disk_view(|doc| {
        channel_block(doc, "webhook", "hooks").insert(
            "secret".into(),
            toml::Value::String("some-other-secret".into()),
        );
    });

    let message = panic_message_from(|| run.assert_submitted_channel_fields_persisted());
    assert!(
        message.contains("channels.webhook.hooks.secret"),
        "unexpected failure message: {message}"
    );
}

/// Guard for the doctor assertion: doctor must actually notice a channel that
/// is enabled with no credential, otherwise the "doctor agrees" test proves
/// nothing. Ignored for the same reason as the test it guards.
#[ignore = "needs a config-only doctor entry point; diagnose() probes host PATH unbounded"]
#[tokio::test]
async fn guard_doctor_reports_an_enabled_channel_with_no_credential() {
    let run = FirstRun::quickstart(submission(
        "bot",
        vec![fresh_channel(
            "telegram",
            "ops",
            &[("bot_token", "111111:placeholder-bot-token")],
        )],
    ))
    .await
    .with_corrupted_disk_view(|doc| {
        channel_block(doc, "telegram", "ops")
            .insert("bot_token".into(), toml::Value::String(String::new()));
    });

    let report = zeroclaw_runtime::doctor::diagnose(&run.reloaded_with_paths());
    let config_items: Vec<&str> = report
        .iter()
        .filter(|item| item.category == "config")
        .map(|item| item.message.as_str())
        .collect();
    assert!(
        config_items
            .iter()
            .any(|message| message.contains("bot_token is unset")),
        "doctor must flag an enabled channel with no credential; config items: {config_items:?}",
    );
}
