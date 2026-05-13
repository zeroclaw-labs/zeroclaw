use anyhow::Context;

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// Build the user path segment: `/me` or `/users/{user_id}`.
/// The user_id is percent-encoded to prevent path-traversal attacks.
fn user_path(user_id: &str) -> String {
    if user_id == "me" {
        "/me".to_string()
    } else {
        format!("/users/{}", urlencoding::encode(user_id))
    }
}

/// Percent-encode a single path segment to prevent path-traversal attacks.
fn encode_path_segment(segment: &str) -> String {
    urlencoding::encode(segment).into_owned()
}

/// List mail messages for a user.
pub async fn mail_list(
    client: &reqwest::Client,
    token: &str,
    user_id: &str,
    folder: Option<&str>,
    top: u32,
) -> anyhow::Result<serde_json::Value> {
    let base = user_path(user_id);
    let path = match folder {
        Some(f) => format!(
            "{GRAPH_BASE}{base}/mailFolders/{}/messages",
            encode_path_segment(f)
        ),
        None => format!("{GRAPH_BASE}{base}/messages"),
    };

    let resp = client
        .get(&path)
        .bearer_auth(token)
        .query(&[("$top", top.to_string())])
        .send()
        .await
        .context("ms365: mail_list request failed")?;

    handle_json_response(resp, "mail_list").await
}

/// Send a mail message.
pub async fn mail_send(
    client: &reqwest::Client,
    token: &str,
    user_id: &str,
    to: &[String],
    subject: &str,
    body: &str,
) -> anyhow::Result<()> {
    let base = user_path(user_id);
    let url = format!("{GRAPH_BASE}{base}/sendMail");

    let to_recipients: Vec<serde_json::Value> = to
        .iter()
        .map(|addr| {
            serde_json::json!({
                "emailAddress": { "address": addr }
            })
        })
        .collect();

    let payload = serde_json::json!({
        "message": {
            "subject": subject,
            "body": {
                "contentType": "Text",
                "content": body
            },
            "toRecipients": to_recipients
        }
    });

    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(&payload)
        .send()
        .await
        .context("ms365: mail_send request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let code = extract_graph_error_code(&body).unwrap_or_else(|| "unknown".to_string());
        tracing::debug!("ms365: mail_send raw error body: {body}");
        anyhow::bail!("ms365: mail_send failed ({status}, code={code})");
    }

    Ok(())
}

/// List messages in a Teams channel.
pub async fn teams_message_list(
    client: &reqwest::Client,
    token: &str,
    team_id: &str,
    channel_id: &str,
    top: u32,
) -> anyhow::Result<serde_json::Value> {
    let url = format!(
        "{GRAPH_BASE}/teams/{}/channels/{}/messages",
        encode_path_segment(team_id),
        encode_path_segment(channel_id)
    );

    let resp = client
        .get(&url)
        .bearer_auth(token)
        .query(&[("$top", top.to_string())])
        .send()
        .await
        .context("ms365: teams_message_list request failed")?;

    handle_json_response(resp, "teams_message_list").await
}

/// Send a message to a Teams channel.
pub async fn teams_message_send(
    client: &reqwest::Client,
    token: &str,
    team_id: &str,
    channel_id: &str,
    body: &str,
) -> anyhow::Result<()> {
    let url = format!(
        "{GRAPH_BASE}/teams/{}/channels/{}/messages",
        encode_path_segment(team_id),
        encode_path_segment(channel_id)
    );

    let payload = serde_json::json!({
        "body": {
            "content": body
        }
    });

    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(&payload)
        .send()
        .await
        .context("ms365: teams_message_send request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let code = extract_graph_error_code(&body).unwrap_or_else(|| "unknown".to_string());
        tracing::debug!("ms365: teams_message_send raw error body: {body}");
        anyhow::bail!("ms365: teams_message_send failed ({status}, code={code})");
    }

    Ok(())
}

