// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::message::{Id, MessagePayload};
use crate::ns;

/// Defines that the message containing this payload should replace a
/// previous message, identified by the id.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::MESSAGE_CORRECT, name = "replace")]
pub struct Replace {
    /// The 'id' attribute of the message getting corrected.
    #[xml(attribute)]
    pub id: Id,
}

impl MessagePayload for Replace {}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Replace, 12);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Replace, 24);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<replace xmlns='urn:xmpp:message-correct:0' id='coucou'/>"
            .parse()
            .unwrap();
        Replace::try_from(elem).unwrap();
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_invalid_attribute() {
        let elem: Element = "<replace xmlns='urn:xmpp:message-correct:0' id='coucou' coucou=''/>"
            .parse()
            .unwrap();
        let error = Replace::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown attribute in Replace element.");
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_invalid_child() {
        let elem: Element =
            "<replace xmlns='urn:xmpp:message-correct:0' id='coucou'><coucou/></replace>"
                .parse()
                .unwrap();
        let error = Replace::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in Replace element.");
    }

    #[test]
    fn test_invalid_id() {
        let elem: Element = "<replace xmlns='urn:xmpp:message-correct:0'/>"
            .parse()
            .unwrap();
        let error = Replace::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'id' on Replace element missing."
        );
    }

    #[test]
    fn test_serialise() {
        let elem: Element = "<replace xmlns='urn:xmpp:message-correct:0' id='coucou'/>"
            .parse()
            .unwrap();
        let replace = Replace {
            id: Id(String::from("coucou")),
        };
        let elem2 = replace.into();
        assert_eq!(elem, elem2);
    }
}
