// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::message::MessagePayload;
use crate::ns;

/// Structure representing an `<encryption xmlns='urn:xmpp:eme:0'/>` element.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::EME, name = "encryption")]
pub struct ExplicitMessageEncryption {
    /// Namespace of the encryption scheme used.
    #[xml(attribute)]
    pub namespace: String,

    /// User-friendly name for the encryption scheme, should be `None` for OTR,
    /// legacy OpenPGP and OX.
    #[xml(attribute(default))]
    pub name: Option<String>,
}

impl MessagePayload for ExplicitMessageEncryption {}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(ExplicitMessageEncryption, 24);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(ExplicitMessageEncryption, 48);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<encryption xmlns='urn:xmpp:eme:0' namespace='urn:xmpp:otr:0'/>"
            .parse()
            .unwrap();
        let encryption = ExplicitMessageEncryption::try_from(elem).unwrap();
        assert_eq!(encryption.namespace, "urn:xmpp:otr:0");
        assert_eq!(encryption.name, None);

        let elem: Element = "<encryption xmlns='urn:xmpp:eme:0' namespace='some.unknown.mechanism' name='SuperMechanism'/>".parse().unwrap();
        let encryption = ExplicitMessageEncryption::try_from(elem).unwrap();
        assert_eq!(encryption.namespace, "some.unknown.mechanism");
        assert_eq!(encryption.name, Some(String::from("SuperMechanism")));
    }

    #[test]
    fn test_unknown() {
        let elem: Element = "<replace xmlns='urn:xmpp:message-correct:0'/>"
            .parse()
            .unwrap();
        let error = ExplicitMessageEncryption::try_from(elem.clone()).unwrap_err();
        let returned_elem = match error {
            FromElementError::Mismatch(elem) => elem,
            _ => panic!(),
        };
        assert_eq!(elem, returned_elem);
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_invalid_child() {
        let elem: Element =
            "<encryption xmlns='urn:xmpp:eme:0' namespace='urn:xmpp:otr:0'><coucou/></encryption>"
                .parse()
                .unwrap();
        let error = ExplicitMessageEncryption::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Unknown child in ExplicitMessageEncryption element."
        );
    }

    #[test]
    fn test_serialise() {
        let elem: Element = "<encryption xmlns='urn:xmpp:eme:0' namespace='coucou'/>"
            .parse()
            .unwrap();
        let eme = ExplicitMessageEncryption {
            namespace: String::from("coucou"),
            name: None,
        };
        let elem2 = eme.into();
        assert_eq!(elem, elem2);
    }
}
