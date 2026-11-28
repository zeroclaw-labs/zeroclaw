// Copyright (c) 2024 Jonas Schäfer <jonas@zombofant.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use alloc::collections::BTreeMap;
use core::{error::Error, fmt};

use minidom::Element;
use xso::{AsXml, FromXml};

use crate::{message::Lang, ns};

/// Enumeration of all stream error conditions as defined in [RFC 6120].
///
/// All variant documentation is directly quoted from [RFC 6120].
///
///    [RFC 6120]: https://datatracker.ietf.org/doc/html/rfc6120#section-4.9.3
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::XMPP_STREAMS)]
pub enum DefinedCondition {
    /// The entity has sent XML that cannot be processed.
    ///
    /// This error can be used instead of the more specific XML-related
    /// errors, such as `<bad-namespace-prefix/>`, `<invalid-xml/>`,
    /// `<not-well-formed/>`, `<restricted-xml/>`, and
    /// `<unsupported-encoding/>`.  However, the more specific errors are
    /// RECOMMENDED.
    #[xml(name = "bad-format")]
    BadFormat,

    /// The entity has sent a namespace prefix that is unsupported, or has
    /// sent no namespace prefix on an element that needs such a prefix (see
    /// [Section 11.2](https://datatracker.ietf.org/doc/html/rfc6120#section-11.2)).
    #[xml(name = "bad-namespace-prefix")]
    BadNamespacePrefix,

    /// The server either (1) is closing the existing stream for this entity
    /// because a new stream has been initiated that conflicts with the
    /// existing stream, or (2) is refusing a new stream for this entity
    /// because allowing the new stream would conflict with an existing
    /// stream (e.g., because the server allows only a certain number of
    /// connections from the same IP address or allows only one server-to-
    /// server stream for a given domain pair as a way of helping to ensure
    /// in-order processing as described under
    /// [Section 10.1](https://datatracker.ietf.org/doc/html/rfc6120#section-10.1)).
    ///
    /// If a client receives a `<conflict/>` stream error, during the resource
    /// binding aspect of its reconnection attempt it MUST NOT blindly request
    /// the resourcepart it used during the former session but instead MUST
    /// choose a different resourcepart; details are provided under
    /// [Section 7](https://datatracker.ietf.org/doc/html/rfc6120#section-7).
    #[xml(name = "conflict")]
    Conflict,

    /// One party is closing the stream because it has reason to believe that
    /// the other party has permanently lost the ability to communicate over
    /// the stream.  The lack of ability to communicate can be discovered
    /// using various methods, such as whitespace keepalives as specified
    /// under
    /// [Section 4.4](https://datatracker.ietf.org/doc/html/rfc6120#section-4.4),
    /// XMPP-level pings as defined in
    /// [XEP-0199](https://xmpp.org/extensions/xep-0199.html), and
    /// XMPP Stream Management as defined in
    /// [XEP-0198](https://xmpp.org/extensions/xep-0198.html).
    ///
    /// Interoperability Note: RFC 3920 specified that the
    /// `<connection-timeout/>` stream error is to be used if the peer has not
    /// generated any traffic over the stream for some period of time.
    /// That behavior is no longer recommended; instead, the error SHOULD be
    /// used only if the connected client or peer server has not responded to
    /// data sent over the stream.
    #[xml(name = "connection-timeout")]
    ConnectionTimeout,

    /// The value of the 'to' attribute provided in the initial stream header
    /// corresponds to an FQDN that is no longer serviced by the receiving
    /// entity.
    #[xml(name = "host-gone")]
    HostGone,

    /// The value of the 'to' attribute provided in the initial stream header
    /// does not correspond to an FQDN that is serviced by the receiving
    /// entity.
    #[xml(name = "host-unknown")]
    HostUnknown,

    /// A stanza sent between two servers lacks a 'to' or 'from' attribute,
    /// the 'from' or 'to' attribute has no value, or the value violates the
    /// rules for XMPP addresses
    /// (see [RFC 6122](https://datatracker.ietf.org/doc/html/rfc6122)).
    #[xml(name = "improper-addressing")]
    ImproperAddressing,

    /// The server has experienced a misconfiguration or other internal error
    /// that prevents it from servicing the stream.
    #[xml(name = "internal-server-error")]
    InternalServerError,

