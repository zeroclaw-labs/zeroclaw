// Copyright (c) 2017 Maxime “pep” Buquet <pep@bouah.net>
// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::date::DateTime;
use crate::ns;
use crate::presence::PresencePayload;

/// Represents the query for messages before our join.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone, Default)]
#[xml(namespace = ns::MUC, name = "history")]
pub struct History {
    /// How many characters of history to send, in XML characters.
    #[xml(attribute(default))]
    pub maxchars: Option<u32>,

    /// How many messages to send.
    #[xml(attribute(default))]
    pub maxstanzas: Option<u32>,

    /// Only send messages received in these last seconds.
    #[xml(attribute(default))]
    pub seconds: Option<u32>,

    /// Only send messages after this date.
    #[xml(attribute(default))]
    pub since: Option<DateTime>,
}

impl History {
    /// Create a new empty history element.
    pub fn new() -> Self {
        History::default()
    }

    /// Set how many characters of history to send.
    pub fn with_maxchars(mut self, maxchars: u32) -> Self {
        self.maxchars = Some(maxchars);
        self
    }

    /// Set how many messages to send.
    pub fn with_maxstanzas(mut self, maxstanzas: u32) -> Self {
        self.maxstanzas = Some(maxstanzas);
        self
    }

    /// Only send messages received in these last seconds.
    pub fn with_seconds(mut self, seconds: u32) -> Self {
        self.seconds = Some(seconds);
        self
    }

    /// Only send messages received since this date.
    pub fn with_since(mut self, since: DateTime) -> Self {
        self.since = Some(since);
        self
    }
}

/// Represents a room join request.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone, Default)]
#[xml(namespace = ns::MUC, name = "x")]
pub struct Muc {
    /// Password to use when the room is protected by a password.
    #[xml(extract(default, fields(text(type_ = String))))]
    pub password: Option<String>,

    /// Controls how much and how old we want to receive history on join.
    #[xml(child(default))]
    pub history: Option<History>,
}

impl PresencePayload for Muc {}

impl Muc {
    /// Create a new MUC join element.
    pub fn new() -> Self {
        Muc::default()
    }

    /// Join a room with this password.
    pub fn with_password(mut self, password: String) -> Self {
        self.password = Some(password);
        self
    }

    /// Join a room with only that much history.
    pub fn with_history(mut self, history: History) -> Self {
        self.history = Some(history);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::str::FromStr;
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(History, 40);
        assert_size!(Muc, 52);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(History, 40);
        assert_size!(Muc, 64);
    }

    #[test]
    fn test_muc_simple() {
        let elem: Element = "<x xmlns='http://jabber.org/protocol/muc'/>"
            .parse()
            .unwrap();
        Muc::try_from(elem).unwrap();
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_muc_invalid_child() {
        let elem: Element = "<x xmlns='http://jabber.org/protocol/muc'><coucou/></x>"
            .parse()
            .unwrap();
        let error = Muc::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in Muc element.");
    }

    #[test]
    fn test_muc_serialise() {
        let elem: Element = "<x xmlns='http://jabber.org/protocol/muc'/>"
            .parse()
            .unwrap();
        let muc = Muc {
            password: None,
            history: None,
        };
        let elem2 = muc.into();
        assert_eq!(elem, elem2);
    }

    #[cfg(feature = "pedantic")]
    #[test]
    fn test_muc_invalid_attribute() {
        let elem: Element = "<x xmlns='http://jabber.org/protocol/muc' coucou=''/>"
            .parse()
            .unwrap();
        let error = Muc::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown attribute in Muc element.");
    }

    #[test]
    fn test_muc_simple_password() {
        let elem: Element =
            "<x xmlns='http://jabber.org/protocol/muc'><password>coucou</password></x>"
                .parse()
                .unwrap();
        let elem1 = elem.clone();
        let muc = Muc::try_from(elem).unwrap();
        assert_eq!(muc.password, Some("coucou".to_owned()));

        let elem2 = Element::from(muc);
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn history() {
        let elem: Element = "<x xmlns='http://jabber.org/protocol/muc'>
                <history maxstanzas='0'/>
            </x>"
            .parse()
            .unwrap();
        let muc = Muc::try_from(elem).unwrap();
        let muc2 = Muc::new().with_history(History::new().with_maxstanzas(0));
        assert_eq!(muc, muc2);

        let history = muc.history.unwrap();
        assert_eq!(history.maxstanzas, Some(0));
        assert_eq!(history.maxchars, None);
        assert_eq!(history.seconds, None);
        assert_eq!(history.since, None);

        let elem: Element = "<x xmlns='http://jabber.org/protocol/muc'>
                <history since='1970-01-01T00:00:00Z'/>
            </x>"
            .parse()
            .unwrap();
        let muc = Muc::try_from(elem).unwrap();
        assert_eq!(
            muc.history.unwrap().since.unwrap(),
            DateTime::from_str("1970-01-01T00:00:00+00:00").unwrap()
        );
    }
}
