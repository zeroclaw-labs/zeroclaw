// Copyright (c) 2024 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::ns;

/// Request to start TLS.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::TLS, name = "starttls")]
pub struct Request;

/// Information that TLS may now commence.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::TLS, name = "proceed")]
pub struct Proceed;

/// Information that the peer cannot do TLS after all.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::TLS, name = "failure")]
pub struct Failure;

/// Stream feature for StartTLS
///
/// Used in [`crate::stream_features::StreamFeatures`].
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::TLS, name = "starttls")]
pub struct StartTls {
    /// Marker for mandatory StartTLS.
    #[xml(flag)]
    pub required: bool,
}

/// Enum which allows parsing/serialising any STARTTLS element.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml()]
pub enum Nonza {
    /// Request to start TLS
    #[xml(transparent)]
    Request(Request),

    /// Information that TLS may now commence
    #[xml(transparent)]
    Proceed(Proceed),

    /// Information that the peer cannot do TLS after all.
    #[xml(transparent)]
    Failure(Failure),
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[test]
    fn test_size() {
        assert_size!(Request, 0);
        assert_size!(Proceed, 0);
        assert_size!(Failure, 0);
        assert_size!(StartTls, 1);
        assert_size!(Nonza, 1);
    }

    #[test]
    fn test_parsers() {
        let elem: Element = "<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>"
            .parse()
            .unwrap();
        let request = Request::try_from(elem.clone()).unwrap();
        let elem2 = Element::from(request);
        assert_eq!(elem, elem2);

        let elem: Element = "<proceed xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>"
            .parse()
            .unwrap();
        let proceed = Proceed::try_from(elem.clone()).unwrap();
        let elem2 = Element::from(proceed);
        assert_eq!(elem, elem2);

        let elem: Element = "<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>"
            .parse()
            .unwrap();
        let starttls = StartTls::try_from(elem.clone()).unwrap();
        assert_eq!(starttls.required, false);
        let elem2 = Element::from(starttls);
        assert_eq!(elem, elem2);

        let elem: Element =
            "<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'><required/></starttls>"
                .parse()
                .unwrap();
        let starttls = StartTls::try_from(elem.clone()).unwrap();
        assert_eq!(starttls.required, true);
        let elem2 = Element::from(starttls);
        assert_eq!(elem, elem2);

        let elem: Element = "<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>"
            .parse()
            .unwrap();
        let nonza = Nonza::try_from(elem.clone()).unwrap();
        assert_eq!(nonza, Nonza::Request(Request));
        let elem2 = Element::from(nonza);
        assert_eq!(elem, elem2);

        let elem: Element = "<proceed xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>"
            .parse()
            .unwrap();
        let nonza = Nonza::try_from(elem.clone()).unwrap();
        assert_eq!(nonza, Nonza::Proceed(Proceed));
        let elem2 = Element::from(nonza);
        assert_eq!(elem, elem2);
    }
}
