//! Keymap abstraction for zerocode.
//!
//! Each leaf action enum carries its own default bindings inline.
//! Consumers call `ChatTabAction::from_chord(&key)` directly — no
//! `Keymap` struct, no plumbed argument.
//!
//! Chords carry literal Control, platform-primary, and literal Super intent
//! explicitly; dispatch compares their effective event modifiers.

pub mod actions;
mod chord;
mod guard;
pub mod overrides;

pub use actions::*;
pub use chord::Chord;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn chord_bypasses_text_input(chord: &Chord) -> bool {
    if !matches!(chord.code, KeyCode::Char(_)) {
        return true;
    }

    let mut modifiers = chord.effective_modifiers();
    modifiers.remove(KeyModifiers::SHIFT);
    !modifiers.is_empty()
}

pub(crate) fn action_bypasses_text_input<A: RebindableActions>(
    action: A,
    event: &KeyEvent,
) -> bool {
    action
        .resolved()
        .iter()
        .any(|chord| chord_bypasses_text_input(chord) && chord.matches(event))
}

pub fn help_bypasses_text_input(event: &KeyEvent) -> bool {
    action_bypasses_text_input(GlobalAction::Help, event)
}

pub fn input_bar_claims_pane_navigation(event: &KeyEvent) -> bool {
    matches!(
        InputBarAction::from_chord(event),
        Some(InputBarAction::CursorWordLeft | InputBarAction::CursorWordRight)
    )
}

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
///
/// Compared with `same_key`: `strip_redundant_shift` drops `SHIFT` from every
/// character chord on every platform, so `shift+space` is the reserved
/// selection-toggle key at dispatch and an `Eq` test would have let the capture
/// widget bind it.
pub fn reserved_reason(chord: &Chord) -> Option<&'static str> {
    reserved_chords()
        .iter()
        .find_map(|(c, reason)| c.same_key(chord).then_some(*reason))
}

pub fn match_chord<A: Copy>(table: &[(Chord, A)], event: &KeyEvent) -> Option<A> {
    table
        .iter()
        .find_map(|(c, a)| c.matches(event).then_some(*a))
}

/// Rendered, OS-aware key labels for an action's currently-resolved
/// chords (e.g. `["Tab"]`, `["⌘x"]`). Help surfaces use this so the keys
/// they advertise track the live keybinding registry instead of literals.
pub fn action_key_labels<A: RebindableActions>(action: A) -> Vec<String> {
    let action_key = action.key();
    action
        .resolved()
        .iter()
        .filter(|chord| show_chord_in_help(&action_key, chord))
        .map(Chord::display)
        .collect()
}

