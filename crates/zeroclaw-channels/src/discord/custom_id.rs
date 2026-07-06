//! `custom_id` codec for Discord message components. Discord echoes a
//! component's `custom_id` (≤100 chars) back verbatim on every click/submit, so
//! it is the only channel for routing state. We encode a `(kind, arg)` pair
//! behind a scheme marker (`zc1`) so the inbound dispatch can tell *our*
//! components from foreign ones (a bot may share a channel with other apps'
//! buttons) and parse them back; anything that isn't a well-formed `zc1` token
//! parses to `None` and is ignored — the same ownership-marker discipline the
//! slash reaper uses. No bearer secrets ever go in a `custom_id`: it flows
//! through logs and client round-trips, so it carries only a routing key.

/// Scheme marker prefixing every custom_id this channel emits. Versioned so a
/// future wire change can coexist with `zc1` tokens still in flight on
/// already-rendered messages.
const SCHEME: &str = "zc1";

/// Discord's hard limit on a `custom_id`.
const MAX_CUSTOM_ID_LEN: usize = 100;

/// A parsed component routing token. `kind` selects the handler (e.g.
/// `"approve"`, `"page"`); `arg` carries an opaque, handler-defined payload
/// (e.g. an interaction id or a page index). Neither may be a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CustomId {
    pub(crate) kind: String,
    pub(crate) arg: String,
}

impl CustomId {
    pub(crate) fn new(kind: impl Into<String>, arg: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            arg: arg.into(),
        }
    }

    /// Encode to the wire form `zc1|<kind>|<arg>`, escaping the `|` and `\`
    /// separators in each field so they round-trip. Returns `None` when the
    /// result would exceed Discord's 100-char `custom_id` limit (the caller must
    /// shorten the payload rather than emit a token Discord will 400) or when
    /// `kind` is empty (a token with no handler is unroutable).
    pub(crate) fn encode(&self) -> Option<String> {
        if self.kind.is_empty() {
            return None;
        }
        let token = format!("{SCHEME}|{}|{}", escape(&self.kind), escape(&self.arg));
        (token.len() <= MAX_CUSTOM_ID_LEN).then_some(token)
    }

    /// Parse a `custom_id` received on an interaction. Returns `None` for any
    /// token that isn't a well-formed `zc1|<kind>|<arg>` with a non-empty
    /// `kind` — foreign apps' components and malformed input are silently
    /// ignored rather than misrouted.
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        if raw.len() > MAX_CUSTOM_ID_LEN {
            return None;
        }
        let rest = raw.strip_prefix(SCHEME)?.strip_prefix('|')?;
        let (kind_esc, arg_esc) = split_unescaped_pipe(rest)?;
        let kind = unescape(kind_esc);
        if kind.is_empty() {
            return None;
        }
        Some(Self {
            kind,
            arg: unescape(arg_esc),
        })
    }
}

/// Escape `\` and `|` so a field can't inject a separator.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('|', "\\|")
}

/// Reverse [`escape`].
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(next) => out.push(next), // `\\` -> `\`, `\|` -> `|`
                None => out.push('\\'),       // trailing backslash, kept literal
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Split on the first *unescaped* `|`, returning `(before, after)`. `None` when
/// there is no separator (a token must have both a kind and an arg field, even
/// if the arg is empty: `zc1|kind|` is valid, `zc1|kind` is not).
fn split_unescaped_pipe(s: &str) -> Option<(&str, &str)> {
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '|' if !escaped => return Some((&s[..i], &s[i + 1..])),
            _ => escaped = false,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_simple() {
        let id = CustomId::new("approve", "interaction-123");
        let wire = id.encode().unwrap();
        assert_eq!(wire, "zc1|approve|interaction-123");
        assert_eq!(CustomId::parse(&wire), Some(id));
    }

    #[test]
    fn round_trips_payload_with_separators() {
        // A payload containing the separators must survive the round trip.
        let id = CustomId::new("page", r"a|b\c");
        let wire = id.encode().unwrap();
        assert_eq!(CustomId::parse(&wire), Some(id));
        // The raw separators are escaped on the wire.
        assert!(wire.contains(r"a\|b\\c"));
    }

    #[test]
    fn empty_arg_is_valid() {
        let id = CustomId::new("refresh", "");
        let wire = id.encode().unwrap();
        assert_eq!(wire, "zc1|refresh|");
        assert_eq!(CustomId::parse(&wire), Some(id));
    }

    #[test]
    fn empty_kind_is_rejected_both_ways() {
        assert_eq!(CustomId::new("", "x").encode(), None);
        assert_eq!(CustomId::parse("zc1||x"), None);
    }

    #[test]
    fn foreign_and_malformed_tokens_parse_to_none() {
        assert_eq!(CustomId::parse("other-app-button"), None);
        assert_eq!(CustomId::parse("zc1"), None); // no fields
        assert_eq!(CustomId::parse("zc1|kind"), None); // missing arg separator
        assert_eq!(CustomId::parse("zc2|kind|arg"), None); // wrong scheme version
        assert_eq!(CustomId::parse(""), None);
    }

    #[test]
    fn over_length_token_is_rejected() {
        let id = CustomId::new("k", "x".repeat(200));
        assert_eq!(id.encode(), None);
        // A 101-char raw input is rejected before parsing.
        assert_eq!(CustomId::parse(&"z".repeat(101)), None);
    }

    #[test]
    fn round_trips_kind_with_separators() {
        let id = CustomId::new(r"ap|pr\ove", "interaction-123");
        let wire = id.encode().unwrap();

        assert_eq!(CustomId::parse(&wire), Some(id));
        assert!(wire.contains(r"ap\|pr\\ove"));
    }
}
