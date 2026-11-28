// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use jid::Jid;

use crate::iq::{IqGetPayload, IqResultPayload};
use crate::ns;

/// Request from a client to stringprep/PRECIS a string into a JID.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::JID_PREP, name = "jid")]
pub struct JidPrepQuery {
    /// The potential JID.
    #[xml(text)]
    pub data: String,
}

impl IqGetPayload for JidPrepQuery {}

impl JidPrepQuery {
    /// Create a new JID Prep query.
    pub fn new<J: Into<String>>(jid: J) -> JidPrepQuery {
        JidPrepQuery { data: jid.into() }
    }
}

/// Response from the server with the stringprep’d/PRECIS’d JID.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::JID_PREP, name = "jid")]
pub struct JidPrepResponse {
    /// The JID.
    #[xml(text)]
    pub jid: Jid,
}

impl IqResultPayload for JidPrepResponse {}

#[cfg(test)]
mod tests {
    use super::*;
    use jid::FullJid;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(JidPrepQuery, 12);
        assert_size!(JidPrepResponse, 16);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(JidPrepQuery, 24);
        assert_size!(JidPrepResponse, 32);
    }

    #[test]
    fn simple() {
        let elem: Element = "<jid xmlns='urn:xmpp:jidprep:0'>ROMeo@montague.lit/orchard</jid>"
            .parse()
            .unwrap();
        let query = JidPrepQuery::try_from(elem).unwrap();
        assert_eq!(query.data, "ROMeo@montague.lit/orchard");

        let elem: Element = "<jid xmlns='urn:xmpp:jidprep:0'>romeo@montague.lit/orchard</jid>"
            .parse()
            .unwrap();
        let response = JidPrepResponse::try_from(elem).unwrap();
        assert_eq!(
            response.jid,
            FullJid::new("romeo@montague.lit/orchard").unwrap()
        );
    }
}
