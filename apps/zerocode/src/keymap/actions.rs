//! Action enums for the keymap.

use serde::{Deserialize, Serialize};

use super::chord::Chord;

macro_rules! keyactions {
    (
        $vis:vis enum $name:ident ( $tag:literal ) {
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
            /// Stable per-enum tag namespacing serialized keys
            /// (`"<tag>.<variant>"`).
            pub const TAG: &'static str = $tag;

            /// Every variant in declaration order — walked by the
            /// keybind surface and override loader.
            pub fn variants() -> &'static [$name] {
                &[ $( $name::$variant ),* ]
            }

            pub fn label(&self) -> &'static str {
                match self {
                    $( $name::$variant => $label ),*
                }
            }

            /// This variant's serialized (snake_case) name, via serde so
            /// it can't drift from the wire form.
            pub fn variant_name(&self) -> String {
                serde_json::to_string(self)
                    .ok()
                    .map(|s| s.trim_matches('"').to_string())
                    .unwrap_or_default()
            }

            /// Fully-qualified action key: `"<tag>.<variant>"`.
            pub fn action_key(&self) -> String {
                format!("{}.{}", Self::TAG, self.variant_name())
            }

            /// Compile-time default chords for this variant.
            pub fn default_chords(&self) -> Vec<Chord> {
                match self {
                    $( $name::$variant => vec![ $( $chord ),* ] ),*
                }
            }

            pub fn bindings() -> Vec<(Chord, $name)> {
                let mut out: Vec<(Chord, $name)> = Vec::new();
                $( for c in [ $( $chord ),* ] { out.push((c, $name::$variant)); } )*
                out
            }

            pub fn from_chord(event: &crossterm::event::KeyEvent) -> Option<$name> {
                super::match_chord(&Self::resolved_bindings(), event)
            }

            /// Bindings after applying any runtime override for `TAG`;
            /// falls back to the compile-time table when none is active.
            /// Sparse: an un-overridden variant keeps its default chords.
            pub fn resolved_bindings() -> Vec<(Chord, $name)> {
                let Some(over) = super::overrides::lookup(Self::TAG) else {
                    return Self::bindings();
                };
                let mut out: Vec<(Chord, $name)> = Vec::new();
                for v in Self::variants() {
                    let chords = match over.get(&v.variant_name()) {
                        Some(cs) => cs.clone(),
                        None => v.default_chords(),
                    };
                    for c in chords {
                        out.push((c, *v));
                    }
                }
                out
            }
        }

        impl super::RebindableActions for $name {
            fn tag() -> &'static str {
                Self::TAG
            }
            fn all() -> &'static [Self] {
                Self::variants()
            }
            fn key(&self) -> String {
                self.action_key()
            }
            fn human_label(&self) -> &'static str {
                self.label()
            }
            fn defaults(&self) -> Vec<Chord> {
                self.default_chords()
            }
            fn resolved(&self) -> Vec<Chord> {
                Self::resolved_bindings()
                    .into_iter()
                    .filter(|(_, a)| a == self)
                    .map(|(c, _)| c)
                    .collect()
            }
        }
    };
}

use crossterm::event::{KeyCode, KeyModifiers};

keyactions! {
    pub enum GlobalAction ("global") {
        Quit         [Chord::ctrl('c')]                                 => "quit",
        Help         [
            Chord::char('?'),
            Chord::ctrl('g'),
        ]                                                              => "help",
        PaneNavLeft  [Chord::with(KeyCode::Left, KeyModifiers::ALT), Chord::with(KeyCode::Char('b'), KeyModifiers::ALT)]  => "prev pane",
        PaneNavRight [Chord::with(KeyCode::Right, KeyModifiers::ALT), Chord::with(KeyCode::Char('f'), KeyModifiers::ALT)] => "next pane",
        ReloadDaemon [Chord::ctrl('r')]                                 => "reload daemon",
        ConfirmYes   []                                                 => "confirm",
        ConfirmNo    []                                                 => "cancel",
    }
}

