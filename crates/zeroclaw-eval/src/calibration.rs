//! Durable schemas and file helpers for LLM-judge calibration.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::hash::Hash;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// Schema tag for one calibratable judge run.
pub const JUDGE_RECORD_SCHEMA: &str = "zeroclaw-eval/judge-record/v1";
/// Schema tag for one blind human label of a judge run.
pub const JUDGE_LABEL_SCHEMA: &str = "zeroclaw-eval/judge-label/v1";
/// Schema tag for the calibration marker consumed by judge gating.
pub const CALIBRATION_SCHEMA: &str = "zeroclaw-eval/calibration/v1";
/// Minimum number of human labels required before judge gating can be enabled.
pub const MIN_CALIBRATION_RECORDS: usize = 50;
/// Minimum judge/human agreement required before judge grades may gate.
pub const AGREEMENT_FLOOR: f64 = 0.85;

/// One parseable, non-unknown LLM-judge result ready for blind labeling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeRunRecord {
    pub schema: String,
    pub id: String,
    pub judge_ref: String,
    pub prompt_hash: String,
    pub case_id: String,
    pub case_hash: String,
    pub rubric_name: String,
    pub rubric_text: String,
    pub rubric_hash: String,
    pub threshold: f64,
    pub include_transcript: bool,
    pub task_turns: Vec<String>,
    /// Serialized agent conversation shown to the judge when the rubric opts
    /// into transcript evidence. `None` otherwise, avoiding unnecessary
    /// disclosure in calibration records.
    pub transcript: Option<String>,
    pub final_response: String,
    pub score: f64,
    pub judge_pass: bool,
    pub reason: String,
}

impl JudgeRunRecord {
    /// Construct a record and derive its stable id and judge verdict.
    #[must_use]
    pub fn new(input: JudgeRunRecordInput) -> Self {
        let id = judge_record_id(&input);
        let judge_pass = input.score >= input.threshold;
        Self {
            schema: JUDGE_RECORD_SCHEMA.to_string(),
            id,
            judge_ref: input.judge_ref,
            prompt_hash: input.prompt_hash,
            case_id: input.case_id,
            case_hash: input.case_hash,
            rubric_name: input.rubric_name,
            rubric_text: input.rubric_text,
            rubric_hash: input.rubric_hash,
            threshold: input.threshold,
            include_transcript: input.include_transcript,
            task_turns: input.task_turns,
            transcript: input.transcript,
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
    pub prompt_hash: String,
    pub case_id: String,
    pub case_hash: String,
    pub rubric_name: String,
    pub rubric_text: String,
    pub rubric_hash: String,
    pub threshold: f64,
    pub include_transcript: bool,
    pub task_turns: Vec<String>,
    pub transcript: Option<String>,
    pub final_response: String,
    pub score: f64,
    pub reason: String,
}

/// One blind human verdict paired with the hidden judge verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeLabel {
    pub schema: String,
    pub record_id: String,
    pub judge_ref: String,
    pub prompt_hash: String,
    pub rubric_name: String,
    pub rubric_hash: String,
    pub human_pass: bool,
    pub judge_pass: bool,
    pub score: f64,
    pub labeler: String,
    pub date: String,
}

/// Calibration evidence retained for one exact rubric contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RubricCalibration {
    pub rubric_name: String,
    pub labeled_records: usize,
    pub agreement: f64,
    pub kappa: Option<f64>,
}

/// Validated marker that permits an LLM judge to gate eval results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationFile {
    pub schema: String,
    pub judge_ref: String,
    /// Hash of the exact system prompt and reply-parser/scoring contract.
    pub prompt_hash: String,
    pub labeled_records: usize,
    pub agreement: f64,
    pub kappa: Option<f64>,
    /// Exact rubric-contract hash to retained calibration evidence.
    pub rubrics: BTreeMap<String, RubricCalibration>,
    pub labeler: String,
    pub date: String,
}

/// Calibration artifact that passed every global runtime gate check. The inner
/// file stays private so callers cannot accidentally construct a trusted handle
/// from unvalidated JSON or an in-memory literal.
#[derive(Debug, Clone)]
pub struct ValidatedCalibration {
    file: CalibrationFile,
}

impl ValidatedCalibration {
    /// Inspect the validated artifact without weakening the construction gate.
    #[must_use]
    pub fn artifact(&self) -> &CalibrationFile {
        &self.file
    }
}

