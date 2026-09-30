//! `zeroclaw plugin update` through the real binary, against a local registry.
//!
//! The package is the in-tree tool fixture, a real component, so an update
//! here runs the same load check the daemon runs at startup: a successful
//! update has downloaded, digest-checked, extracted, admitted, load-checked
//! and replaced a real package. Assertions read names, versions, commands and
//! the files on disk, never translated prose, so they hold in every locale.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PACKAGE: &str = "fixture-tool";

/// Build the in-tree tool fixture once per test binary and return its
/// component. It builds into its own target directory, so the nested Cargo
/// run cannot contend with this test's build lock.
fn fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("crates/zeroclaw-plugins/tests/fixtures/tool-fixture");
            let target_dir =
                PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("plugin-update-fixture");
            let status = Command::new(env!("CARGO"))
                .current_dir(&fixture_dir)
                .args([
                    "build",
                    "--locked",
                    "--quiet",
                    "--package",
                    "zeroclaw-tool-plugin-fixture",
                    "--target",
                    "wasm32-wasip2",
                    "--target-dir",
                ])
                .arg(&target_dir)
                .status()
                .expect("run Cargo for the tool component fixture");
            assert!(
                status.success(),
                "tool fixture must build; install the wasm32-wasip2 target"
            );
            let wasm = target_dir.join("wasm32-wasip2/debug/zeroclaw_tool_plugin_fixture.wasm");
            assert!(wasm.is_file(), "tool fixture WASM was not produced");
            wasm
        })
        .clone()
}

fn manifest(version: &str, extra: &str) -> String {
    format!(
        "name = \"{PACKAGE}\"\nversion = \"{version}\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n{extra}"
    )
}

/// A config dir whose plugins dir holds `fixture-tool` 1.0.0 with the real
/// component, installed the way `plugin install` lays it out.
fn config_dir_with_installed_fixture() -> tempfile::TempDir {
    config_dir_with_installed("", "")
}

/// [`config_dir_with_installed_fixture`] with `manifest_extra` appended to the
/// installed manifest and `config_extra` to the config file.
fn config_dir_with_installed(manifest_extra: &str, config_extra: &str) -> tempfile::TempDir {
    config_dir_with_package(&manifest("1.0.0", manifest_extra), config_extra)
}

/// A config dir whose plugins dir holds `fixture-tool` installed with exactly
/// `manifest_toml` and the real component, and `config_extra` appended to the
/// config file.
fn config_dir_with_package(manifest_toml: &str, config_extra: &str) -> tempfile::TempDir {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let plugins_dir = config_dir.path().join("plugins");
    let package = plugins_dir.join(PACKAGE);
    std::fs::create_dir_all(&package).expect("create the installed package");
    std::fs::write(package.join("manifest.toml"), manifest_toml).expect("write manifest");
    std::fs::copy(fixture(), package.join("plugin.wasm")).expect("copy the fixture component");
    let plugins_dir = plugins_dir.to_str().expect("utf-8 temp path");
    assert!(
        !plugins_dir.contains('\''),
        "a TOML literal string cannot carry a single quote: {plugins_dir}"
    );
    std::fs::write(
        config_dir.path().join("config.toml"),
        format!("schema_version = 3\n\n[plugins]\nplugins_dir = '{plugins_dir}'\n{config_extra}"),
    )
    .expect("write config");
    config_dir
}

/// The `[[plugins.entries]]` key of the fixture's tool instance: package,
/// capability and binding, never the version.
fn tool_instance_key() -> String {
    let manifest: zeroclaw_plugins::PluginManifest =
        toml::from_str(&manifest("1.0.0", "")).expect("parse the fixture manifest");
    zeroclaw_plugins::instance::PluginInstanceScope::for_package_binding(
        &manifest,
        zeroclaw_plugins::PluginCapability::Tool,
        std::iter::empty(),
    )
    .expect("tool scope")
    .id()
    .config_entry_key()
    .expect("instance key")
}

/// A local port nothing listens on, so a registry there refuses connections.
fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind a probe port");
    listener.local_addr().expect("probe address").port()
}

/// The fixture's config schema: the typed contract the tool fixture reads.
const CONFIG_SCHEMA: &str = r#"permissions = ["config_read"]

[config_schema]
"$schema" = "https://json-schema.org/draft/2020-12/schema"
type = "object"
additionalProperties = false

[config_schema.properties.label]
type = "string"

[config_schema.properties.max_len]
type = "integer"

[config_schema.properties.uppercase]
type = "boolean"
"#;

