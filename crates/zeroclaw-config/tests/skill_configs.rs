//! Integration tests for `SkillForgeConfig` and `SkillConfidenceConfig`.
//!
//! These live outside `src/schema.rs::tests` because that inline module has
//! a pre-existing compile issue (requires `tokio-stream/fs` feature not
//! enabled in dev-deps). Moving these into a standalone integration test
//! file lets them run today without waiting on that unrelated fix.

use zeroclaw_config::schema::{Config, SkillConfidenceConfig, SkillForgeConfig, SkillsConfig};

// ── SkillForgeConfig ────────────────────────────────────────────

#[test]
fn skill_forge_config_defaults_are_safe() {
    let c = SkillForgeConfig::default();
    assert!(!c.enabled, "disabled by default (opt-in)");
    assert!(c.auto_integrate);
    assert!(c.sandbox_verify, "sandbox on by default");
    assert_eq!(c.sandbox_timeout_secs, 30);
    assert_eq!(c.scan_interval_hours, 24);
    assert_eq!(c.sources, vec!["github", "clawhub"]);
}

#[test]
fn skill_forge_config_roundtrips_through_toml() {
    let input = r#"
[skills.skill_forge]
enabled = true
auto_integrate = false
sources = ["github"]
scan_interval_hours = 12
min_score = 0.85
output_dir = "/var/lib/zeroclaw/skills"
sandbox_verify = false
sandbox_timeout_secs = 60
"#;
    let config: Config = toml::from_str(input).expect("config should parse");
    let f = &config.skills.skill_forge;
    assert!(f.enabled);
    assert!(!f.auto_integrate);
    assert_eq!(f.sources, vec!["github"]);
    assert_eq!(f.scan_interval_hours, 12);
    assert!((f.min_score - 0.85).abs() < f64::EPSILON);
    assert_eq!(f.output_dir, "/var/lib/zeroclaw/skills");
    assert!(!f.sandbox_verify);
    assert_eq!(f.sandbox_timeout_secs, 60);
}

#[test]
fn skill_forge_config_missing_section_uses_defaults() {
    let config: Config = toml::from_str("").expect("empty config parses");
    let f = &config.skills.skill_forge;
    assert!(!f.enabled);
    assert!(f.sandbox_verify);
}

#[test]
fn skill_forge_config_debug_redacts_github_token() {
    let cfg = SkillForgeConfig {
        github_token: Some("ghp_secret_abc123".into()),
        ..Default::default()
    };
    let dbg = format!("{cfg:?}");
    assert!(
        !dbg.contains("ghp_secret_abc123"),
        "Debug output must never leak the raw token: {dbg}"
    );
    assert!(dbg.contains("***"), "expected redaction sentinel: {dbg}");
}

#[test]
fn skill_forge_config_debug_shows_none_when_no_token() {
    let cfg = SkillForgeConfig::default();
    let dbg = format!("{cfg:?}");
    assert!(dbg.contains("github_token"));
    assert!(dbg.contains("None"));
    assert!(!dbg.contains("***"));
}

// ── SkillConfidenceConfig ───────────────────────────────────────

#[test]
fn skill_confidence_config_defaults_are_reasonable() {
    let c = SkillConfidenceConfig::default();
    assert!(c.enabled, "enabled by default");
    assert_eq!(c.scan_interval_hours, 6);
    assert_eq!(c.saturation_calls, 20);
    assert!((c.recency_half_life_hours - 720.0).abs() < f64::EPSILON);
    // C-2: default raised from 5 → 15 so transient noise doesn't
    // permanently deprecate skills.
    assert_eq!(c.min_samples_for_deprecation, 15);
    assert!((c.deprecation_threshold - 0.3).abs() < f64::EPSILON);
    // C-2: review window on by default (7 days), with hysteresis gap
    // above deprecation threshold.
    assert_eq!(c.review_window_hours, 24 * 7);
    assert!((c.reinstate_threshold - 0.5).abs() < f64::EPSILON);
    assert!(
        c.reinstate_threshold > c.deprecation_threshold,
        "hysteresis invariant: reinstate > deprecate"
    );
}

#[test]
fn skill_confidence_config_roundtrips_through_toml() {
    let input = r#"
[skills.skill_confidence]
enabled = false
scan_interval_hours = 2
saturation_calls = 100
recency_half_life_hours = 168.0
min_samples_for_deprecation = 20
deprecation_threshold = 0.15
review_window_hours = 48
reinstate_threshold = 0.65
"#;
    let config: Config = toml::from_str(input).expect("config should parse");
    let c = &config.skills.skill_confidence;
    assert!(!c.enabled);
    assert_eq!(c.scan_interval_hours, 2);
    assert_eq!(c.saturation_calls, 100);
    assert!((c.recency_half_life_hours - 168.0).abs() < f64::EPSILON);
    assert_eq!(c.min_samples_for_deprecation, 20);
    assert!((c.deprecation_threshold - 0.15).abs() < f64::EPSILON);
    assert_eq!(c.review_window_hours, 48);
    assert!((c.reinstate_threshold - 0.65).abs() < f64::EPSILON);
}

#[test]
fn skill_confidence_config_review_disabled_when_window_zero() {
    // Operators can disable auto-reinstatement by setting review_window_hours = 0.
    let input = r#"
[skills.skill_confidence]
review_window_hours = 0
"#;
    let config: Config = toml::from_str(input).expect("config should parse");
    assert_eq!(config.skills.skill_confidence.review_window_hours, 0);
}

#[test]
fn skills_config_includes_new_sections() {
    let s = SkillsConfig::default();
    assert!(!s.skill_forge.enabled);
    assert!(s.skill_confidence.enabled);
}
