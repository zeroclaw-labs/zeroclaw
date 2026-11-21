//! Cross-crate proof that a channel plugin's outbound `wasi:http` reaches the
//! network only when the operator grants its destination.
//!
//! This is the channel analogue of `egress_plugin_e2e.rs` (which proves the
//! same boundary for tool plugins), run through the whole activation path an
//! operator exercises: a `[channels.plugin.<alias>]` declaration plus an
//! installed package plus a `[[plugins.entries]]` egress grant, in; a live
//! `Channel` backed by a compiled component that issued one real GET, out.
//!
//! Nothing is stubbed. The allow path opens a TCP connection to a listener in
//! this process; the deny path is the same host code the shipped daemon runs.
//! The fixture always constructs — only whether the packet leaves the sandbox,
//! and what the guest observed, changes with the grant. That is what proves the
//! host-owned egress policy gates *reach*, not construction.
//!
//! The first three tests build the declaration and the grant by hand. The next
//! two let the shipped `zeroclaw` binary write them into an isolated config
//! directory: `plugin install --channel-alias` binds the alias and creates the
//! instance's row, `config set` supplies what the binding ceremony leaves to
//! the operator, and the channel is constructed from the file the binary wrote.
//! Only an owning agent is added in memory, since binding an agent is
//! deliberately outside the ceremony. So the row the ceremony writes is proved
//! to be the row the runtime reads, and the command it prints is proved to be
//! the one that opens the destination. The two unix-only tests after them
//! drive only the binary and construct no channel: the command printed for a
//! package that declares no destination is refused and writes nothing when
//! run as printed, and grants exactly the hosts appended to it. The last test
//! drives only the binary and constructs no channel either: a refused install
//! writes and publishes nothing, and a retry under a free alias binds it.

#![cfg(feature = "plugins-wasm-cranelift")]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use tempfile::TempDir;
use zeroclaw_config::providers::{ChannelRef, ModelProviderRef};
use zeroclaw_config::schema::{
    AliasedAgentConfig, AnthropicModelProviderConfig, Config, PluginChannelConfig,
    PluginEntryConfig, RiskProfileConfig,
};
use zeroclaw_config::secrets::SecretStore;
use zeroclaw_plugins::PluginCapability;
use zeroclaw_plugins::host::PluginHost;
use zeroclaw_plugins::instance::PluginInstanceScope;

const MANIFEST: &str =
    "crates/zeroclaw-plugins/tests/fixtures/channel-egress-fixture/plugin-manifest.toml";

/// The destination the ceremony-driven tests declare, grant and reach: the
/// loopback address [`TestServer`] listens on.
const DECLARED_HOST: &str = "127.0.0.1";

// ── fixture provisioning ──────────────────────────────────────────

/// Build the channel egress component once per test binary.
///
/// The fixture is a workspace member built into its own target directory so the
/// nested Cargo invocation cannot contend with this test process's build lock.
/// A missing wasm target is a failure, never a skip — a security boundary that
/// silently stops being tested is worse than one that is loudly broken.
fn fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("crates/zeroclaw-plugins/tests/fixtures/channel-egress-fixture");
            let target_dir =
                PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("channel-egress-fixture");
            let status = Command::new(env!("CARGO"))
                .current_dir(&fixture_dir)
                .args([
                    "build",
                    "--locked",
                    "--quiet",
                    "--package",
                    "zeroclaw-channel-egress-plugin-fixture",
                    "--target",
                    "wasm32-wasip2",
                    "--target-dir",
                ])
                .arg(&target_dir)
                .status()
                .expect("run Cargo for the channel egress component fixture");
            assert!(
                status.success(),
                "channel egress fixture must build; install the wasm32-wasip2 target"
            );

            let wasm =
                target_dir.join("wasm32-wasip2/debug/zeroclaw_channel_egress_plugin_fixture.wasm");
            assert!(
                wasm.is_file(),
                "channel egress fixture WASM was not produced"
            );
            wasm
        })
        .clone()
}

/// Install the fixture as a real plugin package: the canonical manifest copied
/// verbatim, next to the component it names.
fn install_fixture_package() -> TempDir {
    let plugins = TempDir::new().expect("create plugin package root");
    let package = plugins.path().join("channel-egress-fixture");
    std::fs::create_dir_all(&package).expect("create plugin package");
    std::fs::copy(fixture(), package.join("channel-egress-fixture.wasm"))
        .expect("copy channel egress component fixture");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MANIFEST),
        package.join("manifest.toml"),
    )
    .expect("install the canonical fixture manifest");
    plugins
}

