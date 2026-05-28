//! Keymap abstraction for zerocode.
//!
//! Each leaf action enum carries its own default bindings inline.
//! Consumers call `ChatTabAction::from_chord(&key)` directly — no
//! `Keymap` struct, no plumbed argument.
//!
//! On darwin, `Chord::matches` translates the `CTRL` modifier to
//! `SUPER` so Linux's `Ctrl+K` and macOS's `⌘K` resolve identically.

mod actions;
mod chord;

pub use actions::*;
pub use chord::Chord;

use crossterm::event::KeyEvent;

pub fn match_chord<A: Copy>(table: &[(Chord, A)], event: &KeyEvent) -> Option<A> {
    table
        .iter()
        .find_map(|(c, a)| c.matches(event).then_some(*a))
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
}
