// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::ns;

/// Stream:feature sent by the server to advertise it supports CSI.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::CSI, name = "csi")]
pub struct Feature;

/// Client indicates it is inactive.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::CSI, name = "inactive")]
pub struct Inactive;

/// Client indicates it is active again.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::CSI, name = "active")]
pub struct Active;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ns;
    use minidom::Element;

    #[test]
    fn test_size() {
        assert_size!(Feature, 0);
        assert_size!(Inactive, 0);
        assert_size!(Active, 0);
    }

    #[test]
    fn parsing() {
        let elem: Element = "<csi xmlns='urn:xmpp:csi:0'/>".parse().unwrap();
        Feature::try_from(elem).unwrap();

        let elem: Element = "<inactive xmlns='urn:xmpp:csi:0'/>".parse().unwrap();
        Inactive::try_from(elem).unwrap();

        let elem: Element = "<active xmlns='urn:xmpp:csi:0'/>".parse().unwrap();
        Active::try_from(elem).unwrap();
    }

    #[test]
    fn serialising() {
        let elem: Element = Feature.into();
        assert!(elem.is("csi", ns::CSI));

        let elem: Element = Inactive.into();
        assert!(elem.is("inactive", ns::CSI));

        let elem: Element = Active.into();
        assert!(elem.is("active", ns::CSI));
    }
}
