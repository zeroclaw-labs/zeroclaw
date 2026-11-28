// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

/// The `http://jabber.org/protocol/pubsub#event` protocol.
pub mod event;

/// The `http://jabber.org/protocol/pubsub#owner` protocol.
pub mod owner;

/// The `http://jabber.org/protocol/pubsub` protocol.
#[allow(clippy::module_inception)]
pub mod pubsub;

pub use self::event::Event;
pub use self::owner::Owner;
pub use self::pubsub::PubSub;

use minidom::Element;

generate_id!(
    /// The name of a PubSub node, used to identify it on a JID.
    NodeName
);

generate_id!(
    /// The identifier of an item, which is unique per node.
    ItemId
);

generate_id!(
    /// The identifier of a subscription to a PubSub node.
    SubscriptionId
);

generate_attribute!(
    /// The state of a subscription to a node.
    Subscription, "subscription", {
        /// The user is not subscribed to this node.
        None => "none",

        /// The user’s subscription to this node is still pending.
        Pending => "pending",

        /// The user is subscribed to this node.
        Subscribed => "subscribed",

        /// The user’s subscription to this node will only be valid once
        /// configured.
        Unconfigured => "unconfigured",
    }, Default = None
);

generate_attribute!(
    /// A list of possible affiliations to a node.
    AffiliationAttribute, "affiliation", {
        /// You are a member of this node, you can subscribe and retrieve items.
        Member => "member",

        /// You don’t have a specific affiliation with this node, you can only subscribe to it.
        None => "none",

        /// You are banned from this node.
        Outcast => "outcast",

        /// You are an owner of this node, and can do anything with it.
        Owner => "owner",

        /// You are a publisher on this node, you can publish and retract items to it.
        Publisher => "publisher",

        /// You can publish and retract items on this node, but not subscribe or retrieve items.
        PublishOnly => "publish-only",
    }
);

/// This trait should be implemented on any element which can be included as a PubSub payload.
pub trait PubSubPayload: TryFrom<Element> + Into<Element> {}
