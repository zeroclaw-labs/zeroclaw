//! Calibration agreement calculation and marker-file emission.

use anyhow::{Result, bail};
use std::path::PathBuf;

pub fn run(
    _labels: PathBuf,
    _out: Option<PathBuf>,
    _min_agreement: Option<f64>,
    _labeler: Option<String>,
) -> Result<()> {
    bail!("unimplemented")
}
