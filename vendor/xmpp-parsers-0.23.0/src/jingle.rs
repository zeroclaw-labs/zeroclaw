// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::iq::IqSetPayload;
use crate::jingle_grouping::Group;
use crate::jingle_ibb::Transport as IbbTransport;
use crate::jingle_ice_udp::Transport as IceUdpTransport;
use crate::jingle_rtp::Description as RtpDescription;
use crate::jingle_s5b::Transport as Socks5Transport;
use crate::ns;
use alloc::{collections::BTreeMap, fmt};
use jid::Jid;
use minidom::Element;
use xso::error::Error;

generate_attribute!(
    /// The action attribute.
    Action, "action", {
        /// Accept a content-add action received from another party.
        ContentAccept => "content-accept",

        /// Add one or more new content definitions to the session.
        ContentAdd => "content-add",

        /// Change the directionality of media sending.
        ContentModify => "content-modify",

        /// Reject a content-add action received from another party.
        ContentReject => "content-reject",

        /// Remove one or more content definitions from the session.
        ContentRemove => "content-remove",

        /// Exchange information about parameters for an application type.
        DescriptionInfo => "description-info",

        /// Exchange information about security preconditions.
        SecurityInfo => "security-info",

        /// Definitively accept a session negotiation.
        SessionAccept => "session-accept",

        /// Send session-level information, such as a ping or a ringing message.
        SessionInfo => "session-info",

        /// Request negotiation of a new Jingle session.
        SessionInitiate => "session-initiate",

        /// End an existing session.
        SessionTerminate => "session-terminate",

        /// Accept a transport-replace action received from another party.
        TransportAccept => "transport-accept",

        /// Exchange transport candidates.
        TransportInfo => "transport-info",

        /// Reject a transport-replace action received from another party.
        TransportReject => "transport-reject",

        /// Redefine a transport method or replace it with a different method.
        TransportReplace => "transport-replace",
    }
);

generate_attribute!(
    /// Which party originally generated the content type.
    Creator, "creator", {
        /// This content was created by the initiator of this session.
        Initiator => "initiator",

        /// This content was created by the responder of this session.
        Responder => "responder",
    }
);

generate_attribute!(
    /// Which parties in the session will be generating content.
    Senders, "senders", {
        /// Both parties can send for this content.
        Both => "both",

        /// Only the initiator can send for this content.
        Initiator => "initiator",

        /// No one can send for this content.
        None => "none",

        /// Only the responder can send for this content.
        Responder => "responder",
    }, Default = Both
);

generate_attribute!(
    /// How the content definition is to be interpreted by the recipient. The
    /// meaning of this attribute matches the "Content-Disposition" header as
    /// defined in RFC 2183 and applied to SIP by RFC 3261.
    ///
    /// Possible values are defined here:
    /// <https://www.iana.org/assignments/cont-disp/cont-disp.xhtml>
    Disposition, "disposition", {
        /// Displayed automatically.
        Inline => "inline",

        /// User controlled display.
        Attachment => "attachment",

        /// Process as form response.
        FormData => "form-data",

        /// Tunneled content to be processed silently.
        Signal => "signal",

        /// The body is a custom ring tone to alert the user.
        Alert => "alert",

        /// The body is displayed as an icon to the user.
        Icon => "icon",

        /// The body should be displayed to the user.
        Render => "render",

        /// The body contains a list of URIs that indicates the recipients of
        /// the request.
        RecipientListHistory => "recipient-list-history",

        /// The body describes a communications session, for example, an
        /// [RFC2327](https://www.rfc-editor.org/rfc/rfc2327) SDP body.
        Session => "session",

        /// Authenticated Identity Body.
        Aib => "aib",

        /// The body describes an early communications session, for example,
        /// and [RFC2327](https://www.rfc-editor.org/rfc/rfc2327) SDP body.
        EarlySession => "early-session",

        /// The body includes a list of URIs to which URI-list services are to
        /// be applied.
        RecipientList => "recipient-list",

        /// The payload of the message carrying this Content-Disposition header
        /// field value is an Instant Message Disposition Notification as
        /// requested in the corresponding Instant Message.
        Notification => "notification",

        /// The body needs to be handled according to a reference to the body
        /// that is located in the same SIP message as the body.
        ByReference => "by-reference",

        /// The body contains information associated with an Info Package.
        InfoPackage => "info-package",

        /// The body describes either metadata about the RS or the reason for
        /// the metadata snapshot request as determined by the MIME value
        /// indicated in the Content-Type.
        RecordingSession => "recording-session",
    }, Default = Session
);

