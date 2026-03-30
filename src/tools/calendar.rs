//! Calendar integration tools for MoA.
//!
//! Provides the agent with read/write access to the user's calendars:
//! - **Google Calendar** (REST v3) — primary backend, covers Samsung Calendar
//! - **Microsoft Outlook** (Graph API) — enterprise/business users
//! - **KakaoTalk 톡캘린더** (Kakao REST API) — Korean users
//!
//! The tools expose a unified interface so the LLM can:
//! - List upcoming events
//! - Create new events / reminders
//! - Search events by keyword or date range

use async_trait::async_trait;
use serde_json::json;

use super::traits::{Tool, ToolResult};

// ═══════════════════════════════════════════════════════════════════
// Calendar Provider Enum
// ═══════════════════════════════════════════════════════════════════

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
    Kakao {
        access_token: String,
        calendar_id: Option<String>,
    },
}

// ═══════════════════════════════════════════════════════════════════
// Calendar List Events Tool
// ═══════════════════════════════════════════════════════════════════

/// List upcoming calendar events.
pub struct CalendarListEventsTool {
    provider: CalendarProvider,
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
         Supports Google Calendar, Microsoft Outlook, and KakaoTalk 톡캘린더."
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

                let resp = client.get(&url).bearer_auth(access_token).send().await?;
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
                Ok(ToolResult {
                    success: true,
                    output: format_google_events(&result),
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
                    url.push_str(&format!(
                        "&$filter=contains(subject,'{}')",
                        q.replace('\'', "''")
                    ));
                }

                let resp = client.get(&url).bearer_auth(access_token).send().await?;
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
                Ok(ToolResult {
                    success: true,
                    output: format_outlook_events(&result),
                    error: None,
                })
            }
            CalendarProvider::Kakao {
                access_token,
                calendar_id,
            } => {
                // Kakao 톡캘린더 REST API: GET /v2/api/calendar/events
                let from = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
                let to = (now + chrono::Duration::days(days_ahead as i64))
                    .format("%Y-%m-%dT%H:%M:%SZ")
                    .to_string();

                let mut url = format!(
                    "https://kapi.kakao.com/v2/api/calendar/events?from={}&to={}&limit={}",
                    urlencoding::encode(&from),
                    urlencoding::encode(&to),
                    max_results
                );
                if let Some(cal_id) = calendar_id {
                    url.push_str(&format!("&calendar_id={}", urlencoding::encode(cal_id)));
                }

                let resp = client
                    .get(&url)
                    .header("Authorization", format!("Bearer {access_token}"))
                    .send()
                    .await?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("KakaoTalk Calendar API error {status}: {body}")),
                    });
                }

                let result: serde_json::Value = resp.json().await?;
                Ok(ToolResult {
                    success: true,
                    output: format_kakao_events(&result),
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
         and reminders. Works with Google Calendar, Outlook, and KakaoTalk 톡캘린더."
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
                    "description": "Timezone (e.g. 'Asia/Seoul'). Defaults to user's home timezone."
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

                let default_end = format_default_end_time(start_time);
                let (start_json, end_json) = if all_day {
                    let sd = &start_time[..10.min(start_time.len())];
                    let ed = end_time.map(|t| &t[..10.min(t.len())]).unwrap_or(sd);
                    (json!({"date": sd, "timeZone": timezone}), json!({"date": ed, "timeZone": timezone}))
                } else {
                    let end = end_time.unwrap_or(&default_end);
                    (json!({"dateTime": start_time, "timeZone": timezone}), json!({"dateTime": end, "timeZone": timezone}))
                };

                let mut body = json!({
                    "summary": title,
                    "start": start_json,
                    "end": end_json,
                    "reminders": {
                        "useDefault": false,
                        "overrides": [{"method": "popup", "minutes": reminder_minutes}]
                    }
                });
                if let Some(loc) = location { body["location"] = json!(loc); }
                if let Some(desc) = description { body["description"] = json!(desc); }

                let resp = client.post(&url).bearer_auth(access_token).json(&body).send().await?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let err = resp.text().await.unwrap_or_default();
                    return Ok(ToolResult { success: false, output: String::new(), error: Some(format!("Google Calendar create error {status}: {err}")) });
                }
                let result: serde_json::Value = resp.json().await?;
                let event_id = result.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let html_link = result.get("htmlLink").and_then(|v| v.as_str()).unwrap_or("");
                Ok(ToolResult {
                    success: true,
                    output: format!("Event created.\nTitle: {title}\nStart: {start_time}\nID: {event_id}\nLink: {html_link}"),
                    error: None,
                })
            }
            CalendarProvider::Outlook { access_token } => {
                let default_end = format_default_end_time(start_time);
                let end = end_time.unwrap_or(&default_end);
                let mut body = json!({
                    "subject": title,
                    "start": {"dateTime": start_time, "timeZone": timezone},
                    "end": {"dateTime": end, "timeZone": timezone},
                    "isAllDay": all_day,
                    "isReminderOn": true,
                    "reminderMinutesBeforeStart": reminder_minutes
                });
                if let Some(loc) = location { body["location"] = json!({"displayName": loc}); }
                if let Some(desc) = description { body["body"] = json!({"contentType": "Text", "content": desc}); }

                let resp = client.post("https://graph.microsoft.com/v1.0/me/events")
                    .bearer_auth(access_token).json(&body).send().await?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let err = resp.text().await.unwrap_or_default();
                    return Ok(ToolResult { success: false, output: String::new(), error: Some(format!("Outlook create error {status}: {err}")) });
                }
                let result: serde_json::Value = resp.json().await?;
                let event_id = result.get("id").and_then(|v| v.as_str()).unwrap_or("");
                Ok(ToolResult {
                    success: true,
                    output: format!("Outlook event created.\nTitle: {title}\nStart: {start_time}\nID: {event_id}"),
                    error: None,
                })
            }
            CalendarProvider::Kakao {
                access_token,
                calendar_id,
            } => {
                // Kakao 톡캘린더 REST API: POST /v2/api/calendar/create/event
                let default_end = format_default_end_time(start_time);
                let end = end_time.unwrap_or(&default_end);

                let mut event = json!({
                    "title": title,
                    "time": {
                        "start_at": start_time,
                        "end_at": end,
                        "time_zone": timezone,
                        "all_day": all_day
                    }
                });
                if let Some(loc) = location {
                    event["location"] = json!({"name": loc});
                }
                if let Some(desc) = description {
                    event["description"] = json!(desc);
                }
                if reminder_minutes > 0 {
                    event["reminders"] = json!([reminder_minutes as i64]);
                }

                let mut body = json!({"event": event});
                if let Some(cal_id) = calendar_id {
                    body["calendar_id"] = json!(cal_id);
                }

                let resp = client
                    .post("https://kapi.kakao.com/v2/api/calendar/create/event")
                    .header("Authorization", format!("Bearer {access_token}"))
                    .json(&body)
                    .send()
                    .await?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let err = resp.text().await.unwrap_or_default();
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("KakaoTalk Calendar create error {status}: {err}")),
                    });
                }

                let result: serde_json::Value = resp.json().await?;
                let event_id = result
                    .get("event_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                Ok(ToolResult {
                    success: true,
                    output: format!(
                        "톡캘린더 일정이 생성되었습니다.\nTitle: {title}\nStart: {start_time}\nEvent ID: {event_id}"
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
    let mut out = format!("Found {} upcoming event(s):\n\n", items.len());
    for (i, ev) in items.iter().enumerate() {
        let title = ev.get("summary").and_then(|v| v.as_str()).unwrap_or("(No title)");
        let start = ev.get("start").and_then(|s| s.get("dateTime").or_else(|| s.get("date")).and_then(|v| v.as_str())).unwrap_or("?");
        let end = ev.get("end").and_then(|s| s.get("dateTime").or_else(|| s.get("date")).and_then(|v| v.as_str())).unwrap_or("?");
        let loc = ev.get("location").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!("{}. {} ({}~{})", i + 1, title, start, end));
        if !loc.is_empty() { out.push_str(&format!(" @ {loc}")); }
        out.push('\n');
    }
    out
}

