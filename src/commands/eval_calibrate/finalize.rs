//! Calibration agreement calculation and marker-file emission.

use super::localized_jsonl_error;
use anyhow::{Result, bail};
use chrono::Local;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use zeroclaw_eval::calibration::{
    CALIBRATION_SCHEMA, CalibrationFile, JudgeLabel, MIN_CALIBRATION_RECORDS, agreement,
    calibration_stem, cohens_kappa, load_judge_labels,
};
use zeroclaw_runtime::i18n::{get_required_cli_string, get_required_cli_string_with_args};

const RECOMMENDED_AGREEMENT: f64 = 0.85;

#[derive(Debug, Default, PartialEq, Eq)]
struct AgreementCount {
    agreed: usize,
    total: usize,
}

fn per_rubric_agreement(labels: &[JudgeLabel]) -> BTreeMap<String, AgreementCount> {
    let mut breakdown = BTreeMap::new();
    for label in labels {
        let count = breakdown
            .entry(label.rubric_name.clone())
            .or_insert_with(AgreementCount::default);
        count.total += 1;
        count.agreed += usize::from(label.human_pass == label.judge_pass);
    }
    breakdown
}

fn percent(value: f64) -> String {
    format!("{:.2}", value * 100.0)
}

fn default_output_path(judge_ref: &str) -> PathBuf {
    PathBuf::from("evals/calibration").join(format!("{}.json", calibration_stem(judge_ref)))
}

fn run_with_writer(
    labels_path: &Path,
    out: Option<&Path>,
    min_agreement: Option<f64>,
    labeler: Option<&str>,
    date: &str,
    writer: &mut impl Write,
) -> Result<PathBuf> {
    if let Some(minimum) = min_agreement
        && !(0.0..=1.0).contains(&minimum)
    {
        let value = minimum.to_string();
        bail!(get_required_cli_string_with_args(
            "cli-eval-calibrate-finalize-invalid-min-agreement",
            &[("value", value.as_str())],
        ));
    }

    // `load_judge_labels` owns schema validation and last-wins record-id deduplication.
    let labels = load_judge_labels(labels_path)
        .map_err(|error| localized_jsonl_error(labels_path, &error))?;
    let first_label = match labels.first() {
        Some(label) => label,
        None => {
            let path = labels_path.display().to_string();
            bail!(get_required_cli_string_with_args(
                "cli-eval-calibrate-finalize-no-labels",
                &[("path", path.as_str())],
            ));
        }
    };

    let judge_refs: BTreeSet<&str> = labels
        .iter()
        .map(|label| label.judge_ref.as_str())
        .collect();
    if judge_refs.len() != 1 {
        let refs = judge_refs.into_iter().collect::<Vec<_>>().join(", ");
        bail!(get_required_cli_string_with_args(
            "cli-eval-calibrate-finalize-multiple-judge-refs",
            &[("refs", refs.as_str())],
        ));
    }
    let judge_ref = first_label.judge_ref.as_str();

    if labels.len() < MIN_CALIBRATION_RECORDS {
        let count = labels.len().to_string();
        let remaining = (MIN_CALIBRATION_RECORDS - labels.len()).to_string();
        bail!(get_required_cli_string_with_args(
            "cli-eval-calibrate-finalize-too-few",
            &[("count", count.as_str()), ("remaining", remaining.as_str())],
        ));
    }

    let overall_agreement = agreement(&labels);
    let agreed = labels
        .iter()
        .filter(|label| label.human_pass == label.judge_pass)
        .count();
    let overall_percent = percent(overall_agreement);
    let agreed_text = agreed.to_string();
    let total_text = labels.len().to_string();
    writeln!(
        writer,
        "{}",
        get_required_cli_string_with_args(
            "cli-eval-calibrate-finalize-summary",
            &[
                ("agreement", overall_percent.as_str()),
                ("agreed", agreed_text.as_str()),
                ("total", total_text.as_str()),
            ],
        )
    )?;
    writeln!(
        writer,
        "{}",
        get_required_cli_string("cli-eval-calibrate-finalize-rubrics")
    )?;
    for (rubric, count) in per_rubric_agreement(&labels) {
        let rubric_agreement = percent(count.agreed as f64 / count.total as f64);
        let rubric_agreed = count.agreed.to_string();
        let rubric_total = count.total.to_string();
        writeln!(
            writer,
            "{}",
            get_required_cli_string_with_args(
                "cli-eval-calibrate-finalize-rubric",
                &[
                    ("rubric", rubric.as_str()),
                    ("agreement", rubric_agreement.as_str()),
                    ("agreed", rubric_agreed.as_str()),
                    ("total", rubric_total.as_str()),
                ],
            )
        )?;
    }
    match cohens_kappa(&labels) {
        Some(kappa) => {
            let kappa = format!("{kappa:.4}");
            writeln!(
                writer,
                "{}",
                get_required_cli_string_with_args(
                    "cli-eval-calibrate-finalize-kappa",
                    &[("kappa", kappa.as_str())],
                )
            )?;
        }
        None => writeln!(
            writer,
            "{}",
            get_required_cli_string("cli-eval-calibrate-finalize-kappa-undefined")
        )?,
    }

    if let Some(minimum) = min_agreement
        && overall_agreement < minimum
    {
        let minimum_percent = percent(minimum);
        bail!(get_required_cli_string_with_args(
            "cli-eval-calibrate-finalize-min-agreement-refused",
            &[
                ("agreement", overall_percent.as_str()),
                ("minimum", minimum_percent.as_str()),
            ],
        ));
    }

    if overall_agreement < RECOMMENDED_AGREEMENT {
        writeln!(
            writer,
            "{}",
            get_required_cli_string_with_args(
                "cli-eval-calibrate-finalize-low-agreement",
                &[("agreement", overall_percent.as_str())],
            )
        )?;
    }

    let output_labeler = match labeler {
        Some(labeler) => labeler.to_string(),
        None => labels
            .iter()
            .map(|label| label.labeler.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(","),
    };
    let calibration = CalibrationFile {
        schema: CALIBRATION_SCHEMA.to_string(),
        judge_ref: judge_ref.to_string(),
        labeled_records: labels.len(),
        agreement: overall_agreement,
        labeler: output_labeler,
        date: date.to_string(),
    };
    let output_path = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_output_path(judge_ref));
    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut contents = serde_json::to_vec_pretty(&calibration)?;
    contents.push(b'\n');
    std::fs::write(&output_path, contents)?;

    let output_path_text = output_path.display().to_string();
    writeln!(
        writer,
        "{}",
        get_required_cli_string_with_args(
            "cli-eval-calibrate-finalize-wrote",
            &[
                ("judge_ref", judge_ref),
                ("path", output_path_text.as_str()),
                ("count", total_text.as_str()),
            ],
        )
    )?;
    writeln!(
        writer,
        "{}",
        get_required_cli_string("cli-eval-calibrate-finalize-hint")
    )?;

    Ok(output_path)
}

