//! FDX (Final Draft XML) screenplay parser.
//!
//! Parses Final Draft `.fdx` files into structured scene data.
//! Designed for use via PyO3 binding from Django/Celery tasks.

pub mod parser;

use serde::{Deserialize, Serialize};

/// Parsed scene data extracted from an FDX file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SceneData {
    /// Scene number (1-indexed, order of appearance).
    pub scene_number: u32,
    /// Interior/Exterior designation (e.g., "INT", "EXT", "INT/EXT").
    pub int_ext: String,
    /// Location name extracted from the scene heading.
    pub location: String,
    /// Time of day (e.g., "DAY", "NIGHT", "DAWN", "DUSK").
    pub day_night: String,
    /// Action/description paragraphs in the scene.
    pub action_blocks: Vec<String>,
    /// Dialogue blocks as (character_name, dialogue_text) pairs.
    pub dialogue_blocks: Vec<DialogueBlock>,
    /// Estimated page count for this scene (1/8th page increments).
    pub page_count: f64,
}

/// A single dialogue block: character name and their line.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DialogueBlock {
    pub character: String,
    pub text: String,
}
