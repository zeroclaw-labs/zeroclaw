//! Which parts of a URL can carry a credential, and how config reads hide
//! them.
//!
//! A URL can carry a credential in three components: the userinfo before the
//! host (`user:password@`), the query (`?api_key=…`) and the fragment.
//! Parameter names differ by provider, so the whole query and the whole
//! fragment count as sensitive, as does the whole userinfo. The scheme, host,
//! port and path stay visible: they are what an operator needs to recognise
//! an endpoint. The providers' error scrubber removes the same components
//! from error text, using [`split`].
//!
//! Config reads show each sensitive component as [`MASKED_SECRET`]. A client
//! that writes such a value back gets the stored component in its place (see
//! [`restore`]), so editing the host of a masked URL keeps its password.

use std::fmt;

use crate::traits::MASKED_SECRET;

/// A URL split into the components that can carry a credential and the
/// rest. Joining the parts with their separators gives the URL back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UrlParts<'a> {
    /// `scheme://`, or empty when the URL has no scheme.
    pub scheme: &'a str,
    /// The userinfo, without its `@`.
    pub userinfo: Option<&'a str>,
    /// Host, port and path.
    pub location: &'a str,
    /// The query, without its `?`.
    pub query: Option<&'a str>,
    /// The fragment, without its `#`.
    pub fragment: Option<&'a str>,
}

impl UrlParts<'_> {
    fn join(&self, userinfo: Option<&str>, query: Option<&str>, fragment: Option<&str>) -> String {
        let mut url = String::with_capacity(self.scheme.len() + self.location.len() + 32);
        url.push_str(self.scheme);
        if let Some(userinfo) = userinfo {
            url.push_str(userinfo);
            url.push('@');
        }
        url.push_str(self.location);
        if let Some(query) = query {
            url.push('?');
            url.push_str(query);
        }
        if let Some(fragment) = fragment {
            url.push('#');
            url.push_str(fragment);
        }
        url
    }
}

/// Schemes the runtime's URL parser (`reqwest::Url`, which follows the WHATWG
/// URL standard) treats as special: any run of `/` and `\` after the colon
/// comes before the authority, and a `\` also ends it.
const SPECIAL_SCHEMES: [&str; 6] = ["ftp", "file", "http", "https", "ws", "wss"];

/// Split `url` into its parts, with the boundaries the runtime's URL parser
/// draws. The fragment starts at the first `#` and the query at the first
/// `?` before it. The authority follows the scheme: for `http`, `https`,
/// `ws`, `wss`, `ftp` and `file`, after any run of `/` and `\`, so `http:/h`,
/// `http:///h` and `http:\\h` all name host `h`; for any other scheme, after
/// `//`. It ends at the next `/`, or `\` for those schemes. A prefix such as
/// `custom:` before the scheme stays with it. A value with no scheme is read
/// as `authority/path`, the way a client that assumes `http://` reads it.
/// The userinfo is everything in the authority before its last `@`.
#[must_use]
pub fn split(url: &str) -> UrlParts<'_> {
    let (before_fragment, fragment) = match url.split_once('#') {
        Some((before, fragment)) => (before, Some(fragment)),
        None => (url, None),
    };
    let (before_query, query) = match before_fragment.split_once('?') {
        Some((before, query)) => (before, Some(query)),
        None => (before_fragment, None),
    };
    let found = locate(before_query);
    let (scheme, rest) = before_query.split_at(found.authority);
    let authority_end = rest
        .find(|c| c == '/' || (found.special && c == '\\'))
        .unwrap_or(rest.len());
    let (userinfo, location) = match rest[..authority_end].rfind('@') {
        Some(at) => (Some(&rest[..at]), &rest[at + 1..]),
        None => (None, rest),
    };
    UrlParts {
        scheme,
        userinfo,
        location,
        query,
        fragment,
    }
}