/// A plugin source directory as a publisher ships one: the built component
/// next to the canonical fixture manifest with `[egress] hosts = ["127.0.0.1"]`
/// appended, so the install ceremony has a declared destination to grant or
/// withhold. Appending changes the manifest bytes, which the host admits
/// because plugin signature checking is `disabled` by default. The instance key
/// is unaffected: it derives from the package name, capability and alias.
fn fixture_source_with_declaration() -> TempDir {
    let source = TempDir::new().expect("create plugin source directory");
    std::fs::copy(fixture(), source.path().join("channel-egress-fixture.wasm"))
        .expect("copy channel egress component fixture");
    let manifest =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MANIFEST))
            .expect("read the canonical fixture manifest");
    std::fs::write(
        source.path().join("manifest.toml"),
        format!("{manifest}\n[egress]\nhosts = [\"{DECLARED_HOST}\"]\n"),
    )
    .expect("write the declaring fixture manifest");
    source
}

/// A plugin source directory with the canonical fixture manifest as is: it
/// holds `http_client` but declares no destination, so the install ceremony
/// creates the instance's row with an empty grant and prints the command that
/// grants the hosts the deployment uses.
#[cfg(unix)]
fn fixture_source_without_declaration() -> TempDir {
    let source = TempDir::new().expect("create plugin source directory");
    std::fs::copy(fixture(), source.path().join("channel-egress-fixture.wasm"))
        .expect("copy channel egress component fixture");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MANIFEST),
        source.path().join("manifest.toml"),
    )
    .expect("copy the canonical fixture manifest");
    source
}

// ── operator config ───────────────────────────────────────────────

/// The instance key of channel `alias` of the fixture package installed in
/// `plugins_dir`.
///
/// The channel's admitted scope is `(package, Channel, alias)`; the egress
/// resolver and the config loader both key that instance by its
/// `config_entry_key()`. Deriving the key here from the same manifest the
/// loader admits is what proves a row with this name is the one a channel
/// store resolves at request time.
fn expected_channel_key(plugins_dir: &Path, alias: &str) -> String {
    let host = PluginHost::from_plugins_dir(plugins_dir).expect("admit fixture package");
    let manifest = host
        .manifest("channel-egress-fixture")
        .expect("fixture manifest is admitted");
    PluginInstanceScope::from_manifest(
        manifest,
        PluginCapability::Channel,
        alias,
        manifest.permissions.iter().copied(),
    )
    .expect("admit configured logical channel")
    .id()
    .config_entry_key()
    .expect("derive canonical fixture config key")
}

/// The instance-key `[[plugins.entries]]` row for the configured channel,
/// named by [`expected_channel_key`], so this row's grant is exactly the one a
/// channel store resolves at request time.
fn entry_row(
    plugins: &TempDir,
    alias: &str,
    url: &str,
    egress_hosts: &[&str],
    egress_allow_private: &[&str],
) -> PluginEntryConfig {
    PluginEntryConfig {
        name: expected_channel_key(plugins.path(), alias),
        config: HashMap::from([("url".to_string(), url.to_string())]),
        egress_hosts: egress_hosts.iter().map(|h| (*h).to_string()).collect(),
        egress_allow_private: egress_allow_private
            .iter()
            .map(|h| (*h).to_string())
            .collect(),
        tls_profiles: Vec::new(),
    }
}

/// Give `plugin.<alias>` one enabled owning agent, with the risk profile and
/// model provider that agent needs to pass `Config::validate`. The binding
/// ceremony never writes agent ownership, so every activation config here gets
/// it from this one place, whether its binding and row were built by hand or
/// written by the binary.
fn add_owning_agent(config: &mut Config, alias: &str) {
    config
        .risk_profiles
        .insert("default".to_string(), RiskProfileConfig::default());
    config.providers.models.anthropic.insert(
        "default".to_string(),
        AnthropicModelProviderConfig::default(),
    );
    config.agents.insert(
        "operator".to_string(),
        AliasedAgentConfig {
            channels: vec![ChannelRef::new(format!("plugin.{alias}"))],
            model_provider: ModelProviderRef::new("anthropic.default"),
            risk_profile: "default".into(),
            ..AliasedAgentConfig::default()
        },
    );
}