keyactions! {
    pub enum ChatTabAction ("chat") {
        ScrollUp                [] => "scroll up",
        ScrollDown              [] => "scroll down",
        PageUp                  [Chord::key(KeyCode::PageUp)] => "page up",
        PageDown                [Chord::key(KeyCode::PageDown)] => "page down",
        JumpStart               [Chord::char('g')] => "jump to start",
        JumpEnd                 [Chord::char('G')] => "jump to end",
        // Use alt+shift+up/down to avoid macOS Mission Control conflict (ctrl+up/down)
        // and queue navigation conflict (alt+up/down).
        BrowseEnter             [
            Chord::with(KeyCode::Up, KeyModifiers::ALT.union(KeyModifiers::SHIFT)),
            Chord::ctrl('k'),
        ] => "enter browse mode",
        BrowseExit              [Chord::with(KeyCode::Down, KeyModifiers::ALT.union(KeyModifiers::SHIFT))] => "exit browse mode",
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
        TodoToggle              [Chord::ctrl('p')] => "toggle todo tracker",
        NewSession              [Chord::ctrl('n')] => "new session",
        SwitchSession           [Chord::ctrl('s')] => "switch session",
        DeleteSession           [] => "delete session",
        CancelTurn              [Chord::ctrl('d')] => "cancel turn",
        ApprovalApprove         [Chord::key(KeyCode::Enter)] => "approve",
        ApprovalDeny            [] => "deny",
        ApprovalApproveAll      [Chord::char('a')] => "approve all",
        ApprovalApproveEdit     [Chord::char('e')] => "approve + edit",
        DismissModal            [] => "dismiss",
        PauseResumeQueue        [Chord::with(KeyCode::Char('p'), KeyModifiers::ALT)] => "pause/resume queue",
        QueueNavUp              [Chord::with(KeyCode::Up, KeyModifiers::ALT)] => "queue prev",
        QueueNavDown            [Chord::with(KeyCode::Down, KeyModifiers::ALT)] => "queue next",
        QueueDelete             [Chord::with(KeyCode::Char('x'), KeyModifiers::ALT)] => "delete queued",
        QueueEdit               [Chord::with(KeyCode::Char('e'), KeyModifiers::ALT)] => "edit queued",
        QueueWiden              [Chord::shift(KeyCode::Left)] => "widen queue",
        QueueNarrow             [Chord::shift(KeyCode::Right)] => "narrow queue",
        ErrorDismiss            [Chord::char('q')] => "dismiss error",
    }
}