/// Where a URL's scheme and authority start, as [`split`] reads them.
struct Located {
    /// The scheme's first byte, past leading whitespace and any `name:`
    /// prefix; for a value with no scheme, its first non-blank byte.
    scheme: usize,
    /// The authority's first byte.
    authority: usize,
    /// Whether the scheme is special (and a value with no scheme, read as
    /// `http`, counts as one).
    special: bool,
}

/// Walk `name:` prefixes from the first non-blank byte of `url` until a
/// special scheme, or a scheme followed by `//`. Anything else is a value
/// with no scheme.
fn locate(url: &str) -> Located {
    let leading = url.len() - url.trim_start().len();
    let mut at = leading;
    while let Some(colon) = scheme_len(&url[at..]) {
        let name = &url[at..at + colon];
        let after = at + colon + 1;
        if SPECIAL_SCHEMES
            .iter()
            .any(|scheme| name.eq_ignore_ascii_case(scheme))
        {
            let slashes = url[after..]
                .find(|c| c != '/' && c != '\\')
                .unwrap_or(url.len() - after);
            return Located {
                scheme: at,
                authority: after + slashes,
                special: true,
            };
        }
        if url[after..].starts_with("//") {
            return Located {
                scheme: at,
                authority: after + 2,
                special: false,
            };
        }
        at = after;
    }
    Located {
        scheme: leading,
        authority: leading,
        special: true,
    }
}

/// The length of the scheme `s` starts with, when it is followed by `:`: an
/// ASCII letter, then letters, digits, `+`, `-` or `.`.
fn scheme_len(s: &str) -> Option<usize> {
    let colon = s.find(':')?;
    let mut chars = s[..colon].chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then_some(colon)
}

/// `url` with its userinfo, query and fragment, each when present and not
/// empty, shown as [`MASKED_SECRET`]:
/// `https://***MASKED***@host/v1?***MASKED***`. A URL without any of them
/// comes back unchanged when the runtime parser proves its components safe.
/// A remaining credential or indeterminate parsing masks the whole value.
#[must_use]
pub fn mask(url: &str) -> String {
    let parts = split(url);
    let masked = parts.join(
        masked(parts.userinfo),
        masked(parts.query),
        masked(parts.fragment),
    );
    if parser_credential_evidence(&masked) != Some(false) {
        return MASKED_SECRET.to_string();
    }
    masked
}

/// `url` without its userinfo, query and fragment: the bare endpoint, which
/// keeps none of the components [`mask`] hides. Empty when the runtime parser
/// finds a remaining credential or cannot prove the endpoint safe.
#[must_use]
pub fn endpoint(url: &str) -> String {
    let endpoint = split(url).join(None, None, None);
    if parser_credential_evidence(&endpoint) != Some(false) {
        return String::new();
    }
    endpoint
}

/// `Some(false)` proves the parsed components are credential-free;
/// `Some(true)` finds a remaining credential; `None` is indeterminate.
/// Only a scheme-less value may use reqwest's `http://` proxy fallback.
/// Reinterpreting a broken absolute URL as a proxy can move its userinfo
/// into the path and produce a false credential-free result.
fn parser_credential_evidence(url: &str) -> Option<bool> {
    if url.is_empty() {
        return Some(false);
    }
    if url == MASKED_SECRET {
        return None;
    }
    // The embedding factory removes this wrapper before URL parsing.
    // Scheme normalization belongs to the parser, not the display splitter.
    let candidate = url.strip_prefix("custom:").unwrap_or(url);
    let candidate = if url.starts_with("custom:") {
        candidate
    } else {
        &candidate[locate(candidate).scheme..]
    };
    let parsed = match reqwest::Url::parse(candidate) {
        Ok(parsed) => Some(parsed),
        Err(url::ParseError::RelativeUrlWithoutBase) => {
            reqwest::Url::parse(&format!("http://{candidate}")).ok()
        }
        Err(_) => None,
    };
    let sensitive = |component: Option<&str>| {
        component.is_some_and(|value| !value.is_empty() && value != MASKED_SECRET)
    };
    parsed.map(|parsed| {
        sensitive(Some(parsed.username()))
            || sensitive(parsed.password())
            || sensitive(parsed.query())
            || sensitive(parsed.fragment())
    })
}

