//! Blind human labeling flow for LLM-judge calibration.

use super::localized_jsonl_error;
use anyhow::{Result, bail};
use std::collections::{BTreeSet, HashSet};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use zeroclaw_eval::calibration::{
    JUDGE_LABEL_SCHEMA, JudgeLabel, JudgeRunRecord, append_judge_labels, calibration_stem,
    load_judge_labels, load_judge_records,
};
use zeroclaw_runtime::i18n::{get_required_cli_string, get_required_cli_string_with_args};

const JUDGE_RUNS_FILENAME: &str = "judge-runs.jsonl";

#[derive(Debug, PartialEq, Eq)]
struct LabelingOutcome {
    written: usize,
    pending: usize,
    quit: bool,
}

struct LabelingSession<'a> {
    records: &'a [JudgeRunRecord],
    completed_ids: &'a HashSet<String>,
    labeler: &'a str,
    date: &'a str,
    labels_path: &'a str,
}

/// Label judge-run records, resuming an append-only labels file when present.
pub fn run(
    records: PathBuf,
    labels: Option<PathBuf>,
    labeler: Option<String>,
    judge_ref: Option<String>,
) -> Result<()> {
    let (records_path, records) = load_records(&records)?;
    if records.is_empty() {
        let path = records_path.display().to_string();
        bail!(
            "{}",
            tr_args("cli-eval-calibrate-label-no-records", &[("path", &path)])
        );
    }

    let records_display = records_path.display().to_string();
    let judge_ref = select_judge_ref(&records, judge_ref.as_deref(), &records_display)?;
    let records = records
        .into_iter()
        .filter(|record| record.judge_ref == judge_ref)
        .collect::<Vec<_>>();
    let expected_prompt_hash = zeroclaw_eval::grader::judge_prompt_contract_hash();
    let found_prompt_hashes = records
        .iter()
        .map(|record| record.prompt_hash.as_str())
        .collect::<BTreeSet<_>>();
    if !found_prompt_hashes.contains(expected_prompt_hash.as_str()) {
        let found = found_prompt_hashes
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        bail!(tr_args(
            "cli-eval-calibrate-label-stale-prompt-records",
            &[("expected", &expected_prompt_hash), ("found", &found)],
        ));
    }
    let stale_records = records
        .iter()
        .filter(|record| record.prompt_hash != expected_prompt_hash)
        .count();
    if stale_records > 0 {
        let count = stale_records.to_string();
        println!(
            "{}",
            tr_args(
                "cli-eval-calibrate-label-skipped-stale-records",
                &[("count", &count)],
            )
        );
    }
    let records = records
        .into_iter()
        .filter(|record| record.prompt_hash == expected_prompt_hash)
        .collect::<Vec<_>>();
    let labels_path =
        labels.unwrap_or_else(|| default_labels_path(&judge_ref, &expected_prompt_hash));
    let completed_ids = load_completed_ids(&labels_path, &judge_ref, &expected_prompt_hash)?;
    let completed = records
        .iter()
        .filter(|record| completed_ids.contains(&record.id))
        .count();
    let pending = records.len() - completed;

    let records_count = records.len().to_string();
    let completed_count = completed.to_string();
    let pending_count = pending.to_string();
    let mut stdout = io::stdout().lock();
    writeln!(
        stdout,
        "{}",
        tr_args(
            "cli-eval-calibrate-label-loaded",
            &[
                ("records", &records_count),
                ("judge_ref", &judge_ref),
                ("completed", &completed_count),
                ("pending", &pending_count),
            ],
        )
    )?;

    if pending == 0 {
        writeln!(
            stdout,
            "{}",
            tr_args(
                "cli-eval-calibrate-label-nothing-pending",
                &[("records", &records_count), ("judge_ref", &judge_ref)],
            )
        )?;
        return Ok(());
    }

    let labeler = resolve_labeler(labeler)?;
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let labels_display = labels_path.display().to_string();
    run_labeling_loop(
        LabelingSession {
            records: &records,
            completed_ids: &completed_ids,
            labeler: &labeler,
            date: &date,
            labels_path: &labels_display,
        },
        &mut stdin,
        &mut stdout,
        |label| {
            append_judge_labels(&labels_path, std::slice::from_ref(label))
                .map_err(|error| localized_jsonl_error(&labels_path, &error))?;
            Ok(())
        },
    )?;
    Ok(())
}

