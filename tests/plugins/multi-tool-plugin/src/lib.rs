use extism_pdk::*;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Deserialize)]
struct AddInput {
    a: f64,
    b: f64,
}

#[derive(Serialize)]
struct AddOutput {
    sum: f64,
}

#[plugin_fn]
pub fn tool_add(input: String) -> FnResult<String> {
    let params: AddInput = serde_json::from_str(&input)?;
    let result = AddOutput {
        sum: params.a + params.b,
    };
    Ok(serde_json::to_string(&result)?)
}

#[derive(Deserialize)]
struct ReverseInput {
    text: String,
}

#[derive(Serialize)]
struct ReverseOutput {
    reversed: String,
}

#[plugin_fn]
pub fn tool_reverse_string(input: String) -> FnResult<String> {
    let params: ReverseInput = serde_json::from_str(&input)?;
    let result = ReverseOutput {
        reversed: params.text.chars().rev().collect(),
    };
    Ok(serde_json::to_string(&result)?)
}

#[derive(Serialize)]
struct ConfigOutput {
    api_key: Option<String>,
    model: Option<String>,
}

#[plugin_fn]
pub fn tool_lookup_config(_input: String) -> FnResult<String> {
    let result = ConfigOutput {
        api_key: config::get("api_key").unwrap_or(None),
        model: config::get("model").unwrap_or(None),
    };
    Ok(serde_json::to_string(&result)?)
}

#[derive(Deserialize)]
struct HttpGetInput {
    url: String,
}

#[derive(Serialize)]
struct HttpGetOutput {
    status_code: u16,
    body: String,
}

#[plugin_fn]
pub fn tool_http_get(input: String) -> FnResult<String> {
    let params: HttpGetInput = serde_json::from_str(&input)?;
    let req = HttpRequest::new(&params.url);
    let resp = http::request::<()>(&req, None)?;
    let result = HttpGetOutput {
        status_code: resp.status_code(),
        body: String::from_utf8_lossy(&resp.body()).into_owned(),
    };
    Ok(serde_json::to_string(&result)?)
}

/// Like tool_http_get but reads `api_key` from config and sends it as
/// an `Authorization: Bearer <api_key>` header.
#[plugin_fn]
pub fn tool_http_get_auth(input: String) -> FnResult<String> {
    let params: HttpGetInput = serde_json::from_str(&input)?;
    let api_key = config::get("api_key")
        .unwrap_or(None)
        .unwrap_or_default();
    let req = HttpRequest::new(&params.url)
        .with_header("Authorization", format!("Bearer {api_key}"));
    let resp = http::request::<()>(&req, None)?;
    let result = HttpGetOutput {
        status_code: resp.status_code(),
        body: String::from_utf8_lossy(&resp.body()).into_owned(),
    };
    Ok(serde_json::to_string(&result)?)
}

#[derive(Deserialize)]
struct ReadFileInput {
    path: String,
}

#[derive(Serialize)]
struct ReadFileOutput {
    contents: String,
}

#[plugin_fn]
pub fn tool_read_file(input: String) -> FnResult<String> {
    let params: ReadFileInput = serde_json::from_str(&input)?;
    let contents = fs::read_to_string(&params.path)?;
    let result = ReadFileOutput { contents };
    Ok(serde_json::to_string(&result)?)
}
