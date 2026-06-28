pub(crate) fn expand_group(name: &str) -> Option<&'static [&'static str]> {
    match name.trim().to_ascii_lowercase().as_str() {
        "fs" | "filesystem" => Some(&["read_file", "write_file", "edit_file"]),
        "web" | "network" => Some(&["http_request", "web_search"]),
        "shell" | "terminal" => Some(&["shell"]),
        "sop" | "sop-control" | "sop_control" => {
            Some(&["sop_execute", "sop_advance", "sop_approve", "sop_status"])
        }
        _ => None,
    }
}
