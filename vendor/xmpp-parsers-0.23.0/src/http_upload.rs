// Copyright (c) 2021 Maxime “pep” Buquet <pep@bouah.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{error::Error, AsXml, AsXmlText, FromXml, FromXmlText};

use crate::iq::{IqGetPayload, IqResultPayload};
use crate::ns;
use alloc::borrow::Cow;

/// Requesting a slot
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::HTTP_UPLOAD, name = "request")]
pub struct SlotRequest {
    /// The filename to be uploaded.
    #[xml(attribute)]
    pub filename: String,

    /// Size of the file to be uploaded.
    #[xml(attribute)]
    pub size: u64,

    /// Content-Type of the file.
    #[xml(attribute(name = "content-type"))]
    pub content_type: Option<String>,
}

impl IqGetPayload for SlotRequest {}

/// All three possible header names.
#[derive(Debug, Clone, PartialEq)]
pub enum HeaderName {
    /// Authorization header
    Authorization,

    /// Cookie header
    Cookie,

    /// Expires header
    Expires,
}

impl HeaderName {
    /// Returns the string version of this enum value.
    pub fn as_str(&self) -> &'static str {
        match self {
            HeaderName::Authorization => "Authorization",
            HeaderName::Cookie => "Cookie",
            HeaderName::Expires => "Expires",
        }
    }
}

impl FromXmlText for HeaderName {
    fn from_xml_text(mut s: String) -> Result<Self, Error> {
        s.make_ascii_lowercase();
        Ok(match s.as_str() {
            "authorization" => HeaderName::Authorization,
            "cookie" => HeaderName::Cookie,
            "expires" => HeaderName::Expires,
            _ => {
                return Err(Error::Other(
                    "Header name must be either 'Authorization', 'Cookie', or 'Expires'.",
                ))
            }
        })
    }
}

impl AsXmlText for HeaderName {
    fn as_xml_text(&self) -> Result<Cow<'_, str>, Error> {
        Ok(Cow::Borrowed(self.as_str()))
    }
}

/// Slot header
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::HTTP_UPLOAD, name = "header")]
pub struct Header {
    /// Name of the header
    #[xml(attribute)]
    pub name: HeaderName,

    /// Value of the header
    #[xml(text)]
    pub value: String,
}

/// Put URL
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::HTTP_UPLOAD, name = "put")]
pub struct Put {
    /// URL
    #[xml(attribute)]
    pub url: String,

    /// Header list
    #[xml(child(n = ..))]
    pub headers: Vec<Header>,
}

/// Get URL
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::HTTP_UPLOAD, name = "get")]
pub struct Get {
    /// URL
    #[xml(attribute)]
    pub url: String,
}

/// Requesting a slot
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::HTTP_UPLOAD, name = "slot")]
pub struct SlotResult {
    /// Put URL and headers
    #[xml(child)]
    pub put: Put,

    /// Get URL
    #[xml(child)]
    pub get: Get,
}

impl IqResultPayload for SlotResult {}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(SlotRequest, 32);
        assert_size!(HeaderName, 1);
        assert_size!(Header, 16);
        assert_size!(Put, 24);
        assert_size!(Get, 12);
        assert_size!(SlotResult, 36);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(SlotRequest, 56);
        assert_size!(HeaderName, 1);
        assert_size!(Header, 32);
        assert_size!(Put, 48);
        assert_size!(Get, 24);
        assert_size!(SlotResult, 72);
    }

    #[test]
    fn test_slot_request() {
        let elem: Element = "<request xmlns='urn:xmpp:http:upload:0'
            filename='très cool.jpg'
            size='23456'
            content-type='image/jpeg' />"
            .parse()
            .unwrap();
        let slot = SlotRequest::try_from(elem).unwrap();
        assert_eq!(slot.filename, String::from("très cool.jpg"));
        assert_eq!(slot.size, 23456);
        assert_eq!(slot.content_type, Some(String::from("image/jpeg")));
    }

    #[test]
    fn test_slot_result() {
        let elem: Element = "<slot xmlns='urn:xmpp:http:upload:0'>
            <put url='https://upload.montague.tld/4a771ac1-f0b2-4a4a-9700-f2a26fa2bb67/tr%C3%A8s%20cool.jpg'>
              <header name='Authorization'>Basic Base64String==</header>
              <header name='Cookie'>foo=bar; user=romeo</header>
            </put>
            <get url='https://download.montague.tld/4a771ac1-f0b2-4a4a-9700-f2a26fa2bb67/tr%C3%A8s%20cool.jpg' />
          </slot>"
            .parse()
            .unwrap();
        let slot = SlotResult::try_from(elem).unwrap();
        assert_eq!(slot.put.url, String::from("https://upload.montague.tld/4a771ac1-f0b2-4a4a-9700-f2a26fa2bb67/tr%C3%A8s%20cool.jpg"));
        assert_eq!(
            slot.put.headers[0],
            Header {
                name: HeaderName::Authorization,
                value: String::from("Basic Base64String==")
            }
        );
        assert_eq!(
            slot.put.headers[1],
            Header {
                name: HeaderName::Cookie,
                value: String::from("foo=bar; user=romeo")
            }
        );
        assert_eq!(slot.get.url, String::from("https://download.montague.tld/4a771ac1-f0b2-4a4a-9700-f2a26fa2bb67/tr%C3%A8s%20cool.jpg"));
    }

    #[test]
    fn test_result_no_header() {
        let elem: Element = "<slot xmlns='urn:xmpp:http:upload:0'>
            <put url='https://URL' />
            <get url='https://URL' />
          </slot>"
            .parse()
            .unwrap();
        let slot = SlotResult::try_from(elem).unwrap();
        assert_eq!(slot.put.url, String::from("https://URL"));
        assert_eq!(slot.put.headers.len(), 0);
        assert_eq!(slot.get.url, String::from("https://URL"));
    }

    #[test]
    fn test_result_bad_header() {
        let elem: Element = "<slot xmlns='urn:xmpp:http:upload:0'>
            <put url='https://URL'>
              <header name='EvilHeader'>EvilValue</header>
            </put>
            <get url='https://URL' />
          </slot>"
            .parse()
            .unwrap();
        SlotResult::try_from(elem).unwrap_err();
    }
}
