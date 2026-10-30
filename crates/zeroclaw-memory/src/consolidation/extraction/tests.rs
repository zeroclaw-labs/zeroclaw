use super::*;
use serde_json::json;
use zeroclaw_api::schema::SchemaCleanr;

fn valid() -> Value {
    json!({
        "history_entry": "Discussed language choice.",
        "memory_update": "The project uses Rust.",
        "kind": "decision",
        "facts": ["The project uses Rust."],
        "trend": null
    })
}

#[test]
fn schema_type_validator_and_cleaned_projection_share_contract_fixtures() {
    let contract = contract().unwrap();
    assert_eq!(
        contract.schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    let projected = tool_schema(&contract.schema);
    let cleaned = SchemaCleanr::clean_for_anthropic(projected.clone());
    assert_eq!(
        cleaned, projected,
        "provider cleaning must not narrow the projection"
    );
    let projected_validator = jsonschema::draft202012::new(&cleaned).unwrap();
    let mut fixtures = vec![
        valid(),
        json!({"history_entry":"Greeting.","memory_update":null}),
    ];
    for kind in [
        Value::Null,
        json!("preference"),
        json!("fact"),
        json!("decision"),
        json!("entity"),
    ] {
        let mut fixture = valid();
        fixture["kind"] = kind;
        fixtures.push(fixture);
    }
    let mut all_null = valid();
    all_null["memory_update"] = Value::Null;
    all_null["kind"] = Value::Null;
    fixtures.push(all_null);
    for fixture in fixtures {
        assert!(contract.validator.is_valid(&fixture));
        assert!(projected_validator.is_valid(&fixture));
        let decoded = validate(fixture, contract).unwrap();
        let round_trip = serde_json::to_value(decoded).unwrap();
        let fields: Vec<_> = round_trip.as_object().unwrap().keys().collect();
        let properties: Vec<_> = contract.schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(fields, properties, "Rust result and schema fields drifted");
        assert!(contract.validator.is_valid(&round_trip));
    }
    let mut conditional = valid();
    conditional["memory_update"] = Value::Null;
    assert!(projected_validator.is_valid(&conditional));
    assert!(
        validate(conditional, contract).is_err(),
        "canonical conditional must still apply"
    );
}

#[test]
fn rejects_schema_failures_without_coercion() {
    let contract = contract().unwrap();
    let mut fixtures = vec![json!({}), json!([]), json!({"history_entry":"h"})];
    for (field, bad) in [
        ("history_entry", json!(null)),
        ("history_entry", json!("")),
        ("history_entry", json!(" \n\t\u{2003}\u{85}")),
        ("memory_update", json!(17)),
        ("facts", json!(null)),
        ("facts", json!(["valid", "  \u{2003}"])),
        ("facts", json!(["1", "2", "3", "4", "5", "6"])),
        ("facts", json!([false])),
        ("trend", json!({})),
        ("kind", json!("pref")),
        ("kind", json!("FACT")),
        ("kind", json!("unknown")),
        ("unexpected", json!("private")),
    ] {
        let mut fixture = valid();
        fixture[field] = bad;
        fixtures.push(fixture);
    }
    for fixture in fixtures {
        assert!(
            !contract.validator.is_valid(&fixture),
            "accepted invalid fixture: {fixture}"
        );
        assert!(validate(fixture, contract).is_err());
    }
}

#[test]
fn every_string_limit_is_enforced_in_unicode_characters() {
    let contract = contract().unwrap();
    for (field, limit) in [
        ("history_entry", 2000),
        ("memory_update", 4000),
        ("trend", 2000),
    ] {
        assert_eq!(contract.schema["properties"][field]["maxLength"], limit);
        let mut fixture = valid();
        fixture[field] = json!("🦀".repeat(limit as usize));
        assert!(validate(fixture.clone(), contract).is_ok());
        fixture[field] = json!("🦀".repeat(limit as usize + 1));
        assert!(validate(fixture, contract).is_err());
    }
    assert_eq!(
        contract.schema["properties"]["facts"]["items"]["maxLength"],
        1000
    );
    assert_eq!(contract.schema["properties"]["kind"]["maxLength"], 10);
    let mut fixture = valid();
    fixture["facts"] = json!(["🦀".repeat(1000)]);
    assert!(validate(fixture.clone(), contract).is_ok());
    fixture["facts"] = json!(["🦀".repeat(1001)]);
    assert!(validate(fixture, contract).is_err());

    let escaped = json!({
        "history_entry": "🦀".repeat(2000), "memory_update": "🦀".repeat(4000),
        "facts": vec!["🦀".repeat(1000); 5], "trend": "🦀".repeat(2000), "kind":"preference"
    })
    .to_string()
    .replace('🦀', "\\ud83e\\udd80");
    assert!(
        parse_prompt(&escaped).is_ok(),
        "escaped Unicode must fit at every field limit"
    );
}

#[test]
fn accepts_one_plain_fenced_prefixed_or_named_object() {
    let json = valid().to_string();
    for raw in [
        json.clone(),
        format!("```json\n{json}\n```"),
        format!("Result:\n{json}\nDone."),
        format!("<tool_call>{{\"name\":\"{TOOL_NAME}\",\"arguments\":{json}}}</tool_call>"),
    ] {
        assert_eq!(
            parse_prompt(&raw).unwrap().kind.as_deref(),
            Some("decision")
        );
    }
    let mut braces = valid();
    braces["history_entry"] = json!("Literal braces { } and escaped quote \".");
    assert!(parse_prompt(&braces.to_string()).is_ok());
}

#[test]
fn rejects_ambiguous_partial_or_duplicate_key_candidates() {
    let json = valid().to_string();
    for raw in [
        String::new(),
        "no output".into(),
        format!("{json}\n{json}"),
        format!("[{json}]"),
        format!("{{broken: {json}}}"),
        format!("{json} {{"),
        r#"{"history_entry":"h","memory_update":null,"kind":"bad","kind":null}"#.into(),
        format!("{{\"name\":\"wrong\",\"arguments\":{json}}}"),
        format!("{{\"name\":\"{TOOL_NAME}\",\"arguments\":{json},\"extra\":1}}"),
        format!("<tool_call>{{\"name\":\"{TOOL_NAME}\",\"arguments\":{json}}}</tool_call>{json}"),
    ] {
        assert!(parse_prompt(&raw).is_err(), "accepted ambiguous response");
    }
}

#[test]
fn repair_feedback_is_bounded_and_contains_no_untrusted_keys_or_values() {
    let mut fixture = valid();
    fixture["private-key-sentinel"] = json!("private-value-sentinel");
    fixture["facts"] = json!(vec!["private-value-sentinel".repeat(100); 20]);
    fixture["kind"] = json!("private-value-sentinel");
    let diagnostics = validate(fixture, contract().unwrap()).unwrap_err();
    assert!(!diagnostics.contains("sentinel"));
    assert!(diagnostics.len() <= 256);
    assert!(diagnostics.lines().count() <= MAX_DIAGNOSTICS);
    for line in diagnostics.lines() {
        let (keyword, path) = line.split_once(' ').unwrap();
        assert!(
            [
                "type",
                "required",
                "additionalProperties",
                "pattern",
                "maxLength",
                "maxItems",
                "enum"
            ]
            .contains(&keyword)
        );
        assert!(
            [
                "/",
                "/history_entry",
                "/memory_update",
                "/facts",
                "/trend",
                "/kind"
            ]
            .contains(&path)
        );
    }
    assert!(parse_prompt(&"x".repeat(MAX_RESPONSE_BYTES + 1)).is_err());
}

// The test opts a local mock endpoint into forcing. Production custom endpoints
// retain their conservative capability default.
struct WireProvider(zeroclaw_providers::anthropic::AnthropicModelProvider);

#[tokio::test]
async fn repair_retains_source_through_the_default_history_adapter() {
    use parking_lot::Mutex;
    use std::collections::VecDeque;

    struct LastUserProvider {
        replies: Mutex<VecDeque<String>>,
        messages: Mutex<Vec<String>>,
    }
    impl zeroclaw_api::attribution::Attributable for LastUserProvider {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Provider(
                zeroclaw_api::attribution::ProviderKind::Model(
                    zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "last-user-fixture"
        }
    }
    #[async_trait::async_trait]
    impl ModelProvider for LastUserProvider {
        async fn chat_with_system(
            &self,
            _: Option<&str>,
            message: &str,
            _: &str,
            _: Option<f64>,
        ) -> anyhow::Result<String> {
            self.messages.lock().push(message.into());
            Ok(self
                .replies
                .lock()
                .pop_front()
                .expect("unexpected extraction call"))
        }
    }
    let provider = LastUserProvider {
        replies: Mutex::new(VecDeque::from([
            r#"{"history_entry":"candidate-only-sentinel","memory_update":null,"kind":"unknown"}"#
                .into(),
            valid().to_string(),
        ])),
        messages: Mutex::new(Vec::new()),
    };
    let turn = "User: retain this source turn\nAssistant: recorded the source";
    assert!(
        extract(&provider, "test-model", None, "Extract memory.", turn)
            .await
            .is_some()
    );
    let messages = provider.messages.lock();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0], turn);
    assert!(messages[1].starts_with(turn));
    assert!(messages[1].contains("enum /kind"));
    assert!(!messages[1].contains("candidate-only-sentinel"));
}

