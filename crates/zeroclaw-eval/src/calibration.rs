//! Judge calibration artifacts: the only thing that may arm `judge_gate`.
//!
//! `judge_gate` converts a non-deterministic LLM opinion into the process exit
//! code, so the artifact that authorizes it is loaded strictly, not merely
//! probed for existence: schema version, exact effective judge identity,
//! minimum labeled-record count, and a finite agreement floor are all checked,
//! and any failure keeps judge grades diagnostic with a reason the operator can
//! read.

use std::path::Path;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

/// The only schema this build accepts. A calibration produced under a different
/// schema is rejected rather than best-effort parsed.
pub const CALIBRATION_SCHEMA: &str = "zeroclaw-eval/calibration/v1";

/// Minimum human-labeled records a calibration must cover before its judge may
/// gate. Matches the calibration protocol in `docs/book/src/ops/eval-harness.md`.
pub const MIN_LABELS: u64 = 50;

/// Minimum judge/human agreement a calibration must report before its judge may
/// gate.
pub const AGREEMENT_FLOOR: f64 = 0.8;

/// A judge calibration record, as committed under `evals/calibration/`.
///
/// `deny_unknown_fields`: an unrelated JSON document that happens to carry a
/// `judge_ref` must not be silently accepted as a calibration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Calibration {
    /// Schema identity; must equal [`CALIBRATION_SCHEMA`].
    pub schema: String,
    /// The model-inclusive judge ref (`<type>.<alias>:<model>`) this calibration
    /// was issued for. Must equal the judge identity actually served.
    pub judge_ref: String,
    /// How many human-labeled records back the agreement figure.
    pub labeled_records: u64,
    /// Judge/human agreement over those records, in `0.0..=1.0`.
    pub agreement: f64,
    /// Who produced the labels (free-form; recorded for provenance).
    pub labeler: String,
    /// Label date, `YYYY-MM-DD` (free-form; recorded for provenance).
    pub date: String,
}

/// Load and validate the calibration at `path` for the judge identity actually
/// served by `effective_judge_ref`.
///
/// Every failure mode — missing file, a directory at the path, empty file,
/// malformed JSON, wrong schema, mismatched judge ref, too few labels, or an
/// out-of-range/non-finite agreement — returns `Err` with a reason suitable for
/// display, so the caller can warn instead of silently degrading.
pub fn load_gating_calibration(
    path: &Path,
    effective_judge_ref: &str,
) -> anyhow::Result<Calibration> {
    let display = path.display();
    if path.is_dir() {
        anyhow::bail!("calibration path {display} is a directory, not a calibration file");
    }
    let bytes = std::fs::read(path)
        .with_context(|| format!("calibration file {display} could not be read"))?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        anyhow::bail!("calibration file {display} is empty");
    }
    let cal: Calibration = serde_json::from_slice(&bytes)
        .with_context(|| format!("calibration file {display} is not a valid v1 record"))?;

    anyhow::ensure!(
        cal.schema == CALIBRATION_SCHEMA,
        "calibration {display} declares schema {:?}, expected {CALIBRATION_SCHEMA:?}",
        cal.schema
    );
    anyhow::ensure!(
        cal.judge_ref == effective_judge_ref,
        "calibration {display} was issued for judge {:?} but the served judge is {effective_judge_ref:?}",
        cal.judge_ref
    );
    anyhow::ensure!(
        cal.labeled_records >= MIN_LABELS,
        "calibration {display} covers {} labeled record(s); at least {MIN_LABELS} are required",
        cal.labeled_records
    );
    anyhow::ensure!(
        cal.agreement.is_finite() && (0.0..=1.0).contains(&cal.agreement),
        "calibration {display} reports agreement {} which is not a finite value in 0.0..=1.0",
        cal.agreement
    );
    anyhow::ensure!(
        cal.agreement >= AGREEMENT_FLOOR,
        "calibration {display} reports agreement {:.2}, below the {AGREEMENT_FLOOR:.2} floor",
        cal.agreement
    );
    Ok(cal)
}

/// Whether judge grades may affect the process exit code, and why not when they
/// may not. `Refused` carries an operator-readable reason so a degraded gate is
/// never silent.
#[derive(Debug)]
pub enum GateDecision {
    /// `[eval].judge_gate` is not set: judge grades are diagnostic by design.
    Off,
    /// Calibration validated against the served judge identity; judge grades gate.
    Gated(Box<Calibration>),
    /// `judge_gate` was requested but cannot be honored. Judge grades stay
    /// diagnostic and the reason should be surfaced to the operator.
    Refused(String),
}

