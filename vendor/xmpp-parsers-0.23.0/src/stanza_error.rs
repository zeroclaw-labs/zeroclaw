// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{text::EmptyAsNone, AsXml, FromXml};

use crate::message::MessagePayload;
use crate::ns;
use crate::presence::PresencePayload;
use alloc::collections::BTreeMap;
use jid::Jid;
use minidom::Element;

generate_attribute!(
    /// The type of the error.
    ErrorType, "type", {
        /// Retry after providing credentials.
        Auth => "auth",

        /// Do not retry (the error cannot be remedied).
        Cancel => "cancel",

        /// Proceed (the condition was only a warning).
        Continue => "continue",

        /// Retry after changing the data sent.
        Modify => "modify",

        /// Retry after waiting (the error is temporary).
        Wait => "wait",
    }
);

/// List of valid error conditions.
// NOTE: This MUST NOT be marked as exhaustive, because the <text/> elements
// use the same namespace!
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::XMPP_STANZAS)]
pub enum DefinedCondition {
    /// The sender has sent a stanza containing XML that does not conform
    /// to the appropriate schema or that cannot be processed (e.g., an IQ
    /// stanza that includes an unrecognized value of the 'type' attribute,
    /// or an element that is qualified by a recognized namespace but that
    /// violates the defined syntax for the element); the associated error
    /// type SHOULD be "modify".
    #[xml(name = "bad-request")]
    BadRequest,

    /// Access cannot be granted because an existing resource exists with
    /// the same name or address; the associated error type SHOULD be
    /// "cancel".
    #[xml(name = "conflict")]
    Conflict,

    /// The feature represented in the XML stanza is not implemented by the
    /// intended recipient or an intermediate server and therefore the
    /// stanza cannot be processed (e.g., the entity understands the
    /// namespace but does not recognize the element name); the associated
    /// error type SHOULD be "cancel" or "modify".
    #[xml(name = "feature-not-implemented")]
    FeatureNotImplemented,

    /// The requesting entity does not possess the necessary permissions to
    /// perform an action that only certain authorized roles or individuals
    /// are allowed to complete (i.e., it typically relates to
    /// authorization rather than authentication); the associated error
    /// type SHOULD be "auth".
    #[xml(name = "forbidden")]
    Forbidden,

    /// The recipient or server can no longer be contacted at this address,
    /// typically on a permanent basis (as opposed to the \<redirect/\> error
    /// condition, which is used for temporary addressing failures); the
    /// associated error type SHOULD be "cancel" and the error stanza
    /// SHOULD include a new address (if available) as the XML character
    /// data of the \<gone/\> element (which MUST be a Uniform Resource
    /// Identifier (URI) or Internationalized Resource Identifier (IRI) at
    /// which the entity can be contacted, typically an XMPP IRI as
    /// specified in [XMPP‑URI](https://www.rfc-editor.org/rfc/rfc5122)).
    #[xml(name = "gone")]
    Gone {
        /// The new address of the entity for which the error was returned,
        /// if available.
        #[xml(text(codec = EmptyAsNone))]
        new_address: Option<String>,
    },

    /// The server has experienced a misconfiguration or other internal
    /// error that prevents it from processing the stanza; the associated
    /// error type SHOULD be "cancel".
    #[xml(name = "internal-server-error")]
    InternalServerError,

    /// The addressed JID or item requested cannot be found; the associated
    /// error type SHOULD be "cancel".
    #[xml(name = "item-not-found")]
    ItemNotFound,

    /// The sending entity has provided (e.g., during resource binding) or
    /// communicated (e.g., in the 'to' address of a stanza) an XMPP
    /// address or aspect thereof that violates the rules defined in
    /// [XMPP‑ADDR]; the associated error type SHOULD be "modify".
    #[xml(name = "jid-malformed")]
    JidMalformed,

    /// The recipient or server understands the request but cannot process
    /// it because the request does not meet criteria defined by the
    /// recipient or server (e.g., a request to subscribe to information
    /// that does not simultaneously include configuration parameters
    /// needed by the recipient); the associated error type SHOULD be
    /// "modify".
    #[xml(name = "not-acceptable")]
    NotAcceptable,

