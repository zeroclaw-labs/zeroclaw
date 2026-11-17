// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{
    text::{Base64, StripWhitespace, TextCodec},
    AsXml, FromXml,
};

use crate::hashes::Sha1HexAttribute;
use crate::ns;
use crate::pubsub::PubSubPayload;

/// Communicates information about an avatar.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::AVATAR_METADATA, name = "metadata")]
pub struct Metadata {
    /// List of information elements describing this avatar.
    #[xml(child(n = ..))]
    pub infos: Vec<Info>,
}

impl PubSubPayload for Metadata {}

/// Communicates avatar metadata.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::AVATAR_METADATA, name = "info")]
pub struct Info {
    /// The size of the image data in bytes.
    #[xml(attribute)]
    pub bytes: u32,

    /// The width of the image in pixels.
    #[xml(attribute(default))]
    pub width: Option<u16>,

    /// The height of the image in pixels.
    #[xml(attribute(default))]
    pub height: Option<u16>,

    /// The SHA-1 hash of the image data for the specified content-type.
    #[xml(attribute)]
    pub id: Sha1HexAttribute,

    /// The IANA-registered content type of the image data.
    #[xml(attribute = "type")]
    pub type_: String,

    /// The http: or https: URL at which the image data file is hosted.
    #[xml(attribute(default))]
    pub url: Option<String>,
}

/// The actual avatar data.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::AVATAR_DATA, name = "data")]
pub struct Data {
    /// Vector of bytes representing the avatar’s image.
    #[xml(text(codec = Base64.filtered(StripWhitespace)))]
    pub data: Vec<u8>,
}

impl PubSubPayload for Data {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hashes::Algo;
    use minidom::Element;
    #[cfg(feature = "pedantic")]
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Metadata, 12);
        assert_size!(Info, 60);
        assert_size!(Data, 12);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Metadata, 24);
        assert_size!(Info, 112);
        assert_size!(Data, 24);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<metadata xmlns='urn:xmpp:avatar:metadata'>
                                 <info bytes='12345' width='64' height='64'
                                       id='111f4b3c50d7b0df729d299bc6f8e9ef9066971f'
                                       type='image/png'/>
                             </metadata>"
            .parse()
            .unwrap();
        let metadata = Metadata::try_from(elem).unwrap();
        assert_eq!(metadata.infos.len(), 1);
        let info = &metadata.infos[0];
        assert_eq!(info.bytes, 12345);
        assert_eq!(info.width, Some(64));
        assert_eq!(info.height, Some(64));
        assert_eq!(info.id.algo, Algo::Sha_1);
        assert_eq!(info.type_, "image/png");
        assert_eq!(info.url, None);
        assert_eq!(
            info.id.hash,
            [
                17, 31, 75, 60, 80, 215, 176, 223, 114, 157, 41, 155, 198, 248, 233, 239, 144, 102,
                151, 31
            ]
        );

        let elem: Element = "<data xmlns='urn:xmpp:avatar:data'>AAAA</data>"
            .parse()
            .unwrap();
        let data = Data::try_from(elem).unwrap();
        assert_eq!(data.data, b"\0\0\0");
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_invalid() {
        let elem: Element = "<data xmlns='urn:xmpp:avatar:data' id='coucou'/>"
            .parse()
            .unwrap();
        let error = Data::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown attribute in Data element.")
    }
}
