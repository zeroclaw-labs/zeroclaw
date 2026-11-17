// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::iq::{IqGetPayload, IqResultPayload};
use crate::ns;

/// Represents a query for the software version a remote entity is using.
///
/// It should only be used in an `<iq type='get'/>`, as it can only
/// represent the request, and not a result.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::VERSION, name = "query")]
pub struct VersionQuery;

impl IqGetPayload for VersionQuery {}

/// Represents the answer about the software version we are using.
///
/// It should only be used in an `<iq type='result'/>`, as it can only
/// represent the result, and not a request.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::VERSION, name = "query")]
pub struct VersionResult {
    /// The name of this client.
    #[xml(extract(fields(text)))]
    pub name: String,

    /// The version of this client.
    #[xml(extract(fields(text)))]
    pub version: String,

    /// The OS this client is running on.
    #[xml(extract(default, fields(text(type_ = String))))]
    pub os: Option<String>,
}

impl IqResultPayload for VersionResult {}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(VersionQuery, 0);
        assert_size!(VersionResult, 36);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(VersionQuery, 0);
        assert_size!(VersionResult, 72);
    }

    #[test]
    fn simple() {
        let elem: Element =
            "<query xmlns='jabber:iq:version'><name>xmpp-rs</name><version>0.3.0</version></query>"
                .parse()
                .unwrap();
        let version = VersionResult::try_from(elem).unwrap();
        assert_eq!(version.name, String::from("xmpp-rs"));
        assert_eq!(version.version, String::from("0.3.0"));
        assert_eq!(version.os, None);
    }

    #[test]
    fn serialisation() {
        let version = VersionResult {
            name: String::from("xmpp-rs"),
            version: String::from("0.3.0"),
            os: None,
        };
        let elem1 = Element::from(version);
        let elem2: Element =
            "<query xmlns='jabber:iq:version'><name>xmpp-rs</name><version>0.3.0</version></query>"
                .parse()
                .unwrap();
        println!("{:?}", elem1);
        assert_eq!(elem1, elem2);
    }
}
