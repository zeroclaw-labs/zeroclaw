//! Action enums for the keymap.
//!
//! Each enum is produced by the `keyactions!` macro. Every variant
//! declares its default chords and label inline; the macro generates
//! the enum, `Serialize`/`Deserialize` derives, `label()`,
//! `bindings()`, and `from_chord()` from one source.

use serde::{Deserialize, Serialize};

use super::chord::Chord;

macro_rules! keyactions {
    (
        $vis:vis enum $name:ident {
            $( $variant:ident [ $($chord:expr),* $(,)? ] => $label:expr ),* $(,)?
        }
    ) => {
        #[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        $vis enum $name {
            $( $variant ),*
        }

        #[allow(dead_code)]
        impl $name {
            pub fn label(&self) -> &'static str {
                match self {
                    $( $name::$variant => $label ),*
                }
            }

            pub fn bindings() -> Vec<(Chord, $name)> {
                let mut out: Vec<(Chord, $name)> = Vec::new();
                $( for c in [ $( $chord ),* ] { out.push((c, $name::$variant)); } )*
                out
            }

            pub fn from_chord(event: &crossterm::event::KeyEvent) -> Option<$name> {
                super::match_chord(&Self::bindings(), event)
            }
        }
    };
}

use crossterm::event::{KeyCode, KeyModifiers};

keyactions! {
    pub enum GlobalAction {
        Quit         [Chord::ctrl('c')]                                 => "quit",
        Help         [Chord::char('?')]                                 => "help",
        PaneNavLeft  [Chord::with(KeyCode::Left, KeyModifiers::CONTROL)]  => "prev pane",
        PaneNavRight [Chord::with(KeyCode::Right, KeyModifiers::CONTROL)] => "next pane",
        ReloadDaemon [Chord::ctrl('r')]                                 => "reload daemon",
        ConfirmYes   []                                                 => "confirm",
        ConfirmNo    []                                                 => "cancel",
    }
}

keyactions! {
    pub enum ChatTabAction {
        ScrollUp                [] => "scroll up",
        ScrollDown              [] => "scroll down",
        PageUp                  [Chord::key(KeyCode::PageUp)] => "page up",
        PageDown                [Chord::key(KeyCode::PageDown)] => "page down",
        JumpStart               [Chord::char('g')] => "jump to start",
        JumpEnd                 [Chord::char('G')] => "jump to end",
        BrowseEnter             [Chord::with(KeyCode::Up, KeyModifiers::CONTROL)] => "enter browse mode",
        BrowseExit              [Chord::with(KeyCode::Down, KeyModifiers::CONTROL)] => "exit browse mode",
        BrowseUp                [Chord::key(KeyCode::Up)] => "browse prev",
        BrowseDown              [Chord::key(KeyCode::Down)] => "browse next",
        BrowseUpVim             [Chord::char('k')] => "browse prev (vim)",
        BrowseDownVim           [Chord::char('j')] => "browse next (vim)",
        BrowseSelectExtend      [Chord::shift(KeyCode::Up)] => "extend selection up",
        BrowseSelectExtendDown  [Chord::shift(KeyCode::Down)] => "extend selection down",
        FastScrollUp            [Chord::with(KeyCode::Up, KeyModifiers::CONTROL.union(KeyModifiers::SHIFT))] => "fast scroll up",
        FastScrollDown          [Chord::with(KeyCode::Down, KeyModifiers::CONTROL.union(KeyModifiers::SHIFT))] => "fast scroll down",
        BrowseExitSelection     [Chord::key(KeyCode::Esc)] => "exit selection",
        CopySelection           [Chord::char('y')] => "copy selection",
        CopyAllVisible          [Chord::with(KeyCode::Char('C'), KeyModifiers::CONTROL.union(KeyModifiers::SHIFT))] => "copy all visible",
        ToggleThoughts          [Chord::char('t')] => "toggle thoughts",
        NewSession              [Chord::ctrl('n')] => "new session",
        SwitchSession           [Chord::ctrl('s')] => "switch session",
        RenameSession           [Chord::ctrl('r')] => "rename session",
        DeleteSession           [] => "delete session",
        CancelTurn              [Chord::ctrl('d')] => "cancel turn",
        ApprovalApprove         [] => "approve",
        ApprovalDeny            [] => "deny",
        ApprovalApproveAll      [Chord::char('a')] => "approve all",
        ApprovalApproveEdit     [Chord::char('e')] => "approve + edit",
        DismissModal            [] => "dismiss",
    }
}