fn installed_manifest(config_dir: &Path) -> String {
    std::fs::read_to_string(
        config_dir
            .join("plugins")
            .join(PACKAGE)
            .join("manifest.toml"),
    )
    .expect("the installed manifest")
}

/// A zip holding one package directory: `manifest` and `component` as the
/// file the manifest names.
fn package_archive(manifest: &str, component: &[u8]) -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut bytes);
        let options = zip::write::SimpleFileOptions::default();
        writer
            .start_file(format!("{PACKAGE}/manifest.toml"), options)
            .expect("zip manifest entry");
        writer.write_all(manifest.as_bytes()).expect("zip manifest");
        writer
            .start_file(format!("{PACKAGE}/plugin.wasm"), options)
            .expect("zip component entry");
        writer.write_all(component).expect("zip component");
        writer.finish().expect("finish zip");
    }
    bytes.into_inner()
}

/// Serve a registry listing `fixture-tool` at `version`, whose archive is
/// `archive`, with its digest. `downloads` is how many times the archive must
/// be fetched, checked when the server drops.
async fn serve_registry(server: &MockServer, version: &str, archive: Vec<u8>, downloads: u64) {
    let archive_path = format!("/{PACKAGE}-{version}.zip");
    let index = serde_json::json!({
        "plugins": [{
            "name": PACKAGE,
            "version": version,
            "capabilities": ["tool"],
            "url": format!("{}{archive_path}", server.uri()),
            "sha256": hex::encode(Sha256::digest(&archive)),
        }],
    });
    Mock::given(method("GET"))
        .and(path("/registry.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(index))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(archive_path))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
        .expect(downloads)
        .mount(server)
        .await;
}

/// Run `zeroclaw plugin <args>` against `config_dir` on its own thread, so the
/// registry fixture keeps serving while the binary blocks on it.
fn run_plugin(config_dir: &Path, args: &[&str]) -> Output {
    let config_dir = config_dir.to_path_buf();
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    std::thread::spawn(move || {
        Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
            .env("ZEROCLAW_CONFIG_DIR", &config_dir)
            .env("RUST_LOG", "off")
            .arg("plugin")
            .args(&args)
            .output()
            .expect("run zeroclaw plugin")
    })
    .join()
    .expect("the zeroclaw process must not panic")
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[tokio::test]
async fn update_replaces_the_installed_package_with_the_registry_version() {
    let server = MockServer::start().await;
    let archive = package_archive(
        &manifest("2.0.0", ""),
        &std::fs::read(fixture()).expect("read fixture"),
    );
    serve_registry(&server, "2.0.0", archive, 1).await;
    let config_dir = config_dir_with_installed_fixture();
    let registry = format!("{}/registry.json", server.uri());

    let out = run_plugin(
        config_dir.path(),
        &["update", PACKAGE, "--registry", &registry],
    );
    let text = combined(&out);

    assert!(out.status.success(), "the update must succeed: {text}");
    assert!(text.contains(PACKAGE) && text.contains("2.0.0"), "{text}");
    assert!(
        installed_manifest(config_dir.path()).contains("version = \"2.0.0\""),
        "the replacement is installed: {text}"
    );
    let hidden: Vec<String> = std::fs::read_dir(config_dir.path().join("plugins"))
        .expect("plugins dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.starts_with('.'))
        .collect();
    assert!(
        hidden.is_empty(),
        "nothing hidden is left behind: {hidden:?}"
    );

    // The daemon's own check agrees: the replacement loads here.
    let info = run_plugin(config_dir.path(), &["info", PACKAGE]);
    assert!(info.status.success(), "{}", combined(&info));
    assert!(combined(&info).contains("2.0.0"), "{}", combined(&info));
}

#[tokio::test]
async fn update_when_the_registry_lists_the_installed_version_downloads_nothing() {
    let server = MockServer::start().await;
    serve_registry(&server, "1.0.0", b"never fetched".to_vec(), 0).await;
    let config_dir = config_dir_with_installed_fixture();
    let before = installed_manifest(config_dir.path());
    let registry = format!("{}/registry.json", server.uri());

    let out = run_plugin(
        config_dir.path(),
        &["update", PACKAGE, "--registry", &registry],
    );

    assert!(
        out.status.success(),
        "up to date is success: {}",
        combined(&out)
    );
    assert!(combined(&out).contains("1.0.0"), "{}", combined(&out));
    assert_eq!(installed_manifest(config_dir.path()), before);
}

#[tokio::test]
async fn update_refuses_added_authority_until_accepted_with_the_printed_items() {
    let server = MockServer::start().await;
    let archive = package_archive(
        &manifest("2.0.0", "permissions = [\"state_read\"]\n"),
        &std::fs::read(fixture()).expect("read fixture"),
    );
    serve_registry(&server, "2.0.0", archive, 2).await;
    let config_dir = config_dir_with_installed_fixture();
    let registry = format!("{}/registry.json", server.uri());

    let refused = run_plugin(
        config_dir.path(),
        &["update", PACKAGE, "--registry", &registry],
    );
    let text = combined(&refused);
    assert!(
        !refused.status.success(),
        "unaccepted authority must fail: {text}"
    );
    for part in ["--allow", "permission:state_read", "fixture-tool@2.0.0"] {
        assert!(
            text.contains(part),
            "the refusal must print {part:?}: {text}"
        );
    }
    assert!(
        installed_manifest(config_dir.path()).contains("version = \"1.0.0\""),
        "a refused update changes nothing: {text}"
    );

    let accepted = run_plugin(
        config_dir.path(),
        &[
            "update",
            "fixture-tool@2.0.0",
            "--registry",
            &registry,
            "--allow",
            "permission:state_read",
        ],
    );
    assert!(accepted.status.success(), "{}", combined(&accepted));
    assert!(installed_manifest(config_dir.path()).contains("state_read"));
}

#[tokio::test]
async fn update_keeps_the_installed_package_when_the_replacement_does_not_load() {
    let server = MockServer::start().await;
    let archive = package_archive(&manifest("2.0.0", ""), b"not a wasm component");
    serve_registry(&server, "2.0.0", archive, 1).await;
    let config_dir = config_dir_with_installed_fixture();
    let before = installed_manifest(config_dir.path());
    let registry = format!("{}/registry.json", server.uri());

    let out = run_plugin(
        config_dir.path(),
        &["update", PACKAGE, "--registry", &registry],
    );
    let text = combined(&out);

    assert!(
        !out.status.success(),
        "a replacement that does not load must fail: {text}"
    );
    assert!(text.contains("failed to load WASM component"), "{text}");
    assert!(text.contains("1.0.0"), "the kept version is named: {text}");
    assert_eq!(installed_manifest(config_dir.path()), before);
    assert_eq!(
        std::fs::read(
            config_dir
                .path()
                .join("plugins")
                .join(PACKAGE)
                .join("plugin.wasm")
        )
        .expect("installed component"),
        std::fs::read(fixture()).expect("read fixture"),
        "the installed component is untouched"
    );
}

/// One plugin failing does not stop the next. The first plugin fails inside
/// its update, on an archive whose digest does not match the registry, and
/// keeps its installed package; the plugin after it is still replaced.
#[tokio::test]
async fn one_plugin_failing_does_not_stop_the_update_of_the_next() {
    let component = std::fs::read(fixture()).expect("read fixture");
    let config_dir = config_dir_with_installed_fixture();
    let other = config_dir.path().join("plugins").join("other-tool");
    std::fs::create_dir_all(&other).expect("create the second package");
    let other_manifest = manifest("1.0.0", "").replace(PACKAGE, "other-tool");
    std::fs::write(other.join("manifest.toml"), &other_manifest).expect("write manifest");
    std::fs::copy(fixture(), other.join("plugin.wasm")).expect("copy the component");

    let server = MockServer::start().await;
    let good = package_archive(&manifest("2.0.0", ""), &component);
    let tampered = package_archive(
        &manifest("2.0.0", "").replace(PACKAGE, "other-tool"),
        &component,
    );
    let index = serde_json::json!({
        "plugins": [
            {
                "name": "other-tool",
                "version": "2.0.0",
                "capabilities": ["tool"],
                "url": format!("{}/other-tool-2.0.0.zip", server.uri()),
                "sha256": hex::encode(Sha256::digest(b"a different archive")),
            },
            {
                "name": PACKAGE,
                "version": "2.0.0",
                "capabilities": ["tool"],
                "url": format!("{}/{PACKAGE}-2.0.0.zip", server.uri()),
                "sha256": hex::encode(Sha256::digest(&good)),
            },
        ],
    });
    Mock::given(method("GET"))
        .and(path("/registry.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(index))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/other-tool-2.0.0.zip"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(tampered))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/{PACKAGE}-2.0.0.zip")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(good))
        .expect(1)
        .mount(&server)
        .await;
    let registry = format!("{}/registry.json", server.uri());

    let out = run_plugin(
        config_dir.path(),
        &["update", "other-tool", PACKAGE, "--registry", &registry],
    );
    let text = combined(&out);

    assert!(
        !out.status.success(),
        "a plugin that was not updated fails the command: {text}"
    );
    assert!(
        text.contains("sha256 mismatch"),
        "the first plugin fails inside its update: {text}"
    );
    assert_eq!(
        std::fs::read_to_string(other.join("manifest.toml")).expect("installed manifest"),
        other_manifest,
        "the plugin whose update failed keeps its installed package"
    );
    assert!(
        installed_manifest(config_dir.path()).contains("version = \"2.0.0\""),
        "the plugin after the failure is still updated: {text}"
    );
}

/// The recovery the operator is pointed at works without the registry: a
/// package an interrupted update displaced is put back even when the registry
/// cannot be reached, and only then does the unreachable registry fail the
/// command.
#[test]
fn a_displaced_package_is_put_back_without_reaching_the_registry() {
    let config_dir = config_dir_with_installed_fixture();
    let plugins = config_dir.path().join("plugins");
    let displaced = plugins.join(format!(".{PACKAGE}.replaced-4242"));
    std::fs::rename(plugins.join(PACKAGE), &displaced)
        .expect("displace the package the way a stopped update leaves it");
    let registry = format!("http://127.0.0.1:{}/registry.json", closed_port());

    let out = run_plugin(
        config_dir.path(),
        &["update", PACKAGE, "--registry", &registry],
    );
    let text = combined(&out);

    assert!(
        !out.status.success(),
        "the unreachable registry still fails the update: {text}"
    );
    assert!(
        installed_manifest(config_dir.path()).contains("version = \"1.0.0\""),
        "the displaced package is back in place: {text}"
    );
    assert!(!displaced.exists(), "{text}");
}

/// Installing a package that is already installed from a local directory
/// points at updating it from that same directory, never at the registry,
/// which could replace a local build with a same-named registry package.
#[test]
fn installing_an_installed_local_package_points_at_updating_it_from_that_directory() {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let plugins_dir = config_dir.path().join("plugins");
    let plugins = plugins_dir.to_str().expect("utf-8 temp path");
    std::fs::write(
        config_dir.path().join("config.toml"),
        format!("schema_version = 3\n\n[plugins]\nplugins_dir = '{plugins}'\n"),
    )
    .expect("write config");
    let source = tempfile::tempdir().expect("source dir");
    std::fs::write(source.path().join("manifest.toml"), manifest("1.0.0", ""))
        .expect("write manifest");
    std::fs::copy(fixture(), source.path().join("plugin.wasm")).expect("copy component");
    let source_arg = source.path().to_str().expect("utf-8 temp path");

    let first = run_plugin(config_dir.path(), &["install", source_arg]);
    assert!(first.status.success(), "{}", combined(&first));
    let second = run_plugin(config_dir.path(), &["install", source_arg]);
    let text = combined(&second);

    assert!(!second.status.success(), "{text}");
    assert!(
        text.contains("plugin update") && text.contains("--from"),
        "the pointer updates from the local directory: {text}"
    );
    assert!(text.contains(source_arg), "{text}");
}

/// `--all` never puts a displaced package back unasked: it points at the
/// commands that do, and leaves the copy where it is. The note, from `--all`
/// and from `plugin list`, offers two complete commands for this
/// configuration: one from the registry, with the run's `--registry`, and one
/// from a package directory, which never carries `--registry`.
#[test]
fn all_points_at_a_displaced_package_instead_of_putting_it_back() {
    let config_dir = config_dir_with_installed_fixture();
    let plugins = config_dir.path().join("plugins");
    let displaced = plugins.join(format!(".{PACKAGE}.replaced-4242"));
    std::fs::rename(plugins.join(PACKAGE), &displaced)
        .expect("displace the package the way a stopped update leaves it");
    let registry = format!("http://127.0.0.1:{}/registry.json", closed_port());
    let config_dir_name = config_dir
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .expect("utf-8 temp dir name");
    // The note's two commands, the ones in backticks, when `text` has it.
    let commands = |text: &str| -> Option<(String, String)> {
        text.lines().find_map(|line| {
            let quoted: Vec<&str> = line.split('`').skip(1).step_by(2).collect();
            let [registry_command, local_command] = quoted[..] else {
                return None;
            };
            [registry_command, local_command]
                .iter()
                .all(|command| {
                    command.contains("--config-dir")
                        && command.contains(config_dir_name)
                        && command.contains(&format!("plugin update '{PACKAGE}'"))
                })
                .then(|| (registry_command.to_string(), local_command.to_string()))
        })
    };

    let out = run_plugin(
        config_dir.path(),
        &["update", "--all", "--registry", &registry],
    );
    let text = combined(&out);

    let (registry_command, local_command) =
        commands(&text).unwrap_or_else(|| panic!("the note names both commands: {text}"));
    assert!(
        registry_command.contains(&registry) && !registry_command.contains("--from"),
        "{text}"
    );
    assert!(
        local_command.contains("--from") && !local_command.contains("--registry"),
        "{text}"
    );
    assert!(displaced.is_dir(), "the copy stays where it is: {text}");
    assert!(!plugins.join(PACKAGE).exists(), "{text}");

    // Select the configuration through `ZEROCLAW_DATA_DIR` this time, so a
    // note built from anything but the loaded configuration names another
    // directory.
    let home = tempfile::tempdir().expect("empty home");
    let list = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env_remove("ZEROCLAW_CONFIG_DIR")
        .env_remove("ZEROCLAW_WORKSPACE")
        .env("ZEROCLAW_DATA_DIR", config_dir.path())
        .env("HOME", home.path())
        .env("RUST_LOG", "off")
        .args(["plugin", "list"])
        .output()
        .expect("run zeroclaw plugin list");
    let listed = combined(&list);
    let (registry_command, local_command) =
        commands(&listed).unwrap_or_else(|| panic!("`plugin list` names both commands: {listed}"));
    assert!(!registry_command.contains("--registry"), "{listed}");
    assert!(local_command.contains("--from"), "{listed}");
    assert!(displaced.is_dir(), "{listed}");
}

/// A name that is not installed is answered before the registry is
/// consulted, so an unreachable registry cannot hide the pointer to install.
#[test]
fn a_name_that_is_not_installed_is_answered_without_the_registry() {
    let config_dir = config_dir_with_installed_fixture();
    let registry = format!("http://127.0.0.1:{}/registry.json", closed_port());

    let out = run_plugin(
        config_dir.path(),
        &["update", "missing-tool", "--registry", &registry],
    );
    let text = combined(&out);

    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("plugin install") && text.contains("missing-tool"),
        "{text}"
    );
}

/// Authority accepted for a registry update is pinned to the version the
/// operator reviewed: without `name@version` the command is refused before
/// anything is fetched or changed.
#[test]
fn allow_for_a_registry_update_needs_a_pinned_version() {
    let config_dir = config_dir_with_installed_fixture();
    let before = installed_manifest(config_dir.path());
    let registry = format!("http://127.0.0.1:{}/registry.json", closed_port());

    let out = run_plugin(
        config_dir.path(),
        &[
            "update",
            PACKAGE,
            "--registry",
            &registry,
            "--allow",
            "permission:state_read",
        ],
    );
    let text = combined(&out);

    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("--allow"),
        "the refusal names the flag: {text}"
    );
    assert_eq!(installed_manifest(config_dir.path()), before);
}

/// An update preserves the instance's configuration: the row that holds its
/// config values, egress grant and private carve-out is left byte for byte as
/// it was, and the instance keeps its key.
#[tokio::test]
async fn update_leaves_the_instance_config_row_as_it_was() {
    let key = tool_instance_key();
    let row = format!(
        "\n[[plugins.entries]]\nname = \"{key}\"\negress_hosts = [\"api.example.com\"]\negress_allow_private = [\"api.example.com\"]\n\n[plugins.entries.config]\nlabel = \"kept\"\n"
    );
    let config_dir = config_dir_with_installed(CONFIG_SCHEMA, &row);
    let server = MockServer::start().await;
    let archive = package_archive(
        &manifest("2.0.0", CONFIG_SCHEMA),
        &std::fs::read(fixture()).expect("read fixture"),
    );
    serve_registry(&server, "2.0.0", archive, 1).await;
    let registry = format!("{}/registry.json", server.uri());

    // Let any load-time rewrite of the config file happen before the snapshot,
    // so the comparison below sees only what the update wrote.
    let info = run_plugin(config_dir.path(), &["info", PACKAGE]);
    assert!(info.status.success(), "{}", combined(&info));
    assert!(combined(&info).contains(&key), "{}", combined(&info));
    let config_path = config_dir.path().join("config.toml");
    let before = std::fs::read(&config_path).expect("config before");

    let out = run_plugin(
        config_dir.path(),
        &["update", PACKAGE, "--registry", &registry],
    );
    assert!(out.status.success(), "{}", combined(&out));

    assert_eq!(
        std::fs::read(&config_path).expect("config after"),
        before,
        "the update must not rewrite the config file"
    );
    let info = run_plugin(config_dir.path(), &["info", PACKAGE]);
    let info_text = combined(&info);
    assert!(info.status.success(), "{info_text}");
    assert!(
        info_text.contains("2.0.0") && info_text.contains(&key),
        "the new version keeps the instance key: {info_text}"
    );
}

/// An update never grants reach. A new version that adds `http_client` and
/// declares a destination gets a config row created for it, with no grant,
/// and the command that would grant it is printed for the operator to run.
#[tokio::test]
async fn update_never_grants_egress_even_in_a_row_it_creates() {
    let server = MockServer::start().await;
    let archive = package_archive(
        &manifest(
            "2.0.0",
            "permissions = [\"http_client\"]\n\n[egress]\nhosts = [\"api.example.com\"]\n",
        ),
        &std::fs::read(fixture()).expect("read fixture"),
    );
    serve_registry(&server, "2.0.0", archive, 1).await;
    let config_dir = config_dir_with_installed_fixture();
    let registry = format!("{}/registry.json", server.uri());

    let out = run_plugin(
        config_dir.path(),
        &[
            "update",
            "fixture-tool@2.0.0",
            "--registry",
            &registry,
            "--allow",
            "permission:http_client",
        ],
    );
    let text = combined(&out);
    assert!(out.status.success(), "{text}");

    let key = tool_instance_key();
    let config: toml::Table = toml::from_str(
        &std::fs::read_to_string(config_dir.path().join("config.toml")).expect("config after"),
    )
    .expect("config parses");
    let row = config["plugins"]["entries"]
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry.get("name").and_then(toml::Value::as_str) == Some(&key))
        })
        .unwrap_or_else(|| panic!("the update creates the instance's row: {config:#?}"));
    let granted = row
        .get("egress_hosts")
        .and_then(toml::Value::as_array)
        .map_or(0, Vec::len);
    assert_eq!(granted, 0, "a created row grants nothing: {row:#?}");
    assert!(
        text.contains("api.example.com") && text.contains("egress_hosts"),
        "the declared destination and the grant command are printed: {text}"
    );
}

