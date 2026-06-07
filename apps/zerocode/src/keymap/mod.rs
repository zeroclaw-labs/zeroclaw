//! Keymap abstraction for zerocode.
//!
//! Each leaf action enum carries its own default bindings inline.
//! Consumers call `ChatTabAction::from_chord(&key)` directly — no
//! `Keymap` struct, no plumbed argument.
//!
//! On darwin, `Chord::matches` translates the `CTRL` modifier to
//! `SUPER` so Linux's `Ctrl+K` and macOS's `⌘K` resolve identically.

pub mod actions;
mod chord;
pub mod overrides;

pub use actions::*;
pub use chord::Chord;

use crossterm::event::KeyEvent;

/// Uniform interface over every `keyactions!`-generated enum so generic
/// code (the keybind surface) can walk variants, names, labels, and
/// resolved chords without knowing the concrete enum.
pub trait RebindableActions: Sized + Copy + 'static {
    fn tag() -> &'static str;
    fn all() -> &'static [Self];
    fn key(&self) -> String;
    fn human_label(&self) -> &'static str;
    fn defaults(&self) -> Vec<Chord>;
    fn resolved(&self) -> Vec<Chord>;
}

/// Bare chords reserved from user rebinding so structural controls
/// (cancel/back, confirm, selection toggle) can't be stolen and
/// soft-lock the TUI. The capture widget rejects these with the reason;
/// presets validate against the same set.
pub fn reserved_chords() -> &'static [(Chord, &'static str)] {
    use crossterm::event::KeyCode;
    use std::sync::OnceLock;
    static CELL: OnceLock<Vec<(Chord, &'static str)>> = OnceLock::new();
    CELL.get_or_init(|| {
        vec![
            (Chord::key(KeyCode::Esc), "reserved for cancel / back"),
            (Chord::key(KeyCode::Enter), "reserved for confirm"),
            (Chord::char(' '), "reserved for selection toggle"),
        ]
    })
}

/// Whether `chord` is a reserved bare control chord; returns the reason
/// when it is, so the capture widget can explain the rejection.
pub fn reserved_reason(chord: &Chord) -> Option<&'static str> {
    reserved_chords()
        .iter()
        .find_map(|(c, reason)| (c == chord).then_some(*reason))
}

pub fn match_chord<A: Copy>(table: &[(Chord, A)], event: &KeyEvent) -> Option<A> {
    table
        .iter()
        .find_map(|(c, a)| c.matches(event).then_some(*a))
}

/// Rendered, OS-aware key labels for an action's currently-resolved
/// chords (e.g. `["Tab"]`, `["⌘K"]`). Help surfaces use this so the keys
/// they advertise track the live keybinding registry instead of literals.
pub fn action_key_labels<A: RebindableActions>(action: A) -> Vec<String> {
    action.resolved().iter().map(Chord::display).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn global_quit_chord_resolves() {
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(GlobalAction::from_chord(&ev), Some(GlobalAction::Quit));
    }

    #[test]
    fn input_bar_enter_is_submit() {
        let ev = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            InputBarAction::from_chord(&ev),
            Some(InputBarAction::Submit)
        );
    }

    #[test]
    fn logs_enter_is_open_detail() {
        let ev = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            LogsTabAction::from_chord(&ev),
            Some(LogsTabAction::OpenDetail)
        );
    }

    #[test]
    fn labels_are_human_readable() {
        assert_eq!(GlobalAction::Quit.label(), "quit");
        assert_eq!(ChatTabAction::BrowseUpVim.label(), "browse prev (vim)");
        assert_eq!(InputBarAction::Submit.label(), "send");
    }

    #[test]
    fn actions_serde_round_trip() {
        let action = ChatTabAction::ScrollUp;
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, "\"scroll_up\"");
        let back: ChatTabAction = serde_json::from_str(&json).unwrap();
        assert_eq!(action, back);
    }

    /// Every action enum's binding table must have no duplicate chord
    /// keys (one chord → one action per enum). Runs as a unit test so
    /// the rejection is loud and reproducible in CI.
    #[test]
    fn no_intra_enum_chord_conflicts() {
        fn check<A: Copy + std::fmt::Debug>(label: &str, table: Vec<(Chord, A)>) {
            for (i, (c1, a1)) in table.iter().enumerate() {
                for (c2, a2) in &table[i + 1..] {
                    assert!(
                        c1 != c2,
                        "{label}: chord {c1:?} bound to both {a1:?} and {a2:?}"
                    );
                }
            }
        }
        check("global", GlobalAction::bindings());
        check("chat", ChatTabAction::bindings());
        check("logs", LogsTabAction::bindings());
        check("dashboard", DashboardTabAction::bindings());
        check("config", ConfigTabAction::bindings());
        check("quickstart", QuickstartTabAction::bindings());
        check("input_bar", InputBarAction::bindings());
        check("modal", ModalAction::bindings());
        check("file_explorer", FileExplorerAction::bindings());
        check("file_explorer_search", FileExplorerSearchAction::bindings());
        check("search_box", SearchBoxAction::bindings());
        check("config_editor", ConfigEditorAction::bindings());
        check("quickstart_modal", QuickstartModalAction::bindings());
    }

    #[test]
    fn no_cross_enum_global_shadow() {
        let global = GlobalAction::bindings();
        let panes: &[(&str, Vec<Chord>)] = &[
            (
                "chat",
                ChatTabAction::bindings()
                    .into_iter()
                    .map(|(c, _)| c)
                    .collect(),
            ),
            (
                "logs",
                LogsTabAction::bindings()
                    .into_iter()
                    .map(|(c, _)| c)
                    .collect(),
            ),
            (
                "dashboard",
                DashboardTabAction::bindings()
                    .into_iter()
                    .map(|(c, _)| c)
                    .collect(),
            ),
            (
                "config",
                ConfigTabAction::bindings()
                    .into_iter()
                    .map(|(c, _)| c)
                    .collect(),
            ),
            (
                "quickstart",
                QuickstartTabAction::bindings()
                    .into_iter()
                    .map(|(c, _)| c)
                    .collect(),
            ),
        ];
        for (gc, ga) in &global {
            for (label, chords) in panes {
                for pc in chords {
                    assert!(
                        gc != pc,
                        "global {ga:?} chord {gc:?} shadows a {label} action sharing it"
                    );
                }
            }
        }
    }

    /// Every rebindable enum's TAG and serialized variant names must be
    /// snake_case — the action-key wire form (`"<tag>.<variant>"`) is
    /// only valid snake_case, and kebab-case is banned project-wide.
    #[test]
    fn tags_and_variant_names_are_snake_case() {
        fn ok(s: &str) -> bool {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                && !s.starts_with('_')
                && !s.ends_with('_')
        }
        fn check<A: RebindableActions>() {
            assert!(ok(A::tag()), "tag '{}' is not snake_case", A::tag());
            for v in A::all() {
                let key = v.key();
                let variant = key.split_once('.').map(|(_, v)| v).unwrap_or(&key);
                assert!(ok(variant), "variant '{variant}' is not snake_case");
            }
        }
        check::<GlobalAction>();
        check::<ChatTabAction>();
        check::<LogsTabAction>();
        check::<DashboardTabAction>();
        check::<ConfigTabAction>();
        check::<QuickstartTabAction>();
        check::<InputBarAction>();
        check::<FileExplorerAction>();
    }
}
