// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::data_forms::DataForm;
use crate::iq::{IqGetPayload, IqResultPayload};
use crate::ns;
use crate::rsm::{SetQuery, SetResult};
use alloc::collections::BTreeSet;
use jid::Jid;

/// Structure representing a `<query xmlns='http://jabber.org/protocol/disco#info'/>` element.
///
/// It should only be used in an `<iq type='get'/>`, as it can only represent
/// the request, and not a result.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::DISCO_INFO, name = "query")]
pub struct DiscoInfoQuery {
    /// Node on which we are doing the discovery.
    #[xml(attribute(default))]
    pub node: Option<String>,
}

impl IqGetPayload for DiscoInfoQuery {}

/// Structure representing an `<identity xmlns='http://jabber.org/protocol/disco#info'/>` element.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq, Eq, Hash)]
#[xml(namespace = ns::DISCO_INFO, name = "identity")]
pub struct Identity {
    /// Category of this identity.
    // TODO: use an enum here.
    #[xml(attribute)]
    pub category: String,

    /// Type of this identity.
    // TODO: use an enum here.
    #[xml(attribute = "type")]
    pub type_: String,

    /// Lang of the name of this identity.
    #[xml(lang(default))]
    pub lang: Option<String>,

    /// Name of this identity.
    #[xml(attribute(default))]
    pub name: Option<String>,
}

impl Identity {
    /// Create a new `<identity/>`.
    pub fn new<C, T, L, N>(category: C, type_: T, lang: L, name: N) -> Identity
    where
        C: Into<String>,
        T: Into<String>,
        L: Into<String>,
        N: Into<String>,
    {
        Identity {
            category: category.into(),
            type_: type_.into(),
            lang: Some(lang.into()),
            name: Some(name.into()),
        }
    }

    /// Create a new `<identity/>` without a name.
    pub fn new_anonymous<C, T, L, N>(category: C, type_: T) -> Identity
    where
        C: Into<String>,
        T: Into<String>,
    {
        Identity {
            category: category.into(),
            type_: type_.into(),
            lang: None,
            name: None,
        }
    }
}

/// Structure representing a `<query xmlns='http://jabber.org/protocol/disco#info'/>` element.
///
/// It should only be used in an `<iq type='result'/>`, as it can only
/// represent the result, and not a request.
#[derive(FromXml, AsXml, Debug, Clone)]
#[xml(namespace = ns::DISCO_INFO, name = "query")]
pub struct DiscoInfoResult {
    /// Node on which we have done this discovery.
    #[xml(attribute(default))]
    pub node: Option<String>,

    /// List of identities exposed by this entity.
    #[xml(child(n = ..))]
    pub identities: Vec<Identity>,

    /// List of features supported by this entity.
    #[xml(extract(n = .., name = "feature", fields(attribute(name = "var", type_ = String))))]
    pub features: BTreeSet<String>,

    /// List of extensions reported by this entity.
    #[xml(child(n = ..))]
    pub extensions: Vec<DataForm>,
}

impl IqResultPayload for DiscoInfoResult {}

/// Structure representing a `<query xmlns='http://jabber.org/protocol/disco#items'/>` element.
///
/// It should only be used in an `<iq type='get'/>`, as it can only represent
/// the request, and not a result.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::DISCO_ITEMS, name = "query")]
pub struct DiscoItemsQuery {
    /// Node on which we are doing the discovery.
    #[xml(attribute(default))]
    pub node: Option<String>,

    /// Optional paging via Result Set Management
    #[xml(child(default))]
    pub rsm: Option<SetQuery>,
}

impl IqGetPayload for DiscoItemsQuery {}

/// Structure representing an `<item xmlns='http://jabber.org/protocol/disco#items'/>` element.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::DISCO_ITEMS, name = "item")]
pub struct Item {
    /// JID of the entity pointed by this item.
    #[xml(attribute)]
    pub jid: Jid,

    /// Node of the entity pointed by this item.
    #[xml(attribute(default))]
    pub node: Option<String>,

    /// Name of the entity pointed by this item.
    #[xml(attribute(default))]
    pub name: Option<String>,
}

