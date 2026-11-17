// Copyright (c) 2020 Paul Fariello <paul@fariello.eu>
// Copyright (c) 2018 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::data_forms::DataForm;
use crate::iq::{IqGetPayload, IqResultPayload, IqSetPayload};
use crate::ns;
use crate::pubsub::{AffiliationAttribute, NodeName, Subscription};
use jid::Jid;

/// An affiliation element.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::PUBSUB_OWNER, name = "affiliation")]
pub struct Affiliation {
    /// The node this affiliation pertains to.
    #[xml(attribute)]
    jid: Jid,

    /// The affiliation you currently have on this node.
    #[xml(attribute)]
    affiliation: AffiliationAttribute,
}

/// A subscription element, describing the state of a subscription.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::PUBSUB_OWNER, name = "subscription")]
pub struct SubscriptionElem {
    /// The JID affected by this subscription.
    #[xml(attribute)]
    pub jid: Jid,

    /// The state of the subscription.
    #[xml(attribute)]
    pub subscription: Subscription,

    /// Subscription unique id.
    #[xml(attribute(default))]
    pub subid: Option<String>,
}

/// Represents an owner request to a PubSub service.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::PUBSUB_OWNER, name = "pubsub")]
pub struct Owner {
    /// The inner child of this request.
    #[xml(child)]
    pub payload: Payload,
}

// TODO: Differentiate each payload per type someday, to simplify our types.
impl IqGetPayload for Owner {}
impl IqSetPayload for Owner {}
impl IqResultPayload for Owner {}

/// Main payload used to communicate with a PubSub service.
///
/// `<pubsub xmlns="http://jabber.org/protocol/pubsub#owner"/>`
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::PUBSUB_OWNER, exhaustive)]
pub enum Payload {
    /// Manage the affiliations of a node.
    #[xml(name = "affiliations")]
    Affiliations {
        /// The node name this request pertains to.
        #[xml(attribute)]
        node: NodeName,

        /// The actual list of affiliation elements.
        #[xml(child(n = ..))]
        affiliations: Vec<Affiliation>,
    },

    /// Request to configure a node.
    #[xml(name = "configure")]
    Configure {
        /// The node to be configured.
        #[xml(attribute(default))]
        node: Option<NodeName>,

        /// The form to configure it.
        #[xml(child(default))]
        form: Option<DataForm>,
    },

    /// Request the default node configuration.
    #[xml(name = "default")]
    Default {
        /// The form to configure it.
        #[xml(child(default))]
        form: Option<DataForm>,
    },

    /// Delete a node.
    #[xml(name = "delete")]
    Delete {
        /// The node to be deleted.
        #[xml(attribute)]
        node: NodeName,

        /// Redirection to replace the deleted node.
        #[xml(extract(default, name = "redirect", fields(attribute(name = "uri", type_ = String))))]
        redirect_uri: Option<String>,
    },

    /// Purge all items from node.
    #[xml(name = "purge")]
    Purge {
        /// The node to be cleared.
        #[xml(attribute)]
        node: NodeName,
    },