/// List calendar events in a date range.
pub async fn calendar_events_list(
    client: &reqwest::Client,
    token: &str,
    user_id: &str,
    start: &str,
    end: &str,
    top: u32,
) -> anyhow::Result<serde_json::Value> {
    let base = user_path(user_id);
    let url = format!("{GRAPH_BASE}{base}/calendarView");

    let resp = client
        .get(&url)
        .bearer_auth(token)
        .query(&[
            ("startDateTime", start.to_string()),
            ("endDateTime", end.to_string()),
            ("$top", top.to_string()),
        ])
        .send()
        .await
        .context("ms365: calendar_events_list request failed")?;

    handle_json_response(resp, "calendar_events_list").await
}

/// Create a calendar event.
pub async fn calendar_event_create(
    client: &reqwest::Client,
    token: &str,
    user_id: &str,
    subject: &str,
    start: &str,
    end: &str,
    attendees: &[String],
    body_text: Option<&str>,
) -> anyhow::Result<String> {
    let base = user_path(user_id);
    let url = format!("{GRAPH_BASE}{base}/events");

    let attendee_list: Vec<serde_json::Value> = attendees
        .iter()
        .map(|email| {
            serde_json::json!({
                "emailAddress": { "address": email },
                "type": "required"
            })
        })
        .collect();

    let mut payload = serde_json::json!({
        "subject": subject,
        "start": {
            "dateTime": start,
            "timeZone": "UTC"
        },
        "end": {
            "dateTime": end,
            "timeZone": "UTC"
        },
        "attendees": attendee_list
    });

    if let Some(text) = body_text {
        payload["body"] = serde_json::json!({
            "contentType": "Text",
            "content": text
        });
    }

    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(&payload)
        .send()
        .await
        .context("ms365: calendar_event_create request failed")?;

    let value = handle_json_response(resp, "calendar_event_create").await?;
    let event_id = value["id"].as_str().unwrap_or("unknown").to_string();
    Ok(event_id)
}

/// Delete a calendar event by ID.
pub async fn calendar_event_delete(
    client: &reqwest::Client,
    token: &str,
    user_id: &str,
    event_id: &str,
) -> anyhow::Result<()> {
    let base = user_path(user_id);
    let url = format!(
        "{GRAPH_BASE}{base}/events/{}",
        encode_path_segment(event_id)
    );

    let resp = client
        .delete(&url)
        .bearer_auth(token)
        .send()
        .await
        .context("ms365: calendar_event_delete request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let code = extract_graph_error_code(&body).unwrap_or_else(|| "unknown".to_string());
        tracing::debug!("ms365: calendar_event_delete raw error body: {body}");
        anyhow::bail!("ms365: calendar_event_delete failed ({status}, code={code})");
    }

    Ok(())
}

/// List children of a OneDrive folder.
pub async fn onedrive_list(
    client: &reqwest::Client,
    token: &str,
    user_id: &str,
    path: Option<&str>,
) -> anyhow::Result<serde_json::Value> {
    let base = user_path(user_id);
    let url = match path {
        Some(p) if !p.is_empty() => {
            let encoded = urlencoding::encode(p);
            format!("{GRAPH_BASE}{base}/drive/root:/{encoded}:/children")
        }
        _ => format!("{GRAPH_BASE}{base}/drive/root/children"),
    };

    let resp = client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .context("ms365: onedrive_list request failed")?;

    handle_json_response(resp, "onedrive_list").await
}

/// Download a OneDrive item by ID, with a maximum size guard.
pub async fn onedrive_download(
    client: &reqwest::Client,
    token: &str,
    user_id: &str,
    item_id: &str,
    max_size: usize,
) -> anyhow::Result<Vec<u8>> {
    let base = user_path(user_id);
    let url = format!(
        "{GRAPH_BASE}{base}/drive/items/{}/content",
        encode_path_segment(item_id)
    );

    let resp = client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .context("ms365: onedrive_download request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let code = extract_graph_error_code(&body).unwrap_or_else(|| "unknown".to_string());
        tracing::debug!("ms365: onedrive_download raw error body: {body}");
        anyhow::bail!("ms365: onedrive_download failed ({status}, code={code})");
    }

    let bytes = resp
        .bytes()
        .await
        .context("ms365: failed to read download body")?;
    if bytes.len() > max_size {
        anyhow::bail!(
            "ms365: downloaded file exceeds max_size ({} > {max_size})",
            bytes.len()
        );
    }

    Ok(bytes.to_vec())
}

