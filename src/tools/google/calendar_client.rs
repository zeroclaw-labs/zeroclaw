use anyhow::Context;

const CALENDAR_BASE: &str = "https://www.googleapis.com/calendar/v3";

/// Route a Google Calendar API v3 call by resource and method.
///
/// `params` holds both URL path identifiers (`calendarId`, `eventId`) and
/// query string parameters. Path identifiers are extracted before the
/// remainder is forwarded as query parameters.
///
/// Supported operations:
/// - `calendars/list`  → `GET /users/me/calendarList`
/// - `calendars/get`   → `GET /users/me/calendarList/{calendarId}`
/// - `events/list`     → `GET /calendars/{calendarId}/events`
/// - `events/get`      → `GET /calendars/{calendarId}/events/{eventId}`
/// - `events/insert`   → `POST /calendars/{calendarId}/events`
/// - `events/patch`    → `PATCH /calendars/{calendarId}/events/{eventId}`
/// - `events/update`   → `PUT /calendars/{calendarId}/events/{eventId}`
/// - `events/delete`   → `DELETE /calendars/{calendarId}/events/{eventId}`
pub async fn dispatch(
    http: &reqwest::Client,
    token: &str,
    resource: &str,
    method: &str,
    params: Option<serde_json::Value>,
    body: Option<serde_json::Value>,
    timeout_secs: u64,
) -> anyhow::Result<serde_json::Value> {
    // Extract path-level identifiers so they are not forwarded as query params.
    let mut query = params
        .and_then(|v| {
            if let serde_json::Value::Object(m) = v {
                Some(m)
            } else {
                None
            }
        })
        .unwrap_or_default();

    let calendar_id = query
        .remove("calendarId")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "primary".to_string());

    let event_id = query
        .remove("eventId")
        .and_then(|v| v.as_str().map(|s| s.to_string()));

    let cal_enc = urlencoding::encode(&calendar_id).into_owned();
    let timeout = std::time::Duration::from_secs(timeout_secs);

    match (resource, method) {
        // ── CalendarList ─────────────────────────────────────────────
        ("calendars", "list") => {
            let url = format!("{CALENDAR_BASE}/users/me/calendarList");
            get_json(http, token, &url, &query, timeout).await
        }
        ("calendars", "get") => {
            let url = format!("{CALENDAR_BASE}/users/me/calendarList/{cal_enc}");
            get_json(http, token, &url, &query, timeout).await
        }

        // ── Events ───────────────────────────────────────────────────
        ("events", "list") => {
            let url = format!("{CALENDAR_BASE}/calendars/{cal_enc}/events");
            get_json(http, token, &url, &query, timeout).await
        }
        ("events", "get") => {
            let event_id =
                event_id.context("google calendar events get: 'eventId' is required in params")?;
            let ev_enc = urlencoding::encode(&event_id).into_owned();
            let url = format!("{CALENDAR_BASE}/calendars/{cal_enc}/events/{ev_enc}");
            get_json(http, token, &url, &query, timeout).await
        }
        ("events", "insert") => {
            let url = format!("{CALENDAR_BASE}/calendars/{cal_enc}/events");
            let body = body.unwrap_or(serde_json::Value::Object(serde_json::Map::default()));
            post_json(http, token, &url, &query, &body, timeout).await
        }
        ("events", "patch") => {
            let event_id = event_id
                .context("google calendar events patch: 'eventId' is required in params")?;
            let ev_enc = urlencoding::encode(&event_id).into_owned();
            let url = format!("{CALENDAR_BASE}/calendars/{cal_enc}/events/{ev_enc}");
            let body = body.unwrap_or(serde_json::Value::Object(serde_json::Map::default()));
            patch_json(http, token, &url, &query, &body, timeout).await
        }
        ("events", "update") => {
            let event_id = event_id
                .context("google calendar events update: 'eventId' is required in params")?;
            let ev_enc = urlencoding::encode(&event_id).into_owned();
            let url = format!("{CALENDAR_BASE}/calendars/{cal_enc}/events/{ev_enc}");
            let body = body.unwrap_or(serde_json::Value::Object(serde_json::Map::default()));
            put_json(http, token, &url, &query, &body, timeout).await
        }
        ("events", "delete") => {
            let event_id = event_id
                .context("google calendar events delete: 'eventId' is required in params")?;
            let ev_enc = urlencoding::encode(&event_id).into_owned();
            let url = format!("{CALENDAR_BASE}/calendars/{cal_enc}/events/{ev_enc}");
            delete_request(http, token, &url, &query, timeout).await
        }

        _ => anyhow::bail!(
            "google calendar: unsupported operation {resource}/{method}. \
             Supported: calendars/{{list,get}}, \
             events/{{list,get,insert,patch,update,delete}}"
        ),
    }
}

// ── HTTP helpers ─────────────────────────────────────────────────────────────

fn build_query_pairs(query: &serde_json::Map<String, serde_json::Value>) -> Vec<(String, String)> {
    query
        .iter()
        .map(|(k, v)| {
            let val = v
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| v.to_string());
            (k.clone(), val)
        })
        .collect()
}

