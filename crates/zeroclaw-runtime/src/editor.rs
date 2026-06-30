use anyhow::Result;
use std::path::Path;

pub fn open_in_editor(path: &Path) -> Result<()> {
    let Some(editor) = editor_from_env_or_path() else {
        anyhow::bail!("no editor found; set VISUAL or EDITOR");
    };
    let status = std::process::Command::new(&editor).arg(path).status()?;
    if !status.success() {
        anyhow::bail!("{editor} exited with non-zero status");
    }
    Ok(())
}

#[must_use]
pub fn editor_from_env_or_path() -> Option<String> {
    let visual = std::env::var("VISUAL").ok();
    let editor = std::env::var("EDITOR").ok();
    resolve_editor(visual.as_deref(), editor.as_deref(), executable_on_path)
}

fn resolve_editor(
    visual: Option<&str>,
    editor: Option<&str>,
    on_path: impl Fn(&str) -> bool,
) -> Option<String> {
    let non_blank = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|trimmed| !trimmed.is_empty())
            .map(str::to_string)
    };
    non_blank(visual).or_else(|| non_blank(editor)).or_else(|| {
        fallback_editors()
            .iter()
            .copied()
            .find(|candidate| on_path(candidate))
            .map(str::to_string)
    })
}

fn executable_on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
}

#[must_use]
pub fn fallback_editors() -> &'static [&'static str] {
    if cfg!(windows) {
        &["notepad.exe", "nano", "vim"]
    } else {
        &["nano", "vi", "vim", "editor"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visual_wins_over_editor_and_path() {
        let chosen = resolve_editor(Some("emacs"), Some("vim"), |_| true);
        assert_eq!(chosen.as_deref(), Some("emacs"));
    }

    #[test]
    fn editor_used_when_visual_blank() {
        let chosen = resolve_editor(Some("   "), Some("vim"), |_| false);
        assert_eq!(chosen.as_deref(), Some("vim"));
    }

    #[test]
    fn falls_back_to_first_on_path_when_env_absent() {
        let chosen = resolve_editor(None, None, |name| name == "vi");
        assert_eq!(chosen.as_deref(), Some("vi"));
    }

    #[test]
    fn none_when_nothing_resolves() {
        let chosen = resolve_editor(None, Some(""), |_| false);
        assert_eq!(chosen, None);
    }
}
