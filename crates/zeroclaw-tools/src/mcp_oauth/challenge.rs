//! Tolerant parser for `WWW-Authenticate: Bearer` challenges (RFC 7235 /
//! RFC 6750), specifically for extracting the RFC 9728 `resource_metadata`
//! parameter an MCP server advertises with a 401.
//!
//! The parser is deliberately robust because the challenge is produced by
//! third-party servers: the scheme name is case-insensitive, parameters appear
//! in arbitrary order, unknown parameters are preserved rather than rejected,
//! quoted strings support backslash escapes, and a malformed challenge yields
//! `None` (the caller then falls back to the RFC 9728 default discovery URL)
//! rather than an error.

use std::collections::BTreeMap;

/// One parsed `Bearer` challenge. Field names follow RFC 6750 §3; everything
/// else (including `resource_metadata` when unexpected duplicates appear) is
/// preserved in `other` under its lowercased parameter name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BearerChallenge {
    pub realm: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
    pub scope: Option<String>,
    /// RFC 9728 §5.1: URL of the protected-resource metadata document.
    pub resource_metadata: Option<String>,
    pub other: BTreeMap<String, String>,
}

/// Extract the first `Bearer` challenge from a `WWW-Authenticate` header
/// value. Returns `None` when no bearer challenge is present or when the
/// bearer challenge itself is malformed.
///
/// Challenge parameters are comma-separated *and* multiple challenges are
/// comma-separated, so the boundary between "another parameter" and "another
/// challenge" is decided per segment: a continuation segment that opens with
/// a bare token followed by whitespace (e.g. `Basic realm=...`) starts the
/// next challenge and ends the bearer one.
pub fn parse_bearer_challenge(header: &str) -> Option<BearerChallenge> {
    let segments = split_top_level_commas(header.trim());
    let mut challenge: Option<BearerChallenge> = None;
    for segment in segments {
        let segment = segment.trim();
        let params = match challenge {
            None => {
                // Searching for the bearer scheme. Byte-level comparison keeps
                // this panic-free on non-ASCII input (the header may arrive
                // via lossy UTF-8 decoding), and the scheme must be a complete
                // token: `BearerX realm=a` is not a bearer challenge.
                const SCHEME: &[u8] = b"bearer";
                let bytes = segment.as_bytes();
                if bytes.len() >= SCHEME.len() && bytes[..SCHEME.len()].eq_ignore_ascii_case(SCHEME)
                {
                    let tail = &segment[SCHEME.len()..];
                    if tail.is_empty() || tail.starts_with(char::is_whitespace) {
                        tail.trim()
                    } else {
                        continue;
                    }
                } else {
                    continue;
                }
            }
            Some(_) => {
                if opens_new_challenge(segment) {
                    break;
                }
                segment
            }
        };
        let sink = challenge.get_or_insert_with(BearerChallenge::default);
        if !parse_params_into(params, sink) {
            return None;
        }
    }
    challenge
}

/// True when a segment opens with a bare token followed by whitespace
/// *and that whitespace is not followed by `=`*, i.e. the shape of a new
/// `<scheme> <params>` challenge rather than a bearer parameter. Loose
/// servers do emit `name = value` with optional whitespace around `=`
/// (RFC 7235 forbids it but ships it), which must stay a parameter.
fn opens_new_challenge(segment: &str) -> bool {
    let Some((token, tail)) = segment.split_once(char::is_whitespace) else {
        return false;
    };
    if token.is_empty() || !token.bytes().all(is_token_char) {
        return false;
    }
    let tail = tail.trim_start();
    !tail.is_empty() && !tail.starts_with('=')
}

/// Parse the `token [= value]` parameter list of one segment into `sink`.
/// Returns false on structural garbage (empty name, empty unquoted value,
/// unterminated quote).
fn parse_params_into(input: &str, sink: &mut BearerChallenge) -> bool {
    let bytes = input.as_bytes();
    let mut i = 0;
    loop {
        i = skip_whitespace(bytes, i);
        if i >= bytes.len() {
            return true;
        }
        let name_start = i;
        while i < bytes.len() && is_token_char(bytes[i]) {
            i += 1;
        }
        if i == name_start {
            return false;
        }
        let name = input[name_start..i].to_ascii_lowercase();
        i = skip_whitespace(bytes, i);
        let value = if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            i = skip_whitespace(bytes, i);
            if i >= bytes.len() {
                return false;
            }
            if bytes[i] == b'"' {
                match parse_quoted(input, &mut i) {
                    Some(value) => value,
                    None => return false,
                }
            } else {
                let value_start = i;
                while i < bytes.len() && is_token_char(bytes[i]) {
                    i += 1;
                }
                if i == value_start {
                    return false;
                }
                input[value_start..i].to_string()
            }
        } else {
            // Parameter without a value; tolerate and record as empty.
            String::new()
        };
        sink.insert(name, value);
    }
}