    /// The data provided in a 'from' attribute does not match an authorized
    /// JID or validated domain as negotiated (1) between two servers using
    /// SASL or Server Dialback, or (2) between a client and a server via
    /// SASL authentication and resource binding.
    #[xml(name = "invalid-from")]
    InvalidFrom,

    /// The stream namespace name is something other than
    /// `http://etherx.jabber.org/streams` (see
    /// [Section 11.2](https://datatracker.ietf.org/doc/html/rfc6120#section-11.2))
    /// or the content namespace declared as the default namespace is not
    /// supported (e.g., something other than `jabber:client` or
    /// `jabber:server`).
    #[xml(name = "invalid-namespace")]
    InvalidNamespace,

    /// The entity has sent invalid XML over the stream to a server that
    /// performs validation (see
    /// [Section 11.4](https://datatracker.ietf.org/doc/html/rfc6120#section-11.4)).
    #[xml(name = "invalid-xml")]
    InvalidXml,

    /// The entity has attempted to send XML stanzas or other outbound data
    /// before the stream has been authenticated, or otherwise is not
    /// authorized to perform an action related to stream negotiation; the
    /// receiving entity MUST NOT process the offending data before sending
    /// the stream error.
    #[xml(name = "not-authorized")]
    NotAuthorized,

    /// The initiating entity has sent XML that violates the well-formedness
    /// rules of [XML](https://www.w3.org/TR/REC-xml/) or
    /// [XML-NAMES](https://www.w3.org/TR/REC-xml-names/).
    #[xml(name = "not-well-formed")]
    NotWellFormed,

    /// The entity has violated some local service policy (e.g., a stanza
    /// exceeds a configured size limit); the server MAY choose to specify
    /// the policy in the `<text/>` element or in an application-specific
    /// condition element.
    #[xml(name = "policy-violation")]
    PolicyViolation,

    /// The server is unable to properly connect to a remote entity that is
    /// needed for authentication or authorization (e.g., in certain
    /// scenarios related to Server Dialback
    /// [XEP-0220](https://xmpp.org/extensions/xep-0220.html)); this condition
    /// is not to be used when the cause of the error is within the
    /// administrative domain of the XMPP service provider, in which case the
    /// `<internal-server-error/>` condition is more appropriate.
    #[xml(name = "remote-connection-failed")]
    RemoteConnectionFailed,

    /// The server is closing the stream because it has new (typically
    /// security-critical) features to offer, because the keys or
    /// certificates used to establish a secure context for the stream have
    /// expired or have been revoked during the life of the stream
    /// ([Section 13.7.2.3](https://datatracker.ietf.org/doc/html/rfc6120#section-13.7.2.3)),
    /// because the TLS sequence number has wrapped
    /// ([Section 5.3.5](https://datatracker.ietf.org/doc/html/rfc6120#section-5.3.5)),
    /// etc.  The reset applies to the stream and to any security context
    /// established for that stream (e.g., via TLS and SASL), which means that
    /// encryption and authentication need to be negotiated again for the new
    /// stream (e.g., TLS session resumption cannot be used).
    #[xml(name = "reset")]
    Reset,

    /// The server lacks the system resources necessary to service the stream.
    #[xml(name = "resource-constraint")]
    ResourceConstraint,

    /// The entity has attempted to send restricted XML features such as a
    /// comment, processing instruction, DTD subset, or XML entity reference
    /// (see
    /// [Section 11.1](https://datatracker.ietf.org/doc/html/rfc6120#section-11.1)).
    #[xml(name = "restricted-xml")]
    RestrictedXml,

