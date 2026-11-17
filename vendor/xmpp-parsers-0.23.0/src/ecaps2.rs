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
use sha2::{Sha256, Sha512};
use sha3::{Sha3_256, Sha3_512};
use xso::error::Error;

/// Represents a set of capability hashes, all of them must correspond to
/// the same input [disco#info](../disco/struct.DiscoInfoResult.html),
/// using different [algorithms](../hashes/enum.Algo.html).
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::ECAPS2, name = "c")]
pub struct ECaps2 {
    /// Hashes of the [disco#info](../disco/struct.DiscoInfoResult.html).
    #[xml(child(n = ..))]
    pub hashes: Vec<Hash>,
}

impl PresencePayload for ECaps2 {}

impl ECaps2 {
    /// Create an ECaps2 element from a list of hashes.
    pub fn new(hashes: Vec<Hash>) -> ECaps2 {
        ECaps2 { hashes }
    }
}

fn compute_item(field: &str) -> Vec<u8> {
    let mut bytes = field.as_bytes().to_vec();
    bytes.push(0x1f);
    bytes
}

fn compute_items<'x, T: 'x, I: IntoIterator<Item = &'x T>, F: Fn(&'x T) -> Vec<u8>>(
    things: I,
    separator: u8,
    encode: F,
) -> Vec<u8> {
    let mut string: Vec<u8> = vec![];
    let mut accumulator: Vec<Vec<u8>> = vec![];
    for thing in things.into_iter() {
        let bytes = encode(thing);
        accumulator.push(bytes);
    }
    // This works using the expected i;octet collation.
    accumulator.sort();
    for mut bytes in accumulator {
        string.append(&mut bytes);
    }
    string.push(separator);
    string
}

fn compute_features(features: &[String]) -> Vec<u8> {
    compute_items(features, 0x1c, |feature| compute_item(&feature))
}

fn compute_identities(identities: &[Identity]) -> Vec<u8> {
    compute_items(identities, 0x1c, |identity| {
        let mut bytes = compute_item(&identity.category);
        bytes.append(&mut compute_item(&identity.type_));
        bytes.append(&mut compute_item(
            identity.lang.as_deref().unwrap_or_default(),
        ));
        bytes.append(&mut compute_item(
            identity.name.as_deref().unwrap_or_default(),
        ));
        bytes.push(0x1e);
        bytes
    })
}

fn compute_extensions(extensions: &[DataForm]) -> Result<Vec<u8>, Error> {
    for extension in extensions {
        if extension.form_type().is_none() {
            return Err(Error::Other("Missing FORM_TYPE in extension."));
        }
    }
    Ok(compute_items(extensions, 0x1c, |extension| {
        let mut bytes = compute_item("FORM_TYPE");
        bytes.append(&mut compute_item(
            if let Some(form_type) = extension.form_type() {
                form_type
            } else {
                unreachable!()
            },
        ));
        bytes.push(0x1e);
        bytes.append(&mut compute_items(
            extension
                .fields
                .iter()
                .filter(|field| field.var.as_deref().unwrap_or("") != "FORM_TYPE"),
            0x1d,
            |field| {
                let mut bytes = vec![];
                if let Some(var) = &field.var {
                    bytes.append(&mut compute_item(var));
                }
                bytes.append(&mut compute_items(&field.values, 0x1e, |value| {
                    compute_item(value)
                }));
                bytes
            },
        ));
        bytes
    }))
}

/// Applies the [algorithm from
/// XEP-0390](https://xmpp.org/extensions/xep-0390.html#algorithm-input) on a
/// [disco#info query element](../disco/struct.DiscoInfoResult.html).
pub fn compute_disco(disco: &DiscoInfoResult) -> Result<Vec<u8>, Error> {
    // TODO: Figure out a way to remove the clones here.
    let features: Vec<_> = disco.features.iter().cloned().collect();

    let features_string = compute_features(&features);
    let identities_string = compute_identities(&disco.identities);
    let extensions_string = compute_extensions(&disco.extensions)?;

    let mut final_string = vec![];
    final_string.extend(features_string);
    final_string.extend(identities_string);
    final_string.extend(extensions_string);
    Ok(final_string)
}

