//! Key chord type — a `KeyCode` + modifier mask that knows how to
//! match incoming events and render itself per-OS.

use std::fmt;
use std::str::FromStr;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

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
        event.code == self.code
            && normalise_mods(self.code, self.modifiers)
                == normalise_mods(event.code, event.modifiers)
    }

    /// `Ctrl+K` on most platforms; `⌘K` on darwin.
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

    /// OS-independent canonical wire form used for persistence:
    /// lowercase, `+`-joined modifiers then key, e.g. `ctrl+k`,
    /// `shift+up`, `ctrl+shift+down`, `f5`, `pageup`. Never uses the
    /// darwin glyphs — a config written on macOS loads identically on
    /// Linux. Round-trips with [`Chord::from_str`].
    pub fn wire(&self) -> String {
        let mut out = String::new();
        // Modifier tokens walk the canonical registry so render and
        // parse share one source of truth — no string-literal arms.
        for (token, flag) in MOD_TOKENS {
            if self.modifiers.contains(*flag) {
                out.push_str(token);
                out.push('+');
            }
        }
        out.push_str(&keycode_wire(&self.code));
        out
    }
}

/// Canonical modifier token registry. Walked by both `wire()` (render)
/// and `from_str` (parse), so the two directions can never drift.
/// `super` is accepted on parse but never emitted on non-darwin; the
/// Ctrl→Super normalisation stays purely at match time.
const MOD_TOKENS: &[(&str, KeyModifiers)] = &[
    ("ctrl", KeyModifiers::CONTROL),
    ("alt", KeyModifiers::ALT),
    ("shift", KeyModifiers::SHIFT),
    ("super", KeyModifiers::SUPER),
];

/// Named (non-char, non-`F<n>`) key token registry. Walked by both
/// `keycode_wire` and the key-token parse so they stay reversible.
const KEY_TOKENS: &[(&str, KeyCode)] = &[
    ("enter", KeyCode::Enter),
    ("esc", KeyCode::Esc),
    ("tab", KeyCode::Tab),
    ("backtab", KeyCode::BackTab),
    ("backspace", KeyCode::Backspace),
    ("space", KeyCode::Char(' ')),
    ("left", KeyCode::Left),
    ("right", KeyCode::Right),
    ("up", KeyCode::Up),
    ("down", KeyCode::Down),
    ("home", KeyCode::Home),
    ("end", KeyCode::End),
    ("pageup", KeyCode::PageUp),
    ("pagedown", KeyCode::PageDown),
    ("delete", KeyCode::Delete),
    ("insert", KeyCode::Insert),
];

/// Render a `KeyCode` to its canonical wire token. Matching on the
/// `KeyCode` enum (not on strings) is the legitimate direction; the
/// reverse (`&str` -> `KeyCode`) walks `KEY_TOKENS`.
fn keycode_wire(code: &KeyCode) -> String {
    if let KeyCode::Char(c) = code {
        if *c == ' ' {
            return "space".to_string();
        }
        return c.to_string();
    }
    if let KeyCode::F(n) = code {
        return format!("f{n}");
    }
    KEY_TOKENS
        .iter()
        .find_map(|(tok, kc)| (kc == code).then(|| (*tok).to_string()))
        .unwrap_or_else(|| format!("{code:?}").to_lowercase())
}

/// Parse a single key token (no modifiers) into a `KeyCode`. Resolves
/// named keys through `KEY_TOKENS`, single chars to `Char`, and `f<N>`
/// to a function key — structurally, never via string-literal arms.
fn parse_keycode(token: &str) -> Result<KeyCode, ChordParseError> {
    let lower = token.to_lowercase();
    if let Some((_, kc)) = KEY_TOKENS.iter().find(|(t, _)| *t == lower) {
        return Ok(*kc);
    }
    if let Some(rest) = lower.strip_prefix('f')
        && let Ok(n) = rest.parse::<u8>()
    {
        return Ok(KeyCode::F(n));
    }
    let mut chars = token.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(KeyCode::Char(c)),
        _ => Err(ChordParseError(token.to_string())),
    }
}

