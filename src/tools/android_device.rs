use super::traits::{Tool, ToolResult};
use crate::config::{AndroidConfig, AndroidDistribution};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::json;
use std::time::{Duration, Instant};
use std::{net::IpAddr, sync::Arc};

const RATE_LIMIT_WINDOW_SECS: u64 = 3600;

pub struct AndroidDeviceTool {
    security: Arc<SecurityPolicy>,
    config: AndroidConfig,
    sms_events: Mutex<Vec<Instant>>,
    call_events: Mutex<Vec<Instant>>,
}

impl AndroidDeviceTool {
    pub fn new(security: Arc<SecurityPolicy>, config: AndroidConfig) -> Self {
        Self {
            security,
            config,
            sms_events: Mutex::new(Vec::new()),
            call_events: Mutex::new(Vec::new()),
        }
    }

    fn bridge_mode(&self) -> &str {
        self.config.bridge.mode.trim()
    }

    fn validate_bridge_endpoint(&self) -> anyhow::Result<&str> {
        let endpoint = self.config.bridge.endpoint.trim();
        if endpoint.is_empty() {
            anyhow::bail!("android.bridge.endpoint cannot be empty");
        }

        if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
            anyhow::bail!("android.bridge.endpoint must start with http:// or https://");
        }

        if !self.config.bridge.allow_remote_endpoint {
            let host = extract_host(endpoint)?;
            if !is_local_host(&host) {
                anyhow::bail!(
                    "android.bridge.allow_remote_endpoint=false blocks non-local endpoint: {host}"
                );
            }
        }