generate_id!(
    /// An unique identifier in a session, referencing a
    /// [struct.Content.html](Content element).
    ContentId
);

/// Enum wrapping all of the various supported descriptions of a Content.
#[derive(AsXml, Debug, Clone, PartialEq)]
#[xml()]
pub enum Description {
    /// Jingle RTP Sessions (XEP-0167) description.
    #[xml(transparent)]
    Rtp(RtpDescription),

    /// To be used for any description that isn’t known at compile-time.
    // TODO: replace with `#[xml(element, name = ..)]` once we have it.
    #[xml(transparent)]
    Unknown(Element),
}

impl TryFrom<Element> for Description {
    type Error = Error;

    fn try_from(elem: Element) -> Result<Description, Error> {
        Ok(if elem.is("description", ns::JINGLE_RTP) {
            Description::Rtp(RtpDescription::try_from(elem)?)
        } else if elem.name() == "description" {
            Description::Unknown(elem)
        } else {
            return Err(Error::Other("Invalid description."));
        })
    }
}

impl ::xso::FromXml for Description {
    type Builder = ::xso::minidom_compat::FromEventsViaElement<Description>;

    fn from_events(
        qname: ::xso::exports::rxml::QName,
        attrs: ::xso::exports::rxml::AttrMap,
        _ctx: &::xso::Context<'_>,
    ) -> Result<Self::Builder, ::xso::error::FromEventsError> {
        if qname.1 != "description" {
            return Err(::xso::error::FromEventsError::Mismatch { name: qname, attrs });
        }
        Self::Builder::new(qname, attrs)
    }
}

impl From<RtpDescription> for Description {
    fn from(desc: RtpDescription) -> Description {
        Description::Rtp(desc)
    }
}

/// Enum wrapping all of the various supported transports of a Content.
#[derive(AsXml, Debug, Clone, PartialEq)]
#[xml()]
pub enum Transport {
    /// Jingle ICE-UDP Bytestreams (XEP-0176) transport.
    #[xml(transparent)]
    IceUdp(IceUdpTransport),

    /// Jingle In-Band Bytestreams (XEP-0261) transport.
    #[xml(transparent)]
    Ibb(IbbTransport),

    /// Jingle SOCKS5 Bytestreams (XEP-0260) transport.
    #[xml(transparent)]
    Socks5(Socks5Transport),

    /// To be used for any transport that isn’t known at compile-time.
    // TODO: replace with `#[xml(element, name = ..)]` once we have it.
    #[xml(transparent)]
    Unknown(Element),
}

impl TryFrom<Element> for Transport {
    type Error = Error;

    fn try_from(elem: Element) -> Result<Transport, Error> {
        Ok(if elem.is("transport", ns::JINGLE_ICE_UDP) {
            Transport::IceUdp(IceUdpTransport::try_from(elem)?)
        } else if elem.is("transport", ns::JINGLE_IBB) {
            Transport::Ibb(IbbTransport::try_from(elem)?)
        } else if elem.is("transport", ns::JINGLE_S5B) {
            Transport::Socks5(Socks5Transport::try_from(elem)?)
        } else if elem.name() == "transport" {
            Transport::Unknown(elem)
        } else {
            return Err(Error::Other("Invalid transport."));
        })
    }
}

impl ::xso::FromXml for Transport {
    type Builder = ::xso::minidom_compat::FromEventsViaElement<Transport>;

    fn from_events(
        qname: ::xso::exports::rxml::QName,
        attrs: ::xso::exports::rxml::AttrMap,
        _ctx: &::xso::Context<'_>,
    ) -> Result<Self::Builder, ::xso::error::FromEventsError> {
        if qname.1 != "transport" {
            return Err(::xso::error::FromEventsError::Mismatch { name: qname, attrs });
        }
        Self::Builder::new(qname, attrs)
    }
}

impl From<IceUdpTransport> for Transport {
    fn from(transport: IceUdpTransport) -> Transport {
        Transport::IceUdp(transport)
    }
}

impl From<IbbTransport> for Transport {
    fn from(transport: IbbTransport) -> Transport {
        Transport::Ibb(transport)
    }
}

impl From<Socks5Transport> for Transport {
    fn from(transport: Socks5Transport) -> Transport {
        Transport::Socks5(transport)
    }
}

/// A security element inside a Jingle content, stubbed for now.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::JINGLE, name = "security")]
pub struct Security;

/// Describes a session’s content, there can be multiple content in one
/// session.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::JINGLE, name = "content")]
pub struct Content {
    /// Who created this content.
    #[xml(attribute)]
    pub creator: Creator,