/// A realistic operator config: one enabled agent routing to one configured
/// channel plugin, granted the destinations in its instance-key row. Passes
/// `Config::validate`, so this is the config an operator would actually write.
fn activation_config(plugins: &TempDir, alias: &str, entry: PluginEntryConfig) -> Config {
    let mut config = Config::default();
    config.plugins.enabled = true;
    config.plugins.auto_discover = false;
    config.plugins.max_active_instances = 1;
    config.plugins.plugins_dir = plugins.path().display().to_string();
    // Every field is named today; the base keeps this literal compiling when
    // `PluginChannelConfig` gains one.
    #[allow(clippy::needless_update)]
    config.channels.plugin.insert(
        alias.to_string(),
        PluginChannelConfig {
            package: "channel-egress-fixture".to_string(),
            enabled: true,
            ..PluginChannelConfig::default()
        },
    );
    add_owning_agent(&mut config, alias);
    config.plugins.entries.push(entry);
    config
}

/// The config the daemon would load from `config_dir`, plus an agent that owns
/// `plugin.<alias>`.
///
/// `Config::load_or_init` is the only loader, and it finds its directory
/// through `ZEROCLAW_CONFIG_DIR`, which this process cannot set without racing
/// the tests that run beside it. So this runs the two steps of that loader that
/// decide what the runtime reads from the row: the daemon's resilient parse,
/// which must drop nothing, and secret decryption with the config directory's
/// own key, the key the binary encrypted `config.url` with. The binding and the
/// row reach the runtime exactly as the binary wrote them.
fn load_activation_config(config_dir: &Path, alias: &str) -> Config {
    let path = config_dir.join("config.toml");
    let raw = std::fs::read_to_string(&path).expect("read the config the binary wrote");
    let load = zeroclaw_config::migration::migrate_to_current_salvaged(&raw);
    assert!(
        load.dropped.is_empty() && load.dropped_security.is_empty(),
        "the config the binary wrote must load whole; dropped {:?} and {:?}:\n{raw}",
        load.dropped,
        load.dropped_security
    );
    let mut config = load.config;
    config.config_path = path;
    config.data_dir = config_dir.join("data");
    let store = SecretStore::new(config_dir, config.secrets.encrypt);
    config
        .decrypt_secrets(&store)
        .expect("decrypt the row's config with the config directory's key");
    add_owning_agent(&mut config, alias);
    config
        .validate()
        .expect("the written binding and row, plus an owning agent, are valid operator config");
    config
}

// ── local test server ─────────────────────────────────────────────

/// A minimal HTTP/1.1 responder. Deliberately raw `std::net` rather than a
/// server framework: this file proves what the host's client does, so the
/// server adds no machinery of its own. `hits` counts every accepted connection
/// — the load-bearing signal for "the packet actually left the sandbox".
struct TestServer {
    port: u16,
    hits: Arc<AtomicUsize>,
}

impl TestServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback test server");
        let port = listener.local_addr().expect("local addr").port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();

        std::thread::spawn(move || {
            const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0_u8; 2048];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(OK.as_bytes());
                let _ = stream.flush();
            }
        });

        Self { port, hits }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

/// Construct the single configured channel and return its guest-observed egress
/// outcome (recorded in `configure`, surfaced through the cached `self-handle`).
async fn construct_and_probe(config: Config) -> (usize, Option<String>) {
    // fixture()/build happen through activation; validate first so a broken
    // operator config fails as config, not as a mystery empty channel list.
    config
        .validate()
        .expect("the activation declaration is valid operator config");
    let channels =
        zeroclaw_runtime::plugin_runtime::configured_plugin_channels(Arc::new(config), None).await;
    assert_eq!(
        channels.len(),
        1,
        "the configured channel must construct regardless of the egress verdict"
    );
    let outcome = channels[0].self_handle();
    (channels.len(), outcome)
}

// ── the shipped binary ────────────────────────────────────────────

/// An isolated config directory whose `config.toml` enables the plugin system
/// and keeps installed packages inside the directory. `plugins.enabled` is a
/// security posture decision the binding ceremony never makes, so the test
/// makes it here, before any command runs. `locale = "en"` selects the English
/// catalogue on every platform, where `LANG` alone does not (macOS and Windows
/// report the OS locale), so the one prose line parsed below is stable. The
/// file is at the current schema version, which the ceremony requires before
/// it writes.
fn config_dir_with_plugins_dir() -> TempDir {
    let config_dir = TempDir::new().expect("create isolated config directory");
    let plugins_dir = plugins_dir_of(config_dir.path());
    let plugins_dir = plugins_dir.to_str().expect("utf-8 temp path");
    assert!(
        !plugins_dir.contains('\''),
        "a TOML literal string cannot carry a single quote: {plugins_dir}"
    );
    std::fs::write(
        config_dir.path().join("config.toml"),
        format!(
            "schema_version = {}\nlocale = \"en\"\n\n[plugins]\nenabled = true\n\
             auto_discover = false\nmax_active_instances = 1\nplugins_dir = '{plugins_dir}'\n",
            zeroclaw_config::migration::CURRENT_SCHEMA_VERSION
        ),
    )
    .expect("write config.toml");
    config_dir
}

