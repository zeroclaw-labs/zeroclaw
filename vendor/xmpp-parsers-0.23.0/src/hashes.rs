// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use alloc::borrow::Cow;
use core::{
    fmt::Write,
    num::ParseIntError,
    ops::{Deref, DerefMut},
    str::FromStr,
};

use xso::{error::Error, text::Base64, AsXml, AsXmlText, FromXml, FromXmlText};

use base64::{engine::general_purpose::STANDARD as Base64Engine, Engine};
use minidom::IntoAttributeValue;

use crate::ns;

/// List of the algorithms we support, or Unknown.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Algo {
    /// The Secure Hash Algorithm 1, with known vulnerabilities, do not use it.
    ///
    /// See <https://www.rfc-editor.org/rfc/rfc3174>
    Sha_1,

    /// The Secure Hash Algorithm 2, in its 256-bit version.
    ///
    /// See <https://www.rfc-editor.org/rfc/rfc6234>
    Sha_256,

    /// The Secure Hash Algorithm 2, in its 512-bit version.
    ///
    /// See <https://www.rfc-editor.org/rfc/rfc6234>
    Sha_512,

    /// The Secure Hash Algorithm 3, based on Keccak, in its 256-bit version.
    ///
    /// See <https://keccak.team/files/Keccak-submission-3.pdf>
    Sha3_256,

    /// The Secure Hash Algorithm 3, based on Keccak, in its 512-bit version.
    ///
    /// See <https://keccak.team/files/Keccak-submission-3.pdf>
    Sha3_512,

    /// The BLAKE2 hash algorithm, for a 256-bit output.
    ///
    /// See <https://www.rfc-editor.org/rfc/rfc7693>
    Blake2b_256,

    /// The BLAKE2 hash algorithm, for a 512-bit output.
    ///
    /// See <https://www.rfc-editor.org/rfc/rfc7693>
    Blake2b_512,

    /// An unknown hash not in this list, you can probably reject it.
    Unknown(String),
}

impl FromStr for Algo {
    type Err = Error;

    fn from_str(s: &str) -> Result<Algo, Error> {
        Ok(match s {
            "" => return Err(Error::Other("'algo' argument can’t be empty.")),

            "sha-1" => Algo::Sha_1,
            "sha-256" => Algo::Sha_256,
            "sha-512" => Algo::Sha_512,
            "sha3-256" => Algo::Sha3_256,
            "sha3-512" => Algo::Sha3_512,
            "blake2b-256" => Algo::Blake2b_256,
            "blake2b-512" => Algo::Blake2b_512,
            value => Algo::Unknown(value.to_owned()),
        })
    }
}

impl From<Algo> for String {
    fn from(algo: Algo) -> String {
        String::from(match algo {
            Algo::Sha_1 => "sha-1",
            Algo::Sha_256 => "sha-256",
            Algo::Sha_512 => "sha-512",
            Algo::Sha3_256 => "sha3-256",
            Algo::Sha3_512 => "sha3-512",
            Algo::Blake2b_256 => "blake2b-256",
            Algo::Blake2b_512 => "blake2b-512",
            Algo::Unknown(text) => return text,
        })
    }
}

impl FromXmlText for Algo {
    fn from_xml_text(value: String) -> Result<Self, Error> {
        value.parse().map_err(Error::text_parse_error)
    }
}

impl AsXmlText for Algo {
    fn as_xml_text(&self) -> Result<Cow<'_, str>, Error> {
        Ok(Cow::Borrowed(match self {
            Algo::Sha_1 => "sha-1",
            Algo::Sha_256 => "sha-256",
            Algo::Sha_512 => "sha-512",
            Algo::Sha3_256 => "sha3-256",
            Algo::Sha3_512 => "sha3-512",
            Algo::Blake2b_256 => "blake2b-256",
            Algo::Blake2b_512 => "blake2b-512",
            Algo::Unknown(text) => text.as_str(),
        }))
    }
}

impl IntoAttributeValue for Algo {
    fn into_attribute_value(self) -> Option<String> {
        Some(String::from(self))
    }
}

/// This element represents a hash of some data, defined by the hash
/// algorithm used and the computed value.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::HASHES, name = "hash")]
pub struct Hash {
    /// The algorithm used to create this hash.
    #[xml(attribute)]
    pub algo: Algo,

    /// The hash value, as a vector of bytes.
    #[xml(text = Base64)]
    pub hash: Vec<u8>,
}

impl Hash {
    /// Creates a [struct@Hash] element with the given algo and data.
    pub fn new(algo: Algo, hash: Vec<u8>) -> Hash {
        Hash { algo, hash }
    }

    /// Like [new](#method.new) but takes base64-encoded data before decoding
    /// it.
    pub fn from_base64(algo: Algo, hash: &str) -> Result<Hash, Error> {
        Ok(Hash::new(
            algo,
            Base64Engine.decode(hash).map_err(Error::text_parse_error)?,
        ))
    }

