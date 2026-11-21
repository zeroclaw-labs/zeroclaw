// Copyright (c) 2018 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{
    error::{Error, FromElementError},
    AsXml, FromXml,
};

use crate::data_forms::DataForm;
use crate::iq::{IqGetPayload, IqResultPayload, IqSetPayload};
use crate::ns;
use crate::pubsub::{
    AffiliationAttribute, ItemId, NodeName, PubSubPayload, Subscription, SubscriptionId,
};
use jid::Jid;
use minidom::Element;

// TODO: a better solution would be to split this into a query and a result elements, like for
// XEP-0030.
/// A list of affiliations you have on a service, or on a node.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::PUBSUB, name = "affiliations")]
pub struct Affiliations {
    /// The optional node name this request pertains to.
    #[xml(attribute(default))]
    pub node: Option<NodeName>,

    /// The actual list of affiliation elements.
    #[xml(child(n = ..))]
    pub affiliations: Vec<Affiliation>,
}

/// An affiliation element.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::PUBSUB, name = "affiliation")]
pub struct Affiliation {
    /// The node this affiliation pertains to.
    #[xml(attribute)]
    pub node: NodeName,

    /// The affiliation you currently have on this node.
    #[xml(attribute)]
    pub affiliation: AffiliationAttribute,
}

/// Request to configure a new node.
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::PUBSUB, name = "configure")]
pub struct Configure {
    /// The form to configure it.
    #[xml(child(default))]
    pub form: Option<DataForm>,
}

/// Request to create a new node.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::PUBSUB, name = "create")]
pub struct Create {
    /// The node name to create, if `None` the service will generate one.
    #[xml(attribute(default))]
    pub node: Option<NodeName>,
}

/// Request for a default node configuration.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::PUBSUB, name = "default")]
pub struct Default {
    /// The node targeted by this request, otherwise the entire service.
    #[xml(attribute(default))]
    pub node: Option<NodeName>,
    // TODO: do we really want to support collection nodes?
    // #[xml(attribute(default, name = "type"))]
    // type_: Option<String>,
}

/// A request for a list of items.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::PUBSUB, name = "items")]
pub struct Items {
    // TODO: should be an xs:positiveInteger, that is, an unbounded int ≥ 1.
    /// Maximum number of items returned.
    #[xml(attribute(name = "max_items" /*sic!*/, default))]
    pub max_items: Option<u32>,

    /// The node queried by this request.
    #[xml(attribute)]
    pub node: NodeName,

    /// The subscription identifier related to this request.
    #[xml(attribute(default))]
    pub subid: Option<SubscriptionId>,

    /// The actual list of items returned.
    #[xml(child(n = ..))]
    pub items: Vec<Item>,
}

impl Items {
    /// Create a new items request.
    pub fn new(node: &str) -> Items {
        Items {
            node: NodeName(String::from(node)),
            max_items: None,
            subid: None,
            items: Vec::new(),
        }
    }
}

/// A requested item from a PubSub node.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::PUBSUB, name = "item")]
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

impl Item {
    /// Create a new item, accepting only payloads implementing `PubSubPayload`.
    pub fn new<P: PubSubPayload>(
        id: Option<ItemId>,
        publisher: Option<Jid>,
        payload: Option<P>,
    ) -> Item {
        Item {
            id,
            publisher,
            payload: payload.map(Into::into),
        }
    }
}

/// The options associated to a subscription request.
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::PUBSUB, name = "options")]
pub struct Options {
    /// The JID affected by this request.
    #[xml(attribute)]
    pub jid: Jid,

    /// The node affected by this request.
    #[xml(attribute(default))]
    pub node: Option<NodeName>,

    /// The subscription identifier affected by this request.
    #[xml(attribute(default))]
    pub subid: Option<SubscriptionId>,

    /// The form describing the subscription.
    #[xml(child(default))]
    pub form: Option<DataForm>,
}

/// Request to publish items to a node.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::PUBSUB, name = "publish")]
pub struct Publish {
    /// The target node for this operation.
    #[xml(attribute)]
    pub node: NodeName,

    /// The items you want to publish.
    #[xml(child(n = ..))]
    pub items: Vec<Item>,
}

/// The options associated to a publish request.
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::PUBSUB, name = "publish-options")]
pub struct PublishOptions {
    /// The form describing these options.
    #[xml(child(default))]
    pub form: Option<DataForm>,
}

