// Copyright (c) 2017 Maxime “pep” Buquet <pep@bouah.net>
// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::message::MessagePayload;
use crate::muc::mav::AffiliationsVersioning;
use crate::ns;
use crate::presence::PresencePayload;

use jid::{FullJid, Jid};

generate_attribute_enum!(
/// Lists all of the possible status codes used in MUC presences.
Status, "status", MUC_USER, "code", {
    /// 100: Inform user that any occupant is allowed to see the user's full JID
    NonAnonymousRoom => 100,

    /// 101: Inform user that his or her affiliation changed while not in the room
    AffiliationChange => 101,

    /// 102: Inform occupants that room now shows unavailable members
    ConfigShowsUnavailableMembers => 102,

    /// 103: Inform occupants that room now does not show unavailable members
    ConfigHidesUnavailableMembers => 103,

    /// 104: Inform occupants that a non-privacy-related room configuration change has occurred
    ConfigNonPrivacyRelated => 104,

    /// 110: Inform user that presence refers to itself
    SelfPresence => 110,

    /// 170: Inform occupants that room logging is now enabled
    ConfigRoomLoggingEnabled => 170,

    /// 171: Inform occupants that room logging is now disabled
    ConfigRoomLoggingDisabled => 171,

    /// 172: Inform occupants that the room is now non-anonymous
    ConfigRoomNonAnonymous => 172,

    /// 173: Inform occupants that the room is now semi-anonymous
    ConfigRoomSemiAnonymous => 173,

    /// 201: Inform user that a new room has been created
    RoomHasBeenCreated => 201,

    /// 210: Inform user that service has assigned or modified occupant's roomnick
    AssignedNick => 210,

    /// 301: Inform user that they have been banned from the room
    Banned => 301,

    /// 303: Inform all occupants of new room nickname
    NewNick => 303,

    /// 307: Inform user that they have been kicked from the room
    Kicked => 307,

    /// 321: Inform user that they are being removed from the room
    /// because of an affiliation change
    RemovalFromRoom => 321,

    /// 322: Inform user that they are being removed from the room
    /// because the room has been changed to members-only and the
    /// user is not a member
    ConfigMembersOnly => 322,

    /// 332: Inform user that they are being removed from the room
    /// because the MUC service is being shut down
    ServiceShutdown => 332,

    /// 333: Inform user that they are being removed from the room for technical reasons
    ServiceErrorKick => 333,
});

/// Optional \<actor/\> element used in \<item/\> elements inside presence stanzas of type
/// "unavailable" that are sent to users who are kick or banned, as well as within IQs for tracking
/// purposes. -- CHANGELOG  0.17 (2002-10-23)
///
/// Possesses a 'jid' and a 'nick' attribute, so that an action can be attributed either to a real
/// JID or to a roomnick. -- CHANGELOG  1.25 (2012-02-08)
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::MUC_USER, name = "actor")]
pub struct Actor {
    /// The full JID associated with this user.
    #[xml(attribute(default))]
    jid: Option<FullJid>,

    /// The nickname of this user.
    #[xml(attribute(default))]
    nick: Option<String>,
}

/// Used to continue a one-to-one discussion in a room, with more than one
/// participant.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::MUC_USER, name = "continue")]
pub struct Continue {
    /// The thread to continue in this room.
    #[xml(attribute(default))]
    pub thread: Option<String>,
}

generate_elem_id!(
    /// A reason for inviting, declining, etc. a request.
    Reason,
    "reason",
    MUC_USER
);

generate_attribute!(
    /// The affiliation of an entity with a room, which isn’t tied to its
    /// presence in it.
    Affiliation, "affiliation", {
        /// The user who created the room, or who got appointed by its creator
        /// to be their equal.
        Owner => "owner",

        /// A user who has been empowered by an owner to do administrative
        /// operations.
        Admin => "admin",

        /// A user who is whitelisted to speak in moderated rooms, or to join a
        /// member-only room.
        Member => "member",

        /// A user who has been banned from this room.
        Outcast => "outcast",

        /// A normal participant.
        None => "none",
    }, Default = None
);

