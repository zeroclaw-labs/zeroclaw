// Copyright (c) 2022 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::message::MessagePayload;
use crate::ns;

/// Container for a set of reactions.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::REACTIONS, name = "reactions")]
pub struct Reactions {
    /// The id of the message these reactions apply to.
    #[xml(attribute)]
    pub id: String,

    /// The list of reactions.
    #[xml(child(n = ..))]
    pub reactions: Vec<Reaction>,
}

impl MessagePayload for Reactions {}

/// One emoji reaction.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::REACTIONS, name = "reaction")]
pub struct Reaction {
    /// The text of this reaction.
    #[xml(text)]
    pub emoji: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Reactions, 24);
        assert_size!(Reaction, 12);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Reactions, 48);
        assert_size!(Reaction, 24);
    }

    #[test]
    fn test_empty() {
        let elem: Element = "<reactions xmlns='urn:xmpp:reactions:0' id='foo'/>"
            .parse()
            .unwrap();
        let elem2 = elem.clone();
        let reactions = Reactions::try_from(elem2).unwrap();
        assert_eq!(reactions.id, "foo");
        assert_eq!(reactions.reactions.len(), 0);
    }

    #[test]
    fn test_multi() {
        let elem: Element =
            "<reactions xmlns='urn:xmpp:reactions:0' id='foo'><reaction>👋</reaction><reaction>🐢</reaction></reactions>"
                .parse()
                .unwrap();
        let elem2 = elem.clone();
        let reactions = Reactions::try_from(elem2).unwrap();
        assert_eq!(reactions.id, "foo");
        assert_eq!(reactions.reactions.len(), 2);
        let [hand, turtle]: [Reaction; 2] = reactions.reactions.try_into().unwrap();
        assert_eq!(hand.emoji, "👋");
        assert_eq!(turtle.emoji, "🐢");
    }

    #[test]
    fn test_zwj_emoji() {
        let elem: Element =
            "<reactions xmlns='urn:xmpp:reactions:0' id='foo'><reaction>👩🏾‍❤️‍👩🏼</reaction></reactions>"
                .parse()
                .unwrap();
        let elem2 = elem.clone();
        let mut reactions = Reactions::try_from(elem2).unwrap();
        assert_eq!(reactions.id, "foo");
        assert_eq!(reactions.reactions.len(), 1);
        let reaction = reactions.reactions.pop().unwrap();
        assert_eq!(
            reaction.emoji,
            "\u{1F469}\u{1F3FE}\u{200D}\u{2764}\u{FE0F}\u{200D}\u{1F469}\u{1F3FC}"
        );
    }
}