/// A manifest for `version` that binds `component`'s digest and is signed
/// with `private_key`: the shape strict signature policy admits.
fn signed_manifest(
    version: &str,
    component: &[u8],
    private_key: &[u8],
    public_key: &str,
) -> String {
    let unsigned = format!(
        "name = \"{PACKAGE}\"\nversion = \"{version}\"\nwasm_path = \"plugin.wasm\"\nwasm_sha256 = \"{}\"\ncapabilities = [\"tool\"]\n",
        zeroclaw_plugins::signature::sha256_hex(component)
    );
    let signature =
        zeroclaw_plugins::signature::sign_manifest(&unsigned, private_key).expect("sign manifest");
    unsigned.replacen(
        "wasm_path = \"plugin.wasm\"",
        &format!(
            "signature = \"{signature}\"\npublisher_key = \"{public_key}\"\nwasm_path = \"plugin.wasm\""
        ),
        1,
    )
}

/// The configured signature policy governs the replacement through the real
/// command: under `strict`, an unsigned replacement is refused and the
/// installed package stays as it was, and one signed by the trusted key is
/// installed.
#[tokio::test]
async fn update_applies_the_configured_signature_policy() {
    let component = std::fs::read(fixture()).expect("read fixture");
    let (private_key, public_key) =
        zeroclaw_plugins::signature::generate_signing_key().expect("signing key");
    let config_dir = config_dir_with_package(
        &signed_manifest("1.0.0", &component, &private_key, &public_key),
        &format!(
            "\n[plugins.security]\nsignature_mode = \"strict\"\ntrusted_publisher_keys = [\"{public_key}\"]\n"
        ),
    );
    let before = installed_manifest(config_dir.path());

    let unsigned = MockServer::start().await;
    serve_registry(
        &unsigned,
        "2.0.0",
        package_archive(&manifest("2.0.0", ""), &component),
        1,
    )
    .await;
    let registry = format!("{}/registry.json", unsigned.uri());
    let refused = run_plugin(
        config_dir.path(),
        &["update", PACKAGE, "--registry", &registry],
    );
    let text = combined(&refused);
    assert!(
        !refused.status.success(),
        "strict policy refuses an unsigned replacement: {text}"
    );
    // The policy's own refusal, not the authority gate: under a disabled
    // policy the dropped publisher key would be refused too, but only as
    // needing approval.
    assert!(
        text.contains("signature verification is required"),
        "{text}"
    );
    assert_eq!(installed_manifest(config_dir.path()), before);

    let signed = MockServer::start().await;
    serve_registry(
        &signed,
        "2.0.0",
        package_archive(
            &signed_manifest("2.0.0", &component, &private_key, &public_key),
            &component,
        ),
        1,
    )
    .await;
    let registry = format!("{}/registry.json", signed.uri());
    let accepted = run_plugin(
        config_dir.path(),
        &["update", PACKAGE, "--registry", &registry],
    );
    assert!(accepted.status.success(), "{}", combined(&accepted));
    assert!(installed_manifest(config_dir.path()).contains("version = \"2.0.0\""));
}