/// A component as a read shows it: masked unless absent or empty.
fn masked(component: Option<&str>) -> Option<&str> {
    component.map(|value| {
        if value.is_empty() {
            value
        } else {
            MASKED_SECRET
        }
    })
}

/// Whether `value` carries the masked placeholder anywhere.
#[must_use]
pub fn carries_mask(value: &str) -> bool {
    value.contains(MASKED_SECRET)
}

/// Put back the components of `current` that a client echoed masked.
///
/// `edited` is the value a client wrote. Each of its components that is
/// exactly [`MASKED_SECRET`] takes the same component of `current`, the
/// stored value it was masked from; everything else in `edited` stands, so a
/// client can change the host and keep the stored password. A value without
/// the placeholder is returned as it is.
///
/// # Errors
///
/// An exact whole-value placeholder restores the nonempty stored value,
/// including a URL masked whole by the parser safety check.
/// [`UnresolvedMask`] when the placeholder appears anywhere but as a whole
/// value, userinfo, query or fragment, or stands for a component `current` does not
/// have: there is then no stored value to restore, and writing the
/// placeholder would store it as if it were real.
pub fn restore(edited: &str, current: Option<&str>) -> Result<String, UnresolvedMask> {
    if !carries_mask(edited) {
        return Ok(edited.to_string());
    }
    if edited == MASKED_SECRET {
        return current
            .filter(|stored| !stored.is_empty() && !carries_mask(stored))
            .map(str::to_owned)
            .ok_or_else(UnresolvedMask::url);
    }
    let edited_parts = split(edited);
    if carries_mask(edited_parts.scheme) || carries_mask(edited_parts.location) {
        return Err(UnresolvedMask::url());
    }
    let current_parts = current.map(split);
    let userinfo = restored(
        edited_parts.userinfo,
        current_parts.and_then(|parts| parts.userinfo),
    )?;
    let query = restored(
        edited_parts.query,
        current_parts.and_then(|parts| parts.query),
    )?;
    let fragment = restored(
        edited_parts.fragment,
        current_parts.and_then(|parts| parts.fragment),
    )?;
    Ok(edited_parts.join(userinfo, query, fragment))
}

/// Restore a URL list by its displayed endpoints, so reordering cannot
/// transfer a credential to a different relay. An endpoint edited while its
/// credentials remain masked needs the full URL; there is no stable list key
/// identifying which stored credential that edit should inherit.
pub fn restore_list(edited: &mut [String], current: &[String]) -> Result<(), UnresolvedMask> {
    for value in edited {
        if !carries_mask(value) {
            continue;
        }
        let sources: Vec<&str> = current
            .iter()
            .filter(|stored| mask(stored) == *value)
            .map(String::as_str)
            .collect();
        let [source] = sources.as_slice() else {
            return Err(UnresolvedMask::entry(sources.len()));
        };
        *value = restore(value, Some(source))?;
    }
    Ok(())
}

/// A component as written back: the stored one in place of an exact
/// placeholder, the written one otherwise.
fn restored<'a>(
    component: Option<&'a str>,
    stored: Option<&'a str>,
) -> Result<Option<&'a str>, UnresolvedMask> {
    match component {
        Some(value) if value == MASKED_SECRET => stored
            .filter(|stored| !stored.is_empty())
            .map(Some)
            .ok_or_else(UnresolvedMask::url),
        Some(value) if carries_mask(value) => Err(UnresolvedMask::url()),
        other => Ok(other),
    }
}

/// A write that carries the masked placeholder where no stored value can
/// replace it. It is the caller's error, whichever route the write came
/// through: the client has to send the full value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedMask {
    message: String,
}

