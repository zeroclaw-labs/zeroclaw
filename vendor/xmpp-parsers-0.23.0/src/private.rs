// Copyright (c) 2023 xmppftw <xmppftw@kl.netlib.re>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//!
//! This module implements [Private XML Storage](https://xmpp.org/extensions/xep-0049.html) from
//! XEP-0049.
//!
//! However, only legacy bookmarks storage from [XEP-0048
//! v1.0](https://xmpp.org/extensions/attic/xep-0048-1.0.html) is supported at the moment.
//! This should only be used when `urn:xmpp:bookmarks:1#compat` is not advertised on the user's
//! BareJID in a disco info request.
//!
//! See [ModernXMPP docs](https://docs.modernxmpp.org/client/groupchat/#bookmarks) on how to handle
//! all historic and newer specifications for your clients handling of chatroom bookmarks.
//!
//! This module uses the legacy bookmarks [`bookmarks::Conference`][crate::bookmarks::Conference]
//! struct as stored in a legacy [`bookmarks::Storage`][crate::bookmarks::Storage] struct.

use xso::{AsXml, FromXml};

use crate::{
    bookmarks::Storage,
    iq::{IqGetPayload, IqResultPayload, IqSetPayload},
    ns,
};

/// A Private XML Storage query. Only supports XEP-0048 bookmarks.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::PRIVATE, name = "query")]
pub struct Query {
    /// XEP-0048 bookmarks in a [`Storage`] element
    #[xml(child)]
    pub storage: Storage,
}

impl IqSetPayload for Query {}
impl IqGetPayload for Query {}
impl IqResultPayload for Query {}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[test]
    fn test_invalid_duplicate_child() {
        let elem: Element = "<query xmlns='jabber:iq:private'><storage xmlns='storage:bookmarks'/><storage xmlns='storage:bookmarks'/></query>"
            .parse()
            .unwrap();
        let error = Query::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Query element must not have more than one child in field 'storage'."
        );
    }
}
