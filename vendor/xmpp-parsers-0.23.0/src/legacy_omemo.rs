// Copyright (c) 2022 Yureka Lilian <yuka@yuka.dev>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{text::Base64, AsXml, FromXml};

use crate::message::MessagePayload;
use crate::ns;
use crate::pubsub::PubSubPayload;

/// Element of the device list
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::LEGACY_OMEMO, name = "device")]
pub struct Device {
    /// Device id
    #[xml(attribute)]
    pub id: u32,
}

/// A user's device list contains the OMEMO device ids of all the user's
/// devicse. These can be used to look up bundles and build a session.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::LEGACY_OMEMO, name = "list")]
pub struct DeviceList {
    /// List of devices
    #[xml(child(n = ..))]
    pub devices: Vec<Device>,
}

impl PubSubPayload for DeviceList {}

/// SignedPreKey public key
/// Part of a device's bundle
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::LEGACY_OMEMO, name = "signedPreKeyPublic")]
pub struct SignedPreKeyPublic {
    /// SignedPreKey id
    #[xml(attribute(default, name = "signedPreKeyId"))]
    pub signed_pre_key_id: Option<u32>,

    /// Serialized PublicKey
    #[xml(text = Base64)]
    pub data: Vec<u8>,
}

/// SignedPreKey signature
/// Part of a device's bundle
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::LEGACY_OMEMO, name = "signedPreKeySignature")]
pub struct SignedPreKeySignature {
    /// Signature bytes
    #[xml(text = Base64)]
    pub data: Vec<u8>,
}

/// Part of a device's bundle
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::LEGACY_OMEMO, name = "identityKey")]
pub struct IdentityKey {
    /// Serialized PublicKey
    #[xml(text = Base64)]
    pub data: Vec<u8>,
}

/// List of (single use) PreKeys
/// Part of a device's bundle
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::LEGACY_OMEMO, name = "prekeys")]
pub struct Prekeys {
    /// List of (single use) PreKeys
    #[xml(child(n = ..))]
    pub keys: Vec<PreKeyPublic>,
}

/// PreKey public key
/// Part of a device's bundle
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::LEGACY_OMEMO, name = "preKeyPublic")]
pub struct PreKeyPublic {
    /// PreKey id
    #[xml(attribute = "preKeyId")]
    pub pre_key_id: u32,

    /// Serialized PublicKey
    #[xml(text = Base64)]
    pub data: Vec<u8>,
}

/// A collection of publicly accessible data that can be used to build a
/// session with a device, namely its public IdentityKey, a signed PreKey with
/// corresponding signature, and a list of (single use) PreKeys.
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::LEGACY_OMEMO, name = "bundle")]
pub struct Bundle {
    /// SignedPreKey public key
    #[xml(child(default))]
    pub signed_pre_key_public: Option<SignedPreKeyPublic>,

    /// SignedPreKey signature
    #[xml(child(default))]
    pub signed_pre_key_signature: Option<SignedPreKeySignature>,

    /// IdentityKey public key
    #[xml(child(default))]
    pub identity_key: Option<IdentityKey>,

    /// List of (single use) PreKeys
    #[xml(child(default))]
    pub prekeys: Option<Prekeys>,
}

impl PubSubPayload for Bundle {}

/// The header contains encrypted keys for a message
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::LEGACY_OMEMO, name = "header")]
pub struct Header {
    /// The device id of the sender
    #[xml(attribute)]
    pub sid: u32,

    /// The key that the payload message is encrypted with, separately
    /// encrypted for each recipient device.
    #[xml(child(n = ..))]
    pub keys: Vec<Key>,

    /// IV used for payload encryption
    #[xml(child)]
    pub iv: IV,
}

/// IV used for payload encryption
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::LEGACY_OMEMO, name = "iv")]
pub struct IV {
    /// IV bytes
    #[xml(text = Base64)]
    pub data: Vec<u8>,
}

/// Part of the OMEMO element header
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::LEGACY_OMEMO, name = "key")]
pub struct Key {
    /// The device id this key is encrypted for.
    #[xml(attribute)]
    pub rid: u32,

    /// The key element MUST be tagged with a prekey attribute set to true
    /// if a PreKeySignalMessage is being used.
    #[xml(attribute(default))]
    pub prekey: bool,

    /// The 16 bytes key and the GCM authentication tag concatenated together
    /// and encrypted using the corresponding long-standing SignalProtocol
    /// session
    #[xml(text = Base64)]
    pub data: Vec<u8>,
}

/// The encrypted message body
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::LEGACY_OMEMO, name = "payload")]
pub struct Payload {
    /// Encrypted with AES-128 in Galois/Counter Mode (GCM)
    #[xml(text = Base64)]
    pub data: Vec<u8>,
}

/// An OMEMO element, which can be either a MessageElement (with payload),
/// or a KeyTransportElement (without payload).
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::LEGACY_OMEMO, name = "encrypted")]
pub struct Encrypted {
    /// The header contains encrypted keys for a message
    #[xml(child)]
    pub header: Header,