impl UnresolvedMask {
    fn url() -> Self {
        Self {
            message: format!(
                "the URL carries the masked placeholder `{MASKED_SECRET}` where no stored value \
                 can replace it; send the full URL"
            ),
        }
    }

    /// An entry of an object array that carries the placeholder but matches
    /// `matches` stored entries instead of exactly one.
    pub(crate) fn entry(matches: usize) -> Self {
        Self {
            message: format!(
                "an entry carries the masked placeholder `{MASKED_SECRET}` but matches \
                 {matches} stored entries; send its full URL"
            ),
        }
    }

    /// The same refusal, naming the property it was written to.
    #[must_use]
    pub fn at(self, path: &str) -> Self {
        Self {
            message: format!("{path}: {}", self.message),
        }
    }
}

impl fmt::Display for UnresolvedMask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for UnresolvedMask {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_finds_each_credential_bearing_component() {
        let parts = split("https://user:pass@host.example:8443/v1/chat?key=abc&x=1#token=def");
        assert_eq!(parts.scheme, "https://");
        assert_eq!(parts.userinfo, Some("user:pass"));
        assert_eq!(parts.location, "host.example:8443/v1/chat");
        assert_eq!(parts.query, Some("key=abc&x=1"));
        assert_eq!(parts.fragment, Some("token=def"));
        // A `?` inside the fragment belongs to the fragment.
        let parts = split("https://host/v1#a?b");
        assert_eq!(parts.query, None);
        assert_eq!(parts.fragment, Some("a?b"));
        // An `@` in the path is not userinfo; a password may hold `@`.
        assert_eq!(split("https://host/users/@me").userinfo, None);
        assert_eq!(split("https://u:p@ss@host/").userinfo, Some("u:p@ss"));
        // No scheme: the authority still starts the string.
        let parts = split("user:pw@localhost:11434");
        assert_eq!((parts.scheme, parts.userinfo), ("", Some("user:pw")));
    }

    /// Spellings the runtime's URL parser accepts, each with the userinfo it
    /// finds there (or none).
    const SPELLINGS: [(&str, Option<&str>); 18] = [
        ("http://u:pw@h/v1", Some("u:pw")),
        ("http:///u:pw@h/v1", Some("u:pw")),
        ("http:/u:pw@h/v1", Some("u:pw")),
        ("http:////u:pw@h/v1", Some("u:pw")),
        ("HTTP:///u:pw@h/v1", Some("u:pw")),
        ("https:\\\\u:pw@h\\v1", Some("u:pw")),
        ("https:/\\u:pw@h/v1", Some("u:pw")),
        ("wss:///u:pw@h", Some("u:pw")),
        ("  https://u:pw@h/v1", Some("u:pw")),
        ("socks5://u:pw@h:1080", Some("u:pw")),
        ("redis://:pw@h:6379/0", Some(":pw")),
        ("postgres://u:pw@h/db", Some("u:pw")),
        ("https://u:p@ss@h/", Some("u:p@ss")),
        ("https://h/users/@me", None),
        ("https://example.invalid\\docs@v1", None),
        ("socks5:///u:pw@h", None),
        ("https://h:8443/v1", None),
        ("http://[::1]:9/v1", None),
    ];

    /// `split` draws the userinfo where the runtime's URL parser does, for
    /// every spelling it accepts, and the masked value leaves the parser
    /// nothing to find.
    #[test]
    fn split_agrees_with_the_runtime_parser_on_the_userinfo() {
        for (url, userinfo) in SPELLINGS {
            let parsed = reqwest::Url::parse(url).expect(url);
            let parser_sees = !parsed.username().is_empty() || parsed.password().is_some();
            assert_eq!(parser_sees, userinfo.is_some(), "{url}: the fixture");
            assert_eq!(split(url).userinfo, userinfo, "{url}");
            let masked = reqwest::Url::parse(&mask(url)).expect(url);
            assert_eq!(masked.password(), None, "{url}: {}", mask(url));
            if userinfo.is_some() {
                assert_eq!(masked.username(), MASKED_SECRET, "{url}");
            }
        }
    }

