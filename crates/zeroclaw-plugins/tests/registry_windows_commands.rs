//! Native Windows shell regression. Not evidence until executed on Windows.
//! Uses production output and a native executable recorder, not a batch file.
//! Canonical focused Windows entrypoint (authorized native validation only):
//! powershell.exe -NoProfile -File scripts/ci/registry_windows_gate.ps1
//! The gate builds this candidate's recorder in a fresh target directory and
//! binds its Cargo-reported executable path. A bare focused cargo test does not
//! automatically build the example. Missing shells/fixture fail, never skip.
#![cfg(windows)]

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use zeroclaw_plugins::registry::{PluginRegistryEntry, ShellDialect, registry_install_command_for};

const RECORDER: &str = "ZEROCLAW_REGISTRY_ARGV_FILE";
const MARKER: &str = "ZC_REGISTRY_HARMLESS_MARKER_12216";

fn entry(version: &str) -> PluginRegistryEntry {
    PluginRegistryEntry {
        name: "calendar".into(),
        version: version.into(),
        description: None,
        author: None,
        capabilities: vec![],
        url: "https://download.invalid/p.zip".into(),
        sha256: None,
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    record: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // Build this example separately before running this test. No nested
        // compiler invocation and no batch-file approximation of native argv.
        let fixture = std::env::var_os("ZEROCLAW_REGISTRY_ARGV_FIXTURE")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_exe()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("examples/registry_argv_fixture.exe")
            });
        let fixture = std::fs::canonicalize(fixture)
            .expect("build registry_argv_fixture example before native shell tests");
        assert_eq!(
            fixture.extension().and_then(|ext| ext.to_str()),
            Some("exe")
        );
        std::fs::copy(fixture, dir.path().join("zeroclaw.exe")).unwrap();
        let record = dir.path().join("argv.json");
        Self { dir, record }
    }
    fn run(&self, shell: &str, line: &str, delayed: bool) -> std::process::Output {
        let mut command = Command::new(shell);
        if shell == "cmd.exe" {
            command.args(["/D", if delayed { "/V:ON" } else { "/V:OFF" }, "/S", "/C"]);
            // cmd.exe is not a C-runtime argv parser. /S /C removes this
            // extra outer quote pair; raw_arg preserves the already-rendered
            // production line byte-for-byte, including its argument quotes.
            // https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html#tymethod.raw_arg
            command.raw_arg(format!("\"{line}\""));
        } else {
            command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", line]);
        }
        let mut paths = vec![self.dir.path().to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        command
            .current_dir(self.dir.path())
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env(RECORDER, &self.record)
            .env("ZC_REGISTRY_EXPAND", "EXPANDED")
            .env("RUST_TEST_THREADS", "1")
            .output()
            .expect("native Windows shell must be available; do not skip")
    }
    fn args(&self) -> Vec<String> {
        serde_json::from_slice(&std::fs::read(&self.record).expect("native recorder ran")).unwrap()
    }
}

#[test]
fn registry_windows_native_shells_preserve_safe_arguments() {
    for shell in ["cmd.exe", "powershell.exe"] {
        for version in [
            "1.2.3",
            "1 & echo ZC_REGISTRY_HARMLESS_MARKER_12216",
            "1|echo marker",
            "1;echo marker",
            "1^(team)",
            "1's team",
            "1café日历",
        ] {
            for delayed in [false, true] {
                let fixture = Fixture::new();
                let unicode = version == "1café日历";
                let origin = if unicode {
                    "https://registry.invalid/index?channel=stable&team=café日历"
                } else {
                    "https://registry.invalid/index?channel=stable&team=one"
                };
                let config_text = if unicode {
                    "C:/profiles/team café日历"
                } else {
                    "C:/profiles/team one"
                };
                let config = Path::new(config_text);
                let mut plugin = entry(version);
                if unicode {
                    plugin.name = "café日历".into();
                }
                let line = registry_install_command_for(
                    ShellDialect::Windows,
                    &plugin,
                    Some(origin),
                    Some(config),
                    true,
                )
                .unwrap();
                let output = fixture.run(shell, &line, delayed);
                assert!(output.status.success(), "{shell}: {output:?}");
                assert!(!String::from_utf8_lossy(&output.stdout).contains(MARKER));
                let spec = format!("{}@{version}", plugin.name);
                assert_eq!(
                    fixture.args(),
                    vec![
                        "--config-dir",
                        config_text,
                        "plugin",
                        "install",
                        &spec,
                        "--registry",
                        origin,
                        "--no-verify"
                    ]
                );
            }
        }
    }
}

