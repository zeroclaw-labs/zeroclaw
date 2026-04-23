//! FDX XML parser using `quick-xml`.
//!
//! Parses Final Draft `.fdx` files. The FDX format uses `<Paragraph>` elements
//! with a `Type` attribute to distinguish scene headings, action, character names,
//! dialogue, parentheticals, and transitions.
//!
//! # XXE Safety
//!
//! `quick-xml` does not resolve external entities or DTD references by default,
//! making it safe against XXE attacks without additional configuration.
//!
//! # Timeout
//!
//! The public [`parse_fdx`] function enforces a 30-second wall-clock timeout.

use std::time::{Duration, Instant};

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use thiserror::Error;

use super::{DialogueBlock, SceneData};

/// Default parse timeout (30 seconds).
const PARSE_TIMEOUT: Duration = Duration::from_secs(30);

/// Lines per page in standard screenplay format.
const LINES_PER_PAGE: f64 = 56.0;

/// Errors that can occur during FDX parsing.
#[derive(Debug, Error)]
pub enum FdxParseError {
    #[error("XML parse error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("invalid UTF-8 in XML attribute: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("parse timeout exceeded ({0:?})")]
    Timeout(Duration),
    #[error("not a valid FDX file: missing <FinalDraft> root element")]
    NotFdx,
}

/// Parse FDX bytes into structured scene data.
///
/// Enforces a 30-second timeout. Returns an empty `Vec` if the file
/// contains no scene headings.
pub fn parse_fdx(bytes: &[u8]) -> Result<Vec<SceneData>, FdxParseError> {
    parse_fdx_with_timeout(bytes, PARSE_TIMEOUT)
}

