//! The withheld-capability notice must reach an operator under every
//! observability policy.
//!
//! The runtime traces the notice at config load, and that trace reaches a sink
//! only while log persistence is on, so an operator running with
//! `log_persistence = "none"` was told nothing about why an enabled security
//! capability is absent from the registry. `zeroclaw doctor` reads
//! `Config::collect_warnings()` and prints to stdout, which does not involve
//! the log writer, so it delivers the notice whatever the policy is.
//!
//! These run the real binary rather than calling the checker, because the claim
//! is about what an operator sees, and only the process boundary can show that.

use std::path::Path;
use std::process::Command;

use zeroclaw_config::schema::LogPersistence;

/// Every supported value of `observability.log_persistence`, walked through a
/// `match` rather than typed out.
///
/// Two properties the previous `const POLICIES: [&str; 4]` did not have.
/// Adding a variant makes the `match` non-exhaustive, so a new policy cannot be
/// left out here without a compile error, and the wire strings come from
/// `as_wire()`, so they cannot drift from the enum's serde spelling. A string
/// array kept compiling unchanged while covering one policy less.
///
/// What this does not do is put the new variant in the walk on its own. An arm
/// returning `None` compiles and leaves the policy out of the list.
fn policies() -> Vec<&'static str> {
    let mut all = Vec::new();
    let mut next = Some(LogPersistence::None);
    while let Some(policy) = next {
        all.push(policy.as_wire());
        next = match policy {
            LogPersistence::None => Some(LogPersistence::Rolling),
            LogPersistence::Rolling => Some(LogPersistence::Full),
            LogPersistence::Full => Some(LogPersistence::Rotating),
            LogPersistence::Rotating => None,
        };
    }
    all
}

fn write_config(dir: &Path, vi_enabled: bool, log_persistence: &str) {
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "[observability]\nlog_persistence = \"{log_persistence}\"\n\n\
             [verifiable_intent]\nenabled = {vi_enabled}\n"
        ),
    )
    .expect("write config.toml");
}

/// Where the runtime writes its trace, relative to the config dir.
///
/// Shared by the two tests below so they cannot drift apart: a wrong path makes
/// the "absent under `none`" assertion pass for the wrong reason, since a file
/// that is never written anywhere is absent everywhere.
fn trace_path(dir: &Path) -> std::path::PathBuf {
    dir.join("data").join("state").join("runtime-trace.jsonl")
}

/// `RUST_LOG` is deliberately left alone. Sibling component tests set it to
/// `off` to keep stderr quiet, but that also stops the tracing subscriber, and
/// with it the trace file two of these tests measure. Silencing logs here would
/// make the control below pass while proving nothing.
fn doctor_stdout(dir: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", dir)
        .arg("doctor")
        .output()
        .expect("run zeroclaw doctor");
    // The exit status is deliberately not asserted: `doctor` reports on whatever
    // else is unconfigured in a bare temp directory, and this test is about one
    // line of its output rather than the overall verdict.
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn withheld_notice_reaches_the_operator_under_every_log_persistence_policy() {
    for policy in policies() {
        let dir = tempfile::TempDir::new().expect("temp config dir");
        write_config(dir.path(), true, policy);

        let stdout = doctor_stdout(dir.path());

        assert!(
            stdout.contains("vi_verify"),
            "doctor must name the withheld tool under log_persistence = {policy:?}\n\
             stdout:\n{stdout}"
        );
        assert!(
            stdout.contains("verifiable_intent.enabled"),
            "doctor must name the setting that produced the notice under \
             log_persistence = {policy:?}\nstdout:\n{stdout}"
        );
    }
}

/// The traced copy of the notice must carry an explicit category.
///
/// An event with no category is stored as `internal`, and the dashboard Logs
/// view hides that category by default, so an uncategorised posture notice is
/// missing from the history an operator reads even when persistence is on.
/// `hide_internal_drops_internal_category` in `zeroclaw-log` already pins the
/// filter behaviour; this pins the category the notice is actually written
/// with, which is the half that lives in this repository's own call site.
#[test]
fn traced_notice_is_categorised_and_carries_its_warning_code() {
    let dir = tempfile::TempDir::new().expect("temp config dir");
    write_config(dir.path(), true, "rolling");

    let _ = doctor_stdout(dir.path());

    let trace = trace_path(dir.path());
    let body =
        std::fs::read_to_string(&trace).unwrap_or_else(|e| panic!("read {}: {e}", trace.display()));

    let notice = body
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| {
            event["message"]
                .as_str()
                .is_some_and(|m| m.contains("vi_verify is not registered"))
        })
        .unwrap_or_else(|| panic!("the notice is absent from the trace:\n{body}"));

    assert_eq!(
        notice["event"]["category"].as_str(),
        Some("system"),
        "an uncategorised notice stores as `internal`, which the Logs view hides by default"
    );

    // The config surface reports this same fact as a structured warning. The
    // two carry different prose on purpose, so the code is the only thing that
    // identifies them as one fact rather than two lookalike notices.
    assert_eq!(
        notice["attributes"]["code"].as_str(),
        Some("verifiable_intent_tool_withheld"),
        "the traced copy must carry the config warning's code so the two correlate"
    );
    assert_eq!(
        notice["attributes"]["path"].as_str(),
        Some("verifiable_intent.enabled"),
        "and the config key an operator would edit"
    );
}

