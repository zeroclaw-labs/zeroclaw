//! Local keybinding presets and override resolution.
//!
//! A preset is a named, COMPLETE keymap built from the typed action
//! enums and `Chord` constructors — never authored `"tag.variant"` or
//! `"ctrl+p"` strings. Picking a preset fully overwrites the
//! `[keybindings]` table, so every preset must define every rebindable
//! action; a test enforces it. Presets start from the full default map
//! and reassign the actions they change. Walked from `KEY_PRESETS`,
//! mirroring `THEMES`.

use std::collections::HashMap;

use anyhow::{Result, bail};
use crossterm::event::KeyCode;

use crate::keymap::{
    ChatTabAction, Chord, ConfigTabAction, DashboardTabAction, FileExplorerAction, GlobalAction,
    InputBarAction, LogsTabAction, QuickstartTabAction, RebindableActions,
    overrides::OverrideTable,
};

/// Default preset name — the complete compile-time keymap.
pub const DEFAULT_PRESET_NAME: &str = "default";

/// A named keybinding preset. `build` returns the COMPLETE
/// `action_key -> chords` map (every rebindable action present).
#[derive(Clone, Copy)]
pub struct KeyPreset {
    pub build: fn() -> Vec<(String, Vec<Chord>)>,
}

/// The complete default keymap: every variant of every rebindable enum
/// mapped to its compile-time default chords. The base every preset
/// starts from so completeness is automatic.
fn all_defaults() -> HashMap<String, Vec<Chord>> {
    let mut map = HashMap::new();
    fill_defaults::<GlobalAction>(&mut map);
    fill_defaults::<ChatTabAction>(&mut map);
    fill_defaults::<LogsTabAction>(&mut map);
    fill_defaults::<DashboardTabAction>(&mut map);
    fill_defaults::<ConfigTabAction>(&mut map);
    fill_defaults::<QuickstartTabAction>(&mut map);
    fill_defaults::<InputBarAction>(&mut map);
    fill_defaults::<FileExplorerAction>(&mut map);
    map
}

fn fill_defaults<A: RebindableActions>(map: &mut HashMap<String, Vec<Chord>>) {
    for v in A::all() {
        map.insert(v.key(), v.defaults());
    }
}

/// Every rebindable action key — used by the completeness test and to
/// size preset maps.
fn all_action_keys() -> Vec<String> {
    all_defaults().into_keys().collect()
}

/// Materialise a complete preset map: start from defaults, apply the
/// caller's per-action reassignments. Each reassignment is the FULL
/// chord set for that action (it replaces, within the complete map).
fn from_defaults(changes: Vec<(String, Vec<Chord>)>) -> Vec<(String, Vec<Chord>)> {
    let mut map = all_defaults();
    for (key, chords) in changes {
        map.insert(key, chords);
    }
    map.into_iter().collect()
}

fn default_rows() -> Vec<(String, Vec<Chord>)> {
    all_defaults().into_iter().collect()
}

fn emacs_rows() -> Vec<(String, Vec<Chord>)> {
    // Emacs motion ADDED alongside the kept defaults (arrows etc.).
    let with = |action: &str, extra: Vec<Chord>| -> (String, Vec<Chord>) {
        let mut chords = default_chords_for(action);
        for c in extra {
            if !chords.contains(&c) {
                chords.push(c);
            }
        }
        (action.to_string(), chords)
    };
    from_defaults(vec![
        with(&DashboardTabAction::Up.action_key(), vec![Chord::ctrl('p')]),
        with(
            &DashboardTabAction::Down.action_key(),
            vec![Chord::ctrl('n')],
        ),
        with(&LogsTabAction::Up.action_key(), vec![Chord::ctrl('p')]),
        with(&LogsTabAction::Down.action_key(), vec![Chord::ctrl('n')]),
        with(&FileExplorerAction::Up.action_key(), vec![Chord::ctrl('p')]),
        with(
            &FileExplorerAction::Down.action_key(),
            vec![Chord::ctrl('n')],
        ),
    ])
}

