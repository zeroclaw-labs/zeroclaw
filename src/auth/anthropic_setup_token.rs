//! Anthropic setup-token (paste) authentication flow.
//!
//! Allows users to authenticate by pasting an Anthropic OAuth setup-token
//! (`sk-ant-oat01-*` prefix). The token is validated and stored using the
//! existing `SecretStore` encrypted storage.
//!
//! This flow is intentionally simple: the heavy lifting of token issuance
//! happens in the Anthropic Claude CLI (`claude setup-token`), and we only
//! need to accept and verify the resulting token.

use anyhow::{Context, Result};

// ── Constants ───────────────────────────────────────────────────────

/// Expected prefix for Anthropic OAuth setup-tokens.
const SETUP_TOKEN_PREFIX: &str = "sk-ant-oat01-";

/// Anthropic API base URL for token validation.
const ANTHROPIC_API_URL: &str = "https://api.anthropic.com";

/// Anthropic beta header required for OAuth tokens.
const ANTHROPIC_BETA_OAUTH: &str = "oauth-2025-04-20";

// ── Public API ──────────────────────────────────────────────────────

/// Validate that a string looks like an Anthropic setup-token.
pub fn is_setup_token(token: &str) -> bool {
    token.starts_with(SETUP_TOKEN_PREFIX)
}

/// Validate that a string looks like any valid Anthropic credential
/// (either a regular API key or a setup-token).
pub fn is_anthropic_credential(token: &str) -> bool {
    token.starts_with("sk-ant-")
}

/// Run the interactive Anthropic setup-token paste flow.
///
/// Prompts the user to paste their setup-token, validates the format,
/// and optionally verifies it against the Anthropic API.
///
/// Returns the validated token string.
pub fn prompt_setup_token() -> Result<String> {
    println!();
    println!("  \u{1f510} Anthropic Setup Token");
    println!();
    println!("  To get a setup-token, run `claude setup-token` in another terminal.");
    println!("  Then paste the token (sk-ant-oat01-...) below.");
    println!();

    let token: String = dialoguer::Input::new()
        .with_prompt("  Paste your setup-token")
        .validate_with(|input: &String| -> Result<(), String> {
            let trimmed = input.trim();
            if trimmed.is_empty() {
                return Err("Token cannot be empty".to_string());
            }
            if !is_setup_token(trimmed) {
                return Err(format!(
                    "Invalid setup-token format. Expected prefix: {SETUP_TOKEN_PREFIX}"
                ));
            }
            Ok(())
        })
        .interact_text()
        .context("failed to read setup-token input")?;

    let token = token.trim().to_string();

    println!("  \u{2705} Setup-token format validated.");

    Ok(token)
}

/// Verify a setup-token against the Anthropic API (lightweight check).
///
/// Makes a minimal API request to confirm the token is accepted.
/// Returns `Ok(())` if valid, or an error describing the failure.
pub async fn verify_token(token: &str) -> Result<()> {
    let client = reqwest::Client::new();

    // Use a minimal messages request to validate the token.
    // We send an empty conversation that will fail with a validation error
    // (not an auth error) if the token is valid.
    let resp = client
        .post(format!("{ANTHROPIC_API_URL}/v1/messages"))
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-beta", ANTHROPIC_BETA_OAUTH)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .body(r#"{"model":"claude-sonnet-4-20250514","max_tokens":1,"messages":[{"role":"user","content":"ping"}]}"#)
        .send()
        .await
        .context("failed to reach Anthropic API for token verification")?;

    let status = resp.status();

    // 401/403 means the token is invalid or expired
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Setup-token rejected by Anthropic API ({status}): {body}");
    }

    // Any other response (including 200, 400 validation errors) means
    // the token was accepted for authentication
    Ok(())
}

/// Run the full interactive setup-token flow with optional verification.
///
/// Returns the validated (and optionally verified) token.
pub async fn login_anthropic_setup_token() -> Result<String> {
    let token = prompt_setup_token()?;

    // Ask if user wants to verify the token
    let should_verify = dialoguer::Confirm::new()
        .with_prompt("  Verify token against Anthropic API?")
        .default(true)
        .interact()
        .unwrap_or(false);

    if should_verify {
        println!("  Verifying...");
        match verify_token(&token).await {
            Ok(()) => {
                println!("  \u{2705} Token verified — authentication successful!");
            }
            Err(e) => {
                println!("  \u{26a0}\u{fe0f} Verification failed: {e}");
                println!("  The token will still be saved. You can try again later.");
            }
        }
    }

    Ok(token)
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_setup_token_recognizes_valid_prefix() {
        assert!(is_setup_token("sk-ant-oat01-abcdef1234567890"));
        assert!(is_setup_token("sk-ant-oat01-"));
    }

    #[test]
    fn is_setup_token_rejects_invalid_prefix() {
        assert!(!is_setup_token("sk-ant-api03-abcdef"));
        assert!(!is_setup_token("sk-1234567890"));
        assert!(!is_setup_token(""));
        assert!(!is_setup_token("not-a-token"));
    }

    #[test]
    fn is_anthropic_credential_covers_both_types() {
        assert!(is_anthropic_credential("sk-ant-oat01-setup"));
        assert!(is_anthropic_credential("sk-ant-api03-regular"));
        assert!(!is_anthropic_credential("sk-openai-1234"));
    }
}
