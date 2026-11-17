// Copyright (c) 2018 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{text::Base64, AsXml, FromXml};

use crate::ns;
use alloc::collections::BTreeMap;

generate_attribute!(
    /// The list of available SASL mechanisms.
    Mechanism, "mechanism", {
        /// Uses no hashing mechanism and transmit the password in clear to the
        /// server, using a single step.
        Plain => "PLAIN",

        /// Challenge-based mechanism using HMAC and SHA-1, allows both the
        /// client and the server to avoid having to store the password in
        /// clear.
        ///
        /// See <https://www.rfc-editor.org/rfc/rfc5802>
        ScramSha1 => "SCRAM-SHA-1",

        /// Same as [ScramSha1](#structfield.ScramSha1), with the addition of
        /// channel binding.
        ScramSha1Plus => "SCRAM-SHA-1-PLUS",

        /// Same as [ScramSha1](#structfield.ScramSha1), but using SHA-256
        /// instead of SHA-1 as the hash function.
        ScramSha256 => "SCRAM-SHA-256",

        /// Same as [ScramSha256](#structfield.ScramSha256), with the addition
        /// of channel binding.
        ScramSha256Plus => "SCRAM-SHA-256-PLUS",

        /// Creates a temporary JID on login, which will be destroyed on
        /// disconnect.
        Anonymous => "ANONYMOUS",
    }
);

/// The first step of the SASL process, selecting the mechanism and sending
/// the first part of the handshake.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL, name = "auth")]
pub struct Auth {
    /// The mechanism used.
    #[xml(attribute)]
    pub mechanism: Mechanism,

    /// The content of the handshake.
    #[xml(text = Base64)]
    pub data: Vec<u8>,
}

/// In case the mechanism selected at the [auth](struct.Auth.html) step
/// requires a second step, the server sends this element with additional
/// data.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL, name = "challenge")]
pub struct Challenge {
    /// The challenge data.
    #[xml(text = Base64)]
    pub data: Vec<u8>,
}

/// In case the mechanism selected at the [auth](struct.Auth.html) step
/// requires a second step, this contains the client’s response to the
/// server’s [challenge](struct.Challenge.html).
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL, name = "response")]
pub struct Response {
    /// The response data.
    #[xml(text = Base64)]
    pub data: Vec<u8>,
}

/// Sent by the client at any point after [auth](struct.Auth.html) if it
/// wants to cancel the current authentication process.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL, name = "abort")]
pub struct Abort;

/// Sent by the server on SASL success.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL, name = "success")]
pub struct Success {
    /// Possible data sent on success.
    #[xml(text = Base64)]
    pub data: Vec<u8>,
}

/// List of possible failure conditions for SASL.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL)]
pub enum DefinedCondition {
    /// The client aborted the authentication with
    /// [abort](struct.Abort.html).
    #[xml(name = "aborted")]
    Aborted,

    /// The account the client is trying to authenticate against has been
    /// disabled.
    #[xml(name = "account-disabled")]
    AccountDisabled,

    /// The credentials for this account have expired.
    #[xml(name = "credentials-expired")]
    CredentialsExpired,

    /// You must enable StartTLS or use direct TLS before using this
    /// authentication mechanism.
    #[xml(name = "encryption-required")]
    EncryptionRequired,

    /// The base64 data sent by the client is invalid.
    #[xml(name = "incorrect-encoding")]
    IncorrectEncoding,

    /// The authzid provided by the client is invalid.
    #[xml(name = "invalid-authzid")]
    InvalidAuthzid,

    /// The client tried to use an invalid mechanism, or none.
    #[xml(name = "invalid-mechanism")]
    InvalidMechanism,

    /// The client sent a bad request.
    #[xml(name = "malformed-request")]
    MalformedRequest,

    /// The mechanism selected is weaker than what the server allows.
    #[xml(name = "mechanism-too-weak")]
    MechanismTooWeak,

    /// The credentials provided are invalid.
    #[xml(name = "not-authorized")]
    NotAuthorized,

