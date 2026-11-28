// Copyright (c) 2023 XMPP-RS contributors.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Jingle thumbnails (XEP-0264)

use xso::{AsXml, FromXml};

use crate::ns;
use core::num::NonZeroU16;

/// A Jingle thumbnail.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::JINGLE_THUMBNAILS, name = "thumbnail")]
pub struct Thumbnail {
    /// The URI of the thumbnail.
    #[xml(attribute)]
    pub uri: String,

    /// The media type of the thumbnail.
    #[xml(attribute(default, name = "media-type"))]
    pub media_type: Option<String>,

    /// The width of the thumbnail.
    #[xml(attribute(default))]
    pub width: Option<NonZeroU16>,

    /// The height of the thumbnail.
    #[xml(attribute(default))]
    pub height: Option<NonZeroU16>,
}

#[cfg(test)]
mod tests {
    use crate::jingle_thumbnails::Thumbnail;
    use core::num::NonZeroU16;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Thumbnail, 28);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Thumbnail, 56);
    }

    #[test]
    fn test_simple_parse() {
        // Extracted from https://xmpp.org/extensions/xep-0264.html#example-1
        let test_xml = "<thumbnail xmlns='urn:xmpp:thumbs:1'
        uri='cid:sha1+ffd7c8d28e9c5e82afea41f97108c6b4@bob.xmpp.org'
        media-type='image/png'
        width='128'
        height='96'/>";

        let elem: Element = test_xml.parse().unwrap();

        let thumbnail = Thumbnail::try_from(elem).unwrap();

        assert_eq!(
            thumbnail.uri,
            "cid:sha1+ffd7c8d28e9c5e82afea41f97108c6b4@bob.xmpp.org"
        );
        assert_eq!(thumbnail.media_type.unwrap(), "image/png");
        assert_eq!(thumbnail.width, NonZeroU16::new(128));
        assert_eq!(thumbnail.height, NonZeroU16::new(96));
    }
}
