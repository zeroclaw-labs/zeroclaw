// Copyright (c) 2021 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::data_forms::DataForm;
use crate::date::DateTime;
use crate::iq::{IqGetPayload, IqResultPayload, IqSetPayload};
use crate::ns;

generate_attribute!(
    /// When sending a push update, the action value indicates if the service is being added or
    /// deleted from the set of known services (or simply being modified).
    Action, "action", {
        /// The service is being added from the set of known services.
        Add => "add",

        /// The service is being removed from the set of known services.
        Remove => "remove",

        /// The service is being modified.
        Modify => "modify",
    }, Default = Add
);

generate_attribute!(
    /// The underlying transport protocol to be used when communicating with the service.
    Transport, "transport", {
        /// Use TCP as a transport protocol.
        Tcp => "tcp",

        /// Use UDP as a transport protocol.
        Udp => "udp",
    }
);

generate_attribute!(
    /// The service type as registered with the XMPP Registrar.
    Type, "type", {
        /// A server that provides Session Traversal Utilities for NAT (STUN).
        Stun => "stun",

        /// A server that provides Traversal Using Relays around NAT (TURN).
        Turn => "turn",
    }
);

/// Structure representing a `<service xmlns='urn:xmpp:extdisco:2'/>` element.
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::EXT_DISCO, name = "service")]
pub struct Service {
    /// When sending a push update, the action value indicates if the service is being added or
    /// deleted from the set of known services (or simply being modified).
    #[xml(attribute(default))]
    action: Action,

    /// A timestamp indicating when the provided username and password credentials will expire.
    #[xml(attribute(default))]
    expires: Option<DateTime>,

    /// Either a fully qualified domain name (FQDN) or an IP address (IPv4 or IPv6).
    #[xml(attribute)]
    host: String,

    /// A friendly (human-readable) name or label for the service.
    #[xml(attribute(default))]
    name: Option<String>,

    /// A service- or server-generated password for use at the service.
    #[xml(attribute(default))]
    password: Option<String>,

    /// The communications port to be used at the host.
    #[xml(attribute(default))]
    port: Option<u16>,

    /// A boolean value indicating that username and password credentials are required and will
    /// need to be requested if not already provided.
    #[xml(attribute(default))]
    restricted: bool,

    /// The underlying transport protocol to be used when communicating with the service (typically
    /// either TCP or UDP).
    #[xml(attribute(default))]
    transport: Option<Transport>,

    /// The service type as registered with the XMPP Registrar.
    #[xml(attribute = "type")]
    type_: Type,

    /// A service- or server-generated username for use at the service.
    #[xml(attribute(default))]
    username: Option<String>,

    /// Extended information
    #[xml(child(n = ..))]
    ext_info: Vec<DataForm>,
}

impl IqGetPayload for Service {}

/// Structure representing a `<services xmlns='urn:xmpp:extdisco:2'/>` element.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::EXT_DISCO, name = "services")]
pub struct ServicesQuery {
    /// The type of service to filter for.
    #[xml(attribute(default, name = "type"))]
    pub type_: Option<Type>,
}

impl IqGetPayload for ServicesQuery {}

/// Structure representing a `<services xmlns='urn:xmpp:extdisco:2'/>` element.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::EXT_DISCO, name = "services")]
pub struct ServicesResult {
    /// The service type which was requested.
    #[xml(attribute(name = "type", default))]
    pub type_: Option<Type>,

    /// List of services.
    #[xml(child(n = ..))]
    pub services: Vec<Service>,
}

impl IqResultPayload for ServicesResult {}
impl IqSetPayload for ServicesResult {}

/// Structure representing a `<credentials xmlns='urn:xmpp:extdisco:2'/>` element.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::EXT_DISCO, name = "credentials")]
pub struct Credentials {
    /// List of services.
    #[xml(child(n = ..))]
    pub services: Vec<Service>,
}

impl IqGetPayload for Credentials {}
impl IqResultPayload for Credentials {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ns;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Action, 1);
        assert_size!(Transport, 1);
        assert_size!(Type, 1);
        assert_size!(Service, 84);
        assert_size!(ServicesQuery, 1);
        assert_size!(ServicesResult, 16);
        assert_size!(Credentials, 12);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Action, 1);
        assert_size!(Transport, 1);
        assert_size!(Type, 1);
        assert_size!(Service, 144);
        assert_size!(ServicesQuery, 1);
        assert_size!(ServicesResult, 32);
        assert_size!(Credentials, 24);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<service xmlns='urn:xmpp:extdisco:2' host='stun.shakespeare.lit' port='9998' transport='udp' type='stun'/>".parse().unwrap();
        let service = Service::try_from(elem).unwrap();
        assert_eq!(service.action, Action::Add);
        assert!(service.expires.is_none());
        assert_eq!(service.host, "stun.shakespeare.lit");
        assert!(service.name.is_none());
        assert!(service.password.is_none());
        assert_eq!(service.port.unwrap(), 9998);
        assert_eq!(service.restricted, false);
        assert_eq!(service.transport.unwrap(), Transport::Udp);
        assert_eq!(service.type_, Type::Stun);
        assert!(service.username.is_none());
        assert!(service.ext_info.is_empty());
    }

    #[test]
    fn test_service_query() {
        let query = ServicesQuery { type_: None };
        let elem = Element::from(query);
        assert!(elem.is("services", ns::EXT_DISCO));
        assert_eq!(elem.attrs().into_iter().next(), None);
        assert_eq!(elem.nodes().next(), None);
    }

    #[test]
    fn test_service_result() {
        let elem: Element = "<services xmlns='urn:xmpp:extdisco:2' type='stun'><service host='stun.shakespeare.lit' port='9998' transport='udp' type='stun'/></services>".parse().unwrap();
        let services = ServicesResult::try_from(elem).unwrap();
        assert_eq!(services.type_, Some(Type::Stun));
        assert_eq!(services.services.len(), 1);
    }
}