/// A request to retract some items from a node.
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::PUBSUB, name = "retract")]
pub struct Retract {
    /// The node affected by this request.
    #[xml(attribute)]
    pub node: NodeName,

    /// Whether a retract request should notify subscribers or not.
    #[xml(attribute(default))]
    pub notify: bool,

    /// The items affected by this request.
    #[xml(child(n = ..))]
    pub items: Vec<Item>,
}

/// Indicate that the subscription can be configured.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::PUBSUB, name = "subscribe-options")]
pub struct SubscribeOptions {
    /// If `true`, the configuration is actually required.
    #[xml(flag)]
    required: bool,
}

/// A request to subscribe a JID to a node.
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::PUBSUB, name = "subscribe")]
pub struct Subscribe {
    /// The JID being subscribed.
    #[xml(attribute)]
    pub jid: Jid,

    /// The node to subscribe to.
    #[xml(attribute)]
    pub node: Option<NodeName>,
}

/// A request for current subscriptions.
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::PUBSUB, name = "subscriptions")]
pub struct Subscriptions {
    /// The node to query.
    #[xml(attribute(default))]
    pub node: Option<NodeName>,

    /// The list of subscription elements returned.
    #[xml(child(n = ..))]
    pub subscription: Vec<SubscriptionElem>,
}

/// A subscription element, describing the state of a subscription.
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::PUBSUB, name = "subscription")]
pub struct SubscriptionElem {
    /// The JID affected by this subscription.
    #[xml(attribute)]
    jid: Jid,

    /// The node affected by this subscription.
    #[xml(attribute(default))]
    node: Option<NodeName>,

    /// The subscription identifier for this subscription.
    #[xml(attribute(default))]
    subid: Option<SubscriptionId>,

    /// The state of the subscription.
    #[xml(attribute(default))]
    subscription: Option<Subscription>,

    /// The options related to this subscription.
    #[xml(child(default))]
    subscribe_options: Option<SubscribeOptions>,
}

/// An unsubscribe request.
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::PUBSUB, name = "unsubscribe")]
pub struct Unsubscribe {
    /// The JID affected by this request.
    #[xml(attribute)]
    jid: Jid,

    /// The node affected by this request.
    #[xml(attribute)]
    node: Option<NodeName>,

    /// The subscription identifier for this subscription.
    #[xml(attribute)]
    subid: Option<SubscriptionId>,
}

/// Main payload used to communicate with a PubSub service.
///
/// `<pubsub xmlns="http://jabber.org/protocol/pubsub"/>`
#[derive(Debug, Clone, PartialEq)]
pub enum PubSub {
    /// Request to create a new node, with optional suggested name and suggested configuration.
    Create {
        /// The create request.
        create: Create,

        /// The configure request for the new node.
        configure: Option<Configure>,
    },

    /// A subscribe request.
    Subscribe {
        /// The subscribe request.
        subscribe: Option<Subscribe>,

        /// The options related to this subscribe request.
        options: Option<Options>,
    },

    /// Request to publish items to a node, with optional options.
    Publish {
        /// The publish request.
        publish: Publish,

        /// The options related to this publish request.
        publish_options: Option<PublishOptions>,
    },

    /// A list of affiliations you have on a service, or on a node.
    Affiliations(Affiliations),

    /// Request for a default node configuration.
    Default(Default),

    /// A request for a list of items.
    Items(Items),

    /// A request to retract some items from a node.
    Retract(Retract),

    /// A request about a subscription.
    Subscription(SubscriptionElem),

    /// A request for current subscriptions.
    Subscriptions(Subscriptions),

    /// An unsubscribe request.
    Unsubscribe(Unsubscribe),
}

impl IqGetPayload for PubSub {}
impl IqSetPayload for PubSub {}
impl IqResultPayload for PubSub {}

impl TryFrom<Element> for PubSub {
    type Error = FromElementError;

