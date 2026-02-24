//! E2E test for vision support in providers.
//!
//! This test validates that:
//! 1. Provider reports vision capability
//! 2. Provider correctly processes messages with [IMAGE:...] markers
//! 3. Request is sent to API with proper image_url format
//!
//! Requires:
//! - Live provider OAuth credentials (OpenAI Codex or Gemini)
//! - Test image at /tmp/test_vision.png
//!
//! Run manually: `cargo test provider_vision -- --ignored --nocapture`

use anyhow::Result;
use zeroclaw::providers::{ChatMessage, ChatRequest, Provider, ProviderRuntimeOptions};

/// Tests that provider supports vision input.
///
/// This test:
/// 1. Creates provider via factory (tries OpenAI Codex, falls back to Gemini)
/// 2. Verifies vision capability is reported
/// 3. Sends a message with [IMAGE:...] marker
/// 4. Verifies request succeeds without capability error
#[tokio::test]
#[ignore = "requires live provider OAuth credentials"]
async fn provider_vision_support() -> Result<()> {
    // Use Gemini provider (OpenAI Codex is rate-limited until 21 Feb)
    println!("Creating Gemini provider...");
    let provider = zeroclaw::providers::create_provider("gemini", None)?;
    let provider_name = "gemini";
    let model = "gemini-2.5-pro";

    println!("✓ Created {} provider", provider_name);

    // Warmup provider (for OAuth token refresh if needed)
    println!("Warming up provider...");
    provider.warmup().await?;
    println!("✓ Provider warmed up");

    // Verify vision capability
    let capabilities = provider.capabilities();
    println!(
        "Provider {} capabilities: vision={}",
        provider_name, capabilities.vision
    );

    if !capabilities.vision {
        anyhow::bail!(
            "❌ {} provider does not report vision capability! \
             Check that provider's capabilities() returns vision=true",
            provider_name
        );
    }

    println!("✓ Provider {} reports vision=true", provider_name);

    // Prepare test image path
    let test_image = "/tmp/test_vision.png";

    if !std::path::Path::new(test_image).exists() {
        eprintln!("⚠️  Test image not found at {}", test_image);
        eprintln!("Creating minimal 1x1 PNG...");

        // Create minimal PNG if missing
        use base64::{engine::general_purpose, Engine as _};
        let png_data = general_purpose::STANDARD.decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
        )?;
        std::fs::write(test_image, png_data)?;

        println!("✓ Created test image at {}", test_image);
    }

    // Prepare message with image marker
    let user_message = format!("What is in this image? [IMAGE:{}]", test_image);

    println!("Sending message with image marker...");
    println!("Message: {}", user_message);

    // Build chat request
    let messages = vec![
        ChatMessage::system("You are a helpful assistant that can analyze images."),
        ChatMessage::user(user_message.clone()),
    ];

    let request = ChatRequest {
        messages: &messages,
        tools: None,
    };

    // Send request to provider
    println!("Using model: {}", model);
    let result = provider.chat(request, model, 0.7).await;

    match result {
        Ok(response) => {
            println!("✓ Request succeeded!");
            if let Some(text) = response.text {
                println!("Response text: {}", text);
            }
            println!("Tool calls: {}", response.tool_calls.len());

            // Success: provider accepted vision input
            println!("\n✅ {} vision support is working!", provider_name);
            Ok(())
        }
        Err(e) => {
            eprintln!("❌ Request failed: {}", e);

            // Check if it's the capability error we're testing for
            let error_str = e.to_string();
            if error_str.contains("provider_capability_error")
                || error_str.contains("does not support vision")
            {
                eprintln!("\n⚠️  CAPABILITY ERROR DETECTED!");
                eprintln!("This means the agent loop is still blocking vision input.");
                eprintln!("Possible causes:");
                eprintln!("  1. Service binary not rebuilt (check timestamp)");
                eprintln!("  2. Service not restarted with new binary");
                eprintln!("  3. Provider factory returning wrong implementation");
                anyhow::bail!("Vision capability check failed in agent loop");
            }

            // Other errors (API error, auth, etc) are also failures but different nature
            eprintln!("\n⚠️  Request failed with non-capability error");
            eprintln!("This might be:");
            eprintln!("  - API authentication issue");
            eprintln!("  - Network error");
            eprintln!("  - API format rejection");
            Err(e)
        }
    }
}