    /// Alternate spellings round-trip through a masked read and an echo, and
    /// a URL without a credential is left byte for byte.
    #[test]
    fn alternate_spellings_mask_and_restore() {
        for (stored, masked) in [
            (
                "http:///u:pw@127.0.0.1:9/v1",
                "http:///***MASKED***@127.0.0.1:9/v1",
            ),
            (
                "http:/u:pw@127.0.0.1:9/v1",
                "http:/***MASKED***@127.0.0.1:9/v1",
            ),
            (
                "http:////u:pw@127.0.0.1:9/v1",
                "http:////***MASKED***@127.0.0.1:9/v1",
            ),
            (
                "custom:http:///u:pw@h/v1?key=k",
                "custom:http:///***MASKED***@h/v1?***MASKED***",
            ),
        ] {
            assert_eq!(mask(stored), masked, "{stored}");
            assert_eq!(restore(masked, Some(stored)).unwrap(), stored);
            assert_eq!(endpoint(stored), endpoint(masked), "{stored}");
        }
        for url in [
            "https://example.invalid\\docs@v1",
            "custom:https://api.example/v1",
            "localhost:11434",
            "socks5:///u:pw@h",
        ] {
            assert_eq!(mask(url), url, "unchanged: {url}");
        }
    }

    /// A value the runtime's parser reads differently from `split`, here a
    /// scheme with a tab in it, which the parser removes, is masked whole.
    #[test]
    fn a_value_split_cannot_read_is_masked_whole() {
        assert_eq!(mask("ht\ttp://u:pw@h/v1"), MASKED_SECRET);
        assert_eq!(endpoint("ht\ttp://u:pw@h/v1"), "");
    }

    /// An unencoded `/` ends the authority, for `split` as for the parser,
    /// which rejects what is left (`user:p` is not a host and port).
    #[test]
    fn a_slash_ends_the_authority_as_it_does_for_the_parser() {
        assert_eq!(split("https://user:p/w@host/").userinfo, None);
        assert!(reqwest::Url::parse("https://user:p/w@host/").is_err());
        assert_eq!(
            split("https://user:p%2Fw@host/").userinfo,
            Some("user:p%2Fw")
        );
    }

    #[test]
    fn mask_hides_userinfo_query_and_fragment_and_keeps_the_endpoint() {
        assert_eq!(
            mask("http://review-user:pw-654738@127.0.0.1:9/v1?credential=q-938472#frag"),
            "http://***MASKED***@127.0.0.1:9/v1?***MASKED***#***MASKED***"
        );
        assert_eq!(mask("https://api.example/v1"), "https://api.example/v1");
        assert_eq!(mask("socks5://proxy:1080"), "socks5://proxy:1080");
        // Empty components carry nothing and stay as written.
        assert_eq!(mask("https://host/v1?"), "https://host/v1?");
        assert_eq!(mask(""), "");
    }

    #[test]
    fn endpoint_drops_every_component_mask_hides() {
        assert_eq!(
            endpoint("https://user:pw@host.example:8443/v1/chat?key=abc#token=def"),
            "https://host.example:8443/v1/chat"
        );
        assert_eq!(
            endpoint("https://***MASKED***@host/v1?***MASKED***"),
            "https://host/v1"
        );
        assert_eq!(endpoint("socks5://proxy:1080"), "socks5://proxy:1080");
    }

