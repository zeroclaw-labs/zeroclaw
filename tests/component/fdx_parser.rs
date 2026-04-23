//! Integration tests for the FDX parser using reference fixture files.

use lightwave_sys::fdx::parser::parse_fdx;

fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/fdx/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("failed to load fixture {path}: {e}"))
}

#[test]
fn iron_dan_scene_count() {
    let scenes = parse_fdx(&load_fixture("iron_dan.fdx")).unwrap();
    assert_eq!(scenes.len(), 4, "IRON DAN should have 4 scenes");
}

#[test]
fn iron_dan_scene_headings() {
    let scenes = parse_fdx(&load_fixture("iron_dan.fdx")).unwrap();

    assert_eq!(scenes[0].int_ext, "INT");
    assert_eq!(scenes[0].location, "BOXING GYM");
    assert_eq!(scenes[0].day_night, "NIGHT");

    assert_eq!(scenes[1].int_ext, "EXT");
    assert_eq!(scenes[1].location, "DAN'S APARTMENT");
    assert_eq!(scenes[1].day_night, "DAWN");

    assert_eq!(scenes[2].int_ext, "INT");
    assert_eq!(scenes[2].location, "BOXING GYM");
    assert_eq!(scenes[2].day_night, "DAY");

    assert_eq!(scenes[3].int_ext, "INT/EXT");
    assert_eq!(scenes[3].location, "MOVING CAR");
    assert_eq!(scenes[3].day_night, "NIGHT");
}

#[test]
fn iron_dan_dialogue_characters() {
    let scenes = parse_fdx(&load_fixture("iron_dan.fdx")).unwrap();

    // Scene 1: IRON DAN and COACH RUIZ dialogue
    let s1_chars: Vec<&str> = scenes[0]
        .dialogue_blocks
        .iter()
        .map(|d| d.character.as_str())
        .collect();
    assert!(
        s1_chars.contains(&"IRON DAN"),
        "Scene 1 should have IRON DAN dialogue"
    );
    assert!(
        s1_chars.contains(&"COACH RUIZ"),
        "Scene 1 should have COACH RUIZ dialogue"
    );

    // Scene 2: IRON DAN and ELENA dialogue
    let s2_chars: Vec<&str> = scenes[1]
        .dialogue_blocks
        .iter()
        .map(|d| d.character.as_str())
        .collect();
    assert!(s2_chars.contains(&"IRON DAN"));
    assert!(s2_chars.contains(&"ELENA"));
}

#[test]
fn iron_dan_action_blocks() {
    let scenes = parse_fdx(&load_fixture("iron_dan.fdx")).unwrap();

    // Scene 1 has multiple action blocks
    assert!(
        scenes[0].action_blocks.len() >= 2,
        "Scene 1 should have multiple action blocks"
    );
    // First action describes the gym
    assert!(scenes[0].action_blocks[0].contains("boxing gym"));
}

#[test]
fn iron_dan_os_character_stripped() {
    let scenes = parse_fdx(&load_fixture("iron_dan.fdx")).unwrap();

    // Scene 2 has ELENA (O.S.) — the (O.S.) should be stripped
    let elena_blocks: Vec<_> = scenes[1]
        .dialogue_blocks
        .iter()
        .filter(|d| d.character == "ELENA")
        .collect();
    assert!(
        !elena_blocks.is_empty(),
        "ELENA should appear (O.S. stripped from character name)"
    );
}

#[test]
fn embers_scene_count() {
    let scenes = parse_fdx(&load_fixture("embers.fdx")).unwrap();
    assert_eq!(scenes.len(), 4, "EMBERS should have 4 scenes");
}

#[test]
fn embers_scene_headings() {
    let scenes = parse_fdx(&load_fixture("embers.fdx")).unwrap();

    assert_eq!(scenes[0].int_ext, "EXT");
    assert_eq!(scenes[0].location, "WILDFIRE RIDGE");
    assert_eq!(scenes[0].day_night, "DUSK");

    assert_eq!(scenes[1].int_ext, "INT");
    assert_eq!(scenes[1].location, "INCIDENT COMMAND POST");
    assert_eq!(scenes[1].day_night, "NIGHT");

    assert_eq!(scenes[2].int_ext, "EXT");
    assert_eq!(scenes[2].location, "WILDFIRE RIDGE");
    assert_eq!(scenes[2].day_night, "CONTINUOUS");

    assert_eq!(scenes[3].int_ext, "EXT");
    assert_eq!(scenes[3].location, "BURNED RIDGE");
    assert_eq!(scenes[3].day_night, "DAWN");
}

#[test]
fn embers_dialogue_characters() {
    let scenes = parse_fdx(&load_fixture("embers.fdx")).unwrap();

    // Scene 3 should have CAPTAIN MAYA CHEN, DIAZ, ROOKIE FIREFIGHTER
    let s3_chars: Vec<&str> = scenes[2]
        .dialogue_blocks
        .iter()
        .map(|d| d.character.as_str())
        .collect();
    assert!(s3_chars.contains(&"CAPTAIN MAYA CHEN"));
    assert!(s3_chars.contains(&"DIAZ"));
    assert!(s3_chars.contains(&"ROOKIE FIREFIGHTER"));
}

#[test]
fn embers_page_counts_reasonable() {
    let scenes = parse_fdx(&load_fixture("embers.fdx")).unwrap();
    let total: f64 = scenes.iter().map(|s| s.page_count).sum();
    // A 4-scene extract should be roughly 1-4 pages
    assert!(total > 0.5, "total page count should be > 0.5");
    assert!(
        total < 10.0,
        "total page count should be < 10 for this short extract"
    );
}

#[test]
fn both_fixtures_sequential_scene_numbers() {
    for fixture in &["iron_dan.fdx", "embers.fdx"] {
        let scenes = parse_fdx(&load_fixture(fixture)).unwrap();
        for (i, scene) in scenes.iter().enumerate() {
            assert_eq!(
                scene.scene_number,
                (i + 1) as u32,
                "scene_number should be sequential in {fixture}"
            );
        }
    }
}
