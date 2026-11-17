// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{text::EmptyAsNone, AsXml, FromXml};

use crate::date::DateTime;
use crate::message::MessagePayload;
use crate::ns;
use crate::presence::PresencePayload;
use jid::Jid;

/// Notes when and by whom a message got stored for later delivery.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::DELAY, name = "delay")]
pub struct Delay {
    /// The entity which delayed this message.
    #[xml(attribute(default))]
    pub from: Option<Jid>,

    /// The time at which this message got stored.
    #[xml(attribute)]
    pub stamp: DateTime,

    /// The optional reason this message got delayed.
    #[xml(text = EmptyAsNone)]
    pub data: Option<String>,
}

impl MessagePayload for Delay {}
impl PresencePayload for Delay {}

#[cfg(test)]
mod tests {
    use super::*;
    use core::str::FromStr;
    use jid::BareJid;
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Delay, 44);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Delay, 72);
    }

    #[test]
    fn test_simple() {
        let elem: Element =
            "<delay xmlns='urn:xmpp:delay' from='capulet.com' stamp='2002-09-10T23:08:25Z'/>"
                .parse()
                .unwrap();
        let delay = Delay::try_from(elem).unwrap();
        assert_eq!(delay.from.unwrap(), BareJid::new("capulet.com").unwrap());
        assert_eq!(
            delay.stamp,
            DateTime::from_str("2002-09-10T23:08:25Z").unwrap()
        );
        assert_eq!(delay.data, None);
    }

    #[test]
    fn test_unknown() {
        let elem: Element = "<replace xmlns='urn:xmpp:message-correct:0'/>"
            .parse()
            .unwrap();
        let error = Delay::try_from(elem.clone()).unwrap_err();
        let returned_elem = match error {
            FromElementError::Mismatch(elem) => elem,
            _ => panic!(),
        };
        assert_eq!(elem, returned_elem);
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_invalid_child() {
        let elem: Element =
            "<delay xmlns='urn:xmpp:delay' stamp='2002-09-10T23:08:25+00:00'><coucou/></delay>"
                .parse()
                .unwrap();
        let error = Delay::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in Delay element.");
    }

    #[test]
    fn test_serialise() {
        let elem: Element = "<delay xmlns='urn:xmpp:delay' stamp='2002-09-10T23:08:25+00:00'/>"
            .parse()
            .unwrap();
        let delay = Delay {
            from: None,
            stamp: DateTime::from_str("2002-09-10T23:08:25Z").unwrap(),
            data: None,
        };
        let elem2 = delay.into();
        assert_eq!(elem, elem2);
    }

    #[test]
    fn test_serialise_data() {
        let elem: Element = "<delay xmlns='urn:xmpp:delay' from='juliet@example.org' stamp='2002-09-10T23:08:25+00:00'>Reason</delay>".parse().unwrap();
        let delay = Delay {
            from: Some(Jid::new("juliet@example.org").unwrap()),
            stamp: DateTime::from_str("2002-09-10T23:08:25Z").unwrap(),
            data: Some(String::from("Reason")),
        };
        let elem2 = delay.into();
        assert_eq!(elem, elem2);
    }
}
