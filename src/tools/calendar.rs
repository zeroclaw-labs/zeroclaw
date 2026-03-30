//! Calendar integration tools for MoA.
//!
//! Provides the agent with read/write access to the user's calendars:
//! - **Google Calendar** (REST v3) — primary backend, covers Samsung Calendar
//! - **Microsoft Outlook** (Graph API) — enterprise/business users
//!
//! The tools expose a unified interface so the LLM can:
//! - List upcoming events
//! - Create new events / reminders
//! - Search events by keyword or date range
//!
//! Auth tokens are managed via config; first-time setup requires
//! the user to complete an OAuth flow (guided by the agent).

use async_trait::async_trait;
use serde_json::json;

use super::traits::{Tool, ToolResult};

// ═══════════════════════════════════════════════════════════════════
// Calendar List Events Tool
// ═══════════════════════════════════════════════════════════════════

/// List upcoming calendar events.
///
/// Queries the configured calendar provider (Google, Outlook) and
/// returns events in a structured format the agent can reason about.
pub struct CalendarListEventsTool {
    provider: CalendarProvider,
}

/// Supported calendar backends.
#[derive(Clone)]
pub enum CalendarProvider {
    Google {
        access_token: String,
        calendar_id: String,
    },
    Outlook {
        access_token: String,
    },
}

impl CalendarListEventsTool {
    pub fn new(provider: CalendarProvider) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Tool for CalendarListEventsTool {
    fn name(&self) -> &str {
        "calendar_list_events"
    }

    fn description(&self) -> &str {
        "List upcoming events from the user's calendar. \
         Shows event title, start/end time, location, and description. \
         Supports Google Calendar and Microsoft Outlook."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "days_ahead": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 90,
                    "default": 7,
                    "description": "Number of days ahead to fetch events (1-90)"
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50,
                    "default": 20,
                    "description": "Maximum number of events to return"
                },
                "query": {
                    "type": "string",
                    "description": "Optional text search filter for event titles/descriptions"
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let days_ahead = args
            .get("days_ahead")
            .and_then(|v| v.as_u64())
            .unwrap_or(7);
        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(20);
        let query = args.get("query").and_then(|v| v.as_str());

        let client = crate::config::build_runtime_proxy_client("tool.calendar");
        let now = chrono::Utc::now();
        let time_min = now.to_rfc3339();
        let time_max = (now + chrono::Duration::days(days_ahead as i64)).to_rfc3339();

        match &self.provider {
            CalendarProvider::Google {
                access_token,
                calendar_id,
            } => {
                let mut url = format!(
                    "https://www.googleapis.com/calendar/v3/calendars/{}/events\
                     ?timeMin={}&timeMax={}&maxResults={}&singleEvents=true&orderBy=startTime",
                    urlencoding::encode(calendar_id),
                    urlencoding::encode(&time_min),
                    urlencoding::encode(&time_max),
                    max_results
                );
                if let Some(q) = query {
                    url.push_str(&format!("&q={}", urlencoding::encode(q)));
                }

                let resp = client
                    .get(&url)
                    .bearer_auth(access_token)
                    .send()
                    .await?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Google Calendar API error {status}: {body}")),
                    });
                }

                let result: serde_json::Value = resp.json().await?;
                let events = format_google_events(&result);
                Ok(ToolResult {
                    success: true,
                    output: events,
                    error: None,
                })
            }
            CalendarProvider::Outlook { access_token } => {
                let mut url = format!(
                    "https://graph.microsoft.com/v1.0/me/calendarView\
                     ?startDateTime={}&endDateTime={}&$top={}&$orderby=start/dateTime",
                    urlencoding::encode(&time_min),
                    urlencoding::encode(&time_max),
                    max_results
                );
                if let Some(q) = query {
                    url.push_str(&format!("&$filter=contains(subject,'{}')", q.replace('\'', "''")));
                }

                let resp = client
                    .get(&url)
                    .bearer_auth(access_token)
                    .send()
                    .await?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Outlook Calendar API error {status}: {body}")),
                    });
                }

                let result: serde_json::Value = resp.json().await?;
                let events = format_outlook_events(&result);
                Ok(ToolResult {
                    success: true,
                    output: events,
                    error: None,
                })
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Calendar Create Event Tool
// ═══════════════════════════════════════════════════════════════════

/// Create a new event or reminder on the user's calendar.
pub struct CalendarCreateEventTool {
    provider: CalendarProvider,
}

