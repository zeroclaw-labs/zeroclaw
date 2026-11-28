// Copyright (c) 2021 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::iq::{IqGetPayload, IqResultPayload, IqSetPayload};
use crate::ns;
use jid::Jid;

generate_attribute!(
    /// Notes the default archiving preference for the user.
    DefaultPrefs, "default", {
        /// The default is to always log messages in the archive.
        Always => "always",

        /// The default is to never log messages in the archive.
        Never => "never",

        /// The default is to log messages in the archive only for contacts
        /// present in the user’s [roster](../roster/index.html).
        Roster => "roster",
    }
);

/// Controls the archiving preferences of the user.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::MAM, name = "prefs")]
pub struct Prefs {
    /// The default preference for JIDs in neither
    /// [always](#structfield.always) or [never](#structfield.never) lists.
    #[xml(attribute = "default")]
    pub default_: DefaultPrefs,

    /// The set of JIDs for which to always store messages in the archive.
    #[xml(extract(default, fields(extract(n = .., name = "jid", fields(text(type_ = Jid))))))]
    pub always: Vec<Jid>,

    /// The set of JIDs for which to never store messages in the archive.
    #[xml(extract(default, fields(extract(n = .., name = "jid", fields(text(type_ = Jid))))))]
    pub never: Vec<Jid>,
}

impl IqGetPayload for Prefs {}
impl IqSetPayload for Prefs {}
impl IqResultPayload for Prefs {}

#[cfg(test)]
mod tests {
    use super::*;
    use jid::BareJid;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(DefaultPrefs, 1);
        assert_size!(Prefs, 28);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(DefaultPrefs, 1);
        assert_size!(Prefs, 56);
    }

    #[test]
    fn test_prefs_get() {
        let elem: Element = "<prefs xmlns='urn:xmpp:mam:2' default='always'/>"
            .parse()
            .unwrap();
        let prefs = Prefs::try_from(elem).unwrap();
        assert!(prefs.always.is_empty());
        assert!(prefs.never.is_empty());

        let elem: Element = r#"<prefs xmlns='urn:xmpp:mam:2' default='roster'>
  <always/>
  <never/>
</prefs>
"#
        .parse()
        .unwrap();
        let prefs = Prefs::try_from(elem).unwrap();
        assert!(prefs.always.is_empty());
        assert!(prefs.never.is_empty());
    }

    #[test]
    fn test_prefs_result() {
        let elem: Element = r#"<prefs xmlns='urn:xmpp:mam:2' default='roster'>
  <always>
    <jid>romeo@montague.lit</jid>
  </always>
  <never>
    <jid>montague@montague.lit</jid>
  </never>
</prefs>
"#
        .parse()
        .unwrap();
        let prefs = Prefs::try_from(elem).unwrap();
        assert_eq!(prefs.always, [BareJid::new("romeo@montague.lit").unwrap()]);
        assert_eq!(
            prefs.never,
            [BareJid::new("montague@montague.lit").unwrap()]
        );

        let elem2 = Element::from(prefs.clone());
        println!("{:?}", elem2);
        let prefs2 = Prefs::try_from(elem2).unwrap();
        assert_eq!(prefs.default_, prefs2.default_);
        assert_eq!(prefs.always, prefs2.always);
        assert_eq!(prefs.never, prefs2.never);
    }
}