    /// The recipient or server does not allow any entity to perform the
    /// action (e.g., sending to entities at a blacklisted domain); the
    /// associated error type SHOULD be "cancel".
    #[xml(name = "not-allowed")]
    NotAllowed,

    /// The sender needs to provide credentials before being allowed to
    /// perform the action, or has provided improper credentials (the name
    /// "not-authorized", which was borrowed from the "401 Unauthorized"
    /// error of HTTP, might lead the reader to think that this condition
    /// relates to authorization, but instead it is typically used in
    /// relation to authentication); the associated error type SHOULD be
    /// "auth".
    #[xml(name = "not-authorized")]
    NotAuthorized,

    /// The entity has violated some local service policy (e.g., a message
    /// contains words that are prohibited by the service) and the server
    /// MAY choose to specify the policy in the \<text/\> element or in an
    /// application-specific condition element; the associated error type
    /// SHOULD be "modify" or "wait" depending on the policy being
    /// violated.
    #[xml(name = "policy-violation")]
    PolicyViolation,

    /// The intended recipient is temporarily unavailable, undergoing
    /// maintenance, etc.; the associated error type SHOULD be "wait".
    #[xml(name = "recipient-unavailable")]
    RecipientUnavailable,

    /// The recipient or server is redirecting requests for this
    /// information to another entity, typically in a temporary fashion (as
    /// opposed to the \<gone/\> error condition, which is used for permanent
    /// addressing failures); the associated error type SHOULD be "modify"
    /// and the error stanza SHOULD contain the alternate address in the
    /// XML character data of the \<redirect/\> element (which MUST be a URI
    /// or IRI with which the sender can communicate, typically an XMPP IRI
    /// as specified in [XMPP‑URI](https://xmpp.org/rfcs/rfc5122.html)).
    #[xml(name = "redirect")]
    Redirect {
        /// The new address of the entity for which the error was returned,
        /// if available.
        #[xml(text(codec = EmptyAsNone))]
        new_address: Option<String>,
    },

    /// The requesting entity is not authorized to access the requested
    /// service because prior registration is necessary (examples of prior
    /// registration include members-only rooms in XMPP multi-user chat
    /// [XEP‑0045] and gateways to non-XMPP instant messaging services,
    /// which traditionally required registration in order to use the
    /// gateway [XEP‑0100]); the associated error type SHOULD be "auth".
    #[xml(name = "registration-required")]
    RegistrationRequired,

    /// A remote server or service specified as part or all of the JID of
    /// the intended recipient does not exist or cannot be resolved (e.g.,
    /// there is no _xmpp-server._tcp DNS SRV record, the A or AAAA
    /// fallback resolution fails, or A/AAAA lookups succeed but there is
    /// no response on the IANA-registered port 5269); the associated error
    /// type SHOULD be "cancel".
    #[xml(name = "remote-server-not-found")]
    RemoteServerNotFound,

    /// A remote server or service specified as part or all of the JID of
    /// the intended recipient (or needed to fulfill a request) was
    /// resolved but communications could not be established within a
    /// reasonable amount of time (e.g., an XML stream cannot be
    /// established at the resolved IP address and port, or an XML stream
    /// can be established but stream negotiation fails because of problems
    /// with TLS, SASL, Server Dialback, etc.); the associated error type
    /// SHOULD be "wait" (unless the error is of a more permanent nature,
    /// e.g., the remote server is found but it cannot be authenticated or
    /// it violates security policies).
    #[xml(name = "remote-server-timeout")]
    RemoteServerTimeout,

    /// The server or recipient is busy or lacks the system resources
    /// necessary to service the request; the associated error type SHOULD
    /// be "wait".
    #[xml(name = "resource-constraint")]
    ResourceConstraint,

    /// The server or recipient does not currently provide the requested
    /// service; the associated error type SHOULD be "cancel".
    #[xml(name = "service-unavailable")]
    ServiceUnavailable,

    /// The requesting entity is not authorized to access the requested
    /// service because a prior subscription is necessary (examples of
    /// prior subscription include authorization to receive presence
    /// information as defined in [XMPP‑IM] and opt-in data feeds for XMPP
    /// publish-subscribe as defined in [XEP‑0060]); the associated error
    /// type SHOULD be "auth".
    #[xml(name = "subscription-required")]
    SubscriptionRequired,

