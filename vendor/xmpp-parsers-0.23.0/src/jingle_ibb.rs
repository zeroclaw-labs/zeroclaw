// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::ibb::{Stanza, StreamId};
use crate::ns;

/// Describes an [In-Band Bytestream](https://xmpp.org/extensions/xep-0047.html)
/// Jingle transport, see also the [IBB module](../ibb.rs).
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::JINGLE_IBB, name = "transport")]
pub struct Transport {
    /// Maximum size in bytes for each chunk.
    #[xml(attribute(name = "block-size"))]
    pub block_size: u16,

    /// The identifier to be used to create a stream.
    #[xml(attribute)]
    pub sid: StreamId,

    /// Which stanza type to use to exchange data.
    #[xml(attribute(default))]
    pub stanza: Stanza,
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Transport, 16);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Transport, 32);
    }

    #[test]
    fn test_simple() {
        let elem: Element =
            "<transport xmlns='urn:xmpp:jingle:transports:ibb:1' block-size='3' sid='coucou'/>"
                .parse()
                .unwrap();
        let transport = Transport::try_from(elem).unwrap();
        assert_eq!(transport.block_size, 3);
        assert_eq!(transport.sid, StreamId(String::from("coucou")));
        assert_eq!(transport.stanza, Stanza::Iq);
    }

    #[test]
    fn test_invalid() {
        let elem: Element = "<transport xmlns='urn:xmpp:jingle:transports:ibb:1'/>"
            .parse()
            .unwrap();
        let error = Transport::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'block_size' on Transport element missing."
        );

        let elem: Element =
            "<transport xmlns='urn:xmpp:jingle:transports:ibb:1' block-size='65536'/>"
                .parse()
                .unwrap();
        let error = Transport::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(error))
                if error.is::<core::num::ParseIntError>() =>
            {
                error
            }
            _ => panic!(),
        };
        assert_eq!(
            message.to_string(),
            "number too large to fit in target type"
        );

        let elem: Element = "<transport xmlns='urn:xmpp:jingle:transports:ibb:1' block-size='-5'/>"
            .parse()
            .unwrap();
        let error = Transport::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(error))
                if error.is::<core::num::ParseIntError>() =>
            {
                error
            }
            _ => panic!(),
        };
        assert_eq!(message.to_string(), "invalid digit found in string");

        let elem: Element =
            "<transport xmlns='urn:xmpp:jingle:transports:ibb:1' block-size='128'/>"
                .parse()
                .unwrap();
        let error = Transport::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'sid' on Transport element missing."
        );
    }

    #[test]
    fn test_invalid_stanza() {
        let elem: Element = "<transport xmlns='urn:xmpp:jingle:transports:ibb:1' block-size='128' sid='coucou' stanza='fdsq'/>".parse().unwrap();
        let error = Transport::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message.to_string(), "Unknown value for 'stanza' attribute.");
    }
}
