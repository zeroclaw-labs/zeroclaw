//! Durable schemas and file helpers for LLM-judge calibration.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::hash::Hash;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Schema tag for one calibratable judge run.
pub const JUDGE_RECORD_SCHEMA: &str = "zeroclaw-eval/judge-record/v1";
/// Schema tag for one blind human label of a judge run.
pub const JUDGE_LABEL_SCHEMA: &str = "zeroclaw-eval/judge-label/v1";
/// Schema tag for the calibration marker consumed by judge gating.
pub const CALIBRATION_SCHEMA: &str = "zeroclaw-eval/calibration/v1";
/// Minimum number of human labels required before judge gating can be enabled.
pub const MIN_CALIBRATION_RECORDS: usize = 50;

/// One parseable, non-unknown LLM-judge result ready for blind labeling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeRunRecord {
    pub schema: String,
    pub id: String,
    pub judge_ref: String,
    pub case_id: String,
    pub case_hash: String,
    pub rubric_name: String,
    pub rubric_text: String,
    pub threshold: f64,
    pub task_turns: Vec<String>,
    pub final_response: String,
    pub score: f64,
    pub judge_pass: bool,
    pub reason: String,
}

impl JudgeRunRecord {
    /// Construct a record and derive its stable id and judge verdict.
    #[must_use]
    pub fn new(input: JudgeRunRecordInput) -> Self {
        let id = judge_record_id(
            &input.judge_ref,
            &input.case_hash,
            &input.rubric_name,
            input.score,
            &input.reason,
        );
        let judge_pass = input.score >= input.threshold;
        Self {
            schema: JUDGE_RECORD_SCHEMA.to_string(),
            id,
            judge_ref: input.judge_ref,
            case_id: input.case_id,
            case_hash: input.case_hash,
            rubric_name: input.rubric_name,
            rubric_text: input.rubric_text,
            threshold: input.threshold,
            task_turns: input.task_turns,
            final_response: input.final_response,
            score: input.score,
            judge_pass,
            reason: input.reason,
        }
    }
}

/// Inputs whose canonical derived fields are owned by [`JudgeRunRecord::new`].
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeRunRecordInput {
    pub judge_ref: String,
    pub case_id: String,
    pub case_hash: String,
    pub rubric_name: String,
    pub rubric_text: String,
    pub threshold: f64,
    pub task_turns: Vec<String>,
    pub final_response: String,
    pub score: f64,
    pub reason: String,
}

/// One blind human verdict paired with the hidden judge verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeLabel {
    pub schema: String,
    pub record_id: String,
    pub judge_ref: String,
    pub rubric_name: String,
    pub human_pass: bool,
    pub judge_pass: bool,
    pub score: f64,
    pub labeler: String,
    pub date: String,
}

/// Validated marker that permits an LLM judge to gate eval results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationFile {
    pub schema: String,
    pub judge_ref: String,
    pub labeled_records: usize,
    pub agreement: f64,
    pub labeler: String,
    pub date: String,
}

/// Derive the stable 16-hex-character id for a judge-run record.
#[must_use]
pub fn judge_record_id(
    judge_ref: &str,
    case_hash: &str,
    rubric_name: &str,
    score: f64,
    reason: &str,
) -> String {
    let canonical = format!("{judge_ref}|{case_hash}|{rubric_name}|{score}|{reason}");
    let digest = Sha256::digest(canonical.as_bytes());
    format!("{digest:x}")[..16].to_string()
}

/// Convert a model-inclusive judge reference into its calibration filename stem.
#[must_use]
pub fn calibration_stem(judge_ref: &str) -> String {
    judge_ref
        .chars()
        .map(|character| match character {
            '/' | '.' | ':' => '_',
            other => other,
        })
        .collect()
}

/// Errors produced while reading or appending calibration JSONL files.
#[derive(Debug)]
pub enum JsonlError {
    Io(io::Error),
    Decode {
        line: usize,
        source: serde_json::Error,
    },
    Encode(serde_json::Error),
    WrongSchema {
        line: usize,
        expected: &'static str,
        found: String,
    },
}