    /// The error condition is not one of those defined by the other
    /// conditions in this list; any error type can be associated with this
    /// condition, and it SHOULD NOT be used except in conjunction with an
    /// application-specific condition.
    #[xml(name = "undefined-condition")]
    UndefinedCondition,

    /// The recipient or server understood the request but was not
    /// expecting it at this time (e.g., the request was out of order); the
    /// associated error type SHOULD be "wait" or "modify".
    #[xml(name = "unexpected-request")]
    UnexpectedRequest,
}

type Lang = String;

/// The representation of a stanza error.
#[derive(Debug, Clone, PartialEq, FromXml, AsXml)]
#[xml(namespace = ns::DEFAULT_NS, name = "error", discard(attribute = "code"))]
pub struct StanzaError {
    /// The type of this error.
    #[xml(attribute = "type")]
    pub type_: ErrorType,

    /// The JID of the entity who set this error.
    #[xml(attribute(name = "by", default))]
    pub by: Option<Jid>,

    /// One of the defined conditions for this error to happen.
    #[xml(child)]
    pub defined_condition: DefinedCondition,

    /// Human-readable description of this error.
    #[xml(extract(n = .., namespace = ns::XMPP_STANZAS, name = "text", fields(
        lang(type_ = Lang, default),
        text(type_ = String),
    )))]
    pub texts: BTreeMap<Lang, String>,

    /// A protocol-specific extension for this error.
    #[xml(element(default))]
    pub other: Option<Element>,
}

impl MessagePayload for StanzaError {}
impl PresencePayload for StanzaError {}

impl StanzaError {
    /// Create a new `<error/>` with the according content.
    pub fn new<L, T>(
        type_: ErrorType,
        defined_condition: DefinedCondition,
        lang: L,
        text: T,
    ) -> StanzaError
    where
        L: Into<Lang>,
        T: Into<String>,
    {
        StanzaError {
            type_,
            by: None,
            defined_condition,
            texts: {
                let mut map = BTreeMap::new();
                map.insert(lang.into(), text.into());
                map
            },
            other: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(ErrorType, 1);
        assert_size!(DefinedCondition, 16);
        assert_size!(StanzaError, 108);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(ErrorType, 1);
        assert_size!(DefinedCondition, 32);
        assert_size!(StanzaError, 208);
    }

    #[test]
    fn test_simple() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<error xmlns='jabber:client' type='cancel'><undefined-condition xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/></error>".parse().unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<error xmlns='jabber:component:accept' type='cancel'><undefined-condition xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/></error>".parse().unwrap();
        let error = StanzaError::try_from(elem).unwrap();
        assert_eq!(error.type_, ErrorType::Cancel);
        assert_eq!(
            error.defined_condition,
            DefinedCondition::UndefinedCondition
        );
    }