    /// The server will not provide service to the initiating entity but is
    /// redirecting traffic to another host under the administrative control
    /// of the same service provider.  The XML character data of the
    /// `<see-other-host/>` element returned by the server MUST specify the
    /// alternate FQDN or IP address at which to connect, which MUST be a
    /// valid domainpart or a domainpart plus port number (separated by the
    /// ':' character in the form "domainpart:port").  If the domainpart is
    /// the same as the source domain, derived domain, or resolved IPv4 or
    /// IPv6 address to which the initiating entity originally connected
    /// (differing only by the port number), then the initiating entity
    /// SHOULD simply attempt to reconnect at that address.  (The format of
    /// an IPv6 address MUST follow
    /// [IPv6-ADDR](https://datatracker.ietf.org/doc/html/rfc6120#ref-IPv6-ADDR),
    /// which includes the enclosing the IPv6 address in square brackets
    /// '[' and ']' as originally defined by
    /// [URI](https://datatracker.ietf.org/doc/html/rfc6120#ref-URI).
    /// )  Otherwise, the initiating entity MUST resolve the FQDN
    /// specified in the `<see-other-host/>` element as described under
    /// [Section 3.2](https://datatracker.ietf.org/doc/html/rfc6120#section-3.2).
    ///
    /// When negotiating a stream with the host to which it has been
    /// redirected, the initiating entity MUST apply the same policies it
    /// would have applied to the original connection attempt (e.g., a policy
    /// requiring TLS), MUST specify the same 'to' address on the initial
    /// stream header, and MUST verify the identity of the new host using the
    /// same reference identifier(s) it would have used for the original
    /// connection attempt (in accordance with
    /// [TLS-CERTS](https://datatracker.ietf.org/doc/html/rfc6120#ref-TLS-CERTS)).
    /// Even if the receiving entity returns a `<see-other-host/>` error
    /// before the confidentiality and integrity of the stream have been
    /// established (thus introducing the possibility of a denial-of-service
    /// attack), the fact that the initiating entity needs to verify the
    /// identity of the XMPP service based on the same reference identifiers
    /// implies that the initiating entity will not connect to a malicious
    /// entity.  To reduce the possibility of a denial-of-service attack, (a)
    /// the receiving entity SHOULD NOT close the stream with a
    /// `<see-other-host/>` stream error until after the confidentiality and
    /// integrity of the stream have been protected via TLS or an equivalent
    /// security layer (such as the SASL GSSAPI mechanism), and (b) the
    /// receiving entity MAY have a policy of following redirects only if it
    /// has authenticated the receiving entity.  In addition, the initiating
    /// entity SHOULD abort the connection attempt after a certain number of
    /// successive redirects (e.g., at least 2 but no more than 5).
    #[xml(name = "see-other-host")]
    SeeOtherHost(#[xml(text)] String),

    /// The server is being shut down and all active streams are being closed.
    #[xml(name = "system-shutdown")]
    SystemShutdown,

    /// The error condition is not one of those defined by the other
    /// conditions in this list; this error condition SHOULD NOT be used
    /// except in conjunction with an application-specific condition.
    #[xml(name = "undefined-condition")]
    UndefinedCondition,

    /// The initiating entity has encoded the stream in an encoding that is
    /// not supported by the server (see
    /// [Section 11.6](https://datatracker.ietf.org/doc/html/rfc6120#section-11.6))
    /// or has otherwise improperly encoded the stream (e.g., by violating the
    /// rules of the
    /// [UTF-8](https://datatracker.ietf.org/doc/html/rfc6120#ref-UTF-8)
    /// encoding).
    #[xml(name = "unsupported-encoding")]
    UnsupportedEncoding,

    /// The receiving entity has advertised a mandatory-to-negotiate stream
    /// feature that the initiating entity does not support, and has offered
    /// no other mandatory-to-negotiate feature alongside the unsupported
    /// feature.
    #[xml(name = "unsupported-feature")]
    UnsupportedFeature,

    /// The initiating entity has sent a first-level child of the stream that
    /// is not supported by the server, either because the receiving entity
    /// does not understand the namespace or because the receiving entity
    /// does not understand the element name for the applicable namespace
    /// (which might be the content namespace declared as the default
    /// namespace).
    #[xml(name = "unsupported-stanza-type")]
    UnsupportedStanzaType,

    /// The 'version' attribute provided by the initiating entity in the
    /// stream header specifies a version of XMPP that is not supported by
    /// the server.
    #[xml(name = "unsupported-version")]
    UnsupportedVersion,
}

impl fmt::Display for DefinedCondition {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            Self::BadFormat => "bad-format",
            Self::BadNamespacePrefix => "bad-namespace-prefix",
            Self::Conflict => "conflict",
            Self::ConnectionTimeout => "connection-timeout",
            Self::HostGone => "host-gone",
            Self::HostUnknown => "host-unknown",
            Self::ImproperAddressing => "improper-addressing",
            Self::InternalServerError => "internal-server-error",
            Self::InvalidFrom => "invalid-from",
            Self::InvalidNamespace => "invalid-namespace",
            Self::InvalidXml => "invalid-xml",
            Self::NotAuthorized => "not-authorized",
            Self::NotWellFormed => "not-well-formed",
            Self::PolicyViolation => "policy-violation",
            Self::RemoteConnectionFailed => "remote-connection-failed",
            Self::Reset => "reset",
            Self::ResourceConstraint => "resource-constraint",
            Self::RestrictedXml => "restricted-xml",
            Self::SeeOtherHost(ref host) => return write!(f, "see-other-host: {}", host),
            Self::SystemShutdown => "system-shutdown",
            Self::UndefinedCondition => "undefined-condition",
            Self::UnsupportedEncoding => "unsupported-encoding",
            Self::UnsupportedFeature => "unsupported-feature",
            Self::UnsupportedStanzaType => "unsupported-stanza-type",
            Self::UnsupportedVersion => "unsupported-version",
        };
        f.write_str(s)
    }
}

