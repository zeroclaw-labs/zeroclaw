// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::message::MessagePayload;
use crate::ns;

/// Enum representing chatstate elements part of the
/// `http://jabber.org/protocol/chatstates` namespace.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::CHATSTATES, exhaustive)]
pub enum ChatState {
    /// `<active xmlns='http://jabber.org/protocol/chatstates'/>`
    #[xml(name = "active")]
    Active,

    /// `<composing xmlns='http://jabber.org/protocol/chatstates'/>`
    #[xml(name = "composing")]
    Composing,

    /// `<gone xmlns='http://jabber.org/protocol/chatstates'/>`
    #[xml(name = "gone")]
    Gone,

    /// `<inactive xmlns='http://jabber.org/protocol/chatstates'/>`
    #[xml(name = "inactive")]
    Inactive,

    /// `<paused xmlns='http://jabber.org/protocol/chatstates'/>`
    #[xml(name = "paused")]
    Paused,
}

impl MessagePayload for ChatState {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ns;
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[test]
    fn test_size() {
        assert_size!(ChatState, 1);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<active xmlns='http://jabber.org/protocol/chatstates'/>"
            .parse()
            .unwrap();
        ChatState::try_from(elem).unwrap();
    }

    #[test]
    fn test_invalid() {
        let elem: Element = "<coucou xmlns='http://jabber.org/protocol/chatstates'/>"
            .parse()
            .unwrap();
        let error = ChatState::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "This is not a ChatState element.");
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_invalid_child() {
        let elem: Element = "<gone xmlns='http://jabber.org/protocol/chatstates'><coucou/></gone>"
            .parse()
            .unwrap();
        let error = ChatState::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in ChatState::Gone element.");
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_invalid_attribute() {
        let elem: Element = "<inactive xmlns='http://jabber.org/protocol/chatstates' coucou=''/>"
            .parse()
            .unwrap();
        let error = ChatState::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown attribute in ChatState::Inactive element.");
    }

    #[test]
    fn test_serialise() {
        let chatstate = ChatState::Active;
        let elem: Element = chatstate.into();
        assert!(elem.is("active", ns::CHATSTATES));
    }
}