/// Search SharePoint for documents matching a query.
pub async fn sharepoint_search(
    client: &reqwest::Client,
    token: &str,
    query: &str,
    top: u32,
) -> anyhow::Result<serde_json::Value> {
    let url = format!("{GRAPH_BASE}/search/query");

    let payload = serde_json::json!({
        "requests": [{
            "entityTypes": ["driveItem", "listItem", "site"],
            "query": {
                "queryString": query
            },
            "from": 0,
            "size": top
        }]
    });

    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(&payload)
        .send()
        .await
        .context("ms365: sharepoint_search request failed")?;

    handle_json_response(resp, "sharepoint_search").await
}

/// Extract a short, safe error code from a Graph API JSON error body.
/// Returns `None` when the body is not a recognised Graph error envelope.
fn extract_graph_error_code(body: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    parsed
        .get("error")
        .and_then(|e| e.get("code"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
}

/// Parse a JSON response body, returning an error on non-success status.
/// Raw Graph API error bodies are not propagated; only the HTTP status and a
/// short error code (when available) are surfaced to avoid leaking internal
/// API details.
async fn handle_json_response(
    resp: reqwest::Response,
    operation: &str,
) -> anyhow::Result<serde_json::Value> {
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let code = extract_graph_error_code(&body).unwrap_or_else(|| "unknown".to_string());
        tracing::debug!("ms365: {operation} raw error body: {body}");
        anyhow::bail!("ms365: {operation} failed ({status}, code={code})");
    }

    resp.json()
        .await
        .with_context(|| format!("ms365: failed to parse {operation} response"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_path_me() {
        assert_eq!(user_path("me"), "/me");
    }

    #[test]
    fn user_path_specific_user() {
        assert_eq!(user_path("user@contoso.com"), "/users/user%40contoso.com");
    }

    #[test]
    fn mail_list_url_no_folder() {
        let base = user_path("me");
        let url = format!("{GRAPH_BASE}{base}/messages");
        assert_eq!(url, "https://graph.microsoft.com/v1.0/me/messages");
    }

    #[test]
    fn mail_list_url_with_folder() {
        let base = user_path("me");
        let folder = "inbox";
        let url = format!(
            "{GRAPH_BASE}{base}/mailFolders/{}/messages",
            encode_path_segment(folder)
        );
        assert_eq!(
            url,
            "https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages"
        );
    }

    #[test]
    fn calendar_view_url() {
        let base = user_path("user@example.com");
        let url = format!("{GRAPH_BASE}{base}/calendarView");
        assert_eq!(
            url,
            "https://graph.microsoft.com/v1.0/users/user%40example.com/calendarView"
        );
    }

    #[test]
    fn teams_message_url() {
        let url = format!(
            "{GRAPH_BASE}/teams/{}/channels/{}/messages",
            encode_path_segment("team-123"),
            encode_path_segment("channel-456")
        );
        assert_eq!(
            url,
            "https://graph.microsoft.com/v1.0/teams/team-123/channels/channel-456/messages"
        );
    }

    #[test]
    fn onedrive_root_url() {
        let base = user_path("me");
        let url = format!("{GRAPH_BASE}{base}/drive/root/children");
        assert_eq!(
            url,
            "https://graph.microsoft.com/v1.0/me/drive/root/children"
        );
    }

    #[test]
    fn onedrive_path_url() {
        let base = user_path("me");
        let encoded = urlencoding::encode("Documents/Reports");
        let url = format!("{GRAPH_BASE}{base}/drive/root:/{encoded}:/children");
        assert_eq!(
            url,
            "https://graph.microsoft.com/v1.0/me/drive/root:/Documents%2FReports:/children"
        );
    }

    #[test]
    fn sharepoint_search_url() {
        let url = format!("{GRAPH_BASE}/search/query");
        assert_eq!(url, "https://graph.microsoft.com/v1.0/search/query");
    }
}
