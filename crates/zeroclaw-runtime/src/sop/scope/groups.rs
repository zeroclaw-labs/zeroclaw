const FILESYSTEM_ALIASES: &[&str] = &["fs", "filesystem"];
const WEB_ALIASES: &[&str] = &["web", "network"];
const SHELL_ALIASES: &[&str] = &["shell", "terminal"];
const SOP_ALIASES: &[&str] = &["sop", "sop-control", "sop_control"];

const FILESYSTEM_TOOLS: &[&str] = &["read_file", "write_file", "edit_file"];
const WEB_TOOLS: &[&str] = &["http_request", "web_search"];
const SHELL_TOOLS: &[&str] = &["shell"];
const SOP_TOOLS: &[&str] = &["sop_execute", "sop_advance", "sop_approve", "sop_status"];

pub(crate) fn expand_group(name: &str) -> Option<&'static [&'static str]> {
    let normalized = name.trim().to_ascii_lowercase();
    let name = normalized.as_str();

    if FILESYSTEM_ALIASES.contains(&name) {
        Some(FILESYSTEM_TOOLS)
    } else if WEB_ALIASES.contains(&name) {
        Some(WEB_TOOLS)
    } else if SHELL_ALIASES.contains(&name) {
        Some(SHELL_TOOLS)
    } else if SOP_ALIASES.contains(&name) {
        Some(SOP_TOOLS)
    } else {
        None
    }
}
