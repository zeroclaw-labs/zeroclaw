// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::delay::Delay;
use crate::message::Message;
use crate::ns;

/// Contains a forwarded stanza, either standalone or part of another
/// extension (such as carbons).
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::FORWARD, name = "forwarded")]
pub struct Forwarded {
    /// When the stanza originally got sent.
    #[xml(child(default))]
    pub delay: Option<Delay>,

    /// The stanza being forwarded.
    // The schema says that we should allow either a Message, Presence or Iq, in either
    // jabber:client or jabber:server, but in the wild so far we’ve only seen Message being
    // transmitted, so let’s hardcode that for now.  The schema also makes it optional, but so far
    // it’s always present (or this wrapper is useless).
    #[xml(child)]
    pub message: Message,
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Forwarded, 152);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Forwarded, 288);
    }

    #[test]
    #[cfg(not(feature = "component"))] // feature = "component" changes <message/> namespace
    fn test_simple() {
        let elem: Element =
            "<forwarded xmlns='urn:xmpp:forward:0'><message xmlns='jabber:client'/></forwarded>"
                .parse()
                .unwrap();
        Forwarded::try_from(elem).unwrap();
    }

    #[test]
    #[cfg(not(feature = "component"))] // feature = "component" changes <message/> namespace
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_invalid_child() {
        let elem: Element = "<forwarded xmlns='urn:xmpp:forward:0'><message xmlns='jabber:client'/><coucou/></forwarded>"
            .parse()
            .unwrap();
        let error = Forwarded::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in Forwarded element.");
    }

    #[test]
    #[cfg(not(feature = "component"))] // feature = "component" changes <message/> namespace
    fn test_serialise() {
        let elem: Element = "<forwarded xmlns='urn:xmpp:forward:0'><message xmlns='jabber:client' type='chat'/></forwarded>".parse().unwrap();
        let forwarded = Forwarded {
            delay: None,
            message: Message::new(None),
        };
        let elem2 = forwarded.into();
        assert_eq!(elem, elem2);
    }

    #[test]
    #[cfg(not(feature = "component"))] // feature = "component" changes <message/> namespace
    fn test_serialize_with_delay_and_stanza() {
        let reference: Element = "<forwarded xmlns='urn:xmpp:forward:0'><delay xmlns='urn:xmpp:delay' from='capulet.com' stamp='2002-09-10T23:08:25+00:00'/><message xmlns='jabber:client' to='juliet@capulet.example/balcony' from='romeo@montague.example/home'/></forwarded>"
        .parse()
        .unwrap();

        let elem: Element = "<message xmlns='jabber:client' to='juliet@capulet.example/balcony' from='romeo@montague.example/home'/>"
          .parse()
          .unwrap();
        let message = Message::try_from(elem).unwrap();

        let elem: Element =
            "<delay xmlns='urn:xmpp:delay' from='capulet.com' stamp='2002-09-10T23:08:25Z'/>"
                .parse()
                .unwrap();
        let delay = Delay::try_from(elem).unwrap();

        let forwarded = Forwarded {
            delay: Some(delay),
            message,
        };

        let serialized: Element = forwarded.into();
        assert_eq!(serialized, reference);
    }

    #[test]
    fn test_invalid_duplicate_delay() {
        let elem: Element = "<forwarded xmlns='urn:xmpp:forward:0'><delay xmlns='urn:xmpp:delay' from='capulet.com' stamp='2002-09-10T23:08:25+00:00'/><delay xmlns='urn:xmpp:delay' from='capulet.com' stamp='2002-09-10T23:08:25+00:00'/><message xmlns='jabber:client' to='juliet@capulet.example/balcony' from='romeo@montague.example/home'/></forwarded>"
            .parse()
            .unwrap();
        let error = Forwarded::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Forwarded element must not have more than one child in field 'delay'."
        );
    }

    #[test]
    #[cfg(not(feature = "component"))] // feature = "component" changes <message/> namespace
    fn test_invalid_duplicate_message() {
        let elem: Element = "<forwarded xmlns='urn:xmpp:forward:0'><delay xmlns='urn:xmpp:delay' from='capulet.com' stamp='2002-09-10T23:08:25+00:00'/><message xmlns='jabber:client' to='juliet@capulet.example/balcony' from='romeo@montague.example/home'/><message xmlns='jabber:client' to='juliet@capulet.example/balcony' from='romeo@montague.example/home'/></forwarded>"
            .parse()
            .unwrap();
        let error = Forwarded::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Forwarded element must not have more than one child in field 'message'."
        );
    }
}
