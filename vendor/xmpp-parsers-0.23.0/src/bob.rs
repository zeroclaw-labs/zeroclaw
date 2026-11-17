// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use alloc::borrow::Cow;
use core::str::FromStr;

use xso::{error::Error, text::Base64, AsXml, AsXmlText, FromXml, FromXmlText};

use crate::hashes::{Algo, Hash};
use crate::ns;
use minidom::IntoAttributeValue;

/// A Content-ID, as defined in RFC2111.
///
/// The text value SHOULD be of the form algo+hash@bob.xmpp.org, this struct
/// enforces that format.
#[derive(Clone, Debug, PartialEq)]
pub struct ContentId {
    hash: Hash,
}

impl FromStr for ContentId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        let (lhs, rhs) = s
            .split_once('@')
            .ok_or(Error::Other("Missing @ in cid URI."))?;
        if rhs != "bob.xmpp.org" {
            return Err(Error::Other("Wrong domain for cid URI."));
        }
        let (algo, hex) = lhs
            .split_once('+')
            .ok_or(Error::Other("Missing + in cid URI."))?;
        let algo = match algo {
            "sha1" => Algo::Sha_1,
            "sha256" => Algo::Sha_256,
            _ => unimplemented!(),
        };
        let hash = Hash::from_hex(algo, hex).map_err(Error::text_parse_error)?;
        Ok(ContentId { hash })
    }
}

impl FromXmlText for ContentId {
    fn from_xml_text(value: String) -> Result<Self, Error> {
        value.parse().map_err(Error::text_parse_error)
    }
}

impl AsXmlText for ContentId {
    fn as_xml_text(&self) -> Result<Cow<'_, str>, Error> {
        let algo = match self.hash.algo {
            Algo::Sha_1 => "sha1",
            Algo::Sha_256 => "sha256",
            _ => unimplemented!(),
        };
        Ok(Cow::Owned(format!(
            "{}+{}@bob.xmpp.org",
            algo,
            self.hash.to_hex()
        )))
    }
}

impl IntoAttributeValue for ContentId {
    fn into_attribute_value(self) -> Option<String> {
        let algo = match self.hash.algo {
            Algo::Sha_1 => "sha1",
            Algo::Sha_256 => "sha256",
            _ => unimplemented!(),
        };
        Some(format!("{}+{}@bob.xmpp.org", algo, self.hash.to_hex()))
    }
}

/// Request for an uncached cid file.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::BOB, name = "data")]
pub struct Data {
    /// The cid in question.
    #[xml(attribute)]
    pub cid: ContentId,

    /// How long to cache it (in seconds).
    #[xml(attribute(default, name = "max-age"))]
    pub max_age: Option<usize>,

    /// The MIME type of the data being transmitted.
    ///
    /// See the [IANA MIME Media Types Registry][1] for a list of
    /// registered types, but unregistered or yet-to-be-registered are
    /// accepted too.
    ///
    /// [1]: <https://www.iana.org/assignments/media-types/media-types.xhtml>
    #[xml(attribute(default, name = "type"))]
    pub type_: Option<String>,

    /// The actual data.
    #[xml(text = Base64)]
    pub data: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xso::error::FromElementError;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(ContentId, 24);
        assert_size!(Data, 56);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(ContentId, 48);
        assert_size!(Data, 112);
    }

    #[test]
    fn test_simple() {
        let cid: ContentId = "sha1+8f35fef110ffc5df08d579a50083ff9308fb6242@bob.xmpp.org"
            .parse()
            .unwrap();
        assert_eq!(cid.hash.algo, Algo::Sha_1);
        assert_eq!(
            cid.hash.hash,
            b"\x8f\x35\xfe\xf1\x10\xff\xc5\xdf\x08\xd5\x79\xa5\x00\x83\xff\x93\x08\xfb\x62\x42"
        );
        assert_eq!(
            cid.into_attribute_value().unwrap(),
            "sha1+8f35fef110ffc5df08d579a50083ff9308fb6242@bob.xmpp.org"
        );

        let elem: Element = "<data xmlns='urn:xmpp:bob' cid='sha1+8f35fef110ffc5df08d579a50083ff9308fb6242@bob.xmpp.org'/>".parse().unwrap();
        let data = Data::try_from(elem).unwrap();
        assert_eq!(data.cid.hash.algo, Algo::Sha_1);
        assert_eq!(
            data.cid.hash.hash,
            b"\x8f\x35\xfe\xf1\x10\xff\xc5\xdf\x08\xd5\x79\xa5\x00\x83\xff\x93\x08\xfb\x62\x42"
        );
        assert!(data.max_age.is_none());
        assert!(data.type_.is_none());
        assert!(data.data.is_empty());
    }

    #[test]
    fn invalid_cid() {
        let error = "Hello world!".parse::<ContentId>().unwrap_err();
        let message = match error {
            Error::Other(string) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Missing @ in cid URI.");

        let error = "Hello world@bob.xmpp.org".parse::<ContentId>().unwrap_err();
        let message = match error {
            Error::Other(string) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Missing + in cid URI.");

        let error = "sha1+1234@coucou.linkmauve.fr"
            .parse::<ContentId>()
            .unwrap_err();
        let message = match error {
            Error::Other(string) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Wrong domain for cid URI.");

        let error = "sha1+invalid@bob.xmpp.org"
            .parse::<ContentId>()
            .unwrap_err();
        let message = match error {
            Error::TextParseError(error) if error.is::<core::num::ParseIntError>() => error,
            _ => panic!(),
        };
        assert_eq!(message.to_string(), "invalid digit found in string");
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn unknown_child() {
        let elem: Element = "<data xmlns='urn:xmpp:bob' cid='sha1+8f35fef110ffc5df08d579a50083ff9308fb6242@bob.xmpp.org'><coucou/></data>"
            .parse()
            .unwrap();
        let error = Data::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in Data element.");
    }
}