/// Error for an unparseable chord wire-string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChordParseError(String);

impl fmt::Display for ChordParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid chord '{}'", self.0)
    }
}

impl std::error::Error for ChordParseError {}

impl FromStr for Chord {
    type Err = ChordParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(ChordParseError(s.to_string()));
        }
        // Single-char tokens bypass modifier splitting so `+` and `=`
        // round-trip cleanly. Without this, `trimmed.split('+')` on
        // `"+"` yields two empty segments and the parse fails.
        if trimmed.chars().count() == 1 {
            let code = parse_keycode(trimmed)?;
            return Ok(Chord {
                code,
                modifiers: KeyModifiers::NONE,
            });
        }
        let mut segments: Vec<&str> = trimmed.split('+').collect();
        // Last segment is the key (case preserved so 'G' stays distinct
        // from 'g'); everything before is a modifier (case-insensitive).
        let key_token = segments
            .pop()
            .ok_or_else(|| ChordParseError(s.to_string()))?;
        let mut modifiers = KeyModifiers::NONE;
        for seg in segments {
            let lower = seg.to_lowercase();
            let flag = MOD_TOKENS
                .iter()
                .find_map(|(t, f)| (*t == lower).then_some(*f))
                .ok_or_else(|| ChordParseError(s.to_string()))?;
            modifiers.insert(flag);
        }
        let code = parse_keycode(key_token)?;
        Ok(Chord { code, modifiers })
    }
}

impl Serialize for Chord {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.wire())
    }
}

impl<'de> Deserialize<'de> for Chord {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        Chord::from_str(&s).map_err(de::Error::custom)
    }
}

#[cfg(target_os = "macos")]
fn normalise_mods(code: KeyCode, mut m: KeyModifiers) -> KeyModifiers {
    if m.contains(KeyModifiers::CONTROL) {
        m.remove(KeyModifiers::CONTROL);
        m.insert(KeyModifiers::SUPER);
    }
    strip_redundant_shift(code, m)
}

#[cfg(not(target_os = "macos"))]
fn normalise_mods(code: KeyCode, m: KeyModifiers) -> KeyModifiers {
    strip_redundant_shift(code, m)
}

