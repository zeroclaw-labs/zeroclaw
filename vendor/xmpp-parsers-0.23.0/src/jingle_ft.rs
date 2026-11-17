// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{error::Error, AsXml, FromXml};

use crate::date::DateTime;
use crate::hashes::Hash;
use crate::jingle::{ContentId, Creator};
use crate::ns;
use alloc::collections::btree_map::BTreeMap;
use core::str::FromStr;

/// Represents a range in a file.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone, Default)]
#[xml(namespace = ns::JINGLE_FT, name = "range")]
pub struct Range {
    /// The offset in bytes from the beginning of the file.
    #[xml(attribute(default))]
    pub offset: u64,

    /// The length in bytes of the range, or None to be the entire
    /// remaining of the file.
    #[xml(attribute(default))]
    pub length: Option<u64>,

    /// List of hashes for this range.
    #[xml(child(n = ..))]
    pub hashes: Vec<Hash>,
}

impl Range {
    /// Creates a new range.
    pub fn new() -> Range {
        Default::default()
    }
}

type Lang = String;

/// Represents a file to be transferred.
#[derive(FromXml, AsXml, Debug, Clone, Default)]
#[xml(namespace = ns::JINGLE_FT, name = "file")]
pub struct File {
    /// The date of last modification of this file.
    #[xml(extract(default, fields(text(type_ = DateTime))))]
    pub date: Option<DateTime>,

    /// The MIME type of this file.
    #[xml(extract(default, name = "media-type", fields(text(type_ = String))))]
    pub media_type: Option<String>,

    /// The name of this file.
    #[xml(extract(default, fields(text(type_ = String))))]
    pub name: Option<String>,

    /// The description of this file, possibly localised.
    #[xml(extract(n = .., name = "desc", fields(
        lang(type_ = Lang, default),
        text(type_ = String)
    )))]
    pub descs: BTreeMap<Lang, String>,

    /// The size of this file, in bytes.
    #[xml(extract(default, fields(text(type_ = u64))))]
    pub size: Option<u64>,

    /// Used to request only a part of this file.
    #[xml(child(default))]
    pub range: Option<Range>,

    /// A list of hashes matching this entire file.
    #[xml(child(n = ..))]
    pub hashes: Vec<Hash>,
}

impl File {
    /// Creates a new file descriptor.
    pub fn new() -> File {
        File::default()
    }

    /// Sets the date of last modification on this file.
    pub fn with_date(mut self, date: DateTime) -> File {
        self.date = Some(date);
        self
    }

    /// Sets the date of last modification on this file from an ISO-8601
    /// string.
    pub fn with_date_str(mut self, date: &str) -> Result<File, Error> {
        self.date = Some(DateTime::from_str(date).map_err(Error::text_parse_error)?);
        Ok(self)
    }

    /// Sets the MIME type of this file.
    pub fn with_media_type(mut self, media_type: String) -> File {
        self.media_type = Some(media_type);
        self
    }

    /// Sets the name of this file.
    pub fn with_name(mut self, name: String) -> File {
        self.name = Some(name);
        self
    }

    /// Sets a description for this file.
    pub fn add_desc(mut self, lang: &str, desc: String) -> File {
        self.descs.insert(Lang::from(lang), desc);
        self
    }

    /// Sets the file size of this file, in bytes.
    pub fn with_size(mut self, size: u64) -> File {
        self.size = Some(size);
        self
    }

    /// Request only a range of this file.
    pub fn with_range(mut self, range: Range) -> File {
        self.range = Some(range);
        self
    }

    /// Add a hash on this file.
    pub fn add_hash(mut self, hash: Hash) -> File {
        self.hashes.push(hash);
        self
    }
}

/// A wrapper element for a file.
#[derive(FromXml, AsXml, Debug, Clone, Default)]
#[xml(namespace = ns::JINGLE_FT, name = "description")]
pub struct Description {
    /// The actual file descriptor.
    #[xml(child)]
    pub file: File,
}