    /// How the content definition is to be interpreted by the recipient.
    #[xml(attribute(default))]
    pub disposition: Disposition,

    /// A per-session unique identifier for this content.
    #[xml(attribute)]
    pub name: ContentId,

    /// Who can send data for this content.
    #[xml(attribute(default))]
    pub senders: Senders,

    /// What to send.
    #[xml(child(default))]
    pub description: Option<Description>,

    /// How to send it.
    #[xml(child(default))]
    pub transport: Option<Transport>,

    /// With which security.
    #[xml(child(default))]
    pub security: Option<Security>,
}

impl Content {
    /// Create a new content.
    pub fn new(creator: Creator, name: ContentId) -> Content {
        Content {
            creator,
            name,
            disposition: Disposition::Session,
            senders: Senders::Both,
            description: None,
            transport: None,
            security: None,
        }
    }

    /// Set how the content is to be interpreted by the recipient.
    pub fn with_disposition(mut self, disposition: Disposition) -> Content {
        self.disposition = disposition;
        self
    }

    /// Specify who can send data for this content.
    pub fn with_senders(mut self, senders: Senders) -> Content {
        self.senders = senders;
        self
    }

    /// Set the description of this content.
    pub fn with_description<D: Into<Description>>(mut self, description: D) -> Content {
        self.description = Some(description.into());
        self
    }

    /// Set the transport of this content.
    pub fn with_transport<T: Into<Transport>>(mut self, transport: T) -> Content {
        self.transport = Some(transport.into());
        self
    }

    /// Set the security of this content.
    pub fn with_security(mut self, security: Security) -> Content {
        self.security = Some(security);
        self
    }
}

/// Lists the possible reasons to be included in a Jingle iq.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::JINGLE)]
pub enum Reason {
    /// The party prefers to use an existing session with the peer rather than
    /// initiate a new session; the Jingle session ID of the alternative
    /// session SHOULD be provided as the XML character data of the \<sid/\>
    /// child.
    #[xml(name = "alternative-session")]
    AlternativeSession {
        /// Session ID of the alternative session.
        #[xml(extract(namespace = ns::JINGLE, name = "sid", default, fields(text(type_ = String))))]
        sid: Option<String>,
    },

    /// The party is busy and cannot accept a session.
    #[xml(name = "busy")]
    Busy,

    /// The initiator wishes to formally cancel the session initiation request.
    #[xml(name = "cancel")]
    Cancel,

    /// The action is related to connectivity problems.
    #[xml(name = "connectivity-error")]
    ConnectivityError,

    /// The party wishes to formally decline the session.
    #[xml(name = "decline")]
    Decline,

    /// The session length has exceeded a pre-defined time limit (e.g., a
    /// meeting hosted at a conference service).
    #[xml(name = "expired")]
    Expired,

    /// The party has been unable to initialize processing related to the
    /// application type.
    #[xml(name = "failed-application")]
    FailedApplication,

    /// The party has been unable to establish connectivity for the transport
    /// method.
    #[xml(name = "failed-transport")]
    FailedTransport,

    /// The action is related to a non-specific application error.
    #[xml(name = "general-error")]
    GeneralError,

    /// The entity is going offline or is no longer available.
    #[xml(name = "gone")]
    Gone,

    /// The party supports the offered application type but does not support
    /// the offered or negotiated parameters.
    #[xml(name = "incompatible-parameters")]
    IncompatibleParameters,

    /// The action is related to media processing problems.
    #[xml(name = "media-error")]
    MediaError,

    /// The action is related to a violation of local security policies.
    #[xml(name = "security-error")]
    SecurityError,

    /// The action is generated during the normal course of state management
    /// and does not reflect any error.
    #[xml(name = "success")]
    Success,

    /// A request has not been answered so the sender is timing out the
    /// request.
    #[xml(name = "timeout")]
    Timeout,

    /// The party supports none of the offered application types.
    #[xml(name = "unsupported-applications")]
    UnsupportedApplications,

    /// The party supports none of the offered transport methods.
    #[xml(name = "unsupported-transports")]
    UnsupportedTransports,
}

type Lang = String;

/// Informs the recipient of something.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::JINGLE, name = "reason")]
pub struct ReasonElement {
    /// The list of possible reasons to be included in a Jingle iq.
    #[xml(child)]
    pub reason: Reason,

    /// A human-readable description of this reason.
    #[xml(extract(n = .., namespace = ns::JINGLE, name = "text", fields(
        lang(type_ = Lang, default),
        text(type_ = String),
    )))]
    pub texts: BTreeMap<Lang, String>,
}

