// Copyright (c) 2026 Link Mauve <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::ns;

generate_attribute!(
    /// The RFC4796 SDP 'content' attribute.
    Name, "name", {
        /// The media stream includes presentation slides.
        Slides => "slides",

        /// The media stream contains the image of the speaker.
        Speaker => "speaker",

        /// The media stream contains sign language.
        Sl => "sl",

        /// The media stream is taken from the main source.
        Main => "main",

        /// The media stream is taken from the alternative source.
        Alt => "alt",
    }
);

/// Wrapper element around the Jingle content category.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::JINGLE_CONTENT, name = "category")]
pub struct Category {
    /// The actual category being specified.
    #[xml(attribute)]
    name: Name,
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[test]
    fn test_size() {
        assert_size!(Category, 1);
        assert_size!(Name, 1);
    }

    #[test]
    fn parse_category() {
        let elem: Element = "<category xmlns='urn:xmpp:jingle:apps:category:0' name='slides'/>"
            .parse()
            .unwrap();
        let category = Category::try_from(elem).unwrap();
        assert_eq!(category.name, Name::Slides);
    }

    #[test]
    fn invalid_category() {
        let elem: Element = "<category xmlns='urn:xmpp:jingle:apps:category:0'/>"
            .parse()
            .unwrap();
        let error = Category::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'name' on Category element missing."
        );

        let elem: Element = "<category xmlns='urn:xmpp:jingle:apps:category:0' name='anime'/>"
            .parse()
            .unwrap();
        let error = Category::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message.to_string(), "Unknown value for 'name' attribute.");
    }
}
