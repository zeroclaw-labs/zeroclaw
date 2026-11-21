// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
// Copyright (c) 2017 Maxime “pep” Buquet <pep@bouah.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use crate::ns;
use crate::stanza_error::StanzaError;
use jid::Jid;
use minidom::Element;
use xso::{AsXml, FromXml};

/// Should be implemented on every known payload of an `<iq type='get'/>`.
pub trait IqGetPayload: TryFrom<Element> + Into<Element> {}

/// Should be implemented on every known payload of an `<iq type='set'/>`.
pub trait IqSetPayload: TryFrom<Element> + Into<Element> {}

/// Should be implemented on every known payload of an `<iq type='result'/>`.
pub trait IqResultPayload: TryFrom<Element> + Into<Element> {}

/// Metadata of an IQ stanza.
pub struct IqHeader {
    /// The sender JID.
    pub from: Option<Jid>,

    /// The recipient JID.
    pub to: Option<Jid>,

    /// The stanza's ID.
    pub id: String,
}

impl IqHeader {
    /// Combine a header with [`IqPayload`] to create a full [`Iq`] stanza.
    pub fn assemble(self, data: IqPayload) -> Iq {
        data.assemble(self)
    }
}

/// Payload of an IQ request stanza.
pub enum IqRequestPayload {
    /// Payload of a type='get' stanza.
    Get(Element),

    /// Payload of a type='set' stanza.
    Set(Element),
}

/// Payload of an IQ stanza, by type.
pub enum IqPayload {
    /// Payload of a type='get' stanza.
    Get(Element),

    /// Payload of a type='set' stanza.
    Set(Element),

    /// Payload of a type='result' stanza.
    Result(Option<Element>),

    /// The error carried in a type='error' stanza.
    Error(StanzaError),
}

impl IqPayload {
    /// Combine the data with an [`IqHeader`] to create a full [`Iq`] stanza.
    pub fn assemble(self, IqHeader { from, to, id }: IqHeader) -> Iq {
        match self {
            Self::Get(payload) => Iq::Get {
                from,
                to,
                id,
                payload,
            },
            Self::Set(payload) => Iq::Set {
                from,
                to,
                id,
                payload,
            },
            Self::Result(payload) => Iq::Result {
                from,
                to,
                id,
                payload,
            },
            Self::Error(error) => Iq::Error {
                from,
                to,
                id,
                payload: None,
                error,
            },
        }
    }
}

/// The main structure representing the `<iq/>` stanza.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::DEFAULT_NS, name = "iq", attribute = "type", exhaustive)]
pub enum Iq {
    /// An `<iq type='get'/>` stanza.
    #[xml(value = "get")]
    Get {
        /// The JID emitting this stanza.
        #[xml(attribute(default))]
        from: Option<Jid>,

        /// The recipient of this stanza.
        #[xml(attribute(default))]
        to: Option<Jid>,

        /// The @id attribute of this stanza, which is required in order to match a
        /// request with its result/error.
        #[xml(attribute)]
        id: String,

        /// The payload content of this stanza.
        #[xml(element(n = 1))]
        payload: Element,
    },

    /// An `<iq type='set'/>` stanza.
    #[xml(value = "set")]
    Set {
        /// The JID emitting this stanza.
        #[xml(attribute(default))]
        from: Option<Jid>,

        /// The recipient of this stanza.
        #[xml(attribute(default))]
        to: Option<Jid>,

        /// The @id attribute of this stanza, which is required in order to match a
        /// request with its result/error.
        #[xml(attribute)]
        id: String,

        /// The payload content of this stanza.
        #[xml(element(n = 1))]
        payload: Element,
    },

    /// An `<iq type='result'/>` stanza.
    #[xml(value = "result")]
    Result {
        /// The JID emitting this stanza.
        #[xml(attribute(default))]
        from: Option<Jid>,

        /// The recipient of this stanza.
        #[xml(attribute(default))]
        to: Option<Jid>,

        /// The @id attribute of this stanza, which is required in order to match a
        /// request with its result/error.
        #[xml(attribute)]
        id: String,

        /// The payload content of this stanza.
        #[xml(element(n = 1, default))]
        payload: Option<Element>,
    },

