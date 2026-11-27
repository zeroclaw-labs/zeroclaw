//! Parser for the `key` parameter's chord syntax.
//!
//! A model writes hotkeys as one string, for example `cmd+shift+s` or
//! `ctrl+alt+t`. This module turns that string into a [`KeyChord`] the backend
//! can post, and refuses anything ambiguous instead of guessing: a chord must
//! name exactly one non-modifier key, and every token must be one the mapping
//! recognizes. Refusing is safer than silently pressing a different key than
//! the model asked for.

use super::backend::{KeyChord, KeyCode, Modifiers};

/// Why a chord string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyParseError {
    /// The input was empty or only whitespace.
    #[error("key specification is empty")]
    Empty,
    /// Only modifiers were given; a chord needs one key to press.
    #[error("key specification names only modifiers; add the key to press")]
    MissingKey,
    /// A token is neither a known modifier nor a known key.
    #[error("unknown key `{0}`")]
    UnknownKey(String),
    /// More than one non-modifier key was given.
    #[error("key specification names more than one key; a chord presses exactly one")]
    MultipleKeys,
}

/// Parse a chord string into a [`KeyChord`].
///
/// Modifiers are matched case-insensitively and may appear in any order and any
/// number of times. Exactly one non-modifier key must be present. A literal `+`
/// key is written as its own token: `"+"` presses plus, and `"cmd++"` presses
/// plus with Command held.
pub fn parse_key_chord(input: &str) -> Result<KeyChord, KeyParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(KeyParseError::Empty);
    }

    let mut modifiers = Modifiers::default();
    let mut key: Option<KeyCode> = None;
    for token in tokenize(trimmed) {
        let lowered = token.to_ascii_lowercase();
        if apply_modifier(&lowered, &mut modifiers) {
            continue;
        }
        let Some((parsed, shift)) = parse_key_literal(&lowered, &token) else {
            return Err(KeyParseError::UnknownKey(token));
        };
        if key.is_some() {
            return Err(KeyParseError::MultipleKeys);
        }
        if shift {
            modifiers.shift = true;
        }
        key = Some(parsed);
    }

    match key {
        Some(key) => Ok(KeyChord { key, modifiers }),
        None => Err(KeyParseError::MissingKey),
    }
}

/// Split a chord into tokens, treating a bare `+` (or the empty slot between two
/// separators, as in `cmd++`) as the literal plus key.
fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for c in input.chars() {
        if c == '+' {
            let token = current.trim();
            if token.is_empty() {
                // Nothing buffered, so this separator stands for the key itself.
                tokens.push("+".to_string());
            } else {
                tokens.push(token.to_string());
            }
            current.clear();
        } else {
            current.push(c);
        }
    }
    let tail = current.trim();
    if !tail.is_empty() {
        tokens.push(tail.to_string());
    }
    tokens
}

/// Set the modifier flag `token` names, if it names one.
fn apply_modifier(token: &str, modifiers: &mut Modifiers) -> bool {
    match token {
        "cmd" | "command" | "super" | "meta" => modifiers.command = true,
        "ctrl" | "control" => modifiers.control = true,
        "alt" | "option" | "opt" => modifiers.alt = true,
        "shift" => modifiers.shift = true,
        _ => return false,
    }
    true
}

/// The key `token` names, plus whether pressing it implies Shift.
///
/// `token` is the lowercased form used for named-key lookup; `original` is the
/// token as written, which decides the case of a single-character key.
fn parse_key_literal(token: &str, original: &str) -> Option<(KeyCode, bool)> {
    if let Some(named) = named_key(token) {
        return Some((named, false));
    }
    let mut chars = original.chars();
    let single = chars.next()?;
    if chars.next().is_some() || !single.is_ascii() {
        return None;
    }
    if single.is_ascii_alphabetic() {
        // An uppercase letter means the same key with Shift implied.
        let uppercase = single.is_ascii_uppercase();
        return Some((KeyCode::Char(single.to_ascii_lowercase()), uppercase));
    }
    if single.is_ascii_digit() || is_ascii_punctuation(single) {
        return Some((KeyCode::Char(single), false));
    }
    None
}