/// Structure representing a `<query
/// xmlns='http://jabber.org/protocol/disco#items'/>` element.
///
/// It should only be used in an `<iq type='result'/>`, as it can only
/// represent the result, and not a request.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::DISCO_ITEMS, name = "query")]
pub struct DiscoItemsResult {
    /// Node on which we have done this discovery.
    #[xml(attribute(default))]
    pub node: Option<String>,

    /// List of items pointed by this entity.
    #[xml(child(n = ..))]
    pub items: Vec<Item>,

    /// Optional paging via Result Set Management
    #[xml(child(default))]
    pub rsm: Option<SetResult>,
}

impl IqResultPayload for DiscoItemsResult {}

#[cfg(test)]
mod tests {
    use super::*;
    use jid::BareJid;
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Identity, 48);
        assert_size!(DiscoInfoQuery, 12);
        assert_size!(DiscoInfoResult, 48);

        assert_size!(Item, 40);
        assert_size!(DiscoItemsQuery, 52);
        assert_size!(DiscoItemsResult, 64);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Identity, 96);
        assert_size!(DiscoInfoQuery, 24);
        assert_size!(DiscoInfoResult, 96);

        assert_size!(Item, 80);
        assert_size!(DiscoItemsQuery, 104);
        assert_size!(DiscoItemsResult, 128);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<query xmlns='http://jabber.org/protocol/disco#info'><identity category='client' type='pc'/><feature var='http://jabber.org/protocol/disco#info'/></query>".parse().unwrap();
        let query = DiscoInfoResult::try_from(elem).unwrap();
        assert!(query.node.is_none());
        assert_eq!(query.identities.len(), 1);
        assert_eq!(query.features.len(), 1);
        assert!(query.extensions.is_empty());
    }

    #[test]
    fn test_identity_after_feature() {
        let elem: Element = "<query xmlns='http://jabber.org/protocol/disco#info'><feature var='http://jabber.org/protocol/disco#info'/><identity category='client' type='pc'/></query>".parse().unwrap();
        let query = DiscoInfoResult::try_from(elem).unwrap();
        assert_eq!(query.identities.len(), 1);
        assert_eq!(query.features.len(), 1);
        assert!(query.extensions.is_empty());
    }

    #[test]
    fn test_feature_after_dataform() {
        let elem: Element = "<query xmlns='http://jabber.org/protocol/disco#info'><identity category='client' type='pc'/><x xmlns='jabber:x:data' type='result'><field var='FORM_TYPE' type='hidden'><value>coucou</value></field></x><feature var='http://jabber.org/protocol/disco#info'/></query>".parse().unwrap();
        let query = DiscoInfoResult::try_from(elem).unwrap();
        assert_eq!(query.identities.len(), 1);
        assert_eq!(query.features.len(), 1);
        assert_eq!(query.extensions.len(), 1);
    }

    #[test]
    fn test_extension() {
        let elem: Element = "<query xmlns='http://jabber.org/protocol/disco#info'><identity category='client' type='pc'/><feature var='http://jabber.org/protocol/disco#info'/><x xmlns='jabber:x:data' type='result'><field var='FORM_TYPE' type='hidden'><value>example</value></field></x></query>".parse().unwrap();
        let elem1 = elem.clone();
        let query = DiscoInfoResult::try_from(elem).unwrap();
        assert!(query.node.is_none());
        assert_eq!(query.identities.len(), 1);
        assert_eq!(query.features.len(), 1);
        assert_eq!(query.extensions.len(), 1);
        assert_eq!(query.extensions[0].form_type(), Some("example"));

        let elem2 = query.into();
        assert_eq!(elem1, elem2);
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_invalid() {
        let elem: Element =
            "<query xmlns='http://jabber.org/protocol/disco#info'><coucou/></query>"
                .parse()
                .unwrap();
        let error = DiscoInfoResult::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in DiscoInfoResult element.");
    }

    #[test]
    fn test_invalid_identity() {
        let elem: Element =
            "<query xmlns='http://jabber.org/protocol/disco#info'><identity/></query>"
                .parse()
                .unwrap();
        let error = DiscoInfoResult::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'category' on Identity element missing."
        );

        let elem: Element =
            "<query xmlns='http://jabber.org/protocol/disco#info'><identity type='coucou'/></query>"
                .parse()
                .unwrap();
        let error = DiscoInfoResult::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'category' on Identity element missing."
        );

        let elem: Element = "<query xmlns='http://jabber.org/protocol/disco#info'><identity category='coucou'/></query>".parse().unwrap();
        let error = DiscoInfoResult::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'type_' on Identity element missing."
        );
    }

    #[test]
    fn test_invalid_feature() {
        let elem: Element =
            "<query xmlns='http://jabber.org/protocol/disco#info'><feature/></query>"
                .parse()
                .unwrap();
        let error = DiscoInfoResult::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        // TODO: Make xso generate a better error message, with s/unnamed field 0/'var'/ for
        // instance.
        assert_eq!(
            message,
            "Required attribute unnamed field 0 on extraction for field 'features' in DiscoInfoResult element missing."
        );
    }

    // TODO: We stopped validating that there are enough identities and features in this result,
    // this is a limitation of xso which accepts n = .. only, and not n = 1.., so let’s wait until
    // xso implements this to reenable this test.
    #[test]
    #[ignore]
    fn test_invalid_result() {
        let elem: Element = "<query xmlns='http://jabber.org/protocol/disco#info'/>"
            .parse()
            .unwrap();
        let error = DiscoInfoResult::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "There must be at least one identity in disco#info."
        );

        let elem: Element = "<query xmlns='http://jabber.org/protocol/disco#info'><identity category='client' type='pc'/></query>".parse().unwrap();
        let error = DiscoInfoResult::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "There must be at least one feature in disco#info.");
    }

    #[test]
    fn test_simple_items() {
        let elem: Element = "<query xmlns='http://jabber.org/protocol/disco#items'/>"
            .parse()
            .unwrap();
        let query = DiscoItemsQuery::try_from(elem).unwrap();
        assert!(query.node.is_none());

        let elem: Element = "<query xmlns='http://jabber.org/protocol/disco#items' node='coucou'/>"
            .parse()
            .unwrap();
        let query = DiscoItemsQuery::try_from(elem).unwrap();
        assert_eq!(query.node, Some(String::from("coucou")));
    }

    #[test]
    fn test_simple_items_result() {
        let elem: Element = "<query xmlns='http://jabber.org/protocol/disco#items'/>"
            .parse()
            .unwrap();
        let query = DiscoItemsResult::try_from(elem).unwrap();
        assert!(query.node.is_none());
        assert!(query.items.is_empty());

        let elem: Element = "<query xmlns='http://jabber.org/protocol/disco#items' node='coucou'/>"
            .parse()
            .unwrap();
        let query = DiscoItemsResult::try_from(elem).unwrap();
        assert_eq!(query.node, Some(String::from("coucou")));
        assert!(query.items.is_empty());
    }

    #[test]
    fn test_answers_items_result() {
        let elem: Element = "<query xmlns='http://jabber.org/protocol/disco#items'><item jid='component'/><item jid='component2' node='test' name='A component'/></query>".parse().unwrap();
        let query = DiscoItemsResult::try_from(elem).unwrap();
        let elem2 = Element::from(query);
        let query = DiscoItemsResult::try_from(elem2).unwrap();
        assert_eq!(query.items.len(), 2);
        assert_eq!(query.items[0].jid, BareJid::new("component").unwrap());
        assert_eq!(query.items[0].node, None);
        assert_eq!(query.items[0].name, None);
        assert_eq!(query.items[1].jid, BareJid::new("component2").unwrap());
        assert_eq!(query.items[1].node, Some(String::from("test")));
        assert_eq!(query.items[1].name, Some(String::from("A component")));
    }

    // WORKAROUND FOR PROSODY BUG 1664, DO NOT REMOVE BEFORE 2028-12-17 (5 YEARS AFTER FIX)
    // https://issues.prosody.im/1664
    // See also:
    // https://gitlab.com/xmpp-rs/xmpp-rs/-/issues/128
    // https://gitlab.com/xmpp-rs/xmpp-rs/-/merge_requests/302
    #[test]
    fn test_missing_disco_info_feature_workaround() {
        let elem: Element = "<query xmlns='http://jabber.org/protocol/disco#info'><identity category='client' type='pc'/><feature var='http://jabber.org/protocol/muc#user'/></query>".parse().unwrap();
        let query = DiscoInfoResult::try_from(elem).unwrap();
        assert_eq!(query.identities.len(), 1);
        assert_eq!(query.features.len(), 1);
        assert!(query.extensions.is_empty());
    }
}
