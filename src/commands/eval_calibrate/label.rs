//! Blind human labeling flow for LLM-judge calibration.

use anyhow::{Result, bail};
use std::path::PathBuf;

pub fn run(
    _records: PathBuf,
    _labels: Option<PathBuf>,
    _labeler: Option<String>,
    _judge_ref: Option<String>,
) -> Result<()> {
    bail!("unimplemented")
}
