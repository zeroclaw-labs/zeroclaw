//! CLI-level contract for `zeroclaw eval run --format junit`.
//!
//! The requested output format must survive every accepted flag combination.
//! `--write-baseline` used to return before the JUnit renderer ran, so a CI job
//! that asked for JUnit while refreshing fixtures got a zero-byte report and a
//! zero exit status — the failure mode these tests exist to catch.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn regression_suite() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("evals/regression")
}

/// The number of `*.json` fixtures in the suite, i.e. the expected `tests=`.
fn suite_size(suite: &Path) -> usize {
    std::fs::read_dir(suite)
        .expect("read eval suite")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .count()
}

fn run_eval(config_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("RUST_LOG", "off")
        .args(args)
        .output()
        .expect("run zeroclaw eval")
}

/// Parse the document with a real XML reader and return `(testsuite roots,
/// testcase count, the root's `tests=` attribute)`. Panics with the offending
/// document when it is not well-formed.
fn parse_junit(xml: &str) -> (usize, usize, Option<String>) {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let (mut suites, mut cases, mut tests_attr) = (0usize, 0usize, None);
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "testsuite" {
                    suites += 1;
                    for a in e.attributes() {
                        let a = a.expect("attribute");
                        if a.key.as_ref() == b"tests" {
                            tests_attr =
                                Some(String::from_utf8_lossy(a.value.as_ref()).into_owned());
                        }
                    }
                } else if name == "testcase" {
                    cases += 1;
                }
            }
            Ok(_) => {}
            Err(e) => panic!("JUnit output is not well-formed XML: {e}\n---\n{xml}"),
        }
    }
    (suites, cases, tests_attr)
}

#[test]
fn junit_is_emitted_with_write_baseline() {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let out_dir = tempfile::tempdir().expect("temp out dir");
    let baseline = out_dir.path().join("baseline.json");
    let suite = regression_suite();

    let out = run_eval(
        config_dir.path(),
        &[
            "eval",
            "run",
            "--suite",
            suite.to_str().unwrap(),
            "--format",
            "junit",
            "--write-baseline",
            baseline.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "eval run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The baseline is still written…
    assert!(
        baseline.is_file(),
        "--write-baseline must still persist the baseline file"
    );
    // …and the requested format is not dropped on that path.
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let (suites, cases, tests_attr) = parse_junit(&stdout);
    assert_eq!(suites, 1, "exactly one <testsuite> root, got:\n{stdout}");
    let expected = suite_size(&suite);
    assert_eq!(cases, expected, "one <testcase> per fixture");
    assert_eq!(
        tests_attr.as_deref(),
        Some(expected.to_string().as_str()),
        "tests= must equal the suite size"
    );
}

#[test]
fn junit_emitted_exactly_once_per_run() {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let out_dir = tempfile::tempdir().expect("temp out dir");
    let baseline = out_dir.path().join("baseline.json");
    let suite = regression_suite();
    let suite_arg = suite.to_str().unwrap();

    // 1. Plain run, no baseline at all.
    let plain = run_eval(
        config_dir.path(),
        &["eval", "run", "--suite", suite_arg, "--format", "junit"],
    );
    assert!(plain.status.success());
    let plain_out = String::from_utf8(plain.stdout).expect("utf-8");
    assert_eq!(
        plain_out.matches("<testsuite").count(),
        1,
        "no-comparison path must emit exactly one document"
    );
    assert_eq!(parse_junit(&plain_out).0, 1);

    // 2. The baseline-write path.
    let write = run_eval(
        config_dir.path(),
        &[
            "eval",
            "run",
            "--suite",
            suite_arg,
            "--format",
            "junit",
            "--write-baseline",
            baseline.to_str().unwrap(),
        ],
    );
    assert!(write.status.success());
    let write_out = String::from_utf8(write.stdout).expect("utf-8");
    assert_eq!(
        write_out.matches("<testsuite").count(),
        1,
        "baseline-write path must emit exactly one document"
    );

    // 3. The comparison path, against the baseline just written.
    let compare = run_eval(
        config_dir.path(),
        &[
            "eval",
            "run",
            "--suite",
            suite_arg,
            "--format",
            "junit",
            "--baseline",
            baseline.to_str().unwrap(),
        ],
    );
    assert!(compare.status.success());
    let compare_out = String::from_utf8(compare.stdout).expect("utf-8");
    assert_eq!(
        compare_out.matches("<testsuite").count(),
        1,
        "comparison path must emit exactly one document"
    );
    assert_eq!(parse_junit(&compare_out).0, 1);
}

#[test]
fn junit_write_baseline_does_not_leak_table_output() {
    // The single render site owns the format: asking for JUnit must not also
    // print the human table (or the comparison block) to stdout.
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let out_dir = tempfile::tempdir().expect("temp out dir");
    let baseline = out_dir.path().join("baseline.json");
    let suite = regression_suite();

    let out = run_eval(
        config_dir.path(),
        &[
            "eval",
            "run",
            "--suite",
            suite.to_str().unwrap(),
            "--format",
            "junit",
            "--write-baseline",
            baseline.to_str().unwrap(),
        ],
    );
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(
        stdout.trim_start().starts_with("<?xml"),
        "JUnit stdout must be the XML document only, got:\n{stdout}"
    );
}
