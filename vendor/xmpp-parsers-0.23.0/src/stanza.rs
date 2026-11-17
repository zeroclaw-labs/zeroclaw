// Copyright (c) 2024 Jonas Schäfer <jonas@zombofant.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::{iq::Iq, message::Message, presence::Presence};

/// A stanza sent/received over the stream.
///
/// WARNING: do not add variants to this enum! Adding variants which refer
/// to anything but [`Iq`], [`Message`] or [`Presence`] stanzas will cause the
/// stream management counters to be off.
#[derive(FromXml, AsXml, Debug, PartialEq)]
#[xml()]
pub enum Stanza {
    /// IQ stanza
    #[xml(transparent)]
    Iq(Iq),

    /// Message stanza
    #[xml(transparent)]
    Message(Message),

    /// Presence stanza
    #[xml(transparent)]
    Presence(Presence),
}

impl From<Iq> for Stanza {
    fn from(other: Iq) -> Self {
        Self::Iq(other)
    }
}

impl From<Presence> for Stanza {
    fn from(other: Presence) -> Self {
        Self::Presence(other)
    }
}

impl From<Message> for Stanza {
    fn from(other: Message) -> Self {
        Self::Message(other)
    }
}

impl TryFrom<Stanza> for Message {
    type Error = Stanza;

    fn try_from(other: Stanza) -> Result<Self, Stanza> {
        match other {
            Stanza::Message(st) => Ok(st),
            other => Err(other),
        }
    }
}

impl TryFrom<Stanza> for Presence {
    type Error = Stanza;

    fn try_from(other: Stanza) -> Result<Self, Stanza> {
        match other {
            Stanza::Presence(st) => Ok(st),
            other => Err(other),
        }
    }
}

impl TryFrom<Stanza> for Iq {
    type Error = Stanza;

    fn try_from(other: Stanza) -> Result<Self, Stanza> {
        match other {
            Stanza::Iq(st) => Ok(st),
            other => Err(other),
        }
    }
}