/// Where [`config_dir_with_plugins_dir`] points `plugins_dir`.
fn plugins_dir_of(config_dir: &Path) -> PathBuf {
    config_dir.join("plugins")
}

/// Run the real `zeroclaw` binary against an isolated config directory, as the
/// component CLI tests do.
fn run_zeroclaw(config_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("RUST_LOG", "off")
        .env("LANG", "en_US.UTF-8")
        .args(args)
        .output()
        .expect("run zeroclaw")
}

/// `zeroclaw plugin install <source> --channel-alias <alias> --egress <egress>`.
fn install_with_alias(config_dir: &Path, source: &TempDir, alias: &str, egress: &str) -> Output {
    let source = source.path().to_str().expect("utf-8 temp path");
    run_zeroclaw(
        config_dir,
        &[
            "plugin",
            "install",
            source,
            "--channel-alias",
            alias,
            "--egress",
            egress,
        ],
    )
}

/// `zeroclaw config set --no-interactive <path> <value>`, which must succeed.
/// Every `config.*` value of a row is a secret, which `config set` otherwise
/// reads from a masked prompt; `--no-interactive` takes the value as given,
/// and for a plain value such as `egress_allow_private` it changes nothing.
fn config_set(config_dir: &Path, path: &str, value: &str) {
    let out = run_zeroclaw(
        config_dir,
        &["config", "set", "--no-interactive", path, value],
    );
    assert!(
        out.status.success(),
        "config set {path} must succeed: {}",
        combined(&out)
    );
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The instance key named by the line install prints for a row it creates:
/// `Seeded [[plugins.entries]] for '<key>'. Set plugin config values ...`.
fn seeded_key(stdout: &str) -> Option<&str> {
    stdout.lines().find_map(|line| {
        line.strip_prefix("Seeded [[plugins.entries]] for '")?
            .split_once('\'')
            .map(|(key, _)| key)
    })
}

/// Every printed command in `stdout` that sets the `egress_hosts` of the row
/// named `key`. A printed operator command starts with the
/// `zeroclaw --config-dir <dir>` invocation and ends its line, so each one is
/// the tail of its line from that invocation on.
fn grant_commands<'a>(stdout: &'a str, key: &str) -> Vec<&'a str> {
    let grant = format!(" config set plugins.entries.{key}.egress_hosts ");
    stdout
        .lines()
        .filter_map(|line| {
            line.find("zeroclaw --config-dir ")
                .map(|start| &line[start..])
        })
        .filter(|command| command.contains(&grant))
        .collect()
}

/// The printed command that grants the hosts the deployment uses to the row
/// named `key` of a package that declares none: `config set --no-interactive`
/// for the row's `egress_hosts`, with no value, at the end of its line.
#[cfg(unix)]
fn undeclared_hosts_command<'a>(stdout: &'a str, key: &str) -> Option<&'a str> {
    let path = format!(" config set --no-interactive 'plugins.entries.{key}.egress_hosts'");
    stdout.lines().find_map(|line| {
        let command = &line[line.find("zeroclaw --config-dir ")?..];
        command.ends_with(&path).then_some(command)
    })
}

