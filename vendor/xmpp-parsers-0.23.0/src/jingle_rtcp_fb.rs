// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::ns;

/// Wrapper element for a rtcp-fb.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::JINGLE_RTCP_FB, name = "rtcp-fb")]
pub struct RtcpFb {
    /// Type of this rtcp-fb.
    #[xml(attribute = "type")]
    pub type_: String,

    /// Subtype of this rtcp-fb, if relevant.
    #[xml(attribute(default))]
    pub subtype: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(RtcpFb, 24);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(RtcpFb, 48);
    }

    #[test]
    fn parse_simple() {
        let elem: Element =
            "<rtcp-fb xmlns='urn:xmpp:jingle:apps:rtp:rtcp-fb:0' type='nack' subtype='sli'/>"
                .parse()
                .unwrap();
        let rtcp_fb = RtcpFb::try_from(elem).unwrap();
        assert_eq!(rtcp_fb.type_, "nack");
        assert_eq!(rtcp_fb.subtype.unwrap(), "sli");
    }
}
