// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
// Copyright (c) 2017 Maxime “pep” Buquet <pep@bouah.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{error::Error, AsOptionalXmlText, AsXml, AsXmlText, FromXml, FromXmlText};

use crate::message::Lang;
use crate::ns;
use alloc::{borrow::Cow, collections::BTreeMap};
use jid::Jid;
use minidom::Element;

/// Should be implemented on every known payload of a `<presence/>`.
pub trait PresencePayload: TryFrom<Element> + Into<Element> {}

/// Specifies the availability of an entity or resource.
#[derive(Debug, Clone, PartialEq)]
pub enum Show {
    /// The entity or resource is temporarily away.
    Away,

    /// The entity or resource is actively interested in chatting.
    Chat,

    /// The entity or resource is busy (dnd = "Do Not Disturb").
    Dnd,

    /// The entity or resource is away for an extended period (xa = "eXtended
    /// Away").
    Xa,
}

impl FromXmlText for Show {
    fn from_xml_text(s: String) -> Result<Show, Error> {
        Ok(match s.as_ref() {
            "away" => Show::Away,
            "chat" => Show::Chat,
            "dnd" => Show::Dnd,
            "xa" => Show::Xa,

            _ => return Err(Error::Other("Invalid value for show.")),
        })
    }
}

impl AsXmlText for Show {
    fn as_xml_text(&self) -> Result<Cow<'_, str>, Error> {
        Ok(Cow::Borrowed(match self {
            Show::Away => "away",
            Show::Chat => "chat",
            Show::Dnd => "dnd",
            Show::Xa => "xa",
        }))
    }
}

type Status = String;

/// Priority of this presence.  This value can go from -128 to 127, defaults to
/// 0, and any negative value will prevent this resource from receiving
/// messages addressed to the bare JID.
#[derive(FromXml, AsXml, Debug, Default, Clone, PartialEq)]
#[xml(namespace = ns::DEFAULT_NS, name = "priority")]
pub struct Priority(#[xml(text)] pub i8);

/// Accepted values for the 'type' attribute of a presence.
#[derive(Debug, Default, Clone, PartialEq)]
pub enum Type {
    /// This value is not an acceptable 'type' attribute, it is only used
    /// internally to signal the absence of 'type'.
    #[default]
    None,

    /// An error has occurred regarding processing of a previously sent
    /// presence stanza; if the presence stanza is of type "error", it MUST
    /// include an \<error/\> child element (refer to
    /// [XMPP‑CORE](https://xmpp.org/rfcs/rfc6120.html)).
    Error,

    /// A request for an entity's current presence; SHOULD be generated only by
    /// a server on behalf of a user.
    Probe,

    /// The sender wishes to subscribe to the recipient's presence.
    Subscribe,

    /// The sender has allowed the recipient to receive their presence.
    Subscribed,

    /// The sender is no longer available for communication.
    Unavailable,

    /// The sender is unsubscribing from the receiver's presence.
    Unsubscribe,

    /// The subscription request has been denied or a previously granted
    /// subscription has been canceled.
    Unsubscribed,
}

impl FromXmlText for Type {
    fn from_xml_text(s: String) -> Result<Type, Error> {
        Ok(match s.as_ref() {
            "error" => Type::Error,
            "probe" => Type::Probe,
            "subscribe" => Type::Subscribe,
            "subscribed" => Type::Subscribed,
            "unavailable" => Type::Unavailable,
            "unsubscribe" => Type::Unsubscribe,
            "unsubscribed" => Type::Unsubscribed,

            _ => {
                return Err(Error::Other(
                    "Invalid 'type' attribute on presence element.",
                ));
            }
        })
    }
}

impl AsOptionalXmlText for Type {
    fn as_optional_xml_text(&self) -> Result<Option<Cow<'_, str>>, Error> {
        Ok(Some(Cow::Borrowed(match self {
            Type::None => return Ok(None),

            Type::Error => "error",
            Type::Probe => "probe",
            Type::Subscribe => "subscribe",
            Type::Subscribed => "subscribed",
            Type::Unavailable => "unavailable",
            Type::Unsubscribe => "unsubscribe",
            Type::Unsubscribed => "unsubscribed",
        })))
    }
}

/// The main structure representing the `<presence/>` stanza.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::DEFAULT_NS, name = "presence")]
pub struct Presence {
    /// The sender of this presence.
    #[xml(attribute(default))]
    pub from: Option<Jid>,