fn tr(key: &str) -> String {
    get_required_cli_string(key)
}

fn tr_args(key: &str, args: &[(&str, &str)]) -> String {
    get_required_cli_string_with_args(key, args)
}

fn load_records(records_arg: &Path) -> Result<(PathBuf, Vec<JudgeRunRecord>)> {
    let records_path = if records_arg.is_dir() {
        records_arg.join(JUDGE_RUNS_FILENAME)
    } else {
        records_arg.to_path_buf()
    };
    if !records_path.is_file() {
        let path = records_path.display().to_string();
        bail!(
            "{}",
            tr_args(
                "cli-eval-calibrate-label-records-not-found",
                &[("path", &path)],
            )
        );
    }
    let records = load_judge_records(&records_path)
        .map_err(|error| localized_jsonl_error(&records_path, &error))?;
    Ok((records_path, records))
}

fn available_judge_refs(records: &[JudgeRunRecord]) -> BTreeSet<&str> {
    records
        .iter()
        .map(|record| record.judge_ref.as_str())
        .collect()
}

fn select_judge_ref(
    records: &[JudgeRunRecord],
    requested: Option<&str>,
    records_path: &str,
) -> Result<String> {
    let refs = available_judge_refs(records);
    let refs_display = refs.iter().copied().collect::<Vec<_>>().join(", ");
    if let Some(requested) = requested {
        if refs.contains(requested) {
            return Ok(requested.to_string());
        }
        bail!(
            "{}",
            tr_args(
                "cli-eval-calibrate-label-judge-ref-not-found",
                &[("judge_ref", requested), ("refs", &refs_display)],
            )
        );
    }
    if refs.len() > 1 {
        bail!(
            "{}",
            tr_args(
                "cli-eval-calibrate-label-multiple-judge-refs",
                &[("refs", &refs_display)],
            )
        );
    }
    refs.iter()
        .next()
        .map(|value| (*value).to_string())
        .ok_or_else(|| {
            anyhow::Error::msg(tr_args(
                "cli-eval-calibrate-label-no-records",
                &[("path", records_path)],
            ))
        })
}

fn default_labels_path(judge_ref: &str, prompt_hash: &str) -> PathBuf {
    Path::new("evals")
        .join("calibration")
        .join("labels")
        .join(format!(
            "{}-{}.jsonl",
            calibration_stem(judge_ref),
            &prompt_hash[..12]
        ))
}

fn load_completed_ids(
    labels_path: &Path,
    judge_ref: &str,
    prompt_hash: &str,
) -> Result<HashSet<String>> {
    if !labels_path.exists() {
        return Ok(HashSet::new());
    }
    let labels = load_judge_labels(labels_path)
        .map_err(|error| localized_jsonl_error(labels_path, &error))?;
    let mismatched = labels
        .iter()
        .any(|label| label.judge_ref != judge_ref || label.prompt_hash != prompt_hash);
    if mismatched {
        let path = labels_path.display().to_string();
        bail!(tr_args(
            "cli-eval-calibrate-label-existing-labels-mismatch",
            &[("path", &path), ("judge_ref", judge_ref)],
        ));
    }
    Ok(labels.into_iter().map(|label| label.record_id).collect())
}