    /// Like [new](#method.new) but takes hex-encoded data before decoding it.
    pub fn from_hex(algo: Algo, hex: &str) -> Result<Hash, ParseIntError> {
        let mut bytes = vec![];
        for i in 0..hex.len() / 2 {
            let byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)?;
            bytes.push(byte);
        }
        Ok(Hash::new(algo, bytes))
    }

    /// Like [new](#method.new) but takes hex-encoded data before decoding it.
    pub fn from_colon_separated_hex(algo: Algo, hex: &str) -> Result<Hash, ParseIntError> {
        let mut bytes = vec![];
        for i in 0..(1 + hex.len()) / 3 {
            let byte = u8::from_str_radix(&hex[3 * i..3 * i + 2], 16)?;
            if 3 * i + 2 < hex.len() {
                assert_eq!(&hex[3 * i + 2..3 * i + 3], ":");
            }
            bytes.push(byte);
        }
        Ok(Hash::new(algo, bytes))
    }

    /// Formats this hash into base64.
    pub fn to_base64(&self) -> String {
        Base64Engine.encode(&self.hash[..])
    }

    /// Formats this hash into hexadecimal.
    pub fn to_hex(&self) -> String {
        let mut hex = String::with_capacity(self.hash.len() * 2);
        for byte in &self.hash {
            write!(&mut hex, "{:02x}", byte).unwrap();
        }
        hex
    }

    /// Formats this hash into colon-separated hexadecimal.
    pub fn to_colon_separated_hex(&self) -> String {
        self.hash
            .iter()
            .map(|byte| format!("{:02x}", byte))
            .collect::<Vec<_>>()
            .join(":")
    }
}

/// Helper for parsing and serialising a SHA-1 attribute.
#[derive(Debug, Clone, PartialEq)]
pub struct Sha1HexAttribute(Hash);

impl FromStr for Sha1HexAttribute {
    type Err = ParseIntError;

    fn from_str(hex: &str) -> Result<Self, Self::Err> {
        let hash = Hash::from_hex(Algo::Sha_1, hex)?;
        Ok(Sha1HexAttribute(hash))
    }
}

impl FromXmlText for Sha1HexAttribute {
    fn from_xml_text(s: String) -> Result<Self, xso::error::Error> {
        Self::from_str(&s).map_err(xso::error::Error::text_parse_error)
    }
}

impl AsXmlText for Sha1HexAttribute {
    fn as_xml_text(&self) -> Result<Cow<'_, str>, xso::error::Error> {
        Ok(Cow::Owned(self.to_hex()))
    }
}

impl IntoAttributeValue for Sha1HexAttribute {
    fn into_attribute_value(self) -> Option<String> {
        Some(self.to_hex())
    }
}

impl DerefMut for Sha1HexAttribute {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Deref for Sha1HexAttribute {
    type Target = Hash;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xso::error::FromElementError;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Algo, 12);
        assert_size!(Hash, 24);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Algo, 24);
        assert_size!(Hash, 48);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>2XarmwTlNxDAMkvymloX3S5+VbylNrJt/l5QyPa+YoU=</hash>".parse().unwrap();
        let hash = Hash::try_from(elem).unwrap();
        assert_eq!(hash.algo, Algo::Sha_256);
        assert_eq!(
            hash.hash,
            Base64Engine
                .decode("2XarmwTlNxDAMkvymloX3S5+VbylNrJt/l5QyPa+YoU=")
                .unwrap()
        );
    }

    #[test]
    fn value_serialisation() {
        let elem: Element = "<hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>2XarmwTlNxDAMkvymloX3S5+VbylNrJt/l5QyPa+YoU=</hash>".parse().unwrap();
        let hash = Hash::try_from(elem).unwrap();
        assert_eq!(
            hash.to_base64(),
            "2XarmwTlNxDAMkvymloX3S5+VbylNrJt/l5QyPa+YoU="
        );
        assert_eq!(
            hash.to_hex(),
            "d976ab9b04e53710c0324bf29a5a17dd2e7e55bca536b26dfe5e50c8f6be6285"
        );
        assert_eq!(hash.to_colon_separated_hex(), "d9:76:ab:9b:04:e5:37:10:c0:32:4b:f2:9a:5a:17:dd:2e:7e:55:bc:a5:36:b2:6d:fe:5e:50:c8:f6:be:62:85");
    }

    #[test]
    fn test_unknown() {
        let elem: Element = "<replace xmlns='urn:xmpp:message-correct:0'/>"
            .parse()
            .unwrap();
        let error = Hash::try_from(elem.clone()).unwrap_err();
        let returned_elem = match error {
            FromElementError::Mismatch(elem) => elem,
            _ => panic!(),
        };
        assert_eq!(elem, returned_elem);
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_invalid_child() {
        let elem: Element = "<hash xmlns='urn:xmpp:hashes:2' algo='sha-1'><coucou/></hash>"
            .parse()
            .unwrap();
        let error = Hash::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in Hash element.");
    }
}