/// Run `command` as an operator pastes it: through a POSIX `sh` that finds
/// `zeroclaw` on `PATH`, with this build's directory first, and with
/// `ZEROCLAW_CONFIG_DIR` removed, so only the printed `--config-dir` selects
/// the configuration. There is no terminal: standard input is closed, and
/// `EDITOR=false` is an editor that exits without saving.
#[cfg(unix)]
fn run_as_pasted(command: &str) -> Output {
    let binary_dir = Path::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .parent()
        .expect("the binary's directory");
    let path = std::env::join_paths(std::iter::once(binary_dir.to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("join PATH");
    Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("PATH", path)
        .env_remove("ZEROCLAW_CONFIG_DIR")
        .env("RUST_LOG", "off")
        .env("LANG", "en_US.UTF-8")
        .env("EDITOR", "false")
        .env_remove("VISUAL")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run sh")
}

/// Install the fixture without a declaration as `plugin.operations` in a
/// fresh configuration. Returns that configuration's directory, the
/// instance's key, and the printed command that grants it the deployment's
/// hosts.
#[cfg(unix)]
fn install_without_declaration() -> (TempDir, String, String) {
    let source = fixture_source_without_declaration();
    let config_dir = config_dir_with_plugins_dir();
    let dir = config_dir.path();
    let install = run_zeroclaw(
        dir,
        &[
            "plugin",
            "install",
            source.path().to_str().expect("utf-8 temp path"),
            "--channel-alias",
            "operations",
        ],
    );
    let text = combined(&install);
    assert!(
        install.status.success(),
        "an undeclaring package must install and bind: {text}"
    );
    let key = expected_channel_key(&plugins_dir_of(dir), "operations");
    let stdout = String::from_utf8_lossy(&install.stdout);
    let command = undeclared_hosts_command(&stdout, &key)
        .unwrap_or_else(|| panic!("the command that grants the deployment's hosts: {text}"))
        .to_string();
    (config_dir, key, command)
}

/// `config.toml` as the binary left it.
fn config_on_disk(config_dir: &Path) -> toml::Table {
    let raw = std::fs::read_to_string(config_dir.join("config.toml")).expect("read config.toml");
    toml::from_str(&raw).unwrap_or_else(|e| panic!("config.toml must be TOML ({e}):\n{raw}"))
}

/// The package `[channels.plugin.<alias>]` names on disk.
fn bound_package<'a>(on_disk: &'a toml::Table, alias: &str) -> Option<&'a str> {
    on_disk
        .get("channels")?
        .get("plugin")?
        .get(alias)?
        .get("package")?
        .as_str()
}

/// The `[[plugins.entries]]` rows on disk. One save writes the rows it creates
/// in hash order, so callers look rows up by name, never by position.
fn entries_on_disk(on_disk: &toml::Table) -> Vec<&toml::Table> {
    on_disk
        .get("plugins")
        .and_then(|plugins| plugins.get("entries"))
        .and_then(toml::Value::as_array)
        .map(|rows| rows.iter().filter_map(toml::Value::as_table).collect())
        .unwrap_or_default()
}

/// The `[[plugins.entries]]` row named `key` on disk.
fn entry_on_disk<'a>(on_disk: &'a toml::Table, key: &str) -> Option<&'a toml::Table> {
    entries_on_disk(on_disk)
        .into_iter()
        .find(|row| row.get("name").and_then(toml::Value::as_str) == Some(key))
}