/// Parse FDX bytes with a configurable timeout (for testing).
pub fn parse_fdx_with_timeout(
    bytes: &[u8],
    timeout: Duration,
) -> Result<Vec<SceneData>, FdxParseError> {
    let start = Instant::now();
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);

    let mut scenes: Vec<SceneData> = Vec::new();
    let mut scene_number: u32 = 0;

    // Current paragraph state
    let mut in_content = false;
    let mut found_root = false;
    let mut current_para_type = ParaType::Unknown;
    let mut text_buf = String::new();
    let mut in_text = false;

    // Current scene accumulator
    let mut current_scene: Option<SceneBuilder> = None;

    // Pending character name for dialogue pairing
    let mut pending_character: Option<String> = None;

    let mut buf = Vec::new();

    loop {
        if start.elapsed() > timeout {
            return Err(FdxParseError::Timeout(timeout));
        }

        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let qname = e.name();
                let name = std::str::from_utf8(qname.as_ref())?;
                match name {
                    "FinalDraft" => found_root = true,
                    "Content" if found_root => in_content = true,
                    "Paragraph" if in_content => {
                        current_para_type = para_type_from_attrs(e);
                        text_buf.clear();
                    }
                    "Text" if in_content => in_text = true,
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let qname = e.name();
                let name = std::str::from_utf8(qname.as_ref())?;
                match name {
                    "Content" => in_content = false,
                    "Text" => in_text = false,
                    "Paragraph" if in_content => {
                        let text = text_buf.trim().to_string();
                        if text.is_empty() {
                            current_para_type = ParaType::Unknown;
                            continue;
                        }

                        match current_para_type {
                            ParaType::SceneHeading => {
                                // Finalize previous scene
                                if let Some(builder) = current_scene.take() {
                                    scenes.push(builder.build());
                                }
                                pending_character = None;
                                scene_number += 1;
                                let (int_ext, location, day_night) = parse_scene_heading(&text);
                                current_scene = Some(SceneBuilder {
                                    scene_number,
                                    int_ext,
                                    location,
                                    day_night,
                                    action_blocks: Vec::new(),
                                    dialogue_blocks: Vec::new(),
                                    line_count: 1, // heading itself
                                });
                            }
                            ParaType::Action => {
                                if let Some(ref mut scene) = current_scene {
                                    let lines = count_lines(&text);
                                    scene.line_count += lines;
                                    scene.action_blocks.push(text);
                                }
                                pending_character = None;
                            }
                            ParaType::Character => {
                                pending_character = Some(
                                    text.trim_end_matches("(CONT'D)")
                                        .trim_end_matches("(V.O.)")
                                        .trim_end_matches("(O.S.)")
                                        .trim()
                                        .to_string(),
                                );
                                if let Some(ref mut scene) = current_scene {
                                    scene.line_count += 1;
                                }
                            }
                            ParaType::Dialogue => {
                                if let Some(ref mut scene) = current_scene {
                                    let char_name = pending_character.take().unwrap_or_default();
                                    let lines = count_lines(&text);
                                    scene.line_count += lines;
                                    scene.dialogue_blocks.push(DialogueBlock {
                                        character: char_name,
                                        text,
                                    });
                                }
                            }
                            ParaType::Parenthetical => {
                                // Parentheticals count as lines but don't
                                // produce separate blocks — they're part of
                                // the dialogue flow. Keep pending_character.
                                if let Some(ref mut scene) = current_scene {
                                    scene.line_count += 1;
                                }
                            }
                            ParaType::Transition | ParaType::Unknown => {
                                if let Some(ref mut scene) = current_scene {
                                    scene.line_count += 1;
                                }
                                pending_character = None;
                            }
                        }
                        current_para_type = ParaType::Unknown;
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) if in_text => {
                let t = e.unescape()?;
                if !text_buf.is_empty() {
                    text_buf.push(' ');
                }
                text_buf.push_str(&t);
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(FdxParseError::Xml(e)),
            _ => {}
        }
        buf.clear();
    }

    // Finalize last scene
    if let Some(builder) = current_scene.take() {
        scenes.push(builder.build());
    }

    if !found_root && !scenes.is_empty() {
        return Err(FdxParseError::NotFdx);
    }

    Ok(scenes)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParaType {
    SceneHeading,
    Action,
    Character,
    Dialogue,
    Parenthetical,
    Transition,
    Unknown,
}

fn para_type_from_attrs(e: &quick_xml::events::BytesStart<'_>) -> ParaType {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == b"Type" {
            let val = String::from_utf8_lossy(&attr.value);
            return match val.as_ref() {
                "Scene Heading" => ParaType::SceneHeading,
                "Action" => ParaType::Action,
                "Character" => ParaType::Character,
                "Dialogue" => ParaType::Dialogue,
                "Parenthetical" => ParaType::Parenthetical,
                "Transition" => ParaType::Transition,
                _ => ParaType::Unknown,
            };
        }
    }
    ParaType::Unknown
}

/// Parse a scene heading like "INT. COFFEE SHOP - DAY" into components.
fn parse_scene_heading(heading: &str) -> (String, String, String) {
    let heading = heading.trim();

    // Extract INT/EXT prefix
    let (int_ext, rest) = extract_int_ext(heading);

    // The remainder is "LOCATION - TIME_OF_DAY"
    // Split on the last " - " to separate location from time
    let (location, day_night) = if let Some(dash_pos) = rest.rfind(" - ") {
        let loc = rest[..dash_pos].trim().to_string();
        let tod = rest[dash_pos + 3..].trim().to_uppercase();
        (loc, normalize_time_of_day(&tod))
    } else {
        (rest.trim().to_string(), String::new())
    };

    (int_ext, location, day_night)
}

fn extract_int_ext(heading: &str) -> (String, &str) {
    let upper = heading.to_uppercase();

    // Order matters: check compound forms first
    for prefix in &["INT./EXT.", "INT/EXT.", "INT./EXT", "INT/EXT", "I/E."] {
        if upper.starts_with(prefix) {
            let rest = &heading[prefix.len()..];
            let rest = rest.strip_prefix('.').unwrap_or(rest);
            return ("INT/EXT".to_string(), rest.trim_start());
        }
    }
    for (prefix, label) in &[
        ("INT.", "INT"),
        ("EXT.", "EXT"),
        ("INT ", "INT"),
        ("EXT ", "EXT"),
    ] {
        if upper.starts_with(prefix) {
            return ((*label).to_string(), heading[prefix.len()..].trim_start());
        }
    }
    (String::new(), heading)
}