fn hash_part(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn finish_hash(hasher: Sha256) -> String {
    format!("{:x}", hasher.finalize())
}

/// Hash the exact judge prompt and reply-parser/scoring contract.
#[must_use]
pub fn judge_prompt_hash(system_prompt: &str, scoring_contract: &str) -> String {
    let mut hasher = Sha256::new();
    hash_part(&mut hasher, system_prompt.as_bytes());
    hash_part(&mut hasher, scoring_contract.as_bytes());
    finish_hash(hasher)
}

/// Hash every field that defines one rubric's grading contract.
#[must_use]
pub fn rubric_hash(name: &str, text: &str, threshold: f64, include_transcript: bool) -> String {
    let mut hasher = Sha256::new();
    hash_part(&mut hasher, name.as_bytes());
    hash_part(&mut hasher, text.as_bytes());
    hash_part(&mut hasher, &threshold.to_bits().to_be_bytes());
    hash_part(&mut hasher, &[u8::from(include_transcript)]);
    finish_hash(hasher)
}

/// Derive the stable id for a complete judge-run record. Length-prefixed fields
/// prevent separator ambiguity, and all label-visible evidence participates so
/// two different responses cannot silently collapse into one sample.
#[must_use]
pub fn judge_record_id(input: &JudgeRunRecordInput) -> String {
    let mut hasher = Sha256::new();
    for value in [
        input.judge_ref.as_str(),
        input.prompt_hash.as_str(),
        input.case_id.as_str(),
        input.case_hash.as_str(),
        input.rubric_name.as_str(),
        input.rubric_text.as_str(),
        input.rubric_hash.as_str(),
        input.final_response.as_str(),
        input.reason.as_str(),
    ] {
        hash_part(&mut hasher, value.as_bytes());
    }
    for turn in &input.task_turns {
        hash_part(&mut hasher, turn.as_bytes());
    }
    match &input.transcript {
        Some(transcript) => {
            hash_part(&mut hasher, &[1]);
            hash_part(&mut hasher, transcript.as_bytes());
        }
        None => hash_part(&mut hasher, &[0]),
    }
    hash_part(&mut hasher, &input.threshold.to_bits().to_be_bytes());
    hash_part(&mut hasher, &input.score.to_bits().to_be_bytes());
    finish_hash(hasher)
}

/// Convert a model-inclusive judge reference into its calibration filename stem.
#[must_use]
pub fn calibration_stem(judge_ref: &str) -> String {
    let readable: String = judge_ref
        .chars()
        .take(64)
        .map(|character| match character {
            safe if safe.is_ascii_alphanumeric() || matches!(safe, '-' | '_') => safe,
            _ => '_',
        })
        .collect();
    let digest = format!("{:x}", Sha256::digest(judge_ref.as_bytes()));
    format!("{readable}-{}", &digest[..12])
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
    InvalidData {
        line: usize,
        reason: String,
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
            Self::InvalidData { line, reason } => {
                write!(
                    formatter,
                    "invalid calibration JSONL data at line {line}: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for JsonlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Decode { source, .. } | Self::Encode(source) => Some(source),
            Self::WrongSchema { .. } | Self::InvalidData { .. } => None,
        }
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn invalid_field(field: &str, reason: &str) -> String {
    format!("{field} {reason}")
}

fn validate_record(record: &JudgeRunRecord) -> Result<(), String> {
    for (field, value) in [
        ("id", record.id.as_str()),
        ("judge_ref", record.judge_ref.as_str()),
        ("prompt_hash", record.prompt_hash.as_str()),
        ("case_id", record.case_id.as_str()),
        ("case_hash", record.case_hash.as_str()),
        ("rubric_name", record.rubric_name.as_str()),
        ("rubric_text", record.rubric_text.as_str()),
        ("rubric_hash", record.rubric_hash.as_str()),
        ("reason", record.reason.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(invalid_field(field, "must not be empty"));
        }
    }
    for (field, value) in [
        ("id", record.id.as_str()),
        ("prompt_hash", record.prompt_hash.as_str()),
        ("case_hash", record.case_hash.as_str()),
        ("rubric_hash", record.rubric_hash.as_str()),
    ] {
        if !is_sha256(value) {
            return Err(invalid_field(
                field,
                "must be a lowercase SHA-256 hex digest",
            ));
        }
    }
    if record.task_turns.is_empty() || record.task_turns.iter().any(|turn| turn.trim().is_empty()) {
        return Err(invalid_field(
            "task_turns",
            "must contain only nonempty turns",
        ));
    }
    if record.include_transcript != record.transcript.is_some()
        || record
            .transcript
            .as_deref()
            .is_some_and(|transcript| transcript.trim().is_empty())
    {
        return Err(invalid_field(
            "transcript",
            "must be nonempty exactly when include_transcript is true",
        ));
    }
    if record.transcript.as_deref().is_some_and(|transcript| {
        serde_json::from_str::<serde_json::Value>(transcript)
            .map_or(true, |value| !value.is_array())
    }) {
        return Err(invalid_field(
            "transcript",
            "must encode a JSON array of conversation messages",
        ));
    }
    for (field, value) in [("threshold", record.threshold), ("score", record.score)] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(invalid_field(field, "must be finite and between 0 and 1"));
        }
    }
    if record.judge_pass != (record.score >= record.threshold) {
        return Err(invalid_field(
            "judge_pass",
            "does not match score >= threshold",
        ));
    }
    let expected_rubric_hash = rubric_hash(
        &record.rubric_name,
        &record.rubric_text,
        record.threshold,
        record.include_transcript,
    );
    if record.rubric_hash != expected_rubric_hash {
        return Err(invalid_field(
            "rubric_hash",
            "does not match the rubric contents",
        ));
    }
    let expected_id = judge_record_id(&JudgeRunRecordInput {
        judge_ref: record.judge_ref.clone(),
        prompt_hash: record.prompt_hash.clone(),
        case_id: record.case_id.clone(),
        case_hash: record.case_hash.clone(),
        rubric_name: record.rubric_name.clone(),
        rubric_text: record.rubric_text.clone(),
        rubric_hash: record.rubric_hash.clone(),
        threshold: record.threshold,
        include_transcript: record.include_transcript,
        task_turns: record.task_turns.clone(),
        transcript: record.transcript.clone(),
        final_response: record.final_response.clone(),
        score: record.score,
        reason: record.reason.clone(),
    });
    if record.id != expected_id {
        return Err(invalid_field("id", "does not match the record contents"));
    }
    Ok(())
}

fn validate_label(label: &JudgeLabel) -> Result<(), String> {
    for (field, value) in [
        ("record_id", label.record_id.as_str()),
        ("judge_ref", label.judge_ref.as_str()),
        ("prompt_hash", label.prompt_hash.as_str()),
        ("rubric_name", label.rubric_name.as_str()),
        ("rubric_hash", label.rubric_hash.as_str()),
        ("labeler", label.labeler.as_str()),
        ("date", label.date.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(invalid_field(field, "must not be empty"));
        }
    }
    for (field, value) in [
        ("record_id", label.record_id.as_str()),
        ("prompt_hash", label.prompt_hash.as_str()),
        ("rubric_hash", label.rubric_hash.as_str()),
    ] {
        if !is_sha256(value) {
            return Err(invalid_field(
                field,
                "must be a lowercase SHA-256 hex digest",
            ));
        }
    }
    if !label.score.is_finite() || !(0.0..=1.0).contains(&label.score) {
        return Err(invalid_field("score", "must be finite and between 0 and 1"));
    }
    if chrono::NaiveDate::parse_from_str(&label.date, "%Y-%m-%d").is_err() {
        return Err(invalid_field("date", "must use YYYY-MM-DD format"));
    }
    Ok(())
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
                validate_record(&record)
                    .map(|()| record)
                    .map_err(|reason| JsonlError::InvalidData { line, reason })
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
                validate_label(&label)
                    .map(|()| label)
                    .map_err(|reason| JsonlError::InvalidData { line, reason })
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
    let mut options = OpenOptions::new();
    options.create(true).read(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.lock()?;

    let write_result = (|| {
        let needs_separator = if file.metadata()?.len() == 0 {
            false
        } else {
            file.seek(SeekFrom::End(-1))?;
            let mut last_byte = [0];
            file.read_exact(&mut last_byte)?;
            last_byte[0] != b'\n'
        };
        file.seek(SeekFrom::End(0))?;
        {
            let mut writer = BufWriter::new(&mut file);
            if needs_separator {
                writer.write_all(b"\n")?;
            }
            for value in values {
                serde_json::to_writer(&mut writer, value).map_err(JsonlError::Encode)?;
                writer.write_all(b"\n")?;
            }
            writer.flush()?;
        }
        file.sync_data()?;
        Ok(values.len())
    })();
    let unlock_result = file.unlock().map_err(JsonlError::Io);
    match (write_result, unlock_result) {
        (Ok(count), Ok(())) => Ok(count),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(unlock_error)) => Err(JsonlError::Io(io::Error::other(format!(
            "{error}; also failed to unlock calibration JSONL: {unlock_error}"
        )))),
    }
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
    WrongPromptHash {
        expected: String,
        found: String,
    },
    InsufficientRecords {
        found: usize,
        minimum: usize,
    },
    InvalidAgreement {
        found: f64,
    },
    LowAgreement {
        found: f64,
        minimum: f64,
    },
    InvalidEvidence {
        reason: String,
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
            Self::WrongPromptHash { expected, found } => {
                write!(formatter, "prompt_hash is '{found}', expected '{expected}'")
            }
            Self::InsufficientRecords { found, minimum } => write!(
                formatter,
                "labeled_records is {found}, but at least {minimum} are required"
            ),
            Self::InvalidAgreement { found } => write!(
                formatter,
                "agreement is {found}, expected a finite value between 0 and 1"
            ),
            Self::LowAgreement { found, minimum } => write!(
                formatter,
                "agreement is {found:.4}, below the {minimum:.4} gating floor"
            ),
            Self::InvalidEvidence { reason } => {
                write!(formatter, "calibration evidence is inconsistent: {reason}")
            }
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
            | Self::WrongPromptHash { .. }
            | Self::InsufficientRecords { .. }
            | Self::InvalidAgreement { .. }
            | Self::LowAgreement { .. }
            | Self::InvalidEvidence { .. } => None,
        }
    }
}

/// Load and validate the calibration marker for one exact judge reference.
pub fn load_calibration(
    path: &Path,
    expected_judge_ref: &str,
    expected_prompt_hash: &str,
) -> Result<ValidatedCalibration, CalibrationRejection> {
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
    if calibration.prompt_hash != expected_prompt_hash {
        return Err(CalibrationRejection::WrongPromptHash {
            expected: expected_prompt_hash.to_string(),
            found: calibration.prompt_hash,
        });
    }
    if calibration.judge_ref.trim().is_empty() || !is_sha256(&calibration.prompt_hash) {
        return Err(CalibrationRejection::InvalidEvidence {
            reason: "judge_ref must be nonempty and prompt_hash must be a SHA-256 digest"
                .to_string(),
        });
    }
    if calibration.labeled_records < MIN_CALIBRATION_RECORDS {
        return Err(CalibrationRejection::InsufficientRecords {
            found: calibration.labeled_records,
            minimum: MIN_CALIBRATION_RECORDS,
        });
    }
    if !calibration.agreement.is_finite() || !(0.0..=1.0).contains(&calibration.agreement) {
        return Err(CalibrationRejection::InvalidAgreement {
            found: calibration.agreement,
        });
    }
    if calibration.agreement < AGREEMENT_FLOOR {
        return Err(CalibrationRejection::LowAgreement {
            found: calibration.agreement,
            minimum: AGREEMENT_FLOOR,
        });
    }
    if calibration.labeler.trim().is_empty() {
        return Err(CalibrationRejection::InvalidEvidence {
            reason: "labeler must not be empty".to_string(),
        });
    }
    if chrono::NaiveDate::parse_from_str(&calibration.date, "%Y-%m-%d").is_err() {
        return Err(CalibrationRejection::InvalidEvidence {
            reason: "date must use YYYY-MM-DD format".to_string(),
        });
    }
    if calibration.rubrics.is_empty() {
        return Err(CalibrationRejection::InvalidEvidence {
            reason: "at least one rubric contract is required".to_string(),
        });
    }
    let mut total = 0usize;
    let mut weighted_agreement = 0.0;
    for (hash, rubric) in &calibration.rubrics {
        if !is_sha256(hash) {
            return Err(CalibrationRejection::InvalidEvidence {
                reason: format!("rubric hash {hash:?} is not a SHA-256 digest"),
            });
        }
        if rubric.rubric_name.trim().is_empty() || rubric.labeled_records == 0 {
            return Err(CalibrationRejection::InvalidEvidence {
                reason: format!("rubric {hash:?} must have a nonempty name and at least one label"),
            });
        }
        if !rubric.agreement.is_finite() || !(0.0..=1.0).contains(&rubric.agreement) {
            return Err(CalibrationRejection::InvalidEvidence {
                reason: format!("rubric {hash:?} has invalid agreement {}", rubric.agreement),
            });
        }
        if rubric
            .kappa
            .is_some_and(|value| !value.is_finite() || !(-1.0..=1.0).contains(&value))
        {
            return Err(CalibrationRejection::InvalidEvidence {
                reason: format!("rubric {hash:?} has invalid Cohen's kappa"),
            });
        }
        total = total.checked_add(rubric.labeled_records).ok_or_else(|| {
            CalibrationRejection::InvalidEvidence {
                reason: "rubric label counts overflow usize".to_string(),
            }
        })?;
        weighted_agreement += rubric.agreement * rubric.labeled_records as f64;
    }
    if total != calibration.labeled_records {
        return Err(CalibrationRejection::InvalidEvidence {
            reason: format!(
                "rubric counts sum to {total}, expected {}",
                calibration.labeled_records
            ),
        });
    }
    let recomputed_agreement = weighted_agreement / total as f64;
    if (recomputed_agreement - calibration.agreement).abs() > 1e-12 {
        return Err(CalibrationRejection::InvalidEvidence {
            reason: format!(
                "rubric agreement aggregates to {recomputed_agreement}, expected {}",
                calibration.agreement
            ),
        });
    }
    if calibration
        .kappa
        .is_some_and(|value| !value.is_finite() || !(-1.0..=1.0).contains(&value))
    {
        return Err(CalibrationRejection::InvalidEvidence {
            reason: "overall Cohen's kappa must be null or finite in -1..=1".to_string(),
        });
    }
    Ok(ValidatedCalibration { file: calibration })
}

/// Why one exact rubric remains diagnostic under an otherwise valid
/// calibration artifact.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RubricGateRefusal {
    Missing,
    LowAgreement { found: f64, minimum: f64 },
}

