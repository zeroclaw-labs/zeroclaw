// Copyright (c) 2024 xmpp-rs contributors.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! This module implements vCard, for the purpose of vCard-based avatars as defined in
//! [XEP-0054](https://xmpp.org/extensions/xep-0054.html).
//!
//! Only the `<PHOTO>` element is supported as a member of this legacy vCard. For more modern and complete
//! user profile management, see [XEP-0292](https://xmpp.org/extensions/xep-0292.html): vCard4 Over XMPP.
//!
//! For vCard updates defined in [XEP-0153](https://xmpp.org/extensions/xep-0153.html),
//! see [`vcard_update`][crate::vcard_update] module.

use xso::{
    text::{Base64, StripWhitespace, TextCodec},
    AsXml, FromXml,
};

use crate::iq::{IqGetPayload, IqResultPayload, IqSetPayload};
use crate::ns;
use minidom::Element;

/// A photo element.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::VCARD, name = "PHOTO")]
pub struct Photo {
    /// The type of the photo.
    #[xml(child)]
    pub type_: Type,

    /// The binary data of the photo.
    #[xml(child)]
    pub binval: Binval,
}

/// The type of the photo.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::VCARD, name = "TYPE")]
pub struct Type {
    /// The type as a plain text string; at least "image/jpeg", "image/gif" and "image/png" SHOULD be supported.
    #[xml(text)]
    pub data: String,
}

/// The binary data of the photo.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::VCARD, name = "BINVAL")]
pub struct Binval {
    /// The actual data.
    #[xml(text(codec = Base64.filtered(StripWhitespace)))]
    pub data: Vec<u8>,
}

/// A `<vCard>` element; only the `<PHOTO>` element is supported for this legacy vCard ; the rest is ignored.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::VCARD, name = "vCard")]
pub struct VCard {
    /// A photo element.
    #[xml(child(default))]
    pub photo: Option<Photo>,

    /// Every other element in the vCard.
    #[xml(element(n = ..))]
    pub payloads: Vec<Element>,
}

impl IqSetPayload for VCard {}
impl IqResultPayload for VCard {}

/// A `<vCard/>` request element, for use in iq get.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::VCARD, name = "vCard")]
pub struct VCardQuery;

impl IqGetPayload for VCardQuery {}

#[cfg(test)]
mod tests {
    use super::*;
    use core::str::FromStr;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Photo, 24);
        assert_size!(Type, 12);
        assert_size!(Binval, 12);
        assert_size!(VCard, 36);
        assert_size!(VCardQuery, 0);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Photo, 48);
        assert_size!(Type, 24);
        assert_size!(Binval, 24);
        assert_size!(VCard, 72);
        assert_size!(VCardQuery, 0);
    }

    #[test]
    fn test_vcard() {
        // Create some bytes:
        let bytes = [0u8, 1, 2, 129];

        // Test xml stolen from https://xmpp.org/extensions/xep-0153.html#example-5
        let test_vcard = format!(
            r"<vCard xmlns='vcard-temp'>
    <BDAY>1476-06-09</BDAY>
    <ADR>
      <CTRY>Italy</CTRY>
      <LOCALITY>Verona</LOCALITY>
      <HOME/>
    </ADR>
    <NICKNAME/>
    <N><GIVEN>Juliet</GIVEN><FAMILY>Capulet</FAMILY></N>
    <EMAIL>jcapulet@shakespeare.lit</EMAIL>
    <PHOTO>
      <TYPE>image/jpeg</TYPE>
      <BINVAL>{}</BINVAL>
    </PHOTO>
  </vCard>",
            base64::Engine::encode(&base64::prelude::BASE64_STANDARD, &bytes)
        );

        let test_vcard = Element::from_str(&test_vcard).expect("Failed to parse XML");
        let test_vcard = VCard::try_from(test_vcard).expect("Failed to parse vCard");

        let photo = test_vcard.photo.expect("No photo found");

        assert_eq!(photo.type_.data, "image/jpeg".to_string());
        assert_eq!(photo.binval.data, bytes);
    }

    #[test]
    fn test_vcard_with_linebreaks() {
        // Test xml stolen from https://xmpp.org/extensions/xep-0153.html#example-5
        // extended to use a multi-line base64 string as is allowed as per RFC 2426
        let test_vcard = r"<vCard xmlns='vcard-temp'>
    <BDAY>1476-06-09</BDAY>
    <ADR>
      <CTRY>Italy</CTRY>
      <LOCALITY>Verona</LOCALITY>
      <HOME/>
    </ADR>
    <NICKNAME/>
    <N><GIVEN>Juliet</GIVEN><FAMILY>Capulet</FAMILY></N>
    <EMAIL>jcapulet@shakespeare.lit</EMAIL>
    <PHOTO>
      <TYPE>image/jpeg</TYPE>
      <BINVAL>Zm9v
Cg==</BINVAL>
    </PHOTO>
  </vCard>";

        let test_vcard = Element::from_str(&test_vcard).expect("Failed to parse XML");
        let test_vcard = VCard::try_from(test_vcard).expect("Failed to parse vCard");

        let photo = test_vcard.photo.expect("No photo found");

        assert_eq!(photo.type_.data, "image/jpeg".to_string());
        assert_eq!(photo.binval.data, b"foo\n");
    }
}