    /// An `<iq type='error'/>` stanza.
    #[xml(value = "error")]
    Error {
        /// The JID emitting this stanza.
        #[xml(attribute(default))]
        from: Option<Jid>,

        /// The recipient of this stanza.
        #[xml(attribute(default))]
        to: Option<Jid>,

        /// The @id attribute of this stanza, which is required in order to match a
        /// request with its result/error.
        #[xml(attribute)]
        id: String,

        /// The error carried by this stanza.
        #[xml(child)]
        error: StanzaError,

        /// The optional payload content which caused the error.
        ///
        /// As per
        /// [RFC 6120 § 8.3.1](https://datatracker.ietf.org/doc/html/rfc6120#section-8.3.1),
        /// the emitter of an error stanza MAY include the original XML which
        /// caused the error. However, recipients MUST NOT rely on this.
        #[xml(element(n = 1, default))]
        payload: Option<Element>,
    },
}

impl Iq {
    /// Assemble a new Iq stanza from an [`IqHeader`] and the given
    /// [`IqPayload`].
    pub fn assemble(header: IqHeader, data: IqPayload) -> Self {
        data.assemble(header)
    }

    /// Creates an `<iq/>` stanza containing a get request.
    pub fn from_get<S: Into<String>>(id: S, payload: impl IqGetPayload) -> Iq {
        Iq::Get {
            from: None,
            to: None,
            id: id.into(),
            payload: payload.into(),
        }
    }

    /// Creates an `<iq/>` stanza containing a set request.
    pub fn from_set<S: Into<String>>(id: S, payload: impl IqSetPayload) -> Iq {
        Iq::Set {
            from: None,
            to: None,
            id: id.into(),
            payload: payload.into(),
        }
    }

    /// Creates an empty `<iq type="result"/>` stanza.
    pub fn empty_result<S: Into<String>>(to: Jid, id: S) -> Iq {
        Iq::Result {
            from: None,
            to: Some(to),
            id: id.into(),
            payload: None,
        }
    }

    /// Creates an `<iq/>` stanza containing a result.
    pub fn from_result<S: Into<String>>(id: S, payload: Option<impl IqResultPayload>) -> Iq {
        Iq::Result {
            from: None,
            to: None,
            id: id.into(),
            payload: payload.map(Into::into),
        }
    }

    /// Creates an `<iq/>` stanza containing an error.
    pub fn from_error<S: Into<String>>(id: S, payload: StanzaError) -> Iq {
        Iq::Error {
            from: None,
            to: None,
            id: id.into(),
            error: payload,
            payload: None,
        }
    }

    /// Sets the recipient of this stanza.
    pub fn with_to(mut self, to: Jid) -> Iq {
        *self.to_mut() = Some(to);
        self
    }

    /// Sets the emitter of this stanza.
    pub fn with_from(mut self, from: Jid) -> Iq {
        *self.from_mut() = Some(from);
        self
    }

    /// Sets the id of this stanza, in order to later match its response.
    pub fn with_id(mut self, id: String) -> Iq {
        *self.id_mut() = id;
        self
    }

    /// Access the sender address.
    pub fn from(&self) -> Option<&Jid> {
        match self {
            Self::Get { from, .. }
            | Self::Set { from, .. }
            | Self::Result { from, .. }
            | Self::Error { from, .. } => from.as_ref(),
        }
    }

    /// Access the sender address, mutably.
    pub fn from_mut(&mut self) -> &mut Option<Jid> {
        match self {
            Self::Get { ref mut from, .. }
            | Self::Set { ref mut from, .. }
            | Self::Result { ref mut from, .. }
            | Self::Error { ref mut from, .. } => from,
        }
    }

    /// Access the recipient address.
    pub fn to(&self) -> Option<&Jid> {
        match self {
            Self::Get { to, .. }
            | Self::Set { to, .. }
            | Self::Result { to, .. }
            | Self::Error { to, .. } => to.as_ref(),
        }
    }

    /// Access the recipient address, mutably.
    pub fn to_mut(&mut self) -> &mut Option<Jid> {
        match self {
            Self::Get { ref mut to, .. }
            | Self::Set { ref mut to, .. }
            | Self::Result { ref mut to, .. }
            | Self::Error { ref mut to, .. } => to,
        }
    }

