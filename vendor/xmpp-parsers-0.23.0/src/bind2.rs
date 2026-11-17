// Copyright (c) 2024 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::mam;
use crate::ns;
use minidom::Element;

/// Represents the `<bind/>` element, as sent by the server in SASL 2 to advertise which features
/// can be enabled during the binding step.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::BIND2, name = "bind")]
pub struct BindFeature {
    /// The features that can be enabled by the client.
    #[xml(extract(default, name = "inline", fields(extract(n = .., name = "feature", fields(attribute(name = "var", type_ = String))))))]
    pub inline_features: Vec<String>,
}

/// Represents a `<bind/>` element, as sent by the client inline in the `<authenticate/>` SASL 2
/// element, to perform the binding at the same time as the authentication.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::BIND2, name = "bind")]
pub struct BindQuery {
    /// Short text string that typically identifies the software the user is using, mostly useful
    /// for diagnostic purposes for users, operators and developers.  This tag may be visible to
    /// other entities on the XMPP network.
    #[xml(extract(default, fields(text(type_ = String))))]
    pub tag: Option<String>,

    /// Features that the client requests to be automatically enabled for its new session.
    #[xml(element(n = ..))]
    pub payloads: Vec<Element>,
}

/// Represents a `<bound/>` element, which tells the client its resource is bound, alongside other
/// requests.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::BIND2, name = "bound")]
pub struct Bound {
    /// Indicates which messages got missed by this particular device, start is the oldest message
    /// and end is the newest, before this connection.
    #[xml(child(default))]
    pub mam_metadata: Option<mam::MetadataResponse>,

    /// Additional payloads which happened during the binding process.
    #[xml(element(n = ..))]
    pub payloads: Vec<Element>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(BindFeature, 12);
        assert_size!(BindQuery, 24);
        assert_size!(Bound, 68);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(BindFeature, 24);
        assert_size!(BindQuery, 48);
        assert_size!(Bound, 104);
    }

    #[test]
    fn test_empty() {
        let elem: Element = "<bind xmlns='urn:xmpp:bind:0'/>".parse().unwrap();
        let bind = BindQuery::try_from(elem).unwrap();
        assert_eq!(bind.tag, None);
        assert_eq!(bind.payloads.len(), 0);
    }

    #[test]
    fn test_simple() {
        // Example 1
        let elem: Element =
            "<bind xmlns='urn:xmpp:bind:0'><inline><feature var='urn:xmpp:carbons:2'/><feature var='urn:xmpp:csi:0'/><feature var='urn:xmpp:sm:3'/></inline></bind>"
                .parse()
                .unwrap();
        let bind = BindFeature::try_from(elem.clone()).unwrap();
        assert_eq!(bind.inline_features.len(), 3);
        assert_eq!(bind.inline_features[0], "urn:xmpp:carbons:2");
        assert_eq!(bind.inline_features[1], "urn:xmpp:csi:0");
        assert_eq!(bind.inline_features[2], "urn:xmpp:sm:3");
        let elem2 = bind.into();
        assert_eq!(elem, elem2);

        // Example 2
        let elem: Element = "<bind xmlns='urn:xmpp:bind:0'><tag>AwesomeXMPP</tag></bind>"
            .parse()
            .unwrap();
        let bind = BindQuery::try_from(elem).unwrap();
        assert_eq!(bind.tag.unwrap(), "AwesomeXMPP");
        assert_eq!(bind.payloads.len(), 0);

        // Example 3
        let elem: Element = "<bind xmlns='urn:xmpp:bind:0'><tag>AwesomeXMPP</tag><enable xmlns='urn:xmpp:carbons:2'/><enable xmlns='urn:xmpp:sm:3'/><inactive xmlns='urn:xmpp:csi:0'/></bind>".parse().unwrap();
        let bind = BindQuery::try_from(elem).unwrap();
        assert_eq!(bind.tag.unwrap(), "AwesomeXMPP");
        assert_eq!(bind.payloads.len(), 3);

        // Example 4
        let elem: Element = "<bound xmlns='urn:xmpp:bind:0'><metadata xmlns='urn:xmpp:mam:2'><start id='YWxwaGEg' timestamp='2008-08-22T21:09:04Z'/><end id='b21lZ2Eg' timestamp='2020-04-20T14:34:21Z'/></metadata></bound>".parse().unwrap();
        let bound = Bound::try_from(elem).unwrap();
        assert!(bound.mam_metadata.is_some());
        assert_eq!(bound.payloads.len(), 0);
    }
}
