// Copyright (c) 2024 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{error::Error, AsXml, AsXmlText, FromXml, FromXmlText};

use crate::ns;
use alloc::borrow::Cow;

/// One type of channel-binding, as [defined by the IANA](https://www.iana.org/assignments/channel-binding-types/channel-binding-types.xhtml)
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// The tls-unique channel binding.
    TlsUnique,

    /// The tls-server-end-point channel binding.
    TlsServerEndPoint,

    /// The tls-unique-for-telnet channel binding.
    TlsUniqueForTelnet,

    /// The EKM value obtained from the current TLS connection.
    ///
    /// See RFC9266.
    TlsExporter,
}

impl FromXmlText for Type {
    fn from_xml_text(s: String) -> Result<Type, Error> {
        Ok(match s.as_ref() {
            "tls-unique" => Type::TlsUnique,
            "tls-server-end-point" => Type::TlsServerEndPoint,
            "tls-unique-for-telnet" => Type::TlsUniqueForTelnet,
            "tls-exporter" => Type::TlsExporter,

            _ => return Err(Error::Other("Unknown value '{s}' for 'type' attribute.")),
        })
    }
}

impl AsXmlText for Type {
    fn as_xml_text(&self) -> Result<Cow<'_, str>, Error> {
        Ok(Cow::Borrowed(match self {
            Type::TlsUnique => "tls-unique",
            Type::TlsServerEndPoint => "tls-server-end-point",
            Type::TlsUniqueForTelnet => "tls-unique-for-telnet",
            Type::TlsExporter => "tls-exporter",
        }))
    }
}

/// Stream feature listing the channel-binding types supported by the server.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::SASL_CB, name = "sasl-channel-binding")]
pub struct SaslChannelBinding {
    /// The list of channel-binding types supported by the server.
    #[xml(extract(n = .., name = "channel-binding", fields(attribute(name = "type", type_ = Type))))]
    pub types: Vec<Type>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Type, 1);
        assert_size!(SaslChannelBinding, 24);
    }

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Type, 1);
        assert_size!(SaslChannelBinding, 12);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<sasl-channel-binding xmlns='urn:xmpp:sasl-cb:0'><channel-binding type='tls-server-end-point'/><channel-binding type='tls-exporter'/></sasl-channel-binding>".parse().unwrap();
        let sasl_cb = SaslChannelBinding::try_from(elem).unwrap();
        assert_eq!(sasl_cb.types, [Type::TlsServerEndPoint, Type::TlsExporter]);
    }
}