fn format_outlook_events(data: &serde_json::Value) -> String {
    let items = match data.get("value").and_then(|v| v.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return "No upcoming events found.".to_string(),
    };
    let mut out = format!("Found {} upcoming event(s):\n\n", items.len());
    for (i, ev) in items.iter().enumerate() {
        let title = ev.get("subject").and_then(|v| v.as_str()).unwrap_or("(No title)");
        let start = ev.pointer("/start/dateTime").and_then(|v| v.as_str()).unwrap_or("?");
        let end = ev.pointer("/end/dateTime").and_then(|v| v.as_str()).unwrap_or("?");
        let loc = ev.pointer("/location/displayName").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!("{}. {} ({}~{})", i + 1, title, start, end));
        if !loc.is_empty() { out.push_str(&format!(" @ {loc}")); }
        out.push('\n');
    }
    out
}

fn format_kakao_events(data: &serde_json::Value) -> String {
    let events = match data.get("events").and_then(|v| v.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return "톡캘린더에 예정된 일정이 없습니다. (No upcoming events)".to_string(),
    };
    let mut out = format!("톡캘린더: {} 건의 일정\n\n", events.len());
    for (i, ev) in events.iter().enumerate() {
        let title = ev.get("title").and_then(|v| v.as_str()).unwrap_or("(제목 없음)");
        let start = ev.pointer("/time/start_at").and_then(|v| v.as_str()).unwrap_or("?");
        let end = ev.pointer("/time/end_at").and_then(|v| v.as_str()).unwrap_or("?");
        let loc = ev.pointer("/location/name").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!("{}. {} ({}~{})", i + 1, title, start, end));
        if !loc.is_empty() { out.push_str(&format!(" @ {loc}")); }
        out.push('\n');
    }
    out
}

