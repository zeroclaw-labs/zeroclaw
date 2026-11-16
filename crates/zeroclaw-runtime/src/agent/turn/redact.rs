//! Credential redaction for the rendering layer (logs, observer events, and
//! UI-facing turn events). The success data path is untouched: a successful
//! tool result fed back to the model and signed by an HMAC receipt carries
//! raw bytes. The one data-path exception is the failure arms of
//! `execute_one_tool`, which fold a tool's detailed error body into the
//! model-visible text and scrub that combined string first, so a reflected
//! token in a 4xx/5xx body is not forwarded to the model provider.

use regex::Regex;
use std::sync::LazyLock;

/// A credential-named key and its value. An HTTP auth scheme word (`Bearer`,
/// `Basic`) between the separator and the value is kept and the token after it
/// redacted. Values under 8 characters are left alone so that short ordinary
/// values (`token: none`) stay readable.
pub(crate) static SENSITIVE_KV_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(authorization|token|api[_-]?key|password|secret|user[_-]?key|bearer|credential|set[_-]?cookie|cookie)["']?\s*[:=]\s*(?P<scheme>(?:bearer|basic)\s+)?(?:"([^"]{8,})"|'([^']{8,})'|([a-zA-Z0-9_\-\./+=]{8,}))"#)
        .expect("static sensitive key-value regex must compile")
});

/// The password in a URL's userinfo, `scheme://user:password@host`. Its place
/// alone marks it as a credential, so it is redacted whatever its length; the
/// user and host stay.
pub(crate) static URL_USERINFO_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b([a-z][a-z0-9+.\-]*://[^\s:/@"'<>]*:)[^\s/@"'<>]+@"#)
        .expect("static URL userinfo regex must compile")
});

/// Query parameters that carry a credential under a name the key regex above
/// does not catch: API keys passed as `key=`, and URL signatures. Matched only
/// right after `?` or `&`, so free text such as `monkey: bananas123` is left
/// alone, and only these names, so `?page=2` stays readable. Values under 8
/// characters are left alone, as for the key regex.
pub(crate) static CREDENTIAL_QUERY_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)([?&](?:key|sig|signature|x-amz-signature|x-goog-signature)=)[^&#\s"'<>()\[\]{}]{8,}"#)
        .expect("static credential query regex must compile")
});

pub fn scrub_credentials(input: &str) -> String {
    let scrubbed = SENSITIVE_KV_REGEX
        .replace_all(input, |caps: &regex::Captures| {
            let full_match = &caps[0];
            let key = &caps[1];
            let scheme = caps.name("scheme").map_or("", |m| m.as_str());
            let val = caps
                .get(3)
                .or(caps.get(4))
                .or(caps.get(5))
                .map(|m| m.as_str())
                .unwrap_or("");

            // Preserve first 4 chars for context, then redact.
            // Use char_indices to find the byte offset of the 4th character
            // so we never slice in the middle of a multi-byte UTF-8 sequence.
            let prefix = if val.len() > 4 {
                val.char_indices()
                    .nth(4)
                    .map(|(byte_idx, _)| &val[..byte_idx])
                    .unwrap_or(val)
            } else {
                ""
            };
            let prefix = format!("{scheme}{prefix}");

            if full_match.contains(':') {
                if full_match.contains('"') {
                    format!("\"{}\": \"{}*[REDACTED]\"", key, prefix)
                } else {
                    format!("{}: {}*[REDACTED]", key, prefix)
                }
            } else if full_match.contains('=') {
                if full_match.contains('"') {
                    format!("{}=\"{}*[REDACTED]\"", key, prefix)
                } else {
                    format!("{}={}*[REDACTED]", key, prefix)
                }
            } else {
                format!("{}: {}*[REDACTED]", key, prefix)
            }
        })
        .to_string();
    let scrubbed = URL_USERINFO_REGEX.replace_all(&scrubbed, "${1}[REDACTED]@");
    CREDENTIAL_QUERY_REGEX
        .replace_all(&scrubbed, "${1}[REDACTED]")
        .into_owned()
}

pub(crate) static SENSITIVE_KEY_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(authorization|token|api[_-]?key|password|secret|user[_-]?key|bearer|credential|set[_-]?cookie|cookie)"#)
        .expect("static sensitive-key regex must compile")
});
const REDACTED_CREDENTIAL_VALUE: &str = "[REDACTED]";

/// Whether a JSON key denotes a credential value on a human-facing surface.
///
/// Keep every rendering boundary on this classifier so a top-level scalar does
/// not lose the credential context before structured redaction can inspect it.
pub fn is_credential_key(key: &str) -> bool {
    crate::approval::looks_like_secret_key(key) || SENSITIVE_KEY_REGEX.is_match(key)
}

