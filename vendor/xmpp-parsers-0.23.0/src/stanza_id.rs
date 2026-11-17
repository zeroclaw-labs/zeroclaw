// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::message::MessagePayload;
use crate::ns;
use jid::Jid;

/// Gives the identifier a service has stamped on this stanza, often in
/// order to identify it inside of [an archive](../mam/index.html).
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SID, name = "stanza-id")]
pub struct StanzaId {
    /// The id associated to this stanza by another entity.
    #[xml(attribute)]
    pub id: String,

    /// The entity who stamped this stanza-id.
    #[xml(attribute)]
    pub by: Jid,
}

impl MessagePayload for StanzaId {}

/// A hack for MUC before version 1.31 to track a message which may have
/// its 'id' attribute changed.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SID, name = "origin-id")]
pub struct OriginId {
    /// The id this client set for this stanza.
    #[xml(attribute)]
    pub id: String,
}

impl MessagePayload for OriginId {}

#[cfg(test)]
mod tests {
    use super::*;
    use jid::BareJid;
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(StanzaId, 28);
        assert_size!(OriginId, 12);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(StanzaId, 56);
        assert_size!(OriginId, 24);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<stanza-id xmlns='urn:xmpp:sid:0' id='coucou' by='coucou@coucou'/>"
            .parse()
            .unwrap();
        let stanza_id = StanzaId::try_from(elem).unwrap();
        assert_eq!(stanza_id.id, String::from("coucou"));
        assert_eq!(stanza_id.by, BareJid::new("coucou@coucou").unwrap());

        let elem: Element = "<origin-id xmlns='urn:xmpp:sid:0' id='coucou'/>"
            .parse()
            .unwrap();
        let origin_id = OriginId::try_from(elem).unwrap();
        assert_eq!(origin_id.id, String::from("coucou"));
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_invalid_child() {
        let elem: Element =
            "<stanza-id xmlns='urn:xmpp:sid:0' by='a@b' id='x'><coucou/></stanza-id>"
                .parse()
                .unwrap();
        let error = StanzaId::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in StanzaId element.");
    }

    #[test]
    fn test_invalid_id() {
        let elem: Element = "<stanza-id xmlns='urn:xmpp:sid:0'/>".parse().unwrap();
        let error = StanzaId::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'id' on StanzaId element missing."
        );
    }

    #[test]
    fn test_invalid_by() {
        let elem: Element = "<stanza-id xmlns='urn:xmpp:sid:0' id='coucou'/>"
            .parse()
            .unwrap();
        let error = StanzaId::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'by' on StanzaId element missing."
        );
    }

    #[test]
    fn test_serialise() {
        let elem: Element = "<stanza-id xmlns='urn:xmpp:sid:0' id='coucou' by='coucou@coucou'/>"
            .parse()
            .unwrap();
        let stanza_id = StanzaId {
            id: String::from("coucou"),
            by: Jid::new("coucou@coucou").unwrap(),
        };
        let elem2 = stanza_id.into();
        assert_eq!(elem, elem2);
    }
}