/// Stream error as specified in RFC 6120.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::STREAM, name = "error")]
pub struct StreamError {
    /// The enumerated error condition which triggered this stream error.
    #[xml(child)]
    pub condition: DefinedCondition,

    /// Optional error text
    #[xml(extract(n = .., name = "text", namespace = ns::XMPP_STREAMS, fields(
        lang(type_ = Lang, default),
        text(type_ = String),
    )))]
    pub texts: BTreeMap<Lang, String>,

    /// Optional application-defined element which refines the specified
    /// [`Self::condition`].
    // TODO: use n = 1 once we have it.
    #[xml(element(n = ..))]
    pub application_specific: Vec<Element>,
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        <DefinedCondition as fmt::Display>::fmt(&self.condition, f)?;
        if let Some((_, text)) = self.get_best_text(vec!["en"]) {
            write!(f, " ({:?})", text)?
        }
        if let Some(cond) = self.application_specific.first() {
            f.write_str(&String::from(cond))?;
        }
        Ok(())
    }
}

impl StreamError {
    /// Create a new StreamError with condition, text, and language
    pub fn new<S: Into<String>, L: Into<Lang>>(
        condition: DefinedCondition,
        lang: L,
        text: S,
    ) -> Self {
        let mut texts = BTreeMap::new();
        texts.insert(lang.into(), text.into());
        Self {
            condition,
            texts,
            application_specific: Vec::new(),
        }
    }

    /// Add a text element with the specified language
    pub fn add_text<L: Into<Lang>, S: Into<String>>(mut self, lang: L, text: S) -> Self {
        self.texts.insert(lang.into(), text.into());
        self
    }

    /// Append application specific element(s)
    pub fn with_application_specific(mut self, application_specific: Vec<Element>) -> Self {
        self.application_specific = application_specific;
        self
    }

    /// Get the best matching text from a list of preferred languages.
    ///
    /// This follows the same logic as Message::get_best_body:
    /// 1. First tries to find a match from the preferred languages list
    /// 2. Falls back to empty language ("") if available
    /// 3. Returns the first entry if no matches found
    ///
    /// Returns None if no text elements exist.
    pub fn get_best_text(&self, preferred_langs: Vec<&str>) -> Option<(Lang, &String)> {
        Self::get_best(&self.texts, preferred_langs)
    }

    /// Cloned variant of [`StreamError::get_best_text`]
    pub fn get_best_text_cloned(&self, preferred_langs: Vec<&str>) -> Option<(Lang, String)> {
        Self::get_best_cloned(&self.texts, preferred_langs)
    }

    // Private helper methods matching Message's pattern
    fn get_best<'a, T>(
        map: &'a BTreeMap<Lang, T>,
        preferred_langs: Vec<&str>,
    ) -> Option<(Lang, &'a T)> {
        if map.is_empty() {
            return None;
        }
        for lang in preferred_langs {
            if let Some(value) = map.get(lang) {
                return Some((Lang::from(lang), value));
            }
        }
        if let Some(value) = map.get("") {
            return Some((Lang::new(), value));
        }
        map.iter().map(|(lang, value)| (lang.clone(), value)).next()
    }

    fn get_best_cloned<T: ToOwned<Owned = T>>(
        map: &BTreeMap<Lang, T>,
        preferred_langs: Vec<&str>,
    ) -> Option<(Lang, T)> {
        if let Some((lang, item)) = Self::get_best::<T>(map, preferred_langs) {
            Some((lang, item.to_owned()))
        } else {
            None
        }
    }

    /// Check if the error has any text elements
    pub fn has_text(&self) -> bool {
        !self.texts.is_empty()
    }
}