keyactions! {
    pub enum LogsTabAction {
        Up               [Chord::char('k'), Chord::key(KeyCode::Up)] => "prev event",
        Down             [Chord::char('j'), Chord::key(KeyCode::Down)] => "next event",
        PageUp           [Chord::key(KeyCode::PageUp)] => "page up",
        PageDown         [Chord::key(KeyCode::PageDown)] => "page down",
        JumpStart        [Chord::char('g'), Chord::key(KeyCode::Home)] => "jump to start",
        JumpEnd          [Chord::char('G'), Chord::key(KeyCode::End)] => "jump to end",
        OpenDetail       [Chord::key(KeyCode::Enter)] => "open detail",
        CloseDetail      [] => "close detail",
        DetailScrollUp   [Chord::char('K')] => "detail scroll up",
        DetailScrollDown [Chord::char('J')] => "detail scroll down",
        DetailWidenLeft  [Chord::shift(KeyCode::Left)] => "widen detail left",
        DetailWidenRight [Chord::shift(KeyCode::Right)] => "widen detail right",
        DetailWidenUp    [Chord::shift(KeyCode::Up)] => "widen detail up",
        DetailWidenDown  [Chord::shift(KeyCode::Down)] => "widen detail down",
        ToggleFollow     [Chord::char('f')] => "toggle follow",
        BeginSearch      [Chord::char('/')] => "search",
        ClearSearch      [Chord::char('c')] => "clear search",
        CopyDetail       [Chord::char('y')] => "copy detail",
        IncreaseLevel    [Chord::char('+'), Chord::char('=')] => "verbosity up",
        DecreaseLevel    [Chord::char('-')] => "verbosity down",
    }
}

keyactions! {
    pub enum DashboardTabAction {
        Up               [Chord::char('k'), Chord::key(KeyCode::Up)] => "prev",
        Down             [Chord::char('j'), Chord::key(KeyCode::Down)] => "next",
        NextTab          [Chord::key(KeyCode::Tab), Chord::char('l'), Chord::key(KeyCode::Right)] => "next tab",
        PrevTab          [Chord::key(KeyCode::BackTab), Chord::char('h'), Chord::key(KeyCode::Left)] => "prev tab",
        Tab1             [Chord::char('1')] => "tab 1",
        Tab2             [Chord::char('2')] => "tab 2",
        Tab3             [Chord::char('3')] => "tab 3",
        Tab4             [Chord::char('4')] => "tab 4",
        Tab5             [Chord::char('5')] => "tab 5",
        Tab6             [Chord::char('6')] => "tab 6",
        Tab7             [Chord::char('7')] => "tab 7",
        OpenDetail       [Chord::key(KeyCode::Enter)] => "open detail",
        CloseDetail      [] => "close detail",
        DetailScrollUp   [Chord::char('K')] => "detail scroll up",
        DetailScrollDown [Chord::char('J')] => "detail scroll down",
        DetailWidenLeft  [Chord::shift(KeyCode::Left)] => "widen detail left",
        DetailWidenRight [Chord::shift(KeyCode::Right)] => "widen detail right",
        DetailWidenUp    [Chord::shift(KeyCode::Up)] => "widen detail up",
        DetailWidenDown  [Chord::shift(KeyCode::Down)] => "widen detail down",
        BeginSearch      [Chord::char('/')] => "search",
        CopyDetail       [Chord::char('c')] => "copy detail",
        Refresh          [Chord::char('r')] => "refresh",
        JumpStart        [Chord::char('g'), Chord::key(KeyCode::Home)] => "jump to start",
        JumpEnd          [Chord::char('G'), Chord::key(KeyCode::End)] => "jump to end",
    }
}

keyactions! {
    pub enum ConfigTabAction {
        Up            [Chord::char('k'), Chord::key(KeyCode::Up)] => "prev",
        Down          [Chord::char('j'), Chord::key(KeyCode::Down)] => "next",
        Enter         [Chord::key(KeyCode::Enter)] => "open",
        Back          [Chord::char('q'), Chord::key(KeyCode::Esc)] => "back",
        TabLeft       [Chord::char('h'), Chord::key(KeyCode::Left)] => "prev tab",
        TabRight      [Chord::char('l'), Chord::key(KeyCode::Right)] => "next tab",
        ToggleSecret  [Chord::char('x')] => "toggle secret",
        DeleteRow     [Chord::char('d')] => "delete row",
        ApplyTemplate [Chord::char('t')] => "apply template",
    }
}

keyactions! {
    pub enum QuickstartTabAction {
        Up     [Chord::key(KeyCode::Up)] => "prev",
        Down   [Chord::key(KeyCode::Down)] => "next",
        Enter  [Chord::key(KeyCode::Enter)] => "open",
        Create [Chord::char('c'), Chord::char('C')] => "create agent",
    }
}