/// The named key `token` (already lowercased) identifies, if any.
fn named_key(token: &str) -> Option<KeyCode> {
    let key = match token {
        "return" | "enter" => KeyCode::Return,
        "escape" | "esc" => KeyCode::Escape,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Space,
        "backspace" | "bs" => KeyCode::BackSpace,
        "delete" | "del" | "forwarddelete" => KeyCode::Delete,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "page_up" | "prior" => KeyCode::PageUp,
        "pagedown" | "page_down" | "next" => KeyCode::PageDown,
        "f1" => KeyCode::F1,
        "f2" => KeyCode::F2,
        "f3" => KeyCode::F3,
        "f4" => KeyCode::F4,
        "f5" => KeyCode::F5,
        "f6" => KeyCode::F6,
        "f7" => KeyCode::F7,
        "f8" => KeyCode::F8,
        "f9" => KeyCode::F9,
        "f10" => KeyCode::F10,
        "f11" => KeyCode::F11,
        "f12" => KeyCode::F12,
        _ => return None,
    };
    Some(key)
}

/// Whether `c` is an unshifted or shifted character on a US ANSI keyboard.
fn is_ascii_punctuation(c: char) -> bool {
    matches!(
        c,
        '`' | '-'
            | '='
            | '['
            | ']'
            | '\\'
            | ';'
            | '\''
            | ','
            | '.'
            | '/'
            | '~'
            | '!'
            | '@'
            | '#'
            | '$'
            | '%'
            | '^'
            | '&'
            | '*'
            | '('
            | ')'
            | '_'
            | '+'
            | '{'
            | '}'
            | '|'
            | ':'
            | '"'
            | '<'
            | '>'
            | '?'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chord of one character key with no modifiers.
    fn plain(key: KeyCode) -> KeyChord {
        KeyChord {
            key,
            modifiers: Modifiers::default(),
        }
    }

    #[test]
    fn parses_modifier_aliases() {
        for alias in ["cmd", "command", "super", "meta", "CMD", "Command"] {
            let chord = parse_key_chord(&format!("{alias}+s")).expect("modifier alias parses");
            assert_eq!(chord.key, KeyCode::Char('s'));
            assert_eq!(
                chord.modifiers,
                Modifiers {
                    command: true,
                    ..Modifiers::default()
                },
                "`{alias}` must mean Command"
            );
        }
        for alias in ["ctrl", "control", "CTRL"] {
            assert_eq!(
                parse_key_chord(&format!("{alias}+c"))
                    .expect("modifier alias parses")
                    .modifiers,
                Modifiers {
                    control: true,
                    ..Modifiers::default()
                },
                "`{alias}` must mean Control"
            );
        }
        for alias in ["alt", "option", "opt", "ALT"] {
            assert_eq!(
                parse_key_chord(&format!("{alias}+f4"))
                    .expect("modifier alias parses")
                    .modifiers,
                Modifiers {
                    alt: true,
                    ..Modifiers::default()
                },
                "`{alias}` must mean Alt"
            );
        }
        assert_eq!(
            parse_key_chord("shift+tab")
                .expect("shift parses")
                .modifiers,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            }
        );
    }

    #[test]
    fn parses_multi_modifier_chords() {
        let chord = parse_key_chord("cmd+shift+s").expect("chord parses");
        assert_eq!(
            chord,
            KeyChord {
                key: KeyCode::Char('s'),
                modifiers: Modifiers {
                    command: true,
                    shift: true,
                    ..Modifiers::default()
                },
            }
        );

        let chord = parse_key_chord("ctrl+alt+t").expect("chord parses");
        assert_eq!(
            chord,
            KeyChord {
                key: KeyCode::Char('t'),
                modifiers: Modifiers {
                    control: true,
                    alt: true,
                    ..Modifiers::default()
                },
            }
        );

        // Order and repetition do not matter.
        assert_eq!(
            parse_key_chord("shift+command+alt+control+cmd+a").expect("chord parses"),
            KeyChord {
                key: KeyCode::Char('a'),
                modifiers: Modifiers {
                    command: true,
                    control: true,
                    alt: true,
                    shift: true,
                },
            }
        );

        // Whitespace around tokens is ignored.
        assert_eq!(
            parse_key_chord("  cmd + shift + s  ").expect("chord parses"),
            parse_key_chord("cmd+shift+s").expect("chord parses")
        );
    }

    #[test]
    fn parses_literal_plus_key() {
        assert_eq!(
            parse_key_chord("+").expect("literal plus parses"),
            plain(KeyCode::Char('+'))
        );
        assert_eq!(
            parse_key_chord("cmd++").expect("command plus parses"),
            KeyChord {
                key: KeyCode::Char('+'),
                modifiers: Modifiers {
                    command: true,
                    ..Modifiers::default()
                },
            }
        );
        assert_eq!(
            parse_key_chord("shift++").expect("shift plus parses"),
            KeyChord {
                key: KeyCode::Char('+'),
                modifiers: Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
            }
        );
        // `+` also parses as ordinary punctuation when it is the only token.
        assert_eq!(
            parse_key_chord(" + ").expect("padded plus parses"),
            plain(KeyCode::Char('+'))
        );
    }

    #[test]
    fn uppercasing_a_letter_implies_shift() {
        assert_eq!(
            parse_key_chord("A").expect("uppercase letter parses"),
            KeyChord {
                key: KeyCode::Char('a'),
                modifiers: Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
            }
        );
        assert_eq!(
            parse_key_chord("cmd+S").expect("uppercase letter parses"),
            KeyChord {
                key: KeyCode::Char('s'),
                modifiers: Modifiers {
                    command: true,
                    shift: true,
                    ..Modifiers::default()
                },
            }
        );
        // An already explicit shift stays set exactly once.
        assert_eq!(
            parse_key_chord("shift+A").expect("uppercase letter parses"),
            KeyChord {
                key: KeyCode::Char('a'),
                modifiers: Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
            }
        );
        // Lowercase needs no shift.
        assert_eq!(
            parse_key_chord("a").expect("lowercase letter parses"),
            plain(KeyCode::Char('a'))
        );
    }

    #[test]
    fn parses_named_key_aliases() {
        for (input, expected) in [
            ("return", KeyCode::Return),
            ("enter", KeyCode::Return),
            ("escape", KeyCode::Escape),
            ("esc", KeyCode::Escape),
            ("tab", KeyCode::Tab),
            ("space", KeyCode::Space),
            ("backspace", KeyCode::BackSpace),
            ("bs", KeyCode::BackSpace),
            ("delete", KeyCode::Delete),
            ("del", KeyCode::Delete),
            ("forwarddelete", KeyCode::Delete),
            ("up", KeyCode::Up),
            ("down", KeyCode::Down),
            ("left", KeyCode::Left),
            ("right", KeyCode::Right),
            ("home", KeyCode::Home),
            ("end", KeyCode::End),
            ("pageup", KeyCode::PageUp),
            ("page_up", KeyCode::PageUp),
            ("prior", KeyCode::PageUp),
            ("pagedown", KeyCode::PageDown),
            ("page_down", KeyCode::PageDown),
            ("next", KeyCode::PageDown),
        ] {
            assert_eq!(
                parse_key_chord(input).expect("named key parses"),
                plain(expected),
                "`{input}` must parse"
            );
            assert_eq!(
                parse_key_chord(&input.to_uppercase())
                    .expect("named key parses case-insensitively"),
                plain(expected),
                "`{input}` must parse regardless of case"
            );
            assert_eq!(
                parse_key_chord(&format!("cmd+{input}")).expect("named key chord parses"),
                KeyChord {
                    key: expected,
                    modifiers: Modifiers {
                        command: true,
                        ..Modifiers::default()
                    },
                },
                "`cmd+{input}` must parse"
            );
        }

        for (name, expected) in [
            ("f1", KeyCode::F1),
            ("f2", KeyCode::F2),
            ("f3", KeyCode::F3),
            ("f4", KeyCode::F4),
            ("f5", KeyCode::F5),
            ("f6", KeyCode::F6),
            ("f7", KeyCode::F7),
            ("f8", KeyCode::F8),
            ("f9", KeyCode::F9),
            ("f10", KeyCode::F10),
            ("f11", KeyCode::F11),
            ("f12", KeyCode::F12),
        ] {
            assert_eq!(
                parse_key_chord(name).expect("function key parses"),
                plain(expected),
                "`{name}` must parse"
            );
            assert_eq!(
                parse_key_chord(&format!("cmd+{name}")).expect("function key chord parses"),
                KeyChord {
                    key: expected,
                    modifiers: Modifiers {
                        command: true,
                        ..Modifiers::default()
                    },
                },
                "`cmd+{name}` must parse"
            );
        }
    }

    #[test]
    fn parses_digits_and_ansi_punctuation() {
        for digit in '0'..='9' {
            assert_eq!(
                parse_key_chord(&digit.to_string()).expect("digit parses"),
                plain(KeyCode::Char(digit))
            );
        }
        for punctuation in [
            '`', '-', '=', '[', ']', '\\', ';', '\'', ',', '.', '/', '~', '!', '@', '#', '$', '%',
            '^', '&', '*', '(', ')', '_', '{', '}', '|', ':', '"', '<', '>', '?',
        ] {
            assert_eq!(
                parse_key_chord(&punctuation.to_string()).expect("punctuation parses"),
                plain(KeyCode::Char(punctuation)),
                "`{punctuation}` must parse"
            );
        }
        // Punctuation carries no implied Shift; the backend owns the shifted
        // virtual keycode for characters like `!` and `_`.
        assert_eq!(
            parse_key_chord("cmd+!").expect("shifted punctuation parses"),
            KeyChord {
                key: KeyCode::Char('!'),
                modifiers: Modifiers {
                    command: true,
                    ..Modifiers::default()
                },
            }
        );
    }

    #[test]
    fn rejects_empty_and_whitespace_only_input() {
        assert_eq!(parse_key_chord(""), Err(KeyParseError::Empty));
        assert_eq!(parse_key_chord("   "), Err(KeyParseError::Empty));
        assert_eq!(parse_key_chord("\t\n"), Err(KeyParseError::Empty));
    }

    #[test]
    fn rejects_modifier_only_chords() {
        for input in ["cmd", "shift", "cmd+shift", "ctrl+alt+shift+cmd", " cmd + "] {
            assert_eq!(
                parse_key_chord(input),
                Err(KeyParseError::MissingKey),
                "`{input}` must be rejected as missing its key"
            );
        }
    }

    #[test]
    fn rejects_multiple_non_modifier_keys() {
        for input in ["a+b", "cmd+a+b", "cmd+a+shift+b", "a+f1", "enter+escape"] {
            assert_eq!(
                parse_key_chord(input),
                Err(KeyParseError::MultipleKeys),
                "`{input}` must be rejected as naming two keys"
            );
        }
    }

    #[test]
    fn rejects_unknown_and_non_ascii_tokens() {
        assert_eq!(
            parse_key_chord("hyperkey"),
            Err(KeyParseError::UnknownKey("hyperkey".to_string()))
        );
        assert_eq!(
            parse_key_chord("cmd+hyperkey"),
            Err(KeyParseError::UnknownKey("hyperkey".to_string()))
        );
        assert_eq!(
            parse_key_chord("f13"),
            Err(KeyParseError::UnknownKey("f13".to_string()))
        );
        assert_eq!(
            parse_key_chord("cmd+ab"),
            Err(KeyParseError::UnknownKey("ab".to_string()))
        );
        assert_eq!(
            parse_key_chord("é"),
            Err(KeyParseError::UnknownKey("é".to_string()))
        );
        assert_eq!(
            parse_key_chord("cmd+→"),
            Err(KeyParseError::UnknownKey("→".to_string()))
        );
        // The original spelling is preserved in the error, so the model can see
        // exactly which token it wrote.
        assert_eq!(
            parse_key_chord("cmd+HyperKey"),
            Err(KeyParseError::UnknownKey("HyperKey".to_string()))
        );
    }
}
