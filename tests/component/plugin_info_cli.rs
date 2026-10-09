//! Exit-code contract of `zeroclaw plugin info` and `plugin list --verify`,
//! through the real binary.
//!
//! A script that asks about a plugin must be able to branch on the exit code:
//! zero means the plugin is installed and loads here, non-zero means it does
//! not (missing, or refused by this host). `plugin list --verify` is a report,
//! not a check: it names the failure on the row and exits zero either way,
//! and plain `plugin list` never runs the load check at all.

use std::process::{Command, Output};

fn run_zeroclaw(config_dir: &std::path::Path, args: &[&str]) -> Output {
    let bin = env!("CARGO_BIN_EXE_zeroclaw");
    Command::new(bin)
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("RUST_LOG", "off")
        .args(args)
        .output()
        .expect("run zeroclaw")
}

fn throwaway_config_dir() -> tempfile::TempDir {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    std::fs::write(
        config_dir.path().join("config.toml"),
        "schema_version = 3\n",
    )
    .expect("write config");
    config_dir
}

#[test]
fn plugin_info_on_an_unknown_name_exits_non_zero_and_names_it() {
    let config_dir = throwaway_config_dir();
    let out = run_zeroclaw(config_dir.path(), &["plugin", "info", "no-such-plugin"]);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "an unknown plugin must not exit 0: {combined}"
    );
    assert!(
        combined.contains("no-such-plugin"),
        "the failure must name the plugin asked about: {combined}"
    );
}

#[test]
fn plugin_list_with_nothing_installed_still_exits_zero() {
    // The control: listing is a report, not a check, and stays zero.
    let config_dir = throwaway_config_dir();
    let out = run_zeroclaw(config_dir.path(), &["plugin", "list"]);
    assert!(
        out.status.success(),
        "plugin list must exit 0 with nothing installed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The stable head of the loader's error chain for bytes that are not a WASM
/// component; the same text the in-process verifier tests pin.
const LOAD_FAILURE_CAUSE: &str = "failed to load WASM component";

/// A config dir whose plugins dir holds one installed package, `broken-fixture`,
/// whose component file is not a WASM component. Discovery admits it (the
/// manifest is well-formed and signatures are `disabled` by default), so the
/// only thing wrong with it is that it does not load.
fn config_dir_with_a_broken_installed_plugin() -> tempfile::TempDir {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let plugins_dir = config_dir.path().join("plugins");
    let plugin_dir = plugins_dir.join("broken-fixture");
    std::fs::create_dir_all(&plugin_dir).expect("create the installed plugin dir");
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"name = "broken-fixture"
version = "0.0.0"
wasm_path = "broken-fixture.wasm"
capabilities = ["tool"]
permissions = []
"#,
    )
    .expect("write manifest");
    std::fs::write(
        plugin_dir.join("broken-fixture.wasm"),
        b"not a wasm component",
    )
    .expect("write the non-component");
    let plugins_dir = plugins_dir.to_str().expect("utf-8 temp path");
    assert!(
        !plugins_dir.contains('\''),
        "a TOML literal string cannot carry a single quote: {plugins_dir}"
    );
    std::fs::write(
        config_dir.path().join("config.toml"),
        format!("schema_version = 3\n\n[plugins]\nplugins_dir = '{plugins_dir}'\n"),
    )
    .expect("write config");
    config_dir
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn plugin_info_on_an_installed_component_that_does_not_load_exits_non_zero_and_says_why() {
    let config_dir = config_dir_with_a_broken_installed_plugin();
    let out = run_zeroclaw(config_dir.path(), &["plugin", "info", "broken-fixture"]);
    let text = combined(&out);
    assert!(
        !out.status.success(),
        "an installed plugin that does not load must not exit 0: {text}"
    );
    assert!(
        text.contains("broken-fixture"),
        "the verdict must name the plugin: {text}"
    );
    assert!(
        text.contains(LOAD_FAILURE_CAUSE),
        "the verdict must carry the loader's cause: {text}"
    );
}

#[test]
fn plugin_list_verify_names_the_failure_and_exits_zero_while_plain_list_stays_a_catalog() {
    let config_dir = config_dir_with_a_broken_installed_plugin();

    let verified = run_zeroclaw(config_dir.path(), &["plugin", "list", "--verify"]);
    let verified_text = combined(&verified);
    assert!(
        verified.status.success(),
        "plugin list --verify is a report and exits 0 even with a failure on it: {verified_text}"
    );
    let stdout = String::from_utf8_lossy(&verified.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line.contains("broken-fixture") && line.contains(LOAD_FAILURE_CAUSE)),
        "the verified row must carry the plugin's name and its load failure: {verified_text}"
    );

    let plain = run_zeroclaw(config_dir.path(), &["plugin", "list"]);
    let plain_text = combined(&plain);
    assert!(
        plain.status.success(),
        "plain plugin list exits 0: {plain_text}"
    );
    assert!(
        plain_text.contains("broken-fixture"),
        "the catalog still lists the installed package: {plain_text}"
    );
    assert!(
        !plain_text.contains(LOAD_FAILURE_CAUSE),
        "plain plugin list never runs the load check: {plain_text}"
    );
}