impl fmt::Display for JsonlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "calibration JSONL I/O failed: {error}"),
            Self::Decode { line, source } => {
                write!(
                    formatter,
                    "invalid calibration JSONL at line {line}: {source}"
                )
            }
            Self::Encode(error) => write!(formatter, "failed to encode calibration JSONL: {error}"),
            Self::WrongSchema {
                line,
                expected,
                found,
            } => write!(
                formatter,
                "calibration JSONL line {line} has schema '{found}', expected '{expected}'"
            ),
        }
    }
}

impl std::error::Error for JsonlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Decode { source, .. } | Self::Encode(source) => Some(source),
            Self::WrongSchema { .. } => None,
        }
    }
}

impl From<io::Error> for JsonlError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn read_jsonl<T: DeserializeOwned>(reader: impl BufRead) -> Result<Vec<(usize, T)>, JsonlError> {
    let mut values = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line_number = index + 1;
        let line = line.map_err(JsonlError::Io)?;
        if line.trim().is_empty() {
            continue;
        }
        let value = serde_json::from_str(&line).map_err(|source| JsonlError::Decode {
            line: line_number,
            source,
        })?;
        values.push((line_number, value));
    }
    Ok(values)
}

fn dedup_last<T, K>(values: Vec<T>, key: impl Fn(&T) -> K) -> Vec<T>
where
    K: Eq + Hash,
{
    let mut positions = HashMap::new();
    let mut deduplicated = Vec::new();
    for value in values {
        let value_key = key(&value);
        if let Some(index) = positions.get(&value_key).copied() {
            deduplicated[index] = value;
        } else {
            positions.insert(value_key, deduplicated.len());
            deduplicated.push(value);
        }
    }
    deduplicated
}