impl fmt::Display for ReasonElement {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "{}", Element::from(self.reason.clone()).name())?;
        if let Some(text) = self.texts.get("en") {
            write!(fmt, ": {}", text)?;
        } else if let Some(text) = self.texts.get("") {
            write!(fmt, ": {}", text)?;
        }
        Ok(())
    }
}

generate_id!(
    /// Unique identifier for a session between two JIDs.
    SessionId
);

/// The main Jingle container, to be included in an iq stanza.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::JINGLE, name = "jingle")]
pub struct Jingle {
    /// The action to execute on both ends.
    #[xml(attribute)]
    pub action: Action,

    /// Who the initiator is.
    #[xml(attribute(default))]
    pub initiator: Option<Jid>,

    /// Who the responder is.
    #[xml(attribute(default))]
    pub responder: Option<Jid>,

    /// Unique session identifier between two entities.
    #[xml(attribute)]
    pub sid: SessionId,

    /// A list of contents to be negotiated in this session.
    #[xml(child(n = ..))]
    pub contents: Vec<Content>,

    /// An optional reason.
    #[xml(child(default))]
    pub reason: Option<ReasonElement>,

    /// An optional grouping.
    #[xml(child(default))]
    pub group: Option<Group>,

    /// Payloads to be included.
    #[xml(child(n = ..))]
    pub other: Vec<Element>,
}

impl IqSetPayload for Jingle {}

impl Jingle {
    /// Create a new Jingle element.
    pub fn new(action: Action, sid: SessionId) -> Jingle {
        Jingle {
            action,
            sid,
            initiator: None,
            responder: None,
            contents: Vec::new(),
            reason: None,
            group: None,
            other: Vec::new(),
        }
    }

    /// Set the initiator’s JID.
    pub fn with_initiator(mut self, initiator: Jid) -> Jingle {
        self.initiator = Some(initiator);
        self
    }

    /// Set the responder’s JID.
    pub fn with_responder(mut self, responder: Jid) -> Jingle {
        self.responder = Some(responder);
        self
    }

    /// Add a content to this Jingle container.
    pub fn add_content(mut self, content: Content) -> Jingle {
        self.contents.push(content);
        self
    }

    /// Set the reason in this Jingle container.
    pub fn set_reason(mut self, reason: ReasonElement) -> Jingle {
        self.reason = Some(reason);
        self
    }

