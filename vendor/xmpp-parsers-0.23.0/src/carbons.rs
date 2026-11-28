// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::forwarding::Forwarded;
use crate::iq::IqSetPayload;
use crate::message::MessagePayload;
use crate::ns;

/// Enable carbons for this session.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::CARBONS, name = "enable")]
pub struct Enable;

impl IqSetPayload for Enable {}

/// Disable a previously-enabled carbons.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::CARBONS, name = "disable")]
pub struct Disable;

impl IqSetPayload for Disable {}

/// Request the enclosing message to not be copied to other carbons-enabled
/// resources of the user.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::CARBONS, name = "private")]
pub struct Private;

impl MessagePayload for Private {}

/// Wrapper for a message received on another resource.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::CARBONS, name = "received")]
pub struct Received {
    /// Wrapper for the enclosed message.
    #[xml(child)]
    pub forwarded: Forwarded,
}

impl MessagePayload for Received {}

/// Wrapper for a message sent from another resource.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::CARBONS, name = "sent")]
pub struct Sent {
    /// Wrapper for the enclosed message.
    #[xml(child)]
    pub forwarded: Forwarded,
}

impl MessagePayload for Sent {}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(feature = "component"))]
    use jid::Jid;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Enable, 0);
        assert_size!(Disable, 0);
        assert_size!(Private, 0);
        assert_size!(Received, 152);
        assert_size!(Sent, 152);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Enable, 0);
        assert_size!(Disable, 0);
        assert_size!(Private, 0);
        assert_size!(Received, 288);
        assert_size!(Sent, 288);
    }

    #[test]
    fn empty_elements() {
        let elem: Element = "<enable xmlns='urn:xmpp:carbons:2'/>".parse().unwrap();
        Enable::try_from(elem).unwrap();

        let elem: Element = "<disable xmlns='urn:xmpp:carbons:2'/>".parse().unwrap();
        Disable::try_from(elem).unwrap();

        let elem: Element = "<private xmlns='urn:xmpp:carbons:2'/>".parse().unwrap();
        Private::try_from(elem).unwrap();
    }

    #[test]
    #[cfg(not(feature = "component"))] // feature = "component" changes <message/> namespace
    fn forwarded_elements() {
        let elem: Element = "<received xmlns='urn:xmpp:carbons:2'>
  <forwarded xmlns='urn:xmpp:forward:0'>
    <message xmlns='jabber:client'
             to='juliet@capulet.example/balcony'
             from='romeo@montague.example/home'/>
  </forwarded>
</received>"
            .parse()
            .unwrap();
        let received = Received::try_from(elem).unwrap();
        assert_eq!(
            received.forwarded.message.to.unwrap(),
            Jid::new("juliet@capulet.example/balcony").unwrap()
        );
        assert_eq!(
            received.forwarded.message.from.unwrap(),
            Jid::new("romeo@montague.example/home").unwrap()
        );

        let elem: Element = "<sent xmlns='urn:xmpp:carbons:2'>
  <forwarded xmlns='urn:xmpp:forward:0'>
    <message xmlns='jabber:client'
             to='juliet@capulet.example/balcony'
             from='romeo@montague.example/home'/>
  </forwarded>
</sent>"
            .parse()
            .unwrap();
        let sent = Sent::try_from(elem).unwrap();
        assert_eq!(
            sent.forwarded.message.to.unwrap(),
            Jid::new("juliet@capulet.example/balcony").unwrap()
        );
        assert_eq!(
            sent.forwarded.message.from.unwrap(),
            Jid::new("romeo@montague.example/home").unwrap()
        );
    }

    #[test]
    #[cfg(not(feature = "component"))] // feature = "component" changes <message/> namespace
    fn test_serialize_received() {
        let reference: Element = "<received xmlns='urn:xmpp:carbons:2'><forwarded xmlns='urn:xmpp:forward:0'><message xmlns='jabber:client' to='juliet@capulet.example/balcony' from='romeo@montague.example/home'/></forwarded></received>"
        .parse()
        .unwrap();

        let elem: Element = "<forwarded xmlns='urn:xmpp:forward:0'><message xmlns='jabber:client' to='juliet@capulet.example/balcony' from='romeo@montague.example/home'/></forwarded>"
          .parse()
          .unwrap();
        let forwarded = Forwarded::try_from(elem).unwrap();

        let received = Received {
            forwarded: forwarded,
        };

        let serialized: Element = received.into();
        assert_eq!(serialized, reference);
    }

    #[test]
    #[cfg(not(feature = "component"))] // feature = "component" changes <message/> namespace
    fn test_serialize_sent() {
        let reference: Element = "<sent xmlns='urn:xmpp:carbons:2'><forwarded xmlns='urn:xmpp:forward:0'><message xmlns='jabber:client' to='juliet@capulet.example/balcony' from='romeo@montague.example/home'/></forwarded></sent>"
        .parse()
        .unwrap();

        let elem: Element = "<forwarded xmlns='urn:xmpp:forward:0'><message xmlns='jabber:client' to='juliet@capulet.example/balcony' from='romeo@montague.example/home'/></forwarded>"
          .parse()
          .unwrap();
        let forwarded = Forwarded::try_from(elem).unwrap();

        let sent = Sent {
            forwarded: forwarded,
        };

        let serialized: Element = sent.into();
        assert_eq!(serialized, reference);
    }
}
