// Copyright (c) 2025 Maxime “pep” Buquet <pep@bouah.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use crate::ns;

use xso::{AsXml, FromXml};

/// Structure representing a `<json xmlns='urn:xmpp:json:0'/>` element.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::JSON_CONTAINERS, name = "json")]
pub struct JsonContainer(#[xml(text)] serde_json::Value);

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    use crate::Error::TextParseError;
    use xso::error::FromElementError;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(JsonContainer, 24);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(JsonContainer, 32);
    }

    #[test]
    fn test_empty() {
        let elem: Element = "<json xmlns='urn:xmpp:json:0'/>".parse().unwrap();
        let error = JsonContainer::try_from(elem.clone()).unwrap_err();
        match error {
            FromElementError::Invalid(TextParseError(err)) => {
                assert_eq!(
                    err.to_string(),
                    "EOF while parsing a value at line 1 column 0"
                );
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<json xmlns='urn:xmpp:json:0'>{\"a\": 1}</json>"
            .parse()
            .unwrap();
        let result: serde_json::Value = "{\"a\": 1}".parse().unwrap();
        match JsonContainer::try_from(elem.clone()) {
            Ok(json) => assert_eq!(json.0, result),
            _ => panic!(),
        };
    }
}
