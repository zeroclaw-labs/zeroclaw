// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{text::ColonSeparatedHex, AsXml, FromXml};

use crate::hashes::{Algo, Hash};
use crate::ns;
use xso::error::Error;

generate_attribute!(
    /// Indicates which of the end points should initiate the TCP connection establishment.
    Setup, "setup", {
        /// The endpoint will initiate an outgoing connection.
        Active => "active",

        /// The endpoint will accept an incoming connection.
        Passive => "passive",

        /// The endpoint is willing to accept an incoming connection or to initiate an outgoing
        /// connection.
        Actpass => "actpass",

        /*
        /// The endpoint does not want the connection to be established for the time being.
        ///
        /// Note that this value isn’t used, as per the XEP.
        Holdconn => "holdconn",
        */
    }
);

// TODO: use a hashes::Hash instead of two different fields here.
/// Fingerprint of the key used for a DTLS handshake.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::JINGLE_DTLS, name = "fingerprint")]
pub struct Fingerprint {
    /// The hash algorithm used for this fingerprint.
    #[xml(attribute)]
    pub hash: Algo,

    /// Indicates which of the end points should initiate the TCP connection establishment.
    #[xml(attribute)]
    pub setup: Setup,

    /// Hash value of this fingerprint.
    #[xml(text(codec = ColonSeparatedHex))]
    pub value: Vec<u8>,
}

impl Fingerprint {
    /// Create a new Fingerprint from a Setup and a Hash.
    pub fn from_hash(setup: Setup, hash: Hash) -> Fingerprint {
        Fingerprint {
            hash: hash.algo,
            setup,
            value: hash.hash,
        }
    }

    /// Create a new Fingerprint from a Setup and parsing the hash.
    pub fn from_colon_separated_hex(
        setup: Setup,
        algo: &str,
        hash: &str,
    ) -> Result<Fingerprint, Error> {
        let algo = algo.parse()?;
        let hash = Hash::from_colon_separated_hex(algo, hash).map_err(Error::text_parse_error)?;
        Ok(Fingerprint::from_hash(setup, hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Setup, 1);
        assert_size!(Fingerprint, 28);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Setup, 1);
        assert_size!(Fingerprint, 56);
    }

    #[test]
    fn test_ex1() {
        let elem: Element = "<fingerprint xmlns='urn:xmpp:jingle:apps:dtls:0' hash='sha-256' setup='actpass'>02:1A:CC:54:27:AB:EB:9C:53:3F:3E:4B:65:2E:7D:46:3F:54:42:CD:54:F1:7A:03:A2:7D:F9:B0:7F:46:19:B2</fingerprint>"
                .parse()
                .unwrap();
        let fingerprint = Fingerprint::try_from(elem).unwrap();
        assert_eq!(fingerprint.setup, Setup::Actpass);
        assert_eq!(fingerprint.hash, Algo::Sha_256);
        assert_eq!(
            fingerprint.value,
            [
                2, 26, 204, 84, 39, 171, 235, 156, 83, 63, 62, 75, 101, 46, 125, 70, 63, 84, 66,
                205, 84, 241, 122, 3, 162, 125, 249, 176, 127, 70, 25, 178
            ]
        );
    }
}