/// Drop the SHIFT bit for character keys. A shifted character (`?`,
/// `G`, `:`) already encodes its shift in the glyph itself, but
/// platforms disagree on whether SHIFT is *also* reported alongside
/// it: Unix terminals strip it, the Windows console keeps it. Comparing
/// it would make `?` (the default Help chord) only match on platforms
/// that strip SHIFT, forcing Windows users to hand-bind `shift+?`.
/// Modifier keys that genuinely change the keystroke (Ctrl/Alt/Super)
/// are left untouched.
fn strip_redundant_shift(code: KeyCode, mut m: KeyModifiers) -> KeyModifiers {
    if matches!(code, KeyCode::Char(_)) {
        m.remove(KeyModifiers::SHIFT);
    }
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

    #[test]
    fn question_mark_matches_with_or_without_shift() {
        // `?` is Shift+/ physically. Unix terminals report Char('?')
        // with no modifier; the Windows console reports Char('?') with
        // SHIFT still set. The default Help chord (bare '?') must match
        // both, or Windows users have to hand-bind shift+?.
        let chord = Chord::char('?');
        let unix = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE);
        let windows = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert!(chord.matches(&unix));
        assert!(chord.matches(&windows));
    }

    #[test]
    fn explicit_shift_char_chord_still_matches_bare_event() {
        // A user who hand-bound `shift+?` as a workaround keeps working:
        // SHIFT is redundant on a char key, so it's stripped from both
        // sides of the comparison.
        let chord = Chord::with(KeyCode::Char('?'), KeyModifiers::SHIFT);
        let event = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(chord.matches(&event));
    }

    #[test]
    fn shift_still_discriminates_non_char_keys() {
        // Shift is only redundant on character glyphs. On named keys it
        // genuinely changes the chord, so Shift+Up must not match Up.
        let chord = Chord::shift(KeyCode::Up);
        let bare = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        let shifted = KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT);
        assert!(!chord.matches(&bare));
        assert!(chord.matches(&shifted));
    }

    #[test]
    fn ctrl_on_char_key_still_required_despite_shift_stripping() {
        // Stripping SHIFT must not weaken Ctrl/Alt enforcement.
        let chord = Chord::ctrl('k');
        let no_ctrl = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::SHIFT);
        assert!(!chord.matches(&no_ctrl));
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

    #[test]
    fn wire_round_trips_bare_letter() {
        let c = Chord::char('k');
        assert_eq!(c.wire(), "k");
        assert_eq!(Chord::from_str("k").unwrap(), c);
    }

    #[test]
    fn wire_round_trips_modifiers() {
        for c in [
            Chord::ctrl('k'),
            Chord::shift(KeyCode::Up),
            Chord::with(
                KeyCode::Down,
                KeyModifiers::CONTROL.union(KeyModifiers::SHIFT),
            ),
            Chord::with(KeyCode::Enter, KeyModifiers::ALT),
        ] {
            let wire = c.wire();
            assert_eq!(Chord::from_str(&wire).unwrap(), c, "round-trip {wire}");
        }
    }

    #[test]
    fn wire_round_trips_named_and_function_keys() {
        for c in [
            Chord::key(KeyCode::PageUp),
            Chord::key(KeyCode::Home),
            Chord::key(KeyCode::F(5)),
            Chord::key(KeyCode::Esc),
            Chord::key(KeyCode::Enter),
            Chord::char(' '),
        ] {
            let wire = c.wire();
            assert_eq!(Chord::from_str(&wire).unwrap(), c, "round-trip {wire}");
        }
    }

    #[test]
    fn wire_is_os_independent_lowercase() {
        // Never emits the darwin glyphs — same on every platform.
        assert_eq!(Chord::ctrl('k').wire(), "ctrl+k");
        assert_eq!(Chord::key(KeyCode::PageUp).wire(), "pageup");
        assert_eq!(Chord::char(' ').wire(), "space");
    }

    #[test]
    fn wire_preserves_letter_case() {
        // Regression: 'G' (vim jump-to-end) must not collapse to 'g' on
        // the wire, or jump_start and jump_end collide on reload.
        let upper = Chord::char('G');
        let lower = Chord::char('g');
        assert_eq!(upper.wire(), "G");
        assert_eq!(lower.wire(), "g");
        assert_ne!(upper.wire(), lower.wire());
        assert_eq!(Chord::from_str("G").unwrap(), upper);
        assert_eq!(Chord::from_str("g").unwrap(), lower);
        assert_ne!(Chord::from_str("G").unwrap(), Chord::from_str("g").unwrap());
    }

    #[test]
    fn parse_modifier_and_named_keys_are_case_insensitive() {
        assert_eq!(Chord::from_str("UP").unwrap(), Chord::key(KeyCode::Up));
        assert_eq!(
            Chord::from_str("Enter").unwrap(),
            Chord::key(KeyCode::Enter)
        );
        assert_eq!(Chord::from_str("CTRL+k").unwrap(), Chord::ctrl('k'));
        assert_eq!(Chord::from_str("ctrl+k").unwrap(), Chord::ctrl('k'));
        assert_eq!(Chord::from_str("Ctrl+K").unwrap(), Chord::ctrl('K'));
        assert_eq!(
            Chord::from_str("Shift+Up").unwrap(),
            Chord::shift(KeyCode::Up)
        );
    }

    #[test]
    fn parse_rejects_unknown_modifier_and_key() {
        assert!(Chord::from_str("hyper+k").is_err());
        assert!(Chord::from_str("ctrl+nope").is_err());
        assert!(Chord::from_str("").is_err());
    }

    #[test]
    fn serde_round_trips_through_json() {
        let c = Chord::ctrl('s');
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "\"ctrl+s\"");
        let back: Chord = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }
}
