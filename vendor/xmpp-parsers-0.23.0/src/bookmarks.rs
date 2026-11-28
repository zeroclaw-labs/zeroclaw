// Copyright (c) 2018 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//!
//! Chatroom bookmarks from [XEP-0048](https://xmpp.org/extensions/attic/xep-0048-1.0.html). You should never use this, but use
//! [`bookmarks2`][`crate::bookmarks2`], or [`private::Query`][`crate::private::Query`] for legacy servers which do not advertise
//! `urn:xmpp:bookmarks:1#compat` on the user's BareJID in a disco info request.
//!
//! See [ModernXMPP docs](https://docs.modernxmpp.org/client/groupchat/#bookmarks) on how to handle all historic
//! and newer specifications for your clients.
//!
//! The [`Conference`][crate::bookmarks::Conference] struct used in [`private::Query`][`crate::private::Query`] is the one from this module. Only the querying mechanism changes from a legacy PubSub implementation here, to a legacy Private XML Query implementation in that other module. The [`Conference`][crate::bookmarks2::Conference] element from the [`bookmarks2`][crate::bookmarks2] module is a different structure, but conversion is possible from [`bookmarks::Conference`][crate::bookmarks::Conference] to [`bookmarks2::Conference`][crate::bookmarks2::Conference] via the [`Conference::into_bookmarks2`][crate::bookmarks::Conference::into_bookmarks2] method.

use xso::{AsXml, FromXml};

use jid::BareJid;

pub use crate::bookmarks2;
use crate::jid::ResourcePart;
use crate::ns;

/// A conference bookmark.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::BOOKMARKS, name = "conference")]
pub struct Conference {
    /// Whether a conference bookmark should be joined automatically.
    #[xml(attribute(default))]
    pub autojoin: bool,

    /// The JID of the conference.
    #[xml(attribute)]
    pub jid: BareJid,

    /// A user-defined name for this conference.
    #[xml(attribute(default))]
    pub name: Option<String>,

    /// The nick the user will use to join this conference.
    #[xml(extract(default, fields(text(type_ = ResourcePart))))]
    pub nick: Option<ResourcePart>,

    /// The password required to join this conference.
    #[xml(extract(default, fields(text(type_ = String))))]
    pub password: Option<String>,
}

impl Conference {
    /// Turns a XEP-0048 Conference element into a XEP-0402 "Bookmarks2" Conference element, in a
    /// tuple with the room JID.
    pub fn into_bookmarks2(self) -> (BareJid, bookmarks2::Conference) {
        (
            self.jid,
            bookmarks2::Conference {
                autojoin: self.autojoin,
                name: self.name,
                nick: self.nick,
                password: self.password,
                extensions: None,
            },
        )
    }
}

/// An URL bookmark.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::BOOKMARKS, name = "url")]
pub struct Url {
    /// A user-defined name for this URL.
    #[xml(attribute(default))]
    pub name: Option<String>,

    /// The URL of this bookmark.
    #[xml(attribute)]
    pub url: String,
}

/// Container element for multiple bookmarks.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone, Default)]
#[xml(namespace = ns::BOOKMARKS, name = "storage")]
pub struct Storage {
    /// Conferences the user has expressed an interest in.
    #[xml(child(n = ..))]
    pub conferences: Vec<Conference>,

    /// URLs the user is interested in.
    #[xml(child(n = ..))]
    pub urls: Vec<Url>,
}

impl Storage {
    /// Create an empty bookmarks storage.
    pub fn new() -> Storage {
        Storage::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Conference, 56);
        assert_size!(Url, 24);
        assert_size!(Storage, 24);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Conference, 112);
        assert_size!(Url, 48);
        assert_size!(Storage, 48);
    }

    #[test]
    fn empty() {
        let elem: Element = "<storage xmlns='storage:bookmarks'/>".parse().unwrap();
        let elem1 = elem.clone();
        let storage = Storage::try_from(elem).unwrap();
        assert_eq!(storage.conferences.len(), 0);
        assert_eq!(storage.urls.len(), 0);

        let elem2 = Element::from(Storage::new());
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn wrong_resource() {
        // This emoji is not valid according to Resource prep
        let elem: Element = "<storage xmlns='storage:bookmarks'><url name='Example' url='https://example.com/'/><conference autojoin='true' jid='foo@muc.localhost' name='TEST'><nick>Whatever\u{1F469}\u{1F3FE}\u{200D}\u{2764}\u{FE0F}\u{200D}\u{1F469}\u{1F3FC}</nick></conference></storage>".parse().unwrap();
        let res = Storage::try_from(elem);
        assert!(res.is_err());
        assert_eq!(
            res.unwrap_err().to_string().as_str(),
            "text parse error: resource doesn’t pass resourceprep validation"
        );
    }

    #[test]
    fn complete() {
        let elem: Element = "<storage xmlns='storage:bookmarks'><url name='Example' url='https://example.org/'/><conference autojoin='true' jid='test-muc@muc.localhost' name='Test MUC'><nick>Coucou</nick><password>secret</password></conference></storage>".parse().unwrap();
        let storage = Storage::try_from(elem).unwrap();
        assert_eq!(storage.urls.len(), 1);
        assert_eq!(storage.urls[0].clone().name.unwrap(), "Example");
        assert_eq!(storage.urls[0].url, "https://example.org/");
        assert_eq!(storage.conferences.len(), 1);
        assert_eq!(storage.conferences[0].autojoin, true);
        assert_eq!(
            storage.conferences[0].jid,
            BareJid::new("test-muc@muc.localhost").unwrap()
        );
        assert_eq!(storage.conferences[0].clone().name.unwrap(), "Test MUC");
        assert_eq!(
            storage.conferences[0].clone().nick.unwrap().as_str(),
            "Coucou"
        );
        assert_eq!(storage.conferences[0].clone().password.unwrap(), "secret");
    }
}
