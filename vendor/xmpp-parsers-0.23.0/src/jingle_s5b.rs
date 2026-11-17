// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::exports::rxml;
use xso::{
    error::{Error, FromElementError},
    AsXml, FromXml,
};

use crate::ns;
use core::net::IpAddr;
use jid::Jid;
use minidom::Element;

generate_attribute!(
    /// The type of the connection being proposed by this candidate.
    Type, "type", {
        /// Direct connection using NAT assisting technologies like NAT-PMP or
        /// UPnP-IGD.
        Assisted => "assisted",

        /// Direct connection using the given interface.
        Direct => "direct",

        /// SOCKS5 relay.
        Proxy => "proxy",

        /// Tunnel protocol such as Teredo.
        Tunnel => "tunnel",
    }, Default = Direct
);

generate_attribute!(
    /// Which mode to use for the connection.
    Mode, "mode", {
        /// Use TCP, which is the default.
        Tcp => "tcp",

        /// Use UDP.
        Udp => "udp",
    }, Default = Tcp
);

generate_id!(
    /// An identifier for a candidate.
    CandidateId
);

generate_id!(
    /// An identifier for a stream.
    StreamId
);

/// A candidate for a connection.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::JINGLE_S5B, name = "candidate")]
pub struct Candidate {
    /// The identifier for this candidate.
    #[xml(attribute)]
    cid: CandidateId,

    /// The host to connect to.
    #[xml(attribute)]
    host: IpAddr,

    /// The JID to request at the given end.
    #[xml(attribute)]
    jid: Jid,

    /// The port to connect to.
    #[xml(attribute(default))]
    port: Option<u16>,

    /// The priority of this candidate, computed using this formula:
    /// priority = (2^16)*(type preference) + (local preference)
    #[xml(attribute)]
    priority: u32,

    /// The type of the connection being proposed by this candidate.
    #[xml(attribute(default, name = "type"))]
    type_: Type,
}

impl Candidate {
    /// Creates a new candidate with the given parameters.
    pub fn new(cid: CandidateId, host: IpAddr, jid: Jid, priority: u32) -> Candidate {
        Candidate {
            cid,
            host,
            jid,
            priority,
            port: Default::default(),
            type_: Default::default(),
        }
    }

    /// Sets the port of this candidate.
    pub fn with_port(mut self, port: u16) -> Candidate {
        self.port = Some(port);
        self
    }

    /// Sets the type of this candidate.
    pub fn with_type(mut self, type_: Type) -> Candidate {
        self.type_ = type_;
        self
    }
}

/// The payload of a transport.
#[derive(Debug, Clone, PartialEq)]
pub enum TransportPayload {
    /// The responder informs the initiator that the bytestream pointed by this
    /// candidate has been activated.
    Activated(CandidateId),

    /// A list of suggested candidates.
    Candidates(Vec<Candidate>),

    /// Both parties failed to use a candidate, they should fallback to another
    /// transport.
    CandidateError,

    /// The candidate pointed here should be used by both parties.
    CandidateUsed(CandidateId),

    /// This entity can’t connect to the SOCKS5 proxy.
    ProxyError,

    /// XXX: Invalid, should not be found in the wild.
    None,
}

/// Describes a Jingle transport using a direct or proxied connection.
#[derive(Debug, Clone, PartialEq)]
pub struct Transport {
    /// The stream identifier for this transport.
    pub sid: StreamId,

    /// The destination address.
    pub dstaddr: Option<String>,

    /// The mode to be used for the transfer.
    pub mode: Mode,

    /// The payload of this transport.
    pub payload: TransportPayload,
}

impl Transport {
    /// Creates a new transport element.
    pub fn new(sid: StreamId) -> Transport {
        Transport {
            sid,
            dstaddr: None,
            mode: Default::default(),
            payload: TransportPayload::None,
        }
    }

    /// Sets the destination address of this transport.
    pub fn with_dstaddr(mut self, dstaddr: String) -> Transport {
        self.dstaddr = Some(dstaddr);
        self
    }

    /// Sets the mode of this transport.
    pub fn with_mode(mut self, mode: Mode) -> Transport {
        self.mode = mode;
        self
    }

    /// Sets the payload of this transport.
    pub fn with_payload(mut self, payload: TransportPayload) -> Transport {
        self.payload = payload;
        self
    }
}

impl TryFrom<Element> for Transport {
    type Error = FromElementError;

    fn try_from(elem: Element) -> Result<Transport, FromElementError> {
        check_self!(elem, "transport", JINGLE_S5B);
        check_no_unknown_attributes!(elem, "transport", ["sid", "dstaddr", "mode"]);
        let sid = get_attr!(elem, "sid", Required);
        let dstaddr = get_attr!(elem, "dstaddr", Option);
        let mode = get_attr!(elem, "mode", Default);

        let mut payload = None;
        for child in elem.children() {
            payload = Some(if child.is("candidate", ns::JINGLE_S5B) {
                let mut candidates =
                    match payload {
                        Some(TransportPayload::Candidates(candidates)) => candidates,
                        Some(_) => return Err(Error::Other(
                            "Non-candidate child already present in JingleS5B transport element.",
                        )
                        .into()),
                        None => vec![],
                    };
                candidates.push(Candidate::try_from(child.clone())?);
                TransportPayload::Candidates(candidates)
            } else if child.is("activated", ns::JINGLE_S5B) {
                if payload.is_some() {
                    return Err(Error::Other(
                        "Non-activated child already present in JingleS5B transport element.",
                    )
                    .into());
                }
                let cid = get_attr!(child, "cid", Required);
                TransportPayload::Activated(cid)
            } else if child.is("candidate-error", ns::JINGLE_S5B) {
                if payload.is_some() {
                    return Err(Error::Other(
                        "Non-candidate-error child already present in JingleS5B transport element.",
                    )
                    .into());
                }
                TransportPayload::CandidateError
            } else if child.is("candidate-used", ns::JINGLE_S5B) {
                if payload.is_some() {
                    return Err(Error::Other(
                        "Non-candidate-used child already present in JingleS5B transport element.",
                    )
                    .into());
                }
                let cid = get_attr!(child, "cid", Required);
                TransportPayload::CandidateUsed(cid)
            } else if child.is("proxy-error", ns::JINGLE_S5B) {
                if payload.is_some() {
                    return Err(Error::Other(
                        "Non-proxy-error child already present in JingleS5B transport element.",
                    )
                    .into());
                }
                TransportPayload::ProxyError
            } else {
                return Err(Error::Other("Unknown child in JingleS5B transport element.").into());
            });
        }
        let payload = payload.unwrap_or(TransportPayload::None);
        Ok(Transport {
            sid,
            dstaddr,
            mode,
            payload,
        })
    }
}