/// The premise the issue rests on: with persistence off the traced copy has no
/// sink at all, so the structured warning is the only channel left. Re-measured
/// here rather than quoted from the issue, so the claim stays honest if the log
/// writer changes.
///
/// The `rolling` half is the control. Asserting only that a file is absent
/// would also pass if the trace were never written anywhere, which is exactly
/// how an earlier version of this test passed while pointing at the wrong path.
#[test]
fn trace_is_the_channel_that_disappears_and_doctor_is_the_one_that_does_not() {
    let with_sink = tempfile::TempDir::new().expect("temp config dir");
    write_config(with_sink.path(), true, "rolling");
    let rolling_stdout = doctor_stdout(with_sink.path());
    assert!(
        trace_path(with_sink.path()).exists(),
        "control: log_persistence = \"rolling\" must produce a trace file at {}",
        trace_path(with_sink.path()).display()
    );

    let without_sink = tempfile::TempDir::new().expect("temp config dir");
    write_config(without_sink.path(), true, "none");
    let none_stdout = doctor_stdout(without_sink.path());
    assert!(
        !trace_path(without_sink.path()).exists(),
        "log_persistence = \"none\" must leave no trace file, found {}",
        trace_path(without_sink.path()).display()
    );

    for (policy, stdout) in [("rolling", &rolling_stdout), ("none", &none_stdout)] {
        assert!(
            stdout.contains("vi_verify"),
            "doctor reports the notice whether or not a trace sink exists; \
             it did not under {policy}\nstdout:\n{stdout}"
        );
    }
}