        Ok(endpoint)
    }

    fn ensure_capability_enabled(&self, action: &str) -> anyhow::Result<()> {
        match action {
            "launch_app" | "list_apps" | "open_url" | "open_settings" => {
                if !self.config.capabilities.app_launch {
                    anyhow::bail!("android capability app_launch is disabled");
                }
            }
            "sensor_read" | "vibrate" | "get_device_info" | "get_android_version" => {
                if !self.config.capabilities.sensors {
                    anyhow::bail!("android capability sensors is disabled");
                }
            }
            "take_photo" => {
                if !self.config.capabilities.camera {
                    anyhow::bail!("android capability camera is disabled");
                }
            }
            "record_audio" => {
                if !self.config.capabilities.microphone {
                    anyhow::bail!("android capability microphone is disabled");
                }
            }
            "get_location" => {
                if !self.config.capabilities.location {
                    anyhow::bail!("android capability location is disabled");
                }
            }
            "post_notification" => {
                if !self.config.capabilities.notifications {
                    anyhow::bail!("android capability notifications is disabled");
                }
            }
            "set_clipboard" | "read_clipboard" => {
                if !self.config.capabilities.clipboard {
                    anyhow::bail!("android capability clipboard is disabled");
                }
            }
            "get_network" => {
                if !self.config.capabilities.network {
                    anyhow::bail!("android capability network is disabled");
                }
            }
            "get_battery" => {
                if !self.config.capabilities.battery {
                    anyhow::bail!("android capability battery is disabled");
                }
            }
            "send_sms" | "read_sms" => {
                if !self.config.capabilities.sms {
                    anyhow::bail!("android capability sms is disabled");
                }
                if self.config.distribution == AndroidDistribution::Play {
                    anyhow::bail!(
                        "sms actions are disabled in play distribution. Use enterprise/full distribution."
                    );
                }
            }
            "place_call" | "read_call_log" => {
                if !self.config.capabilities.calls {
                    anyhow::bail!("android capability calls is disabled");
                }
                if self.config.distribution == AndroidDistribution::Play {
                    anyhow::bail!(
                        "call actions are disabled in play distribution. Use enterprise/full distribution."
                    );
                }
            }
            "read_contacts" => {
                if !self.config.capabilities.contacts {
                    anyhow::bail!("android capability contacts is disabled");
                }
                if self.config.distribution == AndroidDistribution::Play {
                    anyhow::bail!(
                        "contact actions are disabled in play distribution. Use enterprise/full distribution."
                    );
                }
            }
            "read_calendar" => {
                if !self.config.capabilities.calendar {
                    anyhow::bail!("android capability calendar is disabled");
                }
            }
            other => anyhow::bail!(
                "Unknown android action '{other}'. Supported: launch_app, list_apps, open_url, open_settings, sensor_read, vibrate, get_location, take_photo, record_audio, set_clipboard, read_clipboard, post_notification, get_network, get_battery, get_device_info, get_android_version, read_contacts, read_calendar, send_sms, read_sms, place_call, read_call_log"
            ),
        }

        Ok(())
    }

    fn ensure_package_allowed(&self, package: &str) -> anyhow::Result<()> {
        let allowed = &self.config.policy.allowed_packages;
        if allowed.is_empty() {
            return Ok(());
        }

        if allowed.iter().any(|p| p == package) {
            Ok(())
        } else {
            anyhow::bail!("Package '{package}' is not in android.policy.allowed_packages")
        }
    }

    fn ensure_phone_allowed(&self, number: &str) -> anyhow::Result<()> {
        let normalized = normalize_phone(number);
        let allowed = &self.config.policy.allowed_phone_numbers;
        if allowed.is_empty() {
            return Ok(());
        }

        if allowed
            .iter()
            .any(|v| normalize_phone(v) == normalized.as_str())
        {
            Ok(())
        } else {
            anyhow::bail!("Phone number is not allowlisted in android.policy.allowed_phone_numbers")
        }
    }

    fn enforce_action_budget(
        events: &Mutex<Vec<Instant>>,
        limit: u32,
        action_name: &str,
    ) -> anyhow::Result<()> {
        let now = Instant::now();
        let cutoff = now
            .checked_sub(Duration::from_secs(RATE_LIMIT_WINDOW_SECS))
            .unwrap_or_else(Instant::now);
        let mut tracker = events.lock();
        tracker.retain(|ts| *ts >= cutoff);
        if tracker.len() >= limit as usize {
            anyhow::bail!("{action_name} blocked: hourly limit reached ({limit})");
        }
        tracker.push(now);
        Ok(())
    }

    fn ensure_approved_if_required(&self, approved: bool, action: &str) -> anyhow::Result<()> {
        let needs_approval = self.config.policy.require_explicit_approval
            && matches!(action, "send_sms" | "place_call");

        if needs_approval && !approved {
            anyhow::bail!(
                "{action} requires explicit approval. Retry with approved=true after user confirmation"
            );
        }

        Ok(())
    }

    async fn execute_bridge_call(
        &self,
        action: &str,
        payload: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        match self.bridge_mode() {
            "mock" => Ok(json!({
                "ok": true,
                "action": action,
                "mode": "mock",
                "result": payload
            })),
            "http" => {
                let endpoint = self.validate_bridge_endpoint()?;
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_millis(self.config.bridge.timeout_ms))
                    .build()?;

                let mut request = client.post(endpoint).json(&json!({
                    "action": action,
                    "payload": payload,
                }));

                if let Some(api_key) = self
                    .config
                    .bridge
                    .api_key
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    request = request.bearer_auth(api_key);
                }

                let response = request.send().await?;
                if !response.status().is_success() {
                    anyhow::bail!("android bridge returned {}", response.status().as_u16());
                }

                Ok(response.json::<serde_json::Value>().await?)
            }
            "jni" => anyhow::bail!(
                "android.bridge.mode='jni' requires in-app Rust/Java binding and is not available in CLI process"
            ),
            other => anyhow::bail!(
                "Unsupported android.bridge.mode '{other}'. Supported: mock, http, jni"
            ),
        }
    }
}

#[async_trait]
impl Tool for AndroidDeviceTool {
    fn name(&self) -> &str {
        "android_device"
    }

