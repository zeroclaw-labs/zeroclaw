// Copyright (c) 2020 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use core::net::IpAddr;

use xso::{AsXml, FromXml};

use crate::jingle_ice_udp::Type;
use crate::ns;

/// Wrapper element for an raw UDP transport.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone, Default)]
#[xml(namespace = ns::JINGLE_RAW_UDP, name = "transport")]
pub struct Transport {
    /// List of candidates for this raw UDP session.
    #[xml(child(n = ..))]
    pub candidates: Vec<Candidate>,
}

impl Transport {
    /// Create a new ICE-UDP transport.
    pub fn new() -> Transport {
        Transport::default()
    }

    /// Add a candidate to this transport.
    pub fn add_candidate(mut self, candidate: Candidate) -> Self {
        self.candidates.push(candidate);
        self
    }
}

/// A candidate for an ICE-UDP session.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::JINGLE_RAW_UDP, name = "candidate")]
pub struct Candidate {
    /// A Component ID as defined in ICE-CORE.
    #[xml(attribute)]
    pub component: u8,

    /// An index, starting at 0, that enables the parties to keep track of updates to the
    /// candidate throughout the life of the session.
    #[xml(attribute)]
    pub generation: u8,

    /// A unique identifier for the candidate.
    #[xml(attribute)]
    pub id: String,

    /// The Internet Protocol (IP) address for the candidate transport mechanism; this can be
    /// either an IPv4 address or an IPv6 address.
    #[xml(attribute)]
    pub ip: IpAddr,

    /// The port at the candidate IP address.
    #[xml(attribute)]
    pub port: u16,

    /// A Candidate Type as defined in ICE-CORE.
    #[xml(attribute(default, name = "type"))]
    pub type_: Option<Type>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Transport, 12);
        assert_size!(Candidate, 36);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Transport, 24);
        assert_size!(Candidate, 48);
    }

    #[test]
    fn example_1() {
        let elem: Element = "<transport xmlns='urn:xmpp:jingle:transports:raw-udp:1'>
    <candidate component='1'
               generation='0'
               id='a9j3mnbtu1'
               ip='10.1.1.104'
               port='13540'/>
</transport>"
            .parse()
            .unwrap();
        let mut transport = Transport::try_from(elem).unwrap();
        assert_eq!(transport.candidates.len(), 1);
        let candidate = transport.candidates.pop().unwrap();
        assert_eq!(candidate.component, 1);
        assert_eq!(candidate.generation, 0);
        assert_eq!(candidate.id, "a9j3mnbtu1");
        assert_eq!(candidate.ip, "10.1.1.104".parse::<IpAddr>().unwrap());
        assert_eq!(candidate.port, 13540u16);
        assert!(candidate.type_.is_none());
    }
}
