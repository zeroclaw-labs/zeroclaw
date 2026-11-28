// Copyright (c) 2024 Paul Fariello <xmpp-parsers@fariello.eu>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::message::MessagePayload;
use crate::ns;

/// Defines associated out of band url.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::OOB, name = "x")]
pub struct Oob {
    /// The associated URL.
    #[xml(extract(fields(text)))]
    pub url: String,

    /// An optional description of the out of band data.
    #[xml(extract(default, fields(text(type_ = String))))]
    pub desc: Option<String>,
}

impl MessagePayload for Oob {}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Oob, 24);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Oob, 48);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<x xmlns='jabber:x:oob'><url>http://example.org</url></x>"
            .parse()
            .unwrap();
        Oob::try_from(elem).unwrap();
    }

    #[test]
    fn test_with_desc() {
        let elem: Element =
            "<x xmlns='jabber:x:oob'><url>http://example.org</url><desc>Example website</desc></x>"
                .parse()
                .unwrap();
        Oob::try_from(elem).unwrap();
    }

    #[test]
    fn test_invalid_child() {
        let elem: Element = "<x xmlns='jabber:x:oob'></x>".parse().unwrap();
        let error = Oob::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Missing child field 'url' in Oob element.");
    }
}