#[test]
fn registry_windows_native_shells_have_no_recipe_for_unsafe_values() {
    for shell in ["cmd.exe", "powershell.exe"] {
        for raw in [
            "1%ZC_REGISTRY_EXPAND% & echo ZC_REGISTRY_HARMLESS_MARKER_12216",
            "1!ZC_REGISTRY_EXPAND! & echo ZC_REGISTRY_HARMLESS_MARKER_12216",
            "1\" & echo ZC_REGISTRY_HARMLESS_MARKER_12216",
            "1$HOME & echo ZC_REGISTRY_HARMLESS_MARKER_12216",
            "1`echo marker` & echo ZC_REGISTRY_HARMLESS_MARKER_12216",
            "1\u{201c}",
            "1\u{201d}",
            "1\u{201e}",
            "1”; echo ZC_REGISTRY_HARMLESS_MARKER_12216; echo “",
            "1\r\necho ZC_REGISTRY_HARMLESS_MARKER_12216",
            "1\\",
        ] {
            // Trailing name backslashes are internal to name@version; the
            // dedicated positive case below executes that rendered command.
            let slots = if raw.ends_with('\\') { 3 } else { 4 };
            for slot in 0..slots {
                let fixture = Fixture::new();
                let mut plugin = entry(if slot == 0 { raw } else { "1" });
                if slot == 3 {
                    plugin.name = raw.into();
                }
                let recipe = registry_install_command_for(
                    ShellDialect::Windows,
                    &plugin,
                    (slot == 1).then_some(raw),
                    (slot == 2).then_some(Path::new(raw)),
                    true,
                );
                assert_eq!(recipe, None, "unsupported data has no executable fallback");
                // Only Some is pasted. A fixed benign probe exercises the
                // shell when unavailable, never prose or raw metadata.
                let line = recipe.as_deref().unwrap_or("echo ZC_REGISTRY_NO_RECIPE");
                let output = fixture.run(shell, line, true);
                assert!(output.status.success(), "{shell}: {output:?}");
                assert!(String::from_utf8_lossy(&output.stdout).contains("ZC_REGISTRY_NO_RECIPE"));
                assert!(!String::from_utf8_lossy(&output.stdout).contains(MARKER));
                assert!(
                    !fixture.record.exists(),
                    "unavailable must not launch zeroclaw"
                );
            }
        }
        for delayed in [false, true] {
            let fixture = Fixture::new();
            let mut plugin = entry("1");
            plugin.name = "calendar\\".into();
            let origin = "https://registry.invalid/index?team=one&channel=stable";
            let config = "C:/profiles/team one";
            let line = registry_install_command_for(
                ShellDialect::Windows,
                &plugin,
                Some(origin),
                Some(Path::new(config)),
                true,
            )
            .unwrap();
            assert_eq!(
                line,
                r#"zeroclaw --config-dir "C:/profiles/team one" plugin install "calendar\@1" --registry "https://registry.invalid/index?team=one&channel=stable" --no-verify"#
            );
            let output = fixture.run(shell, &line, delayed);
            assert!(output.status.success(), "{shell}: {output:?}");
            assert!(!String::from_utf8_lossy(&output.stdout).contains(MARKER));
            assert_eq!(
                fixture.args(),
                vec![
                    "--config-dir",
                    config,
                    "plugin",
                    "install",
                    r"calendar\@1",
                    "--registry",
                    origin,
                    "--no-verify"
                ]
            );
        }
    }
}

#[test]
fn registry_explicit_powershell_preserves_shell_specific_values() {
    let fixture = Fixture::new();
    let version = "1%ZC_REGISTRY_EXPAND% & echo ZC_REGISTRY_HARMLESS_MARKER_12216";
    let origin = "https://registry.invalid/index?team=it's%20mine&channel=stable";
    let config = "C:/profiles/100%team";
    let line = registry_install_command_for(
        ShellDialect::PowerShell,
        &entry(version),
        Some(origin),
        Some(Path::new(config)),
        true,
    )
    .unwrap();
    // This form is intentionally exercised ONLY in its explicitly requested
    // shell. It is never offered as a Windows copy-ready fallback.
    let output = fixture.run("powershell.exe", &line, false);
    assert!(output.status.success(), "{output:?}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains(MARKER));
    let spec = format!("calendar@{version}");
    assert_eq!(
        fixture.args(),
        vec![
            "--config-dir",
            config,
            "plugin",
            "install",
            &spec,
            "--registry",
            origin,
            "--no-verify"
        ]
    );
}