generate_attribute!(
    /// The current role of an entity in a room, it can be changed by an owner
    /// or an administrator but will be lost once they leave the room.
    Role, "role", {
        /// This user can kick other participants, as well as grant and revoke
        /// them voice.
        Moderator => "moderator",

        /// A user who can speak in this room.
        Participant => "participant",

        /// A user who cannot speak in this room, and must request voice before
        /// doing so.
        Visitor => "visitor",

        /// A user who is absent from the room.
        None => "none",
    }, Default = None
);

/// An item representing a user in a room.
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::MUC_USER, name = "item")]
pub struct Item {
    /// The affiliation of this user with the room.
    #[xml(attribute)]
    pub affiliation: Affiliation,

    /// The real JID of this user, if you are allowed to see it.
    #[xml(attribute(default))]
    pub jid: Option<FullJid>,

    /// The current nickname of this user.
    #[xml(attribute(default))]
    pub nick: Option<String>,

    /// The current role of this user.
    #[xml(attribute)]
    pub role: Role,

    /// The actor affected by this item.
    #[xml(child(default))]
    pub actor: Option<Actor>,

    /// Whether this continues a one-to-one discussion.
    #[xml(child(default))]
    pub continue_: Option<Continue>,

    /// A reason for this item.
    #[xml(child(default))]
    pub reason: Option<Reason>,
}

impl Item {
    /// Creates a new item with the given affiliation and role.
    pub fn new(affiliation: Affiliation, role: Role) -> Item {
        Item {
            affiliation,
            role,
            jid: None,
            nick: None,
            actor: None,
            continue_: None,
            reason: None,
        }
    }

    /// Set a jid for this Item
    pub fn with_jid(mut self, jid: FullJid) -> Item {
        self.jid = Some(jid);
        self
    }

    /// Set a nick for this Item
    pub fn with_nick<S: Into<String>>(mut self, nick: S) -> Item {
        self.nick = Some(nick.into());
        self
    }

    /// Set an actor for this Item
    pub fn with_actor(mut self, actor: Actor) -> Item {
        self.actor = Some(actor);
        self
    }

    /// Set a continue value for this Item
    pub fn with_continue<S: Into<String>>(mut self, continue_: S) -> Item {
        self.continue_ = Some(Continue {
            thread: Some(continue_.into()),
        });
        self
    }

    /// Set a reason for this Item
    pub fn with_reason<S: Into<String>>(mut self, reason: S) -> Item {
        self.reason = Some(Reason(reason.into()));
        self
    }
}

/// Mediated invite
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::MUC_USER, name = "invite")]
pub struct Invite {
    /// Sender.
    ///
    /// This is only populated for invites which have been mediated by a MUC
    /// and sent to the invitee.
    #[xml(attribute(default))]
    pub from: Option<Jid>,

    /// Recipient.
    ///
    /// This is only populated for requests to mediate an invite through a
    /// MUC, before forwarding it to the invitee.
    #[xml(attribute(default))]
    pub to: Option<Jid>,

    /// The optional reason for the invite.
    #[xml(extract(name = "reason", default, fields(text(type_ = String))))]
    pub reason: Option<String>,
}

/// Rejection of a mediated invite.
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::MUC_USER, name = "decline")]
pub struct Decline {
    /// Sender.
    ///
    /// This is only populated for rejections which have been mediated by a
    /// MUC and sent to the inviter.
    #[xml(attribute(default))]
    pub from: Option<Jid>,

    /// Recipient.
    ///
    /// This is only populated for requests to decline an invite through a
    /// MUC, before forwarding it to the inviter.
    #[xml(attribute(default))]
    pub to: Option<Jid>,

    /// The optional reason for the rejection.
    #[xml(extract(name = "reason", default, fields(text(type_ = String))))]
    pub reason: Option<String>,
}

/// The main muc#user element.
#[derive(FromXml, AsXml, Debug, Default, PartialEq, Clone)]
#[xml(namespace = ns::MUC_USER, name = "x")]
pub struct MucUser {
    /// List of statuses applying to this item.
    #[xml(child(n = ..))]
    pub status: Vec<Status>,

    /// List of items.
    #[xml(child(n = ..))]
    pub items: Vec<Item>,

    /// A mediated invite
    #[xml(child(default))]
    pub invite: Option<Invite>,

    /// A mediated invite rejection
    #[xml(child(default))]
    pub decline: Option<Decline>,

    /// From XEP-0463: MUC Affiliations Versioning
    #[xml(child(default))]
    pub affiliations: Option<AffiliationsVersioning>,
}

