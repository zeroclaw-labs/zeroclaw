// Copyright (c) 2026 Link Mauve <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::date::DateTime;
use crate::iq::{IqGetPayload, IqResultPayload, IqSetPayload};
use crate::ns;
use minidom::Element;

/// List clients that have access to the user’s account.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::CAM, name = "list")]
pub struct List;

impl IqGetPayload for List {}

generate_attribute!(
    /// Either "session" if this client is known to have an active or inactive client session on
    /// the server, or "access" if no session has been established (e.g. it may have been granted
    /// access to the account, but only used non-XMPP APIs or never logged in).
    Type, "type", {
        /// This client is known to have an active or inactive client session on the server.
        Session => "session",

        /// No session has been established (e.g. it may have been granted access to the account,
        /// but only used non-XMPP APIs or never logged in).
        Access => "access",
    }
);

/// Information about the client software.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::CAM, name = "user-agent")]
pub struct UserAgent {
    /// The name of the software.
    #[xml(extract(fields(text)))]
    pub software: String,

    /// A URI/URL for the client, such as a homepage.
    #[xml(extract(default, fields(text(type_ = String))))]
    pub uri: Option<String>,

    /// A human-readable identifier/name for the device where the client runs.
    #[xml(extract(default, fields(text(type_ = String))))]
    pub device: Option<String>,
}

/// Lists the known authentication methods that the client has used to gain access to the account.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::CAM, name = "auth")]
pub struct Auth {
    /// The client has presented a valid password.
    #[xml(flag)]
    pub password: bool,

    /// The client has a valid authorization grant (e.g. via OAuth).
    #[xml(flag)]
    pub grant: bool,

    /// The client has active FAST tokens.
    #[xml(flag)]
    pub fast: bool,

    /// The `<auth/>` element is explicitly extensible - alternative/future authentication
    /// mechanisms may be included under appropriate namespaces.
    #[xml(child(n = ..))]
    pub other: Vec<Element>,
}

generate_attribute!(
    /// Details of the client’s level of access to the user’s account.
    Status, "status", {
        /// The client has full unlimited access to the account.
        Unrestricted => "unrestricted",

        /// The client has general access to the account, but some security-relevant features may
        /// be restricted (such as managing account access and changing the account password).
        Normal => "normal",

        /// the client has additional restrictions in place. In such a case the details of these
        /// restrictions SHOULD be included in an appropriate format (and namespace) within the
        /// `<permission/>` element.
        Restricted => "restricted",
    }
);

/// Contains details of the client’s level of access to the user’s account.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::CAM, name = "permission")]
pub struct Permission {
    /// The actual permission status.
    #[xml(attribute)]
    pub status: Status,
}

/// Represents a client that is know by the server.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::CAM, name = "client")]
pub struct Client {
    /// Reflects whether this client has an active session on the server ("active" includes
    /// connected and sessions that may be disconnected but may yet be reconnected, e.g. using
    /// Stream Management (XEP-0198)).
    #[xml(attribute)]
    pub connected: bool,

    /// An opaque reference for the client, which can be used to revoke access.
    #[xml(attribute)]
    pub id: String,

    /// Either "session" if this client is known to have an active or inactive client session on
    /// the server, or "access" if no session has been established (e.g. it may have been granted
    /// access to the account, but only used non-XMPP APIs or never logged in).
    #[xml(attribute = "type")]
    pub type_: Type,

    /// Contains timestamps that reflect when this client was first granted access to the user’s
    /// account.
    #[xml(extract(default, name = "first-seen", fields(text(type_ = DateTime))))]
    pub first_seen: Option<DateTime>,

    /// Contains timestamps that reflect when this client most recently used that access.
    #[xml(extract(default, name = "last-seen", fields(text(type_ = DateTime))))]
    pub last_seen: Option<DateTime>,

    /// Information about the client software.
    #[xml(child)]
    pub user_agent: UserAgent,

    /// Lists the known authentication methods that the client has used to gain access to the
    /// account.
    #[xml(child)]
    pub auth: Auth,

    /// Contains details of the client’s level of access to the user’s account.
    #[xml(child)]
    pub permission: Permission,
}

/// Container for a list of known clients.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::CAM, name = "clients")]
pub struct Clients {
    /// The list of clients known by the server.
    #[xml(child(n = ..))]
    pub clients: Vec<Client>,
}

