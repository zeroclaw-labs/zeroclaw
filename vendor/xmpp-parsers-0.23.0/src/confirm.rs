// Copyright (c) 2025 Adrien Destugues <pulkomandy@pulkomandy.tk>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::iq::IqGetPayload;
use crate::message::MessagePayload;
use crate::ns;

/// XEP-0070 "confirm" element used to request user confirmation before accessing a protected
/// HTTP resource, or as a reply to such request.
///
/// This can be used as:
/// - A message payload (when the request is sent to a bare JID or when receiving the reply)
/// - A Set Iq payload (when the request is sent to a full JID as an Iq). The confirm element may
///   also be present in responses, but should be ignored (the IQ id is used to identify the
///   request in that case).
#[derive(FromXml, AsXml, PartialEq, Eq, Hash, Debug, Clone)]
#[xml(namespace = ns::HTTP_AUTH, name = "confirm")]
pub struct Confirm {
    /// HTTP method used to access the resource
    #[xml(attribute)]
    pub method: String,

    /// URL being accessed and guarded by the authentication request
    #[xml(attribute)]
    pub url: String,

    /// Identifier of the authentication request
    #[xml(attribute)]
    pub id: String,
}

impl IqGetPayload for Confirm {}
impl MessagePayload for Confirm {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ns;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Confirm, 36);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Confirm, 72);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<confirm xmlns='http://jabber.org/protocol/http-auth' \
            id='a7374jnjlalasdf82' method='GET' \
            url='https://files.shakespeare.lit:9345/missive.html'/>"
            .parse()
            .unwrap();
        let confirm = Confirm::try_from(elem).unwrap();
        assert_eq!(confirm.method, "GET");
        assert_eq!(confirm.id, "a7374jnjlalasdf82");
        assert_eq!(
            confirm.url,
            "https://files.shakespeare.lit:9345/missive.html"
        );
    }

    #[test]
    fn test_serialise() {
        let confirm = Confirm {
            method: "GET".to_string(),
            url: "https://files.shakespeare.lit/".to_string(),
            id: "sesame".to_string(),
        };
        let elem: Element = confirm.into();
        assert!(elem.is("confirm", ns::HTTP_AUTH));
    }
}