/// Manifest text reaches the operator escaped on every line an update
/// prints. The replacement's version carries an escape sequence, and its
/// schema newly requires a property, so the warning that the instance's
/// preserved configuration no longer fits prints that version too; neither it
/// nor the result line may carry a raw control byte.
#[tokio::test]
async fn update_output_never_carries_a_control_byte_from_the_manifest() {
    let component = std::fs::read(fixture()).expect("read fixture");
    let config_dir = config_dir_with_installed(CONFIG_SCHEMA, "");
    let required = CONFIG_SCHEMA.replace(
        "additionalProperties = false",
        "additionalProperties = false\nrequired = [\"label\"]",
    );
    let candidate = format!(
        "name = \"{PACKAGE}\"\nversion = \"2.0.0\\u001B[2K\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n{required}"
    );
    let archive = package_archive(&candidate, &component);
    let server = MockServer::start().await;
    let index = serde_json::json!({
        "plugins": [{
            "name": PACKAGE,
            "version": "2.0.0\u{1b}[2K",
            "capabilities": ["tool"],
            "url": format!("{}/escaped.zip", server.uri()),
            "sha256": hex::encode(Sha256::digest(&archive)),
        }],
    });
    Mock::given(method("GET"))
        .and(path("/registry.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(index))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/escaped.zip"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
        .expect(1)
        .mount(&server)
        .await;
    let registry = format!("{}/registry.json", server.uri());

    let out = run_plugin(
        config_dir.path(),
        &["update", PACKAGE, "--registry", &registry],
    );
    let text = combined(&out);

    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("/required") && text.contains("'label'"),
        "the preserved-config warning names the missing property: {text}"
    );
    assert!(
        !out.stdout.contains(&0x1b) && !out.stderr.contains(&0x1b),
        "no raw escape byte reaches the terminal: {text:?}"
    );
    assert!(
        text.contains("\\u{1b}[2K"),
        "the sequence is shown, escaped: {text}"
    );
}

/// A package whose config row still uses the pre-1.0 key, the package name, is
/// not updated: nothing is replaced or created, and the rename and grant steps
/// `plugin list` prints for that row are printed in order, each on its own
/// line.
#[test]
fn update_refuses_a_package_whose_config_row_uses_the_pre_1_0_key() {
    let egress = "permissions = [\"http_client\"]\n\n[egress]\nhosts = [\"api.example.com\"]\n";
    let config_dir = config_dir_with_installed(
        egress,
        &format!("\n[[plugins.entries]]\nname = \"{PACKAGE}\"\n"),
    );
    let before = installed_manifest(config_dir.path());
    let source = tempfile::tempdir().expect("source dir");
    std::fs::write(
        source.path().join("manifest.toml"),
        manifest("2.0.0", egress),
    )
    .expect("write the replacement manifest");
    std::fs::copy(fixture(), source.path().join("plugin.wasm"))
        .expect("copy the fixture component");
    let source_arg = source.path().to_str().expect("utf-8 temp path");

    let out = run_plugin(
        config_dir.path(),
        &["update", PACKAGE, "--from", source_arg],
    );
    let text = combined(&out);

    assert!(!out.status.success(), "{text}");
    assert_eq!(installed_manifest(config_dir.path()), before, "{text}");
    let config: toml::Table = toml::from_str(
        &std::fs::read_to_string(config_dir.path().join("config.toml")).expect("config after"),
    )
    .expect("config parses");
    let rows: Vec<&str> = config["plugins"]["entries"]
        .as_array()
        .expect("the pre-1.0 row stays")
        .iter()
        .filter_map(|entry| entry.get("name").and_then(toml::Value::as_str))
        .collect();
    assert_eq!(
        rows,
        [PACKAGE],
        "no row is created under the instance key: {text}"
    );

    let key = tool_instance_key();
    let lines: Vec<&str> = text.lines().collect();
    let rename = lines
        .iter()
        .position(|line| line.contains(&key) && !line.contains("egress_hosts"))
        .unwrap_or_else(|| panic!("the rename step names the instance key: {text}"));
    let grant = lines
        .iter()
        .position(|line| line.contains("egress_hosts") && line.contains("api.example.com"))
        .unwrap_or_else(|| panic!("the grant command is printed: {text}"));
    assert!(rename < grant, "the rename comes before the grant: {text}");
    assert!(
        !text.contains("\\u{a}"),
        "no step is folded into another line: {text}"
    );
}

/// Updating a name that is not installed from a local directory points at
/// installing that directory, never at the registry, which could install a
/// same-named registry package in place of the operator's local build.
#[test]
fn a_local_update_of_a_name_that_is_not_installed_points_at_installing_that_directory() {
    let config_dir = config_dir_with_installed_fixture();
    let source = tempfile::tempdir().expect("source dir");
    let source_arg = source.path().to_str().expect("utf-8 temp path");

    let out = run_plugin(
        config_dir.path(),
        &["update", "missing-tool", "--from", source_arg],
    );
    let text = combined(&out);

    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("plugin install") && text.contains(source_arg),
        "the pointer installs the local directory: {text}"
    );
}