fn show_chord_in_help(_action_key: &str, _chord: &Chord) -> bool {
    #[cfg(target_os = "macos")]
    if _action_key == "logs.copy_selection"
        && _chord.code == KeyCode::Char('C')
        && _chord.modifiers == KeyModifiers::CONTROL.union(KeyModifiers::SHIFT)
    {
        // Some macOS terminals collapse Ctrl+Shift+C to Ctrl+C, which
        // reaches ZeroCode as Quit instead of the advertised Logs action.
        return false;
    }
    true
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
    fn global_help_resolves_from_question_mark_and_control_g() {
        let q = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(GlobalAction::from_chord(&q), Some(GlobalAction::Help));
        let ctrl_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
        assert_eq!(GlobalAction::from_chord(&ctrl_g), Some(GlobalAction::Help));
    }

    #[test]
    fn help_bypass_distinguishes_text_from_command_chords() {
        let cases = [
            (
                Chord::char('?'),
                KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
                false,
            ),
            (
                Chord::with(KeyCode::Char('?'), KeyModifiers::SHIFT),
                KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
                false,
            ),
            (
                Chord::key(KeyCode::F(1)),
                KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
                true,
            ),
            (
                Chord::ctrl('g'),
                KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
                true,
            ),
        ];

        for (chord, event, expected) in cases {
            assert!(chord.matches(&event));
            assert_eq!(chord_bypasses_text_input(&chord), expected, "{chord:?}");
        }
    }

    #[test]
    fn action_bypass_distinguishes_text_from_command_chords() {
        assert!(!action_bypasses_text_input(
            ChatTabAction::CopySelection,
            &KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        ));
        assert!(action_bypasses_text_input(
            ChatTabAction::CopySelection,
            &KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER),
        ));
        assert!(action_bypasses_text_input(
            ChatTabAction::PageUp,
            &KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
        ));
        assert!(!action_bypasses_text_input(
            ChatTabAction::CopySelection,
            &KeyEvent::new(KeyCode::Char('y'), KeyModifiers::SHIFT),
        ));
    }

    #[test]
    fn browse_enter_resolves_from_control_k() {
        let ev = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(
            ChatTabAction::from_chord(&ev),
            Some(ChatTabAction::BrowseEnter)
        );
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
    fn input_bar_word_navigation_claims_global_pane_chords() {
        for event in [
            KeyEvent::new(KeyCode::Left, KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Right, KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT),
        ] {
            assert!(matches!(
                GlobalAction::from_chord(&event),
                Some(GlobalAction::PaneNavLeft | GlobalAction::PaneNavRight)
            ));
            assert!(input_bar_claims_pane_navigation(&event));
        }
    }

    #[test]
    fn config_cursor_actions_use_config_editor_registry_keys() {
        assert_eq!(
            ConfigEditorAction::CursorWordLeft.action_key(),
            "config_editor.cursor_word_left"
        );
        assert_eq!(
            ConfigEditorAction::CursorWordRight.action_key(),
            "config_editor.cursor_word_right"
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

    #[cfg(target_os = "macos")]
    #[test]
    fn help_hides_unreliable_fallback_only_for_logs_copy() {
        assert_eq!(action_key_labels(LogsTabAction::CopySelection), vec!["⌘c"]);
        assert_eq!(
            action_key_labels(ChatTabAction::CopyAllVisible),
            vec!["Ctrl+Shift+C"]
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn logs_copy_help_keeps_both_resolved_bindings() {
        assert_eq!(
            action_key_labels(LogsTabAction::CopySelection),
            vec!["Super+c", "Ctrl+Shift+C"]
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

    #[test]
    fn delete_previous_word_answers_to_both_primary_w_and_alt_backspace() {
        use crossterm::event::{KeyCode, KeyEvent};

        with_default_bindings(|| {
            // Both chords resolve to the one action, so the existing
            // Unicode-aware deletion stays the single behavior owner.
            let primary_w = Chord::primary('w');
            assert_eq!(
                InputBarAction::from_chord(&KeyEvent::new(
                    KeyCode::Char('w'),
                    primary_w.effective_modifiers(),
                )),
                Some(InputBarAction::DeletePreviousWord)
            );
            assert_eq!(
                InputBarAction::from_chord(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT)),
                Some(InputBarAction::DeletePreviousWord)
            );

            // Unmodified Backspace keeps its own action rather than falling
            // through to the word delete.
            assert_eq!(
                InputBarAction::from_chord(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
                Some(InputBarAction::Backspace)
            );

            // Help and the rebinding surface read the defaults, so both chords
            // have to be advertised there, not just accepted at match time.
            let defaults = InputBarAction::DeletePreviousWord.default_chords();
            assert!(defaults.contains(&primary_w));
            assert!(defaults.contains(&Chord::with(KeyCode::Backspace, KeyModifiers::ALT)));
            assert_eq!(
                action_key_labels(InputBarAction::DeletePreviousWord).len(),
                2
            );
        });
    }

    /// Run `body` with no override table installed, so the assertions see
    /// compile-time defaults. Needed by any test that reads
    /// `action_key_labels`, which resolves through the process-wide
    /// override state and would otherwise race a test that installs one.
    #[cfg(test)]
    fn with_default_bindings(body: impl FnOnce()) {
        let _g = overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        overrides::reset();
        body();
    }

    /// Install a sparse `input_bar` override for one variant, run `body`,
    /// then drop the table again. Serialized against every other test that
    /// touches the process-wide override state.
    #[cfg(test)]
    fn with_input_bar_override(variant: &str, chords: Vec<Chord>, body: impl FnOnce()) {
        let _g = overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        overrides::reset();
        let mut table = overrides::OverrideTable::new();
        let mut rows = std::collections::HashMap::new();
        rows.insert(variant.to_string(), chords);
        table.insert(InputBarAction::TAG.to_string(), rows);
        overrides::set_active(table);
        body();
        overrides::reset();
    }

    /// The capture widget refuses reserved chords, and that refusal has to use
    /// dispatch semantics too. `strip_redundant_shift` drops `SHIFT` from every
    /// character chord on every platform, so `shift+space` *is* the reserved
    /// selection-toggle key once it reaches `match_chord`. An `Eq` test let the
    /// widget bind it and the binding then did something else.
    #[test]
    fn reserved_reason_sees_a_normalized_spelling() {
        use crossterm::event::{KeyCode, KeyModifiers};
        assert!(reserved_reason(&Chord::char(' ')).is_some());
        assert!(
            reserved_reason(&Chord::with(KeyCode::Char(' '), KeyModifiers::SHIFT)).is_some(),
            "shift+space normalizes to the reserved space chord"
        );
        // Still narrow: an unreserved chord stays bindable.
        assert!(reserved_reason(&Chord::char('x')).is_none());
        assert!(reserved_reason(&Chord::ctrl('w')).is_none());
    }

    /// `same_key` has to answer exactly what dispatch would: two chords are one
    /// chord iff either matches the event the other describes. Asserted on both
    /// platforms, so the Linux run still pins the contract even though the
    /// platform-primary event differs by OS.
    #[test]
    fn same_key_agrees_with_dispatch() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let pairs = [
            (
                Chord::primary('a'),
                Chord::with(KeyCode::Char('a'), KeyModifiers::SUPER),
            ),
            (
                Chord::ctrl('a'),
                Chord::with(KeyCode::Char('a'), KeyModifiers::SUPER),
            ),
            // Literal Control and literal Super remain distinct on every OS.
            (
                Chord::ctrl('c'),
                Chord::with(KeyCode::Char('c'), KeyModifiers::SUPER),
            ),
            (Chord::ctrl('a'), Chord::ctrl('a')),
            (Chord::ctrl('a'), Chord::ctrl('b')),
            (
                Chord::with(KeyCode::Backspace, KeyModifiers::ALT),
                Chord::with(KeyCode::Backspace, KeyModifiers::ALT),
            ),
            (
                Chord::with(KeyCode::Backspace, KeyModifiers::ALT),
                Chord::ctrl('w'),
            ),
        ];
        for (a, b) in pairs {
            let as_event = KeyEvent::new(b.code, b.effective_modifiers());
            assert_eq!(
                a.same_key(&b),
                a.matches(&as_event),
                "same_key disagreed with matches for {} vs {}",
                a.wire(),
                b.wire()
            );
            assert_eq!(a.same_key(&b), b.same_key(&a), "same_key must be symmetric");
        }
    }

    /// The darwin-only half of the precedence contract. Platform-primary
    /// intent resolves to the same event as literal Super there, so
    /// an operator's explicit `super+a` and the earlier-declared
    /// `OpenFileBrowser`'s retained `primary+a` default are one chord at dispatch
    /// and two on the wire. Comparing raw values left the shadowed default in
    /// the table, and the operator's own binding lost to it by declaration
    /// order while Help advertised the chord twice.
    ///
    /// Only meaningful on darwin: elsewhere the two chords really are distinct
    /// and there is nothing to arbitrate. `same_key_agrees_with_dispatch` is
    /// the part that runs everywhere.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_super_override_outranks_a_primary_default_on_darwin() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let super_a = Chord::with(KeyCode::Char('a'), KeyModifiers::SUPER);

        with_input_bar_override("delete_previous_word", vec![super_a.clone()], || {
            assert_eq!(
                InputBarAction::from_chord(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER)),
                Some(InputBarAction::DeletePreviousWord),
                "the operator's explicit binding must win over a normalized default"
            );
            assert!(
                !action_key_labels(InputBarAction::OpenFileBrowser).contains(&super_a.display()),
                "the shadowed primary+a default must leave Help too"
            );
            assert_eq!(
                action_key_labels(InputBarAction::DeletePreviousWord),
                vec![super_a.display()]
            );
        });
    }

    #[test]
    fn an_explicit_binding_outranks_a_retained_default_on_another_action() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let alt_backspace = Chord::with(KeyCode::Backspace, KeyModifiers::ALT);

        // ClearInput is declared *after* DeletePreviousWord, whose defaults
        // include the same chord. Dispatch takes the first match, so without
        // the claim check the operator's binding would never be reached.
        with_input_bar_override("clear_input", vec![alt_backspace.clone()], || {
            assert_eq!(
                InputBarAction::from_chord(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT)),
                Some(InputBarAction::ClearInput),
                "an explicitly bound chord must reach the action the operator chose"
            );

            // Help must not advertise the chord for both actions.
            assert_eq!(
                action_key_labels(InputBarAction::ClearInput),
                vec![alt_backspace.display()]
            );
            assert!(
                !action_key_labels(InputBarAction::DeletePreviousWord)
                    .contains(&alt_backspace.display()),
                "the shadowed default must disappear from Help, not just from dispatch"
            );

            // The default that was not claimed is untouched.
            let primary_w = Chord::primary('w');
            assert_eq!(
                InputBarAction::from_chord(&KeyEvent::new(
                    KeyCode::Char('w'),
                    primary_w.effective_modifiers(),
                )),
                Some(InputBarAction::DeletePreviousWord)
            );
        });
    }

    #[test]
    fn override_precedence_does_not_depend_on_declaration_order() {
        use crossterm::event::{KeyCode, KeyEvent};

        // The mirror of the case above: the override is on the *earlier*
        // declared action and the retained default on the later one. Both
        // directions must land on the explicit binding, or the contract is
        // just an artifact of enum ordering.
        let primary_u = Chord::primary('u');
        with_input_bar_override("backspace", vec![primary_u.clone()], || {
            assert_eq!(
                InputBarAction::from_chord(&KeyEvent::new(
                    KeyCode::Char('u'),
                    primary_u.effective_modifiers(),
                )),
                Some(InputBarAction::Backspace)
            );
            assert!(
                !action_key_labels(InputBarAction::ClearInput).contains(&primary_u.display()),
                "ClearInput's retained default lost the chord to an explicit binding"
            );
        });
    }

    #[test]
    fn an_unrelated_override_leaves_other_defaults_alone() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        // Guard against the claim filter being too eager: a sparse override
        // elsewhere in the enum must not strip defaults it never mentions.
        with_input_bar_override("clear_input", vec![Chord::ctrl('k')], || {
            assert_eq!(
                InputBarAction::from_chord(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT)),
                Some(InputBarAction::DeletePreviousWord)
            );
            assert_eq!(
                InputBarAction::from_chord(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
                Some(InputBarAction::Backspace)
            );
            assert_eq!(
                action_key_labels(InputBarAction::DeletePreviousWord).len(),
                2
            );
        });
    }

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
        check(GlobalAction::TAG, GlobalAction::bindings());
        check(ChatTabAction::TAG, ChatTabAction::bindings());
        check(LogsTabAction::TAG, LogsTabAction::bindings());
        check(DashboardTabAction::TAG, DashboardTabAction::bindings());
        check(ConfigTabAction::TAG, ConfigTabAction::bindings());
        check(DoctorTabAction::TAG, DoctorTabAction::bindings());
        check(QuickstartTabAction::TAG, QuickstartTabAction::bindings());
        check(SopTabAction::TAG, SopTabAction::bindings());
        check(InputBarAction::TAG, InputBarAction::bindings());
        check(ModalAction::TAG, ModalAction::bindings());
        check(CaptureAction::TAG, CaptureAction::bindings());
        check(FileExplorerAction::TAG, FileExplorerAction::bindings());
        check(
            FileExplorerSearchAction::TAG,
            FileExplorerSearchAction::bindings(),
        );
        check(SearchBoxAction::TAG, SearchBoxAction::bindings());
        check(ConfigEditorAction::TAG, ConfigEditorAction::bindings());
        check(
            QuickstartModalAction::TAG,
            QuickstartModalAction::bindings(),
        );
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
            (
                "sop",
                SopTabAction::bindings()
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
        check::<SopTabAction>();
        check::<InputBarAction>();
        check::<FileExplorerAction>();
    }
}