    /// The recipient of this presence.
    #[xml(attribute(default))]
    pub to: Option<Jid>,

    /// The identifier, unique on this stream, of this stanza.
    #[xml(attribute(default))]
    pub id: Option<String>,

    /// The type of this presence stanza.
    #[xml(attribute(default, name = "type"))]
    pub type_: Type,

    /// The availability of the sender of this presence.
    #[xml(extract(name = "show", default, fields(text(type_ = Show))))]
    pub show: Option<Show>,

    /// A localised list of statuses defined in this presence.
    #[xml(extract(n = .., name = "status", fields(
        lang(type_ = Lang, default),
        text(type_ = String),
    )))]
    pub statuses: BTreeMap<Lang, Status>,

    /// The sender’s resource priority, if negative it won’t receive messages
    /// that haven’t been directed to it.
    #[xml(child(default))]
    pub priority: Priority,

    /// A list of payloads contained in this presence.
    #[xml(element(n = ..))]
    pub payloads: Vec<Element>,
}

impl Presence {
    /// Create a new presence of this type.
    pub fn new(type_: Type) -> Presence {
        Presence {
            from: None,
            to: None,
            id: None,
            type_,
            show: None,
            statuses: BTreeMap::new(),
            priority: Priority(0i8),
            payloads: vec![],
        }
    }

    /// Create a presence without a type, which means available
    pub fn available() -> Presence {
        Self::new(Type::None)
    }

    /// Builds a presence of type Error
    pub fn error() -> Presence {
        Self::new(Type::Error)
    }

    /// Builds a presence of type Probe
    pub fn probe() -> Presence {
        Self::new(Type::Probe)
    }

    /// Builds a presence of type Subscribe
    pub fn subscribe() -> Presence {
        Self::new(Type::Subscribe)
    }

    /// Builds a presence of type Subscribed
    pub fn subscribed() -> Presence {
        Self::new(Type::Subscribed)
    }

    /// Builds a presence of type Unavailable
    pub fn unavailable() -> Presence {
        Self::new(Type::Unavailable)
    }

    /// Builds a presence of type Unsubscribe
    pub fn unsubscribe() -> Presence {
        Self::new(Type::Unsubscribe)
    }

    /// Set the emitter of this presence, this should only be useful for
    /// servers and components, as clients can only send presences from their
    /// own resource (which is implicit).
    pub fn with_from<J: Into<Jid>>(mut self, from: J) -> Presence {
        self.from = Some(from.into());
        self
    }

    /// Set the recipient of this presence, this is only useful for directed
    /// presences.
    pub fn with_to<J: Into<Jid>>(mut self, to: J) -> Presence {
        self.to = Some(to.into());
        self
    }

    /// Set the identifier for this presence.
    pub fn with_id(mut self, id: String) -> Presence {
        self.id = Some(id);
        self
    }

    /// Set the availability information of this presence.
    pub fn with_show(mut self, show: Show) -> Presence {
        self.show = Some(show);
        self
    }

    /// Set the priority of this presence.
    pub fn with_priority(mut self, priority: i8) -> Presence {
        self.priority = Priority(priority);
        self
    }

    /// Set a payload inside this presence.
    pub fn with_payload<P: PresencePayload>(mut self, payload: P) -> Presence {
        self.payloads.push(payload.into());
        self
    }

    /// Set the payloads of this presence.
    pub fn with_payloads(mut self, payloads: Vec<Element>) -> Presence {
        self.payloads = payloads;
        self
    }

    /// Set the availability information of this presence.
    pub fn set_status<L, S>(&mut self, lang: L, status: S)
    where
        L: Into<Lang>,
        S: Into<Status>,
    {
        self.statuses.insert(lang.into(), status.into());
    }