fn resolve_labeler(explicit: Option<String>) -> Result<String> {
    resolve_labeler_with(explicit, || {
        Command::new("git")
            .args(["config", "--get", "user.name"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
    })
}

fn resolve_labeler_with(
    explicit: Option<String>,
    git_name: impl FnOnce() -> Option<String>,
) -> Result<String> {
    if let Some(labeler) = explicit
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(labeler);
    }

    let configured = git_name()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    configured.ok_or_else(|| anyhow::Error::msg(tr("cli-eval-calibrate-label-labeler-required")))
}

fn run_labeling_loop<R, W, F>(
    session: LabelingSession<'_>,
    input: &mut R,
    output: &mut W,
    mut save_label: F,
) -> Result<LabelingOutcome>
where
    R: BufRead,
    W: Write,
    F: FnMut(&JudgeLabel) -> Result<()>,
{
    let pending_records = session
        .records
        .iter()
        .filter(|record| !session.completed_ids.contains(&record.id))
        .collect::<Vec<_>>();
    let mut written = 0;
    let mut transcript_shown = true;

    for (index, record) in pending_records.iter().enumerate() {
        render_record(
            output,
            record,
            index + 1,
            pending_records.len(),
            transcript_shown,
        )?;

        loop {
            writeln!(output, "{}", tr("cli-eval-calibrate-label-prompt"))?;
            output.flush()?;
            let mut answer = String::new();
            if input.read_line(&mut answer)? == 0 {
                let remaining = pending_records.len() - written;
                write_outcome(output, true, written, remaining)?;
                return Ok(LabelingOutcome {
                    written,
                    pending: remaining,
                    quit: true,
                });
            }

            match answer.trim().to_ascii_lowercase().as_str() {
                "p" | "f" => {
                    let human_pass = answer.trim().eq_ignore_ascii_case("p");
                    let label = JudgeLabel {
                        schema: JUDGE_LABEL_SCHEMA.to_string(),
                        record_id: record.id.clone(),
                        judge_ref: record.judge_ref.clone(),
                        prompt_hash: record.prompt_hash.clone(),
                        rubric_name: record.rubric_name.clone(),
                        rubric_hash: record.rubric_hash.clone(),
                        human_pass,
                        judge_pass: record.judge_pass,
                        score: record.score,
                        labeler: session.labeler.to_string(),
                        date: session.date.to_string(),
                    };
                    save_label(&label)?;
                    written += 1;
                    reveal_judge_result(output, record)?;
                    let human_verdict = if human_pass {
                        tr("cli-eval-calibrate-label-human-pass")
                    } else {
                        tr("cli-eval-calibrate-label-human-fail")
                    };
                    writeln!(
                        output,
                        "{}",
                        tr_args(
                            "cli-eval-calibrate-label-saved",
                            &[("verdict", &human_verdict), ("path", session.labels_path),],
                        )
                    )?;
                    break;
                }
                "u" => {
                    writeln!(output, "{}", tr("cli-eval-calibrate-label-skipped"))?;
                    break;
                }
                "q" => {
                    let remaining = pending_records.len() - written;
                    write_outcome(output, true, written, remaining)?;
                    return Ok(LabelingOutcome {
                        written,
                        pending: remaining,
                        quit: true,
                    });
                }
                "t" => {
                    transcript_shown = !transcript_shown;
                    if transcript_shown {
                        writeln!(
                            output,
                            "{}",
                            tr("cli-eval-calibrate-label-transcript-shown")
                        )?;
                        write_transcript(output, record)?;
                    } else {
                        writeln!(
                            output,
                            "{}",
                            tr("cli-eval-calibrate-label-transcript-now-hidden")
                        )?;
                    }
                }
                _ => {
                    writeln!(output, "{}", tr("cli-eval-calibrate-label-invalid-choice"))?;
                }
            }
        }
    }

    let remaining = pending_records.len() - written;
    write_outcome(output, false, written, remaining)?;
    Ok(LabelingOutcome {
        written,
        pending: remaining,
        quit: false,
    })
}

fn render_record(
    output: &mut impl Write,
    record: &JudgeRunRecord,
    current: usize,
    total: usize,
    transcript_shown: bool,
) -> Result<()> {
    let current = current.to_string();
    let total = total.to_string();
    writeln!(output)?;
    writeln!(
        output,
        "{}",
        tr_args(
            "cli-eval-calibrate-label-record",
            &[("current", &current), ("total", &total)],
        )
    )?;
    if transcript_shown {
        write_transcript(output, record)?;
    } else {
        writeln!(
            output,
            "{}",
            tr("cli-eval-calibrate-label-transcript-hidden")
        )?;
    }
    writeln!(
        output,
        "{}",
        tr_args(
            "cli-eval-calibrate-label-rubric-name",
            &[("rubric", &record.rubric_name)],
        )
    )?;
    writeln!(
        output,
        "{}",
        tr_args(
            "cli-eval-calibrate-label-rubric-text",
            &[("text", &record.rubric_text)],
        )
    )?;
    writeln!(output, "{}", tr("cli-eval-calibrate-label-final-response"))?;
    writeln!(output, "{}", record.final_response)?;
    Ok(())
}

fn write_transcript(output: &mut impl Write, record: &JudgeRunRecord) -> Result<()> {
    writeln!(output, "{}", tr("cli-eval-calibrate-label-task-turns"))?;
    for (index, turn) in record.task_turns.iter().enumerate() {
        let index = (index + 1).to_string();
        writeln!(
            output,
            "{}",
            tr_args(
                "cli-eval-calibrate-label-task-turn",
                &[("index", &index), ("turn", turn)],
            )
        )?;
    }
    if let Some(transcript) = &record.transcript {
        writeln!(
            output,
            "{}",
            tr("cli-eval-calibrate-label-agent-transcript")
        )?;
        writeln!(output, "{transcript}")?;
    }
    Ok(())
}

fn reveal_judge_result(output: &mut impl Write, record: &JudgeRunRecord) -> Result<()> {
    let verdict = if record.judge_pass {
        tr("cli-eval-calibrate-label-judge-pass")
    } else {
        tr("cli-eval-calibrate-label-judge-fail")
    };
    let score = record.score.to_string();
    writeln!(
        output,
        "{}",
        tr_args(
            "cli-eval-calibrate-label-reveal",
            &[
                ("verdict", &verdict),
                ("score", &score),
                ("reason", &record.reason),
            ],
        )
    )?;
    Ok(())
}

fn write_outcome(
    output: &mut impl Write,
    quit: bool,
    written: usize,
    pending: usize,
) -> Result<()> {
    let written = written.to_string();
    let pending = pending.to_string();
    let key = if quit {
        "cli-eval-calibrate-label-quit"
    } else {
        "cli-eval-calibrate-label-complete"
    };
    writeln!(
        output,
        "{}",
        tr_args(key, &[("written", &written), ("pending", &pending)])
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::io::Cursor;

    fn record(id: &str, judge_ref: &str, score: f64, judge_pass: bool) -> JudgeRunRecord {
        let rubric_name = "quality";
        let rubric_text = "The response is correct and complete.";
        let record = JudgeRunRecord::new(zeroclaw_eval::calibration::JudgeRunRecordInput {
            judge_ref: judge_ref.to_string(),
            prompt_hash: zeroclaw_eval::calibration::judge_prompt_hash("system", "contract"),
            case_id: format!("case-{id}"),
            case_hash: format!("{:x}", Sha256::digest(id.as_bytes())),
            rubric_name: rubric_name.to_string(),
            rubric_text: rubric_text.to_string(),
            rubric_hash: zeroclaw_eval::calibration::rubric_hash(
                rubric_name,
                rubric_text,
                0.5,
                false,
            ),
            threshold: 0.5,
            include_transcript: false,
            task_turns: vec!["system turn".to_string(), "user turn".to_string()],
            transcript: None,
            final_response: format!("candidate response {id}"),
            score,
            reason: "HIDDEN_REASON_TOKEN".to_string(),
        });
        assert_eq!(record.judge_pass, judge_pass);
        record
    }

    fn drive(
        records: &[JudgeRunRecord],
        completed: &HashSet<String>,
        answers: &str,
    ) -> (LabelingOutcome, Vec<JudgeLabel>, String) {
        let mut input = Cursor::new(answers.as_bytes());
        let mut output = Vec::new();
        let mut labels = Vec::new();
        let outcome = run_labeling_loop(
            LabelingSession {
                records,
                completed_ids: completed,
                labeler: "human",
                date: "2026-07-21",
                labels_path: "labels.jsonl",
            },
            &mut input,
            &mut output,
            |label| {
                labels.push(label.clone());
                Ok(())
            },
        )
        .unwrap();
        (outcome, labels, String::from_utf8(output).unwrap())
    }

    #[test]
    fn pass_and_fail_append_labels_and_reveal_after_each_answer() {
        let records = vec![
            record("pass", "provider:model", 0.91, true),
            record("fail", "provider:model", 0.12, false),
        ];
        let (outcome, labels, output) = drive(&records, &HashSet::new(), "p\nf\n");

        assert_eq!(
            outcome,
            LabelingOutcome {
                written: 2,
                pending: 0,
                quit: false,
            }
        );
        assert_eq!(labels.len(), 2);
        assert!(labels[0].human_pass);
        assert!(!labels[1].human_pass);
        assert_eq!(labels[0].record_id, records[0].id);
        assert_eq!(labels[1].record_id, records[1].id);
        assert_eq!(output.matches("Judge verdict:").count(), 2);
        assert!(output.contains("score 0.91"));
        assert!(output.contains("score 0.12"));
    }

    #[test]
    fn transcript_evidence_is_visible_before_the_blind_verdict() {
        let mut item = record("transcript", "provider:model", 0.91, true);
        item.include_transcript = true;
        item.transcript = Some(
            r#"[{"type":"Chat","data":{"role":"user","content":"agent evidence"}}]"#.to_string(),
        );
        let (outcome, labels, output) = drive(&[item], &HashSet::new(), "p\n");

        assert_eq!(outcome.written, 1);
        assert_eq!(labels.len(), 1);
        assert!(output.contains("Agent conversation transcript:"));
        assert!(output.contains("agent evidence"));
        assert!(
            output.find("agent evidence").unwrap() < output.find("Judge verdict:").unwrap(),
            "transcript must be shown before the judge result is revealed"
        );
    }

    #[test]
    fn skip_writes_nothing_and_leaves_record_pending() {
        let records = vec![record("skip", "provider:model", 0.91, true)];
        let (outcome, labels, output) = drive(&records, &HashSet::new(), "u\n");

        assert_eq!(
            outcome,
            LabelingOutcome {
                written: 0,
                pending: 1,
                quit: false,
            }
        );
        assert!(labels.is_empty());
        assert!(output.contains("remains pending"));
        assert!(!output.contains("Judge verdict:"));
        assert!(!output.contains("HIDDEN_REASON_TOKEN"));
    }

    #[test]
    fn quit_writes_nothing_and_stops_before_later_records() {
        let records = vec![
            record("first", "provider:model", 0.91, true),
            record("second", "provider:model", 0.12, false),
        ];
        let (outcome, labels, output) = drive(&records, &HashSet::new(), "q\n");

        assert_eq!(
            outcome,
            LabelingOutcome {
                written: 0,
                pending: 2,
                quit: true,
            }
        );
        assert!(labels.is_empty());
        assert!(!output.contains("candidate response second"));
        assert!(!output.contains("Judge verdict:"));
    }

    #[test]
    fn resume_skips_ids_already_labeled() {
        let records = vec![
            record("done", "provider:model", 0.91, true),
            record("pending", "provider:model", 0.12, false),
        ];
        let completed = HashSet::from([records[0].id.clone()]);
        let (outcome, labels, output) = drive(&records, &completed, "p\n");

        assert_eq!(outcome.written, 1);
        assert_eq!(outcome.pending, 0);
        assert_eq!(labels[0].record_id, records[1].id);
        assert!(!output.contains("candidate response done"));
        assert!(output.contains("candidate response pending"));
    }

    #[test]
    fn transcript_toggle_hides_and_restores_task_turns() {
        let records = vec![record("toggle", "provider:model", 0.91, true)];
        let (_, _, output) = drive(&records, &HashSet::new(), "t\nt\nu\n");

        assert!(output.contains("Task transcript hidden."));
        assert!(output.contains("Task transcript shown."));
        assert!(output.matches("system turn").count() >= 2);
    }

    #[test]
    fn output_before_answer_is_blind_to_judge_result() {
        let records = vec![record("blind", "provider:model", 0.914_159, true)];
        let (_, labels, output) = drive(&records, &HashSet::new(), "t\ninvalid\nq\n");
        let lowercase = output.to_ascii_lowercase();

        assert!(labels.is_empty());
        assert!(output.contains("Task transcript hidden."));
        assert!(output.contains("Enter p, f, u, t, or q."));
        assert!(!lowercase.contains("score"));
        assert!(!lowercase.contains("verdict"));
        assert!(!lowercase.contains("reason"));
        assert!(!output.contains("0.914159"));
        assert!(!output.contains("HIDDEN_REASON_TOKEN"));
    }

    #[test]
    fn mixed_judge_refs_require_selection_and_list_available_refs() {
        let records = vec![
            record("a", "zeta:model", 0.9, true),
            record("b", "alpha:model", 0.2, false),
        ];

        let error = select_judge_ref(&records, None, "records.jsonl")
            .unwrap_err()
            .to_string();
        assert!(error.contains("alpha:model, zeta:model"));
        assert!(error.contains("--judge-ref"));
        assert_eq!(
            select_judge_ref(&records, Some("alpha:model"), "records.jsonl").unwrap(),
            "alpha:model"
        );
    }

    #[test]
    fn labeler_prefers_argument_then_falls_back_to_git_config() {
        assert_eq!(
            resolve_labeler_with(Some("  explicit  ".to_string()), || {
                Some("git name".to_string())
            })
            .unwrap(),
            "explicit"
        );
        assert_eq!(
            resolve_labeler_with(None, || Some("  git name\n".to_string())).unwrap(),
            "git name"
        );
        assert!(
            resolve_labeler_with(None, || None)
                .unwrap_err()
                .to_string()
                .contains("--labeler")
        );
    }

    #[test]
    fn records_load_from_file_or_directory_and_deduplicate_ids() {
        let directory = tempfile::tempdir().unwrap();
        let records_path = directory.path().join(JUDGE_RUNS_FILENAME);
        let first = record("duplicate", "provider:model", 0.9, true);
        append_records_for_test(&records_path, &[first.clone(), first]);

        let (resolved_from_directory, from_directory) = load_records(directory.path()).unwrap();
        let (resolved_from_file, from_file) = load_records(&records_path).unwrap();
        assert_eq!(resolved_from_directory, records_path);
        assert_eq!(resolved_from_file, records_path);
        assert_eq!(from_directory, from_file);
        assert_eq!(from_directory.len(), 1);
        assert_eq!(
            from_directory[0].final_response,
            "candidate response duplicate"
        );
    }

    fn append_records_for_test(path: &Path, records: &[JudgeRunRecord]) {
        zeroclaw_eval::calibration::append_judge_records(path, records).unwrap();
    }

    #[test]
    fn default_path_uses_canonical_calibration_stem() {
        assert_eq!(
            default_labels_path("provider.example/model:v1", &"a".repeat(64)),
            PathBuf::from("evals/calibration/labels").join(format!(
                "{}-aaaaaaaaaaaa.jsonl",
                calibration_stem("provider.example/model:v1")
            ))
        );
    }

    #[test]
    fn resume_rejects_labels_from_another_prompt_contract() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("labels.jsonl");
        let source = record("source", "provider:model", 0.9, true);
        let label = JudgeLabel {
            schema: JUDGE_LABEL_SCHEMA.to_string(),
            record_id: source.id.clone(),
            judge_ref: source.judge_ref.clone(),
            prompt_hash: source.prompt_hash.clone(),
            rubric_name: source.rubric_name.clone(),
            rubric_hash: source.rubric_hash.clone(),
            human_pass: true,
            judge_pass: source.judge_pass,
            score: source.score,
            labeler: "tester".to_string(),
            date: "2026-07-21".to_string(),
        };
        append_judge_labels(&path, &[label]).unwrap();

        assert!(load_completed_ids(&path, "provider:model", &source.prompt_hash).is_ok());
        let error = load_completed_ids(
            &path,
            "provider:model",
            &zeroclaw_eval::calibration::judge_prompt_hash("changed", "contract"),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("different judge or prompt contract")
        );
    }
}