/// Wrapper around [`StreamError`] which implements [`core::error::Error`]
/// with an appropriate error message.
#[derive(FromXml, AsXml, Debug)]
#[xml(transparent)]
pub struct ReceivedStreamError(pub StreamError);

impl fmt::Display for ReceivedStreamError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "received stream error: {}", self.0)
    }
}

impl Error for ReceivedStreamError {}

/// Wrapper around [`StreamError`] which implements [`core::error::Error`]
/// with an appropriate error message.
#[derive(FromXml, AsXml, Debug)]
#[xml(transparent)]
pub struct SentStreamError(pub StreamError);

impl fmt::Display for SentStreamError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "sent stream error: {}", self.0)
    }
}

impl Error for SentStreamError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_condition_from_prosody() {
        let doc = "<undefined-condition xmlns='urn:ietf:params:xml:ns:xmpp-streams'/>";
        let err: DefinedCondition = xso::from_bytes(doc.as_bytes()).unwrap();
        assert_eq!(err, DefinedCondition::UndefinedCondition);
    }

    #[test]
    fn parses_stream_error_from_prosody() {
        let doc = "<stream:error xmlns:stream='http://etherx.jabber.org/streams'><undefined-condition xmlns='urn:ietf:params:xml:ns:xmpp-streams'/><text xmlns='urn:ietf:params:xml:ns:xmpp-streams'>No stream features to proceed with</text></stream:error>";
        let err: StreamError = xso::from_bytes(doc.as_bytes()).unwrap();
        assert_eq!(err.condition, DefinedCondition::UndefinedCondition);
    }

    #[test]
    fn test_stream_error_with_text() {
        let doc = br#"<stream:error xmlns:stream='http://etherx.jabber.org/streams'>
            <system-shutdown xmlns='urn:ietf:params:xml:ns:xmpp-streams'/>
            <text xmlns='urn:ietf:params:xml:ns:xmpp-streams'>Server is shutting down for maintenance.</text>
        </stream:error>"#;

        let err: StreamError = xso::from_bytes(doc).unwrap();
        assert_eq!(err.condition, DefinedCondition::SystemShutdown);
        assert!(err.has_text());

        let (lang, text) = err.get_best_text(vec![]).unwrap();
        assert_eq!(text, "Server is shutting down for maintenance.");
        assert_eq!(lang, "");
    }

    #[test]
    fn test_stream_error_with_multiple_languages() {
        let doc = br#"<stream:error xmlns:stream='http://etherx.jabber.org/streams'>
            <policy-violation xmlns='urn:ietf:params:xml:ns:xmpp-streams'/>
            <text xmlns='urn:ietf:params:xml:ns:xmpp-streams' xml:lang='en'>Message too large</text>
            <text xmlns='urn:ietf:params:xml:ns:xmpp-streams' xml:lang='de'>Nachricht zu lang</text>
        </stream:error>"#;

        let err: StreamError = xso::from_bytes(doc).unwrap();
        assert_eq!(err.condition, DefinedCondition::PolicyViolation);

        // Test German preference
        let (lang, text) = err.get_best_text(vec!["de"]).unwrap();
        assert_eq!(lang, "de");
        assert_eq!(text, "Nachricht zu lang");

        // Test English preference
        let (lang, text) = err.get_best_text(vec!["en"]).unwrap();
        assert_eq!(lang, "en");
        assert_eq!(text, "Message too large");

        // Test cloned variant
        let (lang, text) = err.get_best_text_cloned(vec!["en"]).unwrap();
        assert_eq!(lang, "en");
        assert_eq!(text, "Message too large");
    }

    #[test]
    fn test_stream_error_constructors() {
        let err = StreamError::new(DefinedCondition::Reset, "en", "Connection reset");
        let (lang, text) = err.get_best_text(vec!["en"]).unwrap();
        assert_eq!(lang, "en");
        assert_eq!(text, "Connection reset");

        let err = err.add_text("de", "Verbindung zurückgesetzt");
        assert_eq!(err.texts.len(), 2);
    }
}
