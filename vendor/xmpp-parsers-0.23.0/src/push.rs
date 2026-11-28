// Copyright (c) 2025 saarko <saarko@tutanota.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use crate::data_forms::DataForm;
use crate::iq::IqSetPayload;
use crate::jid::BareJid;
use crate::ns;
use crate::pubsub::PubSubPayload;
use minidom::Element;
use xso::{AsXml, FromXml};

/// An enable element for push notifications
#[derive(Debug, Clone, FromXml, AsXml)]
#[xml(namespace = ns::PUSH, name = "enable")]
pub struct Enable {
    /// The 'jid' attribute of the XMPP Push Service being enabled.
    #[xml(attribute)]
    pub jid: BareJid,

    /// The 'node' attribute which is set to the provisioned node specified by the App Server.
    #[xml(attribute(default))]
    pub node: Option<String>,

    /// Optional additional information to be provided with each published notification, such as authentication credentials.
    #[xml(child(default))]
    pub form: Option<DataForm>,
}

impl IqSetPayload for Enable {}

/// A disable element for push notifications
#[derive(Debug, Clone, FromXml, AsXml)]
#[xml(namespace = ns::PUSH, name = "disable")]
pub struct Disable {
    /// The 'jid' attribute of the XMPP Push Service being disabled.
    #[xml(attribute)]
    pub jid: BareJid,

    /// The 'node' attribute which was set to the provisioned node specified by the App Server.
    #[xml(attribute(default))]
    pub node: Option<String>,
}

impl IqSetPayload for Disable {}

/// A notification element containing push notification data
#[derive(Debug, Clone, FromXml, AsXml)]
#[xml(namespace = ns::PUSH, name = "notification")]
pub struct Notification {
    /// The 'form' to provide summarized information such as the number of unread messages or number of pending subscription requests.
    #[xml(child(default))]
    pub form: Option<DataForm>,

    /// Child elements for the notification that are not part of the summary data form.
    #[xml(element(n = ..))]
    pub payloads: Vec<Element>,
}

impl PubSubPayload for Notification {}

#[cfg(test)]
mod tests {
    use super::*;
    use core::str::FromStr;

    #[test]
    fn test_enable() {
        let test_enable = r#"<enable xmlns='urn:xmpp:push:0'
            jid='push-5.client.example'
            node='yxs32uqsflafdk3iuqo'>
            <x xmlns='jabber:x:data' type='submit'>
                <field var='FORM_TYPE'><value>http://jabber.org/protocol/pubsub#publish-options</value></field>
                <field var='secret'><value>eruio234vzxc2kla-91</value></field>
            </x>
        </enable>"#;

        let elem = Element::from_str(test_enable).expect("Failed to parse XML");
        let enable = Enable::try_from(elem).expect("Failed to parse enable");

        assert_eq!(
            enable.jid,
            BareJid::from_str("push-5.client.example").unwrap()
        );
        assert_eq!(enable.node.unwrap(), "yxs32uqsflafdk3iuqo");
        assert!(enable.form.is_some());
    }

    #[test]
    fn test_enable_only_with_required_fields() {
        let test_enable = r#"<enable xmlns='urn:xmpp:push:0'
            jid='push-5.client.example' />"#;

        let elem = Element::from_str(test_enable).expect("Failed to parse XML");
        let enable = Enable::try_from(elem).expect("Failed to parse enable");

        assert_eq!(
            enable.jid,
            BareJid::from_str("push-5.client.example").unwrap()
        );
        assert!(enable.node.is_none());
        assert!(enable.form.is_none());
    }

    #[test]
    fn test_disable() {
        let test_disable = r#"<disable xmlns='urn:xmpp:push:0'
            jid='push-5.client.example'
            node='yxs32uqsflafdk3iuqo' />"#;

        let elem = Element::from_str(test_disable).expect("Failed to parse XML");
        let disable = Disable::try_from(elem).expect("Failed to parse disable");

        assert_eq!(
            disable.jid,
            BareJid::from_str("push-5.client.example").unwrap()
        );
        assert_eq!(disable.node.unwrap(), "yxs32uqsflafdk3iuqo");
    }

    #[test]
    fn test_disable_only_with_required_fields() {
        let test_disable = r#"<disable xmlns='urn:xmpp:push:0'
            jid='push-5.client.example' />"#;

        let elem = Element::from_str(test_disable).expect("Failed to parse XML");
        let disable = Disable::try_from(elem).expect("Failed to parse disable");

        assert_eq!(
            disable.jid,
            BareJid::from_str("push-5.client.example").unwrap()
        );
        assert!(disable.node.is_none());
    }

    #[test]
    fn test_notification() {
        let test_notification = r#"<notification xmlns='urn:xmpp:push:0'>
            <x xmlns='jabber:x:data' type='result'>
                <field var='FORM_TYPE'><value>urn:xmpp:push:summary</value></field>
                <field var='message-count'><value>1</value></field>
                <field var='last-message-sender'><value>juliet@capulet.example/balcony</value></field>
                <field var='last-message-body'><value>Wherefore art thou, Romeo?</value></field>
            </x>
            <additional xmlns='http://example.com/custom'>Additional custom elements</additional>
        </notification>"#;

        let elem = Element::from_str(test_notification).expect("Failed to parse XML");
        let notification = Notification::try_from(elem).expect("Failed to parse notification");

        assert!(notification.form.is_some());
        assert_eq!(notification.payloads.len(), 1);
    }

    #[test]
    fn test_notification_only_with_required_fields() {
        let test_notification = r#"<notification xmlns='urn:xmpp:push:0'>
            <additional xmlns='http://example.com/custom'>Additional custom elements</additional>
        </notification>"#;

        let elem = Element::from_str(test_notification).expect("Failed to parse XML");
        let notification = Notification::try_from(elem).expect("Failed to parse notification");

        assert!(notification.form.is_none());
        assert_eq!(notification.payloads.len(), 1);
    }
}