    /// Payload for MessageElement
    #[xml(child(default))]
    pub payload: Option<Payload>,
}

impl MessagePayload for Encrypted {}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[test]
    fn parse_bundle() {
        let elem: Element = r#"<bundle xmlns="eu.siacs.conversations.axolotl">
  <signedPreKeyPublic signedPreKeyId="1">BYAbACA15bPn95p7RGC2XbgQyly8aRKS4BaJ+hD8Ybhe</signedPreKeyPublic>
  <signedPreKeySignature>sIJVNDZi/NgFsry4OBdM+adyGttLEXbUh/h/5dVOZveMgyVoIdgwBUzq8Wgd2xYTQMioNzwYebTX+9p0h9eujA==</signedPreKeySignature>
  <identityKey>BQFd2p/Oq97vAAdLKA09DlcSg0x1xWn260p1jaeyIhAZ</identityKey>
  <prekeys>
    <preKeyPublic preKeyId="1">BbjHsF5ndtNV8ToRcJTYSNGePgAWsFGkSL6OG7B7LXRe</preKeyPublic>
    <preKeyPublic preKeyId="2">BeWHsbBNx1uer1ia/nW/6tn/OlqHll9itjjUTIvV39x7</preKeyPublic>
    <preKeyPublic preKeyId="3">BeVr5xPmNErkwK3ocPmv0Nohy3C4PKQBnxMuOqiXotJY</preKeyPublic>
  </prekeys>
</bundle>
        "#.parse().unwrap();
        let bundle: Bundle = elem.try_into().unwrap();
        let bundle2 = Bundle {
            signed_pre_key_public: Some(SignedPreKeyPublic {
                signed_pre_key_id: Some(1),
                data: vec![
                    5, 128, 27, 0, 32, 53, 229, 179, 231, 247, 154, 123, 68, 96, 182, 93, 184, 16,
                    202, 92, 188, 105, 18, 146, 224, 22, 137, 250, 16, 252, 97, 184, 94,
                ],
            }),
            signed_pre_key_signature: Some(SignedPreKeySignature {
                data: vec![
                    176, 130, 85, 52, 54, 98, 252, 216, 5, 178, 188, 184, 56, 23, 76, 249, 167,
                    114, 26, 219, 75, 17, 118, 212, 135, 248, 127, 229, 213, 78, 102, 247, 140,
                    131, 37, 104, 33, 216, 48, 5, 76, 234, 241, 104, 29, 219, 22, 19, 64, 200, 168,
                    55, 60, 24, 121, 180, 215, 251, 218, 116, 135, 215, 174, 140,
                ],
            }),
            identity_key: Some(IdentityKey {
                data: vec![
                    5, 1, 93, 218, 159, 206, 171, 222, 239, 0, 7, 75, 40, 13, 61, 14, 87, 18, 131,
                    76, 117, 197, 105, 246, 235, 74, 117, 141, 167, 178, 34, 16, 25,
                ],
            }),
            prekeys: Some(Prekeys {
                keys: vec![
                    PreKeyPublic {
                        pre_key_id: 1,
                        data: vec![
                            5, 184, 199, 176, 94, 103, 118, 211, 85, 241, 58, 17, 112, 148, 216,
                            72, 209, 158, 62, 0, 22, 176, 81, 164, 72, 190, 142, 27, 176, 123, 45,
                            116, 94,
                        ],
                    },
                    PreKeyPublic {
                        pre_key_id: 2,
                        data: vec![
                            5, 229, 135, 177, 176, 77, 199, 91, 158, 175, 88, 154, 254, 117, 191,
                            234, 217, 255, 58, 90, 135, 150, 95, 98, 182, 56, 212, 76, 139, 213,
                            223, 220, 123,
                        ],
                    },
                    PreKeyPublic {
                        pre_key_id: 3,
                        data: vec![
                            5, 229, 107, 231, 19, 230, 52, 74, 228, 192, 173, 232, 112, 249, 175,
                            208, 218, 33, 203, 112, 184, 60, 164, 1, 159, 19, 46, 58, 168, 151,
                            162, 210, 88,
                        ],
                    },
                ],
            }),
        };
        assert_eq!(bundle, bundle2);
    }
    #[test]
    fn parse_device_list() {
        let elem: Element = r#"<list xmlns="eu.siacs.conversations.axolotl">
  <device id="1164059891" />
  <device id="26052318" />
  <device id="564866972" />
</list>
        "#
        .parse()
        .unwrap();
        let list: DeviceList = elem.try_into().unwrap();
        let list2 = DeviceList {
            devices: vec![
                Device { id: 1164059891 },
                Device { id: 26052318 },
                Device { id: 564866972 },
            ],
        };
        assert_eq!(list, list2);
    }
    #[test]
    fn parse_encrypted() {
        let elem: Element = r#"<encrypted xmlns="eu.siacs.conversations.axolotl">
  <header sid="564866972">
    <key prekey="true" rid="1236">Mwjp9AESIQVylscLPpj/HlowaTiIsaBj73HCVEllXpVTtMG9EYwRexohBQFd2p/Oq97vAAdLKA09DlcSg0x1xWn260p1jaeyIhAZImIzCiEFhaQ4I+DuQgo6vCLCjHu4uewDZmWHuBl8uJw1IkyZxhUQABgAIjCoEVgVThWlaIlnN3V5Bg1hQX7OD1cvstLD5lH3zZMadL3KeONELESlBbeKmNgcYC/e3HZnbgWzBiic36yNAjAW</key>
    <key rid="26052318">MwohBTV6dpumL1OxA9MdIFmu2E19+cIWDHWYfhdubvo0hmh6EAAYHCIwNc9/59eeYi8pVZQhMJJMVkKUkFP/yrTfG3o1lfpHGseCqb/JTgtDytQPiYrTpHl2V/mdsM6IPig=</key>
    <key rid="1164059891">MwohBVnhz9pvEj1s1waEHuk5qpQqhUrpavycFz0hq/KYwI8oEAAYCSIwedEGN6MidxyvaPI8zorLcpG0Y7e7ecGkkd5vdDrL7Qt1tXaHb0iDyE/rZZHpFiNN38Izfp5vHv4=</key>
    <iv>SY/SCGPt0CnA2odB</iv>
  </header>
  <payload>Vas=</payload>
</encrypted>
        "#.parse().unwrap();
        let encrypted: Encrypted = elem.try_into().unwrap();
        let encrypted2 = Encrypted {
            header: Header {
                sid: 564866972,
                keys: vec![
                    Key {
                        rid: 1236,
                        prekey: true,
                        data: vec![
                            51, 8, 233, 244, 1, 18, 33, 5, 114, 150, 199, 11, 62, 152, 255, 30, 90,
                            48, 105, 56, 136, 177, 160, 99, 239, 113, 194, 84, 73, 101, 94, 149,
                            83, 180, 193, 189, 17, 140, 17, 123, 26, 33, 5, 1, 93, 218, 159, 206,
                            171, 222, 239, 0, 7, 75, 40, 13, 61, 14, 87, 18, 131, 76, 117, 197,
                            105, 246, 235, 74, 117, 141, 167, 178, 34, 16, 25, 34, 98, 51, 10, 33,
                            5, 133, 164, 56, 35, 224, 238, 66, 10, 58, 188, 34, 194, 140, 123, 184,
                            185, 236, 3, 102, 101, 135, 184, 25, 124, 184, 156, 53, 34, 76, 153,
                            198, 21, 16, 0, 24, 0, 34, 48, 168, 17, 88, 21, 78, 21, 165, 104, 137,
                            103, 55, 117, 121, 6, 13, 97, 65, 126, 206, 15, 87, 47, 178, 210, 195,
                            230, 81, 247, 205, 147, 26, 116, 189, 202, 120, 227, 68, 44, 68, 165,
                            5, 183, 138, 152, 216, 28, 96, 47, 222, 220, 118, 103, 110, 5, 179, 6,
                            40, 156, 223, 172, 141, 2, 48, 22,
                        ],
                    },
                    Key {
                        rid: 26052318,
                        prekey: false,
                        data: vec![
                            51, 10, 33, 5, 53, 122, 118, 155, 166, 47, 83, 177, 3, 211, 29, 32, 89,
                            174, 216, 77, 125, 249, 194, 22, 12, 117, 152, 126, 23, 110, 110, 250,
                            52, 134, 104, 122, 16, 0, 24, 28, 34, 48, 53, 207, 127, 231, 215, 158,
                            98, 47, 41, 85, 148, 33, 48, 146, 76, 86, 66, 148, 144, 83, 255, 202,
                            180, 223, 27, 122, 53, 149, 250, 71, 26, 199, 130, 169, 191, 201, 78,
                            11, 67, 202, 212, 15, 137, 138, 211, 164, 121, 118, 87, 249, 157, 176,
                            206, 136, 62, 40,
                        ],
                    },
                    Key {
                        rid: 1164059891,
                        prekey: false,
                        data: vec![
                            51, 10, 33, 5, 89, 225, 207, 218, 111, 18, 61, 108, 215, 6, 132, 30,
                            233, 57, 170, 148, 42, 133, 74, 233, 106, 252, 156, 23, 61, 33, 171,
                            242, 152, 192, 143, 40, 16, 0, 24, 9, 34, 48, 121, 209, 6, 55, 163, 34,
                            119, 28, 175, 104, 242, 60, 206, 138, 203, 114, 145, 180, 99, 183, 187,
                            121, 193, 164, 145, 222, 111, 116, 58, 203, 237, 11, 117, 181, 118,
                            135, 111, 72, 131, 200, 79, 235, 101, 145, 233, 22, 35, 77, 223, 194,
                            51, 126, 158, 111, 30, 254,
                        ],
                    },
                ],
                iv: IV {
                    data: vec![73, 143, 210, 8, 99, 237, 208, 41, 192, 218, 135, 65],
                },
            },
            payload: Some(Payload {
                data: vec![85, 171],
            }),
        };
        assert_eq!(encrypted, encrypted2);
    }
}