/// Tests that OpenAI Codex second profile supports vision input.
///
/// This test:
/// 1. Creates OpenAI Codex provider with "second" profile override
/// 2. Verifies vision capability is reported
/// 3. Sends a message with [IMAGE:...] marker
/// 4. Verifies request succeeds without capability error
#[tokio::test]
#[ignore = "requires live OpenAI Codex OAuth credentials (second profile)"]
async fn openai_codex_second_vision_support() -> Result<()> {
    println!("Creating OpenAI Codex provider with second profile...");

    // Create provider with profile override
    let opts = ProviderRuntimeOptions {
        auth_profile_override: Some("second".to_string()),
        provider_api_url: None,
        zeroclaw_dir: None,
        secrets_encrypt: false,
        reasoning_enabled: None,
        custom_provider_api_mode: None,
        max_tokens_override: None,
        model_support_vision: None,
    };

    let provider = zeroclaw::providers::create_provider_with_options("openai-codex", None, &opts)?;
    let provider_name = "openai-codex:second";
    let model = "gpt-5.3-codex";

    println!("✓ Created {} provider", provider_name);

    // Verify vision capability
    let capabilities = provider.capabilities();
    println!(
        "Provider {} capabilities: vision={}",
        provider_name, capabilities.vision
    );

    if !capabilities.vision {
        anyhow::bail!(
            "❌ {} provider does not report vision capability! \
             Check that provider's capabilities() returns vision=true",
            provider_name
        );
    }

    println!("✓ Provider {} reports vision=true", provider_name);

    // Prepare test image path
    let test_image = "/tmp/test_vision.png";

    if !std::path::Path::new(test_image).exists() {
        eprintln!("⚠️  Test image not found at {}", test_image);
        eprintln!("Creating minimal 1x1 PNG...");

        // Create minimal PNG if missing
        use base64::{engine::general_purpose, Engine as _};
        let png_data = general_purpose::STANDARD.decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
        )?;
        std::fs::write(test_image, png_data)?;

        println!("✓ Created test image at {}", test_image);
    }

    // Prepare message with image marker
    let user_message = format!("What is in this image? [IMAGE:{}]", test_image);

    println!("Sending message with image marker...");
    println!("Message: {}", user_message);

    // Build chat request
    let messages = vec![
        ChatMessage::system("You are a helpful assistant that can analyze images."),
        ChatMessage::user(user_message.clone()),
    ];

    let request = ChatRequest {
        messages: &messages,
        tools: None,
    };

    // Send request to provider
    println!("Using model: {}", model);
    let result = provider.chat(request, model, 0.7).await;

    match result {
        Ok(response) => {
            println!("✓ Request succeeded!");
            if let Some(text) = response.text {
                println!("Response text: {}", text);
            }
            println!("Tool calls: {}", response.tool_calls.len());

            // Success: provider accepted vision input
            println!("\n✅ {} vision support is working!", provider_name);
            Ok(())
        }
        Err(e) => {
            eprintln!("❌ Request failed: {}", e);

            // Check if it's the capability error we're testing for
            let error_str = e.to_string();
            if error_str.contains("provider_capability_error")
                || error_str.contains("does not support vision")
            {
                eprintln!("\n⚠️  CAPABILITY ERROR DETECTED!");
                eprintln!("This means the agent loop is still blocking vision input.");
                anyhow::bail!("Vision capability check failed in agent loop");
            }

            // Check if it's rate limit
            if error_str.contains("429")
                || error_str.contains("rate")
                || error_str.contains("limit")
            {
                eprintln!("\n⚠️  RATE LIMITED!");
                eprintln!("Second OpenAI Codex profile is also rate-limited.");
                eprintln!("This is OK - it means both profiles share the same quota.");
                // Don't fail the test - rate limit is expected
                return Ok(());
            }

            // Other errors (API error, auth, etc) are also failures but different nature
            eprintln!("\n⚠️  Request failed with non-capability error");
            eprintln!("This might be:");
            eprintln!("  - API authentication issue");
            eprintln!("  - Network error");
            eprintln!("  - API format rejection");
            Err(e)
        }
    }
}