impl zeroclaw_api::attribution::Attributable for WireProvider {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        self.0.role()
    }
    fn alias(&self) -> &str {
        "mock-anthropic-extraction"
    }
}

#[async_trait::async_trait]
impl ModelProvider for WireProvider {
    fn supports_forced_tool_choice(&self, _: &str) -> bool {
        true
    }
    async fn chat_with_system(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("wire fixture requires structured dispatch")
    }
    async fn chat_with_tool_round_policy(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
        policy: ToolRoundPolicy,
    ) -> anyhow::Result<ChatResponse> {
        self.0
            .chat_with_tool_round_policy(request, model, temperature, policy)
            .await
    }
}

#[tokio::test]
async fn native_extraction_reaches_anthropic_wire_with_derived_schema_and_forcing() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = zeroclaw_spawn::spawn!(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        let mut chunk = [0u8; 4096];
        let (header_end, content_length) = loop {
            let read = socket.read(&mut chunk).await.unwrap();
            assert!(read > 0 && received.len() < 100_000);
            received.extend_from_slice(&chunk[..read]);
            if let Some(end) = received.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&received[..end]).unwrap();
                assert!(headers.starts_with("POST /v1/messages HTTP/1.1"));
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                break (end + 4, length);
            }
        };
        while received.len() < header_end + content_length {
            let read = socket.read(&mut chunk).await.unwrap();
            assert!(read > 0 && received.len() < 100_000);
            received.extend_from_slice(&chunk[..read]);
        }
        let request: Value =
            serde_json::from_slice(&received[header_end..header_end + content_length]).unwrap();
        let body = json!({"content":[{"type":"tool_use","id":"extract-1","name":TOOL_NAME,"input":{
            "history_entry":"Wire summary", "memory_update":null, "kind":null, "facts":[], "trend":null
        }}],"stop_reason":"tool_use","usage":{"input_tokens":10,"output_tokens":10}}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        request
    });
    let provider = WireProvider(
        zeroclaw_providers::anthropic::AnthropicModelProvider::builder("wire")
            .credential(Some("test-key"))
            .base_url(&format!("http://{address}"))
            .timeout_secs(5)
            .build(),
    );
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        extract(
            &provider,
            "claude-sonnet-4-6",
            None,
            "Extract a memory summary.",
            "User: greeting",
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.history_entry, "Wire summary");
    assert!(result.memory_update.is_none());
    let request = server.await.unwrap();
    assert_eq!(request["tool_choice"]["type"], "any");
    assert_eq!(request["tools"].as_array().unwrap().len(), 1);
    assert_eq!(request["tools"][0]["name"], TOOL_NAME);
    assert_eq!(
        request["tools"][0]["input_schema"],
        tool_schema(&contract().unwrap().schema)
    );
    assert!(request.get("thinking").is_none());
}
