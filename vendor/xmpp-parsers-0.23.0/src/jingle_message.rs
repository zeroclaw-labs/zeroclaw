// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::jingle::SessionId;
use crate::ns;
use minidom::Element;

/// Defines a protocol for broadcasting Jingle requests to all of the clients
/// of a user.
#[derive(FromXml, AsXml, Debug, Clone)]
#[xml(namespace = ns::JINGLE_MESSAGE, exhaustive)]
pub enum JingleMI {
    /// Indicates we want to start a Jingle session.
    #[xml(name = "propose")]
    Propose {
        /// The generated session identifier, must be unique between two users.
        #[xml(attribute = "id")]
        sid: SessionId,

        /// The application description of the proposed session.
        // TODO: Use a more specialised type here.
        #[xml(element)]
        description: Element,
    },

    /// Cancels a previously proposed session.
    #[xml(name = "retract")]
    Retract(#[xml(attribute = "id")] SessionId),

    /// Accepts a session proposed by the other party.
    #[xml(name = "accept")]
    Accept(#[xml(attribute = "id")] SessionId),

    /// Proceed with a previously proposed session.
    #[xml(name = "proceed")]
    Proceed(#[xml(attribute = "id")] SessionId),

    /// Rejects a session proposed by the other party.
    #[xml(name = "reject")]
    Reject(#[xml(attribute = "id")] SessionId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(JingleMI, 72);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(JingleMI, 136);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<accept xmlns='urn:xmpp:jingle-message:0' id='coucou'/>"
            .parse()
            .unwrap();
        JingleMI::try_from(elem).unwrap();
    }

    // TODO: Enable this test again once #[xml(element)] supports filtering on the element name.
    #[test]
    #[ignore]
    fn test_invalid_child() {
        let elem: Element =
            "<propose xmlns='urn:xmpp:jingle-message:0' id='coucou'><coucou/></propose>"
                .parse()
                .unwrap();
        let error = JingleMI::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in propose element.");
    }
}
