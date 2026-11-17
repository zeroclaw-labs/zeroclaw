// Copyright (c) 2018 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{text::FixedHex, AsXml, FromXml};

use crate::ns;
use digest::Digest;
use sha1::Sha1;

/// The main authentication mechanism for components.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone, Default)]
#[xml(namespace = ns::COMPONENT, name = "handshake")]
pub struct Handshake {
    /// If Some, contains the hex-encoded SHA-1 of the concatenation of the
    /// stream id and the password, and is used to authenticate against the
    /// server.
    ///
    /// If None, it is the successful reply from the server, the stream is now
    /// fully established and both sides can now exchange stanzas.
    #[xml(text(codec = FixedHex<20>))]
    pub data: Option<[u8; 20]>,
}

impl Handshake {
    /// Creates a successful reply from a server.
    pub fn new() -> Handshake {
        Handshake::default()
    }

    /// Creates an authentication request from the component.
    pub fn from_stream_id_and_password(stream_id: String, password: &str) -> Handshake {
        let input = stream_id + password;
        let hash = Sha1::digest(input.as_bytes());
        Handshake {
            data: Some(hash.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[test]
    fn test_size() {
        assert_size!(Handshake, 21);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<handshake xmlns='jabber:component:accept'/>"
            .parse()
            .unwrap();
        let handshake = Handshake::try_from(elem).unwrap();
        assert_eq!(handshake.data, None);

        let elem: Element = "<handshake xmlns='jabber:component:accept'>9accec263ab84a43c6037ccf7cd48cb1d3f6df8e</handshake>"
            .parse()
            .unwrap();
        let handshake = Handshake::try_from(elem).unwrap();
        assert_eq!(
            handshake.data,
            Some([
                0x9a, 0xcc, 0xec, 0x26, 0x3a, 0xb8, 0x4a, 0x43, 0xc6, 0x03, 0x7c, 0xcf, 0x7c, 0xd4,
                0x8c, 0xb1, 0xd3, 0xf6, 0xdf, 0x8e
            ])
        );
    }

    #[test]
    fn test_constructors() {
        let handshake = Handshake::new();
        assert_eq!(handshake.data, None);

        let stream_id = String::from("sid");
        let password = "123456";
        let handshake = Handshake::from_stream_id_and_password(stream_id, password);
        assert_eq!(
            handshake.data,
            Some([
                0x9a, 0xcc, 0xec, 0x26, 0x3a, 0xb8, 0x4a, 0x43, 0xc6, 0x03, 0x7c, 0xcf, 0x7c, 0xd4,
                0x8c, 0xb1, 0xd3, 0xf6, 0xdf, 0x8e
            ])
        );
    }
}