/// One config application records the notice once.
///
/// `Config::validate()` traces every entry from `collect_warnings()` and sets no
/// category on them, so each persists as `internal`, which the Logs view hides
/// by default. The runtime separately writes this one notice with an explicit
/// `system` category and its warning code. Any command that loads config a
/// second time while the trace writer is already installed therefore recorded
/// the same fact twice: once visible, once hidden.
///
/// `peripheral add` has exactly that shape at the process boundary. The
/// dispatcher loads config and installs the writer, then the subcommand loads
/// config again. The daemon reload arm does the same thing, and no test here can
/// drive a reload, so this covers the ordering rather than the daemon.
#[test]
fn the_withheld_notice_is_recorded_once_per_config_application() {
    let dir = tempfile::TempDir::new().expect("temp config dir");
    write_config(dir.path(), true, "rolling");

    let out = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", dir.path())
        .args(["peripheral", "add", "rpi-gpio", "native"])
        .output()
        .expect("run zeroclaw peripheral add");
    assert!(
        out.status.success(),
        "peripheral add must succeed, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let trace = trace_path(dir.path());
    let body =
        std::fs::read_to_string(&trace).unwrap_or_else(|e| panic!("read {}: {e}", trace.display()));
    let events: Vec<serde_json::Value> = body
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    let withheld: Vec<&serde_json::Value> = events
        .iter()
        .filter(|event| {
            event["attributes"]["code"].as_str() == Some("verifiable_intent_tool_withheld")
        })
        .collect();

    assert_eq!(
        withheld.len(),
        1,
        "the notice must be recorded once, found {}:\n{}",
        withheld.len(),
        withheld
            .iter()
            .map(|event| format!(
                "  category={:?} message={:?}",
                event["event"]["category"].as_str(),
                event["message"].as_str()
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(
        withheld[0]["event"]["category"].as_str(),
        Some("system"),
        "the surviving record must be the categorised one, not the hidden copy"
    );

    // Positive control over the same trace. Without it, a count of one could be
    // produced by tracing having stopped working, and the assertion above would
    // pass for the wrong reason. This code is raised by the same
    // `collect_warnings()` list on this same config, so it proves the generic
    // path still traces and that the exclusion reaches one code only.
    let control = events.iter().any(|event| {
        event["attributes"]["code"].as_str() == Some("memory_semantic_search_without_embedder")
    });
    assert!(
        control,
        "control: validate() must still trace other warning codes\ntrace:\n{body}"
    );
}

/// Enabling the section through `config patch` records the notice.
///
/// The runtime writes this notice after the initial config load and after a
/// daemon reload. Neither of those runs when an operator flips the setting with
/// `config patch`: the startup call saw the section disabled, so it recorded
/// nothing, and the patch applies the new value afterwards. Once the generic
/// validation trace stopped carrying this code, that left the transition
/// reporting nothing at all, which is the one moment the operator most needs to
/// be told the tool stays absent.
///
/// `schema_version` is written because `config patch` refuses to modify a config
/// it would have to migrate first.
#[test]
fn enabling_the_section_through_config_patch_records_the_notice_once() {
    let dir = tempfile::TempDir::new().expect("temp config dir");
    std::fs::write(
        dir.path().join("config.toml"),
        "schema_version = 3\n\n[observability]\nlog_persistence = \"rolling\"\n\n\
         [verifiable_intent]\nenabled = false\n",
    )
    .expect("write config.toml");
    let patch = dir.path().join("patch.json");
    std::fs::write(
        &patch,
        r#"[{"op":"replace","path":"/verifiable_intent/enabled","value":true}]"#,
    )
    .expect("write patch.json");

    let out = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", dir.path())
        .args(["config", "patch"])
        .arg(&patch)
        .output()
        .expect("run zeroclaw config patch");
    assert!(
        out.status.success(),
        "config patch must succeed, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let trace = trace_path(dir.path());
    let body =
        std::fs::read_to_string(&trace).unwrap_or_else(|e| panic!("read {}: {e}", trace.display()));
    let withheld: Vec<serde_json::Value> = body
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| {
            event["attributes"]["code"].as_str() == Some("verifiable_intent_tool_withheld")
        })
        .collect();

    assert_eq!(
        withheld.len(),
        1,
        "enabling the section must record the notice exactly once, found {}\ntrace:\n{body}",
        withheld.len()
    );
    assert_eq!(
        withheld[0]["event"]["category"].as_str(),
        Some("system"),
        "the record must carry the category the Logs view shows by default"
    );
    assert_eq!(
        withheld[0]["attributes"]["path"].as_str(),
        Some("verifiable_intent.enabled"),
        "the record must name the setting the operator just changed"
    );
}

/// A patch that leaves an already-enabled section alone must not add a second
/// record. This is the control for the test above: without it, reporting the
/// transition could be implemented by recording on every patch, which restores
/// the duplicate the skip in the validation loop exists to remove.
#[test]
fn a_patch_that_does_not_enable_the_section_adds_no_second_record() {
    let dir = tempfile::TempDir::new().expect("temp config dir");
    std::fs::write(
        dir.path().join("config.toml"),
        "schema_version = 3\n\n[observability]\nlog_persistence = \"rolling\"\n\n\
         [verifiable_intent]\nenabled = true\n",
    )
    .expect("write config.toml");
    let patch = dir.path().join("patch.json");
    std::fs::write(
        &patch,
        r#"[{"op":"replace","path":"/observability/log_persistence","value":"rolling"}]"#,
    )
    .expect("write patch.json");

    let out = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", dir.path())
        .args(["config", "patch"])
        .arg(&patch)
        .output()
        .expect("run zeroclaw config patch");
    assert!(out.status.success(), "config patch must succeed");

    let trace = trace_path(dir.path());
    let body =
        std::fs::read_to_string(&trace).unwrap_or_else(|e| panic!("read {}: {e}", trace.display()));
    let count = body
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| {
            event["attributes"]["code"].as_str() == Some("verifiable_intent_tool_withheld")
        })
        .count();

    assert_eq!(
        count, 1,
        "an already-enabled section must keep its single startup record, found {count}\ntrace:\n{body}"
    );
}

/// The negative control for the test above. Without it, a `doctor` that printed
/// the notice unconditionally would pass every assertion there.
#[test]
fn withheld_notice_is_absent_when_the_section_is_not_enabled() {
    for policy in policies() {
        let dir = tempfile::TempDir::new().expect("temp config dir");
        write_config(dir.path(), false, policy);

        let stdout = doctor_stdout(dir.path());

        assert!(
            !stdout.contains("vi_verify"),
            "an operator who has not opted in must not be told about the tool \
             under log_persistence = {policy:?}\nstdout:\n{stdout}"
        );
    }
}