impl IqResultPayload for Clients {}

/// Request to revoke access to a client by id.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::CAM, name = "revoke")]
pub struct Revoke {
    /// One of the client ids fetched from the list.
    #[xml(attribute)]
    pub id: String,
}

impl IqSetPayload for Revoke {}

/// If the identified client has previously authenticated with a password, there is no way to
/// revoke access except by changing the user’s password. If you request revocation of such a
/// client, the server will respond with a 'service-unavailable' error, with the
/// 'password-reset-required' application error:
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::CAM, name = "password-reset-required")]
pub struct PasswordResetRequiredError;

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(List, 0);
        assert_size!(Type, 1);
        assert_size!(UserAgent, 36);
        assert_size!(Auth, 16);
        assert_size!(Status, 1);
        assert_size!(Permission, 1);
        assert_size!(Client, 100);
        assert_size!(Clients, 12);
        assert_size!(Revoke, 12);
        assert_size!(PasswordResetRequiredError, 0);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(List, 0);
        assert_size!(Type, 1);
        assert_size!(UserAgent, 72);
        assert_size!(Auth, 32);
        assert_size!(Status, 1);
        assert_size!(Permission, 1);
        assert_size!(Client, 168);
        assert_size!(Clients, 24);
        assert_size!(Revoke, 24);
        assert_size!(PasswordResetRequiredError, 0);
    }

    #[test]
    fn test_example_2() {
        let elem: Element = "<clients xmlns='urn:xmpp:cam:0'>
    <client connected='true' id='zeiP41HLglIu' type='session'>
      <first-seen>2023-04-06T14:26:08Z</first-seen>
      <last-seen>2023-04-06T14:37:25Z</last-seen>
      <auth>
        <password/>
      </auth>
      <permission status='unrestricted'/>
      <user-agent>
        <software>Gajim</software>
        <uri>https://gajim.org/</uri>
        <device>Juliet's laptop</device>
      </user-agent>
    </client>
    <client connected='false' id='HjEEr45_LQr' type='access'>
      <first-seen>2023-03-27T15:16:09Z</first-seen>
      <last-seen>2023-03-27T15:37:24Z</last-seen>
      <auth>
        <grant/>
      </auth>
      <permission status='normal'/>
      <user-agent>
        <software>REST client</software>
      </user-agent>
    </client>
  </clients>"
            .parse()
            .unwrap();
        let clients = Clients::try_from(elem).unwrap();
        let client = &clients.clients[0];
        assert_eq!(client.connected, true);
        assert_eq!(client.id, "zeiP41HLglIu");
        assert_eq!(client.type_, Type::Session);
        assert_eq!(
            client.first_seen,
            Some("2023-04-06T14:26:08Z".parse().unwrap())
        );
        assert_eq!(
            client.last_seen,
            Some("2023-04-06T14:37:25Z".parse().unwrap())
        );
        assert_eq!(client.auth.password, true);
        assert_eq!(client.auth.grant, false);
        assert_eq!(client.auth.fast, false);
        assert!(client.auth.other.is_empty());
        assert_eq!(client.permission.status, Status::Unrestricted);
        assert_eq!(client.user_agent.software, "Gajim");
        assert_eq!(
            client.user_agent.uri,
            Some(String::from("https://gajim.org/"))
        );
        assert_eq!(
            client.user_agent.device,
            Some(String::from("Juliet's laptop"))
        );

        let client = &clients.clients[1];
        assert_eq!(client.connected, false);
        assert_eq!(client.id, "HjEEr45_LQr");
        assert_eq!(client.type_, Type::Access);
        assert_eq!(
            client.first_seen,
            Some("2023-03-27T15:16:09Z".parse().unwrap())
        );
        assert_eq!(
            client.last_seen,
            Some("2023-03-27T15:37:24Z".parse().unwrap())
        );
        assert_eq!(client.auth.password, false);
        assert_eq!(client.auth.grant, true);
        assert_eq!(client.auth.fast, false);
        assert!(client.auth.other.is_empty());
        assert_eq!(client.permission.status, Status::Normal);
        assert_eq!(client.user_agent.software, "REST client");
        assert!(client.user_agent.uri.is_none());
        assert!(client.user_agent.device.is_none());
    }
}
