//! Live model availability test for Gemini via OAuth.
//!
//! Uses real OAuth credentials from auth-profiles.json to verify
//! that each configured Gemini model actually works via cloudcode-pa.
//!
//! Run with:
//!   cargo test --test gemini_model_availability -- --ignored --nocapture
//!
//! Or via the helper script:
//!   ./dev/test_models.sh

use zeroclaw::providers::create_provider_with_options;
use zeroclaw::providers::traits::Provider;
use zeroclaw::providers::ProviderRuntimeOptions;

/// All Gemini models that should be available via OAuth.
/// Models available via OAuth (cloudcode-pa).
const GEMINI_MODELS: &[&str] = &[
    "gemini-3-pro-preview",
    "gemini-3-flash-preview",
    "gemini-2.5-pro",
    "gemini-2.5-flash",
    "gemini-2.5-flash-lite",
];

#[allow(dead_code)]
const GEMINI_MODELS_API_KEY_ONLY: &[&str] = &["gemini-3.1-pro-preview"];

/// Create a Gemini provider using managed OAuth from auth-profiles.json.
fn create_gemini_provider(profile: Option<&str>) -> Box<dyn Provider> {
    let mut options = ProviderRuntimeOptions::default();
    if let Some(p) = profile {
        options.auth_profile_override = Some(p.to_string());
    }

    create_provider_with_options("gemini", None, &options)
        .expect("Failed to create Gemini provider — check auth-profiles.json")
}

/// Test a single model with a minimal prompt.
async fn test_model(provider: &dyn Provider, model: &str) -> Result<String, String> {
    match provider
        .chat_with_system(Some("Reply with exactly one word: OK"), "test", model, 0.0)
        .await
    {
        Ok(response) => Ok(response),
        Err(e) => Err(format!("{e:#}")),
    }
}

#[tokio::test]
#[ignore] // Only run manually — requires live OAuth credentials
async fn gemini_models_available_via_oauth() {
    let provider = create_gemini_provider(None);

    let mut passed = 0;
    let mut failed = 0;

    for model in GEMINI_MODELS {
        eprint!("  Testing {model:40} ... ");
        match test_model(provider.as_ref(), model).await {
            Ok(resp) => {
                let preview: String = resp.chars().take(50).collect();
                eprintln!("✓  {preview}");
                passed += 1;
            }
            Err(e) => {
                // 429 means model exists but rate limited
                if e.contains("429") || e.contains("rate") || e.contains("Rate") {
                    eprintln!("⚠  rate limited (model exists)");
                    passed += 1;
                } else {
                    eprintln!("✗  {e}");
                    failed += 1;
                }
            }
        }
    }

    eprintln!(
        "\n  Results: {passed} passed, {failed} failed out of {} models",
        GEMINI_MODELS.len()
    );
    assert_eq!(failed, 0, "Some models failed — see output above");
}

#[tokio::test]
#[ignore]
async fn gemini_profiles_rotation_live() {
    // Test both profiles can authenticate
    for profile in &["gemini-1", "gemini-2"] {
        let provider = create_gemini_provider(Some(profile));
        eprint!("  Profile {profile:15} ... ");

        match test_model(provider.as_ref(), "gemini-2.5-flash").await {
            Ok(resp) => {
                let preview: String = resp.chars().take(30).collect();
                eprintln!("✓  {preview}");
            }
            Err(e) => {
                if e.contains("429") || e.contains("rate") {
                    eprintln!("⚠  rate limited (auth works)");
                } else {
                    panic!("Profile {profile} failed: {e}");
                }
            }
        }
    }
}