    fn try_from(elem: Element) -> Result<PubSub, FromElementError> {
        check_self!(elem, "pubsub", PUBSUB);
        check_no_attributes!(elem, "pubsub");

        let mut payload = None;
        for child in elem.children() {
            if child.is("create", ns::PUBSUB) {
                if payload.is_some() {
                    return Err(
                        Error::Other("Payload is already defined in pubsub element.").into(),
                    );
                }
                let create = Create::try_from(child.clone())?;
                payload = Some(PubSub::Create {
                    create,
                    configure: None,
                });
            } else if child.is("subscribe", ns::PUBSUB) {
                if payload.is_some() {
                    return Err(
                        Error::Other("Payload is already defined in pubsub element.").into(),
                    );
                }
                let subscribe = Subscribe::try_from(child.clone())?;
                payload = Some(PubSub::Subscribe {
                    subscribe: Some(subscribe),
                    options: None,
                });
            } else if child.is("options", ns::PUBSUB) {
                if let Some(PubSub::Subscribe { subscribe, options }) = payload {
                    if options.is_some() {
                        return Err(
                            Error::Other("Options is already defined in pubsub element.").into(),
                        );
                    }
                    let options = Some(Options::try_from(child.clone())?);
                    payload = Some(PubSub::Subscribe { subscribe, options });
                } else if payload.is_none() {
                    let options = Options::try_from(child.clone())?;
                    payload = Some(PubSub::Subscribe {
                        subscribe: None,
                        options: Some(options),
                    });
                } else {
                    return Err(
                        Error::Other("Payload is already defined in pubsub element.").into(),
                    );
                }
            } else if child.is("configure", ns::PUBSUB) {
                if let Some(PubSub::Create { create, configure }) = payload {
                    if configure.is_some() {
                        return Err(Error::Other(
                            "Configure is already defined in pubsub element.",
                        )
                        .into());
                    }
                    let configure = Some(Configure::try_from(child.clone())?);
                    payload = Some(PubSub::Create { create, configure });
                } else {
                    return Err(
                        Error::Other("Payload is already defined in pubsub element.").into(),
                    );
                }
            } else if child.is("publish", ns::PUBSUB) {
                if payload.is_some() {
                    return Err(
                        Error::Other("Payload is already defined in pubsub element.").into(),
                    );
                }
                let publish = Publish::try_from(child.clone())?;
                payload = Some(PubSub::Publish {
                    publish,
                    publish_options: None,
                });
            } else if child.is("publish-options", ns::PUBSUB) {
                if let Some(PubSub::Publish {
                    publish,
                    publish_options,
                }) = payload
                {
                    if publish_options.is_some() {
                        return Err(Error::Other(
                            "Publish-options are already defined in pubsub element.",
                        )
                        .into());
                    }
                    let publish_options = Some(PublishOptions::try_from(child.clone())?);
                    payload = Some(PubSub::Publish {
                        publish,
                        publish_options,
                    });
                } else {
                    return Err(
                        Error::Other("Payload is already defined in pubsub element.").into(),
                    );
                }
            } else if child.is("affiliations", ns::PUBSUB) {
                if payload.is_some() {
                    return Err(
                        Error::Other("Payload is already defined in pubsub element.").into(),
                    );
                }
                let affiliations = Affiliations::try_from(child.clone())?;
                payload = Some(PubSub::Affiliations(affiliations));
            } else if child.is("default", ns::PUBSUB) {
                if payload.is_some() {
                    return Err(
                        Error::Other("Payload is already defined in pubsub element.").into(),
                    );
                }
                let default = Default::try_from(child.clone())?;
                payload = Some(PubSub::Default(default));
            } else if child.is("items", ns::PUBSUB) {
                if payload.is_some() {
                    return Err(
                        Error::Other("Payload is already defined in pubsub element.").into(),
                    );
                }
                let items = Items::try_from(child.clone())?;
                payload = Some(PubSub::Items(items));
            } else if child.is("retract", ns::PUBSUB) {
                if payload.is_some() {
                    return Err(
                        Error::Other("Payload is already defined in pubsub element.").into(),
                    );
                }
                let retract = Retract::try_from(child.clone())?;
                payload = Some(PubSub::Retract(retract));
            } else if child.is("subscription", ns::PUBSUB) {
                if payload.is_some() {
                    return Err(
                        Error::Other("Payload is already defined in pubsub element.").into(),
                    );
                }
                let subscription = SubscriptionElem::try_from(child.clone())?;
                payload = Some(PubSub::Subscription(subscription));
            } else if child.is("subscriptions", ns::PUBSUB) {
                if payload.is_some() {
                    return Err(
                        Error::Other("Payload is already defined in pubsub element.").into(),
                    );
                }
                let subscriptions = Subscriptions::try_from(child.clone())?;
                payload = Some(PubSub::Subscriptions(subscriptions));
            } else if child.is("unsubscribe", ns::PUBSUB) {
                if payload.is_some() {
                    return Err(
                        Error::Other("Payload is already defined in pubsub element.").into(),
                    );
                }
                let unsubscribe = Unsubscribe::try_from(child.clone())?;
                payload = Some(PubSub::Unsubscribe(unsubscribe));
            } else {
                return Err(Error::Other("Unknown child in pubsub element.").into());
            }
        }
        payload.ok_or(Error::Other("No payload in pubsub element.").into())
    }
}