keyactions! {
    pub enum LogsTabAction ("logs") {
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
    pub enum DashboardTabAction ("dashboard") {
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
        RenameAgent      [Chord::char('e')] => "rename agent",
        KillSession      [Chord::char('X')] => "kill session",
        TriggerCron      [Chord::char('R')] => "run cron job now",
        Refresh          [Chord::char('r')] => "refresh",
        JumpStart        [Chord::char('g'), Chord::key(KeyCode::Home)] => "jump to start",
        JumpEnd          [Chord::char('G'), Chord::key(KeyCode::End)] => "jump to end",
    }
}

keyactions! {
    pub enum ConfigTabAction ("config_tab") {
        Up            [Chord::char('k'), Chord::key(KeyCode::Up)] => "prev",
        Down          [Chord::char('j'), Chord::key(KeyCode::Down)] => "next",
        Enter         [Chord::key(KeyCode::Enter)] => "open",
        Back          [Chord::char('q'), Chord::key(KeyCode::Esc)] => "back",
        TabLeft       [Chord::char('h'), Chord::key(KeyCode::Left)] => "prev tab",
        TabRight      [Chord::char('l'), Chord::key(KeyCode::Right)] => "next tab",
        SectionNext   [Chord::key(KeyCode::Tab)] => "next section",
        SectionPrev   [Chord::key(KeyCode::BackTab)] => "prev section",
        BeginSearch   [Chord::char('/')] => "search",
        ToggleSecret  [Chord::char('x')] => "toggle secret",
        DeleteRow     [Chord::char('d')] => "delete row",
        ApplyTemplate [Chord::char('t')] => "apply template",
    }
}

keyactions! {
    pub enum CaptureAction ("capture") {
        Cancel [Chord::key(KeyCode::Esc)] => "cancel capture",
    }
}

keyactions! {
    pub enum DoctorTabAction ("doctor") {
        Up         [Chord::char('k'), Chord::key(KeyCode::Up)] => "prev",
        Down       [Chord::char('j'), Chord::key(KeyCode::Down)] => "next",
        Refresh    [Chord::char('r')] => "refresh",
        FilterNext [Chord::char('+'), Chord::char('=')] => "next filter",
        FilterPrev [Chord::char('-')] => "prev filter",
        PageUp     [Chord::key(KeyCode::PageUp)] => "page up",
        PageDown   [Chord::key(KeyCode::PageDown)] => "page down",
        JumpStart  [Chord::char('g'), Chord::key(KeyCode::Home)] => "jump to start",
        JumpEnd    [Chord::char('G'), Chord::key(KeyCode::End)] => "jump to end",
    }
}

keyactions! {
    pub enum QuickstartTabAction ("quickstart") {
        Up     [Chord::char('k'), Chord::key(KeyCode::Up)] => "prev",
        Down   [Chord::char('j'), Chord::key(KeyCode::Down)] => "next",
        Enter  [Chord::key(KeyCode::Enter)] => "open",
        Back   [Chord::char('q'), Chord::key(KeyCode::Esc)] => "leave",
        Create [Chord::char('c'), Chord::char('C')] => "create agent",
    }
}

keyactions! {
    pub enum SopTabAction ("sop") {
        Up     [Chord::char('k'), Chord::key(KeyCode::Up)] => "prev",
        Down   [Chord::char('j'), Chord::key(KeyCode::Down)] => "next",
        Enter  [Chord::key(KeyCode::Enter)] => "load graph",
        Run    [Chord::char('r'), Chord::char('R')] => "run manual",
        Watch  [Chord::char('w'), Chord::char('W')] => "watch run",
        New    [Chord::char('n')] => "new sop",
        Edit   [Chord::char('e')] => "edit sop",
        Delete [Chord::char('d')] => "delete sop",
        Approve [Chord::char('a'), Chord::char('A')] => "approve checkpoint",
        Deny    [Chord::char('x'), Chord::char('X')] => "deny checkpoint",
        Toggle [Chord::char('v')] => "toggle layer",
        PanLeft  [Chord::shift(KeyCode::Left)] => "pan left",
        PanRight [Chord::shift(KeyCode::Right)] => "pan right",
        PanUp    [Chord::shift(KeyCode::Up)] => "pan up",
        PanDown  [Chord::shift(KeyCode::Down)] => "pan down",
    }
}

keyactions! {
    pub enum SopEditorAction ("sop_editor") {
        SourcePrev  [Chord::with(KeyCode::Left, KeyModifiers::ALT)]  => "prev trigger source",
        SourceNext  [Chord::with(KeyCode::Right, KeyModifiers::ALT)] => "next trigger source",
        ChannelNext [Chord::with(KeyCode::Char('c'), KeyModifiers::ALT)] => "cycle channel",
        AliasNext   [Chord::with(KeyCode::Char('a'), KeyModifiers::ALT)] => "cycle alias",
        Add         [Chord::with(KeyCode::Char('n'), KeyModifiers::ALT)] => "add trigger",
        Remove      [Chord::with(KeyCode::Char('x'), KeyModifiers::ALT)] => "remove trigger",
    }
}

keyactions! {
    pub enum InputBarAction ("input_bar") {
        Submit             [Chord::key(KeyCode::Enter)] => "send",
        Inject             [Chord::with(KeyCode::Enter, KeyModifiers::CONTROL)] => "send now",
        NewLine            [Chord::shift(KeyCode::Enter)] => "new line",
        CursorLeft         [Chord::key(KeyCode::Left)] => "cursor left",
        CursorRight        [Chord::key(KeyCode::Right)] => "cursor right",
        CursorWordLeft     [Chord::with(KeyCode::Left, KeyModifiers::ALT), Chord::with(KeyCode::Char('b'), KeyModifiers::ALT)] => "word left",
        CursorWordRight    [Chord::with(KeyCode::Right, KeyModifiers::ALT), Chord::with(KeyCode::Char('f'), KeyModifiers::ALT)] => "word right",
        CursorStart        [Chord::key(KeyCode::Home)] => "line start",
        CursorEnd          [Chord::key(KeyCode::End), Chord::ctrl('e')] => "line end",
        OpenFileBrowser    [Chord::ctrl('a')] => "browse files",
        Backspace          [Chord::key(KeyCode::Backspace)] => "backspace",
        DeletePreviousWord [Chord::ctrl('w')] => "delete previous word",
        ClearInput         [Chord::ctrl('u')] => "clear input",
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
    pub enum ModalAction ("modal") {
        Confirm [Chord::key(KeyCode::Enter), Chord::char('y'), Chord::char('Y')] => "confirm",
        Cancel  [Chord::key(KeyCode::Esc), Chord::char('n'), Chord::char('N')] => "cancel",
        Up      [Chord::key(KeyCode::Up)] => "prev",
        Down    [Chord::key(KeyCode::Down)] => "next",
        Toggle  [Chord::char(' ')] => "toggle selection",
    }
}

keyactions! {
    pub enum FileExplorerAction ("file_explorer") {
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
    pub enum FileExplorerSearchAction ("file_explorer_search") {
        Accept    [Chord::key(KeyCode::Enter)] => "accept",
        Cancel    [Chord::key(KeyCode::Esc)] => "cancel",
        Backspace [Chord::key(KeyCode::Backspace)] => "backspace",
    }
}

keyactions! {
    pub enum SearchBoxAction ("search_box") {
        Accept    [Chord::key(KeyCode::Enter)] => "accept",
        Cancel    [Chord::key(KeyCode::Esc)] => "cancel",
        Backspace [Chord::key(KeyCode::Backspace)] => "backspace",
        Up        [Chord::key(KeyCode::Up)] => "prev",
        Down      [Chord::key(KeyCode::Down)] => "next",
    }
}

keyactions! {
    pub enum ConfigEditorAction ("config_editor") {
        Confirm   [Chord::key(KeyCode::Enter)] => "confirm",
        Cancel    [Chord::key(KeyCode::Esc)] => "cancel",
        Save      [Chord::ctrl('s')] => "save",
        Backspace [Chord::key(KeyCode::Backspace)] => "backspace",
        CursorLeft      [Chord::key(KeyCode::Left)] => "cursor left",
        CursorRight     [Chord::key(KeyCode::Right)] => "cursor right",
        CursorWordLeft  [Chord::with(KeyCode::Left, KeyModifiers::ALT), Chord::with(KeyCode::Char('b'), KeyModifiers::ALT)] => "word left",
        CursorWordRight [Chord::with(KeyCode::Right, KeyModifiers::ALT), Chord::with(KeyCode::Char('f'), KeyModifiers::ALT)] => "word right",
        CursorStart     [Chord::key(KeyCode::Home)] => "input start",
        CursorEnd       [Chord::key(KeyCode::End)] => "input end",
        Up        [Chord::key(KeyCode::Up)] => "prev",
        Down      [Chord::key(KeyCode::Down)] => "next",
    }
}

keyactions! {
    pub enum QuickstartModalAction ("quickstart_modal") {
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
        Save           [Chord::ctrl('s')] => "save",
        EditWithEditor [Chord::char('e'), Chord::char('E')] => "edit file",
        EditTemplate   [Chord::char('t'), Chord::char('T')] => "from template",
        ClearFile      [Chord::char('c'), Chord::char('C')] => "clear file",
        Create         [] => "create",
    }
}
