// Copyright (c) 2018 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::ns;
use crate::stanza_error::DefinedCondition;

/// Acknowledgement of the currently received stanzas.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SM, name = "a")]
pub struct A {
    /// The last handled stanza.
    #[xml(attribute)]
    pub h: u32,
}

impl A {
    /// Generates a new `<a/>` element.
    pub fn new(h: u32) -> A {
        A { h }
    }
}

/// Client request for enabling stream management.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone, Default)]
#[xml(namespace = ns::SM, name = "enable")]
pub struct Enable {
    /// The preferred resumption time in seconds by the client.
    // TODO: should be the infinite integer set ≥ 1.
    #[xml(attribute(default))]
    pub max: Option<u32>,

    /// Whether the client wants to be allowed to resume the stream.
    #[xml(attribute(default))]
    pub resume: bool,
}

impl Enable {
    /// Generates a new `<enable/>` element.
    pub fn new() -> Self {
        Enable::default()
    }

    /// Sets the preferred resumption time in seconds.
    pub fn with_max(mut self, max: u32) -> Self {
        self.max = Some(max);
        self
    }

    /// Asks for resumption to be possible.
    pub fn with_resume(mut self) -> Self {
        self.resume = true;
        self
    }
}

generate_id!(
    /// A random identifier used for stream resumption.
    StreamId
);

/// Server response once stream management is enabled.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SM, name = "enabled")]
pub struct Enabled {
    /// A random identifier used for stream resumption.
    #[xml(attribute(default))]
    pub id: Option<StreamId>,

    /// The preferred IP, domain, IP:port or domain:port location for
    /// resumption.
    #[xml(attribute(default))]
    pub location: Option<String>,

    /// The preferred resumption time in seconds by the server.
    // TODO: should be the infinite integer set ≥ 1.
    #[xml(attribute(default))]
    pub max: Option<u32>,

    /// Whether stream resumption is allowed.
    #[xml(attribute(default))]
    pub resume: bool,
}

/// A stream management error happened.
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::SM, name = "failed")]
pub struct Failed {
    /// The last handled stanza.
    #[xml(attribute)]
    pub h: Option<u32>,

    /// The error returned.
    // XXX: implement the * handling.
    #[xml(child(default))]
    pub error: Option<DefinedCondition>,
}

/// Requests the currently received stanzas by the other party.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SM, name = "r")]
pub struct R;

/// Requests a stream resumption.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SM, name = "resume")]
pub struct Resume {
    /// The last handled stanza.
    #[xml(attribute)]
    pub h: u32,

    /// The previous id given by the server on
    /// [enabled](struct.Enabled.html).
    #[xml(attribute)]
    pub previd: StreamId,
}

/// The response by the server for a successfully resumed stream.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SM, name = "resumed")]
pub struct Resumed {
    /// The last handled stanza.
    #[xml(attribute)]
    pub h: u32,

    /// The previous id given by the server on
    /// [enabled](struct.Enabled.html).
    #[xml(attribute)]
    pub previd: StreamId,
}

/// Represents availability of Stream Management in `<stream:features/>`.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SM, name = "sm")]
pub struct StreamManagement {
    /// Undocumented `<optional/>` flag.
    // TODO: Remove this flag once servers in the wild have been updated to not send it, as it is
    // completely undocumented in XEP-0198, only appearing in the XML Schema before 1.6.3.
    #[xml(flag)]
    pub optional: bool,
}

/// Application-specific error condition to use when the peer acknowledges
/// more stanzas than the local side has sent.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SM, name = "handled-count-too-high")]
pub struct HandledCountTooHigh {
    /// The `h` value received by the peer.
    #[xml(attribute)]
    pub h: u32,

    /// The number of stanzas which were in fact sent.
    #[xml(attribute = "send-count")]
    pub send_count: u32,
}

impl From<HandledCountTooHigh> for crate::stream_error::StreamError {
    fn from(other: HandledCountTooHigh) -> Self {
        Self::new(
            crate::stream_error::DefinedCondition::UndefinedCondition,
            "en",
            format!(
                "You acknowledged {} stanza(s), while I only sent {} so far.",
                other.h, other.send_count
            ),
        )
        .with_application_specific(vec![other.into()])
    }
}