/// Finalize human labels into a validated judge calibration marker.
pub fn run(
    labels: PathBuf,
    out: Option<PathBuf>,
    min_agreement: Option<f64>,
    labeler: Option<String>,
) -> Result<()> {
    let date = Local::now().format("%Y-%m-%d").to_string();
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    run_with_writer(
        &labels,
        out.as_deref(),
        min_agreement,
        labeler.as_deref(),
        &date,
        &mut writer,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tempfile::tempdir;
    use zeroclaw_eval::calibration::{JUDGE_LABEL_SCHEMA, append_judge_labels};

    fn label(
        index: usize,
        rubric_name: &str,
        human_pass: bool,
        judge_pass: bool,
        labeler: &str,
        judge_ref: &str,
    ) -> JudgeLabel {
        JudgeLabel {
            schema: JUDGE_LABEL_SCHEMA.to_string(),
            record_id: format!("record-{index}"),
            judge_ref: judge_ref.to_string(),
            rubric_name: rubric_name.to_string(),
            human_pass,
            judge_pass,
            score: if judge_pass { 0.9 } else { 0.1 },
            labeler: labeler.to_string(),
            date: "2026-07-20".to_string(),
        }
    }

    fn labels_with_agreement(total: usize, agreed: usize) -> Vec<JudgeLabel> {
        (0..total)
            .map(|index| {
                let judge_pass = index % 2 == 0;
                label(
                    index,
                    "accuracy",
                    if index < agreed {
                        judge_pass
                    } else {
                        !judge_pass
                    },
                    judge_pass,
                    "Zoe",
                    "provider/model",
                )
            })
            .collect()
    }

    fn write_labels(path: &Path, labels: &[JudgeLabel]) {
        append_judge_labels(path, labels).expect("test labels should be writable");
    }

    #[test]
    fn emitted_calibration_has_exact_six_key_schema() {
        let temp = tempdir().expect("temporary directory should be created");
        let labels_path = temp.path().join("labels.jsonl");
        let output_path = temp.path().join("calibration.json");
        write_labels(&labels_path, &labels_with_agreement(50, 40));

        run_with_writer(
            &labels_path,
            Some(&output_path),
            None,
            Some("Override"),
            "2026-07-21",
            &mut Vec::new(),
        )
        .expect("valid labels should finalize");

        let value: Value = serde_json::from_slice(
            &std::fs::read(&output_path).expect("calibration file should be readable"),
        )
        .expect("calibration file should contain JSON");
        let keys = value
            .as_object()
            .expect("calibration should be a JSON object")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            keys,
            BTreeSet::from([
                "agreement",
                "date",
                "judge_ref",
                "labeled_records",
                "labeler",
                "schema",
            ])
        );
        assert_eq!(value["agreement"], 0.8);
        assert_eq!(value["labeled_records"], 50);
        assert_eq!(value["labeler"], "Override");
        assert_eq!(value["date"], "2026-07-21");
        assert!(value.get("kappa").is_none());
        assert!(value.get("rubrics").is_none());
    }

    #[test]
    fn fewer_than_fifty_labels_reports_remaining_count() {
        let temp = tempdir().expect("temporary directory should be created");
        let labels_path = temp.path().join("labels.jsonl");
        let output_path = temp.path().join("calibration.json");
        write_labels(&labels_path, &labels_with_agreement(49, 49));

        let error = run_with_writer(
            &labels_path,
            Some(&output_path),
            None,
            None,
            "2026-07-21",
            &mut Vec::new(),
        )
        .expect_err("49 labels must not finalize");

        assert!(
            error.to_string().contains("found 49 (1 more needed)"),
            "unexpected error: {error}"
        );
        assert!(!output_path.exists());
    }

    #[test]
    fn min_agreement_refuses_without_emitting() {
        let temp = tempdir().expect("temporary directory should be created");
        let labels_path = temp.path().join("labels.jsonl");
        let output_path = temp.path().join("calibration.json");
        write_labels(&labels_path, &labels_with_agreement(50, 40));
        let mut terminal = Vec::new();

        let error = run_with_writer(
            &labels_path,
            Some(&output_path),
            Some(0.9),
            None,
            "2026-07-21",
            &mut terminal,
        )
        .expect_err("agreement below the requested minimum must be refused");

        assert!(
            error
                .to_string()
                .contains("agreement 80.00% is below --min-agreement 90.00%"),
            "unexpected error: {error}"
        );
        assert!(!output_path.exists());
        assert!(
            String::from_utf8(terminal)
                .expect("output should be UTF-8")
                .contains("Overall agreement: 80.00% (40/50)")
        );
    }

    #[test]
    fn permissive_minimum_still_warns_below_recommended_floor() {
        let temp = tempdir().expect("temporary directory should be created");
        let labels_path = temp.path().join("labels.jsonl");
        let output_path = temp.path().join("calibration.json");
        write_labels(&labels_path, &labels_with_agreement(50, 40));
        let mut terminal = Vec::new();

        run_with_writer(
            &labels_path,
            Some(&output_path),
            Some(0.7),
            None,
            "2026-07-21",
            &mut terminal,
        )
        .expect("agreement above the requested minimum should emit");

        let terminal = String::from_utf8(terminal).expect("output should be UTF-8");
        assert!(output_path.exists());
        assert!(terminal.contains("below the recommended 85%"));
        assert!(!terminal.contains("--min-agreement was not set"));
    }

    #[test]
    fn joins_distinct_labelers_in_sorted_order() {
        let temp = tempdir().expect("temporary directory should be created");
        let labels_path = temp.path().join("labels.jsonl");
        let output_path = temp.path().join("calibration.json");
        let mut labels = labels_with_agreement(50, 50);
        for (index, label) in labels.iter_mut().enumerate() {
            label.labeler = if index % 3 == 0 { "Zoe" } else { "Amy" }.to_string();
        }
        write_labels(&labels_path, &labels);

        run_with_writer(
            &labels_path,
            Some(&output_path),
            None,
            None,
            "2026-07-21",
            &mut Vec::new(),
        )
        .expect("valid labels should finalize");

        let calibration: CalibrationFile = serde_json::from_slice(
            &std::fs::read(output_path).expect("calibration file should be readable"),
        )
        .expect("calibration file should contain JSON");
        assert_eq!(calibration.labeler, "Amy,Zoe");
    }

    #[test]
    fn reports_undefined_kappa_when_both_raters_use_one_class() {
        let temp = tempdir().expect("temporary directory should be created");
        let labels_path = temp.path().join("labels.jsonl");
        let output_path = temp.path().join("calibration.json");
        let labels = (0..50)
            .map(|index| label(index, "accuracy", true, true, "Labeler", "provider/model"))
            .collect::<Vec<_>>();
        write_labels(&labels_path, &labels);
        let mut terminal = Vec::new();

        run_with_writer(
            &labels_path,
            Some(&output_path),
            None,
            None,
            "2026-07-21",
            &mut terminal,
        )
        .expect("one-class labels still produce a calibration file");

        let terminal = String::from_utf8(terminal).expect("output should be UTF-8");
        assert!(terminal.contains("Cohen's kappa: undefined"));
    }

    #[test]
    fn reports_sorted_per_rubric_breakdown_math() {
        let temp = tempdir().expect("temporary directory should be created");
        let labels_path = temp.path().join("labels.jsonl");
        let output_path = temp.path().join("calibration.json");
        let mut labels = Vec::new();
        for index in 0..25 {
            let judge_pass = index % 2 == 0;
            labels.push(label(
                index,
                "alpha",
                if index < 20 { judge_pass } else { !judge_pass },
                judge_pass,
                "Labeler",
                "provider/model",
            ));
        }
        for index in 25..50 {
            let rubric_index = index - 25;
            let judge_pass = index % 2 == 0;
            labels.push(label(
                index,
                "beta",
                if rubric_index < 15 {
                    judge_pass
                } else {
                    !judge_pass
                },
                judge_pass,
                "Labeler",
                "provider/model",
            ));
        }
        write_labels(&labels_path, &labels);
        let mut terminal = Vec::new();

        run_with_writer(
            &labels_path,
            Some(&output_path),
            None,
            None,
            "2026-07-21",
            &mut terminal,
        )
        .expect("valid labels should finalize");

        let terminal = String::from_utf8(terminal).expect("output should be UTF-8");
        assert!(terminal.contains("Overall agreement: 70.00% (35/50)"));
        let alpha = terminal
            .find("alpha: 80.00% (20/25)")
            .expect("alpha breakdown should be reported");
        let beta = terminal
            .find("beta: 60.00% (15/25)")
            .expect("beta breakdown should be reported");
        assert!(alpha < beta, "rubrics should be sorted by name");
        assert!(terminal.contains("Cohen's kappa:"));
        assert!(terminal.contains("below the recommended 85%"));
    }

    #[test]
    fn duplicate_record_ids_use_the_last_label() {
        let temp = tempdir().expect("temporary directory should be created");
        let labels_path = temp.path().join("labels.jsonl");
        let output_path = temp.path().join("calibration.json");
        let mut labels = labels_with_agreement(50, 50);
        let mut replacement = labels[0].clone();
        replacement.human_pass = !replacement.judge_pass;
        labels.push(replacement);
        write_labels(&labels_path, &labels);

        run_with_writer(
            &labels_path,
            Some(&output_path),
            None,
            None,
            "2026-07-21",
            &mut Vec::new(),
        )
        .expect("last-wins labels should finalize");

        let calibration: CalibrationFile = serde_json::from_slice(
            &std::fs::read(output_path).expect("calibration file should be readable"),
        )
        .expect("calibration file should contain JSON");
        assert_eq!(calibration.labeled_records, 50);
        assert_eq!(calibration.agreement, 49.0 / 50.0);
    }

    #[test]
    fn mixed_judge_refs_are_rejected_in_sorted_order() {
        let temp = tempdir().expect("temporary directory should be created");
        let labels_path = temp.path().join("labels.jsonl");
        let output_path = temp.path().join("calibration.json");
        let mut labels = labels_with_agreement(50, 50);
        labels[0].judge_ref = "z-ref".to_string();
        labels[1].judge_ref = "a-ref".to_string();
        write_labels(&labels_path, &labels);

        let error = run_with_writer(
            &labels_path,
            Some(&output_path),
            None,
            None,
            "2026-07-21",
            &mut Vec::new(),
        )
        .expect_err("mixed judge refs must be rejected");

        assert!(
            error.to_string().contains("a-ref, provider/model, z-ref"),
            "unexpected error: {error}"
        );
        assert!(!output_path.exists());
    }
}