    /// Access the id.
    pub fn id(&self) -> &str {
        match self {
            Self::Get { id, .. }
            | Self::Set { id, .. }
            | Self::Result { id, .. }
            | Self::Error { id, .. } => id.as_str(),
        }
    }

    /// Access the id mutably.
    pub fn id_mut(&mut self) -> &mut String {
        match self {
            Self::Get { ref mut id, .. }
            | Self::Set { ref mut id, .. }
            | Self::Result { ref mut id, .. }
            | Self::Error { ref mut id, .. } => id,
        }
    }

    /// Split the IQ stanza in its metadata and data.
    ///
    /// Note that this discards the optional original error-inducing
    /// [`payload`][`Self::Error::payload`] of the
    /// [`Iq::Error`][`Self::Error`] variant.
    pub fn split(self) -> (IqHeader, IqPayload) {
        match self {
            Self::Get {
                from,
                to,
                id,
                payload,
            } => (IqHeader { from, to, id }, IqPayload::Get(payload)),
            Self::Set {
                from,
                to,
                id,
                payload,
            } => (IqHeader { from, to, id }, IqPayload::Set(payload)),
            Self::Result {
                from,
                to,
                id,
                payload,
            } => (IqHeader { from, to, id }, IqPayload::Result(payload)),
            Self::Error {
                from,
                to,
                id,
                error,
                payload: _,
            } => (IqHeader { from, to, id }, IqPayload::Error(error)),
        }
    }

    /// Return the [`IqHeader`] of this stanza, discarding the payload.
    pub fn into_header(self) -> IqHeader {
        self.split().0
    }

    /// Return the [`IqPayload`] of this stanza, discarding the header.
    ///
    /// Note that this also discards the optional original error-inducing
    /// [`payload`][`Self::Error::payload`] of the
    /// [`Iq::Error`][`Self::Error`] variant.
    pub fn into_payload(self) -> IqPayload {
        self.split().1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disco::DiscoInfoQuery;
    use crate::stanza_error::{DefinedCondition, ErrorType};
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(IqHeader, 44);
        assert_size!(IqPayload, 108);
        assert_size!(Iq, 212);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(IqHeader, 88);
        assert_size!(IqPayload, 208);
        assert_size!(Iq, 408);
    }