/// A row's string list. An empty list is not written, so an absent key and an
/// empty list are the same grant.
fn string_list(row: &toml::Table, field: &str) -> Vec<String> {
    row.get(field)
        .and_then(toml::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The names in `plugins_dir`, including dot-prefixed staging directories; an
/// absent directory holds nothing.
fn plugins_dir_names(plugins_dir: &Path) -> Vec<String> {
    match std::fs::read_dir(plugins_dir) {
        Ok(entries) => entries
            .map(|entry| {
                entry
                    .expect("read plugins dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("read {}: {e}", plugins_dir.display()),
    }
}

// ── the proof ─────────────────────────────────────────────────────

/// Granted: the operator lists the destination (and opens its private address
/// class), so the guest's GET reaches the listener. The hit count is the
/// load-bearing half — it proves a real packet arrived, not merely that the
/// guest saw no error.
#[tokio::test]
async fn a_granted_destination_reaches_the_server() {
    let plugins = install_fixture_package();
    let server = TestServer::start();
    let entry = entry_row(
        &plugins,
        "operations",
        &server.url("/hook"),
        &["127.0.0.1"],
        &["127.0.0.1"],
    );
    let config = activation_config(&plugins, "operations", entry);

    let (_, outcome) = construct_and_probe(config).await;

    assert_eq!(
        outcome.as_deref(),
        Some("egress:status=200"),
        "a granted channel must reach the server; got: {outcome:?}"
    );
    assert_eq!(
        server.hits(),
        1,
        "the granted request must reach the socket exactly once; got: {outcome:?}"
    );
}

/// Ungranted: `http_client` grants the `wasi:http` surface, but with no
/// `egress_hosts` the channel reaches nothing. The denial surfaces to the guest
/// (its outcome names the policy) and the listener is never touched — refused
/// before any packet left, which is the property that shuts the self-grant path.
#[tokio::test]
async fn an_ungranted_destination_is_denied_before_the_network() {
    let plugins = install_fixture_package();
    let server = TestServer::start();
    let entry = entry_row(&plugins, "operations", &server.url("/hook"), &[], &[]);
    let config = activation_config(&plugins, "operations", entry);

    let (_, outcome) = construct_and_probe(config).await;

    let outcome = outcome.expect("the guest must report an egress outcome");
    assert!(
        outcome.contains("error=") && !outcome.contains("status=200"),
        "an ungranted channel must be denied; got: {outcome}"
    );
    assert!(
        outcome.contains("egress policy"),
        "the guest's denial must name the policy; got: {outcome}"
    );
    assert_eq!(
        server.hits(),
        0,
        "a denied request must never reach the socket; got: {outcome}"
    );
}

/// Gated by host, not on/off: the operator grants a *different* host than the
/// server runs on, so the same store — surface granted, a policy present —
/// still cannot reach the listener. This distinguishes "policy says no" from
/// "no policy", and proves the allowlist match is by destination.
#[tokio::test]
async fn a_grant_for_a_different_host_still_denies_the_server() {
    let plugins = install_fixture_package();
    let server = TestServer::start();
    let entry = entry_row(
        &plugins,
        "operations",
        &server.url("/hook"),
        &["api.example.com"],
        &["api.example.com"],
    );
    let config = activation_config(&plugins, "operations", entry);

    let (_, outcome) = construct_and_probe(config).await;

    let outcome = outcome.expect("the guest must report an egress outcome");
    assert!(
        outcome.contains("error=") && !outcome.contains("status=200"),
        "a grant for a different host must not reach this server; got: {outcome}"
    );
    assert_eq!(
        server.hits(),
        0,
        "the policy must gate by host: a different grant reaches nothing; got: {outcome}"
    );
}

// ── the proof, through the binding ceremony ───────────────────────

/// Granted by the ceremony: `plugin install --channel-alias operations
/// --egress declared` binds the alias and seeds the instance's row with the
/// manifest's declaration, and the operator adds only what the ceremony leaves
/// to them, the URL and the private-address carve-out loopback needs. Built
/// from the file the binary wrote, the guest's GET reaches the listener exactly
/// once. The instance-key assertion is the load-bearing half: the row the
/// install names is the row the activation plan derives, so the hit proves the
/// ceremony's grant, not a hand-built one, let the packet out.
#[tokio::test]
async fn a_grant_seeded_by_the_install_ceremony_reaches_the_server() {
    let source = fixture_source_with_declaration();
    let config_dir = config_dir_with_plugins_dir();
    let dir = config_dir.path();
    let server = TestServer::start();

    let install = install_with_alias(dir, &source, "operations", "declared");
    let text = combined(&install);
    assert!(
        install.status.success(),
        "the install ceremony must succeed: {text}"
    );
    let key = expected_channel_key(&plugins_dir_of(dir), "operations");
    let stdout = String::from_utf8_lossy(&install.stdout);
    assert_eq!(
        seeded_key(&stdout),
        Some(key.as_str()),
        "install must name the row the activation plan derives: {text}"
    );
    let grants = grant_commands(&stdout, &key);
    assert!(
        !grants.is_empty() && grants.iter().all(|command| command.contains(DECLARED_HOST)),
        "install must print the grant it made, as the command that edits it: {text}"
    );

    let on_disk = config_on_disk(dir);
    assert_eq!(
        bound_package(&on_disk, "operations"),
        Some("channel-egress-fixture"),
        "install must bind the alias to the package: {on_disk}"
    );
    let row = entry_on_disk(&on_disk, &key)
        .unwrap_or_else(|| panic!("install must create the row '{key}': {on_disk}"));
    assert_eq!(
        string_list(row, "egress_hosts"),
        [DECLARED_HOST],
        "--egress declared must grant exactly the declaration: {on_disk}"
    );
    assert!(
        row.get("egress_allow_private").is_none(),
        "the ceremony must never seed a private-address carve-out: {on_disk}"
    );

    config_set(
        dir,
        &format!("plugins.entries.{key}.config.url"),
        &server.url("/hook"),
    );
    config_set(
        dir,
        &format!("plugins.entries.{key}.egress_allow_private"),
        DECLARED_HOST,
    );
    let (_, outcome) = construct_and_probe(load_activation_config(dir, "operations")).await;

    assert_eq!(
        outcome.as_deref(),
        Some("egress:status=200"),
        "a channel granted by the ceremony must reach the server; got: {outcome:?}"
    );
    assert_eq!(
        server.hits(),
        1,
        "the ceremony's grant must let the request reach the socket exactly once; got: {outcome:?}"
    );
}

/// Declined, then granted by the printed command: `--egress none` binds the
/// alias and creates the row with no destination, and the guest is refused
/// before the network. `plugin list` names the gap and the command that closes
/// it; that command, run exactly as printed through a real `sh`, writes the
/// grant into this configuration, and the next construction reaches the
/// listener once. The hit count going from zero to one is the load-bearing
/// half: between the two constructions only the printed grant and the
/// operator's private-address carve-out changed.
///
/// Unix only: the command is rendered for the host's shell, and this runs it
/// through POSIX `sh`.
#[cfg(unix)]
#[tokio::test]
async fn a_declined_grant_denies_egress_until_the_printed_command_is_applied() {
    let source = fixture_source_with_declaration();
    let config_dir = config_dir_with_plugins_dir();
    let dir = config_dir.path();
    let server = TestServer::start();

    let install = install_with_alias(dir, &source, "operations", "none");
    let text = combined(&install);
    assert!(
        install.status.success(),
        "a declined grant must still install and bind: {text}"
    );
    let key = expected_channel_key(&plugins_dir_of(dir), "operations");
    let stdout = String::from_utf8_lossy(&install.stdout);
    assert_eq!(
        seeded_key(&stdout),
        Some(key.as_str()),
        "a declined grant must still create the instance's row: {text}"
    );
    let withheld = grant_commands(&stdout, &key);
    assert!(
        !withheld.is_empty()
            && withheld
                .iter()
                .all(|command| command.contains(DECLARED_HOST)),
        "a declined grant must name the command that grants the declaration later: {text}"
    );
    let on_disk = config_on_disk(dir);
    let row = entry_on_disk(&on_disk, &key)
        .unwrap_or_else(|| panic!("a declined grant must still create the row '{key}': {on_disk}"));
    assert!(
        string_list(row, "egress_hosts").is_empty(),
        "--egress none must grant nothing: {on_disk}"
    );

    config_set(
        dir,
        &format!("plugins.entries.{key}.config.url"),
        &server.url("/hook"),
    );
    let (_, outcome) = construct_and_probe(load_activation_config(dir, "operations")).await;
    let outcome = outcome.expect("the guest must report an egress outcome");
    assert!(
        outcome.contains("error=") && !outcome.contains("status=200"),
        "a declined grant must deny the channel; got: {outcome}"
    );
    assert!(
        outcome.contains("egress policy"),
        "the guest's denial must name the policy; got: {outcome}"
    );
    assert_eq!(
        server.hits(),
        0,
        "a declined grant must never reach the socket; got: {outcome}"
    );

    let list = run_zeroclaw(dir, &["plugin", "list"]);
    let list_text = combined(&list);
    assert!(
        list.status.success(),
        "plugin list must exit 0: {list_text}"
    );
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    let gap = grant_commands(&list_stdout, &key);
    assert_eq!(
        gap.len(),
        1,
        "plugin list must print one grant command for the instance: {list_text}"
    );
    let command = gap[0];
    assert!(
        list_stdout
            .lines()
            .any(|line| line.contains("(plugin.operations)") && line.ends_with(command)),
        "the grant command must be on the instance's gap line: {list_text}"
    );
    assert!(
        command.contains(DECLARED_HOST),
        "the gap line's command must grant the declared host: {command}"
    );
    let dir_text = dir.to_str().expect("utf-8 temp path");
    assert!(
        command.starts_with(&format!("zeroclaw --config-dir '{dir_text}' ")),
        "the printed command must select this configuration: {command}"
    );

    // Run it as an operator would, pasted into a POSIX shell.
    let applied = run_as_pasted(command);
    assert!(
        applied.status.success(),
        "the printed command must run as printed: {command}\n{}",
        combined(&applied)
    );
    let on_disk = config_on_disk(dir);
    let row = entry_on_disk(&on_disk, &key)
        .unwrap_or_else(|| panic!("the row '{key}' must still exist: {on_disk}"));
    assert_eq!(
        string_list(row, "egress_hosts"),
        [DECLARED_HOST],
        "the printed command must write the grant into this configuration: {on_disk}"
    );

    config_set(
        dir,
        &format!("plugins.entries.{key}.egress_allow_private"),
        DECLARED_HOST,
    );
    let (_, outcome) = construct_and_probe(load_activation_config(dir, "operations")).await;

    assert_eq!(
        outcome.as_deref(),
        Some("egress:status=200"),
        "once the printed grant is applied the channel must reach the server; got: {outcome:?}"
    );
    assert_eq!(
        server.hits(),
        1,
        "only the granted construction may reach the socket, once; got: {outcome:?}"
    );
}

/// A package that holds a governed transport but declares no destination
/// gets a printed `config set --no-interactive` command for its row's
/// `egress_hosts` with no value, so the command as printed carries no
/// placeholder to write. Pasted as printed, with no terminal and an editor
/// that saves nothing, it is refused for the missing value and leaves
/// `config.toml` byte-identical.
#[cfg(unix)]
#[test]
fn the_command_for_undeclared_hosts_writes_nothing_as_printed() {
    let (config_dir, _key, command) = install_without_declaration();
    let config_file = config_dir.path().join("config.toml");
    let before = std::fs::read(&config_file).expect("read config.toml");

    let ran = run_as_pasted(&command);

    let text = combined(&ran);
    assert!(
        !ran.status.success() && text.contains("Value required in --no-interactive mode"),
        "the command run as printed must be refused for the missing value: {command}\n{text}"
    );
    assert_eq!(
        std::fs::read(&config_file).expect("read config.toml"),
        before,
        "the command run as printed must write nothing: {command}\n{text}"
    );
}

/// The same printed command with the deployment's hosts appended as one
/// comma-separated value grants exactly those hosts on the instance's row.
#[cfg(unix)]
#[test]
fn the_command_for_undeclared_hosts_grants_the_hosts_appended_to_it() {
    let (config_dir, key, command) = install_without_declaration();

    let completed = format!("{command} '{DECLARED_HOST},irc.example.net'");
    let ran = run_as_pasted(&completed);

    assert!(
        ran.status.success(),
        "the completed command must run: {completed}\n{}",
        combined(&ran)
    );
    let on_disk = config_on_disk(config_dir.path());
    let row = entry_on_disk(&on_disk, &key)
        .unwrap_or_else(|| panic!("the row '{key}' must exist: {on_disk}"));
    assert_eq!(
        string_list(row, "egress_hosts"),
        [DECLARED_HOST, "irc.example.net"],
        "the appended hosts must be the grant: {on_disk}"
    );
}

/// Refused before publishing: an alias bound to another package is never
/// taken over. The install exits non-zero and names the owner, publishes
/// nothing (no package directory, no staging directory), and leaves
/// `config.toml` byte-identical, because the binding plan is checked before the
/// load check and before any write. A retry under a free alias is then an
/// ordinary fresh install: it binds and creates that alias's row only, and the
/// other package keeps its binding.
#[test]
fn install_refuses_an_alias_owned_by_another_package_before_publishing_and_a_retry_binds() {
    let source = fixture_source_with_declaration();
    let config_dir = config_dir_with_plugins_dir();
    let dir = config_dir.path();
    let config_path = dir.join("config.toml");
    let mut initial = std::fs::read_to_string(&config_path).expect("read config.toml");
    initial.push_str("\n[channels.plugin.operations]\npackage = \"other-package\"\n");
    std::fs::write(&config_path, &initial).expect("bind the alias to another package");
    let before = std::fs::read(&config_path).expect("read config.toml");

    let refused = install_with_alias(dir, &source, "operations", "declared");
    let text = combined(&refused);
    assert!(
        !refused.status.success(),
        "an alias bound to another package must refuse the install: {text}"
    );
    assert!(
        text.contains("other-package"),
        "the refusal must name the package that owns the alias: {text}"
    );
    let published = plugins_dir_names(&plugins_dir_of(dir));
    assert!(
        !published
            .iter()
            .any(|name| name.contains("channel-egress-fixture")),
        "a refused install must publish nothing: {published:?}\n{text}"
    );
    let after = std::fs::read(&config_path).expect("read config.toml");
    assert!(
        after == before,
        "a refused install must leave config.toml byte-identical; now:\n{}\n{text}",
        String::from_utf8_lossy(&after)
    );

    let retry = install_with_alias(dir, &source, "backup", "declared");
    let retry_text = combined(&retry);
    assert!(
        retry.status.success(),
        "a retry under a free alias must install and bind: {retry_text}"
    );
    let list = run_zeroclaw(dir, &["plugin", "list"]);
    let list_text = combined(&list);
    assert!(
        list.status.success()
            && String::from_utf8_lossy(&list.stdout).contains("channel-egress-fixture"),
        "plugin list must show the installed package: {list_text}"
    );
    let on_disk = config_on_disk(dir);
    assert_eq!(
        bound_package(&on_disk, "operations"),
        Some("other-package"),
        "the other package must keep its alias: {on_disk}"
    );
    assert_eq!(
        bound_package(&on_disk, "backup"),
        Some("channel-egress-fixture"),
        "the retry must bind the free alias: {on_disk}"
    );
    let backup_key = expected_channel_key(&plugins_dir_of(dir), "backup");
    let rows: Vec<&str> = entries_on_disk(&on_disk)
        .into_iter()
        .filter_map(|row| row.get("name").and_then(toml::Value::as_str))
        .collect();
    assert_eq!(
        rows,
        [backup_key.as_str()],
        "only the bound alias may have a row; the refused attempt must create none: {on_disk}"
    );
}
