// Copyright (c) 2024 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::ns;
use crate::stanza_id::StanzaId;
use alloc::collections::BTreeMap;

generate_attribute!(
    /// The possible reasons for a report.
    Reason, "reason", {
        /// Used for reporting a JID that is sending unwanted messages.
        Spam => "urn:xmpp:reporting:spam",

        /// Used for reporting general abuse.
        Abuse => "urn:xmpp:reporting:abuse",
    }
);

type Lang = String;

/// Represents an abuse or spam report.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::SPAM_REPORTING, name = "report")]
pub struct Report {
    /// The reason for this report.
    #[xml(attribute)]
    reason: Reason,

    /// Ids of the incriminated stanzas.
    #[xml(child(n = ..))]
    stanza_ids: Vec<StanzaId>,

    /// Some text explaining the reason for this report.
    #[xml(extract(n = .., name = "text", fields(
        lang(type_ = Lang, default),
        text(type_ = String)
    )))]
    texts: BTreeMap<Lang, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use jid::Jid;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Reason, 1);
        assert_size!(Report, 28);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Reason, 1);
        assert_size!(Report, 56);
    }

    #[test]
    // Comes from https://xmpp.org/extensions/xep-0377.html#example-2
    fn test_example_1() {
        let elem: Element =
            "<report xmlns='urn:xmpp:reporting:1' reason='urn:xmpp:reporting:spam'/>"
                .parse()
                .unwrap();
        let report = Report::try_from(elem).unwrap();
        assert_eq!(report.reason, Reason::Spam);
        assert!(report.stanza_ids.is_empty());
        assert!(report.texts.is_empty());
    }

    #[test]
    // Comes from https://xmpp.org/extensions/xep-0377.html#example-5
    fn test_example_5() {
        let elem: Element = "<report xmlns='urn:xmpp:reporting:1' reason='urn:xmpp:reporting:spam'>
            <stanza-id xmlns='urn:xmpp:sid:0' by='romeo@example.net' id='28482-98726-73623'/>
            <stanza-id xmlns='urn:xmpp:sid:0' by='romeo@example.net' id='38383-38018-18385'/>
            <text xml:lang='en'>Never came trouble to my house like this.</text>
        </report>"
            .parse()
            .unwrap();
        let report = Report::try_from(elem).unwrap();
        let romeo = Jid::new("romeo@example.net").unwrap();
        assert_eq!(report.reason, Reason::Spam);
        assert_eq!(report.stanza_ids.len(), 2);
        assert_eq!(report.stanza_ids[0].by, romeo);
        assert_eq!(report.stanza_ids[0].id, "28482-98726-73623");
        assert_eq!(report.stanza_ids[1].by, romeo);
        assert_eq!(report.stanza_ids[1].id, "38383-38018-18385");
        assert_eq!(
            report.texts["en"],
            "Never came trouble to my house like this."
        );
    }
}