    fn description(&self) -> &str {
        "Android device bridge for app launch, sensors, and optional telephony actions. High-risk actions (sms/call) are policy-gated and disabled for Google Play distribution by default."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "Action to perform: launch_app, list_apps, open_url, open_settings, sensor_read, vibrate, get_location, take_photo, record_audio, set_clipboard, read_clipboard, post_notification, get_network, get_battery, get_device_info, get_android_version, read_contacts, read_calendar, send_sms, read_sms, place_call, read_call_log"
                },
                "package": {
                    "type": "string",
                    "description": "Android package id for launch_app"
                },
                "url": {
                    "type": "string",
                    "description": "HTTPS URL for open_url"
                },
                "sensor": {
                    "type": "string",
                    "description": "Sensor name for sensor_read (e.g. accelerometer, gyroscope)"
                },
                "lens": {
                    "type": "string",
                    "description": "Optional camera lens for take_photo (front|rear)",
                    "default": "rear"
                },
                "text": {
                    "type": "string",
                    "description": "Clipboard text or notification body"
                },
                "title": {
                    "type": "string",
                    "description": "Notification title"
                },
                "duration_ms": {
                    "type": "integer",
                    "description": "Vibration duration in milliseconds",
                    "default": 500
                },
                "to": {
                    "type": "string",
                    "description": "Destination phone number for send_sms/place_call"
                },
                "body": {
                    "type": "string",
                    "description": "Message body for send_sms"
                },
                "limit": {
                    "type": "integer",
                    "description": "Optional read_sms/read_call_log result limit",
                    "default": 10
                },
                "approved": {
                    "type": "boolean",
                    "description": "Explicit approval flag for high-risk actions",
                    "default": false
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: global rate limit exceeded".into()),
            });
        }

        let action = args
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();

        let approved = args
            .get("approved")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        if let Err(error) = self.ensure_capability_enabled(&action) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error.to_string()),
            });
        }

        if let Err(error) = self.ensure_approved_if_required(approved, &action) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error.to_string()),
            });
        }

        let result: anyhow::Result<serde_json::Value> = match action.as_str() {
            "launch_app" => {
                let package = args
                    .get("package")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim();
                if package.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Missing 'package' for launch_app".into()),
                    });
                }
                self.ensure_package_allowed(package)?;
                self.execute_bridge_call("launch_app", json!({ "package": package }))
                    .await
            }
            "list_apps" => self.execute_bridge_call("list_apps", json!({})).await,
            "open_url" => {
                let url = args
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim();
                if url.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("open_url requires 'url'".into()),
                    });
                }
                if !url.starts_with("https://") {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("open_url requires an https:// URL".into()),
                    });
                }
                self.execute_bridge_call("open_url", json!({ "url": url }))
                    .await
            }
            "open_settings" => self.execute_bridge_call("open_settings", json!({})).await,
            "sensor_read" => {
                let sensor = args
                    .get("sensor")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim();
                if sensor.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Missing 'sensor' for sensor_read".into()),
                    });
                }
                self.execute_bridge_call("sensor_read", json!({ "sensor": sensor }))
                    .await
            }
            "vibrate" => {
                let duration_ms = args
                    .get("duration_ms")
                    .and_then(serde_json::Value::as_u64)
                    .map_or(500, |v| v.clamp(50, 10_000));
                self.execute_bridge_call("vibrate", json!({ "duration_ms": duration_ms }))
                    .await
            }
            "get_location" => self.execute_bridge_call("get_location", json!({})).await,
            "take_photo" => {
                let lens = args
                    .get("lens")
                    .and_then(serde_json::Value::as_str)
                    .map_or("rear", |v| {
                        if v.eq_ignore_ascii_case("front") {
                            "front"
                        } else {
                            "rear"
                        }
                    });
                self.execute_bridge_call("take_photo", json!({ "lens": lens }))
                    .await
            }
            "record_audio" => self.execute_bridge_call("record_audio", json!({})).await,
            "set_clipboard" => {
                let text = args
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim();
                if text.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("set_clipboard requires non-empty 'text'".into()),
                    });
                }
                self.execute_bridge_call("set_clipboard", json!({ "text": text }))
                    .await
            }
            "read_clipboard" => self.execute_bridge_call("read_clipboard", json!({})).await,
            "post_notification" => {
                let title = args
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("MobileClaw")
                    .trim();
                let text = args
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim();
                if text.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("post_notification requires non-empty 'text'".into()),
                    });
                }
                self.execute_bridge_call(
                    "post_notification",
                    json!({ "title": title, "text": text }),
                )
                .await
            }
            "get_network" => self.execute_bridge_call("get_network", json!({})).await,
            "get_battery" => self.execute_bridge_call("get_battery", json!({})).await,
            "get_device_info" | "get_android_version" => {
                self.execute_bridge_call("get_device_info", json!({})).await
            }
            "read_contacts" => self.execute_bridge_call("read_contacts", json!({})).await,
            "read_calendar" => self.execute_bridge_call("read_calendar", json!({})).await,
            "send_sms" => {
                let to = args
                    .get("to")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim();
                let body = args
                    .get("body")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim();
                if to.is_empty() || body.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("send_sms requires 'to' and 'body'".into()),
                    });
                }
                self.ensure_phone_allowed(to)?;
                Self::enforce_action_budget(
                    &self.sms_events,
                    self.config.policy.max_sms_per_hour,
                    "send_sms",
                )?;
                tracing::warn!(target: "android_device", to = %redact_phone(to), "android sms action requested");
                self.execute_bridge_call(
                    "send_sms",
                    json!({ "to": normalize_phone(to), "body": body }),
                )
                .await
            }
            "read_sms" => {
                let limit = args
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .map_or(10, |v| v.clamp(1, 100));
                self.execute_bridge_call("read_sms", json!({ "limit": limit }))
                    .await
            }
            "read_call_log" => {
                let limit = args
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .map_or(10, |v| v.clamp(1, 100));
                self.execute_bridge_call("read_call_log", json!({ "limit": limit }))
                    .await
            }
            "place_call" => {
                let to = args
                    .get("to")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim();
                if to.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("place_call requires 'to'".into()),
                    });
                }
                self.ensure_phone_allowed(to)?;
                Self::enforce_action_budget(
                    &self.call_events,
                    self.config.policy.max_calls_per_hour,
                    "place_call",
                )?;
                tracing::warn!(target: "android_device", to = %redact_phone(to), "android call action requested");
                self.execute_bridge_call("place_call", json!({ "to": normalize_phone(to) }))
                    .await
            }
            _ => anyhow::bail!("Unsupported action"),
        };

        let result = match result {
            Ok(value) => value,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(error.to_string()),
                });
            }
        };

        if result
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .is_some_and(|ok| !ok)
        {
            let detail = result
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("android_bridge_error")
                .to_string();
            return Ok(ToolResult {
                success: false,
                output: serde_json::to_string_pretty(&result)
                    .unwrap_or_else(|_| result.to_string()),
                error: Some(detail),
            });
        }

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string()),
            error: None,
        })
    }
}