    /// Add a payload to this presence.
    pub fn add_payload<P: PresencePayload>(&mut self, payload: P) {
        self.payloads.push(payload.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jid::{BareJid, FullJid};
    use xso::error::FromElementError;
    use xso::exports::rxml;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Show, 1);
        assert_size!(Type, 1);
        assert_size!(Presence, 72);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Show, 1);
        assert_size!(Type, 1);
        assert_size!(Presence, 144);
    }

    #[test]
    fn test_simple() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<presence xmlns='jabber:client'/>".parse().unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<presence xmlns='jabber:component:accept'/>"
            .parse()
            .unwrap();
        let presence = Presence::try_from(elem).unwrap();
        assert_eq!(presence.from, None);
        assert_eq!(presence.to, None);
        assert_eq!(presence.id, None);
        assert_eq!(presence.type_, Type::None);
        assert!(presence.payloads.is_empty());
    }

    // TODO: This test is currently ignored because it serializes <priority/>
    // always, so let’s implement that in xso first.  The only downside to
    // having it included is some more bytes on the wire, we can live with that
    // for now.
    #[test]
    #[ignore]
    fn test_serialise() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<presence xmlns='jabber:client' type='unavailable'/>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<presence xmlns='jabber:component:accept' type='unavailable'/>"
            .parse()
            .unwrap();
        let presence = Presence::new(Type::Unavailable);
        let elem2 = presence.into();
        assert_eq!(elem, elem2);
    }

    #[test]
    fn test_show() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<presence xmlns='jabber:client'><show>chat</show></presence>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element =
            "<presence xmlns='jabber:component:accept'><show>chat</show></presence>"
                .parse()
                .unwrap();
        let presence = Presence::try_from(elem).unwrap();
        assert_eq!(presence.payloads.len(), 0);
        assert_eq!(presence.show, Some(Show::Chat));
    }

    #[test]
    fn test_empty_show_value() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<presence xmlns='jabber:client'/>".parse().unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<presence xmlns='jabber:component:accept'/>"
            .parse()
            .unwrap();
        let presence = Presence::try_from(elem).unwrap();
        assert_eq!(presence.show, None);
    }

    #[test]
    fn test_missing_show_value() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<presence xmlns='jabber:client'><show/></presence>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<presence xmlns='jabber:component:accept'><show/></presence>"
            .parse()
            .unwrap();
        let error = Presence::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Invalid value for show.");
    }

    #[test]
    fn test_invalid_show() {
        // "online" used to be a pretty common mistake.
        #[cfg(not(feature = "component"))]
        let elem: Element = "<presence xmlns='jabber:client'><show>online</show></presence>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element =
            "<presence xmlns='jabber:component:accept'><show>online</show></presence>"
                .parse()
                .unwrap();
        let error = Presence::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Invalid value for show.");
    }

    #[test]
    fn test_empty_status() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<presence xmlns='jabber:client'><status/></presence>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<presence xmlns='jabber:component:accept'><status/></presence>"
            .parse()
            .unwrap();
        let presence = Presence::try_from(elem).unwrap();
        assert_eq!(presence.payloads.len(), 0);
        assert_eq!(presence.statuses.len(), 1);
        assert_eq!(presence.statuses[""], "");
    }

    #[test]
    fn test_status() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<presence xmlns='jabber:client'><status>Here!</status></presence>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element =
            "<presence xmlns='jabber:component:accept'><status>Here!</status></presence>"
                .parse()
                .unwrap();
        let presence = Presence::try_from(elem).unwrap();
        assert_eq!(presence.payloads.len(), 0);
        assert_eq!(presence.statuses.len(), 1);
        assert_eq!(presence.statuses[""], "Here!");
    }

    #[test]
    fn test_multiple_statuses() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<presence xmlns='jabber:client'><status>Here!</status><status xml:lang='fr'>Là!</status></presence>".parse().unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<presence xmlns='jabber:component:accept'><status>Here!</status><status xml:lang='fr'>Là!</status></presence>".parse().unwrap();
        let presence = Presence::try_from(elem).unwrap();
        assert_eq!(presence.payloads.len(), 0);
        assert_eq!(presence.statuses.len(), 2);
        assert_eq!(presence.statuses[""], "Here!");
        assert_eq!(presence.statuses["fr"], "Là!");
    }

    // TODO: Enable that test again once xso supports rejecting multiple
    // identical xml:lang versions.
    #[test]
    #[ignore]
    fn test_invalid_multiple_statuses() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<presence xmlns='jabber:client'><status xml:lang='fr'>Here!</status><status xml:lang='fr'>Là!</status></presence>".parse().unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<presence xmlns='jabber:component:accept'><status xml:lang='fr'>Here!</status><status xml:lang='fr'>Là!</status></presence>".parse().unwrap();
        let error = Presence::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Status element present twice for the same xml:lang."
        );
    }

    #[test]
    fn test_priority() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<presence xmlns='jabber:client'><priority>-1</priority></presence>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element =
            "<presence xmlns='jabber:component:accept'><priority>-1</priority></presence>"
                .parse()
                .unwrap();
        let presence = Presence::try_from(elem).unwrap();
        assert_eq!(presence.payloads.len(), 0);
        assert_eq!(presence.priority, Priority(-1i8));
    }

    #[test]
    fn test_invalid_priority() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<presence xmlns='jabber:client'><priority>128</priority></presence>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element =
            "<presence xmlns='jabber:component:accept'><priority>128</priority></presence>"
                .parse()
                .unwrap();
        let error = Presence::try_from(elem).unwrap_err();
        match error {
            FromElementError::Invalid(Error::TextParseError(e))
                if e.is::<core::num::ParseIntError>() =>
            {
                ()
            }
            _ => panic!(),
        };
    }

    #[test]
    fn test_unknown_child() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<presence xmlns='jabber:client'><test xmlns='invalid'/></presence>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element =
            "<presence xmlns='jabber:component:accept'><test xmlns='invalid'/></presence>"
                .parse()
                .unwrap();
        let presence = Presence::try_from(elem).unwrap();
        let payload = &presence.payloads[0];
        assert!(payload.is("test", "invalid"));
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_invalid_status_child() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<presence xmlns='jabber:client'><status><coucou/></status></presence>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element =
            "<presence xmlns='jabber:component:accept'><status><coucou/></status></presence>"
                .parse()
                .unwrap();
        let error = Presence::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Unknown child in extraction for field 'statuses' in Presence element."
        );
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_invalid_attribute() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<presence xmlns='jabber:client'><status coucou=''/></presence>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element =
            "<presence xmlns='jabber:component:accept'><status coucou=''/></presence>"
                .parse()
                .unwrap();
        let error = Presence::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Unknown attribute in extraction for field 'statuses' in Presence element."
        );
    }

    #[test]
    fn test_serialise_status() {
        let status = Status::from("Hello world!");
        let mut presence = Presence::new(Type::Unavailable);
        presence.statuses.insert(Lang::from(""), status);
        let elem: Element = presence.into();
        assert!(elem.is("presence", ns::DEFAULT_NS));
        assert!(elem.children().next().unwrap().is("status", ns::DEFAULT_NS));
    }

    #[test]
    fn test_serialise_priority() {
        let presence = Presence::new(Type::None).with_priority(42);
        let elem: Element = presence.into();
        assert!(elem.is("presence", ns::DEFAULT_NS));
        let priority = elem.children().next().unwrap();
        assert!(priority.is("priority", ns::DEFAULT_NS));
        assert_eq!(priority.text(), "42");
    }

    #[test]
    fn presence_with_to() {
        let presence = Presence::new(Type::None);
        let elem: Element = presence.into();
        assert_eq!(elem.attr(rxml::xml_ncname!("to")), None);

        let presence = Presence::new(Type::None).with_to(Jid::new("localhost").unwrap());
        let elem: Element = presence.into();
        assert_eq!(elem.attr(rxml::xml_ncname!("to")), Some("localhost"));

        let presence = Presence::new(Type::None).with_to(BareJid::new("localhost").unwrap());
        let elem: Element = presence.into();
        assert_eq!(elem.attr(rxml::xml_ncname!("to")), Some("localhost"));

        let presence =
            Presence::new(Type::None).with_to(Jid::new("test@localhost/coucou").unwrap());
        let elem: Element = presence.into();
        assert_eq!(
            elem.attr(rxml::xml_ncname!("to")),
            Some("test@localhost/coucou")
        );

        let presence =
            Presence::new(Type::None).with_to(FullJid::new("test@localhost/coucou").unwrap());
        let elem: Element = presence.into();
        assert_eq!(
            elem.attr(rxml::xml_ncname!("to")),
            Some("test@localhost/coucou")
        );
    }

    #[test]
    fn test_xml_lang() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<presence xmlns='jabber:client' xml:lang='fr'/>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<presence xmlns='jabber:component:accept' xml:lang='fr'/>"
            .parse()
            .unwrap();
        let presence = Presence::try_from(elem).unwrap();
        assert_eq!(presence.from, None);
        assert_eq!(presence.to, None);
        assert_eq!(presence.id, None);
        assert_eq!(presence.type_, Type::None);
        assert!(presence.payloads.is_empty());
    }
}