impl ValidatedCalibration {
    /// Confirm that this artifact contains reliable evidence for the exact
    /// rubric contract being graded.
    pub fn rubric_gate_refusal(&self, rubric_hash: &str) -> Option<RubricGateRefusal> {
        let rubric = match self.file.rubrics.get(rubric_hash) {
            Some(rubric) => rubric,
            None => return Some(RubricGateRefusal::Missing),
        };
        (rubric.agreement < AGREEMENT_FLOOR).then_some(RubricGateRefusal::LowAgreement {
            found: rubric.agreement,
            minimum: AGREEMENT_FLOOR,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn prompt_hash() -> String {
        judge_prompt_hash("system", "contract")
    }

    fn quality_rubric_hash() -> String {
        rubric_hash("quality", "Be correct", 0.5, false)
    }

    fn label(id: &str, human_pass: bool, judge_pass: bool) -> JudgeLabel {
        JudgeLabel {
            schema: JUDGE_LABEL_SCHEMA.to_string(),
            record_id: format!("{:x}", Sha256::digest(id.as_bytes())),
            judge_ref: "provider:model".to_string(),
            prompt_hash: prompt_hash(),
            rubric_name: "quality".to_string(),
            rubric_hash: quality_rubric_hash(),
            human_pass,
            judge_pass,
            score: if judge_pass { 0.9 } else { 0.1 },
            labeler: "tester".to_string(),
            date: "2026-07-21".to_string(),
        }
    }

    fn record(id: &str, reason: &str) -> JudgeRunRecord {
        JudgeRunRecord::new(JudgeRunRecordInput {
            judge_ref: "provider:model".to_string(),
            prompt_hash: prompt_hash(),
            case_id: format!("case-{id}"),
            case_hash: format!("{:x}", Sha256::digest(id.as_bytes())),
            rubric_name: "quality".to_string(),
            rubric_text: "Be correct".to_string(),
            rubric_hash: quality_rubric_hash(),
            threshold: 0.5,
            include_transcript: false,
            task_turns: vec!["question".to_string()],
            transcript: None,
            final_response: "answer".to_string(),
            score: 0.9,
            reason: reason.to_string(),
        })
    }

    fn calibration_json(schema: &str, judge_ref: &str, labeled_records: usize) -> String {
        serde_json::json!({
            "schema": schema,
            "judge_ref": judge_ref,
            "prompt_hash": prompt_hash(),
            "labeled_records": labeled_records,
            "agreement": 0.9,
            "kappa": 0.8,
            "rubrics": {
                (quality_rubric_hash()): {
                    "rubric_name": "quality",
                    "labeled_records": labeled_records,
                    "agreement": 0.9,
                    "kappa": 0.8,
                }
            },
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
        let input = JudgeRunRecordInput {
            judge_ref: "anthropic.sonnet:claude-x".to_string(),
            prompt_hash: prompt_hash(),
            case_id: "case".to_string(),
            case_hash: format!("{:x}", Sha256::digest(b"case")),
            rubric_name: "correctness".to_string(),
            rubric_text: "Be correct".to_string(),
            rubric_hash: rubric_hash("correctness", "Be correct", 0.7, false),
            threshold: 0.7,
            include_transcript: false,
            task_turns: vec!["task".to_string()],
            transcript: None,
            final_response: "answer".to_string(),
            score: 0.87,
            reason: "well supported".to_string(),
        };
        let id = judge_record_id(&input);
        assert_eq!(id.len(), 64);
        assert_eq!(id, judge_record_id(&input));
        let changed = JudgeRunRecordInput {
            final_response: "different answer".to_string(),
            ..input
        };
        assert_ne!(id, judge_record_id(&changed));
    }

    #[test]
    fn constructor_owns_id_and_verdict_derivation() {
        let record = JudgeRunRecord::new(JudgeRunRecordInput {
            judge_ref: "provider:model".to_string(),
            prompt_hash: prompt_hash(),
            case_id: "case".to_string(),
            case_hash: format!("{:x}", Sha256::digest(b"case")),
            rubric_name: "quality".to_string(),
            rubric_text: "Be correct".to_string(),
            rubric_hash: rubric_hash("quality", "Be correct", 0.8, false),
            threshold: 0.8,
            include_transcript: false,
            task_turns: vec!["question".to_string()],
            transcript: None,
            final_response: "answer".to_string(),
            score: 0.8,
            reason: "reason".to_string(),
        });
        assert_eq!(record.schema, JUDGE_RECORD_SCHEMA);
        assert!(record.judge_pass);
        assert_eq!(record.id.len(), 64);
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
    fn exact_record_duplicates_are_deduplicated_without_reordering() {
        let first = record("a", "first");
        let second = record("b", "middle");
        let input = [
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap(),
            serde_json::to_string(&first).unwrap(),
        ]
        .join("\n");
        let records = read_judge_records(Cursor::new(input)).unwrap();
        assert_eq!(records, vec![first, second]);
    }

    #[test]
    fn record_reader_rejects_tampered_ids_and_unknown_fields() {
        let mut tampered = record("a", "first");
        tampered.final_response = "changed without changing id".to_string();
        let error =
            read_judge_records(Cursor::new(serde_json::to_string(&tampered).unwrap())).unwrap_err();
        assert!(matches!(error, JsonlError::InvalidData { .. }));

        let mut unknown = serde_json::to_value(record("b", "second")).unwrap();
        unknown["extra"] = serde_json::json!(true);
        let error = read_judge_records(Cursor::new(unknown.to_string())).unwrap_err();
        assert!(matches!(error, JsonlError::Decode { .. }));
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
            load_calibration(malformed.path(), "provider:model", &prompt_hash()),
            Err(CalibrationRejection::Malformed { .. })
        ));

        let wrong_schema = write_calibration(&calibration_json(
            "zeroclaw-eval/calibration/v0",
            "provider:model",
            MIN_CALIBRATION_RECORDS,
        ));
        assert!(matches!(
            load_calibration(wrong_schema.path(), "provider:model", &prompt_hash()),
            Err(CalibrationRejection::WrongSchema { .. })
        ));

        let wrong_ref = write_calibration(&calibration_json(
            CALIBRATION_SCHEMA,
            "other:model",
            MIN_CALIBRATION_RECORDS,
        ));
        assert!(matches!(
            load_calibration(wrong_ref.path(), "provider:model", &prompt_hash()),
            Err(CalibrationRejection::WrongJudgeRef { .. })
        ));

        let too_small = write_calibration(&calibration_json(
            CALIBRATION_SCHEMA,
            "provider:model",
            MIN_CALIBRATION_RECORDS - 1,
        ));
        assert!(matches!(
            load_calibration(too_small.path(), "provider:model", &prompt_hash()),
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
        let loaded = load_calibration(valid.path(), "provider:model", &prompt_hash()).unwrap();
        assert_eq!(loaded.artifact().labeled_records, MIN_CALIBRATION_RECORDS);
    }

    #[test]
    fn gating_rejects_stale_prompt_and_low_agreement() {
        let valid_json = calibration_json(
            CALIBRATION_SCHEMA,
            "provider:model",
            MIN_CALIBRATION_RECORDS,
        );
        let stale_prompt = write_calibration(&valid_json);
        assert!(matches!(
            load_calibration(
                stale_prompt.path(),
                "provider:model",
                &judge_prompt_hash("changed", "contract")
            ),
            Err(CalibrationRejection::WrongPromptHash { .. })
        ));

        let mut low: serde_json::Value = serde_json::from_str(&valid_json).unwrap();
        low["agreement"] = serde_json::json!(AGREEMENT_FLOOR - 0.01);
        let rubric = low["rubrics"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        rubric["agreement"] = serde_json::json!(AGREEMENT_FLOOR - 0.01);
        let low = write_calibration(&low.to_string());
        assert!(matches!(
            load_calibration(low.path(), "provider:model", &prompt_hash()),
            Err(CalibrationRejection::LowAgreement { .. })
        ));
    }

    #[test]
    fn low_agreement_rubric_stays_diagnostic_inside_valid_artifact() {
        let weak_hash = rubric_hash("weak", "weak rubric", 0.5, false);
        let strong_hash = rubric_hash("strong", "strong rubric", 0.5, false);
        let artifact = CalibrationFile {
            schema: CALIBRATION_SCHEMA.to_string(),
            judge_ref: "provider:model".to_string(),
            prompt_hash: prompt_hash(),
            labeled_records: MIN_CALIBRATION_RECORDS,
            agreement: 0.9,
            kappa: Some(0.8),
            rubrics: BTreeMap::from([
                (
                    weak_hash.clone(),
                    RubricCalibration {
                        rubric_name: "weak".to_string(),
                        labeled_records: 10,
                        agreement: 0.8,
                        kappa: Some(0.6),
                    },
                ),
                (
                    strong_hash.clone(),
                    RubricCalibration {
                        rubric_name: "strong".to_string(),
                        labeled_records: 40,
                        agreement: 0.925,
                        kappa: Some(0.85),
                    },
                ),
            ]),
            labeler: "tester".to_string(),
            date: "2026-07-21".to_string(),
        };
        let file = write_calibration(&serde_json::to_string(&artifact).unwrap());
        let validated = load_calibration(file.path(), "provider:model", &prompt_hash()).unwrap();
        assert!(matches!(
            validated.rubric_gate_refusal(&weak_hash),
            Some(RubricGateRefusal::LowAgreement { .. })
        ));
        assert_eq!(validated.rubric_gate_refusal(&strong_hash), None);
        assert_eq!(
            validated.rubric_gate_refusal(&rubric_hash("new", "new", 0.5, false)),
            Some(RubricGateRefusal::Missing)
        );
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
            load_calibration(file.path(), "provider:model", &prompt_hash()),
            Err(CalibrationRejection::Malformed { .. })
        ));
    }

    #[test]
    fn stem_sanitizes_model_inclusive_ref() {
        let stem = calibration_stem("anthropic.sonnet/v2:claude-x");
        assert!(stem.starts_with("anthropic_sonnet_v2_claude-x-"));
        assert_eq!(stem.len(), "anthropic_sonnet_v2_claude-x-".len() + 12);
        assert_ne!(calibration_stem("a.b:model"), calibration_stem("a_b:model"));
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
        let rejection =
            load_calibration(file.path(), "provider:model", &prompt_hash()).unwrap_err();
        assert_eq!(
            rejection.to_string(),
            "labeled_records is 49, but at least 50 are required"
        );
    }
}