    #[test]
    fn test_require_type() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<iq xmlns='jabber:client'/>".parse().unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<iq xmlns='jabber:component:accept'/>".parse().unwrap();
        let error = Iq::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Missing discriminator attribute.");
    }

    #[test]
    fn test_require_id() {
        for type_ in ["get", "set", "result", "error"] {
            #[cfg(not(feature = "component"))]
            let elem: Element = format!("<iq xmlns='jabber:client' type='{}'/>", type_)
                .parse()
                .unwrap();
            #[cfg(feature = "component")]
            let elem: Element = format!("<iq xmlns='jabber:component:accept' type='{}'/>", type_)
                .parse()
                .unwrap();
            let error = Iq::try_from(elem).unwrap_err();
            let message = match error {
                FromElementError::Invalid(Error::Other(string)) => string,
                _ => panic!(),
            };
            // Slicing here, because the rest of the error message is specific
            // about the enum variant.
            assert_eq!(&message[..33], "Required attribute field 'id' on ");
        }
    }

    #[test]
    fn test_get() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<iq xmlns='jabber:client' type='get' id='foo'>
            <foo xmlns='bar'/>
        </iq>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<iq xmlns='jabber:component:accept' type='get' id='foo'>
            <foo xmlns='bar'/>
        </iq>"
            .parse()
            .unwrap();
        let iq = Iq::try_from(elem).unwrap();
        let query: Element = "<foo xmlns='bar'/>".parse().unwrap();
        assert_eq!(iq.from(), None);
        assert_eq!(iq.to(), None);
        assert_eq!(iq.id(), "foo");
        assert!(match iq {
            Iq::Get { payload, .. } => payload == query,
            _ => false,
        });
    }

    #[test]
    fn test_set() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<iq xmlns='jabber:client' type='set' id='vcard'>
            <vCard xmlns='vcard-temp'/>
        </iq>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<iq xmlns='jabber:component:accept' type='set' id='vcard'>
            <vCard xmlns='vcard-temp'/>
        </iq>"
            .parse()
            .unwrap();
        let iq = Iq::try_from(elem).unwrap();
        let vcard: Element = "<vCard xmlns='vcard-temp'/>".parse().unwrap();
        assert_eq!(iq.from(), None);
        assert_eq!(iq.to(), None);
        assert_eq!(iq.id(), "vcard");
        assert!(match iq {
            Iq::Set { payload, .. } => payload == vcard,
            _ => false,
        });
    }

    #[test]
    fn test_result_empty() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<iq xmlns='jabber:client' type='result' id='res'/>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<iq xmlns='jabber:component:accept' type='result' id='res'/>"
            .parse()
            .unwrap();
        let iq = Iq::try_from(elem).unwrap();
        assert_eq!(iq.from(), None);
        assert_eq!(iq.to(), None);
        assert_eq!(iq.id(), "res");
        assert!(match iq {
            Iq::Result { payload: None, .. } => true,
            _ => false,
        });
    }

    #[test]
    fn test_result() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<iq xmlns='jabber:client' type='result' id='res'>
            <query xmlns='http://jabber.org/protocol/disco#items'/>
        </iq>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<iq xmlns='jabber:component:accept' type='result' id='res'>
            <query xmlns='http://jabber.org/protocol/disco#items'/>
        </iq>"
            .parse()
            .unwrap();
        let iq = Iq::try_from(elem).unwrap();
        let query: Element = "<query xmlns='http://jabber.org/protocol/disco#items'/>"
            .parse()
            .unwrap();
        assert_eq!(iq.from(), None);
        assert_eq!(iq.to(), None);
        assert_eq!(iq.id(), "res");
        assert!(match iq {
            Iq::Result {
                payload: Some(element),
                ..
            } => element == query,
            _ => false,
        });
    }

    #[test]
    fn test_error() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<iq xmlns='jabber:client' type='error' id='err1'>
            <ping xmlns='urn:xmpp:ping'/>
            <error type='cancel'>
                <service-unavailable xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>
            </error>
        </iq>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<iq xmlns='jabber:component:accept' type='error' id='err1'>
            <ping xmlns='urn:xmpp:ping'/>
            <error type='cancel'>
                <service-unavailable xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>
            </error>
        </iq>"
            .parse()
            .unwrap();
        let iq = Iq::try_from(elem).unwrap();
        assert_eq!(iq.from(), None);
        assert_eq!(iq.to(), None);
        assert_eq!(iq.id(), "err1");
        match iq {
            Iq::Error { error, .. } => {
                assert_eq!(error.type_, ErrorType::Cancel);
                assert_eq!(error.by, None);
                assert_eq!(
                    error.defined_condition,
                    DefinedCondition::ServiceUnavailable
                );
                assert_eq!(error.texts.len(), 0);
                assert_eq!(error.other, None);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_children_invalid() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<iq xmlns='jabber:client' type='error' id='error'/>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<iq xmlns='jabber:component:accept' type='error' id='error'/>"
            .parse()
            .unwrap();
        let error = Iq::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Missing child field 'error' in Iq::Error element.");
    }

    #[test]
    fn test_serialise() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<iq xmlns='jabber:client' type='result' id='res'/>"
            .parse()
            .unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<iq xmlns='jabber:component:accept' type='result' id='res'/>"
            .parse()
            .unwrap();
        let iq2 = Iq::Result {
            from: None,
            to: None,
            id: String::from("res"),
            payload: None,
        };
        let elem2 = iq2.into();
        assert_eq!(elem, elem2);
    }

    #[test]
    fn test_disco() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<iq xmlns='jabber:client' type='get' id='disco'><query xmlns='http://jabber.org/protocol/disco#info'/></iq>".parse().unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<iq xmlns='jabber:component:accept' type='get' id='disco'><query xmlns='http://jabber.org/protocol/disco#info'/></iq>".parse().unwrap();
        let iq = Iq::try_from(elem).unwrap();
        let disco_info = match iq {
            Iq::Get { payload, .. } => DiscoInfoQuery::try_from(payload).unwrap(),
            _ => panic!(),
        };
        assert!(disco_info.node.is_none());
    }
}