impl From<Transport> for Element {
    fn from(transport: Transport) -> Element {
        Element::builder("transport", ns::JINGLE_S5B)
            .attr(rxml::xml_ncname!("sid").into(), transport.sid)
            .attr(rxml::xml_ncname!("dstaddr").into(), transport.dstaddr)
            .attr(rxml::xml_ncname!("mode").into(), transport.mode)
            .append_all(match transport.payload {
                TransportPayload::Candidates(candidates) => candidates
                    .into_iter()
                    .map(Element::from)
                    .collect::<Vec<_>>(),
                TransportPayload::Activated(cid) => {
                    vec![Element::builder("activated", ns::JINGLE_S5B)
                        .attr(rxml::xml_ncname!("cid").into(), cid)
                        .build()]
                }
                TransportPayload::CandidateError => {
                    vec![Element::builder("candidate-error", ns::JINGLE_S5B).build()]
                }
                TransportPayload::CandidateUsed(cid) => {
                    vec![Element::builder("candidate-used", ns::JINGLE_S5B)
                        .attr(rxml::xml_ncname!("cid").into(), cid)
                        .build()]
                }
                TransportPayload::ProxyError => {
                    vec![Element::builder("proxy-error", ns::JINGLE_S5B).build()]
                }
                TransportPayload::None => vec![],
            })
            .build()
    }
}

impl ::xso::FromXml for Transport {
    type Builder = ::xso::minidom_compat::FromEventsViaElement<Transport>;

    fn from_events(
        qname: ::xso::exports::rxml::QName,
        attrs: ::xso::exports::rxml::AttrMap,
        _ctx: &::xso::Context<'_>,
    ) -> Result<Self::Builder, ::xso::error::FromEventsError> {
        if qname.0 != crate::ns::JINGLE_S5B || qname.1 != "transport" {
            return Err(::xso::error::FromEventsError::Mismatch { name: qname, attrs });
        }
        Self::Builder::new(qname, attrs)
    }
}

impl ::xso::AsXml for Transport {
    type ItemIter<'x> = ::xso::minidom_compat::AsItemsViaElement<'x>;

    fn as_xml_iter(&self) -> Result<Self::ItemIter<'_>, ::xso::error::Error> {
        ::xso::minidom_compat::AsItemsViaElement::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::str::FromStr;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Type, 1);
        assert_size!(Mode, 1);
        assert_size!(CandidateId, 12);
        assert_size!(StreamId, 12);
        assert_size!(Candidate, 56);
        assert_size!(TransportPayload, 16);
        assert_size!(Transport, 44);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Type, 1);
        assert_size!(Mode, 1);
        assert_size!(CandidateId, 24);
        assert_size!(StreamId, 24);
        assert_size!(Candidate, 88);
        assert_size!(TransportPayload, 32);
        assert_size!(Transport, 88);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<transport xmlns='urn:xmpp:jingle:transports:s5b:1' sid='coucou'/>"
            .parse()
            .unwrap();
        let transport = Transport::try_from(elem).unwrap();
        assert_eq!(transport.sid, StreamId(String::from("coucou")));
        assert_eq!(transport.dstaddr, None);
        assert_eq!(transport.mode, Mode::Tcp);
        match transport.payload {
            TransportPayload::None => (),
            _ => panic!("Wrong element inside transport!"),
        }
    }

    #[test]
    fn test_serialise_activated() {
        let elem: Element = "<transport xmlns='urn:xmpp:jingle:transports:s5b:1' sid='coucou'><activated cid='coucou'/></transport>".parse().unwrap();
        let transport = Transport {
            sid: StreamId(String::from("coucou")),
            dstaddr: None,
            mode: Mode::Tcp,
            payload: TransportPayload::Activated(CandidateId(String::from("coucou"))),
        };
        let elem2: Element = transport.into();
        assert_eq!(elem, elem2);
    }

    #[test]
    fn test_serialise_candidate() {
        let elem: Element = "<transport xmlns='urn:xmpp:jingle:transports:s5b:1' sid='coucou'><candidate cid='coucou' host='127.0.0.1' jid='coucou@coucou' priority='0'/></transport>".parse().unwrap();
        let transport = Transport {
            sid: StreamId(String::from("coucou")),
            dstaddr: None,
            mode: Mode::Tcp,
            payload: TransportPayload::Candidates(vec![Candidate {
                cid: CandidateId(String::from("coucou")),
                host: IpAddr::from_str("127.0.0.1").unwrap(),
                jid: Jid::new("coucou@coucou").unwrap(),
                port: None,
                priority: 0u32,
                type_: Type::Direct,
            }]),
        };
        let elem2: Element = transport.into();
        assert_eq!(elem, elem2);
    }
}
