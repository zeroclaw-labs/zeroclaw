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

/// Split `url` into its parts. The fragment starts at the first `#`, the
/// query at the first `?` before it, the authority ends at the first `/`
/// after `scheme://`, and the userinfo is everything in the authority before
/// its last `@`.
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
    let scheme_end = before_query
        .find("://")
        .map_or(0, |separator| separator + 3);
    let (scheme, rest) = before_query.split_at(scheme_end);
    let authority_end = rest.find('/').unwrap_or(rest.len());
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

/// `url` with its userinfo, query and fragment, each when present and not
/// empty, shown as [`MASKED_SECRET`]:
/// `https://***MASKED***@host/v1?***MASKED***`. A URL without any of them
/// comes back unchanged.
#[must_use]
pub fn mask(url: &str) -> String {
    let parts = split(url);
    parts.join(
        masked(parts.userinfo),
        masked(parts.query),
        masked(parts.fragment),
    )
}

/// `url` without its userinfo, query and fragment: the bare endpoint, which
/// keeps none of the components [`mask`] hides.
#[must_use]
pub fn endpoint(url: &str) -> String {
    split(url).join(None, None, None)
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
/// [`UnresolvedMask`] when the placeholder appears anywhere but as a whole
/// userinfo, query or fragment, or stands for a component `current` does not
/// have: there is then no stored value to restore, and writing the
/// placeholder would store it as if it were real.
pub fn restore(edited: &str, current: Option<&str>) -> Result<String, UnresolvedMask> {
    if !carries_mask(edited) {
        return Ok(edited.to_string());
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
