// Caller Match — incoming phone number → ontology Object lookup (v3.0 Section B)
//
// When a call comes in:
// 1. Normalize the number to E.164 format
// 2. Search ontology objects for matching phone_numbers in properties
// 3. Return the matched object (if any) for context injection

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::ontology::repo::OntologyRepo;
use crate::ontology::types::OntologyObject;

/// Result of a caller matching attempt.
#[derive(Debug, Clone, Serialize)]
pub struct CallerMatchResult {
    /// Matched ontology object (if found).
    pub object: Option<OntologyObject>,
    /// The normalized E.164 number used for matching.
    pub normalized_number: String,
    /// Whether this is a known contact.
    pub is_known: bool,
}

/// Normalize a phone number to E.164 format.
///
/// Handles common Korean number formats:
/// - `010-1234-5678` → `+821012345678`
/// - `01012345678` → `+821012345678`
/// - `+821012345678` → `+821012345678` (already E.164)
///
/// For non-Korean numbers, strips non-digit chars and prepends `+` if missing.
pub fn normalize_e164(number: &str, default_country_code: &str) -> String {
    // Strip all non-digit and non-+ characters
    let cleaned: String = number.chars().filter(|c| c.is_ascii_digit() || *c == '+').collect();

    if cleaned.is_empty() {
        return String::new();
    }

    // Already has country code
    if cleaned.starts_with('+') {
        return cleaned;
    }

    // Korean domestic format: starts with 0
    if cleaned.starts_with('0') {
        return format!("+{}{}", default_country_code, &cleaned[1..]);
    }

    // Assume it needs the country code prepended
    format!("+{}{}", default_country_code, cleaned)
}

/// Match an incoming caller number against ontology objects.
///
/// Searches the `properties.phone_numbers` JSON array of all objects
/// owned by the given user. Returns the first matching object.
pub fn match_caller(
    repo: &OntologyRepo,
    caller_number: &str,
    owner_user_id: &str,
    default_country_code: &str,
) -> Result<CallerMatchResult> {
    let normalized = normalize_e164(caller_number, default_country_code);
    if normalized.is_empty() {
        return Ok(CallerMatchResult {
            object: None,
            normalized_number: normalized,
            is_known: false,
        });
    }

    // Search ontology for objects containing this phone number.
    // We search with the raw digits to maximize FTS match chances.
    let digits: String = normalized.chars().filter(|c| c.is_ascii_digit()).collect();
    let search_query = if digits.len() >= 4 {
        // Use last 8 digits for matching (avoids country code mismatch)
        let suffix_start = digits.len().saturating_sub(8);
        digits[suffix_start..].to_string()
    } else {
        digits
    };

    let candidates = repo.search_objects(owner_user_id, None, &search_query, 20)?;

    // Check each candidate's properties for phone_numbers containing our number
    for obj in candidates {
        if object_has_phone_number(&obj, &normalized) {
            return Ok(CallerMatchResult {
                normalized_number: normalized,
                is_known: true,
                object: Some(obj),
            });
        }
    }

    Ok(CallerMatchResult {
        object: None,
        normalized_number: normalized,
        is_known: false,
    })
}

/// Check if an ontology object's properties contain a matching phone number.
fn object_has_phone_number(obj: &OntologyObject, normalized_e164: &str) -> bool {
    // Check properties.phone_numbers array
    if let Some(numbers) = obj.properties.get("phone_numbers") {
        if let Some(arr) = numbers.as_array() {
            for num in arr {
                if let Some(s) = num.as_str() {
                    if phone_numbers_equivalent(s, normalized_e164) {
                        return true;
                    }
                }
            }
        }
    }

    // Also check properties.phone (single value)
    if let Some(phone) = obj.properties.get("phone") {
        if let Some(s) = phone.as_str() {
            if phone_numbers_equivalent(s, normalized_e164) {
                return true;
            }
        }
    }

    false
}

/// Compare two phone numbers for equivalence, handling country code variations.
///
/// Numbers are considered equal if:
/// - Their digit-only forms are identical, OR
/// - The longer number's suffix equals the shorter number (stripping leading 0)
///
/// Examples of matches:
/// - `010-1234-5678` ≡ `+821012345678` (Korean domestic ↔ international)
/// - `+12125551234` ≡ `2125551234` (US with/without country code)
fn phone_numbers_equivalent(a: &str, b: &str) -> bool {
    let a_digits = normalize_simple(a);
    let b_digits = normalize_simple(b);

    if a_digits.is_empty() || b_digits.is_empty() {
        return false;
    }

    if a_digits == b_digits {
        return true;
    }

    // Compare the last 10 digits (standard phone number length for most regions).
    // If one has a leading '0' that the other lacks (common for Korean mobile),
    // strip it before suffix matching.
    let a_core = a_digits.strip_prefix('0').unwrap_or(&a_digits);
    let b_core = b_digits.strip_prefix('0').unwrap_or(&b_digits);

    let suffix_len = a_core.len().min(b_core.len()).min(10);
    if suffix_len < 7 {
        return false; // too short to be meaningful
    }

    let a_suffix = &a_core[a_core.len() - suffix_len..];
    let b_suffix = &b_core[b_core.len() - suffix_len..];
    a_suffix == b_suffix
}

/// Strip everything except digits for comparison.
fn normalize_simple(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e164_korean_dashed() {
        assert_eq!(normalize_e164("010-1234-5678", "82"), "+821012345678");
    }

    #[test]
    fn e164_korean_no_dash() {
        assert_eq!(normalize_e164("01012345678", "82"), "+821012345678");
    }

    #[test]
    fn e164_already_international() {
        assert_eq!(normalize_e164("+821012345678", "82"), "+821012345678");
    }

    #[test]
    fn e164_us_format() {
        assert_eq!(normalize_e164("2125551234", "1"), "+12125551234");
    }

    #[test]
    fn e164_empty() {
        assert_eq!(normalize_e164("", "82"), "");
    }

    #[test]
    fn e164_with_spaces_and_parens() {
        assert_eq!(normalize_e164("(010) 1234 5678", "82"), "+821012345678");
    }

    #[test]
    fn normalize_simple_strips_non_digits() {
        assert_eq!(normalize_simple("+82-10-1234-5678"), "821012345678");
        assert_eq!(normalize_simple("01012345678"), "01012345678");
    }

    #[test]
    fn object_phone_match_array() {
        let obj = OntologyObject {
            id: 1,
            type_id: 1,
            title: Some("Test Person".to_string()),
            properties: serde_json::json!({
                "phone_numbers": ["+821012345678", "+821098765432"]
            }),
            owner_user_id: "user1".to_string(),
            created_at: 0,
            updated_at: 0,
        };
        assert!(object_has_phone_number(&obj, "+821012345678"));
        assert!(!object_has_phone_number(&obj, "+821011111111"));
    }

    #[test]
    fn object_phone_match_single() {
        let obj = OntologyObject {
            id: 1,
            type_id: 1,
            title: Some("Office".to_string()),
            properties: serde_json::json!({
                "phone": "010-1234-5678"
            }),
            owner_user_id: "user1".to_string(),
            created_at: 0,
            updated_at: 0,
        };
        assert!(object_has_phone_number(&obj, "+821012345678"));
    }

    #[test]
    fn object_no_phone_returns_false() {
        let obj = OntologyObject {
            id: 1,
            type_id: 1,
            title: Some("No Phone".to_string()),
            properties: serde_json::json!({}),
            owner_user_id: "user1".to_string(),
            created_at: 0,
            updated_at: 0,
        };
        assert!(!object_has_phone_number(&obj, "+821012345678"));
    }
}
