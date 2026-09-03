//! HTTP-based tool derived from a skill's `[[tools]]` section.

use async_trait::async_trait;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_infra::net_guard::{Nat64Prefix, PrivateNetworkAccess, ResolvedDestination};

/// Maximum response body size (1 MB).
const MAX_RESPONSE_BYTES: usize = 1_048_576;
const RESPONSE_TRUNCATION_MARKER: &str = "\n... [response truncated at 1MB]";
/// HTTP request timeout (seconds).
const HTTP_TIMEOUT_SECS: u64 = 30;

/// A tool derived from a skill's `[[tools]]` section that makes HTTP requests.
pub struct SkillHttpTool {
    tool_name: String,
    tool_description: String,
    url_template: String,
    args: HashMap<String, String>,
    nat64_prefixes: Vec<Nat64Prefix>,
}

struct ValidatedTarget {
    url: reqwest::Url,
    destination: ResolvedDestination,
}

impl SkillHttpTool {
    /// Create a new skill HTTP tool with the runtime's parsed NAT64 snapshot.
    /// The tool name is prefixed with the skill name (`skill_name__tool_name`)
    /// to prevent collisions with built-in tools.
    pub fn new(
        skill_name: &str,
        tool: &crate::skills::SkillTool,
        nat64_prefixes: &[Nat64Prefix],
    ) -> Self {
        Self {
            tool_name: crate::tools::skill_tool::composed_tool_name(skill_name, &tool.name),
            tool_description: tool.description.clone(),
            url_template: tool.command.clone(),
            args: tool.args.clone(),
            nat64_prefixes: nat64_prefixes.to_vec(),
        }
    }

    fn build_parameters_schema(&self) -> serde_json::Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        for (name, description) in &self.args {
            properties.insert(
                name.clone(),
                serde_json::json!({
                    "type": "string",
                    "description": description
                }),
            );
            required.push(serde_json::Value::String(name.clone()));
        }

        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required
        })
    }

    /// Substitute `{{arg_name}}` placeholders in the URL template with
    /// the provided argument values.
    fn substitute_args(&self, args: &serde_json::Value) -> String {
        let mut url = self.url_template.clone();
        if let Some(obj) = args.as_object() {
            for (key, value) in obj {
                let placeholder = format!("{{{{{}}}}}", key);
                let replacement = value.as_str().unwrap_or_default();
                url = url.replace(&placeholder, &encode_url_component(replacement));
            }
        }
        url
    }

    async fn validate_target(&self, raw_url: &str) -> anyhow::Result<ValidatedTarget> {
        let mut url =
            reqwest::Url::parse(raw_url).map_err(|_| anyhow::Error::msg("Invalid URL"))?;
        if !url.username().is_empty() || url.password().is_some() {
            anyhow::bail!("URL userinfo is not allowed");
        }
        if !matches!(url.scheme(), "http" | "https") {
            anyhow::bail!("Only http:// and https:// URLs are allowed");
        }

        let request_host = url
            .host_str()
            .ok_or_else(|| anyhow::Error::msg("URL must include a host"))?;
        let host = zeroclaw_infra::net_guard::normalize_host(request_host)
            .map_err(|_| anyhow::Error::msg("URL host is invalid"))?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| anyhow::Error::msg("URL must include a valid port"))?;

        let addresses = if let Ok(ip) = host.parse::<IpAddr>() {
            vec![SocketAddr::new(ip, port)]
        } else {
            tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|_| anyhow::Error::msg("Failed to resolve HTTP destination"))?
                .collect::<Vec<_>>()
        };
        let destination = ResolvedDestination::new(
            &host,
            port,
            addresses,
            PrivateNetworkAccess::Deny,
            &self.nat64_prefixes,
        )
        .map_err(|_| anyhow::Error::msg("HTTP destination rejected by network policy"))?;

        // Reqwest's DNS pin is keyed by the request host. Use the canonical
        // validated spelling so normalization cannot make the pin miss.
        if host.parse::<IpAddr>().is_err() {
            url.set_host(Some(destination.host()))
                .map_err(|_| anyhow::Error::msg("URL host is invalid"))?;
        }

        Ok(ValidatedTarget { url, destination })
    }

    fn build_client(&self, target: &ValidatedTarget) -> anyhow::Result<reqwest::Client> {
        let builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS));
        let builder = if target.destination.host().parse::<IpAddr>().is_ok() {
            builder
        } else {
            builder.resolve_to_addrs(target.destination.host(), target.destination.addresses())
        };
        builder
            .build()
            .map_err(|_| anyhow::Error::msg("Failed to build HTTP client"))
    }
}