/// Hashes the result of [compute_disco()] with one of the supported [hash
/// algorithms](../hashes/enum.Algo.html).
pub fn hash_ecaps2(data: &[u8], algo: Algo) -> Result<Hash, Error> {
    Ok(Hash {
        hash: match algo {
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
            Algo::Sha_1 => return Err(Error::Other("Disabled algorithm sha-1: unsafe.")),
            Algo::Unknown(_algo) => return Err(Error::Other("Unknown algorithm in ecaps2.")),
        },
        algo,
    })
}

/// Helper function to create the query for the disco#info corresponding to an
/// ecaps2 hash.
pub fn query_ecaps2(hash: Hash) -> DiscoInfoQuery {
    DiscoInfoQuery {
        node: Some(format!(
            "{}#{}.{}",
            ns::ECAPS2,
            String::from(hash.algo),
            Base64.encode(&hash.hash)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xso::error::FromElementError;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(ECaps2, 12);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(ECaps2, 24);
    }

    #[test]
    fn test_parse() {
        let elem: Element = "<c xmlns='urn:xmpp:caps'><hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>K1Njy3HZBThlo4moOD5gBGhn0U0oK7/CbfLlIUDi6o4=</hash><hash xmlns='urn:xmpp:hashes:2' algo='sha3-256'>+sDTQqBmX6iG/X3zjt06fjZMBBqL/723knFIyRf0sg8=</hash></c>".parse().unwrap();
        let ecaps2 = ECaps2::try_from(elem).unwrap();
        assert_eq!(ecaps2.hashes.len(), 2);
        assert_eq!(ecaps2.hashes[0].algo, Algo::Sha_256);
        assert_eq!(
            ecaps2.hashes[0].hash,
            Base64
                .decode("K1Njy3HZBThlo4moOD5gBGhn0U0oK7/CbfLlIUDi6o4=")
                .unwrap()
        );
        assert_eq!(ecaps2.hashes[1].algo, Algo::Sha3_256);
        assert_eq!(
            ecaps2.hashes[1].hash,
            Base64
                .decode("+sDTQqBmX6iG/X3zjt06fjZMBBqL/723knFIyRf0sg8=")
                .unwrap()
        );
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_invalid_child() {
        let elem: Element = "<c xmlns='urn:xmpp:caps'><hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>K1Njy3HZBThlo4moOD5gBGhn0U0oK7/CbfLlIUDi6o4=</hash><hash xmlns='urn:xmpp:hashes:1' algo='sha3-256'>+sDTQqBmX6iG/X3zjt06fjZMBBqL/723knFIyRf0sg8=</hash></c>".parse().unwrap();
        let error = ECaps2::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in ECaps2 element.");
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<query xmlns='http://jabber.org/protocol/disco#info'><identity category='client' type='pc'/><feature var='http://jabber.org/protocol/disco#info'/></query>".parse().unwrap();
        let disco = DiscoInfoResult::try_from(elem).unwrap();
        let ecaps2 = compute_disco(&disco).unwrap();
        assert_eq!(ecaps2.len(), 54);
    }

    #[test]
    fn test_xep_ex1() {
        let elem: Element = r#"<query xmlns="http://jabber.org/protocol/disco#info">
  <identity category="client" name="BombusMod" type="mobile"/>
  <feature var="http://jabber.org/protocol/si"/>
  <feature var="http://jabber.org/protocol/bytestreams"/>
  <feature var="http://jabber.org/protocol/chatstates"/>
  <feature var="http://jabber.org/protocol/disco#info"/>
  <feature var="http://jabber.org/protocol/disco#items"/>
  <feature var="urn:xmpp:ping"/>
  <feature var="jabber:iq:time"/>
  <feature var="jabber:iq:privacy"/>
  <feature var="jabber:iq:version"/>
  <feature var="http://jabber.org/protocol/rosterx"/>
  <feature var="urn:xmpp:time"/>
  <feature var="jabber:x:oob"/>
  <feature var="http://jabber.org/protocol/ibb"/>
  <feature var="http://jabber.org/protocol/si/profile/file-transfer"/>
  <feature var="urn:xmpp:receipts"/>
  <feature var="jabber:iq:roster"/>
  <feature var="jabber:iq:last"/>
</query>
"#
        .parse()
        .unwrap();
        let expected = vec![
            104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114, 103, 47, 112,
            114, 111, 116, 111, 99, 111, 108, 47, 98, 121, 116, 101, 115, 116, 114, 101, 97, 109,
            115, 31, 104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114, 103,
            47, 112, 114, 111, 116, 111, 99, 111, 108, 47, 99, 104, 97, 116, 115, 116, 97, 116,
            101, 115, 31, 104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114,
            103, 47, 112, 114, 111, 116, 111, 99, 111, 108, 47, 100, 105, 115, 99, 111, 35, 105,
            110, 102, 111, 31, 104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111,
            114, 103, 47, 112, 114, 111, 116, 111, 99, 111, 108, 47, 100, 105, 115, 99, 111, 35,
            105, 116, 101, 109, 115, 31, 104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114,
            46, 111, 114, 103, 47, 112, 114, 111, 116, 111, 99, 111, 108, 47, 105, 98, 98, 31, 104,
            116, 116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114, 103, 47, 112, 114,
            111, 116, 111, 99, 111, 108, 47, 114, 111, 115, 116, 101, 114, 120, 31, 104, 116, 116,
            112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114, 103, 47, 112, 114, 111, 116,
            111, 99, 111, 108, 47, 115, 105, 31, 104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98,
            101, 114, 46, 111, 114, 103, 47, 112, 114, 111, 116, 111, 99, 111, 108, 47, 115, 105,
            47, 112, 114, 111, 102, 105, 108, 101, 47, 102, 105, 108, 101, 45, 116, 114, 97, 110,
            115, 102, 101, 114, 31, 106, 97, 98, 98, 101, 114, 58, 105, 113, 58, 108, 97, 115, 116,
            31, 106, 97, 98, 98, 101, 114, 58, 105, 113, 58, 112, 114, 105, 118, 97, 99, 121, 31,
            106, 97, 98, 98, 101, 114, 58, 105, 113, 58, 114, 111, 115, 116, 101, 114, 31, 106, 97,
            98, 98, 101, 114, 58, 105, 113, 58, 116, 105, 109, 101, 31, 106, 97, 98, 98, 101, 114,
            58, 105, 113, 58, 118, 101, 114, 115, 105, 111, 110, 31, 106, 97, 98, 98, 101, 114, 58,
            120, 58, 111, 111, 98, 31, 117, 114, 110, 58, 120, 109, 112, 112, 58, 112, 105, 110,
            103, 31, 117, 114, 110, 58, 120, 109, 112, 112, 58, 114, 101, 99, 101, 105, 112, 116,
            115, 31, 117, 114, 110, 58, 120, 109, 112, 112, 58, 116, 105, 109, 101, 31, 28, 99,
            108, 105, 101, 110, 116, 31, 109, 111, 98, 105, 108, 101, 31, 31, 66, 111, 109, 98,
            117, 115, 77, 111, 100, 31, 30, 28, 28,
        ];
        let disco = DiscoInfoResult::try_from(elem).unwrap();
        let ecaps2 = compute_disco(&disco).unwrap();
        assert_eq!(ecaps2.len(), 0x1d9);
        assert_eq!(ecaps2, expected);

        let sha_256 = hash_ecaps2(&ecaps2, Algo::Sha_256).unwrap();
        assert_eq!(
            sha_256.hash,
            Base64
                .decode("kzBZbkqJ3ADrj7v08reD1qcWUwNGHaidNUgD7nHpiw8=")
                .unwrap()
        );
        let sha3_256 = hash_ecaps2(&ecaps2, Algo::Sha3_256).unwrap();
        assert_eq!(
            sha3_256.hash,
            Base64
                .decode("79mdYAfU9rEdTOcWDO7UEAt6E56SUzk/g6TnqUeuD9Q=")
                .unwrap()
        );
    }

    #[test]
    fn test_xep_ex2() {
        let elem: Element = r#"<query xmlns="http://jabber.org/protocol/disco#info">
  <identity category="client" name="Tkabber" type="pc" xml:lang="en"/>
  <identity category="client" name="Ткаббер" type="pc" xml:lang="ru"/>
  <feature var="games:board"/>
  <feature var="http://jabber.org/protocol/activity"/>
  <feature var="http://jabber.org/protocol/activity+notify"/>
  <feature var="http://jabber.org/protocol/bytestreams"/>
  <feature var="http://jabber.org/protocol/chatstates"/>
  <feature var="http://jabber.org/protocol/commands"/>
  <feature var="http://jabber.org/protocol/disco#info"/>
  <feature var="http://jabber.org/protocol/disco#items"/>
  <feature var="http://jabber.org/protocol/evil"/>
  <feature var="http://jabber.org/protocol/feature-neg"/>
  <feature var="http://jabber.org/protocol/geoloc"/>
  <feature var="http://jabber.org/protocol/geoloc+notify"/>
  <feature var="http://jabber.org/protocol/ibb"/>
  <feature var="http://jabber.org/protocol/iqibb"/>
  <feature var="http://jabber.org/protocol/mood"/>
  <feature var="http://jabber.org/protocol/mood+notify"/>
  <feature var="http://jabber.org/protocol/rosterx"/>
  <feature var="http://jabber.org/protocol/si"/>
  <feature var="http://jabber.org/protocol/si/profile/file-transfer"/>
  <feature var="http://jabber.org/protocol/tune"/>
  <feature var="http://www.facebook.com/xmpp/messages"/>
  <feature var="http://www.xmpp.org/extensions/xep-0084.html#ns-metadata+notify"/>
  <feature var="jabber:iq:avatar"/>
  <feature var="jabber:iq:browse"/>
  <feature var="jabber:iq:dtcp"/>
  <feature var="jabber:iq:filexfer"/>
  <feature var="jabber:iq:ibb"/>
  <feature var="jabber:iq:inband"/>
  <feature var="jabber:iq:jidlink"/>
  <feature var="jabber:iq:last"/>
  <feature var="jabber:iq:oob"/>
  <feature var="jabber:iq:privacy"/>
  <feature var="jabber:iq:roster"/>
  <feature var="jabber:iq:time"/>
  <feature var="jabber:iq:version"/>
  <feature var="jabber:x:data"/>
  <feature var="jabber:x:event"/>
  <feature var="jabber:x:oob"/>
  <feature var="urn:xmpp:avatar:metadata+notify"/>
  <feature var="urn:xmpp:ping"/>
  <feature var="urn:xmpp:receipts"/>
  <feature var="urn:xmpp:time"/>
  <x xmlns="jabber:x:data" type="result">
    <field type="hidden" var="FORM_TYPE">
      <value>urn:xmpp:dataforms:softwareinfo</value>
    </field>
    <field var="software">
      <value>Tkabber</value>
    </field>
    <field var="software_version">
      <value>0.11.1-svn-20111216-mod (Tcl/Tk 8.6b2)</value>
    </field>
    <field var="os">
      <value>Windows</value>
    </field>
    <field var="os_version">
      <value>XP</value>
    </field>
  </x>
</query>
"#
        .parse()
        .unwrap();
        let expected = vec![
            103, 97, 109, 101, 115, 58, 98, 111, 97, 114, 100, 31, 104, 116, 116, 112, 58, 47, 47,
            106, 97, 98, 98, 101, 114, 46, 111, 114, 103, 47, 112, 114, 111, 116, 111, 99, 111,
            108, 47, 97, 99, 116, 105, 118, 105, 116, 121, 31, 104, 116, 116, 112, 58, 47, 47, 106,
            97, 98, 98, 101, 114, 46, 111, 114, 103, 47, 112, 114, 111, 116, 111, 99, 111, 108, 47,
            97, 99, 116, 105, 118, 105, 116, 121, 43, 110, 111, 116, 105, 102, 121, 31, 104, 116,
            116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114, 103, 47, 112, 114, 111,
            116, 111, 99, 111, 108, 47, 98, 121, 116, 101, 115, 116, 114, 101, 97, 109, 115, 31,
            104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114, 103, 47, 112,
            114, 111, 116, 111, 99, 111, 108, 47, 99, 104, 97, 116, 115, 116, 97, 116, 101, 115,
            31, 104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114, 103, 47,
            112, 114, 111, 116, 111, 99, 111, 108, 47, 99, 111, 109, 109, 97, 110, 100, 115, 31,
            104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114, 103, 47, 112,
            114, 111, 116, 111, 99, 111, 108, 47, 100, 105, 115, 99, 111, 35, 105, 110, 102, 111,
            31, 104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114, 103, 47,
            112, 114, 111, 116, 111, 99, 111, 108, 47, 100, 105, 115, 99, 111, 35, 105, 116, 101,
            109, 115, 31, 104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114,
            103, 47, 112, 114, 111, 116, 111, 99, 111, 108, 47, 101, 118, 105, 108, 31, 104, 116,
            116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114, 103, 47, 112, 114, 111,
            116, 111, 99, 111, 108, 47, 102, 101, 97, 116, 117, 114, 101, 45, 110, 101, 103, 31,
            104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114, 103, 47, 112,
            114, 111, 116, 111, 99, 111, 108, 47, 103, 101, 111, 108, 111, 99, 31, 104, 116, 116,
            112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114, 103, 47, 112, 114, 111, 116,
            111, 99, 111, 108, 47, 103, 101, 111, 108, 111, 99, 43, 110, 111, 116, 105, 102, 121,
            31, 104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114, 103, 47,
            112, 114, 111, 116, 111, 99, 111, 108, 47, 105, 98, 98, 31, 104, 116, 116, 112, 58, 47,
            47, 106, 97, 98, 98, 101, 114, 46, 111, 114, 103, 47, 112, 114, 111, 116, 111, 99, 111,
            108, 47, 105, 113, 105, 98, 98, 31, 104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98,
            101, 114, 46, 111, 114, 103, 47, 112, 114, 111, 116, 111, 99, 111, 108, 47, 109, 111,
            111, 100, 31, 104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114,
            103, 47, 112, 114, 111, 116, 111, 99, 111, 108, 47, 109, 111, 111, 100, 43, 110, 111,
            116, 105, 102, 121, 31, 104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46,
            111, 114, 103, 47, 112, 114, 111, 116, 111, 99, 111, 108, 47, 114, 111, 115, 116, 101,
            114, 120, 31, 104, 116, 116, 112, 58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114,
            103, 47, 112, 114, 111, 116, 111, 99, 111, 108, 47, 115, 105, 31, 104, 116, 116, 112,
            58, 47, 47, 106, 97, 98, 98, 101, 114, 46, 111, 114, 103, 47, 112, 114, 111, 116, 111,
            99, 111, 108, 47, 115, 105, 47, 112, 114, 111, 102, 105, 108, 101, 47, 102, 105, 108,
            101, 45, 116, 114, 97, 110, 115, 102, 101, 114, 31, 104, 116, 116, 112, 58, 47, 47,
            106, 97, 98, 98, 101, 114, 46, 111, 114, 103, 47, 112, 114, 111, 116, 111, 99, 111,
            108, 47, 116, 117, 110, 101, 31, 104, 116, 116, 112, 58, 47, 47, 119, 119, 119, 46,
            102, 97, 99, 101, 98, 111, 111, 107, 46, 99, 111, 109, 47, 120, 109, 112, 112, 47, 109,
            101, 115, 115, 97, 103, 101, 115, 31, 104, 116, 116, 112, 58, 47, 47, 119, 119, 119,
            46, 120, 109, 112, 112, 46, 111, 114, 103, 47, 101, 120, 116, 101, 110, 115, 105, 111,
            110, 115, 47, 120, 101, 112, 45, 48, 48, 56, 52, 46, 104, 116, 109, 108, 35, 110, 115,
            45, 109, 101, 116, 97, 100, 97, 116, 97, 43, 110, 111, 116, 105, 102, 121, 31, 106, 97,
            98, 98, 101, 114, 58, 105, 113, 58, 97, 118, 97, 116, 97, 114, 31, 106, 97, 98, 98,
            101, 114, 58, 105, 113, 58, 98, 114, 111, 119, 115, 101, 31, 106, 97, 98, 98, 101, 114,
            58, 105, 113, 58, 100, 116, 99, 112, 31, 106, 97, 98, 98, 101, 114, 58, 105, 113, 58,
            102, 105, 108, 101, 120, 102, 101, 114, 31, 106, 97, 98, 98, 101, 114, 58, 105, 113,
            58, 105, 98, 98, 31, 106, 97, 98, 98, 101, 114, 58, 105, 113, 58, 105, 110, 98, 97,
            110, 100, 31, 106, 97, 98, 98, 101, 114, 58, 105, 113, 58, 106, 105, 100, 108, 105,
            110, 107, 31, 106, 97, 98, 98, 101, 114, 58, 105, 113, 58, 108, 97, 115, 116, 31, 106,
            97, 98, 98, 101, 114, 58, 105, 113, 58, 111, 111, 98, 31, 106, 97, 98, 98, 101, 114,
            58, 105, 113, 58, 112, 114, 105, 118, 97, 99, 121, 31, 106, 97, 98, 98, 101, 114, 58,
            105, 113, 58, 114, 111, 115, 116, 101, 114, 31, 106, 97, 98, 98, 101, 114, 58, 105,
            113, 58, 116, 105, 109, 101, 31, 106, 97, 98, 98, 101, 114, 58, 105, 113, 58, 118, 101,
            114, 115, 105, 111, 110, 31, 106, 97, 98, 98, 101, 114, 58, 120, 58, 100, 97, 116, 97,
            31, 106, 97, 98, 98, 101, 114, 58, 120, 58, 101, 118, 101, 110, 116, 31, 106, 97, 98,
            98, 101, 114, 58, 120, 58, 111, 111, 98, 31, 117, 114, 110, 58, 120, 109, 112, 112, 58,
            97, 118, 97, 116, 97, 114, 58, 109, 101, 116, 97, 100, 97, 116, 97, 43, 110, 111, 116,
            105, 102, 121, 31, 117, 114, 110, 58, 120, 109, 112, 112, 58, 112, 105, 110, 103, 31,
            117, 114, 110, 58, 120, 109, 112, 112, 58, 114, 101, 99, 101, 105, 112, 116, 115, 31,
            117, 114, 110, 58, 120, 109, 112, 112, 58, 116, 105, 109, 101, 31, 28, 99, 108, 105,
            101, 110, 116, 31, 112, 99, 31, 101, 110, 31, 84, 107, 97, 98, 98, 101, 114, 31, 30,
            99, 108, 105, 101, 110, 116, 31, 112, 99, 31, 114, 117, 31, 208, 162, 208, 186, 208,
            176, 208, 177, 208, 177, 208, 181, 209, 128, 31, 30, 28, 70, 79, 82, 77, 95, 84, 89,
            80, 69, 31, 117, 114, 110, 58, 120, 109, 112, 112, 58, 100, 97, 116, 97, 102, 111, 114,
            109, 115, 58, 115, 111, 102, 116, 119, 97, 114, 101, 105, 110, 102, 111, 31, 30, 111,
            115, 31, 87, 105, 110, 100, 111, 119, 115, 31, 30, 111, 115, 95, 118, 101, 114, 115,
            105, 111, 110, 31, 88, 80, 31, 30, 115, 111, 102, 116, 119, 97, 114, 101, 31, 84, 107,
            97, 98, 98, 101, 114, 31, 30, 115, 111, 102, 116, 119, 97, 114, 101, 95, 118, 101, 114,
            115, 105, 111, 110, 31, 48, 46, 49, 49, 46, 49, 45, 115, 118, 110, 45, 50, 48, 49, 49,
            49, 50, 49, 54, 45, 109, 111, 100, 32, 40, 84, 99, 108, 47, 84, 107, 32, 56, 46, 54,
            98, 50, 41, 31, 30, 29, 28,
        ];
        let disco = DiscoInfoResult::try_from(elem).unwrap();
        let ecaps2 = compute_disco(&disco).unwrap();
        assert_eq!(ecaps2.len(), 0x543);
        assert_eq!(ecaps2, expected);

        let sha_256 = hash_ecaps2(&ecaps2, Algo::Sha_256).unwrap();
        assert_eq!(
            sha_256.hash,
            Base64
                .decode("u79ZroNJbdSWhdSp311mddz44oHHPsEBntQ5b1jqBSY=")
                .unwrap()
        );
        let sha3_256 = hash_ecaps2(&ecaps2, Algo::Sha3_256).unwrap();
        assert_eq!(
            sha3_256.hash,
            Base64
                .decode("XpUJzLAc93258sMECZ3FJpebkzuyNXDzRNwQog8eycg=")
                .unwrap()
        );
    }

    #[test]
    fn test_blake2b_512() {
        let hash = hash_ecaps2("abc".as_bytes(), Algo::Blake2b_512).unwrap();
        let known_hash: Vec<u8> = vec![
            0xBA, 0x80, 0xA5, 0x3F, 0x98, 0x1C, 0x4D, 0x0D, 0x6A, 0x27, 0x97, 0xB6, 0x9F, 0x12,
            0xF6, 0xE9, 0x4C, 0x21, 0x2F, 0x14, 0x68, 0x5A, 0xC4, 0xB7, 0x4B, 0x12, 0xBB, 0x6F,
            0xDB, 0xFF, 0xA2, 0xD1, 0x7D, 0x87, 0xC5, 0x39, 0x2A, 0xAB, 0x79, 0x2D, 0xC2, 0x52,
            0xD5, 0xDE, 0x45, 0x33, 0xCC, 0x95, 0x18, 0xD3, 0x8A, 0xA8, 0xDB, 0xF1, 0x92, 0x5A,
            0xB9, 0x23, 0x86, 0xED, 0xD4, 0x00, 0x99, 0x23,
        ];
        assert_eq!(hash.hash, known_hash);
    }
}
