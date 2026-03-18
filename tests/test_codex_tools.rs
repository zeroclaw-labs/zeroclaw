// Quick manual test: Codex native tool calling with gpt-5.1 + low reasoning
// Run: source .env && ZEROCLAW_CODEX_REASONING_EFFORT=low cargo test --test test_codex_tools -- --ignored --nocapture

use std::time::Instant;

#[tokio::test]
#[ignore]
async fn codex_gpt51_native_tool_call() {
    // Setup provider
    let options = zeroclaw::providers::ProviderRuntimeOptions {
        provider_api_url: None,
        zeroclaw_dir: Some(
            directories::UserDirs::new()
                .unwrap()
                .home_dir()
                .join(".zeroclaw"),
        ),
        secrets_encrypt: false,
        auth_profile_override: None,
        reasoning_enabled: None,
        reasoning_effort: None,
        provider_timeout_secs: None,
        extra_headers: std::collections::HashMap::new(),
        api_path: None,
    };

    let provider = zeroclaw::providers::openai_codex::OpenAiCodexProvider::new(&options, None)
        .expect("provider init");

    // Verify capabilities
    use zeroclaw::providers::traits::Provider;
    let caps = provider.capabilities();
    assert!(
        caps.native_tool_calling,
        "must report native_tool_calling=true"
    );
    println!(
        "✅ capabilities: native_tool_calling={}",
        caps.native_tool_calling
    );

    // Build a simple tool
    let tools = vec![zeroclaw::tools::ToolSpec {
        name: "get_weather".to_string(),
        description: "Get current weather for a city".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "city": {
                    "type": "string",
                    "description": "City name"
                }
            },
            "required": ["city"]
        }),
    }];

    // Send a request that should trigger tool use
    let messages = vec![
        zeroclaw::providers::traits::ChatMessage::system(
            "You are a helpful assistant. Use the get_weather tool when asked about weather.",
        ),
        zeroclaw::providers::traits::ChatMessage::user("What's the weather in Bangkok?"),
    ];

    let request = zeroclaw::providers::traits::ChatRequest {
        messages: &messages,
        tools: Some(&tools),
    };

    let start = Instant::now();
    let response = provider.chat(request, "gpt-5.1", 0.7).await;
    let elapsed = start.elapsed();

    match response {
        Ok(resp) => {
            println!("✅ Response in {:.1}s", elapsed.as_secs_f64());
            println!(
                "   text: {:?}",
                resp.text.as_deref().map(|t| &t[..t.len().min(200)])
            );
            println!("   tool_calls: {}", resp.tool_calls.len());
            for tc in &resp.tool_calls {
                println!("     - {}(id={}) args={}", tc.name, tc.id, tc.arguments);
            }
            assert!(
                resp.tool_calls.iter().any(|tc| tc.name == "get_weather"),
                "Expected get_weather tool call, got: {:?}",
                resp.tool_calls
                    .iter()
                    .map(|tc| &tc.name)
                    .collect::<Vec<_>>()
            );
            println!("✅ get_weather tool call confirmed!");
        }
        Err(e) => {
            println!("❌ Error after {:.1}s: {e}", elapsed.as_secs_f64());
            // Don't panic on auth/rate-limit errors in CI
            let err_str = e.to_string();
            if err_str.contains("rate") || err_str.contains("auth") || err_str.contains("token") {
                println!("⚠️ Skipping assertion due to auth/rate error");
            } else {
                panic!("Unexpected error: {e}");
            }
        }
    }
}
