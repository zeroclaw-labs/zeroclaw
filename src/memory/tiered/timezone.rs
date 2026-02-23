//! Timezone resolution via LLM.
//!
//! Given a user-provided location string (e.g. "Cebu, Philippines"), resolves
//! it to an IANA timezone identifier (e.g. "Asia/Manila") by querying an LLM.
//!
//! Resolution is designed to be non-blocking: the caller can use the system
//! timezone as an immediate fallback while the LLM runs in the background with
//! a 2–3 second timeout.

use std::time::Duration;

use chrono_tz::Tz;
use serde_json::json;
use tokio::time::timeout;

/// Sanitize a user-provided location string before sending to the LLM.
///
/// - Trims leading/trailing whitespace.
/// - Strips ASCII control characters (0x00–0x1F, 0x7F).
/// - Caps the result at 256 characters.
pub fn sanitize_location(input: &str) -> String {
    let trimmed = input.trim();
    let cleaned: String = trimmed.chars().filter(|c| !c.is_ascii_control()).collect();
    if cleaned.len() <= 256 {
        cleaned
    } else {
        // Truncate at a char boundary that is <= 256 bytes.
        let mut end = 256;
        while !cleaned.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        cleaned[..end].to_string()
    }
}

/// Returns `true` if `tz` matches a known IANA timezone identifier in
/// `chrono-tz`.
pub fn is_valid_iana_tz(tz: &str) -> bool {
    tz.parse::<Tz>().is_ok()
}

/// Resolve a user location to an IANA timezone via an LLM call.
///
/// # Arguments
///
/// * `location` – free-form location string (e.g. "Cebu, Philippines").
/// * `llm_call` – an async closure that accepts a prompt string and returns
///   the LLM's text response. This abstraction lets callers plug in any
///   provider without pulling in the full `Provider` trait.
///
/// # Returns
///
/// `Some(iana_tz)` if the LLM returns a valid IANA timezone within the
/// timeout window, or `None` on failure / timeout / invalid response.
///
/// # Design
///
/// - Input is sanitized via [`sanitize_location`] and embedded in a JSON
///   data field to prevent prompt injection.
/// - The LLM response is validated against `chrono-tz` before being returned.
/// - A 3-second timeout prevents blocking if the LLM is slow.
pub async fn resolve_timezone_via_llm<F, Fut>(location: &str, llm_call: F) -> Option<String>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, anyhow::Error>>,
{
    let sanitized = sanitize_location(location);
    if sanitized.is_empty() {
        return None;
    }

    // Build prompt as a JSON object so user-supplied data cannot escape into
    // the instruction text.
    let request_payload = json!({
        "instruction": "Given the location in the 'data' field, respond with ONLY the IANA timezone identifier (e.g. 'Asia/Manila'). Do not include any other text.",
        "data": sanitized,
    });

    let prompt = request_payload.to_string();

    let result = timeout(Duration::from_secs(3), llm_call(prompt)).await;

    match result {
        Ok(Ok(response)) => {
            let candidate = response.trim().to_string();
            if is_valid_iana_tz(&candidate) {
                Some(candidate)
            } else {
                None
            }
        }
        Ok(Err(_)) => None, // LLM call failed
        Err(_) => None,     // Timeout
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_control_chars() {
        let input = "Cebu\x00, Philippines\x01";
        let result = sanitize_location(input);
        assert!(!result.contains('\x00'));
        assert!(!result.contains('\x01'));
        assert!(result.contains("Cebu"));
        assert!(result.contains("Philippines"));
    }

    #[test]
    fn sanitize_caps_length() {
        let long = "a".repeat(500);
        assert!(sanitize_location(&long).len() <= 256);
    }

    #[test]
    fn sanitize_trims_whitespace() {
        assert_eq!(sanitize_location("  Tokyo  "), "Tokyo");
    }

    #[test]
    fn sanitize_preserves_normal_input() {
        assert_eq!(sanitize_location("New York, USA"), "New York, USA");
    }

    #[test]
    fn sanitize_empty_string() {
        assert_eq!(sanitize_location(""), "");
        assert_eq!(sanitize_location("   "), "");
    }

    #[test]
    fn valid_iana_accepts_known_timezones() {
        assert!(is_valid_iana_tz("Asia/Manila"));
        assert!(is_valid_iana_tz("America/New_York"));
        assert!(is_valid_iana_tz("Europe/London"));
        assert!(is_valid_iana_tz("UTC"));
    }

    #[test]
    fn valid_iana_rejects_garbage() {
        assert!(!is_valid_iana_tz("not-a-timezone"));
        assert!(!is_valid_iana_tz(""));
        assert!(!is_valid_iana_tz("Cebu Philippines")); // location, not tz
    }

    #[tokio::test]
    async fn resolve_returns_none_on_empty_location() {
        // No LLM call needed for empty input — short-circuits immediately.
        let result = resolve_timezone_via_llm("", |_prompt| async {
            panic!("LLM should not be called for empty input");
        })
        .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn resolve_returns_none_on_whitespace_only_location() {
        let result = resolve_timezone_via_llm("   ", |_prompt| async {
            panic!("LLM should not be called for whitespace-only input");
        })
        .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn resolve_returns_valid_tz_from_llm() {
        let result = resolve_timezone_via_llm("Cebu, Philippines", |_prompt| async {
            Ok("Asia/Manila".to_string())
        })
        .await;
        assert_eq!(result, Some("Asia/Manila".to_string()));
    }

    #[tokio::test]
    async fn resolve_returns_none_for_invalid_llm_response() {
        let result = resolve_timezone_via_llm("SomePlace", |_prompt| async {
            Ok("NotATimezone/Garbage".to_string())
        })
        .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn resolve_returns_none_on_llm_error() {
        let result = resolve_timezone_via_llm("Tokyo", |_prompt| async {
            Err(anyhow::anyhow!("LLM service unavailable"))
        })
        .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn resolve_returns_none_on_timeout() {
        let result = resolve_timezone_via_llm("London", |_prompt| async {
            // Simulate a slow LLM that exceeds the 3s timeout.
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok("Europe/London".to_string())
        })
        .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn resolve_trims_llm_response_whitespace() {
        let result = resolve_timezone_via_llm("Tokyo", |_prompt| async {
            Ok("  Asia/Tokyo  \n".to_string())
        })
        .await;
        assert_eq!(result, Some("Asia/Tokyo".to_string()));
    }

    #[tokio::test]
    async fn resolve_sends_sanitized_input_as_json() {
        let result = resolve_timezone_via_llm("Cebu\x00, Philippines", |prompt| async move {
            // Verify the prompt is valid JSON and contains sanitized data.
            let parsed: serde_json::Value =
                serde_json::from_str(&prompt).expect("prompt should be valid JSON");
            let data = parsed["data"].as_str().unwrap();
            assert!(!data.contains('\x00'), "control chars should be stripped");
            assert!(data.contains("Cebu"), "location should be preserved");
            Ok("Asia/Manila".to_string())
        })
        .await;
        assert_eq!(result, Some("Asia/Manila".to_string()));
    }
}