/// Split on commas that sit outside quoted strings (quoted parameter values
/// may legitimately contain commas).
fn split_top_level_commas(input: &str) -> Vec<&str> {
    let bytes = input.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    let mut escaped = false;
    for (i, byte) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' if in_quotes => escaped = true,
            b'"' => in_quotes = !in_quotes,
            b',' if !in_quotes => {
                segments.push(&input[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    segments.push(&input[start..]);
    segments
}

impl BearerChallenge {
    fn insert(&mut self, name: String, value: String) {
        // First occurrence of a duplicated parameter wins.
        let slot = match name.as_str() {
            "realm" => &mut self.realm,
            "error" => &mut self.error,
            "error_description" => &mut self.error_description,
            "scope" => &mut self.scope,
            "resource_metadata" => &mut self.resource_metadata,
            _ => {
                self.other.entry(name).or_insert_with(|| value.clone());
                return;
            }
        };
        if slot.is_none() {
            *slot = Some(value);
        }
    }
}

fn parse_quoted(input: &str, i: &mut usize) -> Option<String> {
    let bytes = input.as_bytes();
    debug_assert_eq!(bytes[*i], b'"');
    *i += 1;
    let mut out = String::new();
    while *i < bytes.len() {
        match bytes[*i] {
            b'\\' if *i + 1 < bytes.len() => {
                *i += 1;
                out.push(input[*i..].chars().next()?);
                // Advance by the byte length of the escaped character (it may
                // be non-ASCII inside the quoted string).
                *i += input[*i..].chars().next()?.len_utf8();
            }
            b'"' => {
                *i += 1;
                return Some(out);
            }
            _ => {
                let ch = input[*i..].chars().next()?;
                out.push(ch);
                *i += ch.len_utf8();
            }
        }
    }
    None
}

fn skip_whitespace(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// RFC 7230 `tchar`.
fn is_token_char(byte: u8) -> bool {
    matches!(byte,
        b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.'
        | b'^' | b'_' | b'`' | b'|' | b'~' | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z'
    )
}

/// Pick the `WWW-Authenticate` field of a response that carries the OAuth
/// challenge. A server advertising several schemes may send one field per
/// scheme (RFC 9110 §8.6.1 allows that as readily as a comma-separated
/// list), and the first is then often a `Basic` challenge while the Bearer
/// parameters naming this resource's authorization server sit in a later
/// field; reading only the first drops the `resource_metadata` hint that
/// lets discovery reach a login service at a custom URL. Every field goes
/// through [`parse_bearer_challenge`], so a field that packs several
/// challenges stays handled. When no field carries a Bearer challenge the
/// first one is returned unchanged, so a caller sees the answer it sees
/// today.
pub fn pick_bearer_challenge(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let mut first: Option<&str> = None;
    for value in headers
        .get_all(reqwest::header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|value| value.to_str().ok())
    {
        if first.is_none() {
            first = Some(value);
        }
        if parse_bearer_challenge(value).is_some() {
            return Some(value.to_string());
        }
    }
    first.map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_field_that_carries_the_bearer_challenge() {
        // A server may advertise one scheme per field, and the Bearer
        // parameters that make discovery work are then in the second field.
        let mut headers = reqwest::header::HeaderMap::new();
        headers.append(
            reqwest::header::WWW_AUTHENTICATE,
            reqwest::header::HeaderValue::from_static("Basic realm=\"mcp\""),
        );
        headers.append(
            reqwest::header::WWW_AUTHENTICATE,
            reqwest::header::HeaderValue::from_static(
                "Bearer realm=\"mcp\", resource_metadata=\"https://as.example/prm\"",
            ),
        );
        let picked = pick_bearer_challenge(&headers).expect("a bearer field");
        assert_eq!(
            picked,
            "Bearer realm=\"mcp\", resource_metadata=\"https://as.example/prm\""
        );
        assert_eq!(
            parse_bearer_challenge(&picked)
                .expect("the picked field parses")
                .resource_metadata
                .as_deref(),
            Some("https://as.example/prm"),
            "the picked field is the one carrying the hint discovery needs"
        );
    }

    #[test]
    fn keeps_the_first_field_when_no_bearer_challenge_is_present() {
        // A non-Bearer answer is still the answer the server gave: the caller
        // reports it rather than inventing a challenge.
        let mut headers = reqwest::header::HeaderMap::new();
        headers.append(
            reqwest::header::WWW_AUTHENTICATE,
            reqwest::header::HeaderValue::from_static("Basic realm=\"mcp\""),
        );
        headers.append(
            reqwest::header::WWW_AUTHENTICATE,
            reqwest::header::HeaderValue::from_static("Negotiate"),
        );
        assert_eq!(
            pick_bearer_challenge(&headers).as_deref(),
            Some("Basic realm=\"mcp\"")
        );
        assert_eq!(
            pick_bearer_challenge(&reqwest::header::HeaderMap::new()),
            None,
            "no challenge header is not a challenge"
        );
    }

    #[test]
    fn parses_typical_mcp_401_challenge() {
        let header = r#"Bearer realm="mcp", error="invalid_token", error_description="The access token expired", resource_metadata="https://mcp.silpo.ua/.well-known/oauth-protected-resource/mcp""#;
        let challenge = parse_bearer_challenge(header).expect("bearer challenge");
        assert_eq!(challenge.realm.as_deref(), Some("mcp"));
        assert_eq!(challenge.error.as_deref(), Some("invalid_token"));
        assert_eq!(
            challenge.error_description.as_deref(),
            Some("The access token expired")
        );
        assert_eq!(
            challenge.resource_metadata.as_deref(),
            Some("https://mcp.silpo.ua/.well-known/oauth-protected-resource/mcp")
        );
        assert!(challenge.other.is_empty());
    }

    #[test]
    fn scheme_case_and_parameter_order_are_irrelevant() {
        let header = r#"BEARER resource_metadata="https://x/.well-known/oauth-protected-resource/mcp" realm=a"#;
        let challenge = parse_bearer_challenge(header).expect("bearer challenge");
        assert_eq!(
            challenge.resource_metadata.as_deref(),
            Some("https://x/.well-known/oauth-protected-resource/mcp")
        );
        assert_eq!(challenge.realm.as_deref(), Some("a"));
    }

    #[test]
    fn unquoted_token_values_accepted() {
        let challenge =
            parse_bearer_challenge("Bearer error=invalid_token, max_age=3600").expect("parses");
        assert_eq!(challenge.error.as_deref(), Some("invalid_token"));
        assert_eq!(
            challenge.other.get("max_age").map(String::as_str),
            Some("3600")
        );
    }

    #[test]
    fn unknown_parameters_are_preserved() {
        let challenge = parse_bearer_challenge(r#"Bearer foo="bar", scope="a b""#).expect("parses");
        assert_eq!(challenge.other.get("foo").map(String::as_str), Some("bar"));
        assert_eq!(challenge.scope.as_deref(), Some("a b"));
    }

    #[test]
    fn quoted_commas_and_escapes_survive() {
        let challenge =
            parse_bearer_challenge(r#"Bearer realm="a,b", error_description="say \"hi\" now""#)
                .expect("parses");
        assert_eq!(challenge.realm.as_deref(), Some("a,b"));
        assert_eq!(
            challenge.error_description.as_deref(),
            Some("say \"hi\" now")
        );
    }

    #[test]
    fn picks_bearer_among_multiple_challenges() {
        let header = r#"Basic realm="corp", Bearer resource_metadata="https://h/.well-known/oauth-protected-resource/mcp""#;
        let challenge = parse_bearer_challenge(header).expect("bearer segment found");
        assert!(challenge.resource_metadata.is_some());
    }

    #[test]
    fn non_bearer_or_malformed_yield_none() {
        assert!(parse_bearer_challenge(r#"Basic realm="corp""#).is_none());
        assert!(parse_bearer_challenge("").is_none());
        // Scheme token must be complete.
        assert!(parse_bearer_challenge("BearerX realm=a").is_none());
        // Unterminated quote.
        assert!(parse_bearer_challenge(r#"Bearer realm="oops"#).is_none());
    }

    #[test]
    fn bare_scheme_is_a_valid_empty_challenge() {
        let challenge = parse_bearer_challenge("Bearer").expect("empty challenge");
        assert_eq!(challenge, BearerChallenge::default());
    }

    #[test]
    fn duplicate_parameter_keeps_first_value() {
        let challenge =
            parse_bearer_challenge(r#"Bearer realm="one", realm="two""#).expect("parses");
        assert_eq!(challenge.realm.as_deref(), Some("one"));
    }

    #[test]
    fn multibyte_garbage_yields_none_not_panic() {
        // The scheme-window byte comparison must not panic when a multibyte
        // character straddles the 6-byte boundary.
        assert!(parse_bearer_challenge("Bearsé realm=a").is_none());
        assert!(parse_bearer_challenge("Bea😀").is_none());
        assert!(parse_bearer_challenge("éBearer realm=a").is_none());
        // Multibyte inside a quoted value stays intact.
        let challenge =
            parse_bearer_challenge("Bearer error_description=\"скажи «привіт»\"").expect("parses");
        assert_eq!(
            challenge.error_description.as_deref(),
            Some("скажи «привіт»")
        );
    }

    #[test]
    fn whitespace_around_equals_stays_a_parameter_not_a_challenge_boundary() {
        // RFC 7235 forbids OWS around '=', but loose servers emit it; the
        // later parameters (incl. resource_metadata) must not be dropped.
        let challenge = parse_bearer_challenge(
            r#"Bearer realm="m", error = invalid_token, resource_metadata = "https://h/.well-known/oauth-protected-resource/mcp""#,
        )
        .expect("loose spacing must still parse");
        assert_eq!(challenge.error.as_deref(), Some("invalid_token"));
        assert_eq!(
            challenge.resource_metadata.as_deref(),
            Some("https://h/.well-known/oauth-protected-resource/mcp")
        );
    }
}