/// Source admission accepts the nested link, but copying deliberately omits
/// it. The CLI must reject that incomplete staging tree before replacement.
#[cfg(unix)]
#[test]
fn local_update_rejects_incomplete_materialized_skill_and_preserves_config() {
    let key = tool_instance_key();
    let row = format!(
        "\n[[plugins.entries]]\nname = \"{key}\"\negress_hosts = [\"api.example.com\"]\negress_allow_private = [\"api.example.com\"]\n\n[plugins.entries.config]\nlabel = \"kept\"\n"
    );
    let bundle_manifest = |version| {
        manifest(version, CONFIG_SCHEMA).replace(
            "capabilities = [\"tool\"]",
            "capabilities = [\"tool\", \"skill\"]",
        )
    };
    let config_dir = config_dir_with_package(&bundle_manifest("1.0.0"), &row);
    let config_path = config_dir.path().join("config.toml");
    let config = std::fs::read_to_string(&config_path).unwrap();
    std::fs::write(&config_path, format!("locale = \"en\"\n{config}")).unwrap();
    let installed = config_dir.path().join("plugins").join(PACKAGE);
    let skill =
        "---\nname: alpha\ndescription: Original usable skill\n---\nOriginal instructions.\n";
    std::fs::create_dir_all(installed.join("skills/alpha")).unwrap();
    std::fs::write(installed.join("skills/alpha/SKILL.md"), skill).unwrap();
    let info = run_plugin(config_dir.path(), &["info", PACKAGE]);
    assert!(info.status.success(), "{}", combined(&info));
    let config_path = config_dir.path().join("config.toml");
    let before_config = std::fs::read(&config_path).unwrap();
    let before_manifest = std::fs::read(installed.join("manifest.toml")).unwrap();
    let before_component = std::fs::read(installed.join("plugin.wasm")).unwrap();

    let source = tempfile::tempdir().unwrap();
    std::fs::write(
        source.path().join("manifest.toml"),
        bundle_manifest("2.0.0"),
    )
    .unwrap();
    std::fs::copy(fixture(), source.path().join("plugin.wasm")).unwrap();
    std::fs::create_dir_all(source.path().join("skills/beta")).unwrap();
    let target = source.path().join("skill-target.md");
    std::fs::write(
        &target,
        "---\nname: beta\ndescription: Replacement skill\n---\nNew instructions.\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&target, source.path().join("skills/beta/SKILL.md")).unwrap();
    let source_arg = source.path().to_str().unwrap();
    let host =
        zeroclaw_plugins::host::PluginHost::from_plugins_dir(&config_dir.path().join("plugins"))
            .unwrap();
    host.admit_update(PACKAGE, source_arg)
        .expect("valid source admission must reach the materialization boundary");

    let out = run_plugin(
        config_dir.path(),
        &["update", PACKAGE, "--from", source_arg],
    );
    let text = combined(&out);
    assert!(
        !out.status.success(),
        "incomplete staging must fail: {text}"
    );
    assert!(
        text.contains("subdirectory 'beta' is missing SKILL.md"),
        "must reject the incomplete materialized skill, not an earlier boundary: {text}"
    );
    assert_eq!(
        std::fs::read(&config_path).unwrap(),
        before_config,
        "config and grants stay unchanged"
    );
    assert_eq!(
        std::fs::read(installed.join("manifest.toml")).unwrap(),
        before_manifest
    );
    assert_eq!(
        std::fs::read(installed.join("plugin.wasm")).unwrap(),
        before_component
    );
    assert_eq!(
        std::fs::read_to_string(installed.join("skills/alpha/SKILL.md")).unwrap(),
        skill
    );
    assert!(!installed.join("skills/beta").exists());
    assert_eq!(
        std::fs::read_dir(config_dir.path().join("plugins"))
            .unwrap()
            .count(),
        1,
        "no displaced or staging package remains"
    );
    let info = run_plugin(config_dir.path(), &["info", PACKAGE]);
    let text = combined(&info);
    assert!(
        info.status.success() && text.contains("1.0.0"),
        "original package remains usable: {text}"
    );
}