impl CalendarCreateEventTool {
    pub fn new(provider: CalendarProvider) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Tool for CalendarCreateEventTool {
    fn name(&self) -> &str {
        "calendar_create_event"
    }

    fn description(&self) -> &str {
        "Create a new event or reminder on the user's calendar. \
         Supports setting title, start/end time, location, description, \
         and reminders. Works with Google Calendar and Outlook."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Event title / summary"
                },
                "start_time": {
                    "type": "string",
                    "description": "Start time in ISO 8601 format (e.g. '2026-03-30T09:00:00+09:00')"
                },
                "end_time": {
                    "type": "string",
                    "description": "End time in ISO 8601 format. If omitted, defaults to 1 hour after start."
                },
                "location": {
                    "type": "string",
                    "description": "Event location (address or virtual meeting URL)"
                },
                "description": {
                    "type": "string",
                    "description": "Event description / notes"
                },
                "reminder_minutes": {
                    "type": "integer",
                    "default": 30,
                    "description": "Reminder notification before event (in minutes)"
                },
                "all_day": {
                    "type": "boolean",
                    "default": false,
                    "description": "Create as an all-day event"
                },
                "timezone": {
                    "type": "string",
                    "description": "Timezone for the event (e.g. 'Asia/Seoul', 'America/New_York'). Defaults to user's home timezone."
                }
            },
            "required": ["title", "start_time"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let title = args
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: title"))?;
        let start_time = args
            .get("start_time")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: start_time"))?;

        let end_time = args.get("end_time").and_then(|v| v.as_str());
        let location = args.get("location").and_then(|v| v.as_str());
        let description = args.get("description").and_then(|v| v.as_str());
        let reminder_minutes = args
            .get("reminder_minutes")
            .and_then(|v| v.as_u64())
            .unwrap_or(30);
        let all_day = args
            .get("all_day")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let timezone = args
            .get("timezone")
            .and_then(|v| v.as_str())
            .unwrap_or("Asia/Seoul");

        let client = crate::config::build_runtime_proxy_client("tool.calendar");

        match &self.provider {
            CalendarProvider::Google {
                access_token,
                calendar_id,
            } => {
                let url = format!(
                    "https://www.googleapis.com/calendar/v3/calendars/{}/events",
                    urlencoding::encode(calendar_id)
                );

                let (start_json, end_json) = if all_day {
                    // All-day events use date (not dateTime)
                    let start_date = &start_time[..10]; // "2026-03-30"
                    let end_date = end_time
                        .map(|t| &t[..10])
                        .unwrap_or(start_date);
                    (
                        json!({"date": start_date, "timeZone": timezone}),
                        json!({"date": end_date, "timeZone": timezone}),
                    )
                } else {
                    let default_end = format_default_end_time(start_time);
                    let end = end_time.unwrap_or(&default_end);
                    (
                        json!({"dateTime": start_time, "timeZone": timezone}),
                        json!({"dateTime": end, "timeZone": timezone}),
                    )
                };

                let mut body = json!({
                    "summary": title,
                    "start": start_json,
                    "end": end_json,
                    "reminders": {
                        "useDefault": false,
                        "overrides": [
                            {"method": "popup", "minutes": reminder_minutes}
                        ]
                    }
                });
                if let Some(loc) = location {
                    body["location"] = json!(loc);
                }
                if let Some(desc) = description {
                    body["description"] = json!(desc);
                }

                let resp = client
                    .post(&url)
                    .bearer_auth(access_token)
                    .json(&body)
                    .send()
                    .await?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let err = resp.text().await.unwrap_or_default();
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Google Calendar create error {status}: {err}")),
                    });
                }

                let result: serde_json::Value = resp.json().await?;
                let event_id = result.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let html_link = result
                    .get("htmlLink")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                Ok(ToolResult {
                    success: true,
                    output: format!(
                        "Event created successfully.\n\
                         Title: {title}\n\
                         Start: {start_time}\n\
                         Event ID: {event_id}\n\
                         Link: {html_link}"
                    ),
                    error: None,
                })
            }
            CalendarProvider::Outlook { access_token } => {
                let url = "https://graph.microsoft.com/v1.0/me/events";

                let (start_json, end_json) = if all_day {
                    (
                        json!({"dateTime": start_time, "timeZone": timezone}),
                        json!({
                            "dateTime": end_time.unwrap_or(start_time),
                            "timeZone": timezone
                        }),
                    )
                } else {
                    let default_end = format_default_end_time(start_time);
                    let end = end_time.unwrap_or(&default_end);
                    (
                        json!({"dateTime": start_time, "timeZone": timezone}),
                        json!({"dateTime": end, "timeZone": timezone}),
                    )
                };

                let mut body = json!({
                    "subject": title,
                    "start": start_json,
                    "end": end_json,
                    "isAllDay": all_day,
                    "isReminderOn": true,
                    "reminderMinutesBeforeStart": reminder_minutes
                });
                if let Some(loc) = location {
                    body["location"] = json!({"displayName": loc});
                }
                if let Some(desc) = description {
                    body["body"] = json!({"contentType": "Text", "content": desc});
                }

                let resp = client
                    .post(url)
                    .bearer_auth(access_token)
                    .json(&body)
                    .send()
                    .await?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let err = resp.text().await.unwrap_or_default();
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Outlook Calendar create error {status}: {err}")),
                    });
                }

                let result: serde_json::Value = resp.json().await?;
                let event_id = result.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let web_link = result
                    .get("webLink")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                Ok(ToolResult {
                    success: true,
                    output: format!(
                        "Outlook event created successfully.\n\
                         Title: {title}\n\
                         Start: {start_time}\n\
                         Event ID: {event_id}\n\
                         Link: {web_link}"
                    ),
                    error: None,
                })
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════

