// Copyright (c) 2026 Link Mauve <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::ns;

generate_id!(
    /// Unique and opaque string, indicating the last affiliation version sent by the server that
    /// the client has seen, and cached.
    Version
);

/// A `<mav/>` element sent by the client on join, if it wants to keep track of the affiliations in
/// the room.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::MAV, name = "mav")]
pub struct AffiliationsVersioning {
    /// Unique and opaque string, indicating the last affiliation version sent by the server that
    /// the client has seen, and cached.  Sending the mav element without a since attribute is a
    /// called bootstrap request, which asks the server for a full response.
    #[xml(attribute(default))]
    since: Option<Version>,

    /// The latest version the server has.
    #[xml(attribute(default))]
    until: Option<Version>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Version, 12);
        assert_size!(AffiliationsVersioning, 24);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Version, 24);
        assert_size!(AffiliationsVersioning, 48);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<mav xmlns='urn:xmpp:muc:affiliations:1'/>"
            .parse()
            .unwrap();
        let ver = AffiliationsVersioning::try_from(elem).unwrap();
        assert!(ver.since.is_none());
        assert!(ver.until.is_none());

        let elem: Element =
            "<mav xmlns='urn:xmpp:muc:affiliations:1' since='9pacabr2q1' until='ruz41312vw'/>"
                .parse()
                .unwrap();
        let ver = AffiliationsVersioning::try_from(elem).unwrap();
        assert_eq!(ver.since.unwrap().0, "9pacabr2q1");
        assert_eq!(ver.until.unwrap().0, "ruz41312vw");
    }
}
