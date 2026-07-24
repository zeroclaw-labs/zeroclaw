//! Regression for a malformed `[plugins.entries]`
//! (table instead of array-of-tables) must be reported as a dropped section,
//! never silently swallowed into defaults.

use zeroclaw_config::migration::migrate_to_current_salvaged;

#[test]
fn malformed_plugins_entries_reports_dropped_section() {
    let bad = r#"
schema_version = 3

[plugins]
enabled = true

[plugins.entries]
name = "translator"
"#;
    let salvage = migrate_to_current_salvaged(bad);
    assert_eq!(salvage.dropped, vec!["plugins".to_string()]);
    assert!(salvage.dropped_security.is_empty());
    assert!(
        !salvage.config.plugins.enabled,
        "section resets to defaults"
    );
}