    /// The server encountered an issue which may be fixed later, the
    /// client should retry at some point.
    #[xml(name = "temporary-auth-failure")]
    TemporaryAuthFailure,
}

type Lang = String;

/// Sent by the server on SASL failure.
#[derive(FromXml, AsXml, Debug, Clone)]
#[xml(namespace = ns::SASL, name = "failure")]
pub struct Failure {
    /// One of the allowed defined-conditions for SASL.
    #[xml(child)]
    pub defined_condition: DefinedCondition,

    /// A human-readable explanation for the failure.
    #[xml(extract(n = .., name = "text", fields(
        lang(type_ = Lang, default),
        text(type_ = String),
    )))]
    pub texts: BTreeMap<Lang, String>,
}

/// Enum which allows parsing/serialising any SASL element.
#[derive(FromXml, AsXml, Debug, Clone)]
#[xml()]
pub enum Nonza {
    /// Abortion of SASL transaction
    #[xml(transparent)]
    Abort(Abort),

    /// Failure of SASL transaction
    #[xml(transparent)]
    Failure(Failure),

    /// Success of SASL transaction
    #[xml(transparent)]
    Success(Success),

    /// Initiation of SASL transaction
    #[xml(transparent)]
    Auth(Auth),

    /// Challenge sent by the server to the client
    #[xml(transparent)]
    Challenge(Challenge),

    /// Response sent by the client to the server
    #[xml(transparent)]
    Response(Response),
}

#[cfg(test)]
mod tests {
    use super::*;

    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Mechanism, 1);
        assert_size!(Auth, 16);
        assert_size!(Challenge, 12);
        assert_size!(Response, 12);
        assert_size!(Abort, 0);
        assert_size!(Success, 12);
        assert_size!(DefinedCondition, 1);
        assert_size!(Failure, 16);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Mechanism, 1);
        assert_size!(Auth, 32);
        assert_size!(Challenge, 24);
        assert_size!(Response, 24);
        assert_size!(Abort, 0);
        assert_size!(Success, 24);
        assert_size!(DefinedCondition, 1);
        assert_size!(Failure, 32);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'/>"
            .parse()
            .unwrap();
        let auth = Auth::try_from(elem).unwrap();
        assert_eq!(auth.mechanism, Mechanism::Plain);
        assert!(auth.data.is_empty());
    }

    #[test]
    fn section_6_5_1() {
        let elem: Element =
            "<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><aborted/></failure>"
                .parse()
                .unwrap();
        let failure = Failure::try_from(elem).unwrap();
        assert_eq!(failure.defined_condition, DefinedCondition::Aborted);
        assert!(failure.texts.is_empty());
    }

    #[test]
    fn section_6_5_2() {
        let elem: Element = "<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'>
            <account-disabled/>
            <text xml:lang='en'>Call 212-555-1212 for assistance.</text>
        </failure>"
            .parse()
            .unwrap();
        let failure = Failure::try_from(elem).unwrap();
        assert_eq!(failure.defined_condition, DefinedCondition::AccountDisabled);
        assert_eq!(
            failure.texts["en"],
            String::from("Call 212-555-1212 for assistance.")
        );
    }

    /// Some servers apparently use a non-namespaced 'lang' attribute, which is invalid as not part
    /// of the schema.  This tests whether we can parse it when disabling validation.
    #[cfg(not(feature = "pedantic"))]
    #[test]
    fn invalid_failure_with_non_prefixed_text_lang() {
        let elem: Element = "<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'>
            <not-authorized xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>
            <text xmlns='urn:ietf:params:xml:ns:xmpp-sasl' lang='en'>Invalid username or password</text>
        </failure>"
            .parse()
            .unwrap();
        let failure = Failure::try_from(elem).unwrap();
        assert_eq!(failure.defined_condition, DefinedCondition::NotAuthorized);
        assert_eq!(
            failure.texts[""],
            String::from("Invalid username or password")
        );
    }
}