    #[test]
    fn test_invalid_type() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<error xmlns='jabber:client'/>".parse().unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<error xmlns='jabber:component:accept'/>".parse().unwrap();
        let error = StanzaError::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'type_' on StanzaError element missing."
        );

        #[cfg(not(feature = "component"))]
        let elem: Element = "<error xmlns='jabber:client' type='coucou'/>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<error xmlns='jabber:component:accept' type='coucou'/>"
            .parse()
            .unwrap();
        let error = StanzaError::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message.to_string(), "Unknown value for 'type' attribute.");
    }

    #[test]
    fn test_invalid_condition() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<error xmlns='jabber:client' type='cancel'/>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<error xmlns='jabber:component:accept' type='cancel'/>"
            .parse()
            .unwrap();
        let error = StanzaError::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Missing child field 'defined_condition' in StanzaError element."
        );
    }

    #[test]
    fn test_error_code() {
        #[cfg(not(feature = "component"))]
        let elem: Element = r#"<error code="501" type="cancel" xmlns='jabber:client'>
    <feature-not-implemented xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>
    <text xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'>The feature requested is not implemented by the recipient or server and therefore cannot be processed.</text>
</error>"#
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element = r#"<error code="501" type="cancel" xmlns='jabber:component:accept'>
    <feature-not-implemented xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>
    <text xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'>The feature requested is not implemented by the recipient or server and therefore cannot be processed.</text>
</error>"#
            .parse()
            .unwrap();
        let stanza_error = StanzaError::try_from(elem).unwrap();
        assert_eq!(stanza_error.type_, ErrorType::Cancel);
    }

    #[test]
    fn test_error_multiple_text() {
        #[cfg(not(feature = "component"))]
        let elem: Element = r#"<error type="cancel" xmlns='jabber:client'>
    <item-not-found xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>
    <text xmlns='urn:ietf:params:xml:ns:xmpp-stanzas' xml:lang="fr">Nœud non trouvé</text>
    <text xmlns='urn:ietf:params:xml:ns:xmpp-stanzas' xml:lang="en">Node not found</text>
</error>"#
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element = r#"<error type="cancel" xmlns='jabber:component:accept'>
    <item-not-found xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>
    <text xmlns='urn:ietf:params:xml:ns:xmpp-stanzas' xml:lang="fr">Nœud non trouvé</text>
    <text xmlns='urn:ietf:params:xml:ns:xmpp-stanzas' xml:lang="en">Node not found</text>
</error>"#
            .parse()
            .unwrap();
        let stanza_error = StanzaError::try_from(elem).unwrap();
        assert_eq!(stanza_error.type_, ErrorType::Cancel);
    }

    #[test]
    fn test_gone_with_new_address() {
        #[cfg(not(feature = "component"))]
            let elem: Element = "<error xmlns='jabber:client' type='cancel'><gone xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'>xmpp:room@muc.example.org?join</gone></error>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
            let elem: Element = "<error xmlns='jabber:component:accept' type='cancel'><gone xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'>xmpp:room@muc.example.org?join</gone></error>"
            .parse()
            .unwrap();
        let error = StanzaError::try_from(elem).unwrap();
        assert_eq!(error.type_, ErrorType::Cancel);
        assert_eq!(
            error.defined_condition,
            DefinedCondition::Gone {
                new_address: Some("xmpp:room@muc.example.org?join".to_string()),
            }
        );
    }

    #[test]
    fn test_gone_without_new_address() {
        #[cfg(not(feature = "component"))]
            let elem: Element = "<error xmlns='jabber:client' type='cancel'><gone xmlns='urn:ietf:params:xml:ns:xmpp-stanzas' /></error>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
            let elem: Element = "<error xmlns='jabber:component:accept' type='cancel'><gone xmlns='urn:ietf:params:xml:ns:xmpp-stanzas' /></error>"
            .parse()
            .unwrap();
        let error = StanzaError::try_from(elem).unwrap();
        assert_eq!(error.type_, ErrorType::Cancel);
        assert_eq!(
            error.defined_condition,
            DefinedCondition::Gone { new_address: None }
        );
    }

    #[test]
    fn test_redirect_with_alternate_address() {
        #[cfg(not(feature = "component"))]
            let elem: Element = "<error xmlns='jabber:client' type='modify'><redirect xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'>xmpp:characters@conference.example.org</redirect></error>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
            let elem: Element = "<error xmlns='jabber:component:accept' type='modify'><redirect xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'>xmpp:characters@conference.example.org</redirect></error>"
            .parse()
            .unwrap();
        let error = StanzaError::try_from(elem).unwrap();
        assert_eq!(error.type_, ErrorType::Modify);
        assert_eq!(
            error.defined_condition,
            DefinedCondition::Redirect {
                new_address: Some("xmpp:characters@conference.example.org".to_string()),
            }
        );
    }

    #[test]
    fn test_redirect_without_alternate_address() {
        #[cfg(not(feature = "component"))]
            let elem: Element = "<error xmlns='jabber:client' type='modify'><redirect xmlns='urn:ietf:params:xml:ns:xmpp-stanzas' /></error>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
            let elem: Element = "<error xmlns='jabber:component:accept' type='modify'><redirect xmlns='urn:ietf:params:xml:ns:xmpp-stanzas' /></error>"
            .parse()
            .unwrap();
        let error = StanzaError::try_from(elem).unwrap();
        assert_eq!(error.type_, ErrorType::Modify);
        assert_eq!(
            error.defined_condition,
            DefinedCondition::Redirect { new_address: None }
        );
    }
}