    /// Request the current subscriptions to a node.
    #[xml(name = "subscriptions")]
    Subscriptions {
        /// The node to query.
        #[xml(attribute)]
        node: NodeName,

        /// The list of subscription elements returned.
        #[xml(child(n = ..))]
        subscriptions: Vec<SubscriptionElem>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_forms::{DataFormType, Field, FieldType};
    use core::str::FromStr;
    use jid::BareJid;
    use minidom::Element;

    #[test]
    fn affiliations() {
        let elem: Element = "<pubsub xmlns='http://jabber.org/protocol/pubsub#owner'><affiliations node='foo'><affiliation jid='hamlet@denmark.lit' affiliation='owner'/><affiliation jid='polonius@denmark.lit' affiliation='outcast'/></affiliations></pubsub>"
        .parse()
        .unwrap();
        let elem1 = elem.clone();

        let payload = Payload::Affiliations {
            node: NodeName(String::from("foo")),
            affiliations: vec![
                Affiliation {
                    jid: Jid::from(BareJid::from_str("hamlet@denmark.lit").unwrap()),
                    affiliation: AffiliationAttribute::Owner,
                },
                Affiliation {
                    jid: Jid::from(BareJid::from_str("polonius@denmark.lit").unwrap()),
                    affiliation: AffiliationAttribute::Outcast,
                },
            ],
        };
        let pubsub = Owner { payload };

        let elem2 = Element::from(pubsub);
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn configure() {
        let elem: Element = "<pubsub xmlns='http://jabber.org/protocol/pubsub#owner'><configure node='foo'><x xmlns='jabber:x:data' type='submit'><field var='FORM_TYPE' type='hidden'><value>http://jabber.org/protocol/pubsub#node_config</value></field><field var='pubsub#access_model' type='list-single'><value>whitelist</value></field></x></configure></pubsub>"
        .parse()
        .unwrap();
        let elem1 = elem.clone();

        let payload = Payload::Configure {
            node: Some(NodeName(String::from("foo"))),
            form: Some(DataForm::new(
                DataFormType::Submit,
                ns::PUBSUB_CONFIGURE,
                vec![Field::new("pubsub#access_model", FieldType::ListSingle)
                    .with_value("whitelist")],
            )),
        };
        let pubsub = Owner { payload };

        let elem2 = Element::from(pubsub);
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn test_serialize_configure() {
        let reference: Element = "<pubsub xmlns='http://jabber.org/protocol/pubsub#owner'><configure node='foo'><x xmlns='jabber:x:data' type='submit'/></configure></pubsub>"
        .parse()
        .unwrap();

        let elem: Element = "<x xmlns='jabber:x:data' type='submit'/>".parse().unwrap();

        let form = DataForm::try_from(elem).unwrap();

        let payload = Payload::Configure {
            node: Some(NodeName(String::from("foo"))),
            form: Some(form),
        };
        let configure = Owner { payload };
        let serialized: Element = configure.into();
        assert_eq!(serialized, reference);
    }

    #[test]
    fn default() {
        let elem: Element = "<pubsub xmlns='http://jabber.org/protocol/pubsub#owner'><default><x xmlns='jabber:x:data' type='submit'><field var='FORM_TYPE' type='hidden'><value>http://jabber.org/protocol/pubsub#node_config</value></field><field var='pubsub#access_model' type='list-single'><value>whitelist</value></field></x></default></pubsub>"
        .parse()
        .unwrap();
        let elem1 = elem.clone();

        let payload = Payload::Default {
            form: Some(DataForm::new(
                DataFormType::Submit,
                ns::PUBSUB_CONFIGURE,
                vec![Field::new("pubsub#access_model", FieldType::ListSingle)
                    .with_value("whitelist")],
            )),
        };
        let pubsub = Owner { payload };

        let elem2 = Element::from(pubsub);
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn delete() {
        let elem: Element = "<pubsub xmlns='http://jabber.org/protocol/pubsub#owner'><delete node='foo'><redirect uri='xmpp:hamlet@denmark.lit?;node=blog'/></delete></pubsub>"
        .parse()
        .unwrap();
        let elem1 = elem.clone();

        let payload = Payload::Delete {
            node: NodeName(String::from("foo")),
            redirect_uri: Some(String::from("xmpp:hamlet@denmark.lit?;node=blog")),
        };
        let pubsub = Owner { payload };

        let elem2 = Element::from(pubsub);
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn purge() {
        let elem: Element = "<pubsub xmlns='http://jabber.org/protocol/pubsub#owner'><purge node='foo'></purge></pubsub>"
        .parse()
        .unwrap();
        let elem1 = elem.clone();

        let payload = Payload::Purge {
            node: NodeName(String::from("foo")),
        };
        let pubsub = Owner { payload };

        let elem2 = Element::from(pubsub);
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn subscriptions() {
        let elem: Element = "<pubsub xmlns='http://jabber.org/protocol/pubsub#owner'><subscriptions node='foo'><subscription jid='hamlet@denmark.lit' subscription='subscribed'/><subscription jid='polonius@denmark.lit' subscription='unconfigured'/><subscription jid='bernardo@denmark.lit' subscription='subscribed' subid='123-abc'/><subscription jid='bernardo@denmark.lit' subscription='subscribed' subid='004-yyy'/></subscriptions></pubsub>"
        .parse()
        .unwrap();
        let elem1 = elem.clone();

        let payload = Payload::Subscriptions {
            node: NodeName(String::from("foo")),
            subscriptions: vec![
                SubscriptionElem {
                    jid: Jid::from(BareJid::from_str("hamlet@denmark.lit").unwrap()),
                    subscription: Subscription::Subscribed,
                    subid: None,
                },
                SubscriptionElem {
                    jid: Jid::from(BareJid::from_str("polonius@denmark.lit").unwrap()),
                    subscription: Subscription::Unconfigured,
                    subid: None,
                },
                SubscriptionElem {
                    jid: Jid::from(BareJid::from_str("bernardo@denmark.lit").unwrap()),
                    subscription: Subscription::Subscribed,
                    subid: Some(String::from("123-abc")),
                },
                SubscriptionElem {
                    jid: Jid::from(BareJid::from_str("bernardo@denmark.lit").unwrap()),
                    subscription: Subscription::Subscribed,
                    subid: Some(String::from("004-yyy")),
                },
            ],
        };
        let pubsub = Owner { payload };

        let elem2 = Element::from(pubsub);
        assert_eq!(elem1, elem2);
    }
}
