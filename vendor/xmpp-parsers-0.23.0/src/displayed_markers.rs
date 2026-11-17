// Copyright (c) 2026 Paul Fariello <paul@fariello.eu>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::message::{Id, MessagePayload};
use crate::ns;

/// Request to know if the recipient has displayed a message.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::DISPLAYED_MARKERS, name = "markable")]
pub struct Markable;

impl MessagePayload for Markable {}

/// Inform that all messages up to a given message have been displayed.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::DISPLAYED_MARKERS, name = "displayed")]
pub struct Displayed {
    /// The 'id' attribute of the displayed message.
    #[xml(attribute)]
    pub id: Id,
}

impl MessagePayload for Displayed {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ns;
    use minidom::Element;
    use xso::error::{Error, FromElementError};
    use xso::exports::rxml;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Markable, 0);
        assert_size!(Displayed, 12);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Markable, 0);
        assert_size!(Displayed, 24);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<markable xmlns='urn:xmpp:chat-markers:0'/>"
            .parse()
            .unwrap();
        Markable::try_from(elem).unwrap();

        let elem: Element = "<displayed xmlns='urn:xmpp:chat-markers:0' id='39K7ZYIp'/>"
            .parse()
            .unwrap();
        let displayed = Displayed::try_from(elem).unwrap();
        assert_eq!(displayed.id, Id(String::from("39K7ZYIp")));
    }

    #[test]
    fn test_missing_id() {
        let elem: Element = "<displayed xmlns='urn:xmpp:chat-markers:0'/>"
            .parse()
            .unwrap();
        let error = Displayed::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'id' on Displayed element missing."
        );
    }

    #[test]
    fn test_serialise() {
        let receipt = Markable;
        let elem: Element = receipt.into();
        assert!(elem.is("markable", ns::DISPLAYED_MARKERS));
        assert_eq!(elem.attrs().into_iter().count(), 0);

        let receipt = Displayed {
            id: Id(String::from("39K7ZYIp")),
        };
        let elem: Element = receipt.into();
        assert!(elem.is("displayed", ns::DISPLAYED_MARKERS));
        assert_eq!(elem.attr(rxml::xml_ncname!("id")), Some("39K7ZYIp"));
    }
}
