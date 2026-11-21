// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::data_forms::DataForm;
use crate::disco::{DiscoInfoQuery, DiscoInfoResult, Identity};
use crate::hashes::{Algo, Hash};
use crate::ns;
use crate::presence::PresencePayload;
use base64::{engine::general_purpose::STANDARD as Base64, Engine};
use blake2::{
    digest::{Update, VariableOutput},
    Blake2bVar,
};
use digest::Digest;
use sha1::Sha1;
use sha2::{Sha256, Sha512};
use sha3::{Sha3_256, Sha3_512};

/// Represents a capability hash for a given client.
///
/// Warning: This protocol is insecure, you may want to switch to
/// [ecaps2](../ecaps2/index.html) instead, see [this
/// email](https://mail.jabber.org/pipermail/security/2009-July/000812.html).
#[derive(FromXml, AsXml, Debug, Clone)]
#[xml(namespace = ns::CAPS, name = "c")]
pub struct Caps {
    /// Deprecated list of additional feature bundles.
    #[xml(attribute(default))]
    pub ext: Option<String>,

    /// A URI identifying an XMPP application.
    #[xml(attribute)]
    pub node: String,

    /// The algorithm of the hash of these caps.
    #[xml(attribute)]
    pub hash: Algo,

    /// The hash of that application’s
    /// [disco#info](../disco/struct.DiscoInfoResult.html).
    #[xml(attribute(codec = Base64))]
    pub ver: Vec<u8>,
}

impl PresencePayload for Caps {}

impl Caps {
    /// Create a Caps element from its node and hash.
    pub fn new<N: Into<String>>(node: N, hash: Hash) -> Caps {
        Caps {
            ext: None,
            node: node.into(),
            hash: hash.algo,
            ver: hash.hash,
        }
    }
}

fn compute_item(field: &str) -> Vec<u8> {
    let mut bytes = field.as_bytes().to_vec();
    bytes.push(b'<');
    bytes
}

fn compute_items<T, F: Fn(&T) -> Vec<u8>>(things: &[T], encode: F) -> Vec<u8> {
    let mut string: Vec<u8> = vec![];
    let mut accumulator: Vec<Vec<u8>> = vec![];
    for thing in things {
        let bytes = encode(thing);
        accumulator.push(bytes);
    }
    // This works using the expected i;octet collation.
    accumulator.sort();
    for mut bytes in accumulator {
        string.append(&mut bytes);
    }
    string
}

fn compute_features(features: &[String]) -> Vec<u8> {
    compute_items(features, |feature| compute_item(&feature))
}

fn compute_identities(identities: &[Identity]) -> Vec<u8> {
    compute_items(identities, |identity| {
        let lang = identity.lang.as_deref().unwrap_or_default();
        let name = identity.name.as_deref().unwrap_or_default();
        let string = format!("{}/{}/{}/{}", identity.category, identity.type_, lang, name);
        let mut vec = string.as_bytes().to_vec();
        vec.push(b'<');
        vec
    })
}

fn compute_extensions(extensions: &[DataForm]) -> Vec<u8> {
    compute_items(extensions, |extension| {
        // TODO: maybe handle the error case?
        let mut bytes = if let Some(form_type) = extension.form_type() {
            form_type.as_bytes().to_vec()
        } else {
            vec![]
        };
        bytes.push(b'<');
        for field in &extension.fields {
            if field.var.as_deref() == Some("FORM_TYPE") {
                continue;
            }
            if let Some(var) = &field.var {
                bytes.append(&mut compute_item(var));
            }
            bytes.append(&mut compute_items(&field.values, |value| {
                compute_item(value)
            }));
        }
        bytes
    })
}

/// Applies the caps algorithm on the provided disco#info result, to generate
/// the hash input.
///
/// Warning: This protocol is insecure, you may want to switch to
/// [ecaps2](../ecaps2/index.html) instead, see [this
/// email](https://mail.jabber.org/pipermail/security/2009-July/000812.html).
pub fn compute_disco(disco: &DiscoInfoResult) -> Vec<u8> {
    // TODO: Figure out a way to remove the clones here.
    let features: Vec<_> = disco.features.iter().cloned().collect();

    let identities_string = compute_identities(&disco.identities);
    let features_string = compute_features(&features);
    let extensions_string = compute_extensions(&disco.extensions);

    let mut final_string = vec![];
    final_string.extend(identities_string);
    final_string.extend(features_string);
    final_string.extend(extensions_string);
    final_string
}

/// Hashes the result of [compute_disco()] with one of the supported [hash
/// algorithms](../hashes/enum.Algo.html).
pub fn hash_caps(data: &[u8], algo: Algo) -> Result<Hash, String> {
    Ok(Hash {
        hash: match algo {
            Algo::Sha_1 => {
                let hash = Sha1::digest(data);
                hash.to_vec()
            }
            Algo::Sha_256 => {
                let hash = Sha256::digest(data);
                hash.to_vec()
            }
            Algo::Sha_512 => {
                let hash = Sha512::digest(data);
                hash.to_vec()
            }
            Algo::Sha3_256 => {
                let hash = Sha3_256::digest(data);
                hash.to_vec()
            }
            Algo::Sha3_512 => {
                let hash = Sha3_512::digest(data);
                hash.to_vec()
            }
            Algo::Blake2b_256 => {
                let mut hasher = Blake2bVar::new(32).unwrap();
                hasher.update(data);
                let mut vec = vec![0u8; 32];
                hasher.finalize_variable(&mut vec).unwrap();
                vec
            }
            Algo::Blake2b_512 => {
                let mut hasher = Blake2bVar::new(64).unwrap();
                hasher.update(data);
                let mut vec = vec![0u8; 64];
                hasher.finalize_variable(&mut vec).unwrap();
                vec
            }
            Algo::Unknown(algo) => return Err(format!("Unknown algorithm: {}.", algo)),
        },
        algo,
    })
}