fn format_default_end_time(start: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(start) {
        (dt + chrono::Duration::hours(1)).to_rfc3339()
    } else {
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
        assert_eq!(tool.spec().name, "calendar_list_events");
    }

    #[test]
    fn calendar_create_event_tool_spec() {
        let tool = CalendarCreateEventTool::new(CalendarProvider::Google {
            access_token: "test".into(),
            calendar_id: "primary".into(),
        });
        let spec = tool.spec();
        assert_eq!(spec.name, "calendar_create_event");
        let props = spec.parameters.get("properties").unwrap();
        assert!(props.get("title").is_some());
        assert!(props.get("start_time").is_some());
        assert!(props.get("timezone").is_some());
    }

    #[test]
    fn kakao_calendar_provider_spec() {
        let tool = CalendarListEventsTool::new(CalendarProvider::Kakao {
            access_token: "test".into(),
            calendar_id: None,
        });
        assert_eq!(tool.spec().name, "calendar_list_events");
        assert!(tool.spec().description.contains("톡캘린더"));
    }

    #[test]
    fn format_google_events_empty() {
        assert_eq!(format_google_events(&json!({"items": []})), "No upcoming events found.");
    }

    #[test]
    fn format_google_events_with_data() {
        let data = json!({"items": [{"summary": "Meeting", "start": {"dateTime": "2026-03-30T10:00:00+09:00"}, "end": {"dateTime": "2026-03-30T11:00:00+09:00"}, "location": "Room A"}]});
        let out = format_google_events(&data);
        assert!(out.contains("Meeting"));
        assert!(out.contains("Room A"));
    }

    #[test]
    fn format_outlook_events_empty() {
        assert_eq!(format_outlook_events(&json!({"value": []})), "No upcoming events found.");
    }

    #[test]
    fn format_kakao_events_empty() {
        let out = format_kakao_events(&json!({"events": []}));
        assert!(out.contains("없습니다"));
    }

    #[test]
    fn format_kakao_events_with_data() {
        let data = json!({"events": [{"title": "점심 약속", "time": {"start_at": "2026-03-30T12:00:00+09:00", "end_at": "2026-03-30T13:00:00+09:00"}, "location": {"name": "강남역"}}]});
        let out = format_kakao_events(&data);
        assert!(out.contains("점심 약속"));
        assert!(out.contains("강남역"));
    }

    #[test]
    fn default_end_time_rfc3339() {
        let end = format_default_end_time("2026-03-30T10:00:00+09:00");
        assert!(end.contains("11:00:00"));
    }

    #[test]
    fn default_end_time_fallback() {
        assert_eq!(format_default_end_time("not-a-date"), "not-a-date");
    }

    #[tokio::test]
    async fn calendar_create_missing_title_fails() {
        let tool = CalendarCreateEventTool::new(CalendarProvider::Google {
            access_token: "t".into(), calendar_id: "primary".into(),
        });
        assert!(tool.execute(json!({"start_time": "2026-03-30T10:00:00+09:00"})).await.is_err());
    }

    #[tokio::test]
    async fn calendar_create_missing_start_fails() {
        let tool = CalendarCreateEventTool::new(CalendarProvider::Kakao {
            access_token: "t".into(), calendar_id: None,
        });
        assert!(tool.execute(json!({"title": "Test"})).await.is_err());
    }
}