    /// Set the grouping in this Jingle container.
    pub fn set_group(mut self, group: Group) -> Jingle {
        self.group = Some(group);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xso::error::FromElementError;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Action, 1);
        assert_size!(Creator, 1);
        assert_size!(Senders, 1);
        assert_size!(Disposition, 1);
        assert_size!(ContentId, 12);
        assert_size!(Content, 160);
        assert_size!(Reason, 12);
        assert_size!(ReasonElement, 24);
        assert_size!(SessionId, 12);
        assert_size!(Jingle, 112);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Action, 1);
        assert_size!(Creator, 1);
        assert_size!(Senders, 1);
        assert_size!(Disposition, 1);
        assert_size!(ContentId, 24);
        assert_size!(Content, 304);
        assert_size!(Reason, 24);
        assert_size!(ReasonElement, 48);
        assert_size!(SessionId, 24);
        assert_size!(Jingle, 224);
    }

    #[test]
    fn test_simple() {
        let elem: Element =
            "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='coucou'/>"
                .parse()
                .unwrap();
        let jingle = Jingle::try_from(elem).unwrap();
        assert_eq!(jingle.action, Action::SessionInitiate);
        assert_eq!(jingle.sid, SessionId(String::from("coucou")));
    }

    #[test]
    fn test_invalid_jingle() {
        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1'/>".parse().unwrap();
        let error = Jingle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'action' on Jingle element missing."
        );

        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='session-info'/>"
            .parse()
            .unwrap();
        let error = Jingle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'sid' on Jingle element missing."
        );

        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='coucou' sid='coucou'/>"
            .parse()
            .unwrap();
        let error = Jingle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message.to_string(), "Unknown value for 'action' attribute.");
    }

    #[test]
    fn test_content() {
        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='coucou'><content creator='initiator' name='coucou'><description/><transport xmlns='urn:xmpp:jingle:transports:stub:0'/></content></jingle>".parse().unwrap();
        let jingle = Jingle::try_from(elem).unwrap();
        assert_eq!(jingle.contents[0].creator, Creator::Initiator);
        assert_eq!(jingle.contents[0].name, ContentId(String::from("coucou")));
        assert_eq!(jingle.contents[0].senders, Senders::Both);
        assert_eq!(jingle.contents[0].disposition, Disposition::Session);

        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='coucou'><content creator='initiator' name='coucou' senders='both'><description/><transport xmlns='urn:xmpp:jingle:transports:stub:0'/></content></jingle>".parse().unwrap();
        let jingle = Jingle::try_from(elem).unwrap();
        assert_eq!(jingle.contents[0].senders, Senders::Both);

        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='coucou'><content creator='initiator' name='coucou' disposition='early-session'><description/><transport xmlns='urn:xmpp:jingle:transports:stub:0'/></content></jingle>".parse().unwrap();
        let jingle = Jingle::try_from(elem).unwrap();
        assert_eq!(jingle.contents[0].disposition, Disposition::EarlySession);
    }

    #[test]
    fn test_invalid_content() {
        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='coucou'><content/></jingle>".parse().unwrap();
        let error = Jingle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'creator' on Content element missing."
        );

        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='coucou'><content creator='initiator'/></jingle>".parse().unwrap();
        let error = Jingle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'name' on Content element missing."
        );

        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='coucou'><content creator='coucou' name='coucou'/></jingle>".parse().unwrap();
        let error = Jingle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(string)) => string,
            other => panic!("unexpected result: {:?}", other),
        };
        assert_eq!(
            message.to_string(),
            "Unknown value for 'creator' attribute."
        );

        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='coucou'><content creator='initiator' name='coucou' senders='coucou'/></jingle>".parse().unwrap();
        let error = Jingle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message.to_string(),
            "Unknown value for 'senders' attribute."
        );

        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='coucou'><content creator='initiator' name='coucou' senders=''/></jingle>".parse().unwrap();
        let error = Jingle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message.to_string(),
            "Unknown value for 'senders' attribute."
        );
    }

    #[test]
    fn test_reason() {
        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='coucou'><reason><success/></reason></jingle>".parse().unwrap();
        let jingle = Jingle::try_from(elem).unwrap();
        let reason = jingle.reason.unwrap();
        assert_eq!(reason.reason, Reason::Success);
        assert_eq!(reason.texts, BTreeMap::new());

        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='coucou'><reason><success/><text>coucou</text></reason></jingle>".parse().unwrap();
        let jingle = Jingle::try_from(elem).unwrap();
        let reason = jingle.reason.unwrap();
        assert_eq!(reason.reason, Reason::Success);
        assert_eq!(reason.texts.get(""), Some(&String::from("coucou")));
    }

    #[test]
    fn test_missing_reason_text() {
        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='coucou'><reason/></jingle>".parse().unwrap();
        let error = Jingle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Missing child field 'reason' in ReasonElement element."
        );
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_invalid_child_in_reason() {
        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='coucou'><reason><decline/><a/></reason></jingle>".parse().unwrap();
        let error = Jingle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in ReasonElement element.");
    }

    #[test]
    fn test_multiple_reason_children() {
        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='coucou'><reason><decline/></reason><reason/></jingle>".parse().unwrap();
        let error = Jingle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Jingle element must not have more than one child in field 'reason'."
        );

        // TODO: Reenable this test once xso is able to validate that no more than one text is
        // there for every xml:lang.
        /*
        let elem: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='coucou'><reason><decline/><text/><text/></reason></jingle>".parse().unwrap();
        let error = Jingle::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Text element present twice for the same xml:lang.");
        */
    }

    #[test]
    fn test_serialize_jingle() {
        let reference: Element = "<jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='a73sjjvkla37jfea'><content xmlns='urn:xmpp:jingle:1' creator='initiator' name='this-is-a-stub'><description xmlns='urn:xmpp:jingle:apps:stub:0'/><transport xmlns='urn:xmpp:jingle:transports:stub:0'/></content></jingle>"
        .parse()
        .unwrap();

        let jingle = Jingle {
            action: Action::SessionInitiate,
            initiator: None,
            responder: None,
            sid: SessionId(String::from("a73sjjvkla37jfea")),
            contents: vec![Content {
                creator: Creator::Initiator,
                disposition: Disposition::default(),
                name: ContentId(String::from("this-is-a-stub")),
                senders: Senders::default(),
                description: Some(Description::Unknown(
                    Element::builder("description", "urn:xmpp:jingle:apps:stub:0").build(),
                )),
                transport: Some(Transport::Unknown(
                    Element::builder("transport", "urn:xmpp:jingle:transports:stub:0").build(),
                )),
                security: None,
            }],
            reason: None,
            group: None,
            other: vec![],
        };
        let serialized: Element = jingle.into();
        assert_eq!(serialized, reference);
    }
}