/// Read judge-run records from JSONL, validate their schemas, and deduplicate by id.
pub fn read_judge_records(reader: impl BufRead) -> Result<Vec<JudgeRunRecord>, JsonlError> {
    let records = read_jsonl(reader)?
        .into_iter()
        .map(|(line, record): (usize, JudgeRunRecord)| {
            if record.schema == JUDGE_RECORD_SCHEMA {
                Ok(record)
            } else {
                Err(JsonlError::WrongSchema {
                    line,
                    expected: JUDGE_RECORD_SCHEMA,
                    found: record.schema,
                })
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(dedup_last(records, |record| record.id.clone()))
}

/// Open and read judge-run JSONL from a file.
pub fn load_judge_records(path: &Path) -> Result<Vec<JudgeRunRecord>, JsonlError> {
    let file = File::open(path)?;
    read_judge_records(BufReader::new(file))
}

/// Read judge labels from JSONL, validate their schemas, and deduplicate by record id.
pub fn read_judge_labels(reader: impl BufRead) -> Result<Vec<JudgeLabel>, JsonlError> {
    let labels = read_jsonl(reader)?
        .into_iter()
        .map(|(line, label): (usize, JudgeLabel)| {
            if label.schema == JUDGE_LABEL_SCHEMA {
                Ok(label)
            } else {
                Err(JsonlError::WrongSchema {
                    line,
                    expected: JUDGE_LABEL_SCHEMA,
                    found: label.schema,
                })
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(dedup_last(labels, |label| label.record_id.clone()))
}

/// Open and read judge-label JSONL from a file.
pub fn load_judge_labels(path: &Path) -> Result<Vec<JudgeLabel>, JsonlError> {
    let file = File::open(path)?;
    read_judge_labels(BufReader::new(file))
}

fn append_jsonl<T: Serialize>(path: &Path, values: &[T]) -> Result<usize, JsonlError> {
    if values.is_empty() {
        return Ok(0);
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?;
    let needs_separator = if file.metadata()?.len() == 0 {
        false
    } else {
        file.seek(SeekFrom::End(-1))?;
        let mut last_byte = [0];
        file.read_exact(&mut last_byte)?;
        last_byte[0] != b'\n'
    };
    file.seek(SeekFrom::End(0))?;
    let mut writer = BufWriter::new(file);
    if needs_separator {
        writer.write_all(b"\n")?;
    }
    for value in values {
        serde_json::to_writer(&mut writer, value).map_err(JsonlError::Encode)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(values.len())
}

/// Append judge-run records as one JSON object per line.
pub fn append_judge_records(path: &Path, records: &[JudgeRunRecord]) -> Result<usize, JsonlError> {
    append_jsonl(path, records)
}

/// Append judge labels as one JSON object per line.
pub fn append_judge_labels(path: &Path, labels: &[JudgeLabel]) -> Result<usize, JsonlError> {
    append_jsonl(path, labels)
}

/// Fraction of labels on which the human and judge verdicts agree.
#[must_use]
pub fn agreement(labels: &[JudgeLabel]) -> f64 {
    if labels.is_empty() {
        return 0.0;
    }
    let agreed = labels
        .iter()
        .filter(|label| label.human_pass == label.judge_pass)
        .count();
    agreed as f64 / labels.len() as f64
}

/// Cohen's kappa for human and judge pass/fail verdicts.
///
/// Returns `None` when no labels exist or expected agreement is one, because
/// kappa is undefined when both raters assign every item to the same class.
#[must_use]
pub fn cohens_kappa(labels: &[JudgeLabel]) -> Option<f64> {
    if labels.is_empty() {
        return None;
    }
    let total = labels.len() as f64;
    let human_pass = labels.iter().filter(|label| label.human_pass).count() as f64 / total;
    let judge_pass = labels.iter().filter(|label| label.judge_pass).count() as f64 / total;
    let observed = agreement(labels);
    let expected = human_pass * judge_pass + (1.0 - human_pass) * (1.0 - judge_pass);
    let denominator = 1.0 - expected;
    if denominator.abs() < f64::EPSILON {
        return None;
    }
    Some((observed - expected) / denominator)
}

/// Concrete reasons a calibration marker cannot enable judge gating.
#[derive(Debug)]
pub enum CalibrationRejection {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Malformed {
        path: PathBuf,
        source: serde_json::Error,
    },
    WrongSchema {
        expected: &'static str,
        found: String,
    },
    WrongJudgeRef {
        expected: String,
        found: String,
    },
    InsufficientRecords {
        found: usize,
        minimum: usize,
    },
}

impl fmt::Display for CalibrationRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "cannot read {}: {source}", path.display())
            }
            Self::Malformed { path, source } => {
                write!(
                    formatter,
                    "{} is not valid calibration JSON: {source}",
                    path.display()
                )
            }
            Self::WrongSchema { expected, found } => {
                write!(formatter, "schema is '{found}', expected '{expected}'")
            }
            Self::WrongJudgeRef { expected, found } => {
                write!(formatter, "judge_ref is '{found}', expected '{expected}'")
            }
            Self::InsufficientRecords { found, minimum } => write!(
                formatter,
                "labeled_records is {found}, but at least {minimum} are required"
            ),
        }
    }
}

impl std::error::Error for CalibrationRejection {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Malformed { source, .. } => Some(source),
            Self::WrongSchema { .. }
            | Self::WrongJudgeRef { .. }
            | Self::InsufficientRecords { .. } => None,
        }
    }
}

/// Load and validate the calibration marker for one exact judge reference.
pub fn load_calibration(
    path: &Path,
    expected_judge_ref: &str,
) -> Result<CalibrationFile, CalibrationRejection> {
    let contents = std::fs::read_to_string(path).map_err(|source| CalibrationRejection::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let calibration = serde_json::from_str::<CalibrationFile>(&contents).map_err(|source| {
        CalibrationRejection::Malformed {
            path: path.to_path_buf(),
            source,
        }
    })?;
    if calibration.schema != CALIBRATION_SCHEMA {
        return Err(CalibrationRejection::WrongSchema {
            expected: CALIBRATION_SCHEMA,
            found: calibration.schema,
        });
    }
    if calibration.judge_ref != expected_judge_ref {
        return Err(CalibrationRejection::WrongJudgeRef {
            expected: expected_judge_ref.to_string(),
            found: calibration.judge_ref,
        });
    }
    if calibration.labeled_records < MIN_CALIBRATION_RECORDS {
        return Err(CalibrationRejection::InsufficientRecords {
            found: calibration.labeled_records,
            minimum: MIN_CALIBRATION_RECORDS,
        });
    }
    Ok(calibration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn label(id: &str, human_pass: bool, judge_pass: bool) -> JudgeLabel {
        JudgeLabel {
            schema: JUDGE_LABEL_SCHEMA.to_string(),
            record_id: id.to_string(),
            judge_ref: "provider:model".to_string(),
            rubric_name: "quality".to_string(),
            human_pass,
            judge_pass,
            score: if judge_pass { 0.9 } else { 0.1 },
            labeler: "tester".to_string(),
            date: "2026-07-21".to_string(),
        }
    }

    fn record(id: &str, reason: &str) -> JudgeRunRecord {
        JudgeRunRecord {
            schema: JUDGE_RECORD_SCHEMA.to_string(),
            id: id.to_string(),
            judge_ref: "provider:model".to_string(),
            case_id: "case".to_string(),
            case_hash: "hash".to_string(),
            rubric_name: "quality".to_string(),
            rubric_text: "Be correct".to_string(),
            threshold: 0.5,
            task_turns: vec!["question".to_string()],
            final_response: "answer".to_string(),
            score: 0.9,
            judge_pass: true,
            reason: reason.to_string(),
        }
    }

    fn calibration_json(schema: &str, judge_ref: &str, labeled_records: usize) -> String {
        serde_json::json!({
            "schema": schema,
            "judge_ref": judge_ref,
            "labeled_records": labeled_records,
            "agreement": 0.9,
            "labeler": "tester",
            "date": "2026-07-21",
        })
        .to_string()
    }

    fn write_calibration(contents: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        file
    }

    #[test]
    fn record_id_is_stable_and_uses_the_fixed_inputs() {
        let id = judge_record_id(
            "anthropic.sonnet:claude-x",
            "abc123",
            "correctness",
            0.87,
            "well supported",
        );
        assert_eq!(id, "44ea55c537bdf6e1");
        assert_eq!(
            id,
            judge_record_id(
                "anthropic.sonnet:claude-x",
                "abc123",
                "correctness",
                0.87,
                "well supported",
            )
        );
        assert_ne!(
            id,
            judge_record_id(
                "anthropic.sonnet:claude-x",
                "abc123",
                "correctness",
                0.88,
                "well supported",
            )
        );
    }

    #[test]
    fn constructor_owns_id_and_verdict_derivation() {
        let record = JudgeRunRecord::new(JudgeRunRecordInput {
            judge_ref: "provider:model".to_string(),
            case_id: "case".to_string(),
            case_hash: "hash".to_string(),
            rubric_name: "quality".to_string(),
            rubric_text: "Be correct".to_string(),
            threshold: 0.8,
            task_turns: vec!["question".to_string()],
            final_response: "answer".to_string(),
            score: 0.8,
            reason: "reason".to_string(),
        });
        assert_eq!(record.schema, JUDGE_RECORD_SCHEMA);
        assert!(record.judge_pass);
        assert_eq!(record.id.len(), 16);
    }

    #[test]
    fn agreement_and_kappa_match_hand_computation() {
        let labels = vec![
            label("1", true, true),
            label("2", true, true),
            label("3", true, true),
            label("4", true, false),
            label("5", false, true),
            label("6", false, true),
            label("7", false, false),
            label("8", false, false),
            label("9", false, false),
            label("10", false, false),
        ];
        assert!((agreement(&labels) - 0.7).abs() < f64::EPSILON);
        let kappa = cohens_kappa(&labels).expect("both classes make kappa defined");
        assert!((kappa - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn kappa_all_one_class_perfect_agreement_is_undefined() {
        let labels = vec![label("1", true, true), label("2", true, true)];
        assert!((agreement(&labels) - 1.0).abs() < f64::EPSILON);
        assert_eq!(cohens_kappa(&labels), None);
    }

    #[test]
    fn dedup_uses_last_value_without_reordering() {
        let input = [
            serde_json::to_string(&record("a", "first")).unwrap(),
            serde_json::to_string(&record("b", "middle")).unwrap(),
            serde_json::to_string(&record("a", "last")).unwrap(),
        ]
        .join("\n");
        let records = read_judge_records(Cursor::new(input)).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].id, "a");
        assert_eq!(records[0].reason, "last");
        assert_eq!(records[1].id, "b");
    }

    #[test]
    fn label_dedup_uses_last_value() {
        let mut replacement = label("a", false, false);
        replacement.labeler = "replacement".to_string();
        let input = [
            serde_json::to_string(&label("a", true, true)).unwrap(),
            serde_json::to_string(&replacement).unwrap(),
        ]
        .join("\n");
        let labels = read_judge_labels(Cursor::new(input)).unwrap();
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].labeler, "replacement");
    }

    #[test]
    fn load_calibration_rejection_matrix_and_valid_case() {
        let malformed = write_calibration("not json");
        assert!(matches!(
            load_calibration(malformed.path(), "provider:model"),
            Err(CalibrationRejection::Malformed { .. })
        ));

        let wrong_schema = write_calibration(&calibration_json(
            "zeroclaw-eval/calibration/v0",
            "provider:model",
            MIN_CALIBRATION_RECORDS,
        ));
        assert!(matches!(
            load_calibration(wrong_schema.path(), "provider:model"),
            Err(CalibrationRejection::WrongSchema { .. })
        ));

        let wrong_ref = write_calibration(&calibration_json(
            CALIBRATION_SCHEMA,
            "other:model",
            MIN_CALIBRATION_RECORDS,
        ));
        assert!(matches!(
            load_calibration(wrong_ref.path(), "provider:model"),
            Err(CalibrationRejection::WrongJudgeRef { .. })
        ));

        let too_small = write_calibration(&calibration_json(
            CALIBRATION_SCHEMA,
            "provider:model",
            MIN_CALIBRATION_RECORDS - 1,
        ));
        assert!(matches!(
            load_calibration(too_small.path(), "provider:model"),
            Err(CalibrationRejection::InsufficientRecords {
                found: 49,
                minimum: MIN_CALIBRATION_RECORDS,
            })
        ));

        let valid = write_calibration(&calibration_json(
            CALIBRATION_SCHEMA,
            "provider:model",
            MIN_CALIBRATION_RECORDS,
        ));
        let loaded = load_calibration(valid.path(), "provider:model").unwrap();
        assert_eq!(loaded.labeled_records, MIN_CALIBRATION_RECORDS);
    }

    #[test]
    fn calibration_rejects_unknown_keys() {
        let file = write_calibration(
            &serde_json::json!({
                "schema": CALIBRATION_SCHEMA,
                "judge_ref": "provider:model",
                "labeled_records": MIN_CALIBRATION_RECORDS,
                "agreement": 0.9,
                "labeler": "tester",
                "date": "2026-07-21",
                "extra": true,
            })
            .to_string(),
        );
        assert!(matches!(
            load_calibration(file.path(), "provider:model"),
            Err(CalibrationRejection::Malformed { .. })
        ));
    }

    #[test]
    fn stem_sanitizes_model_inclusive_ref() {
        assert_eq!(
            calibration_stem("anthropic.sonnet/v2:claude-x"),
            "anthropic_sonnet_v2_claude-x"
        );
    }

    #[test]
    fn jsonl_append_and_read_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/judge-runs.jsonl");
        let first = record("a", "first");
        let second = record("b", "second");
        assert_eq!(
            append_judge_records(&path, std::slice::from_ref(&first)).unwrap(),
            1
        );
        assert_eq!(
            append_judge_records(&path, std::slice::from_ref(&second)).unwrap(),
            1
        );
        assert_eq!(load_judge_records(&path).unwrap(), vec![first, second]);
    }

    #[test]
    fn record_append_repairs_missing_terminal_newline() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("judge-runs.jsonl");
        let first = record("a", "first");
        let second = record("b", "second");
        std::fs::write(&path, serde_json::to_vec(&first).unwrap()).unwrap();

        append_judge_records(&path, std::slice::from_ref(&second)).unwrap();

        assert_eq!(load_judge_records(&path).unwrap(), vec![first, second]);
    }

    #[test]
    fn label_append_repairs_missing_terminal_newline() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("judge-labels.jsonl");
        let first = label("a", true, true);
        let second = label("b", false, false);
        std::fs::write(&path, serde_json::to_vec(&first).unwrap()).unwrap();

        append_judge_labels(&path, std::slice::from_ref(&second)).unwrap();

        assert_eq!(load_judge_labels(&path).unwrap(), vec![first, second]);
    }

    #[test]
    fn insufficient_record_message_states_floor() {
        let file = write_calibration(&calibration_json(
            CALIBRATION_SCHEMA,
            "provider:model",
            MIN_CALIBRATION_RECORDS - 1,
        ));
        let rejection = load_calibration(file.path(), "provider:model").unwrap_err();
        assert_eq!(
            rejection.to_string(),
            "labeled_records is 49, but at least 50 are required"
        );
    }
}