fn format_google_events(data: &serde_json::Value) -> String {
    let items = match data.get("items").and_then(|i| i.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return "No upcoming events found.".to_string(),
    };

    let mut output = format!("Found {} upcoming event(s):\n\n", items.len());
    for (i, event) in items.iter().enumerate() {
        let title = event
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("(No title)");
        let start = event
            .get("start")
            .and_then(|s| {
                s.get("dateTime")
                    .or_else(|| s.get("date"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("?");
        let end = event
            .get("end")
            .and_then(|s| {
                s.get("dateTime")
                    .or_else(|| s.get("date"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("?");
        let location = event
            .get("location")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        output.push_str(&format!("{}. {} ({}~{})", i + 1, title, start, end));
        if !location.is_empty() {
            output.push_str(&format!(" @ {location}"));
        }
        output.push('\n');
    }
    output
}

fn format_outlook_events(data: &serde_json::Value) -> String {
    let items = match data.get("value").and_then(|v| v.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return "No upcoming events found.".to_string(),
    };

    let mut output = format!("Found {} upcoming event(s):\n\n", items.len());
    for (i, event) in items.iter().enumerate() {
        let title = event
            .get("subject")
            .and_then(|v| v.as_str())
            .unwrap_or("(No title)");
        let start = event
            .pointer("/start/dateTime")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let end = event
            .pointer("/end/dateTime")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let location = event
            .pointer("/location/displayName")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        output.push_str(&format!("{}. {} ({}~{})", i + 1, title, start, end));
        if !location.is_empty() {
            output.push_str(&format!(" @ {location}"));
        }
        output.push('\n');
    }
    output
}

/// Given a start time string, produce a default end time 1 hour later.
/// Falls back to appending "+01:00" if parsing fails.
fn format_default_end_time(start: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(start) {
        (dt + chrono::Duration::hours(1)).to_rfc3339()
    } else {
        // Best-effort: just return the same time (API will likely accept it)
        start.to_string()
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_list_events_tool_spec() {
        let tool = CalendarListEventsTool::new(CalendarProvider::Google {
            access_token: "test".into(),
            calendar_id: "primary".into(),
        });
        let spec = tool.spec();
        assert_eq!(spec.name, "calendar_list_events");
        assert!(spec.description.contains("calendar"));
    }

    #[test]
    fn calendar_create_event_tool_spec() {
        let tool = CalendarCreateEventTool::new(CalendarProvider::Google {
            access_token: "test".into(),
            calendar_id: "primary".into(),
        });
        let spec = tool.spec();
        assert_eq!(spec.name, "calendar_create_event");
        assert!(spec.description.contains("event"));

        let params = spec.parameters;
        let props = params.get("properties").unwrap();
        assert!(props.get("title").is_some());
        assert!(props.get("start_time").is_some());
        assert!(props.get("reminder_minutes").is_some());
        assert!(props.get("timezone").is_some());
    }

    #[test]
    fn format_google_events_empty() {
        let data = json!({"items": []});
        assert_eq!(format_google_events(&data), "No upcoming events found.");
    }

    #[test]
    fn format_google_events_with_data() {
        let data = json!({
            "items": [{
                "summary": "Team Meeting",
                "start": {"dateTime": "2026-03-30T10:00:00+09:00"},
                "end": {"dateTime": "2026-03-30T11:00:00+09:00"},
                "location": "Room A"
            }]
        });
        let output = format_google_events(&data);
        assert!(output.contains("Team Meeting"));
        assert!(output.contains("Room A"));
    }

    #[test]
    fn format_outlook_events_empty() {
        let data = json!({"value": []});
        assert_eq!(format_outlook_events(&data), "No upcoming events found.");
    }

    #[test]
    fn format_outlook_events_with_data() {
        let data = json!({
            "value": [{
                "subject": "Standup",
                "start": {"dateTime": "2026-03-30T09:00:00"},
                "end": {"dateTime": "2026-03-30T09:30:00"},
                "location": {"displayName": "Online"}
            }]
        });
        let output = format_outlook_events(&data);
        assert!(output.contains("Standup"));
        assert!(output.contains("Online"));
    }

    #[test]
    fn default_end_time_rfc3339() {
        let start = "2026-03-30T10:00:00+09:00";
        let end = format_default_end_time(start);
        assert!(end.contains("11:00:00"));
    }

    #[test]
    fn default_end_time_fallback() {
        let start = "not-a-date";
        let end = format_default_end_time(start);
        assert_eq!(end, "not-a-date");
    }

    #[tokio::test]
    async fn calendar_create_missing_title_fails() {
        let tool = CalendarCreateEventTool::new(CalendarProvider::Google {
            access_token: "test".into(),
            calendar_id: "primary".into(),
        });
        let result = tool.execute(json!({"start_time": "2026-03-30T10:00:00+09:00"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn calendar_create_missing_start_fails() {
        let tool = CalendarCreateEventTool::new(CalendarProvider::Google {
            access_token: "test".into(),
            calendar_id: "primary".into(),
        });
        let result = tool.execute(json!({"title": "Test"})).await;
        assert!(result.is_err());
    }
}