fn vim_rows() -> Vec<(String, Vec<Chord>)> {
    // Vim motion ADDED alongside the kept defaults (arrows + Tab survive).
    let with = |action: &str, extra: Vec<Chord>| -> (String, Vec<Chord>) {
        let mut chords = default_chords_for(action);
        for c in extra {
            if !chords.contains(&c) {
                chords.push(c);
            }
        }
        (action.to_string(), chords)
    };
    from_defaults(vec![
        with(&DashboardTabAction::Up.action_key(), vec![Chord::char('k')]),
        with(
            &DashboardTabAction::Down.action_key(),
            vec![Chord::char('j')],
        ),
        with(
            &DashboardTabAction::PrevTab.action_key(),
            vec![Chord::char('h')],
        ),
        with(
            &DashboardTabAction::NextTab.action_key(),
            vec![Chord::char('l')],
        ),
        with(
            &DashboardTabAction::JumpStart.action_key(),
            vec![Chord::char('g')],
        ),
        with(
            &DashboardTabAction::JumpEnd.action_key(),
            vec![Chord::char('G')],
        ),
        with(&LogsTabAction::Up.action_key(), vec![Chord::char('k')]),
        with(&LogsTabAction::Down.action_key(), vec![Chord::char('j')]),
        with(
            &LogsTabAction::JumpStart.action_key(),
            vec![Chord::char('g')],
        ),
        with(&LogsTabAction::JumpEnd.action_key(), vec![Chord::char('G')]),
        with(&FileExplorerAction::Up.action_key(), vec![Chord::char('k')]),
        with(
            &FileExplorerAction::Down.action_key(),
            vec![Chord::char('j')],
        ),
        with(
            &FileExplorerAction::JumpStart.action_key(),
            vec![Chord::char('g')],
        ),
        with(
            &FileExplorerAction::JumpEnd.action_key(),
            vec![Chord::char('G')],
        ),
    ])
}

fn arrows_only_rows() -> Vec<(String, Vec<Chord>)> {
    // Arrows REPLACE vim letters on the motion actions (full set per row).
    from_defaults(vec![
        (
            DashboardTabAction::Up.action_key(),
            vec![Chord::key(KeyCode::Up)],
        ),
        (
            DashboardTabAction::Down.action_key(),
            vec![Chord::key(KeyCode::Down)],
        ),
        (
            DashboardTabAction::NextTab.action_key(),
            vec![Chord::key(KeyCode::Tab), Chord::key(KeyCode::Right)],
        ),
        (
            DashboardTabAction::PrevTab.action_key(),
            vec![Chord::key(KeyCode::BackTab), Chord::key(KeyCode::Left)],
        ),
        (
            LogsTabAction::Up.action_key(),
            vec![Chord::key(KeyCode::Up)],
        ),
        (
            LogsTabAction::Down.action_key(),
            vec![Chord::key(KeyCode::Down)],
        ),
        (
            FileExplorerAction::Up.action_key(),
            vec![Chord::key(KeyCode::Up)],
        ),
        (
            FileExplorerAction::Down.action_key(),
            vec![Chord::key(KeyCode::Down)],
        ),
    ])
}

/// The compile-time default chords for one action key.
fn default_chords_for(action_key: &str) -> Vec<Chord> {
    all_defaults().get(action_key).cloned().unwrap_or_default()
}

/// Registry of named presets. Walked by the zerocode tab's preset picker.
pub const KEY_PRESETS: &[(&str, KeyPreset)] = &[
    (
        DEFAULT_PRESET_NAME,
        KeyPreset {
            build: default_rows,
        },
    ),
    ("vim", KeyPreset { build: vim_rows }),
    ("emacs", KeyPreset { build: emacs_rows }),
    (
        "arrows_only",
        KeyPreset {
            build: arrows_only_rows,
        },
    ),
];

pub fn preset_names() -> impl Iterator<Item = &'static str> {
    KEY_PRESETS.iter().map(|(n, _)| *n)
}

pub fn preset_by_name(name: &str) -> Option<&'static KeyPreset> {
    KEY_PRESETS
        .iter()
        .find_map(|(n, p)| (*n == name).then_some(p))
}

impl KeyPreset {
    /// Resolve into a validated override table keyed `tag -> variant ->
    /// chords`, running the full validation battery.
    pub fn resolve(&self) -> Result<OverrideTable> {
        let rows: HashMap<String, Vec<Chord>> = (self.build)().into_iter().collect();
        build_override_table(rows)
    }
}

