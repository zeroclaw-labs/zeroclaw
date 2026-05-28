//! Key chord type — a `KeyCode` + modifier mask that knows how to
//! match incoming events and render itself per-OS.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A single keystroke pattern.
///
/// On darwin, `CONTROL` in a chord is silently translated to `SUPER`
/// at match time so Linux's `Ctrl+K` and macOS's `⌘K` resolve to the
/// same chord.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Chord {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl Chord {
    pub const fn key(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::NONE,
        }
    }

    pub const fn char(c: char) -> Self {
        Self::key(KeyCode::Char(c))
    }

    pub const fn with(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    pub const fn ctrl(c: char) -> Self {
        Self::with(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    pub const fn shift(code: KeyCode) -> Self {
        Self::with(code, KeyModifiers::SHIFT)
    }

    pub fn matches(&self, event: &KeyEvent) -> bool {
        event.code == self.code && normalise_mods(self.modifiers) == normalise_mods(event.modifiers)
    }

    /// `Ctrl+K` on most platforms; `⌘K` on darwin.
    #[allow(dead_code)]
    pub fn display(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            parts.push(if cfg!(target_os = "macos") {
                "⌘"
            } else {
                "Ctrl"
            });
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            parts.push(if cfg!(target_os = "macos") {
                "⌥"
            } else {
                "Alt"
            });
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            parts.push("Shift");
        }
        let key = render_keycode(&self.code);
        if parts.is_empty() {
            key
        } else if cfg!(target_os = "macos") {
            format!("{}{}", parts.join(""), key)
        } else {
            format!("{}+{}", parts.join("+"), key)
        }
    }
}

#[cfg(target_os = "macos")]
fn normalise_mods(mut m: KeyModifiers) -> KeyModifiers {
    if m.contains(KeyModifiers::CONTROL) {
        m.remove(KeyModifiers::CONTROL);
        m.insert(KeyModifiers::SUPER);
    }
    m
}

#[cfg(not(target_os = "macos"))]
fn normalise_mods(m: KeyModifiers) -> KeyModifiers {
    m
}

#[allow(dead_code)]
fn render_keycode(code: &KeyCode) -> String {
    match code {
        KeyCode::Char(c) => c.to_uppercase().to_string(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::BackTab => "Shift+Tab".into(),
        KeyCode::Backspace => "Backspace".into(),
        KeyCode::Left => "←".into(),
        KeyCode::Right => "→".into(),
        KeyCode::Up => "↑".into(),
        KeyCode::Down => "↓".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::PageUp => "PgUp".into(),
        KeyCode::PageDown => "PgDn".into(),
        KeyCode::Delete => "Del".into(),
        KeyCode::Insert => "Ins".into(),
        KeyCode::F(n) => format!("F{n}"),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_letter_matches_no_modifier_event() {
        let chord = Chord::char('q');
        let event = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(chord.matches(&event));
    }

    #[test]
    fn ctrl_chord_rejects_unmodified_event() {
        let chord = Chord::ctrl('k');
        let event = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        assert!(!chord.matches(&event));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn ctrl_chord_matches_ctrl_event_on_non_darwin() {
        let chord = Chord::ctrl('k');
        let event = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert!(chord.matches(&event));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ctrl_chord_matches_super_event_on_darwin() {
        let chord = Chord::ctrl('k');
        let event = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::SUPER);
        assert!(chord.matches(&event));
    }

    #[test]
    fn display_bare_letter_uppercases() {
        assert_eq!(Chord::char('k').display(), "K");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn display_ctrl_on_non_darwin() {
        assert_eq!(Chord::ctrl('k').display(), "Ctrl+K");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn display_ctrl_on_darwin() {
        assert_eq!(Chord::ctrl('k').display(), "⌘K");
    }

    #[test]
    fn display_arrow_keys() {
        assert_eq!(Chord::key(KeyCode::Up).display(), "↑");
        assert_eq!(Chord::key(KeyCode::Left).display(), "←");
    }
}