    #[test]
    fn restore_puts_back_what_was_masked_and_keeps_edits() {
        let stored = "https://user:pw@old.example/v1?key=abc#f";
        // An unchanged echo restores the stored value exactly.
        assert_eq!(restore(&mask(stored), Some(stored)).unwrap(), stored);
        // A new host keeps the stored password and query.
        assert_eq!(
            restore(
                "https://***MASKED***@new.example/v2?***MASKED***",
                Some(stored)
            )
            .unwrap(),
            "https://user:pw@new.example/v2?key=abc"
        );
        // A new credential replaces the stored one.
        assert_eq!(
            restore("https://other:secret@new.example/v1", Some(stored)).unwrap(),
            "https://other:secret@new.example/v1"
        );
        // A value without the placeholder is written as it is.
        assert_eq!(restore("https://h/v1", None).unwrap(), "https://h/v1");
    }

    #[test]
    fn combined_custom_prefix_and_parser_controls_are_masked_and_restored() {
        for inner in [
            "\thttp",
            "\nhttp",
            "\rhttp",
            "\0http",
            " \x01http",
            "\x01http",
            "\x0bhttp",
            " \r\nhttp",
            "ht\ntps",
            "h\tttp",
            "ht\ntp",
            "htt\rp",
        ] {
            let url = format!(
                "custom:{inner}://reader:combined-password@example.invalid/v1?token=combined-query"
            );
            let parsed = reqwest::Url::parse(url.strip_prefix("custom:").unwrap()).unwrap();
            assert_eq!(parsed.password(), Some("combined-password"));
            let shown = mask(&url);
            for marker in ["combined-password", "combined-query"] {
                assert!(!shown.contains(marker), "masked: {shown:?}");
                assert!(
                    !endpoint(&url).contains(marker),
                    "endpoint: {:?}",
                    endpoint(&url)
                );
            }
            assert_eq!(restore(&shown, Some(&url)).unwrap(), url);
        }
        let stored = "h\tttp://reader:whole-password@example.invalid/v1";
        assert_eq!(mask(stored), MASKED_SECRET);
        assert_eq!(restore(MASKED_SECRET, Some(stored)).unwrap(), stored);
        assert!(restore(MASKED_SECRET, None).is_err());
        assert!(restore(MASKED_SECRET, Some(MASKED_SECRET)).is_err());
    }

    #[test]
    fn indeterminate_urls_are_withheld() {
        for raw in [
            "custom:h\tttp://reader:invalid-password@bad host.invalid/v1",
            "custom:\0http://reader:invalid-password@bad host.invalid/v1",
            "h\tttp://reader:invalid-password@bad host.invalid/v1",
        ] {
            assert!(reqwest::Url::parse(raw.strip_prefix("custom:").unwrap_or(raw)).is_err());
            assert_eq!(mask(raw), MASKED_SECRET, "{raw:?}");
            assert_eq!(endpoint(raw), "", "{raw:?}");
            assert_eq!(restore(MASKED_SECRET, Some(raw)).unwrap(), raw);
        }
    }

    #[test]
    fn credential_free_urls_and_selectors_keep_exact_bytes() {
        // Empty values and real selectors carry no credential. Valid URLs
        // without credential components retain the operator's exact spelling.
        for raw in [
            "",
            "none",
            "openai",
            "openrouter",
            "https://EXAMPLE.invalid:443/a%2Fb",
            "custom:https://example.invalid/v1",
            "file:///C:/example",
            "socks5:///u:pw@h",
        ] {
            assert_eq!(mask(raw), raw, "{raw:?}");
        }
    }

    #[test]
    fn restore_refuses_a_placeholder_it_cannot_resolve() {
        // Nothing stored to restore from.
        assert!(restore("https://***MASKED***@h/v1", Some("https://h/v1")).is_err());
        assert!(restore("https://***MASKED***@h/v1", None).is_err());
        // The placeholder only partly replaced, or outside a credential part.
        assert!(
            restore(
                "https://user:***MASKED***@h/v1",
                Some("https://user:pw@h/v1")
            )
            .is_err()
        );
        assert!(restore("https://***MASKED***.example/v1", Some("https://h/v1")).is_err());
    }
}