keyactions! {
    pub enum InputBarAction {
        Submit             [Chord::key(KeyCode::Enter)] => "send",
        NewLine            [Chord::shift(KeyCode::Enter)] => "new line",
        CursorLeft         [Chord::key(KeyCode::Left)] => "cursor left",
        CursorRight        [Chord::key(KeyCode::Right)] => "cursor right",
        CursorStart        [Chord::key(KeyCode::Home), Chord::ctrl('a')] => "line start",
        CursorEnd          [Chord::key(KeyCode::End), Chord::ctrl('e')] => "line end",
        Backspace          [Chord::key(KeyCode::Backspace)] => "backspace",
        SelectAll          [] => "select all",
        Paste              [Chord::ctrl('v')] => "paste",
        HistoryPrev        [Chord::key(KeyCode::Up)] => "history prev",
        HistoryNext        [Chord::key(KeyCode::Down)] => "history next",
        AutocompleteNext   [] => "autocomplete next",
        AutocompletePrev   [] => "autocomplete prev",
        AutocompleteAccept [Chord::key(KeyCode::Tab)] => "accept completion",
        AutocompleteCancel [Chord::key(KeyCode::Esc)] => "cancel completion",
        AttachClipboard    [] => "attach clipboard",
    }
}

keyactions! {
    pub enum ModalAction {
        Confirm [Chord::key(KeyCode::Enter), Chord::char('y'), Chord::char('Y')] => "confirm",
        Cancel  [Chord::key(KeyCode::Esc), Chord::char('n'), Chord::char('N')] => "cancel",
    }
}

keyactions! {
    pub enum FileExplorerAction {
        Up           [Chord::char('k'), Chord::key(KeyCode::Up)] => "prev",
        Down         [Chord::char('j'), Chord::key(KeyCode::Down)] => "next",
        JumpStart    [Chord::char('g'), Chord::key(KeyCode::Home)] => "jump to start",
        JumpEnd      [Chord::char('G'), Chord::key(KeyCode::End)] => "jump to end",
        EnterDir     [Chord::char('l'), Chord::key(KeyCode::Right)] => "enter dir",
        LeaveDir     [Chord::char('h'), Chord::key(KeyCode::Left), Chord::key(KeyCode::Backspace)] => "up dir",
        ToggleSelect [Chord::char(' ')] => "toggle select",
        Activate     [Chord::key(KeyCode::Enter)] => "open / attach",
        ToggleHidden [Chord::char('.')] => "toggle hidden",
        BeginSearch  [Chord::char('/')] => "search",
        ConfirmDir   [Chord::char('c')] => "confirm dir",
        Cancel       [Chord::char('q'), Chord::key(KeyCode::Esc)] => "cancel",
    }
}

keyactions! {
    pub enum FileExplorerSearchAction {
        Accept    [Chord::key(KeyCode::Enter)] => "accept",
        Cancel    [Chord::key(KeyCode::Esc)] => "cancel",
        Backspace [Chord::key(KeyCode::Backspace)] => "backspace",
    }
}

keyactions! {
    pub enum SearchBoxAction {
        Accept    [Chord::key(KeyCode::Enter)] => "accept",
        Cancel    [Chord::key(KeyCode::Esc)] => "cancel",
        Backspace [Chord::key(KeyCode::Backspace)] => "backspace",
    }
}

keyactions! {
    pub enum ConfigEditorAction {
        Confirm   [Chord::key(KeyCode::Enter)] => "confirm",
        Cancel    [Chord::key(KeyCode::Esc)] => "cancel",
        Save      [Chord::ctrl('s')] => "save",
        Backspace [Chord::key(KeyCode::Backspace)] => "backspace",
        Up        [Chord::key(KeyCode::Up)] => "prev",
        Down      [Chord::key(KeyCode::Down)] => "next",
    }
}

keyactions! {
    pub enum QuickstartModalAction {
        Confirm        [Chord::key(KeyCode::Enter)] => "confirm",
        Cancel         [Chord::key(KeyCode::Esc)] => "cancel",
        Up             [Chord::key(KeyCode::Up)] => "prev",
        Down           [Chord::key(KeyCode::Down)] => "next",
        Left           [Chord::key(KeyCode::Left)] => "left",
        Right          [Chord::key(KeyCode::Right)] => "right",
        NextField      [Chord::key(KeyCode::Tab)] => "next field",
        PrevField      [Chord::key(KeyCode::BackTab)] => "prev field",
        Backspace      [Chord::key(KeyCode::Backspace)] => "backspace",
        DeleteRow      [Chord::char('d'), Chord::char('D')] => "delete row",
        EditWithEditor [Chord::char('e'), Chord::char('E')] => "edit in $EDITOR",
        EditTemplate   [Chord::char('t'), Chord::char('T')] => "from template",
        EditCopy       [Chord::char('c'), Chord::char('C')] => "copy contents",
        Create         [] => "create",
    }
}