impl MucUser {
    /// Creates an empty MucUser
    pub fn new() -> MucUser {
        MucUser::default()
    }

    /// Set statuses for this MucUser
    pub fn with_statuses(mut self, status: Vec<Status>) -> MucUser {
        self.status = status;
        self
    }

    /// Set items for this MucUser
    pub fn with_items(mut self, items: Vec<Item>) -> MucUser {
        self.items = items;
        self
    }
}

impl MessagePayload for MucUser {}
impl PresencePayload for MucUser {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Message;
    use crate::presence::{Presence, Type as PresenceType};
    use jid::Jid;
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Status, 1);
        assert_size!(Actor, 28);
        assert_size!(Continue, 12);
        assert_size!(Reason, 12);
        assert_size!(Affiliation, 1);
        assert_size!(Role, 1);
        assert_size!(Item, 84);
        assert_size!(Invite, 44);
        assert_size!(Decline, 44);
        assert_size!(MucUser, 136);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Status, 1);
        assert_size!(Actor, 56);
        assert_size!(Continue, 24);
        assert_size!(Reason, 24);
        assert_size!(Affiliation, 1);
        assert_size!(Role, 1);
        assert_size!(Item, 168);
        assert_size!(Invite, 88);
        assert_size!(Decline, 88);
        assert_size!(MucUser, 272);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<x xmlns='http://jabber.org/protocol/muc#user'/>"
            .parse()
            .unwrap();
        MucUser::try_from(elem).unwrap();
    }

    #[test]
    fn statuses_and_items() {
        let elem: Element = "<x xmlns='http://jabber.org/protocol/muc#user'>
                <status code='101'/>
                <status code='102'/>
                <item affiliation='member' role='moderator'/>
            </x>"
            .parse()
            .unwrap();
        let muc_user = MucUser::try_from(elem).unwrap();
        assert_eq!(muc_user.status.len(), 2);
        assert_eq!(muc_user.status[0], Status::AffiliationChange);
        assert_eq!(muc_user.status[1], Status::ConfigShowsUnavailableMembers);
        assert_eq!(muc_user.items.len(), 1);
        assert_eq!(muc_user.items[0].affiliation, Affiliation::Member);
        assert_eq!(muc_user.items[0].role, Role::Moderator);
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_invalid_child() {
        let elem: Element = "<x xmlns='http://jabber.org/protocol/muc#user'>
                <coucou/>
            </x>"
            .parse()
            .unwrap();
        let error = MucUser::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in MucUser element.");
    }

    #[test]
    fn test_serialise() {
        let elem: Element = "<x xmlns='http://jabber.org/protocol/muc#user'/>"
            .parse()
            .unwrap();
        let muc = MucUser::new();
        let elem2 = muc.into();
        assert_eq!(elem, elem2);
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_invalid_attribute() {
        let elem: Element = "<x xmlns='http://jabber.org/protocol/muc#user' coucou=''/>"
            .parse()
            .unwrap();
        let error = MucUser::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown attribute in MucUser element.");
    }

    #[test]
    fn test_status_simple() {
        let elem: Element = "<status xmlns='http://jabber.org/protocol/muc#user' code='110'/>"
            .parse()
            .unwrap();
        Status::try_from(elem).unwrap();
    }

    #[test]
    fn test_status_invalid() {
        let elem: Element = "<status xmlns='http://jabber.org/protocol/muc#user'/>"
            .parse()
            .unwrap();
        let error = Status::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Required attribute 'code' missing.");
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_status_invalid_child() {
        let elem: Element = "<status xmlns='http://jabber.org/protocol/muc#user' code='110'>
                <foo/>
            </status>"
            .parse()
            .unwrap();
        let error = Status::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in status element.");
    }

    #[test]
    fn test_status_simple_code() {
        let elem: Element = "<status xmlns='http://jabber.org/protocol/muc#user' code='307'/>"
            .parse()
            .unwrap();
        let status = Status::try_from(elem).unwrap();
        assert_eq!(status, Status::Kicked);
    }

    #[test]
    fn test_status_invalid_code() {
        let elem: Element = "<status xmlns='http://jabber.org/protocol/muc#user' code='666'/>"
            .parse()
            .unwrap();
        let error = Status::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Invalid status code value.");
    }

    #[test]
    fn test_status_invalid_code2() {
        let elem: Element = "<status xmlns='http://jabber.org/protocol/muc#user' code='coucou'/>"
            .parse()
            .unwrap();
        let error = Status::try_from(elem).unwrap_err();
        let error = match error {
            FromElementError::Invalid(Error::TextParseError(error))
                if error.is::<core::num::ParseIntError>() =>
            {
                error
            }
            _ => panic!(),
        };
        assert_eq!(error.to_string(), "invalid digit found in string");
    }

    // This test is now ignored because we switched to a representation where we can’t currently
    // validate whether one of the required attributes is present or not.
    #[test]
    #[ignore]
    fn test_actor_required_attributes() {
        let elem: Element = "<actor xmlns='http://jabber.org/protocol/muc#user'/>"
            .parse()
            .unwrap();
        let error = Actor::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Either 'jid' or 'nick' attribute is required.");
    }

    #[test]
    fn test_actor_jid() {
        let elem: Element = "<actor xmlns='http://jabber.org/protocol/muc#user'
                   jid='foo@bar/baz'/>"
            .parse()
            .unwrap();
        let actor = Actor::try_from(elem).unwrap();
        assert_eq!(actor.jid, Some("foo@bar/baz".parse::<FullJid>().unwrap()));
        assert_eq!(actor.nick, None);
    }

    #[test]
    fn test_actor_nick() {
        let elem: Element = "<actor xmlns='http://jabber.org/protocol/muc#user' nick='baz'/>"
            .parse()
            .unwrap();
        let actor = Actor::try_from(elem).unwrap();
        assert_eq!(actor.nick, Some("baz".to_owned()));
        assert_eq!(actor.jid, None);
    }

    #[test]
    fn test_continue_simple() {
        let elem: Element = "<continue xmlns='http://jabber.org/protocol/muc#user'/>"
            .parse()
            .unwrap();
        Continue::try_from(elem).unwrap();
    }

    #[test]
    fn test_continue_thread_attribute() {
        let elem: Element = "<continue xmlns='http://jabber.org/protocol/muc#user'
                      thread='foo'/>"
            .parse()
            .unwrap();
        let continue_ = Continue::try_from(elem).unwrap();
        assert_eq!(continue_.thread, Some("foo".to_owned()));
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_continue_invalid() {
        let elem: Element =
            "<continue xmlns='http://jabber.org/protocol/muc#user'><foobar/></continue>"
                .parse()
                .unwrap();
        let continue_ = Continue::try_from(elem).unwrap_err();
        let message = match continue_ {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in Continue element.".to_owned());
    }

    #[test]
    fn test_reason_simple() {
        let elem: Element = "<reason xmlns='http://jabber.org/protocol/muc#user'>Reason</reason>"
            .parse()
            .unwrap();
        let elem2 = elem.clone();
        let reason = Reason::try_from(elem).unwrap();
        assert_eq!(reason.0, "Reason".to_owned());

        let elem3 = reason.into();
        assert_eq!(elem2, elem3);
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_reason_invalid_attribute() {
        let elem: Element = "<reason xmlns='http://jabber.org/protocol/muc#user' foo='bar'/>"
            .parse()
            .unwrap();
        let error = Reason::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown attribute in Reason element.".to_owned());
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_reason_invalid() {
        let elem: Element = "<reason xmlns='http://jabber.org/protocol/muc#user'>
                <foobar/>
            </reason>"
            .parse()
            .unwrap();
        let error = Reason::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in Reason element.".to_owned());
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_item_invalid_attr() {
        let elem: Element = "<item xmlns='http://jabber.org/protocol/muc#user'
                  affiliation='member'
                  role='moderator'
                  foo='bar'/>"
            .parse()
            .unwrap();
        let error = Item::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown attribute in Item element.".to_owned());
    }

    #[test]
    fn test_item_affiliation_role_attr() {
        let elem: Element = "<item xmlns='http://jabber.org/protocol/muc#user'
                  affiliation='member'
                  role='moderator'/>"
            .parse()
            .unwrap();
        Item::try_from(elem).unwrap();
    }

    #[test]
    fn test_item_affiliation_role_invalid_attr() {
        let elem: Element = "<item xmlns='http://jabber.org/protocol/muc#user'
                  affiliation='member'/>"
            .parse()
            .unwrap();
        let error = Item::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'role' on Item element missing.".to_owned()
        );
    }

    #[test]
    fn test_item_nick_attr() {
        let elem: Element = "<item xmlns='http://jabber.org/protocol/muc#user'
                  affiliation='member'
                  role='moderator'
                  nick='foobar'/>"
            .parse()
            .unwrap();
        let item = Item::try_from(elem).unwrap();
        match item {
            Item { nick, .. } => assert_eq!(nick, Some("foobar".to_owned())),
        }
    }

    #[test]
    fn test_item_affiliation_role_invalid_attr2() {
        let elem: Element = "<item xmlns='http://jabber.org/protocol/muc#user'
                  role='moderator'/>"
            .parse()
            .unwrap();
        let error = Item::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'affiliation' on Item element missing.".to_owned()
        );
    }

    #[test]
    fn test_item_role_actor_child() {
        let elem: Element = "<item xmlns='http://jabber.org/protocol/muc#user'
                  affiliation='member'
                  role='moderator'>
                <actor nick='foobar'/>
            </item>"
            .parse()
            .unwrap();
        let item = Item::try_from(elem).unwrap();
        let Item { actor, .. } = item;
        let actor = actor.unwrap();
        assert_eq!(actor.nick, Some("foobar".to_owned()));
        assert_eq!(actor.jid, None);
    }

    #[test]
    fn test_item_role_continue_child() {
        let elem: Element = "<item xmlns='http://jabber.org/protocol/muc#user'
                  affiliation='member'
                  role='moderator'>
                <continue thread='foobar'/>
            </item>"
            .parse()
            .unwrap();
        let item = Item::try_from(elem).unwrap();
        let continue_1 = Continue {
            thread: Some("foobar".to_owned()),
        };
        match item {
            Item {
                continue_: Some(continue_2),
                ..
            } => assert_eq!(continue_2.thread, continue_1.thread),
            _ => panic!(),
        }
    }

    #[test]
    fn test_item_role_reason_child() {
        let elem: Element = "<item xmlns='http://jabber.org/protocol/muc#user'
                  affiliation='member'
                  role='moderator'>
                <reason>foobar</reason>
            </item>"
            .parse()
            .unwrap();
        let item = Item::try_from(elem).unwrap();
        match item {
            Item { reason, .. } => assert_eq!(reason, Some(Reason("foobar".to_owned()))),
        }
    }

    #[test]
    fn test_serialize_item() {
        let reference: Element = "<item xmlns='http://jabber.org/protocol/muc#user' affiliation='member' role='moderator'><actor nick='foobar'/><continue thread='foobar'/><reason>foobar</reason></item>"
        .parse()
        .unwrap();

        let elem: Element = "<actor xmlns='http://jabber.org/protocol/muc#user' nick='foobar'/>"
            .parse()
            .unwrap();
        let actor = Actor::try_from(elem).unwrap();

        let elem: Element =
            "<continue xmlns='http://jabber.org/protocol/muc#user' thread='foobar'/>"
                .parse()
                .unwrap();
        let continue_ = Continue::try_from(elem).unwrap();

        let elem: Element = "<reason xmlns='http://jabber.org/protocol/muc#user'>foobar</reason>"
            .parse()
            .unwrap();
        let reason = Reason::try_from(elem).unwrap();

        let item = Item {
            affiliation: Affiliation::Member,
            role: Role::Moderator,
            jid: None,
            nick: None,
            actor: Some(actor),
            reason: Some(reason),
            continue_: Some(continue_),
        };

        let serialized: Element = item.into();
        assert_eq!(serialized, reference);
    }

    #[test]
    fn presence_payload() {
        let elem: Element = "<x xmlns='http://jabber.org/protocol/muc#user'/>"
            .parse()
            .unwrap();
        let presence = Presence::new(PresenceType::None).with_payloads(vec![elem]);
        assert_eq!(presence.payloads.len(), 1);
    }

    #[test]
    fn message_payload() {
        let jid: Jid = Jid::new("louise@example.com").unwrap();
        let elem: Element = "<x xmlns='http://jabber.org/protocol/muc#user'/>"
            .parse()
            .unwrap();
        let message = Message::new(jid).with_payloads(vec![elem]);
        assert_eq!(message.payloads.len(), 1);
    }
}
