// Copyright (c) 2022 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{text::EmptyAsNone, AsXml, FromXml};

use crate::ns;

generate_attribute!(
    /// Events for real-time text.
    Event, "event", {
        /// Begin a new real-time message.
        New => "new",

        /// Re-initialize the real-time message.
        Reset => "reset",

        /// Modify existing real-time message.
        Edit => "edit",

        /// Signals activation of real-time text.
        Init => "init",

        /// Signals deactivation of real-time text.
        Cancel => "cancel",
    }, Default = Edit
);

generate_attribute!(
    /// The number of codepoints to erase.
    Num, "n", u32, Default = 1
);

/// Choice between the three possible actions.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::RTT, exhaustive)]
pub enum Action {
    /// Supports the transmission of text, including key presses, and text block inserts.
    #[xml(name = "t")]
    Insert {
        /// Position in the message to start inserting from.  If None, this means to start from the
        /// end of the message.
        #[xml(attribute(default, name = "p"))]
        pos: Option<u32>,

        /// Text to insert.
        #[xml(text = EmptyAsNone)]
        text: Option<String>,
    },

    /// Supports the behavior of backspace key presses.  Text is removed towards beginning of the
    /// message.  This element is also used for all delete operations, including the backspace key,
    /// the delete key, and text block deletes.
    #[xml(name = "e")]
    Erase {
        /// Position in the message to start erasing from.  If None, this means to start from the end
        /// of the message.
        #[xml(attribute(default))]
        pos: Option<u32>,

        /// Amount of characters to erase, to the left.
        #[xml(attribute(default))]
        num: Num,
    },

    /// Allow for the transmission of intervals, between real-time text actions, to recreate the
    /// pauses between key presses.
    #[xml(name = "w")]
    Wait {
        /// Amount of milliseconds to wait before the next action.
        #[xml(attribute = "n")]
        time: u32,
    },
}

/// Element transmitted at regular interval by the sender client while a message is being composed.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::RTT, name = "rtt")]
pub struct Rtt {
    /// Counter to maintain synchronisation of real-time text.  Senders MUST increment this value
    /// by 1 for each subsequent edit to the same real-time message, including when appending new
    /// text.  Receiving clients MUST monitor this 'seq' value as a lightweight verification on the
    /// synchronization of real-time text messages.  The bounds of 'seq' is 31-bits, the range of
    /// positive values for a signed 32-bit integer.
    #[xml(attribute)]
    pub seq: u32,

    /// This attribute signals events for real-time text.
    #[xml(attribute(default))]
    pub event: Event,

    /// When editing a message using XEP-0308, this references the id of the message being edited.
    #[xml(attribute(default))]
    pub id: Option<String>,

    /// Vector of actions being transmitted by this element.
    #[xml(child(n = ..))]
    pub actions: Vec<Action>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Event, 1);
        assert_size!(Action, 20);
        assert_size!(Rtt, 32);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Event, 1);
        assert_size!(Action, 32);
        assert_size!(Rtt, 56);
    }

    #[test]
    fn simple() {
        let elem: Element = "<rtt xmlns='urn:xmpp:rtt:0' seq='0'/>".parse().unwrap();
        let rtt = Rtt::try_from(elem).unwrap();
        assert_eq!(rtt.seq, 0);
        assert_eq!(rtt.event, Event::Edit);
        assert_eq!(rtt.id, None);
        assert_eq!(rtt.actions.len(), 0);
    }

    #[test]
    fn sequence() {
        let elem: Element = "<rtt xmlns='urn:xmpp:rtt:0' seq='0' event='new'><t>Hello,</t><w n='50'/><e/><t>!</t></rtt>"
            .parse()
            .unwrap();

        let rtt = Rtt::try_from(elem).unwrap();
        assert_eq!(rtt.seq, 0);
        assert_eq!(rtt.event, Event::New);
        assert_eq!(rtt.id, None);

        let mut actions = rtt.actions.into_iter();
        assert_eq!(actions.len(), 4);

        let Action::Insert { pos, text } = actions.next().unwrap() else {
            panic!()
        };
        assert_eq!(pos, None);
        assert_eq!(text.unwrap(), "Hello,");

        let Action::Wait { time } = actions.next().unwrap() else {
            panic!()
        };
        assert_eq!(time, 50);

        let Action::Erase { pos, num } = actions.next().unwrap() else {
            panic!()
        };
        assert_eq!(pos, None);
        assert_eq!(num, Num(1));

        let Action::Insert { pos, text } = actions.next().unwrap() else {
            panic!()
        };
        assert_eq!(pos, None);
        assert_eq!(text.unwrap(), "!");
    }
}