fn extract_host(url: &str) -> anyhow::Result<String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| anyhow::anyhow!("invalid scheme"))?;
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .ok_or_else(|| anyhow::anyhow!("invalid endpoint"))?;
    if authority.is_empty() {
        anyhow::bail!("endpoint host missing");
    }
    let without_userinfo = authority
        .split_once('@')
        .map_or(authority, |(_, value)| value);
    let host = without_userinfo
        .split_once(':')
        .map_or(without_userinfo, |(value, _)| value);

    Ok(host.to_ascii_lowercase())
}

fn is_local_host(host: &str) -> bool {
    if host == "localhost" {
        return true;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return ip.is_loopback();
    }
    false
}

fn normalize_phone(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_digit() || *c == '+')
        .collect::<String>()
}

fn redact_phone(raw: &str) -> String {
    let normalized = normalize_phone(raw);
    let count = normalized.chars().count();
    if count <= 4 {
        return "****".into();
    }
    let suffix = normalized.chars().skip(count - 4).collect::<String>();
    format!("***{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> AndroidConfig {
        AndroidConfig {
            enabled: true,
            distribution: AndroidDistribution::Full,
            capabilities: crate::config::AndroidCapabilitiesConfig {
                sms: true,
                calls: true,
                app_launch: true,
                sensors: true,
                camera: true,
                microphone: true,
                location: true,
                notifications: true,
                clipboard: true,
                network: true,
                battery: true,
                contacts: true,
                calendar: true,
            },
            bridge: crate::config::AndroidBridgeConfig::default(),
            policy: crate::config::AndroidPolicyConfig {
                require_explicit_approval: true,
                allowed_packages: vec!["com.example.app".into()],
                allowed_phone_numbers: vec!["+15551234567".into()],
                max_sms_per_hour: 5,
                max_calls_per_hour: 5,
            },
        }
    }

    #[test]
    fn helper_normalizes_phone() {
        assert_eq!(normalize_phone("+1 (555) 123-4567"), "+15551234567");
    }

    #[tokio::test]
    async fn play_distribution_blocks_sms() {
        let mut cfg = test_config();
        cfg.distribution = AndroidDistribution::Play;
        let tool = AndroidDeviceTool::new(Arc::new(SecurityPolicy::default()), cfg);

        let result = tool
            .execute(json!({
                "action": "send_sms",
                "to": "+15551234567",
                "body": "hello",
                "approved": true
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .unwrap_or_default()
            .contains("disabled in play"));
    }

    #[tokio::test]
    async fn mock_launch_app_succeeds() {
        let tool = AndroidDeviceTool::new(Arc::new(SecurityPolicy::default()), test_config());

        let result = tool
            .execute(json!({
                "action": "launch_app",
                "package": "com.example.app"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("launch_app"));
    }

    #[tokio::test]
    async fn open_url_requires_https() {
        let tool = AndroidDeviceTool::new(Arc::new(SecurityPolicy::default()), test_config());

        let result = tool
            .execute(json!({
                "action": "open_url",
                "url": "http://example.com"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap_or_default().contains("https://"));
    }

    #[tokio::test]
    async fn clipboard_roundtrip_action_allowed_in_mock_mode() {
        let tool = AndroidDeviceTool::new(Arc::new(SecurityPolicy::default()), test_config());

        let result = tool
            .execute(json!({
                "action": "set_clipboard",
                "text": "hello"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("set_clipboard"));
    }
}
