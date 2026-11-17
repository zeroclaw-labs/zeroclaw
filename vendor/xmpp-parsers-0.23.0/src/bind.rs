// Copyright (c) 2018 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::iq::{IqResultPayload, IqSetPayload};
use crate::ns;
use jid::{FullJid, Jid};

/// The bind feature exposed in stream:features.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::BIND, name = "bind")]
pub struct BindFeature {
    /// Present if bind is required.
    #[xml(flag)]
    pub required: bool,
}

/// The request for resource binding, which is the process by which a client
/// can obtain a full JID and start exchanging on the XMPP network.
///
/// See <https://xmpp.org/rfcs/rfc6120.html#bind>
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::BIND, name = "bind")]
pub struct BindQuery {
    /// Requests this resource, the server may associate another one though.
    ///
    /// If this is None, we request no particular resource, and a random one
    /// will be affected by the server.
    #[xml(extract(default, fields(text(type_ = String))))]
    pub resource: Option<String>,
}

impl BindQuery {
    /// Creates a resource binding request.
    pub fn new(resource: Option<String>) -> BindQuery {
        BindQuery { resource }
    }
}

impl IqSetPayload for BindQuery {}

/// The response for resource binding, containing the client’s full JID.
///
/// See <https://xmpp.org/rfcs/rfc6120.html#bind>
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::BIND, name = "bind")]
pub struct BindResponse {
    /// The full JID returned by the server for this client.
    #[xml(extract(fields(text(type_ = FullJid))))]
    pub jid: FullJid,
}

impl IqResultPayload for BindResponse {}

impl From<BindResponse> for FullJid {
    fn from(bind: BindResponse) -> FullJid {
        bind.jid
    }
}

impl From<BindResponse> for Jid {
    fn from(bind: BindResponse) -> Jid {
        Jid::from(bind.jid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    #[cfg(feature = "pedantic")]
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(BindFeature, 1);
        assert_size!(BindQuery, 12);
        assert_size!(BindResponse, 16);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(BindFeature, 1);
        assert_size!(BindQuery, 24);
        assert_size!(BindResponse, 32);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/>"
            .parse()
            .unwrap();
        let bind = BindQuery::try_from(elem).unwrap();
        assert_eq!(bind.resource, None);

        let elem: Element =
            "<bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><resource>Hello™</resource></bind>"
                .parse()
                .unwrap();
        let bind = BindQuery::try_from(elem).unwrap();
        // FIXME: “™” should be resourceprep’d into “TM” here…
        //assert_eq!(bind.resource.unwrap(), "HelloTM");
        assert_eq!(bind.resource.unwrap(), "Hello™");

        let elem: Element = "<bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><jid>coucou@linkmauve.fr/Hello™</jid></bind>"
            .parse()
            .unwrap();
        let bind = BindResponse::try_from(elem).unwrap();
        assert_eq!(
            bind.jid,
            FullJid::new("coucou@linkmauve.fr/HelloTM").unwrap()
        );
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_invalid_resource() {
        let elem: Element = "<bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><resource attr='coucou'>resource</resource></bind>"
            .parse()
            .unwrap();
        let error = BindQuery::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Unknown attribute in extraction for field 'resource' in BindQuery element."
        );

        let elem: Element = "<bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><resource><hello-world/>resource</resource></bind>"
            .parse()
            .unwrap();
        let error = BindQuery::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Unknown child in extraction for field 'resource' in BindQuery element."
        );
    }
}
