use extism_pdk::*;
use serde::Serialize;

#[derive(Serialize)]
struct HttpOutput {
    status_code: u16,
    body: String,
}

/// Makes a GET request to the `base_url` from config, passing an
/// `Authorization: Bearer <auth_token>` header (also from config).
/// Returns JSON with `status_code` and `body`.
#[plugin_fn]
pub fn tool_http_fetch(_input: String) -> FnResult<String> {
    let base_url = config::get("base_url")
        .unwrap_or(None)
        .unwrap_or_default();

    let auth_token = config::get("auth_token")
        .unwrap_or(None)
        .unwrap_or_default();

    let req = HttpRequest::new(&base_url)
        .with_header("Authorization", format!("Bearer {auth_token}"));

    let resp = http::request::<()>(&req, None)?;

    let result = HttpOutput {
        status_code: resp.status_code(),
        body: String::from_utf8_lossy(&resp.body()).into_owned(),
    };

    Ok(serde_json::to_string(&result)?)
}