impl From<PubSub> for Element {
    fn from(pubsub: PubSub) -> Element {
        Element::builder("pubsub", ns::PUBSUB)
            .append_all(match pubsub {
                PubSub::Create { create, configure } => {
                    let mut elems = vec![Element::from(create)];
                    if let Some(configure) = configure {
                        elems.push(Element::from(configure));
                    }
                    elems
                }
                PubSub::Subscribe { subscribe, options } => {
                    let mut elems = vec![];
                    if let Some(subscribe) = subscribe {
                        elems.push(Element::from(subscribe));
                    }
                    if let Some(options) = options {
                        elems.push(Element::from(options));
                    }
                    elems
                }
                PubSub::Publish {
                    publish,
                    publish_options,
                } => {
                    let mut elems = vec![Element::from(publish)];
                    if let Some(publish_options) = publish_options {
                        elems.push(Element::from(publish_options));
                    }
                    elems
                }
                PubSub::Affiliations(affiliations) => vec![Element::from(affiliations)],
                PubSub::Default(default) => vec![Element::from(default)],
                PubSub::Items(items) => vec![Element::from(items)],
                PubSub::Retract(retract) => vec![Element::from(retract)],
                PubSub::Subscription(subscription) => vec![Element::from(subscription)],
                PubSub::Subscriptions(subscriptions) => vec![Element::from(subscriptions)],
                PubSub::Unsubscribe(unsubscribe) => vec![Element::from(unsubscribe)],
            })
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_forms::{DataFormType, Field, FieldType};

    #[test]
    fn create() {
        let elem: Element = "<pubsub xmlns='http://jabber.org/protocol/pubsub'><create/></pubsub>"
            .parse()
            .unwrap();
        let elem1 = elem.clone();
        let pubsub = PubSub::try_from(elem).unwrap();
        match pubsub.clone() {
            PubSub::Create { create, configure } => {
                assert!(create.node.is_none());
                assert!(configure.is_none());
            }
            _ => panic!(),
        }

        let elem2 = Element::from(pubsub);
        assert_eq!(elem1, elem2);

        let elem: Element =
            "<pubsub xmlns='http://jabber.org/protocol/pubsub'><create node='coucou'/></pubsub>"
                .parse()
                .unwrap();
        let elem1 = elem.clone();
        let pubsub = PubSub::try_from(elem).unwrap();
        match pubsub.clone() {
            PubSub::Create { create, configure } => {
                assert_eq!(&create.node.unwrap().0, "coucou");
                assert!(configure.is_none());
            }
            _ => panic!(),
        }

        let elem2 = Element::from(pubsub);
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn create_and_configure_empty() {
        let elem: Element =
            "<pubsub xmlns='http://jabber.org/protocol/pubsub'><create/><configure/></pubsub>"
                .parse()
                .unwrap();
        let elem1 = elem.clone();
        let pubsub = PubSub::try_from(elem).unwrap();
        match pubsub.clone() {
            PubSub::Create { create, configure } => {
                assert!(create.node.is_none());
                assert!(configure.unwrap().form.is_none());
            }
            _ => panic!(),
        }

        let elem2 = Element::from(pubsub);
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn create_and_configure_simple() {
        // XXX: Do we want xmpp-parsers to always specify the field type in the output Element?
        let elem: Element = "<pubsub xmlns='http://jabber.org/protocol/pubsub'><create node='foo'/><configure><x xmlns='jabber:x:data' type='submit'><field var='FORM_TYPE' type='hidden'><value>http://jabber.org/protocol/pubsub#node_config</value></field><field var='pubsub#access_model' type='list-single'><value>whitelist</value></field></x></configure></pubsub>"
        .parse()
        .unwrap();
        let elem1 = elem.clone();

        let pubsub = PubSub::Create {
            create: Create {
                node: Some(NodeName(String::from("foo"))),
            },
            configure: Some(Configure {
                form: Some(DataForm::new(
                    DataFormType::Submit,
                    ns::PUBSUB_CONFIGURE,
                    vec![Field::new("pubsub#access_model", FieldType::ListSingle)
                        .with_value("whitelist")],
                )),
            }),
        };

        let elem2 = Element::from(pubsub);
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn publish() {
        let elem: Element =
            "<pubsub xmlns='http://jabber.org/protocol/pubsub'><publish node='coucou'/></pubsub>"
                .parse()
                .unwrap();
        let elem1 = elem.clone();
        let pubsub = PubSub::try_from(elem).unwrap();
        match pubsub.clone() {
            PubSub::Publish {
                publish,
                publish_options,
            } => {
                assert_eq!(&publish.node.0, "coucou");
                assert!(publish_options.is_none());
            }
            _ => panic!(),
        }

        let elem2 = Element::from(pubsub);
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn publish_with_publish_options() {
        let elem: Element = "<pubsub xmlns='http://jabber.org/protocol/pubsub'><publish node='coucou'/><publish-options/></pubsub>".parse().unwrap();
        let elem1 = elem.clone();
        let pubsub = PubSub::try_from(elem).unwrap();
        match pubsub.clone() {
            PubSub::Publish {
                publish,
                publish_options,
            } => {
                assert_eq!(&publish.node.0, "coucou");
                assert!(publish_options.unwrap().form.is_none());
            }
            _ => panic!(),
        }

        let elem2 = Element::from(pubsub);
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn invalid_empty_pubsub() {
        let elem: Element = "<pubsub xmlns='http://jabber.org/protocol/pubsub'/>"
            .parse()
            .unwrap();
        let error = PubSub::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "No payload in pubsub element.");
    }

    #[test]
    fn publish_option() {
        let elem: Element = "<publish-options xmlns='http://jabber.org/protocol/pubsub'><x xmlns='jabber:x:data' type='submit'><field var='FORM_TYPE' type='hidden'><value>http://jabber.org/protocol/pubsub#publish-options</value></field></x></publish-options>".parse().unwrap();
        let publish_options = PublishOptions::try_from(elem).unwrap();
        assert_eq!(
            publish_options.form.unwrap().form_type().unwrap(),
            "http://jabber.org/protocol/pubsub#publish-options"
        );
    }

    #[test]
    fn subscribe_options() {
        let elem1: Element = "<subscribe-options xmlns='http://jabber.org/protocol/pubsub'/>"
            .parse()
            .unwrap();
        let subscribe_options1 = SubscribeOptions::try_from(elem1).unwrap();
        assert_eq!(subscribe_options1.required, false);

        let elem2: Element = "<subscribe-options xmlns='http://jabber.org/protocol/pubsub'><required/></subscribe-options>".parse().unwrap();
        let subscribe_options2 = SubscribeOptions::try_from(elem2).unwrap();
        assert_eq!(subscribe_options2.required, true);
    }

    #[test]
    fn test_options_without_subscribe() {
        let elem: Element = "<pubsub xmlns='http://jabber.org/protocol/pubsub'><options xmlns='http://jabber.org/protocol/pubsub' jid='juliet@capulet.lit/balcony'><x xmlns='jabber:x:data' type='submit'/></options></pubsub>".parse().unwrap();
        let elem1 = elem.clone();
        let pubsub = PubSub::try_from(elem).unwrap();
        match pubsub.clone() {
            PubSub::Subscribe { subscribe, options } => {
                assert!(subscribe.is_none());
                assert!(options.is_some());
            }
            _ => panic!(),
        }

        let elem2 = Element::from(pubsub);
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn test_serialize_options() {
        let reference: Element = "<options xmlns='http://jabber.org/protocol/pubsub' jid='juliet@capulet.lit/balcony'><x xmlns='jabber:x:data' type='submit'/></options>"
        .parse()
        .unwrap();

        let elem: Element = "<x xmlns='jabber:x:data' type='submit'/>".parse().unwrap();

        let form = DataForm::try_from(elem).unwrap();

        let options = Options {
            jid: Jid::new("juliet@capulet.lit/balcony").unwrap(),
            node: None,
            subid: None,
            form: Some(form),
        };
        let serialized: Element = options.into();
        assert_eq!(serialized, reference);
    }

    #[test]
    fn test_serialize_publish_options() {
        let reference: Element = "<publish-options xmlns='http://jabber.org/protocol/pubsub'><x xmlns='jabber:x:data' type='submit'/></publish-options>"
        .parse()
        .unwrap();

        let elem: Element = "<x xmlns='jabber:x:data' type='submit'/>".parse().unwrap();

        let form = DataForm::try_from(elem).unwrap();

        let options = PublishOptions { form: Some(form) };
        let serialized: Element = options.into();
        assert_eq!(serialized, reference);
    }
}
