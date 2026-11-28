// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::iq::{IqGetPayload, IqResultPayload, IqSetPayload};
use crate::ns;
use jid::Jid;

/// The element requesting the blocklist, the result iq will contain a
/// [BlocklistResult].
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::BLOCKING, name = "blocklist")]
pub struct BlocklistRequest;

impl IqGetPayload for BlocklistRequest {}

/// The element containing the current blocklist, as a reply from
/// [BlocklistRequest].
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::BLOCKING, name = "blocklist")]
pub struct BlocklistResult {
    /// List of JIDs affected by this command.
    #[xml(extract(n = .., name = "item", fields(attribute(name = "jid", type_ = Jid))))]
    pub items: Vec<Jid>,
}

impl IqResultPayload for BlocklistResult {}

/// A query to block one or more JIDs.
// TODO: Prevent zero elements from being allowed.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::BLOCKING, name = "block")]
pub struct Block {
    /// List of JIDs affected by this command.
    #[xml(extract(n = .., name = "item", fields(attribute(name = "jid", type_ = Jid))))]
    pub items: Vec<Jid>,
}

impl IqSetPayload for Block {}

/// A query to unblock one or more JIDs, or all of them.
///
/// Warning: not putting any JID there means clearing out the blocklist.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::BLOCKING, name = "unblock")]
pub struct Unblock {
    /// List of JIDs affected by this command.
    #[xml(extract(n = .., name = "item", fields(attribute(name = "jid", type_ = Jid))))]
    pub items: Vec<Jid>,
}

impl IqSetPayload for Unblock {}

/// The application-specific error condition when a message is blocked.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::BLOCKING_ERRORS, name = "blocked")]
pub struct Blocked;

#[cfg(test)]
mod tests {
    #[cfg(feature = "pedantic")]
    use xso::error::{Error, FromElementError};

    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(BlocklistRequest, 0);
        assert_size!(BlocklistResult, 12);
        assert_size!(Block, 12);
        assert_size!(Unblock, 12);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(BlocklistRequest, 0);
        assert_size!(BlocklistResult, 24);
        assert_size!(Block, 24);
        assert_size!(Unblock, 24);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<blocklist xmlns='urn:xmpp:blocking'/>".parse().unwrap();
        let request_elem = elem.clone();
        BlocklistRequest::try_from(request_elem).unwrap();

        let result_elem = elem.clone();
        let result = BlocklistResult::try_from(result_elem).unwrap();
        assert!(result.items.is_empty());

        let elem: Element = "<block xmlns='urn:xmpp:blocking'/>".parse().unwrap();
        let block = Block::try_from(elem).unwrap();
        assert!(block.items.is_empty());

        let elem: Element = "<unblock xmlns='urn:xmpp:blocking'/>".parse().unwrap();
        let unblock = Unblock::try_from(elem).unwrap();
        assert!(unblock.items.is_empty());
    }

    #[test]
    fn test_items() {
        let elem: Element = "<blocklist xmlns='urn:xmpp:blocking'><item jid='coucou@coucou'/><item jid='domain'/></blocklist>".parse().unwrap();
        let two_items = vec![
            Jid::new("coucou@coucou").unwrap(),
            Jid::new("domain").unwrap(),
        ];

        let result_elem = elem.clone();
        let result = BlocklistResult::try_from(result_elem).unwrap();
        assert_eq!(result.items, two_items);

        let elem: Element = "<block xmlns='urn:xmpp:blocking'><item jid='coucou@coucou'/><item jid='domain'/></block>".parse().unwrap();
        let block = Block::try_from(elem).unwrap();
        assert_eq!(block.items, two_items);

        let elem: Element = "<unblock xmlns='urn:xmpp:blocking'><item jid='coucou@coucou'/><item jid='domain'/></unblock>".parse().unwrap();
        let unblock = Unblock::try_from(elem).unwrap();
        assert_eq!(unblock.items, two_items);
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_invalid() {
        let elem: Element = "<blocklist xmlns='urn:xmpp:blocking' coucou=''/>"
            .parse()
            .unwrap();
        let request_elem = elem.clone();
        let error = BlocklistRequest::try_from(request_elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown attribute in BlocklistRequest element.");

        let result_elem = elem.clone();
        let error = BlocklistResult::try_from(result_elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown attribute in BlocklistResult element.");

        let elem: Element = "<block xmlns='urn:xmpp:blocking' coucou=''/>"
            .parse()
            .unwrap();
        let error = Block::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown attribute in Block element.");

        let elem: Element = "<unblock xmlns='urn:xmpp:blocking' coucou=''/>"
            .parse()
            .unwrap();
        let error = Unblock::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown attribute in Unblock element.");
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_non_empty_blocklist_request() {
        let elem: Element = "<blocklist xmlns='urn:xmpp:blocking'><item jid='coucou@coucou'/><item jid='domain'/></blocklist>".parse().unwrap();
        let error = BlocklistRequest::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in BlocklistRequest element.");
    }
}
