use std::ffi::OsString;
use std::io::Write;
use std::process::Command;

use crossterm::{
    event::{KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

use crate::config_manager::Term;

pub(crate) fn editor_from_env_or_path() -> Option<String> {
    std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| {
            fallback_editors()
                .iter()
                .copied()
                .find(|candidate| executable_on_path(candidate))
                .map(str::to_string)
        })
}

pub(crate) fn edit_text_in_external_editor(
    term: &mut Term,
    content: &str,
    filename_hint: &str,
) -> Result<String, String> {
    let Some(editor) = editor_from_env_or_path() else {
        return Err(crate::i18n::t("zc-input-editor-missing"));
    };
    let mut tempfile = tempfile::Builder::new()
        .prefix("zerocode-")
        .suffix(&temp_suffix(filename_hint))
        .tempfile()
        .map_err(|error| format!("tmp create: {error}"))?;
    tempfile
        .write_all(content.as_bytes())
        .map_err(|error| format!("tmp write: {error}"))?;
    tempfile
        .flush()
        .map_err(|error| format!("tmp flush: {error}"))?;
    let tmp_path = tempfile.path().to_path_buf();

    suspend_terminal(term)?;
    let status = run_editor(&editor, tmp_path.as_os_str().to_os_string());
    restore_terminal(term)?;

    let result = match status {
        Ok(status) if status.success() => {
            std::fs::read_to_string(&tmp_path).map_err(|error| format!("tmp read: {error}"))
        }
        Ok(status) => Err(format!("{editor} exited with {status}")),
        Err(error) => Err(format!("failed to launch {editor}: {error}")),
    };
    tempfile
        .close()
        .map_err(|error| format!("tmp cleanup: {error}"))?;
    result
}

pub(crate) fn editor_command_parts(editor: &str) -> (String, Vec<String>) {
    let mut parts = shlex::split(editor)
        .filter(|parts| !parts.is_empty())
        .unwrap_or_else(|| vec![editor.to_string()]);
    let program = parts.remove(0);
    (program, parts)
}

fn run_editor(editor: &str, path: OsString) -> std::io::Result<std::process::ExitStatus> {
    let (program, args) = editor_command_parts(editor);
    Command::new(program).args(args).arg(path).status()
}

fn executable_on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
}

fn fallback_editors() -> &'static [&'static str] {
    if cfg!(windows) {
        &["notepad.exe", "nano", "vim"]
    } else {
        &["nano", "vi", "vim", "editor"]
    }
}

fn temp_suffix(filename_hint: &str) -> String {
    let cleaned: String = filename_hint
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => character,
            _ => '-',
        })
        .collect();
    format!("-{cleaned}")
}

fn keyboard_enhancement_flags() -> KeyboardEnhancementFlags {
    KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        | KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
}

fn suspend_terminal(term: &mut Term) -> Result<(), String> {
    execute!(
        term.backend_mut(),
        PopKeyboardEnhancementFlags,
        LeaveAlternateScreen
    )
    .map_err(|error| format!("terminal suspend: {error}"))?;
    disable_raw_mode().map_err(|error| format!("terminal raw mode off: {error}"))
}

fn restore_terminal(term: &mut Term) -> Result<(), String> {
    enable_raw_mode().map_err(|error| format!("terminal raw mode on: {error}"))?;
    execute!(term.backend_mut(), EnterAlternateScreen)
        .map_err(|error| format!("terminal restore: {error}"))?;
    match crossterm::terminal::supports_keyboard_enhancement() {
        Ok(true) => execute!(
            term.backend_mut(),
            PushKeyboardEnhancementFlags(keyboard_enhancement_flags())
        )
        .map_err(|error| format!("terminal keyboard flags: {error}"))?,
        Ok(false) => {}
        Err(error) => return Err(format!("terminal keyboard support: {error}")),
    }
    term.clear()
        .map_err(|error| format!("terminal redraw: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_suffix_sanitizes_hint() {
        assert_eq!(temp_suffix("../prompt md"), "-..-prompt-md");
    }
}