fn finalize_response_text(mut text: String, overflowed: bool) -> String {
    if !overflowed && text.len() <= MAX_RESPONSE_BYTES {
        return text;
    }

    let text_limit = MAX_RESPONSE_BYTES.saturating_sub(RESPONSE_TRUNCATION_MARKER.len());
    let mut boundary = text_limit.min(text.len());
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
    text.push_str(RESPONSE_TRUNCATION_MARKER);
    text
}

fn encode_url_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

#[async_trait]
impl Tool for SkillHttpTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.build_parameters_schema()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let raw_url = self.substitute_args(&args);
        let target = match self.validate_target(&raw_url).await {
            Ok(target) => target,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(error.to_string()),
                });
            }
        };
        let client = match self.build_client(&target) {
            Ok(client) => client,
            Err(error) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": error.to_string()})),
                    "skill_http tool: reqwest client build failed"
                );
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("Failed to build HTTP client".to_string()),
                });
            }
        };

        let response = match client.get(target.url).send().await {
            Ok(resp) => resp,
            Err(_e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("HTTP request failed".to_string()),
                });
            }
        };

        let status = response.status();
        let body =
            match zeroclaw_tools::helpers::read_response_text(response, Some(MAX_RESPONSE_BYTES))
                .await
            {
                Ok((text, overflowed)) => finalize_response_text(text, overflowed),
                Err(_e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some("Failed to read response body".to_string()),
                    });
                }
            };

        Ok(ToolResult {
            success: status.is_success(),
            output: body.into(),
            error: if status.is_success() {
                None
            } else {
                Some(format!("HTTP {}", status))
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::SkillTool;
    use serde_json::json;
    use zeroclaw_api::attribution::{Attributable, ToolProvenance};

    fn sample_http_tool() -> SkillTool {
        let mut args = HashMap::new();
        args.insert("city".to_string(), "City name to look up".to_string());

        SkillTool {
            name: "get_weather".to_string(),
            description: "Fetch weather for a city".to_string(),
            kind: "http".to_string(),
            command: "https://api.example.com/weather?city={{city}}".to_string(),
            args,
            target: None,
            locked_args: HashMap::new(),
            timeout_secs: None,
        }
    }

    fn wttr_in_weather_tool() -> SkillTool {
        let mut args = HashMap::new();
        args.insert(
            "location".to_string(),
            "Location to get weather for".to_string(),
        );

        SkillTool {
            name: "weather_lookup".to_string(),
            description: "Fetch weather from wttr.in".to_string(),
            kind: "http".to_string(),
            command: "https://wttr.in/{{location}}?format=j1".to_string(),
            args,
            target: None,
            locked_args: HashMap::new(),
            timeout_secs: None,
        }
    }

    #[test]
    fn skill_http_tool_name_is_prefixed() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool(), &[]);
        assert_eq!(tool.name(), "weather_skill__get_weather");
    }

    #[test]
    fn manifest_loaded_skill_http_is_an_extension() {
        let tool = SkillHttpTool::new("browser", &sample_http_tool(), &[]);

        assert_eq!(tool.tool_provenance(), ToolProvenance::Extension);
    }

    #[test]
    fn skill_http_tool_description() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool(), &[]);
        assert_eq!(tool.description(), "Fetch weather for a city");
    }

    #[test]
    fn skill_http_tool_parameters_schema() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool(), &[]);
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["city"].is_object());
        assert_eq!(schema["properties"]["city"]["type"], "string");
    }

    #[test]
    fn skill_http_tool_substitute_args() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool(), &[]);
        let result = tool.substitute_args(&serde_json::json!({"city": "London"}));
        assert_eq!(result, "https://api.example.com/weather?city=London");
    }

    #[test]
    fn skill_http_tool_encodes_structural_and_unicode_arguments() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool(), &[]);
        let result = tool.substitute_args(&serde_json::json!({
            "city": "../?x=1#fragment&name=alice@example.com:443/東京"
        }));
        assert_eq!(
            result,
            "https://api.example.com/weather?city=..%2F%3Fx%3D1%23fragment%26name%3Dalice%40example.com%3A443%2F%E6%9D%B1%E4%BA%AC"
        );
    }

    #[test]
    fn final_response_text_bounds_lossy_utf8_expansion() {
        let raw = vec![0xff; MAX_RESPONSE_BYTES];
        let text = String::from_utf8_lossy(&raw).into_owned();
        let finalized = finalize_response_text(text, false);

        assert!(finalized.len() <= MAX_RESPONSE_BYTES);
        assert!(finalized.ends_with(RESPONSE_TRUNCATION_MARKER));
        assert!(std::str::from_utf8(finalized.as_bytes()).is_ok());
    }

    #[test]
    fn skill_http_tool_rejects_private_metadata_mixed_and_nat64_destinations() {
        use std::net::{Ipv4Addr, Ipv6Addr};
        use zeroclaw_infra::net_guard::parse_nat64_prefixes;

        let reject = |host: &str, addresses: &[SocketAddr], prefixes: &[Nat64Prefix]| {
            ResolvedDestination::new(
                host,
                443,
                addresses.iter().copied(),
                PrivateNetworkAccess::Deny,
                prefixes,
            )
            .is_err()
        };
        assert!(reject(
            "private.example",
            &[SocketAddr::from(([10, 0, 0, 1], 443))],
            &[]
        ));
        assert!(reject(
            "metadata.example",
            &[SocketAddr::from(([169, 254, 169, 254], 443))],
            &[]
        ));
        assert!(reject(
            "mixed.example",
            &[
                SocketAddr::from(([8, 8, 8, 8], 443)),
                SocketAddr::from(([10, 0, 0, 1], 443)),
            ],
            &[]
        ));
        let prefixes = parse_nat64_prefixes(&["2001:db8:122:344::/96".to_string()], "test")
            .expect("valid NAT64 prefix");
        assert!(reject(
            "nat64.example",
            &[SocketAddr::new(
                Ipv6Addr::new(0x2001, 0x0db8, 0x0122, 0x0344, 0, 0, 0x0a00, 1).into(),
                443,
            )],
            &prefixes,
        ));
        assert!(reject(
            "loopback.example",
            &[SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 443)],
            &[]
        ));
    }

    #[tokio::test]
    async fn skill_http_client_pins_destination_and_does_not_follow_redirects() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local redirect server");
        let address = listener.local_addr().expect("local redirect address");
        zeroclaw_spawn::spawn!(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut buffer = [0_u8; 1024];
                let read = stream.read(&mut buffer).await.expect("read request");
                assert!(read > 0, "client closed before request headers");
                request.extend_from_slice(&buffer[..read]);
            }
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write redirect");
        });

        // The production path always uses Deny. Allow is used only here to
        // pin the test's public hostname to the local held-open server.
        let destination = ResolvedDestination::new(
            "example.com",
            address.port(),
            [address],
            PrivateNetworkAccess::Allow,
            &[],
        )
        .expect("test destination");
        let target = ValidatedTarget {
            url: reqwest::Url::parse(&format!("http://example.com:{}/redirect", address.port()))
                .expect("test URL"),
            destination,
        };
        let client = SkillHttpTool::new("skill", &sample_http_tool(), &[])
            .build_client(&target)
            .expect("build pinned client");
        let response = client
            .get(target.url)
            .send()
            .await
            .expect("request local redirect server");
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
    }

    #[test]
    fn skill_http_can_model_minimal_wttr_weather_lookup() {
        let tool = SkillHttpTool::new("weather_skill", &wttr_in_weather_tool(), &[]);

        assert_eq!(tool.name(), "weather_skill__weather_lookup");
        assert_eq!(tool.description(), "Fetch weather from wttr.in");

        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["location"]["type"], "string");
        assert_eq!(
            schema["properties"]["location"]["description"],
            "Location to get weather for"
        );
        assert!(
            schema["required"]
                .as_array()
                .expect("required array")
                .iter()
                .any(|name| name == "location")
        );

        let url = tool.substitute_args(&serde_json::json!({"location": "London"}));
        assert_eq!(url, "https://wttr.in/London?format=j1");
    }

    #[test]
    fn skill_http_tool_spec_roundtrip() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool(), &[]);
        let spec = tool.spec();
        assert_eq!(spec.name, "weather_skill__get_weather");
        assert_eq!(spec.description, "Fetch weather for a city");
        assert_eq!(spec.parameters["type"], "object");
    }

    #[test]
    fn skill_http_tool_name_sanitized_for_provider_regex() {
        // A plugin-namespaced HTTP skill (colons) or a dotted tool name must
        // still yield a provider-valid function name, the same as shell/builtin
        // tools, socannot survive through the HTTP registration path.
        let mut st = sample_http_tool();
        st.name = "fetch.weather".to_string();
        let tool = SkillHttpTool::new("pr-review-toolkit:code-reviewer", &st, &[]);
        let name = tool.name();
        assert!(
            !name.is_empty()
                && name.len() <= 64
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "HTTP skill tool name `{name}` is not provider-valid",
        );
        // A valid name must still pass through unchanged (no spurious suffix).
        let plain = SkillHttpTool::new("weather_skill", &sample_http_tool(), &[]);
        assert_eq!(plain.name(), "weather_skill__get_weather");
    }

    #[test]
    fn skill_http_tool_empty_args() {
        let st = SkillTool {
            name: "ping".to_string(),
            description: "Ping endpoint".to_string(),
            kind: "http".to_string(),
            command: "https://api.example.com/ping".to_string(),
            args: HashMap::new(),
            target: None,
            locked_args: HashMap::new(),
            timeout_secs: None,
        };
        let tool = SkillHttpTool::new("s", &st, &[]);
        let schema = tool.parameters_schema();
        assert!(schema["properties"].as_object().unwrap().is_empty());
    }

    fn http_tool_with_command(command: &str) -> SkillHttpTool {
        let st = SkillTool {
            name: "fetch".to_string(),
            description: "test".to_string(),
            kind: "http".to_string(),
            command: command.to_string(),
            args: HashMap::new(),
            target: None,
            locked_args: HashMap::new(),
            timeout_secs: None,
        };
        SkillHttpTool::new("test_skill", &st, &[])
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_literal_private_and_metadata_destinations_before_send() {
        for url in [
            "http://127.0.0.1:9/private",
            "http://169.254.169.254/metadata",
        ] {
            let tool = http_tool_with_command(url);
            let result = tool.execute(json!({})).await.unwrap();
            assert!(!result.success);
            assert_eq!(
                result.error.as_deref(),
                Some("HTTP destination rejected by network policy")
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_userinfo_targeting_private_host() {
        // `{{user}}` substitution yields a userinfo prefix. Parser-level
        // reject, no opt-out (the skill author cannot allow userinfo even
        // for legitimate credentials in URL).
        let tool = http_tool_with_command("https://{{user}}@api.example.com/path");
        let result = tool
            .execute(json!({"user": "attacker@127.0.0.1"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("userinfo"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_userinfo_with_password() {
        let tool = http_tool_with_command("https://{{u}}:{{p}}@api.example.com/path");
        let result = tool
            .execute(json!({"u": "alice", "p": "secret@10.0.0.1"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("userinfo"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_userinfo_even_when_host_is_public() {
        // The userinfo reject is parser-level, not policy-level. A
        // legitimate-looking public host with userinfo is still rejected
        // so the gate cannot be bypassed by mixing public/private strings.
        let tool = http_tool_with_command("https://{{u}}@example.com/path");
        let result = tool.execute(json!({"u": "anyone"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("userinfo"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_non_http_scheme() {
        // `file://`, `gopher://`, etc. must still be rejected.
        let tool = http_tool_with_command("file://etc/passwd");
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("http") || err.contains("Invalid URL"),
            "got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_malformed_url() {
        // The skill author supplied a template with no scheme, only a host.
        // `reqwest::Url::parse` rejects it as RelativeUrlWithoutBase.
        let tool = http_tool_with_command("example.com/ping");
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("Invalid URL"), "got: {err}");
    }
}