fn normalize_time_of_day(tod: &str) -> String {
    match tod {
        "DAY" | "MORNING" | "AFTERNOON" => "DAY".to_string(),
        "NIGHT" | "EVENING" => "NIGHT".to_string(),
        "DAWN" | "SUNRISE" => "DAWN".to_string(),
        "DUSK" | "SUNSET" | "MAGIC HOUR" => "DUSK".to_string(),
        "CONTINUOUS" | "LATER" | "MOMENTS LATER" | "SAME" => tod.to_string(),
        other => other.to_string(),
    }
}

/// Estimate line count for page calculation.
fn count_lines(text: &str) -> u32 {
    // ~60 chars per line in standard screenplay format.
    // Screenplay text is always short enough for u32.
    let chars = u32::try_from(text.len()).unwrap_or(u32::MAX);
    (chars / 60).max(1)
}

struct SceneBuilder {
    scene_number: u32,
    int_ext: String,
    location: String,
    day_night: String,
    action_blocks: Vec<String>,
    dialogue_blocks: Vec<DialogueBlock>,
    line_count: u32,
}

impl SceneBuilder {
    fn build(self) -> SceneData {
        // Page count: lines / 56, rounded to nearest 1/8th page
        let raw_pages = f64::from(self.line_count) / LINES_PER_PAGE;
        let page_count = (raw_pages * 8.0).ceil() / 8.0;

        SceneData {
            scene_number: self.scene_number,
            int_ext: self.int_ext,
            location: self.location,
            day_night: self.day_night,
            action_blocks: self.action_blocks,
            dialogue_blocks: self.dialogue_blocks,
            page_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE_FDX: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
<Content>
<Paragraph Type="Scene Heading">
<Text>INT. COFFEE SHOP - DAY</Text>
</Paragraph>
<Paragraph Type="Action">
<Text>A busy coffee shop. SARAH (30s) sits at a corner table, laptop open.</Text>
</Paragraph>
<Paragraph Type="Character">
<Text>SARAH</Text>
</Paragraph>
<Paragraph Type="Dialogue">
<Text>I can't believe you actually showed up.</Text>
</Paragraph>
<Paragraph Type="Character">
<Text>DAN</Text>
</Paragraph>
<Paragraph Type="Dialogue">
<Text>You asked me to come.</Text>
</Paragraph>
<Paragraph Type="Scene Heading">
<Text>EXT. PARKING LOT - NIGHT</Text>
</Paragraph>
<Paragraph Type="Action">
<Text>Rain hammers the asphalt. Dan sprints toward his car.</Text>
</Paragraph>
</Content>
</FinalDraft>"#;

    #[test]
    fn parse_simple_fdx() {
        let scenes = parse_fdx(SIMPLE_FDX.as_bytes()).unwrap();
        assert_eq!(scenes.len(), 2);

        let s1 = &scenes[0];
        assert_eq!(s1.scene_number, 1);
        assert_eq!(s1.int_ext, "INT");
        assert_eq!(s1.location, "COFFEE SHOP");
        assert_eq!(s1.day_night, "DAY");
        assert_eq!(s1.action_blocks.len(), 1);
        assert_eq!(s1.dialogue_blocks.len(), 2);
        assert_eq!(s1.dialogue_blocks[0].character, "SARAH");
        assert_eq!(s1.dialogue_blocks[1].character, "DAN");

        let s2 = &scenes[1];
        assert_eq!(s2.scene_number, 2);
        assert_eq!(s2.int_ext, "EXT");
        assert_eq!(s2.location, "PARKING LOT");
        assert_eq!(s2.day_night, "NIGHT");
        assert_eq!(s2.action_blocks.len(), 1);
        assert_eq!(s2.dialogue_blocks.len(), 0);
    }

    #[test]
    fn parse_empty_fdx() {
        let fdx = r#"<?xml version="1.0" encoding="UTF-8"?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
<Content>
</Content>
</FinalDraft>"#;
        let scenes = parse_fdx(fdx.as_bytes()).unwrap();
        assert!(scenes.is_empty());
    }

    #[test]
    fn parse_int_ext_compound() {
        let fdx = r#"<?xml version="1.0" encoding="UTF-8"?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
<Content>
<Paragraph Type="Scene Heading">
<Text>INT./EXT. MOVING CAR - DAY</Text>
</Paragraph>
<Paragraph Type="Action">
<Text>The car weaves through traffic.</Text>
</Paragraph>
</Content>
</FinalDraft>"#;
        let scenes = parse_fdx(fdx.as_bytes()).unwrap();
        assert_eq!(scenes[0].int_ext, "INT/EXT");
        assert_eq!(scenes[0].location, "MOVING CAR");
    }

    #[test]
    fn timeout_enforced() {
        // A zero-duration timeout should fail immediately on any non-trivial input
        let result = parse_fdx_with_timeout(SIMPLE_FDX.as_bytes(), Duration::ZERO);
        assert!(matches!(result, Err(FdxParseError::Timeout(_))));
    }

    #[test]
    fn xxe_entities_not_resolved() {
        // quick-xml does not resolve external entities — this should parse
        // without fetching anything or expanding the entity reference.
        let fdx = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE foo [
  <!ENTITY xxe SYSTEM "file:///etc/passwd">
]>
<FinalDraft DocumentType="Script" Template="No" Version="5">
<Content>
<Paragraph Type="Scene Heading">
<Text>INT. OFFICE - DAY</Text>
</Paragraph>
<Paragraph Type="Action">
<Text>Normal action text.</Text>
</Paragraph>
</Content>
</FinalDraft>"#;
        let scenes = parse_fdx(fdx.as_bytes()).unwrap();
        assert_eq!(scenes.len(), 1);
        assert_eq!(scenes[0].location, "OFFICE");
        // The entity reference should NOT appear in any parsed text
        for scene in &scenes {
            for action in &scene.action_blocks {
                assert!(!action.contains("root:"));
            }
        }
    }

    #[test]
    fn scene_heading_variants() {
        let cases = vec![
            ("INT. KITCHEN - DAY", "INT", "KITCHEN", "DAY"),
            ("EXT. BEACH - SUNSET", "EXT", "BEACH", "DUSK"),
            ("INT./EXT. CAR - NIGHT", "INT/EXT", "CAR", "NIGHT"),
            ("INT. HALLWAY - CONTINUOUS", "INT", "HALLWAY", "CONTINUOUS"),
            ("EXT. ROOFTOP - DAWN", "EXT", "ROOFTOP", "DAWN"),
        ];
        for (heading, exp_ie, exp_loc, exp_tod) in cases {
            let (ie, loc, tod) = parse_scene_heading(heading);
            assert_eq!(ie, exp_ie, "int_ext for '{heading}'");
            assert_eq!(loc, exp_loc, "location for '{heading}'");
            assert_eq!(tod, exp_tod, "day_night for '{heading}'");
        }
    }

    #[test]
    fn character_cont_d_stripped() {
        let fdx = r#"<?xml version="1.0" encoding="UTF-8"?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
<Content>
<Paragraph Type="Scene Heading">
<Text>INT. ROOM - DAY</Text>
</Paragraph>
<Paragraph Type="Character">
<Text>SARAH (CONT'D)</Text>
</Paragraph>
<Paragraph Type="Dialogue">
<Text>Where was I?</Text>
</Paragraph>
</Content>
</FinalDraft>"#;
        let scenes = parse_fdx(fdx.as_bytes()).unwrap();
        assert_eq!(scenes[0].dialogue_blocks[0].character, "SARAH");
    }

    #[test]
    fn page_count_positive() {
        let scenes = parse_fdx(SIMPLE_FDX.as_bytes()).unwrap();
        for scene in &scenes {
            assert!(scene.page_count > 0.0);
            // Should be rounded to 1/8th page
            let eighths = scene.page_count * 8.0;
            assert!(
                (eighths - eighths.round()).abs() < f64::EPSILON,
                "page_count {pc} is not a 1/8th increment",
                pc = scene.page_count
            );
        }
    }
}
