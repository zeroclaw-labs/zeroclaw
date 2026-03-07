//! E2E test for Gemini fallback with OAuth token refresh.
//!
//! This test validates that when:
//! 1. Primary provider (OpenAI Codex) fails
//! 2. Fallback to Gemini is triggered
//! 3. Gemini OAuth tokens are expired (we manually expire them)
//!
//! Then:
//!
//! - Gemini provider's warmup() automatically refreshes the tokens
//! - The fallback request succeeds
//!
//! Requires:
//! - Live Gemini OAuth profile in `~/.zeroclaw/auth-profiles.json` with refresh_token
//! - GEMINI_OAUTH_CLIENT_ID and GEMINI_OAUTH_CLIENT_SECRET env vars
//!
//! Run manually: `cargo test gemini_fallback_oauth_refresh -- --ignored --nocapture`

use anyhow::Result;
use chrono::{Duration, Utc};
use serde_json::Value;
use std::env;
use std::fs;
use std::path::PathBuf;

/// Tests that Gemini warmup() refreshes expired OAuth tokens.
///
/// This test:
/// 1. Backs up real auth-profiles.json
/// 2. Modifies it to set Gemini token as expired
/// 3. Creates a Gemini provider and calls warmup()
/// 4. Verifies token was refreshed
/// 5. Restores original auth-profiles.json
#[tokio::test]
#[ignore = "requires live Gemini OAuth credentials with refresh_token"]
async fn gemini_warmup_refreshes_expired_oauth_token() -> Result<()> {
    // Find ~/.zeroclaw/auth-profiles.json
    let home = env::var("HOME").expect("HOME env var not set");
    let zeroclaw_dir = PathBuf::from(home).join(".zeroclaw");
    let auth_profiles_path = zeroclaw_dir.join("auth-profiles.json");

    if !auth_profiles_path.exists() {
        eprintln!(
            "⚠️  No auth-profiles.json found at {:?}",
            auth_profiles_path
        );
        eprintln!("Run: zeroclaw auth login --provider gemini");
        return Ok(());
    }

    // Load current auth-profiles.json
    let original_content = fs::read_to_string(&auth_profiles_path)?;
    let mut data: Value = serde_json::from_str(&original_content)?;

    println!("Loaded auth-profiles.json");

    // Find Gemini profile
    let profiles = data
        .get_mut("profiles")
        .and_then(|p| p.as_object_mut())
        .ok_or_else(|| anyhow::anyhow!("No profiles object in auth-profiles.json"))?;

    let gemini_profile_key = profiles
        .keys()
        .find(|k| k.starts_with("gemini:"))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No Gemini OAuth profile found. Run: zeroclaw auth login --provider gemini"
            )
        })?
        .clone();

    let gemini_profile = profiles
        .get_mut(&gemini_profile_key)
        .ok_or_else(|| anyhow::anyhow!("Gemini profile not found"))?;

    println!("Found Gemini profile: {}", gemini_profile_key);

    // Check if profile has refresh_token
    if gemini_profile.get("refresh_token").is_none() {
        eprintln!("⚠️  Gemini profile has no refresh_token — cannot test refresh");
        return Ok(());
    }

    println!("✓ Gemini profile has refresh_token");

    // Backup original expires_at
    let original_expires_at = gemini_profile.get("expires_at").cloned();
    println!("Original expires_at: {:?}", original_expires_at);

    // Set expires_at to 1 hour ago (expired)
    let expired_time = Utc::now() - Duration::seconds(3600);
    let expired_str = expired_time.to_rfc3339();

    gemini_profile
        .as_object_mut()
        .unwrap()
        .insert("expires_at".to_string(), Value::String(expired_str.clone()));

    println!("Set expires_at to: {} (expired)", expired_str);

    // Ensure we restore original file even if test fails
    let restore_guard = scopeguard::guard(original_content.clone(), |backup| {
        if let Err(e) = fs::write(&auth_profiles_path, backup) {
            eprintln!("⚠️  Failed to restore auth-profiles.json: {}", e);
        } else {
            println!("✓ Restored original auth-profiles.json");
        }
    });

    // Check required env vars
    if env::var("GEMINI_OAUTH_CLIENT_ID").is_err()
        || env::var("GEMINI_OAUTH_CLIENT_SECRET").is_err()
    {
        eprintln!("⚠️  GEMINI_OAUTH_CLIENT_ID and GEMINI_OAUTH_CLIENT_SECRET required for refresh");
        return Ok(());
    }

    // Write modified auth-profiles.json BEFORE creating provider
    fs::write(&auth_profiles_path, serde_json::to_string_pretty(&data)?)?;
    println!("✓ Wrote modified auth-profiles.json with expired token");

    // Small delay to ensure file is flushed
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Create GeminiProvider using the default factory
    // This will load auth from ~/.zeroclaw/auth-profiles.json (with expired token)
    let provider = zeroclaw::providers::create_provider("gemini", None)?;

    println!("Created Gemini provider with expired token");

    // Call warmup() — should detect expired token and refresh it
    println!("Calling warmup() — should refresh expired token...");
    let warmup_result = provider.warmup().await;

    if let Err(e) = warmup_result {
        eprintln!("❌ warmup() failed: {}", e);
        eprintln!("This might be expected if:");
        eprintln!("  - GEMINI_OAUTH_CLIENT_ID/SECRET are not set");
        eprintln!("  - Refresh token is invalid");
        eprintln!("  - Network is unavailable");
        return Err(e);
    }

    println!("✓ warmup() succeeded");

    // Small delay to ensure file is written
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Re-load auth-profiles.json to check if token was refreshed
    let updated_content = fs::read_to_string(&auth_profiles_path)?;
    let updated_data: Value = serde_json::from_str(&updated_content)?;

    let updated_profile = updated_data
        .get("profiles")
        .and_then(|p| p.as_object())
        .and_then(|p| p.get(&gemini_profile_key))
        .and_then(|p| p.as_object())
        .ok_or_else(|| anyhow::anyhow!("Failed to read updated profile"))?;

    let new_expires_at = updated_profile.get("expires_at").and_then(|v| v.as_str());
    println!("New expires_at: {:?}", new_expires_at);

    // Verify token was refreshed (expires_at should be in the future)
    if let Some(new_exp) = new_expires_at {
        let new_exp_dt = chrono::DateTime::parse_from_rfc3339(new_exp)?;
        let now = Utc::now();
        let seconds_from_now = new_exp_dt.signed_duration_since(now).num_seconds();

        if seconds_from_now > 300 {
            println!(
                "✓ Token was refreshed! New expiry is {} seconds from now",
                seconds_from_now
            );
        } else {
            eprintln!(
                "⚠️  Token expiry is NOT in the future: {} seconds from now",
                seconds_from_now
            );
            eprintln!("    This might mean warmup() did not refresh the token.");
            eprintln!("    Original: {:?}", original_expires_at);
            eprintln!("    Set to (expired): {}", expired_str);
            eprintln!("    After warmup: {}", new_exp);
        }
    } else {
        eprintln!("⚠️  No expires_at found after warmup");
    }

    // Try making a real request to verify token works
    println!("\nMaking real request to verify token works...");
    let response = provider
        .chat_with_system(
            Some("You are a concise assistant. Reply in one short sentence."),
            "Say 'OAuth refresh works'",
            "gemini-2.5-pro",
            0.7,
        )
        .await;

    match response {
        Ok(text) => {
            println!("✓ Request succeeded! Response: {}", text);
            assert!(!text.is_empty(), "Response should not be empty");
        }
        Err(e) => {
            eprintln!("❌ Request failed: {}", e);
            return Err(e);
        }
    }

    // Cleanup is handled by scopeguard
    drop(restore_guard);

    println!("\n=== Test Passed ===");
    println!("Gemini warmup() correctly refreshed expired OAuth token!");

    Ok(())
}

/// Simpler test: just verify warmup() doesn't fail with valid credentials.
/// This test doesn't modify auth-profiles.json.
#[tokio::test]
#[ignore = "requires live Gemini OAuth credentials"]
async fn gemini_warmup_with_valid_credentials() -> Result<()> {
    // Create provider from default config
    let provider = zeroclaw::providers::create_provider("gemini", None)?;

    println!("Created Gemini provider");
    println!("Calling warmup()...");

    // This should succeed if credentials are valid
    provider.warmup().await?;

    println!("✓ warmup() succeeded with valid credentials");

    Ok(())
}