/// Structured-aware credential scrub for a JSON value bound for a human-facing
/// surface. Object entries whose key names a credential have their entire value
/// replaced with a marker, preserving the key. Every other string leaf still
/// runs through the text [`scrub_credentials`] so inline `token=...` patterns
/// inside unrelated fields are caught too. Serialize-then-scrub would corrupt
/// key names that merely contain a sensitive word (e.g. `access_token`), so this
/// walks the value instead. Same rendering-boundary contract as
/// [`scrub_credentials`].
pub fn scrub_credentials_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let scrubbed = map
                .into_iter()
                .map(|(key, val)| {
                    if is_credential_key(&key) {
                        (
                            key,
                            serde_json::Value::String(REDACTED_CREDENTIAL_VALUE.to_string()),
                        )
                    } else {
                        (key, scrub_credentials_value(val))
                    }
                })
                .collect();
            serde_json::Value::Object(scrubbed)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(scrub_credentials_value).collect())
        }
        serde_json::Value::String(s) => serde_json::Value::String(scrub_credentials(&s)),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_credential_key, scrub_credentials, scrub_credentials_value};

    #[test]
    fn credential_key_classification_combines_shared_secret_policies() {
        for key in ["private_key", "api_key", "cookie", "set-cookie", "user-key"] {
            assert!(is_credential_key(key), "expected credential key: {key}");
        }
        assert!(!is_credential_key("status"));
    }

    /// The shape the RPC run-detail response leans on: tool arguments are
    /// arbitrary JSON, so a secret under a credential-named key is routinely
    /// carried by a descendant that names nothing sensitive. A key-based walk
    /// that recursed would leave every one of these intact. Sibling coverage in
    /// `scrub_credentials_value_replaces_every_credential_value_shape` takes the
    /// nested-object, numeric, and boolean shapes; this one adds the array and
    /// `null` cases and asserts no seeded secret reaches the wire.
    #[test]
    fn a_credential_key_redacts_its_value_whatever_the_shape() {
        let value = serde_json::json!({
            "api_key": {"value": "NESTEDSECRET"},
            "token": ["ARRAYSECRET"],
            "password": 12345678,
            "secret": true,
            "credential": null,
        });

        let scrubbed = scrub_credentials_value(value);
        let wire = serde_json::to_string(&scrubbed).expect("serializable");

        for secret in ["NESTEDSECRET", "ARRAYSECRET", "12345678"] {
            assert!(
                !wire.contains(secret),
                "{secret} must not survive scrubbing"
            );
        }
        for key in ["api_key", "token", "password", "secret"] {
            assert_eq!(scrubbed[key], "[REDACTED]", "{key} must be replaced whole");
        }
        // Even `null` is replaced rather than preserved: the marker is what
        // tells a reader the key was withheld, and a surviving `null` would
        // read as a value the run genuinely had.
        assert_eq!(scrubbed["credential"], "[REDACTED]");
    }

    #[test]
    fn scrub_credentials_value_redacts_nested_secret_and_keeps_key() {
        let input = serde_json::json!({
            "body": {"access_token": "sk-live-abcdef0123456789", "status": "ok"},
            "count": 3
        });
        let out = scrub_credentials_value(input);
        let token = out["body"]["access_token"].as_str().unwrap();
        assert!(token.contains("[REDACTED]"));
        assert!(!token.contains("abcdef0123456789"));
        assert_eq!(out["body"]["status"], "ok");
        assert_eq!(out["count"], 3);
    }

    #[test]
    fn scrub_credentials_value_redacts_authorization_and_cookie_keys() {
        let input = serde_json::json!({
            "body": {
                "authorization": "Bearer sk-live-abcdef0123456789",
                "cookie": "session=deadbeefcafebabe0123",
                "set-cookie": "sid=9f8e7d6c5b4a3210feed",
                "status": "ok"
            }
        });
        let out = scrub_credentials_value(input);
        let authorization = out["body"]["authorization"].as_str().unwrap();
        assert!(authorization.contains("[REDACTED]"));
        assert!(!authorization.contains("sk-live-abcdef0123456789"));
        let cookie = out["body"]["cookie"].as_str().unwrap();
        assert!(cookie.contains("[REDACTED]"));
        assert!(!cookie.contains("deadbeefcafebabe0123"));
        let set_cookie = out["body"]["set-cookie"].as_str().unwrap();
        assert!(set_cookie.contains("[REDACTED]"));
        assert!(!set_cookie.contains("9f8e7d6c5b4a3210feed"));
        assert_eq!(out["body"]["status"], "ok");
    }

    #[test]
    fn scrub_credentials_value_matches_summary_secret_key_policy() {
        let input = serde_json::json!({"body": {"private_key": "tiny"}});
        let out = scrub_credentials_value(input);

        assert_eq!(out["body"]["private_key"], "[REDACTED]");
        assert_ne!(out["body"]["private_key"], "tiny");
    }

    #[test]
    fn scrub_credentials_value_replaces_every_credential_value_shape() {
        let input = serde_json::json!({
            "body": {
                "numeric_token": 1234,
                "credentials": {"value": "tiny"},
                "auth_enabled": true,
                "status": "ok"
            }
        });
        let out = scrub_credentials_value(input);

        for key in ["numeric_token", "credentials", "auth_enabled"] {
            assert_eq!(out["body"][key], "[REDACTED]");
        }
        assert_eq!(out["body"]["status"], "ok");
    }

    #[test]
    fn scrub_credentials_redacts_unquoted_base64_credential_values() {
        let input = "token=QWxh+GRpbjpvcGVu/IHNlc2FtZQ== next=public";
        let scrubbed = scrub_credentials(input);

        assert_eq!(scrubbed, "token=QWxh*[REDACTED] next=public");
        assert!(!scrubbed.contains("IHNlc2FtZQ"));
        assert!(!scrubbed.contains("=="));
    }

    #[test]
    fn scrub_credentials_redacts_quoted_base64_credential_values() {
        let input = r#"secret="QWxhZGRpbjpvcGVu/IHNlc2FtZQ==""#;
        let scrubbed = scrub_credentials(input);

        assert_eq!(scrubbed, r#"secret="QWxh*[REDACTED]""#);
        assert!(!scrubbed.contains("IHNlc2FtZQ"));
        assert!(!scrubbed.contains("=="));
    }

    #[test]
    fn scrub_credentials_redacts_token_after_auth_scheme() {
        let bearer = "curl -H 'Authorization: Bearer abcdefghijklmnop1234' https://x";
        assert_eq!(
            scrub_credentials(bearer),
            "curl -H 'Authorization: Bearer abcd*[REDACTED]' https://x"
        );
        let basic = "Authorization: Basic dXNlcjpzM2NyZXQ=";
        assert_eq!(
            scrub_credentials(basic),
            "Authorization: Basic dXNl*[REDACTED]"
        );
    }

    #[test]
    fn scrub_credentials_redacts_url_password_and_keeps_user_and_host() {
        let input = "git clone https://alice:hunter2@example.com/repo.git";
        assert_eq!(
            scrub_credentials(input),
            "git clone https://alice:[REDACTED]@example.com/repo.git"
        );
        let dsn = "redis://:s3cret@h:6379";
        assert_eq!(scrub_credentials(dsn), dsn.replace("s3cret", "[REDACTED]"));
        for untouched in [
            "ssh://git@example.com:22/repo",
            "http://example.com:8080/a@b",
            "http://[::1]:8080/",
            "mail user@host or mailto:ops@example.com",
            r#"{"url":"http://api.example.com:8443","contact":"ops@example.com"}"#,
        ] {
            assert_eq!(scrub_credentials(untouched), untouched);
        }
    }

    #[test]
    fn scrub_credentials_redacts_credential_query_keys_only() {
        let input = "https://x.test/v1?key=AIzaSyA1b2c3d4e5&page=2&sig=0a1b2c3d4e5f6789";
        assert_eq!(
            scrub_credentials(input),
            "https://x.test/v1?key=[REDACTED]&page=2&sig=[REDACTED]"
        );
        let signed = "https://b.test/o?X-Amz-Signature=deadbeefcafe0123&signature=f00dfeed99";
        assert_eq!(
            scrub_credentials(signed),
            "https://b.test/o?X-Amz-Signature=[REDACTED]&signature=[REDACTED]"
        );
        let j = r#"{"url":"https://h/?key=12345678","next":2}"#;
        assert_eq!(scrub_credentials(j), j.replace("12345678", "[REDACTED]"));
    }

    #[test]
    fn scrub_credentials_leaves_ordinary_and_short_values_alone() {
        for input in [
            "https://x.test/list?page=2&per_page=50",
            "monkey: bananas123",
            "https://x.test/?monkey=bananas12345",
            "https://x.test/?keyboard=qwertyuiop",
            "https://x.test/?key=short&sig=abc",
            "Authorization: Bearer short",
            "token: none",
        ] {
            assert_eq!(scrub_credentials(input), input);
        }
    }

    #[test]
    fn scrub_credentials_value_redacts_new_forms_inside_unrelated_keys() {
        let input = serde_json::json!({
            "cmd": "curl -H 'Authorization: Bearer abcdefghijklmnop1234' https://u:p4ss@h.test/?sig=0a1b2c3d4e5f6789&page=2",
            "url": "https://h.test/?key=AIzaSyA1b2c3d4e5"
        });
        let out = scrub_credentials_value(input);
        assert_eq!(
            out["cmd"],
            "curl -H 'Authorization: Bearer abcd*[REDACTED]' https://u:[REDACTED]@h.test/?sig=[REDACTED]&page=2"
        );
        assert_eq!(out["url"], "https://h.test/?key=[REDACTED]");
    }
}