/// Helper function to create the query for the disco#info corresponding to a
/// caps hash.
pub fn query_caps(caps: Caps) -> DiscoInfoQuery {
    DiscoInfoQuery {
        node: Some(format!("{}#{}", caps.node, Base64.encode(&caps.ver))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps;
    use minidom::Element;
    #[cfg(feature = "pedantic")]
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Caps, 48);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Caps, 96);
    }

    #[test]
    fn test_parse() {
        let elem: Element = "<c xmlns='http://jabber.org/protocol/caps' hash='sha-256' node='coucou' ver='K1Njy3HZBThlo4moOD5gBGhn0U0oK7/CbfLlIUDi6o4='/>".parse().unwrap();
        let caps = Caps::try_from(elem).unwrap();
        assert_eq!(caps.node, String::from("coucou"));
        assert_eq!(caps.hash, Algo::Sha_256);
        assert_eq!(
            caps.ver,
            Base64
                .decode("K1Njy3HZBThlo4moOD5gBGhn0U0oK7/CbfLlIUDi6o4=")
                .unwrap()
        );
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_invalid_child() {
        let elem: Element = "<c xmlns='http://jabber.org/protocol/caps' node='coucou' hash='sha-256' ver='K1Njy3HZBThlo4moOD5gBGhn0U0oK7/CbfLlIUDi6o4='><hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>K1Njy3HZBThlo4moOD5gBGhn0U0oK7/CbfLlIUDi6o4=</hash></c>".parse().unwrap();
        let error = Caps::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in Caps element.");
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<query xmlns='http://jabber.org/protocol/disco#info'><identity category='client' type='pc'/><feature var='http://jabber.org/protocol/disco#info'/></query>".parse().unwrap();
        let disco = DiscoInfoResult::try_from(elem).unwrap();
        let caps = caps::compute_disco(&disco);
        assert_eq!(caps.len(), 50);
    }

    #[test]
    fn test_xep_5_2() {
        let elem: Element = r#"<query xmlns='http://jabber.org/protocol/disco#info'
       node='http://psi-im.org#q07IKJEyjvHSyhy//CH0CxmKi8w='>
  <identity category='client' name='Exodus 0.9.1' type='pc'/>
  <feature var='http://jabber.org/protocol/caps'/>
  <feature var='http://jabber.org/protocol/disco#info'/>
  <feature var='http://jabber.org/protocol/disco#items'/>
  <feature var='http://jabber.org/protocol/muc'/>
</query>
"#
        .parse()
        .unwrap();

        let expected = b"client/pc//Exodus 0.9.1<http://jabber.org/protocol/caps<http://jabber.org/protocol/disco#info<http://jabber.org/protocol/disco#items<http://jabber.org/protocol/muc<".to_vec();
        let disco = DiscoInfoResult::try_from(elem).unwrap();
        let caps = caps::compute_disco(&disco);
        assert_eq!(caps, expected);

        let sha_1 = caps::hash_caps(&caps, Algo::Sha_1).unwrap();
        assert_eq!(
            sha_1.hash,
            Base64.decode("QgayPKawpkPSDYmwT/WM94uAlu0=").unwrap()
        );
    }

    #[test]
    fn test_xep_5_3() {
        let elem: Element = r#"<query xmlns='http://jabber.org/protocol/disco#info'
       node='http://psi-im.org#q07IKJEyjvHSyhy//CH0CxmKi8w='>
  <identity xml:lang='en' category='client' name='Psi 0.11' type='pc'/>
  <identity xml:lang='el' category='client' name='Ψ 0.11' type='pc'/>
  <feature var='http://jabber.org/protocol/caps'/>
  <feature var='http://jabber.org/protocol/disco#info'/>
  <feature var='http://jabber.org/protocol/disco#items'/>
  <feature var='http://jabber.org/protocol/muc'/>
  <x xmlns='jabber:x:data' type='result'>
    <field var='FORM_TYPE' type='hidden'>
      <value>urn:xmpp:dataforms:softwareinfo</value>
    </field>
    <field var='ip_version'>
      <value>ipv4</value>
      <value>ipv6</value>
    </field>
    <field var='os'>
      <value>Mac</value>
    </field>
    <field var='os_version'>
      <value>10.5.1</value>
    </field>
    <field var='software'>
      <value>Psi</value>
    </field>
    <field var='software_version'>
      <value>0.11</value>
    </field>
  </x>
</query>
"#
        .parse()
        .unwrap();
        let expected = b"client/pc/el/\xce\xa8 0.11<client/pc/en/Psi 0.11<http://jabber.org/protocol/caps<http://jabber.org/protocol/disco#info<http://jabber.org/protocol/disco#items<http://jabber.org/protocol/muc<urn:xmpp:dataforms:softwareinfo<ip_version<ipv4<ipv6<os<Mac<os_version<10.5.1<software<Psi<software_version<0.11<".to_vec();
        let disco = DiscoInfoResult::try_from(elem).unwrap();
        let caps = caps::compute_disco(&disco);
        assert_eq!(caps, expected);

        let sha_1 = caps::hash_caps(&caps, Algo::Sha_1).unwrap();
        assert_eq!(
            sha_1.hash,
            Base64.decode("q07IKJEyjvHSyhy//CH0CxmKi8w=").unwrap()
        );
    }
}
