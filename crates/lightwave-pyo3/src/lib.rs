//! PyO3 bindings for lightwave-sys.
//!
//! Exposes the FDX parser as a Python module named `lightwave_sys`.
//!
//! # Usage from Python
//!
//! ```python
//! import lightwave_sys
//! scenes = lightwave_sys.parse_fdx(raw_bytes)
//! for scene in scenes:
//!     print(scene.scene_number, scene.location)
//! ```

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use ::lightwave_sys::fdx;

/// A parsed dialogue block (character + line).
#[pyclass(frozen)]
#[derive(Clone)]
struct PyDialogueBlock {
    #[pyo3(get)]
    character: String,
    #[pyo3(get)]
    text: String,
}

/// A parsed scene from an FDX file.
#[pyclass(frozen)]
#[derive(Clone)]
struct PySceneData {
    #[pyo3(get)]
    scene_number: u32,
    #[pyo3(get)]
    int_ext: String,
    #[pyo3(get)]
    location: String,
    #[pyo3(get)]
    day_night: String,
    #[pyo3(get)]
    action_blocks: Vec<String>,
    #[pyo3(get)]
    dialogue_blocks: Vec<PyDialogueBlock>,
    #[pyo3(get)]
    page_count: f64,
}

#[pymethods]
impl PySceneData {
    /// Convert to a dict suitable for Django model creation.
    fn to_django_dict(&self, py: Python<'_>) -> PyResult<PyObject> {
        let dict = pyo3::types::PyDict::new(py);
        dict.set_item("scene_number", self.scene_number)?;
        dict.set_item("int_ext", map_int_ext(&self.int_ext))?;
        dict.set_item("day_night", map_day_night(&self.day_night))?;
        dict.set_item("name", &self.location)?;
        dict.set_item("page_count", self.page_count)?;

        // Build description from action blocks
        let description = self.action_blocks.join("\n\n");
        dict.set_item("description", description)?;

        Ok(dict.into())
    }

    fn __repr__(&self) -> String {
        format!(
            "PySceneData(scene_number={}, int_ext='{}', location='{}', day_night='{}')",
            self.scene_number, self.int_ext, self.location, self.day_night
        )
    }
}

/// Parse an FDX file from raw bytes, returning a list of scenes.
///
/// Args:
///     data: Raw FDX file bytes.
///
/// Returns:
///     List of PySceneData objects.
///
/// Raises:
///     ValueError: If the input is not valid FDX XML.
#[pyfunction]
fn parse_fdx(data: &[u8]) -> PyResult<Vec<PySceneData>> {
    let scenes = fdx::parser::parse_fdx(data).map_err(|e| PyValueError::new_err(e.to_string()))?;

    Ok(scenes
        .into_iter()
        .map(|s| PySceneData {
            scene_number: s.scene_number,
            int_ext: s.int_ext,
            location: s.location,
            day_night: s.day_night,
            action_blocks: s.action_blocks,
            dialogue_blocks: s
                .dialogue_blocks
                .into_iter()
                .map(|d| PyDialogueBlock {
                    character: d.character,
                    text: d.text,
                })
                .collect(),
            page_count: s.page_count,
        })
        .collect())
}

/// The lightwave_sys Python module.
#[pymodule]
fn lightwave_sys(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(parse_fdx, m)?)?;
    m.add_class::<PySceneData>()?;
    m.add_class::<PyDialogueBlock>()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Mapping helpers: Rust values → Django model choices
// ---------------------------------------------------------------------------

/// Map parser int_ext values to Django model choices.
fn map_int_ext(value: &str) -> &str {
    match value {
        "INT" => "interior",
        "EXT" => "exterior",
        // INT/EXT doesn't have a direct mapping in the current model;
        // default to interior (the model only supports interior/exterior).
        "INT/EXT" => "interior",
        _ => "interior",
    }
}

/// Map parser day_night values to Django model choices.
fn map_day_night(value: &str) -> &str {
    match value {
        "DAY" => "day",
        "NIGHT" => "night",
        "DAWN" => "dawn",
        "DUSK" => "dusk",
        // Non-standard values default to day
        _ => "day",
    }
}
