// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
// Copyright (c) 2017 Maxime “pep” Buquet <pep@bouah.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::iq::IqGetPayload;
use crate::ns;

/// Represents a ping to the recipient, which must be answered with an
/// empty `<iq/>` or with an error.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::PING, name = "ping")]
pub struct Ping;

impl IqGetPayload for Ping {}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    #[cfg(feature = "pedantic")]
    use xso::error::{Error, FromElementError};

    #[test]
    fn test_size() {
        assert_size!(Ping, 0);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<ping xmlns='urn:xmpp:ping'/>".parse().unwrap();
        Ping::try_from(elem).unwrap();
    }

    #[test]
    fn test_serialise() {
        let elem1 = Element::from(Ping);
        let elem2: Element = "<ping xmlns='urn:xmpp:ping'/>".parse().unwrap();
        assert_eq!(elem1, elem2);
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_invalid() {
        let elem: Element = "<ping xmlns='urn:xmpp:ping'><coucou/></ping>"
            .parse()
            .unwrap();
        let error = Ping::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in Ping element.");
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_invalid_attribute() {
        let elem: Element = "<ping xmlns='urn:xmpp:ping' coucou=''/>".parse().unwrap();
        let error = Ping::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown attribute in Ping element.");
    }
}
