// Copyright (c) 2018 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use jid::BareJid;

use crate::ns;

/// The stream opening for WebSocket.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::WEBSOCKET, name = "open")]
pub struct Open {
    /// The JID of the entity opening this stream.
    #[xml(attribute(default))]
    pub from: Option<BareJid>,

    /// The JID of the entity receiving this stream opening.
    #[xml(attribute(default))]
    pub to: Option<BareJid>,

    /// The id of the stream, used for authentication challenges.
    #[xml(attribute(default))]
    pub id: Option<String>,

    /// The XMPP version used during this stream.
    #[xml(attribute(default))]
    pub version: Option<String>,

    /// The default human language for all subsequent stanzas, which will
    /// be transmitted to other entities for better localisation.
    #[xml(lang(default))]
    pub xml_lang: Option<String>,
}

impl Open {
    /// Creates a simple client→server `<open/>` element.
    pub fn new(to: BareJid) -> Open {
        Open {
            from: None,
            to: Some(to),
            id: None,
            version: Some(String::from("1.0")),
            xml_lang: None,
        }
    }

    /// Sets the [@from](#structfield.from) attribute on this `<open/>`
    /// element.
    pub fn with_from(mut self, from: BareJid) -> Open {
        self.from = Some(from);
        self
    }

    /// Sets the [@id](#structfield.id) attribute on this `<open/>` element.
    pub fn with_id(mut self, id: String) -> Open {
        self.id = Some(id);
        self
    }

    /// Sets the [@xml:lang](#structfield.xml_lang) attribute on this `<open/>`
    /// element.
    pub fn with_lang(mut self, xml_lang: String) -> Open {
        self.xml_lang = Some(xml_lang);
        self
    }

    /// Checks whether the version matches the expected one.
    pub fn is_version(&self, version: &str) -> bool {
        match self.version {
            None => false,
            Some(ref self_version) => self_version == &String::from(version),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Open, 68);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Open, 136);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<open xmlns='urn:ietf:params:xml:ns:xmpp-framing'/>"
            .parse()
            .unwrap();
        let open = Open::try_from(elem).unwrap();
        assert_eq!(open.from, None);
        assert_eq!(open.to, None);
        assert_eq!(open.id, None);
        assert_eq!(open.version, None);
        assert_eq!(open.xml_lang, None);
    }
}