async fn get_json(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    query: &serde_json::Map<String, serde_json::Value>,
    timeout: std::time::Duration,
) -> anyhow::Result<serde_json::Value> {
    let qp = build_query_pairs(query);
    let resp = http
        .get(url)
        .bearer_auth(token)
        .query(&qp)
        .timeout(timeout)
        .send()
        .await
        .context("google calendar: HTTP GET failed")?;
    handle_json_response(resp).await
}

async fn post_json(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    query: &serde_json::Map<String, serde_json::Value>,
    body: &serde_json::Value,
    timeout: std::time::Duration,
) -> anyhow::Result<serde_json::Value> {
    let qp = build_query_pairs(query);
    let resp = http
        .post(url)
        .bearer_auth(token)
        .query(&qp)
        .json(body)
        .timeout(timeout)
        .send()
        .await
        .context("google calendar: HTTP POST failed")?;
    handle_json_response(resp).await
}

async fn patch_json(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    query: &serde_json::Map<String, serde_json::Value>,
    body: &serde_json::Value,
    timeout: std::time::Duration,
) -> anyhow::Result<serde_json::Value> {
    let qp = build_query_pairs(query);
    let resp = http
        .patch(url)
        .bearer_auth(token)
        .query(&qp)
        .json(body)
        .timeout(timeout)
        .send()
        .await
        .context("google calendar: HTTP PATCH failed")?;
    handle_json_response(resp).await
}

async fn put_json(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    query: &serde_json::Map<String, serde_json::Value>,
    body: &serde_json::Value,
    timeout: std::time::Duration,
) -> anyhow::Result<serde_json::Value> {
    let qp = build_query_pairs(query);
    let resp = http
        .put(url)
        .bearer_auth(token)
        .query(&qp)
        .json(body)
        .timeout(timeout)
        .send()
        .await
        .context("google calendar: HTTP PUT failed")?;
    handle_json_response(resp).await
}

async fn delete_request(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    query: &serde_json::Map<String, serde_json::Value>,
    timeout: std::time::Duration,
) -> anyhow::Result<serde_json::Value> {
    let qp = build_query_pairs(query);
    let resp = http
        .delete(url)
        .bearer_auth(token)
        .query(&qp)
        .timeout(timeout)
        .send()
        .await
        .context("google calendar: HTTP DELETE failed")?;
    // 204 No Content is a successful delete
    if resp.status().as_u16() == 204 {
        return Ok(serde_json::json!({"success": true}));
    }
    handle_json_response(resp).await
}

async fn handle_json_response(resp: reqwest::Response) -> anyhow::Result<serde_json::Value> {
    let status = resp.status();
    if status.is_success() {
        let body = resp
            .text()
            .await
            .context("google calendar: failed to read response body")?;
        if body.is_empty() {
            return Ok(serde_json::json!({"success": true}));
        }
        serde_json::from_str(&body).context("google calendar: failed to parse JSON response")
    } else {
        let body = resp.text().await.unwrap_or_default();
        let detail = extract_error_detail(&body);
        tracing::debug!("google calendar: raw API error body: {body}");
        anyhow::bail!("google calendar: request failed ({status}{detail})")
    }
}

/// Extract a brief, safe error detail from a Google API error envelope.
/// Truncated to avoid leaking sensitive information into error messages.
fn extract_error_detail(body: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return String::new();
    };
    // Prefer the structured reason code from the errors array.
    if let Some(reason) = v
        .get("error")
        .and_then(|e| e.get("errors"))
        .and_then(|e| e.as_array())
        .and_then(|arr| arr.first())
        .and_then(|e| e.get("reason"))
        .and_then(|r| r.as_str())
    {
        return format!(", reason: {reason}");
    }
    // Fall back to the top-level message, truncated.
    if let Some(msg) = v
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
    {
        let truncated = &msg[..msg.len().min(120)];
        return format!(", message: {truncated}");
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_error_detail_handles_google_error_envelope() {
        let body = r#"{"error": {"code": 403, "message": "Insufficient Permission", "errors": [{"reason": "insufficientPermissions"}]}}"#;
        let detail = extract_error_detail(body);
        assert!(detail.contains("insufficientPermissions"), "got: {detail}");
    }

    #[test]
    fn extract_error_detail_falls_back_to_message() {
        let body = r#"{"error": {"code": 404, "message": "Not Found"}}"#;
        let detail = extract_error_detail(body);
        assert!(detail.contains("Not Found"), "got: {detail}");
    }

    #[test]
    fn extract_error_detail_returns_empty_for_invalid_json() {
        let detail = extract_error_detail("not json");
        assert!(detail.is_empty());
    }

    #[test]
    fn build_query_pairs_handles_non_string_values() {
        let mut map = serde_json::Map::new();
        map.insert("maxResults".into(), serde_json::json!(10));
        map.insert("showDeleted".into(), serde_json::json!(false));
        map.insert("calendarId".into(), serde_json::json!("primary"));
        let pairs = build_query_pairs(&map);
        assert!(pairs.iter().any(|(k, v)| k == "maxResults" && v == "10"));
        assert!(pairs
            .iter()
            .any(|(k, v)| k == "showDeleted" && v == "false"));
    }
}
