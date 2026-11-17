// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::data_forms::DataForm;
use crate::date::DateTime;
use crate::message::MessagePayload;
use crate::ns;
use crate::pubsub::{ItemId, NodeName, Subscription, SubscriptionId};
use jid::Jid;
use minidom::Element;

/// An event item from a PubSub node.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::PUBSUB_EVENT, name = "item")]
pub struct Item {
    /// The identifier for this item, unique per node.
    #[xml(attribute(default))]
    pub id: Option<ItemId>,

    /// The JID of the entity who published this item.
    #[xml(attribute(default))]
    pub publisher: Option<Jid>,

    /// The payload of this item, in an arbitrary namespace.
    #[xml(element(default))]
    pub payload: Option<Element>,
}

/// Represents an event happening to a PubSub node.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::PUBSUB_EVENT, name = "event")]
pub struct Event {
    /// The inner child of this event.
    #[xml(child)]
    pub payload: Payload,
}

impl MessagePayload for Event {}

/// Represents an event happening to a PubSub node.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::PUBSUB_EVENT, exhaustive)]
pub enum Payload {
    /*
    Collection {
    },
    */
    /// This node’s configuration changed.
    #[xml(name = "configuration")]
    Configuration {
        /// The node affected.
        #[xml(attribute)]
        node: NodeName,

        /// The new configuration of this node.
        #[xml(child(default))]
        form: Option<DataForm>,
    },

    /// This node has been deleted, with an optional redirect to another node.
    #[xml(name = "delete")]
    Delete {
        /// The node affected.
        #[xml(attribute)]
        node: NodeName,

        /// The xmpp: URI of another node replacing this one.
        #[xml(extract(default, fields(attribute(default, name = "uri"))))]
        redirect: Option<String>,
    },

    /// Some items have been published or retracted on this node.
    #[xml(name = "items")]
    Items {
        /// The node affected.
        #[xml(attribute)]
        node: NodeName,

        /// The list of published items.
        #[xml(child(n = ..))]
        published: Vec<Item>,

        /// The list of retracted items.
        #[xml(extract(n = .., name = "retract", fields(attribute(name = "id", type_ = ItemId))))]
        retracted: Vec<ItemId>,
    },

    /// All items of this node just got removed at once.
    #[xml(name = "purge")]
    Purge {
        /// The node affected.
        #[xml(attribute)]
        node: NodeName,
    },

    /// The user’s subscription to this node has changed.
    #[xml(name = "subscription")]
    Subscription {
        /// The node affected.
        #[xml(attribute)]
        node: NodeName,

        /// The time at which this subscription will expire.
        #[xml(attribute(default))]
        expiry: Option<DateTime>,

        /// The JID of the user affected.
        #[xml(attribute(default))]
        jid: Option<Jid>,

        /// An identifier for this subscription.
        #[xml(attribute(default))]
        subid: Option<SubscriptionId>,

        /// The state of this subscription.
        #[xml(attribute(default))]
        subscription: Option<Subscription>,
    },
}

