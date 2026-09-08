//! The CI gate: every fixture in evals/regression must replay green.

use std::path::PathBuf;
use zeroclaw_config::scattered_types::EvalHarnessConfig;
use zeroclaw_eval::grader::evaluate_expects;
use zeroclaw_eval::{LlmTrace, Mode, RecordedCall, RunRecord, run_case, run_suite};

/// Resolve the gated suite from the shipped config default rather than a second
/// hardcoded literal, so the directory this gate certifies cannot drift away
/// from the directory `zeroclaw eval run` uses by default.
fn regression_dir() -> PathBuf {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    repo_root.join(EvalHarnessConfig::default().suite_dir)
}

#[tokio::test]
async fn regression_suite_replays_green() {
    let report = run_suite(&regression_dir(), Mode::Replay)
        .await
        .expect("regression suite must load and run");
    assert!(
        report.all_passed(),
        "regression suite failed:\n{}",
        report.render_table()
    );
    assert_eq!(report.exit_code(), 0);
}

/// A case earns its place in a required suite only if it fails when the behavior
/// it names changes. `missing_tool_argument_continues_loop` names a dispatch that
/// carries an argument the tool cannot read, so grade its committed expectations
/// against the record a silently repaired dispatch would produce: the same
/// scripted reply and one successful `echo` call, but with the key rewritten to
/// the one the tool reads. Expectations over the scripted response alone stay
/// green on that record; the boundary expectations must not.
#[tokio::test]
async fn missing_argument_fixture_fails_when_the_dispatch_is_silently_repaired() {
    let path = regression_dir().join("missing_tool_argument_continues_loop.json");
    let trace = LlmTrace::from_file(&path).expect("the committed fixture must load");

    let observed = run_case(&trace).await.expect("the fixture must replay");
    let graded = evaluate_expects(&trace.expects, &observed);
    assert!(
        graded.iter().all(|grade| grade.passed),
        "the fixture must pass on the run it actually produces: {graded:?}"
    );

    let repaired = RunRecord {
        final_response: observed.final_response.clone(),
        history: Vec::new(),
        tool_calls: vec![RecordedCall {
            name: "echo".to_string(),
            arguments: r#"{"message":"hello"}"#.to_string(),
            result: "hello".to_string(),
            success: true,
        }],
        input_tokens: observed.input_tokens,
        output_tokens: observed.output_tokens,
    };
    let failures: Vec<String> = evaluate_expects(&trace.expects, &repaired)
        .into_iter()
        .filter(|grade| !grade.passed)
        .map(|grade| grade.check)
        .collect();
    assert!(
        !failures.is_empty(),
        "a repaired dispatch left every expectation green, so the case cannot \
         detect the regression its name claims"
    );
}

#[test]
fn gated_suite_directory_matches_the_configured_default() {
    assert_eq!(
        EvalHarnessConfig::default().suite_dir,
        "evals/regression",
        "the CI gate certifies the configured default suite; if this default moves, \
         move the gated fixtures with it"
    );
    assert!(
        regression_dir().is_dir(),
        "configured default suite directory must exist at {}",
        regression_dir().display()
    );
}
