// Copyright (c) 2020 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::jingle::ContentId;
use crate::ns;

generate_attribute!(
    /// The semantics of the grouping.
    Semantics, "semantics", {
        /// Lip synchronsation.
        Ls => "LS",

        /// Bundle.
        Bundle => "BUNDLE",
    }
);

/// Describes a content that should be grouped with other ones.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::JINGLE_GROUPING, name = "content")]
pub struct Content {
    /// The name of the matching [`Content`](crate::jingle::Content).
    #[xml(attribute)]
    pub name: ContentId,
}

impl Content {
    /// Creates a new \<content/\> element.
    pub fn new(name: &str) -> Content {
        Content {
            name: ContentId(name.to_string()),
        }
    }
}

/// A semantic group of contents.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::JINGLE_GROUPING, name = "group")]
pub struct Group {
    /// Semantics of the grouping.
    #[xml(attribute)]
    pub semantics: Semantics,

    /// List of contents that should be grouped with each other.
    #[xml(child(n = ..))]
    pub contents: Vec<Content>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Semantics, 1);
        assert_size!(Content, 12);
        assert_size!(Group, 16);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Semantics, 1);
        assert_size!(Content, 24);
        assert_size!(Group, 32);
    }

    #[test]
    fn parse_group() {
        let elem: Element = "<group xmlns='urn:xmpp:jingle:apps:grouping:0' semantics='BUNDLE'>
            <content name='voice'/>
            <content name='webcam'/>
        </group>"
            .parse()
            .unwrap();
        let group = Group::try_from(elem).unwrap();
        assert_eq!(group.semantics, Semantics::Bundle);
        assert_eq!(group.contents.len(), 2);
        assert_eq!(
            group.contents,
            &[Content::new("voice"), Content::new("webcam")]
        );
    }
}