impl Payload {
    /// Return the name of the node to which this event is related.
    pub fn node_name(&self) -> &NodeName {
        match self {
            Self::Purge { node, .. } => node,
            Self::Items { node, .. } => node,
            Self::Subscription { node, .. } => node,
            Self::Delete { node, .. } => node,
            Self::Configuration { node, .. } => node,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jid::BareJid;
    use xso::error::{Error, FromElementError};

    // TODO: Reenable this test once we support asserting that a Vec isn’t empty.
    #[test]
    #[ignore]
    fn missing_items() {
        let elem: Element =
            "<event xmlns='http://jabber.org/protocol/pubsub#event'><items node='coucou'/></event>"
                .parse()
                .unwrap();
        let error = Event::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Missing children in items element.");
    }

    #[test]
    fn test_simple_items() {
        let elem: Element = "<event xmlns='http://jabber.org/protocol/pubsub#event'><items node='coucou'><item id='test' publisher='test@coucou'/></items></event>".parse().unwrap();
        let event = Event::try_from(elem).unwrap();
        match event.payload {
            Payload::Items {
                node,
                published,
                retracted,
            } => {
                assert_eq!(node, NodeName(String::from("coucou")));
                assert_eq!(retracted.len(), 0);
                assert_eq!(published[0].id, Some(ItemId(String::from("test"))));
                assert_eq!(
                    published[0].publisher.clone().unwrap(),
                    BareJid::new("test@coucou").unwrap()
                );
                assert_eq!(published[0].payload, None);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_simple_pep() {
        let elem: Element = "<event xmlns='http://jabber.org/protocol/pubsub#event'><items node='something'><item><foreign xmlns='example:namespace'/></item></items></event>".parse().unwrap();
        let event = Event::try_from(elem).unwrap();
        match event.payload {
            Payload::Items {
                node,
                published,
                retracted,
            } => {
                assert_eq!(node, NodeName(String::from("something")));
                assert_eq!(retracted.len(), 0);
                assert_eq!(published[0].id, None);
                assert_eq!(published[0].publisher, None);
                match published[0].payload {
                    Some(ref elem) => assert!(elem.is("foreign", "example:namespace")),
                    _ => panic!(),
                }
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_simple_retract() {
        let elem: Element = "<event xmlns='http://jabber.org/protocol/pubsub#event'><items node='something'><retract id='coucou'/><retract id='test'/></items></event>".parse().unwrap();
        let event = Event::try_from(elem).unwrap();
        match event.payload {
            Payload::Items {
                node,
                published,
                retracted,
            } => {
                assert_eq!(node, NodeName(String::from("something")));
                assert_eq!(published.len(), 0);
                assert_eq!(retracted[0], ItemId(String::from("coucou")));
                assert_eq!(retracted[1], ItemId(String::from("test")));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_simple_delete() {
        let elem: Element = "<event xmlns='http://jabber.org/protocol/pubsub#event'><delete node='coucou'><redirect uri='hello'/></delete></event>".parse().unwrap();
        let event = Event::try_from(elem).unwrap();
        match event.payload {
            Payload::Delete { node, redirect } => {
                assert_eq!(node, NodeName(String::from("coucou")));
                assert_eq!(redirect, Some(String::from("hello")));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_simple_purge() {
        let elem: Element =
            "<event xmlns='http://jabber.org/protocol/pubsub#event'><purge node='coucou'/></event>"
                .parse()
                .unwrap();
        let event = Event::try_from(elem).unwrap();
        match event.payload {
            Payload::Purge { node } => {
                assert_eq!(node, NodeName(String::from("coucou")));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_simple_configure() {
        let elem: Element = "<event xmlns='http://jabber.org/protocol/pubsub#event'><configuration node='coucou'><x xmlns='jabber:x:data' type='result'><field var='FORM_TYPE' type='hidden'><value>http://jabber.org/protocol/pubsub#node_config</value></field></x></configuration></event>".parse().unwrap();
        let event = Event::try_from(elem).unwrap();
        match event.payload {
            Payload::Configuration { node, form: _ } => {
                assert_eq!(node, NodeName(String::from("coucou")));
                //assert_eq!(form.type_, Result_);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_invalid() {
        let elem: Element =
            "<event xmlns='http://jabber.org/protocol/pubsub#event'><coucou node='test'/></event>"
                .parse()
                .unwrap();
        let error = Event::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "This is not a Payload element.");
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_invalid_attribute() {
        let elem: Element = "<event xmlns='http://jabber.org/protocol/pubsub#event' coucou=''/>"
            .parse()
            .unwrap();
        let error = Event::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown attribute in Event element.");
    }

    #[test]
    fn test_ex221_subscription() {
        let elem: Element = "<event xmlns='http://jabber.org/protocol/pubsub#event'><subscription expiry='2006-02-28T23:59:59+00:00' jid='francisco@denmark.lit' node='princely_musings' subid='ba49252aaa4f5d320c24d3766f0bdcade78c78d3' subscription='subscribed'/></event>"
        .parse()
        .unwrap();
        let event = Event::try_from(elem.clone()).unwrap();
        match event.payload.clone() {
            Payload::Subscription {
                node,
                expiry,
                jid,
                subid,
                subscription,
            } => {
                assert_eq!(node, NodeName(String::from("princely_musings")));
                assert_eq!(
                    subid,
                    Some(SubscriptionId(String::from(
                        "ba49252aaa4f5d320c24d3766f0bdcade78c78d3"
                    )))
                );
                assert_eq!(subscription, Some(Subscription::Subscribed));
                assert_eq!(jid.unwrap(), BareJid::new("francisco@denmark.lit").unwrap());
                assert_eq!(expiry, Some("2006-02-28T23:59:59Z".parse().unwrap()));
            }
            _ => panic!(),
        }

        let elem2: Element = event.into();
        assert_eq!(elem, elem2);
    }
}