impl GateDecision {
    /// Whether judge grades affect the exit code.
    pub fn gates(&self) -> bool {
        matches!(self, GateDecision::Gated(_))
    }

    /// The reason a requested gate was refused, if it was.
    pub fn refusal(&self) -> Option<&str> {
        match self {
            GateDecision::Refused(reason) => Some(reason),
            _ => None,
        }
    }
}

/// Decide whether judge grades may gate.
///
/// `judge_ref` is the model-inclusive identity of the configured judge. A
/// calibration authorizes exactly one identity, so the gate is only honored when
/// that identity is the ONLY one the provider can serve: a configured provider
/// fallback or model fallback means a successful call can be served by a
/// different provider/model than the one calibrated, which would let a
/// calibration for the primary authorize an uncalibrated judge. In that case the
/// gate is refused rather than trusted.
pub fn decide_gate(
    gate_requested: bool,
    calibration_path: &Path,
    judge_ref: &str,
    provider_fallbacks: &[String],
    model_fallbacks: &[String],
) -> GateDecision {
    if !gate_requested {
        return GateDecision::Off;
    }
    if !provider_fallbacks.is_empty() || !model_fallbacks.is_empty() {
        return GateDecision::Refused(format!(
            "the judge provider for {judge_ref} has {} provider fallback(s) and {} model \
             fallback(s) configured, so a call can be served by an identity the calibration was \
             not issued for; remove them to gate on the judge",
            provider_fallbacks.len(),
            model_fallbacks.len()
        ));
    }
    match load_gating_calibration(calibration_path, judge_ref) {
        Ok(cal) => GateDecision::Gated(Box::new(cal)),
        Err(e) => GateDecision::Refused(format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REF: &str = "anthropic.judge:claude-x";

    fn valid_json(judge_ref: &str, labeled: u64, agreement: &str) -> String {
        format!(
            r#"{{"schema":"{CALIBRATION_SCHEMA}","judge_ref":"{judge_ref}","labeled_records":{labeled},"agreement":{agreement},"labeler":"human","date":"2026-08-01"}}"#
        )
    }

    fn write(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        let p = dir.join("cal.json");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn valid_calibration_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), &valid_json(REF, 50, "0.9"));
        let cal = load_gating_calibration(&p, REF).expect("valid calibration must load");
        assert_eq!(cal.judge_ref, REF);
        assert_eq!(cal.labeled_records, 50);
    }

    #[test]
    fn missing_file_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let err = load_gating_calibration(&tmp.path().join("nope.json"), REF).unwrap_err();
        assert!(
            err.to_string().contains("could not be read"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn directory_at_path_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("cal.json");
        std::fs::create_dir(&dir).unwrap();
        let err = load_gating_calibration(&dir, REF).unwrap_err();
        assert!(err.to_string().contains("directory"), "unexpected: {err}");
    }

    #[test]
    fn empty_file_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "   \n");
        let err = load_gating_calibration(&p, REF).unwrap_err();
        assert!(err.to_string().contains("is empty"), "unexpected: {err}");
    }

    #[test]
    fn malformed_json_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "{not json");
        let err = load_gating_calibration(&p, REF).unwrap_err();
        assert!(
            err.to_string().contains("not a valid v1 record"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn unrelated_json_object_is_rejected() {
        // An unrelated document that happens to mention judge_ref must not arm the gate.
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), r#"{"judge_ref":"anthropic.judge:claude-x"}"#);
        let err = load_gating_calibration(&p, REF).unwrap_err();
        assert!(
            err.to_string().contains("not a valid v1 record"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn wrong_schema_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let body = valid_json(REF, 50, "0.9").replace(CALIBRATION_SCHEMA, "some/other/v2");
        let p = write(tmp.path(), &body);
        let err = load_gating_calibration(&p, REF).unwrap_err();
        assert!(
            err.to_string().contains("declares schema"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn mismatched_judge_ref_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), &valid_json("openai.judge:gpt-y", 50, "0.9"));
        let err = load_gating_calibration(&p, REF).unwrap_err();
        assert!(
            err.to_string().contains("was issued for judge"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn insufficient_labels_is_rejected_at_the_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), &valid_json(REF, 49, "0.9"));
        let err = load_gating_calibration(&p, REF).unwrap_err();
        assert!(
            err.to_string().contains("labeled record"),
            "unexpected: {err}"
        );
        // 50 is the inclusive floor.
        let p = write(tmp.path(), &valid_json(REF, 50, "0.9"));
        assert!(load_gating_calibration(&p, REF).is_ok());
    }

    #[test]
    fn agreement_below_floor_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), &valid_json(REF, 50, "0.79"));
        let err = load_gating_calibration(&p, REF).unwrap_err();
        assert!(err.to_string().contains("below the"), "unexpected: {err}");
    }

    #[test]
    fn out_of_range_agreement_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), &valid_json(REF, 50, "999.0"));
        let err = load_gating_calibration(&p, REF).unwrap_err();
        assert!(
            err.to_string().contains("not a finite value"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn gate_off_when_not_requested() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), &valid_json(REF, 50, "0.9"));
        let d = decide_gate(false, &p, REF, &[], &[]);
        assert!(!d.gates());
        assert!(
            d.refusal().is_none(),
            "not requesting a gate is not a refusal"
        );
    }

    #[test]
    fn gate_on_with_valid_calibration() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), &valid_json(REF, 50, "0.9"));
        assert!(decide_gate(true, &p, REF, &[], &[]).gates());
    }

    #[test]
    fn gate_refused_with_reason_when_calibration_is_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let d = decide_gate(true, &tmp.path().join("nope.json"), REF, &[], &[]);
        assert!(!d.gates());
        assert!(
            d.refusal().is_some_and(|r| r.contains("could not be read")),
            "the refusal must name the reason: {:?}",
            d.refusal()
        );
    }

    #[test]
    fn judge_gate_stays_off_for_empty_calibration_file() {
        // `touch <calibration_path>` must NOT be sufficient to arm the gate.
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "");
        let d = decide_gate(true, &p, REF, &[], &[]);
        assert!(!d.gates(), "an empty file must not arm judge_gate");
        assert!(d.refusal().is_some_and(|r| r.contains("is empty")));
    }

    #[test]
    fn judge_gate_stays_off_for_directory_at_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("cal.json");
        std::fs::create_dir(&dir).unwrap();
        let d = decide_gate(true, &dir, REF, &[], &[]);
        assert!(!d.gates(), "a directory must not arm judge_gate");
        assert!(d.refusal().is_some_and(|r| r.contains("directory")));
    }

    #[test]
    fn judge_gate_stays_off_for_malformed_json() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "{not json");
        let d = decide_gate(true, &p, REF, &[], &[]);
        assert!(!d.gates(), "malformed JSON must not arm judge_gate");
        assert!(
            d.refusal()
                .is_some_and(|r| r.contains("not a valid v1 record"))
        );
    }

    #[test]
    fn judge_gate_rejects_mismatched_judge_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), &valid_json("openai.other:gpt-y", 50, "0.9"));
        let d = decide_gate(true, &p, REF, &[], &[]);
        assert!(!d.gates(), "a calibration for another judge must not gate");
        assert!(
            d.refusal()
                .is_some_and(|r| r.contains("was issued for judge"))
        );
    }

    #[test]
    fn judge_gate_rejects_insufficient_labels() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), &valid_json(REF, 49, "0.9"));
        assert!(!decide_gate(true, &p, REF, &[], &[]).gates());
    }

    #[test]
    fn judge_gate_rejects_agreement_below_floor() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), &valid_json(REF, 50, "0.5"));
        assert!(!decide_gate(true, &p, REF, &[], &[]).gates());
    }

    #[test]
    fn gated_judge_refuses_provider_fallback() {
        // A calibration authorizes ONE identity. If the provider can fail over to
        // another alias, a successful call may be served by an uncalibrated judge.
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), &valid_json(REF, 50, "0.9"));
        let d = decide_gate(true, &p, REF, &["openai.other".to_string()], &[]);
        assert!(!d.gates(), "a fallback-capable judge must not gate");
        assert!(
            d.refusal().is_some_and(|r| r.contains("fallback")),
            "the refusal must name fallback: {:?}",
            d.refusal()
        );
    }

    #[test]
    fn gated_judge_refuses_model_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), &valid_json(REF, 50, "0.9"));
        let d = decide_gate(true, &p, REF, &[], &["claude-y".to_string()]);
        assert!(!d.gates(), "a model-fallback judge must not gate");
        assert!(d.refusal().is_some_and(|r| r.contains("fallback")));
    }
}