/// Turn a sparse `action_key -> chords` map into the nested
/// `tag -> variant -> chords` override table, validating intra-action
/// duplicates and intra-tag chord uniqueness. Reserved-chord rejection
/// lives on the capture-modal path only — the compile-time defaults
/// legitimately use Enter/Esc (e.g. open-detail, cancel), so blocking
/// them here would reject the baseline itself.
pub fn build_override_table(rows: HashMap<String, Vec<Chord>>) -> Result<OverrideTable> {
    let mut table: OverrideTable = HashMap::new();
    let mut seen: HashMap<String, HashMap<Chord, String>> = HashMap::new();

    for (action_key, chords) in rows {
        let (tag, variant) = action_key.split_once('.').ok_or_else(|| {
            anyhow::Error::msg(format!(
                "keybinding key '{action_key}' missing '.<variant>'"
            ))
        })?;

        for (i, a) in chords.iter().enumerate() {
            if chords[i + 1..].contains(a) {
                bail!("'{action_key}' lists '{}' twice", a.wire());
            }
        }
        let tag_seen = seen.entry(tag.to_string()).or_default();
        for c in &chords {
            if let Some(other) = tag_seen.get(c) {
                bail!(
                    "chord '{}' bound to both '{action_key}' and '{other}'",
                    c.wire()
                );
            }
            tag_seen.insert(c.clone(), action_key.clone());
        }

        table
            .entry(tag.to_string())
            .or_default()
            .insert(variant.to_string(), chords);
    }
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_preset_is_complete() {
        let t = preset_by_name(DEFAULT_PRESET_NAME)
            .unwrap()
            .resolve()
            .unwrap();
        assert!(!t.is_empty());
    }

    /// Every preset must define EVERY rebindable action — full overwrite
    /// means a missing action would silently lose its binding. This is
    /// the invariant that makes validation tractable.
    #[test]
    fn every_preset_covers_every_action() {
        let expected: std::collections::BTreeSet<String> = all_action_keys().into_iter().collect();
        for name in preset_names() {
            let rows = (preset_by_name(name).unwrap().build)();
            let got: std::collections::BTreeSet<String> =
                rows.into_iter().map(|(k, _)| k).collect();
            let missing: Vec<&String> = expected.difference(&got).collect();
            let extra: Vec<&String> = got.difference(&expected).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "preset '{name}' incomplete — missing: {missing:?}, unknown: {extra:?}"
            );
        }
    }

    #[test]
    fn every_preset_resolves_and_is_clean() {
        for name in preset_names() {
            preset_by_name(name)
                .unwrap()
                .resolve()
                .unwrap_or_else(|e| panic!("preset '{name}' invalid: {e}"));
        }
    }

    #[test]
    fn preset_names_are_snake_case() {
        let ok = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                && !s.starts_with('_')
                && !s.ends_with('_')
        };
        for name in preset_names() {
            assert!(ok(name), "preset name '{name}' is not snake_case");
        }
    }

    #[test]
    fn reserved_chord_allowed_in_table_guarded_at_capture() {
        // The compile-time defaults legitimately use Enter/Esc, so table
        // building must accept reserved chords; rejection is the capture
        // modal's job (tested via keymap::reserved_reason).
        let mut rows = HashMap::new();
        rows.insert("chat.scroll_up".to_string(), vec![Chord::key(KeyCode::Esc)]);
        assert!(build_override_table(rows).is_ok());
        assert!(crate::keymap::reserved_reason(&Chord::key(KeyCode::Esc)).is_some());
    }

    #[test]
    fn intra_tag_chord_clash_is_rejected() {
        let mut rows = HashMap::new();
        rows.insert("dashboard.up".to_string(), vec![Chord::char('z')]);
        rows.insert("dashboard.down".to_string(), vec![Chord::char('z')]);
        assert!(build_override_table(rows).is_err());
    }

    #[test]
    fn intra_action_duplicate_is_rejected() {
        let mut rows = HashMap::new();
        rows.insert(
            "dashboard.up".to_string(),
            vec![Chord::char('z'), Chord::char('z')],
        );
        assert!(build_override_table(rows).is_err());
    }
}