/// Enum which allows parsing/serialising any XEP-0198 element.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml()]
pub enum Nonza {
    /// Request to enable SM
    #[xml(transparent)]
    Enable(Enable),

    /// Successful SM enablement response
    #[xml(transparent)]
    Enabled(Enabled),

    /// Request to resume SM
    #[xml(transparent)]
    Resume(Resume),

    /// Successful SM resumption response
    #[xml(transparent)]
    Resumed(Resumed),

    /// Error response
    #[xml(transparent)]
    Failed(Failed),

    /// Acknowledgement
    #[xml(transparent)]
    Ack(A),

    /// Request for an acknowledgement
    #[xml(transparent)]
    Req(R),
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(A, 4);
        assert_size!(Enable, 12);
        assert_size!(StreamId, 12);
        assert_size!(Enabled, 36);
        assert_size!(Failed, 24);
        assert_size!(R, 0);
        assert_size!(Resume, 16);
        assert_size!(Resumed, 16);
        assert_size!(StreamManagement, 1);
        assert_size!(HandledCountTooHigh, 8);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(A, 4);
        assert_size!(Enable, 12);
        assert_size!(StreamId, 24);
        assert_size!(Enabled, 64);
        assert_size!(Failed, 40);
        assert_size!(R, 0);
        assert_size!(Resume, 32);
        assert_size!(Resumed, 32);
        assert_size!(StreamManagement, 1);
        assert_size!(HandledCountTooHigh, 8);
    }

    #[test]
    fn a() {
        let elem: Element = "<a xmlns='urn:xmpp:sm:3' h='5'/>".parse().unwrap();
        let a = A::try_from(elem).unwrap();
        assert_eq!(a.h, 5);
    }

    #[test]
    fn stream_feature() {
        let elem: Element = "<sm xmlns='urn:xmpp:sm:3'/>".parse().unwrap();
        StreamManagement::try_from(elem).unwrap();
    }

    #[test]
    fn handle_count_too_high() {
        let elem: Element = "<handled-count-too-high xmlns='urn:xmpp:sm:3' h='10' send-count='8'/>"
            .parse()
            .unwrap();
        let elem = HandledCountTooHigh::try_from(elem).unwrap();
        assert_eq!(elem.h, 10);
        assert_eq!(elem.send_count, 8);
    }

    #[test]
    fn resume() {
        let elem: Element = "<enable xmlns='urn:xmpp:sm:3' resume='true'/>"
            .parse()
            .unwrap();
        let enable = Enable::try_from(elem).unwrap();
        assert_eq!(enable.max, None);
        assert_eq!(enable.resume, true);

        let elem: Element = "<enabled xmlns='urn:xmpp:sm:3' resume='true' id='coucou' max='600'/>"
            .parse()
            .unwrap();
        let enabled = Enabled::try_from(elem).unwrap();
        let previd = enabled.id.unwrap();
        assert_eq!(enabled.resume, true);
        assert_eq!(previd, StreamId(String::from("coucou")));
        assert_eq!(enabled.max, Some(600));
        assert_eq!(enabled.location, None);

        let elem: Element = "<resume xmlns='urn:xmpp:sm:3' h='5' previd='coucou'/>"
            .parse()
            .unwrap();
        let resume = Resume::try_from(elem).unwrap();
        assert_eq!(resume.h, 5);
        assert_eq!(resume.previd, previd);

        let elem: Element = "<resumed xmlns='urn:xmpp:sm:3' h='5' previd='coucou'/>"
            .parse()
            .unwrap();
        let resumed = Resumed::try_from(elem).unwrap();
        assert_eq!(resumed.h, 5);
        assert_eq!(resumed.previd, previd);
    }

    #[test]
    fn test_serialize_failed() {
        let reference: Element = "<failed xmlns='urn:xmpp:sm:3'><unexpected-request xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/></failed>"
        .parse()
        .unwrap();

        let elem: Element = "<unexpected-request xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>"
            .parse()
            .unwrap();

        let error = DefinedCondition::try_from(elem).unwrap();

        let failed = Failed {
            h: None,
            error: Some(error),
        };
        let serialized: Element = failed.into();
        assert_eq!(serialized, reference);
    }
}