/// A checksum for checking that the file has been transferred correctly.
#[derive(FromXml, AsXml, Debug, Clone)]
#[xml(namespace = ns::JINGLE_FT, name = "checksum")]
pub struct Checksum {
    /// The identifier of the file transfer content.
    #[xml(attribute)]
    pub name: ContentId,

    /// The creator of this file transfer.
    #[xml(attribute)]
    pub creator: Creator,

    /// The file being checksummed.
    #[xml(child)]
    pub file: File,
}

/// A notice that the file transfer has been completed.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::JINGLE_FT, name = "received")]
pub struct Received {
    /// The content identifier of this Jingle session.
    #[xml(attribute)]
    pub name: ContentId,

    /// The creator of this file transfer.
    #[xml(attribute)]
    pub creator: Creator,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hashes::Algo;
    use base64::{engine::general_purpose::STANDARD as Base64, Engine};
    use minidom::Element;
    use xso::error::FromElementError;

    // Apparently, i686 and AArch32/PowerPC seem to disagree here. So instead
    // of trying to figure this out now, we just ignore the test.
    #[cfg(target_pointer_width = "32")]
    #[test]
    #[ignore]
    fn test_size() {
        assert_size!(Range, 32);
        assert_size!(File, 104);
        assert_size!(Description, 104);
        assert_size!(Checksum, 128);
        assert_size!(Received, 16);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Range, 48);
        assert_size!(File, 176);
        assert_size!(Description, 176);
        assert_size!(Checksum, 208);
        assert_size!(Received, 32);
    }

    #[test]
    fn test_description() {
        let elem: Element = r#"<description xmlns='urn:xmpp:jingle:apps:file-transfer:5'>
  <file>
    <media-type>text/plain</media-type>
    <name>test.txt</name>
    <date>2015-07-26T21:46:00+01:00</date>
    <size>6144</size>
    <hash xmlns='urn:xmpp:hashes:2'
          algo='sha-1'>w0mcJylzCn+AfvuGdqkty2+KP48=</hash>
  </file>
</description>
"#
        .parse()
        .unwrap();
        let desc = Description::try_from(elem).unwrap();
        assert_eq!(desc.file.media_type, Some(String::from("text/plain")));
        assert_eq!(desc.file.name, Some(String::from("test.txt")));
        assert_eq!(desc.file.descs, BTreeMap::new());
        assert_eq!(
            desc.file.date,
            DateTime::from_str("2015-07-26T21:46:00+01:00").ok()
        );
        assert_eq!(desc.file.size, Some(6144u64));
        assert_eq!(desc.file.range, None);
        assert_eq!(desc.file.hashes[0].algo, Algo::Sha_1);
        assert_eq!(
            desc.file.hashes[0].hash,
            Base64.decode("w0mcJylzCn+AfvuGdqkty2+KP48=").unwrap()
        );
    }

    #[test]
    fn test_request() {
        let elem: Element = r#"<description xmlns='urn:xmpp:jingle:apps:file-transfer:5'>
  <file>
    <hash xmlns='urn:xmpp:hashes:2'
          algo='sha-1'>w0mcJylzCn+AfvuGdqkty2+KP48=</hash>
  </file>
</description>
"#
        .parse()
        .unwrap();
        let desc = Description::try_from(elem).unwrap();
        assert_eq!(desc.file.media_type, None);
        assert_eq!(desc.file.name, None);
        assert_eq!(desc.file.descs, BTreeMap::new());
        assert_eq!(desc.file.date, None);
        assert_eq!(desc.file.size, None);
        assert_eq!(desc.file.range, None);
        assert_eq!(desc.file.hashes[0].algo, Algo::Sha_1);
        assert_eq!(
            desc.file.hashes[0].hash,
            Base64.decode("w0mcJylzCn+AfvuGdqkty2+KP48=").unwrap()
        );
    }

    #[test]
    // TODO: Reenable that test once we correctly treat same @xml:lang as errors!
    #[ignore]
    fn test_descs() {
        let elem: Element = r#"<description xmlns='urn:xmpp:jingle:apps:file-transfer:5'>
  <file>
    <media-type>text/plain</media-type>
    <desc xml:lang='fr'>Fichier secret !</desc>
    <desc xml:lang='en'>Secret file!</desc>
    <hash xmlns='urn:xmpp:hashes:2'
          algo='sha-1'>w0mcJylzCn+AfvuGdqkty2+KP48=</hash>
  </file>
</description>
"#
        .parse()
        .unwrap();
        let desc = Description::try_from(elem).unwrap();
        assert_eq!(
            desc.file.descs.keys().cloned().collect::<Vec<_>>(),
            ["en", "fr"]
        );
        assert_eq!(desc.file.descs["en"], String::from("Secret file!"));
        assert_eq!(desc.file.descs["fr"], String::from("Fichier secret !"));

        let elem: Element = r#"<description xmlns='urn:xmpp:jingle:apps:file-transfer:5'>
  <file>
    <media-type>text/plain</media-type>
    <desc xml:lang='fr'>Fichier secret !</desc>
    <desc xml:lang='fr'>Secret file!</desc>
    <hash xmlns='urn:xmpp:hashes:2'
          algo='sha-1'>w0mcJylzCn+AfvuGdqkty2+KP48=</hash>
  </file>
</description>
"#
        .parse()
        .unwrap();
        let error = Description::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Desc element present twice for the same xml:lang.");
    }

    #[test]
    fn test_received_valid() {
        let elem: Element = "<received xmlns='urn:xmpp:jingle:apps:file-transfer:5' name='coucou' creator='initiator'/>".parse().unwrap();
        let received = Received::try_from(elem).unwrap();
        assert_eq!(received.name, ContentId(String::from("coucou")));
        assert_eq!(received.creator, Creator::Initiator);
        let elem2 = Element::from(received.clone());
        let received2 = Received::try_from(elem2).unwrap();
        assert_eq!(received2.name, ContentId(String::from("coucou")));
        assert_eq!(received2.creator, Creator::Initiator);
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_received_unknown_child() {
        let elem: Element = "<received xmlns='urn:xmpp:jingle:apps:file-transfer:5' name='coucou' creator='initiator'><coucou/></received>".parse().unwrap();
        let error = Received::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in Received element.");
    }

    #[test]
    fn test_received_missing_name() {
        let elem: Element =
            "<received xmlns='urn:xmpp:jingle:apps:file-transfer:5' creator='initiator'/>"
                .parse()
                .unwrap();
        let error = Received::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'name' on Received element missing."
        );
    }

    #[test]
    fn test_received_invalid_creator() {
        let elem: Element = "<received xmlns='urn:xmpp:jingle:apps:file-transfer:5' name='coucou' creator='coucou'/>".parse().unwrap();
        let error = Received::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message.to_string(),
            "Unknown value for 'creator' attribute."
        );
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_invalid_received() {
        let elem: Element = "<received xmlns='urn:xmpp:jingle:apps:file-transfer:5' name='coucou' creator='initiator' coucou=''/>".parse().unwrap();
        let error = Received::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown attribute in Received element.");
    }

    #[test]
    fn test_checksum_valid() {
        let elem: Element = "<checksum xmlns='urn:xmpp:jingle:apps:file-transfer:5' name='coucou' creator='initiator'><file><hash xmlns='urn:xmpp:hashes:2' algo='sha-1'>w0mcJylzCn+AfvuGdqkty2+KP48=</hash></file></checksum>".parse().unwrap();
        let hash = vec![
            195, 73, 156, 39, 41, 115, 10, 127, 128, 126, 251, 134, 118, 169, 45, 203, 111, 138,
            63, 143,
        ];
        let checksum = Checksum::try_from(elem).unwrap();
        assert_eq!(checksum.name, ContentId(String::from("coucou")));
        assert_eq!(checksum.creator, Creator::Initiator);
        assert_eq!(
            checksum.file.hashes,
            vec!(Hash {
                algo: Algo::Sha_1,
                hash: hash.clone()
            })
        );
        let elem2 = Element::from(checksum);
        let checksum2 = Checksum::try_from(elem2).unwrap();
        assert_eq!(checksum2.name, ContentId(String::from("coucou")));
        assert_eq!(checksum2.creator, Creator::Initiator);
        assert_eq!(
            checksum2.file.hashes,
            vec!(Hash {
                algo: Algo::Sha_1,
                hash: hash.clone()
            })
        );
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_checksum_unknown_child() {
        let elem: Element = "<checksum xmlns='urn:xmpp:jingle:apps:file-transfer:5' name='coucou' creator='initiator'><file><hash xmlns='urn:xmpp:hashes:2' algo='sha-1'>w0mcJylzCn+AfvuGdqkty2+KP48=</hash></file><coucou/></checksum>".parse().unwrap();
        let error = Checksum::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            other => panic!("unexpected error: {:?}", other),
        };
        assert_eq!(message, "Unknown child in Checksum element.");
    }

    #[test]
    fn test_checksum_missing_name() {
        let elem: Element = "<checksum xmlns='urn:xmpp:jingle:apps:file-transfer:5' creator='initiator'><file><hash xmlns='urn:xmpp:hashes:2' algo='sha-1'>w0mcJylzCn+AfvuGdqkty2+KP48=</hash></file></checksum>".parse().unwrap();
        let error = Checksum::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'name' on Checksum element missing."
        );
    }

    #[test]
    fn test_checksum_invalid_creator() {
        let elem: Element = "<checksum xmlns='urn:xmpp:jingle:apps:file-transfer:5' name='coucou' creator='coucou'><file><hash xmlns='urn:xmpp:hashes:2' algo='sha-1'>w0mcJylzCn+AfvuGdqkty2+KP48=</hash></file></checksum>".parse().unwrap();
        let error = Checksum::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message.to_string(),
            "Unknown value for 'creator' attribute."
        );
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_invalid_checksum() {
        let elem: Element = "<checksum xmlns='urn:xmpp:jingle:apps:file-transfer:5' name='coucou' creator='initiator' coucou=''><file><hash xmlns='urn:xmpp:hashes:2' algo='sha-1'>w0mcJylzCn+AfvuGdqkty2+KP48=</hash></file></checksum>".parse().unwrap();
        let error = Checksum::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown attribute in Checksum element.");
    }

    #[test]
    fn test_range() {
        let elem: Element = "<range xmlns='urn:xmpp:jingle:apps:file-transfer:5'/>"
            .parse()
            .unwrap();
        let range = Range::try_from(elem).unwrap();
        assert_eq!(range.offset, 0);
        assert_eq!(range.length, None);
        assert_eq!(range.hashes, vec!());

        let elem: Element = "<range xmlns='urn:xmpp:jingle:apps:file-transfer:5' offset='2048' length='1024'><hash xmlns='urn:xmpp:hashes:2' algo='sha-1'>kHp5RSzW/h7Gm1etSf90Mr5PC/k=</hash></range>".parse().unwrap();
        let hashes = vec![Hash {
            algo: Algo::Sha_1,
            hash: vec![
                144, 122, 121, 69, 44, 214, 254, 30, 198, 155, 87, 173, 73, 255, 116, 50, 190, 79,
                11, 249,
            ],
        }];
        let range = Range::try_from(elem).unwrap();
        assert_eq!(range.offset, 2048);
        assert_eq!(range.length, Some(1024));
        assert_eq!(range.hashes, hashes);
        let elem2 = Element::from(range);
        let range2 = Range::try_from(elem2).unwrap();
        assert_eq!(range2.offset, 2048);
        assert_eq!(range2.length, Some(1024));
        assert_eq!(range2.hashes, hashes);
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_invalid_range() {
        let elem: Element = "<range xmlns='urn:xmpp:jingle:apps:file-transfer:5' coucou=''/>"
            .parse()
            .unwrap();
        let error = Range::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown attribute in Range element.");
    }
}
