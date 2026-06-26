//! Registry-driven help entries. Keys come from the live keybinding
//! registry and text from i18n, so the help modal always reflects the
//! actual (possibly-overridden) bindings. Panes never hand-write key
//! literals; they hand this module a set of actions and render the result.

use crate::keymap::{RebindableActions, action_key_labels};
use crate::widgets::HelpEntry;

/// Localized help description for an action. Derives the i18n key from the
/// action's stable `<tag>.<variant>` identity (`zc-help-<tag>-<variant>`),
/// falling back to the action's built-in English label when no localized
/// string is registered.
pub fn action_help_text<A: RebindableActions>(action: A) -> String {
    let i18n_key = format!("zc-help-{}", action.key().replace('.', "-"));
    crate::i18n::try_t(&i18n_key).unwrap_or_else(|| action.human_label().to_string())
}

/// One help entry per *bound* variant of an action enum, in declaration
/// order. Keys from the live registry, text from [`action_help_text`].
/// Unbound variants are skipped so they never show a keyless row.
pub fn help_entries<A: RebindableActions>() -> Vec<HelpEntry> {
    entries_for(A::all().iter().copied())
}

/// Registry-driven help entries for an explicit, ordered set of actions —
/// used by panes that surface only a context-relevant subset (per-tab or
/// per-mode) while still sourcing keys and text from the live registry.
pub fn entries_for<A: RebindableActions>(actions: impl IntoIterator<Item = A>) -> Vec<HelpEntry> {
    actions
        .into_iter()
        .filter_map(|action| {
            let keys = action_key_labels(action);
            if keys.is_empty() {
                return None;
            }
            Some(HelpEntry::new(keys, action_help_text(action)))
        })
        .collect()
}

/// Source guard: help rows must source their keys from the keybinding
/// registry (`help_entries` / `entries_for` / `action_key_labels`), never
/// from a hand-written key literal. A literal key in a help row drifts from
/// user rebinds and silently omits new actions — exactly the class of bug
/// this module exists to kill.
#[cfg(test)]
mod source_guard {
    use std::path::Path;

    const SRC: &str = env!("CARGO_MANIFEST_DIR");

    /// Key-name tokens that mark a string literal as a hand-written
    /// keybinding rather than descriptive prose. Case-sensitive on purpose:
    /// `Enter`/`Ctrl`/`Esc` are chord glyphs, while words like "enter the"
    /// in prose are lowercase.
    const KEY_TOKENS: &[&str] = &[
        "Ctrl+",
        "Ctrl ",
        "Shift+",
        "Alt+",
        "Enter",
        "Esc",
        "Tab",
        "Backspace",
        "PgUp",
        "PgDn",
        "PageUp",
        "PageDown",
        "Home",
        "End",
        "⌘",
        "↑",
        "↓",
        "←",
        "→",
    ];

    /// A reviewed, non-key descriptor row may opt out with this marker.
    /// The reason text is unconstrained on purpose: each waiver is audited by a
    /// human at PR-review time, not validated at compile time, so the commit that
    /// adds one must justify it in review.
    const WAIVER: &str = "// helpguard: ";

    /// `E::key(` / `HelpEntry::key(` first-arg, or the `vec![...]` /
    /// `[...]` first-arg of `E::new(` — anything that becomes the key
    /// column of a help row.
    fn help_key_call_start(trimmed: &str) -> bool {
        trimmed.contains("E::key(")
            || trimmed.contains("HelpEntry::key(")
            || trimmed.contains("E::new(")
            || trimmed.contains("HelpEntry::new(")
    }

    fn literal_has_key_token(line: &str) -> bool {
        let Some(open) = line.find('"') else {
            return false;
        };
        let rest = &line[open + 1..];
        let Some(close) = rest.find('"') else {
            return false;
        };
        let literal = &rest[..close];
        KEY_TOKENS.iter().any(|t| literal.contains(t))
    }

    fn scan(dir: &Path, root: &Path, violations: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                scan(&path, root, violations);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let text = std::fs::read_to_string(&path).expect("read_to_string");
            let mut in_test = false;
            for (i, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("mod ") {
                    in_test = true;
                }
                if in_test || line.contains(WAIVER) {
                    continue;
                }
                if help_key_call_start(trimmed) && literal_has_key_token(line) {
                    violations.push(format!("{rel}:{}: {}", i + 1, line.trim()));
                }
            }
        }
    }

    #[test]
    fn no_hardcoded_keys_in_help_rows() {
        let root = Path::new(SRC);
        let src = root.join("src");
        let mut violations = Vec::new();
        scan(&src, root, &mut violations);
        assert!(
            violations.is_empty(),
            "hand-written key literal in a help row (source keys from the registry via \
             help_entries/entries_for/action_key_labels, or annotate a reviewed non-key \
             descriptor with `// helpguard: <reason>`):\n{}",
            violations.join("\n")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::InputBarAction;

    #[test]
    fn entries_skip_unbound_actions() {
        // SelectAll has no default chord; it must not produce a keyless row.
        let entries = help_entries::<InputBarAction>();
        assert!(
            entries.iter().all(|e| !e.keys.is_empty()),
            "no help entry may have empty keys"
        );
    }

    #[test]
    fn input_bar_help_surfaces_clear_input_from_registry() {
        let entries = help_entries::<InputBarAction>();
        let clear = entries
            .iter()
            .find(|e| e.action == action_help_text(InputBarAction::ClearInput))
            .expect("clear input must appear in input-bar help");
        assert_eq!(
            clear.keys,
            crate::keymap::action_key_labels(InputBarAction::ClearInput),
            "clear-input keys must match the live registry"
        );
        assert!(
            !clear.keys.is_empty(),
            "clear input is bound, so it must advertise its chord"
        );
    }

    #[test]
    fn entries_for_preserves_requested_order() {
        let entries = entries_for([InputBarAction::Paste, InputBarAction::Submit]);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].action, action_help_text(InputBarAction::Paste));
        assert_eq!(entries[1].action, action_help_text(InputBarAction::Submit));
    }
}
