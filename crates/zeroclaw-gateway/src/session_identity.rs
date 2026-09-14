//! Injective gateway session identity across WebSocket, REST, lifecycle,
//! turn versions, cancellation, memory, and persistence layers.

pub const GW_SESSION_PREFIX: &str = "gw_";

/// Maximum allowed length for a client-selected gateway session ID.
pub const MAX_SESSION_ID_LEN: usize = 128;

/// Error indicating a gateway session ID violates the collision-free alphabet
/// or prefix constraints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionIdError {
    Empty,
    TooLong { len: usize, max: usize },
    InvalidCharacter(char),
    DoublePrefix,
}

impl std::fmt::Display for SessionIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "session ID cannot be empty"),
            Self::TooLong { len, max } => {
                write!(f, "session ID length {len} exceeds maximum {max}")
            }
            Self::InvalidCharacter(c) => write!(
                f,
                "session ID contains invalid character '{c}'; only ASCII alphanumeric, '-', and '_' are allowed"
            ),
            Self::DoublePrefix => write!(
                f,
                "bare gateway session ID cannot start with '{GW_SESSION_PREFIX}'"
            ),
        }
    }
}

impl std::error::Error for SessionIdError {}

/// Canonical gateway session identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GatewaySessionIdentity {
    bare_id: String,
    session_key: String,
}

impl GatewaySessionIdentity {
    /// Return the bare display / memory session ID without the `gw_` prefix.
    #[must_use]
    pub fn bare_id(&self) -> &str {
        &self.bare_id
    }

    /// Return the canonical storage / lifecycle session key (always prefixed with `gw_`).
    #[must_use]
    pub fn session_key(&self) -> &str {
        &self.session_key
    }
}

/// Validate and canonicalize a gateway session identifier.
///
/// A valid gateway session identifier:
/// - Must be non-empty and at most [`MAX_SESSION_ID_LEN`] characters.
/// - May optionally have a single `gw_` prefix (e.g. `gw_team_alpha`), which is stripped to obtain the bare display ID.
/// - The bare display ID must be non-empty (so bare `""` or input `"gw_"` is rejected).
/// - The bare display ID must NOT start with `gw_` (rejecting ambiguous double prefixes like `gw_gw_foo`).
/// - The bare display ID must contain only ASCII alphanumeric characters, hyphens (`-`), and underscores (`_`).
///   Characters outside `[A-Za-z0-9_-]` (such as `.`, `/`, `\`, `@`, ` `) are rejected because they would be
///   sanitized by persistence and memory layers, causing collisions between distinct client-selected IDs
///   (e.g. `a.b` and `a_b`).
///
/// Returns a [`GatewaySessionIdentity`] where `session_key` is strictly `format!("gw_{bare_id}")`.
pub fn validate_and_canonicalize_gateway_session_id(
    raw: &str,
) -> Result<GatewaySessionIdentity, SessionIdError> {
    if raw.is_empty() {
        return Err(SessionIdError::Empty);
    }
    if raw.len() > MAX_SESSION_ID_LEN {
        return Err(SessionIdError::TooLong {
            len: raw.len(),
            max: MAX_SESSION_ID_LEN,
        });
    }

    let bare = if let Some(stripped) = raw.strip_prefix(GW_SESSION_PREFIX) {
        if stripped.is_empty() {
            return Err(SessionIdError::Empty);
        }
        if stripped.starts_with(GW_SESSION_PREFIX) {
            return Err(SessionIdError::DoublePrefix);
        }
        stripped
    } else {
        raw
    };

    if bare.starts_with(GW_SESSION_PREFIX) {
        return Err(SessionIdError::DoublePrefix);
    }

    for c in bare.chars() {
        if !c.is_ascii_alphanumeric() && c != '_' && c != '-' {
            return Err(SessionIdError::InvalidCharacter(c));
        }
    }

    Ok(GatewaySessionIdentity {
        bare_id: bare.to_string(),
        session_key: format!("{GW_SESSION_PREFIX}{bare}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_bare_and_prefixed_forms_identically() {
        let bare = validate_and_canonicalize_gateway_session_id("team_alpha").unwrap();
        let prefixed = validate_and_canonicalize_gateway_session_id("gw_team_alpha").unwrap();

        assert_eq!(bare.bare_id(), "team_alpha");
        assert_eq!(bare.session_key(), "gw_team_alpha");
        assert_eq!(bare, prefixed);
    }

    #[test]
    fn rejects_empty_and_prefix_only() {
        assert_eq!(
            validate_and_canonicalize_gateway_session_id(""),
            Err(SessionIdError::Empty)
        );
        assert_eq!(
            validate_and_canonicalize_gateway_session_id("gw_"),
            Err(SessionIdError::Empty)
        );
    }

    #[test]
    fn rejects_double_prefix() {
        assert_eq!(
            validate_and_canonicalize_gateway_session_id("gw_gw_alpha"),
            Err(SessionIdError::DoublePrefix)
        );
    }

    #[test]
    fn rejects_punctuation_collisions() {
        // Punctuation like '.' would be sanitized to '_' by JSONL / memory layers,
        // causing distinct IDs to collide. Validating the alphabet prevents this.
        assert_eq!(
            validate_and_canonicalize_gateway_session_id("a.b"),
            Err(SessionIdError::InvalidCharacter('.'))
        );
        assert_eq!(
            validate_and_canonicalize_gateway_session_id("a/b"),
            Err(SessionIdError::InvalidCharacter('/'))
        );
        assert_eq!(
            validate_and_canonicalize_gateway_session_id("a b"),
            Err(SessionIdError::InvalidCharacter(' '))
        );
        assert_eq!(
            validate_and_canonicalize_gateway_session_id("a@b"),
            Err(SessionIdError::InvalidCharacter('@'))
        );
    }

    #[test]
    fn accepts_uuid_and_hyphenated_alphanumeric() {
        let uuid_str = "123e4567-e89b-12d3-a456-426614174000";
        let ident = validate_and_canonicalize_gateway_session_id(uuid_str).unwrap();
        assert_eq!(ident.bare_id(), uuid_str);
        assert_eq!(ident.session_key(), format!("gw_{uuid_str}"));
    }

    #[test]
    fn enforces_maximum_length() {
        let long_id = "a".repeat(MAX_SESSION_ID_LEN + 1);
        assert!(matches!(
            validate_and_canonicalize_gateway_session_id(&long_id),
            Err(SessionIdError::TooLong { .. })
        ));
    }
}
