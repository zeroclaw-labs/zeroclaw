//! Regression: `--config-dir` must drive CLI locale detection, not only the
//! `ZEROCLAW_CONFIG_DIR` env var.
//!
//! Before the fix, locale was detected (and the clap help tree translated)
//! *before* the parsed `--config-dir` flag was applied to the environment, so
//! `--help` always rendered in the default/env locale and the flag was ignored
//! for every CLI string. These tests spawn the real binary (each invocation is a
//! fresh process, which is required because the resolved locale lives in a
//! process-global `OnceLock`) and assert the flag now selects the locale.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;

const JA_CLI_FTL: &str = include_str!("../../crates/zeroclaw-runtime/locales/ja/cli.ftl");
const ES_CLI_FTL: &str = include_str!("../../crates/zeroclaw-runtime/locales/es/cli.ftl");

fn catalog_message<'a>(catalog: &'a str, key: &str) -> &'a str {
    catalog
        .lines()
        .find_map(|line| {
            let (candidate, value) = line.split_once(" = ")?;
            (candidate == key).then_some(value)
        })
        .unwrap_or_else(|| panic!("missing single-line Fluent message `{key}`"))
}

fn write_locale_config(dir: &Path, locale: &str) {
    std::fs::write(dir.join("config.toml"), format!("locale = \"{locale}\"\n")).unwrap();
}

fn help_stdout<I, S>(args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .args(args)
        .output()
        .expect("failed to run zeroclaw --help");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "zeroclaw help failed with status {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status.code()
    );
    stdout.into_owned()
}

#[test]
fn config_dir_flag_drives_cli_locale_space_form() {
    let tmp = tempfile::tempdir().unwrap();
    write_locale_config(tmp.path(), "ja");
    let help = help_stdout([
        OsString::from("--config-dir"),
        tmp.path().as_os_str().to_owned(),
        OsString::from("--help"),
    ]);
    let expected = catalog_message(JA_CLI_FTL, "cli-about");
    assert!(
        help.contains(expected),
        "`--config-dir <ja-dir> --help` must render Japanese; got:\n{help}"
    );
}

#[test]
fn config_dir_equals_flag_after_subcommand_drives_locale() {
    let tmp = tempfile::tempdir().unwrap();
    write_locale_config(tmp.path(), "es");
    let mut config_arg = OsString::from("--config-dir=");
    config_arg.push(tmp.path());
    let help = help_stdout([
        OsString::from("status"),
        config_arg,
        OsString::from("--help"),
    ]);
    let expected = catalog_message(ES_CLI_FTL, "cli-status-about");
    assert!(
        help.contains(expected),
        "`status --config-dir=<es-dir> --help` must render Spanish; got:\n{help}"
    );
}
