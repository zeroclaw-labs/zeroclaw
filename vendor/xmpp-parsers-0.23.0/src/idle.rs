// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::date::DateTime;
use crate::ns;
use crate::presence::PresencePayload;

/// Represents the last time the user interacted with their system.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::IDLE, name = "idle")]
pub struct Idle {
    /// The time at which the user stopped interacting.
    #[xml(attribute)]
    pub since: DateTime,
}

impl PresencePayload for Idle {}

#[cfg(test)]
mod tests {
    use super::*;
    use core::str::FromStr;
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[test]
    fn test_size() {
        assert_size!(Idle, 16);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<idle xmlns='urn:xmpp:idle:1' since='2017-05-21T20:19:55+01:00'/>"
            .parse()
            .unwrap();
        Idle::try_from(elem).unwrap();
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_invalid_child() {
        let elem: Element =
            "<idle xmlns='urn:xmpp:idle:1' since='2017-05-21T20:19:55+01:00'><coucou/></idle>"
                .parse()
                .unwrap();
        let error = Idle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            other => panic!("unexpected result: {:?}", other),
        };
        assert_eq!(message, "Unknown child in Idle element.");
    }

    #[test]
    fn test_invalid_id() {
        let elem: Element = "<idle xmlns='urn:xmpp:idle:1'/>".parse().unwrap();
        let error = Idle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'since' on Idle element missing."
        );
    }

    #[test]
    fn test_invalid_date() {
        // There is no thirteenth month.
        let elem: Element = "<idle xmlns='urn:xmpp:idle:1' since='2017-13-01T12:23:34Z'/>"
            .parse()
            .unwrap();
        let error = Idle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(string))
                if string.is::<time::error::Parse>() =>
            {
                string
            }
            other => panic!("unexpected result: {:?}", other),
        };
        assert!(!message.to_string().is_empty());

        // Timezone ≥24:00 aren’t allowed.
        let elem: Element = "<idle xmlns='urn:xmpp:idle:1' since='2017-05-27T12:11:02+25:00'/>"
            .parse()
            .unwrap();
        let error = Idle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(string))
                if string.is::<time::error::Parse>() =>
            {
                string
            }
            _ => panic!(),
        };
        assert!(!message.to_string().is_empty());

        // Timezone without the : separator aren’t allowed.
        let elem: Element = "<idle xmlns='urn:xmpp:idle:1' since='2017-05-27T12:11:02+0100'/>"
            .parse()
            .unwrap();
        let error = Idle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(string))
                if string.is::<time::error::Parse>() =>
            {
                string
            }
            _ => panic!(),
        };
        assert!(!message.to_string().is_empty());

        // No seconds, error message could be improved.
        let elem: Element = "<idle xmlns='urn:xmpp:idle:1' since='2017-05-27T12:11+01:00'/>"
            .parse()
            .unwrap();
        let error = Idle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(string))
                if string.is::<time::error::Parse>() =>
            {
                string
            }
            _ => panic!(),
        };
        assert!(!message.to_string().is_empty());

        // TODO: maybe we’ll want to support this one, as per XEP-0082 §4.
        let elem: Element = "<idle xmlns='urn:xmpp:idle:1' since='20170527T12:11:02+01:00'/>"
            .parse()
            .unwrap();
        let error = Idle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(string))
                if string.is::<time::error::Parse>() =>
            {
                string
            }
            _ => panic!(),
        };
        assert!(!message.to_string().is_empty());

        // No timezone.
        let elem: Element = "<idle xmlns='urn:xmpp:idle:1' since='2017-05-27T12:11:02'/>"
            .parse()
            .unwrap();
        let error = Idle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(string))
                if string.is::<time::error::Parse>() =>
            {
                string
            }
            _ => panic!(),
        };
        assert!(!message.to_string().is_empty());
    }

    #[test]
    fn test_serialise() {
        let elem: Element = "<idle xmlns='urn:xmpp:idle:1' since='2017-05-21T20:19:55+01:00'/>"
            .parse()
            .unwrap();
        let idle = Idle {
            since: DateTime::from_str("2017-05-21T20:19:55+01:00").unwrap(),
        };
        let elem2 = idle.into();
        assert_eq!(elem, elem2);
    }
}
