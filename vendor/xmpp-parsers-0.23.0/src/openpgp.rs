// Copyright (c) 2019 Maxime “pep” Buquet <pep@bouah.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{text::Base64, AsXml, FromXml};

use crate::date::DateTime;
use crate::ns;
use crate::pubsub::PubSubPayload;

/// Data contained in the PubKey element
// TODO: Merge this container with the PubKey struct
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::OX, name = "data")]
pub struct PubKeyData {
    /// Base64 data
    #[xml(text = Base64)]
    pub data: Vec<u8>,
}

/// Pubkey element to be used in PubSub publish payloads.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::OX, name = "pubkey")]
pub struct PubKey {
    /// Last updated date
    #[xml(attribute(default))]
    pub date: Option<DateTime>,

    /// Public key as base64 data
    #[xml(child)]
    pub data: PubKeyData,
}

impl PubSubPayload for PubKey {}

/// Public key metadata
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::OX, name = "pubkey-metadata")]
pub struct PubKeyMeta {
    /// OpenPGP v4 fingerprint
    #[xml(attribute = "v4-fingerprint")]
    pub v4fingerprint: String,

    /// Time the key was published or updated
    #[xml(attribute = "date")]
    pub date: DateTime,
}

/// List of public key metadata
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::OX, name = "public-key-list")]
pub struct PubKeysMeta {
    /// Public keys
    #[xml(child(n = ..))]
    pub pubkeys: Vec<PubKeyMeta>,
}

impl PubSubPayload for PubKeysMeta {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ns;
    use crate::pubsub::{
        pubsub::{Item, Publish},
        NodeName,
    };
    use core::str::FromStr;
    use minidom::Element;

    #[test]
    fn pubsub_publish_pubkey_data() {
        let pubkey = PubKey {
            date: None,
            data: PubKeyData {
                data: (&"Foo").as_bytes().to_vec(),
            },
        };
        println!("Foo1: {:?}", pubkey);

        let pubsub = Publish {
            node: NodeName(format!("{}:{}", ns::OX_PUBKEYS, "some-fingerprint")),
            items: vec![Item::new(None, None, Some(pubkey))],
        };
        println!("Foo2: {:?}", pubsub);
    }

    #[test]
    fn pubsub_publish_pubkey_meta() {
        let pubkeymeta = PubKeysMeta {
            pubkeys: vec![PubKeyMeta {
                v4fingerprint: "some-fingerprint".to_owned(),
                date: DateTime::from_str("2019-03-30T18:30:25Z").unwrap(),
            }],
        };
        println!("Foo1: {:?}", pubkeymeta);

        let pubsub = Publish {
            node: NodeName("foo".to_owned()),
            items: vec![Item::new(None, None, Some(pubkeymeta))],
        };
        println!("Foo2: {:?}", pubsub);
    }

    #[test]
    fn test_serialize_pubkey() {
        let reference: Element = "<pubkey xmlns='urn:xmpp:openpgp:0'><data>AAAA</data></pubkey>"
            .parse()
            .unwrap();

        let pubkey = PubKey {
            date: None,
            data: PubKeyData {
                data: b"\0\0\0".to_vec(),
            },
        };

        let serialized: Element = pubkey.into();
        assert_eq!(serialized, reference);
    }
}
