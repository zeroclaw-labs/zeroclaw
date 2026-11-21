// Copyright (c) 2024 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{text::Base64, AsXml, FromXml};

use crate::bind2;
use crate::ns;
use crate::sm::StreamManagement;
use jid::Jid;
use minidom::Element;

/// Server advertisement for supported auth mechanisms
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL2, name = "authentication")]
pub struct Authentication {
    /// Plaintext names of supported auth mechanisms
    #[xml(extract(n = .., name = "mechanism", fields(text(type_ = String))))]
    pub mechanisms: Vec<String>,

    /// Additional auth information provided by server
    #[xml(child(default))]
    pub inline: Option<InlineFeatures>,
}

/// Additional auth information provided by server
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL2, name = "inline")]
pub struct InlineFeatures {
    /// Bind 2 inline feature
    #[xml(child(default))]
    pub bind2: Option<bind2::BindFeature>,

    /// Stream management inline feature
    #[xml(child(default))]
    pub sm: Option<StreamManagement>,

    /// Additional inline features
    #[xml(element(n = ..))]
    pub payloads: Vec<Element>,
}

/// Client aborts the connection.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL2, name = "abort")]
pub struct Abort {
    /// Plaintext reason for aborting
    #[xml(extract(default, fields(text(type_ = String))))]
    pub text: Option<String>,

    /// Extra untyped payloads
    #[xml(element(n = ..))]
    pub payloads: Vec<Element>,
}

/// Optional client software information
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL2, name = "user-agent")]
pub struct UserAgent {
    /// Random, unique identifier for the client
    #[xml(attribute)]
    pub id: uuid::Uuid,

    /// Name of the client software
    #[xml(extract(default, fields(text(type_ = String))))]
    pub software: Option<String>,

    /// Name of the client device (eg. phone/laptop)
    #[xml(extract(default, fields(text(type_ = String))))]
    pub device: Option<String>,
}

/// Client authentication request
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL2, name = "authenticate")]
pub struct Authenticate {
    /// Chosen SASL mechanism
    #[xml(attribute)]
    pub mechanism: String,

    /// SASL response
    #[xml(extract(default, name = "initial-response", fields(text = Base64)))]
    pub initial_response: Option<Vec<u8>>,

    /// Information about client software
    #[xml(child)]
    pub user_agent: UserAgent,

    /// Extra untyped payloads
    #[xml(element(n = ..))]
    pub payloads: Vec<Element>,
}

/// SASL challenge
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL2, name = "challenge")]
pub struct Challenge {
    /// SASL challenge data
    #[xml(text = Base64)]
    pub sasl_data: Vec<u8>,
}

/// SASL response
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL2, name = "response")]
pub struct Response {
    /// SASL challenge data
    #[xml(text = Base64)]
    pub sasl_data: Vec<u8>,
}

/// Authentication was successful
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL2, name = "success")]
pub struct Success {
    /// Additional SASL data
    #[xml(extract(default, name = "additional-data", fields(text = Base64)))]
    pub additional_data: Option<Vec<u8>>,

    /// Identity assigned by the server
    #[xml(extract(name = "authorization-identifier", fields(text)))]
    pub authorization_identifier: Jid,

    /// Extra untyped payloads
    #[xml(element(n = ..))]
    pub payloads: Vec<Element>,
}

/// Authentication failed
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL2, name = "failure")]
pub struct Failure {
    /// Plaintext reason for failure
    #[xml(extract(default, fields(text(type_ = String))))]
    pub text: Option<String>,

    /// Extra untyped payloads
    #[xml(element(n = ..))]
    pub payloads: Vec<Element>,
}

/// Authentication requires extra steps (eg. 2FA)
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL2, name = "continue")]
pub struct Continue {
    /// Additional SASL data
    #[xml(extract(name = "additional-data", fields(text = Base64)))]
    pub additional_data: Vec<u8>,

    /// List of extra authentication steps.
    ///
    /// The client may choose any, but the server may respond with more Continue steps until all required
    /// steps are fulfilled.
    #[xml(extract(fields(extract(n = .., name = "task", fields(text(type_ = String))))))]
    pub tasks: Vec<String>,

    /// Plaintext reason for extra steps
    #[xml(extract(default, fields(text(type_ = String))))]
    pub text: Option<String>,
}

/// Client answers Continue extra step by selecting task.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL2, name = "next")]
pub struct Next {
    /// Task selected by client
    #[xml(attribute)]
    pub task: String,

    /// Extra untyped payloads
    #[xml(element(n = ..))]
    pub payloads: Vec<Element>,
}

/// Client/Server data exchange about selected task.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::SASL2, name = "task-data")]
pub struct TaskData {
    /// Extra untyped payloads
    #[xml(element(n = ..))]
    pub payloads: Vec<Element>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::prelude::*;
    use uuid::Uuid;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Authentication, 40);
        assert_size!(InlineFeatures, 28);
        assert_size!(Abort, 24);
        assert_size!(UserAgent, 40);
        assert_size!(Authenticate, 76);
        assert_size!(Challenge, 12);
        assert_size!(Response, 12);
        assert_size!(Success, 40);
        assert_size!(Failure, 24);
        assert_size!(Continue, 36);
        assert_size!(Next, 24);
        assert_size!(TaskData, 12);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Authentication, 80);
        assert_size!(InlineFeatures, 56);
        assert_size!(Abort, 48);
        assert_size!(UserAgent, 64);
        assert_size!(Authenticate, 136);
        assert_size!(Challenge, 24);
        assert_size!(Response, 24);
        assert_size!(Success, 80);
        assert_size!(Failure, 48);
        assert_size!(Continue, 72);
        assert_size!(Next, 48);
        assert_size!(TaskData, 24);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<authentication xmlns='urn:xmpp:sasl:2'><mechanism>SCRAM-SHA-1</mechanism></authentication>"
            .parse()
            .unwrap();
        let auth = Authentication::try_from(elem).unwrap();
        assert_eq!(auth.mechanisms.len(), 1);
        assert_eq!(auth.inline, None);

        let elem: Element = "<challenge xmlns='urn:xmpp:sasl:2'>AAAA</challenge>"
            .parse()
            .unwrap();
        let challenge = Challenge::try_from(elem).unwrap();
        assert_eq!(challenge.sasl_data, b"\0\0\0");

        let elem: Element = "<response xmlns='urn:xmpp:sasl:2'>YWJj</response>"
            .parse()
            .unwrap();
        let response = Response::try_from(elem).unwrap();
        assert_eq!(response.sasl_data, b"abc");
    }

    // XEP-0388 Example 2
    #[test]
    fn test_auth() {
        let elem: Element = r#"<authentication xmlns='urn:xmpp:sasl:2'>
            <mechanism>SCRAM-SHA-1</mechanism>
            <mechanism>SCRAM-SHA-1-PLUS</mechanism>
            <inline>
              <sm xmlns='urn:xmpp:sm:3'/>
              <bind xmlns='urn:xmpp:bind:0'/>
            </inline>
          </authentication>"#
            .parse()
            .unwrap();

        let auth = Authentication::try_from(elem).unwrap();

        assert_eq!(auth.mechanisms.len(), 2);
        let mut mech = auth.mechanisms.iter();
        assert_eq!(mech.next().unwrap(), "SCRAM-SHA-1");
        assert_eq!(mech.next().unwrap(), "SCRAM-SHA-1-PLUS");
        assert_eq!(mech.next(), None);

        let inline = auth.inline.unwrap();
        assert_eq!(inline.bind2.unwrap().inline_features.len(), 0);
        assert_eq!(inline.sm.unwrap(), StreamManagement { optional: false });
        assert_eq!(inline.payloads.len(), 0);
    }

    // XEP-0388 Example 3
    #[test]
    fn test_authenticate() {
        let elem: Element = r#"<authenticate xmlns='urn:xmpp:sasl:2' mechanism='SCRAM-SHA-1-PLUS'>
              <initial-response>cD10bHMtZXhwb3J0ZXIsLG49dXNlcixyPTEyQzRDRDVDLUUzOEUtNEE5OC04RjZELTE1QzM4RjUxQ0NDNg==</initial-response>
              <user-agent id='d4565fa7-4d72-4749-b3d3-740edbf87770'>
                <software>AwesomeXMPP</software>
                <device>Kiva's Phone</device>
              </user-agent>
            </authenticate>"#
                .parse()
                .unwrap();

        let auth = Authenticate::try_from(elem).unwrap();

        assert_eq!(auth.mechanism, "SCRAM-SHA-1-PLUS");
        assert_eq!(
            auth.initial_response.unwrap(),
            BASE64_STANDARD.decode("cD10bHMtZXhwb3J0ZXIsLG49dXNlcixyPTEyQzRDRDVDLUUzOEUtNEE5OC04RjZELTE1QzM4RjUxQ0NDNg==").unwrap()
        );

        assert_eq!(auth.user_agent.software.as_ref().unwrap(), "AwesomeXMPP");
        assert_eq!(auth.user_agent.device.as_ref().unwrap(), "Kiva's Phone");
    }

    // XEP-0388 Example 4
    #[test]
    fn test_authenticate_2() {
        let elem: Element = r#"<authenticate xmlns='urn:xmpp:sasl:2' mechanism='BLURDYBLOOP'>
              <initial-response>SSBzaG91bGQgbWFrZSB0aGlzIGEgY29tcGV0aXRpb24=</initial-response>
              <user-agent id='d4565fa7-4d72-4749-b3d3-740edbf87770'>
                <software>AwesomeXMPP</software>
                <device>Kiva's Phone</device>
              </user-agent>
              <bind xmlns='urn:xmpp:bind:example'/>
            </authenticate>"#
            .parse()
            .unwrap();

        let auth = Authenticate::try_from(elem).unwrap();

        assert_eq!(auth.mechanism, "BLURDYBLOOP");
        assert_eq!(
            auth.initial_response.unwrap(),
            BASE64_STANDARD
                .decode("SSBzaG91bGQgbWFrZSB0aGlzIGEgY29tcGV0aXRpb24=")
                .unwrap()
        );

        assert_eq!(auth.user_agent.software.as_ref().unwrap(), "AwesomeXMPP");
        assert_eq!(auth.user_agent.device.as_ref().unwrap(), "Kiva's Phone");

        assert_eq!(auth.payloads.len(), 1);
        let bind = auth.payloads.iter().next().unwrap();
        assert!(bind.is("bind", "urn:xmpp:bind:example"));
    }

    // XEP-0388 Example 5
    #[test]
    fn test_example_5() {
        let elem: Element = "<challenge xmlns='urn:xmpp:sasl:2'>cj0xMkM0Q0Q1Qy1FMzhFLTRBOTgtOEY2RC0xNUMzOEY1MUNDQzZhMDkxMTdhNi1hYzUwLTRmMmYtOTNmMS05Mzc5OWMyYmRkZjYscz1RU1hDUitRNnNlazhiZjkyLGk9NDA5Ng==</challenge>"
            .parse()
            .unwrap();
        let challenge = Challenge::try_from(elem).unwrap();
        assert_eq!(
            challenge.sasl_data,
            b"r=12C4CD5C-E38E-4A98-8F6D-15C38F51CCC6a09117a6-ac50-4f2f-93f1-93799c2bddf6,s=QSXCR+Q6sek8bf92,i=4096"
        );

        let elem: Element = "<response xmlns='urn:xmpp:sasl:2'>Yz1jRDEwYkhNdFpYaHdiM0owWlhJc0xNY29Rdk9kQkRlUGQ0T3N3bG1BV1YzZGcxYTFXaDF0WVBUQndWaWQxMFZVLHI9MTJDNENENUMtRTM4RS00QTk4LThGNkQtMTVDMzhGNTFDQ0M2YTA5MTE3YTYtYWM1MC00ZjJmLTkzZjEtOTM3OTljMmJkZGY2LHA9VUFwbzd4bzZQYTlKK1ZhZWpmei9kRzdCb21VPQ==</response>"
            .parse()
            .unwrap();
        let response = Response::try_from(elem).unwrap();
        assert_eq!(
            response.sasl_data,
            b"c=cD10bHMtZXhwb3J0ZXIsLMcoQvOdBDePd4OswlmAWV3dg1a1Wh1tYPTBwVid10VU,r=12C4CD5C-E38E-4A98-8F6D-15C38F51CCC6a09117a6-ac50-4f2f-93f1-93799c2bddf6,p=UApo7xo6Pa9J+Vaejfz/dG7BomU="
        );
    }

    // XEP-0388 Example 7 and 8
    #[test]
    fn test_example_7_8() {
        let elem: Element = r#"<success xmlns='urn:xmpp:sasl:2'>
              <additional-data>dj1tc1ZIcy9CeklPSERxWGVWSDdFbW1EdTlpZDg9</additional-data>
              <authorization-identifier>user@example.org</authorization-identifier>
           </success>"#
            .parse()
            .unwrap();

        let success = Success::try_from(elem).unwrap();

        assert_eq!(
            success.additional_data.unwrap(),
            BASE64_STANDARD
                .decode("dj1tc1ZIcy9CeklPSERxWGVWSDdFbW1EdTlpZDg9")
                .unwrap()
        );

        assert_eq!(
            success.authorization_identifier,
            Jid::new("user@example.org").unwrap()
        );

        let elem: Element = r#"<success xmlns='urn:xmpp:sasl:2'>
              <additional-data>ip/AeIOfZXKBV+fW2smE0GUB3I//nnrrLCYkt0Vj</additional-data>
              <authorization-identifier>juliet@montague.example/Balcony/a987dsh9a87sdh</authorization-identifier>
           </success>"#
            .parse()
            .unwrap();

        let success = Success::try_from(elem).unwrap();

        assert_eq!(
            success.additional_data.unwrap(),
            BASE64_STANDARD
                .decode("ip/AeIOfZXKBV+fW2smE0GUB3I//nnrrLCYkt0Vj")
                .unwrap()
        );

        assert_eq!(
            success.authorization_identifier,
            Jid::new("juliet@montague.example/Balcony/a987dsh9a87sdh").unwrap()
        );
    }

    // XEP-0388 Example 9
    #[test]
    fn example_success_stream_management() {
        let elem: Element = r#"<success xmlns='urn:xmpp:sasl:2'>
              <additional-data>SGFkIHlvdSBnb2luZywgdGhlcmUsIGRpZG4ndCBJPw==</additional-data>
              <authorization-identifier>juliet@montague.example</authorization-identifier>
              <resumed xmlns='urn:xmpp:sm:3' h='345' previd='124'/>
           </success>"#
            .parse()
            .unwrap();

        let success = Success::try_from(elem).unwrap();

        assert_eq!(
            success.additional_data.unwrap(),
            BASE64_STANDARD
                .decode("SGFkIHlvdSBnb2luZywgdGhlcmUsIGRpZG4ndCBJPw==")
                .unwrap()
        );

        assert_eq!(
            success.authorization_identifier,
            Jid::new("juliet@montague.example").unwrap()
        );

        assert_eq!(success.payloads.len(), 1);
        let resumed =
            crate::sm::Resumed::try_from(success.payloads.into_iter().next().unwrap()).unwrap();
        assert_eq!(resumed.h, 345);
        assert_eq!(resumed.previd, crate::sm::StreamId(String::from("124")));
    }

    // XEP-0388 Example 10
    #[test]
    fn example_failure() {
        let elem: Element = r#"<failure xmlns='urn:xmpp:sasl:2'>
  <aborted xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>
  <optional-application-specific xmlns='urn:something:else'/>
  <text>This is a terrible example.</text>
</failure>"#
            .parse()
            .unwrap();

        let failure = Failure::try_from(elem).unwrap();

        assert_eq!(failure.text.unwrap(), "This is a terrible example.");

        assert_eq!(failure.payloads.len(), 2);

        let mut payloads = failure.payloads.into_iter();

        let condition = crate::sasl::DefinedCondition::try_from(payloads.next().unwrap()).unwrap();
        assert_eq!(condition, crate::sasl::DefinedCondition::Aborted);

        assert!(payloads
            .next()
            .unwrap()
            .is("optional-application-specific", "urn:something:else"));
    }

    #[test]
    fn example_failure_no_text() {
        let elem: Element = r#"<failure xmlns='urn:xmpp:sasl:2'><aborted xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/></failure>"#
            .parse()
            .unwrap();

        let failure = Failure::try_from(elem).unwrap();

        assert_eq!(failure.text, None);

        assert_eq!(failure.payloads.len(), 1);

        let mut payloads = failure.payloads.into_iter();

        let condition = crate::sasl::DefinedCondition::try_from(payloads.next().unwrap()).unwrap();
        assert_eq!(condition, crate::sasl::DefinedCondition::Aborted);
    }

    // XEP-0388 Example 11
    #[test]
    fn example_11() {
        let elem: Element = r#"<continue xmlns='urn:xmpp:sasl:2'>
  <additional-data>SSdtIGJvcmVkIG5vdy4=</additional-data>
  <tasks>
    <task>HOTP-EXAMPLE</task>
    <task>TOTP-EXAMPLE</task>
  </tasks>
  <text>This account requires 2FA</text>
</continue>"#
            .parse()
            .unwrap();

        let cont = Continue::try_from(elem).unwrap();

        assert_eq!(
            cont.additional_data,
            BASE64_STANDARD.decode("SSdtIGJvcmVkIG5vdy4=").unwrap()
        );

        assert_eq!(cont.text.as_deref(), Some("This account requires 2FA"));

        assert_eq!(cont.tasks.len(), 2);
        let mut tasks = cont.tasks.into_iter();

        assert_eq!(tasks.next().unwrap(), "HOTP-EXAMPLE");

        assert_eq!(tasks.next().unwrap(), "TOTP-EXAMPLE");
    }

    // XEP-0388 Example 12
    #[test]
    fn test_fictional_totp() {
        let elem: Element = r#"<next xmlns='urn:xmpp:sasl:2' task='TOTP-EXAMPLE'>
  <totp xmlns="urn:totp:example">SSd2ZSBydW4gb3V0IG9mIGlkZWFzIGhlcmUu</totp>
</next>"#
            .parse()
            .unwrap();

        let next = Next::try_from(elem).unwrap();
        assert_eq!(next.task, "TOTP-EXAMPLE");

        let payload = next.payloads.into_iter().next().unwrap();
        assert!(payload.is("totp", "urn:totp:example"));
        assert_eq!(&payload.text(), "SSd2ZSBydW4gb3V0IG9mIGlkZWFzIGhlcmUu");

        let elem: Element = r#"<task-data xmlns='urn:xmpp:sasl:2'>
  <totp xmlns="urn:totp:example">94d27acffa2e99a42ba7786162a9e73e7ab17b9d</totp>
</task-data>"#
            .parse()
            .unwrap();

        let task_data = TaskData::try_from(elem).unwrap();
        let payload = task_data.payloads.into_iter().next().unwrap();
        assert!(payload.is("totp", "urn:totp:example"));
        assert_eq!(&payload.text(), "94d27acffa2e99a42ba7786162a9e73e7ab17b9d");

        let elem: Element = r#"<task-data xmlns='urn:xmpp:sasl:2'>
  <totp xmlns="urn:totp:example">OTRkMjdhY2ZmYTJlOTlhNDJiYTc3ODYxNjJhOWU3M2U3YWIxN2I5ZAo=</totp>
</task-data>"#
            .parse()
            .unwrap();

        let task_data = TaskData::try_from(elem).unwrap();
        let payload = task_data.payloads.into_iter().next().unwrap();
        assert!(payload.is("totp", "urn:totp:example"));
        assert_eq!(
            &payload.text(),
            "OTRkMjdhY2ZmYTJlOTlhNDJiYTc3ODYxNjJhOWU3M2U3YWIxN2I5ZAo="
        );

        let elem: Element = r#"<success xmlns='urn:xmpp:sasl:2'>
  <totp xmlns="urn:totp:example">SGFkIHlvdSBnb2luZywgdGhlcmUsIGRpZG4ndCBJPw==</totp>
  <authorization-identifier>juliet@montague.example</authorization-identifier>
</success>"#
            .parse()
            .unwrap();

        let success = Success::try_from(elem).unwrap();
        assert_eq!(success.additional_data, None);

        let payload = success.payloads.into_iter().next().unwrap();
        assert!(payload.is("totp", "urn:totp:example"));
        assert_eq!(
            &payload.text(),
            "SGFkIHlvdSBnb2luZywgdGhlcmUsIGRpZG4ndCBJPw=="
        );

        assert_eq!(
            success.authorization_identifier,
            Jid::new("juliet@montague.example").unwrap(),
        )
    }

    /// XEP-0388 Example 13
    #[test]
    fn example_13() {
        let elem: Element = r#"<authenticate xmlns='urn:xmpp:sasl:2' mechanism='PLAIN'>
  <initial-response>AGFsaWNlQGV4YW1wbGUub3JnCjM0NQ==</initial-response>
  <user-agent id='d4565fa7-4d72-4749-b3d3-740edbf87770'>
    <software>AwesomeXMPP</software>
    <device>Kiva's Phone</device>
  </user-agent>
</authenticate>"#
            .parse()
            .unwrap();

        let auth = Authenticate::try_from(elem).unwrap();

        assert_eq!(auth.mechanism, "PLAIN");
        assert_eq!(
            auth.initial_response.unwrap(),
            BASE64_STANDARD
                .decode("AGFsaWNlQGV4YW1wbGUub3JnCjM0NQ==")
                .unwrap()
        );

        assert_eq!(auth.payloads.len(), 0);

        let user_agent = auth.user_agent;
        assert_eq!(
            user_agent.id,
            "d4565fa7-4d72-4749-b3d3-740edbf87770"
                .parse::<Uuid>()
                .unwrap()
        );
        assert_eq!(user_agent.software.as_deref(), Some("AwesomeXMPP"));
        assert_eq!(user_agent.device.as_deref(), Some("Kiva's Phone"));

        let elem: Element = r#"<success xmlns='urn:xmpp:sasl:2'>
  <authorization-identifier>alice@example.org</authorization-identifier>
</success>"#
            .parse()
            .unwrap();

        let success = Success::try_from(elem).unwrap();
        assert_eq!(
            success.authorization_identifier,
            Jid::new("alice@example.org").unwrap()
        );
        assert_eq!(success.additional_data, None);
        assert_eq!(success.payloads.len(), 0);
    }

    // XEP-0388 Example 14
    #[test]
    fn example_14() {
        let elem: Element = r#"<authenticate xmlns='urn:xmpp:sasl:2' mechanism='CRAM-MD5'>
  <user-agent id='d4565fa7-4d72-4749-b3d3-740edbf87770'>
    <software>AwesomeXMPP</software>
    <device>Kiva's Phone</device>
  </user-agent>
</authenticate>"#
            .parse()
            .unwrap();

        let auth = Authenticate::try_from(elem).unwrap();

        assert_eq!(auth.mechanism, "CRAM-MD5");
        assert_eq!(auth.initial_response, None);
        assert_eq!(auth.payloads.len(), 0);

        let user_agent = auth.user_agent;
        assert_eq!(
            user_agent.id,
            "d4565fa7-4d72-4749-b3d3-740edbf87770"
                .parse::<Uuid>()
                .unwrap()
        );
        assert_eq!(user_agent.software.as_deref(), Some("AwesomeXMPP"));
        assert_eq!(user_agent.device.as_deref(), Some("Kiva's Phone"));

        let elem: Element = r#"<challenge xmlns='urn:xmpp:sasl:2'>PDE4OTYuNjk3MTcwOTUyQHBvc3RvZmZpY2UucmVzdG9uLm1jaS5uZXQ+</challenge>"#
        .parse()
        .unwrap();

        let challenge = Challenge::try_from(elem).unwrap();
        assert_eq!(
            challenge.sasl_data,
            BASE64_STANDARD
                .decode("PDE4OTYuNjk3MTcwOTUyQHBvc3RvZmZpY2UucmVzdG9uLm1jaS5uZXQ+")
                .unwrap()
        );

        let elem: Element = r#"<response xmlns='urn:xmpp:sasl:2'>dGltIGI5MTNhNjAyYzdlZGE3YTQ5NWI0ZTZlNzMzNGQzODkw</response>"#
        .parse()
        .unwrap();

        let response = Response::try_from(elem).unwrap();
        assert_eq!(
            response.sasl_data,
            BASE64_STANDARD
                .decode("dGltIGI5MTNhNjAyYzdlZGE3YTQ5NWI0ZTZlNzMzNGQzODkw")
                .unwrap()
        );

        let elem: Element = r#"<success xmlns='urn:xmpp:sasl:2'>
  <authorization-identifier>tim@example.org</authorization-identifier>
</success>
        "#
        .parse()
        .unwrap();

        let success = Success::try_from(elem).unwrap();
        assert_eq!(
            success.authorization_identifier,
            Jid::new("tim@example.org").unwrap()
        );
        assert_eq!(success.additional_data, None);
        assert_eq!(success.payloads.len(), 0);
    }

    // XEP-0388 Example 15
    #[test]
    fn example_15() {
        let elem: Element = r#"<authenticate xmlns='urn:xmpp:sasl:2' mechanism='BLURDYBLOOP'>
  <initial-response>SW5pdGlhbCBSZXNwb25zZQ==</initial-response>
  <user-agent id='d4565fa7-4d72-4749-b3d3-740edbf87770'>
    <software>AwesomeXMPP</software>
    <device>Kiva's Phone</device>
  </user-agent>
  <megabind xmlns='urn:example:megabind'>
    <resource>this-one-please</resource>
  </megabind>
</authenticate>"#
            .parse()
            .unwrap();

        let auth = Authenticate::try_from(elem).unwrap();
        assert_eq!(auth.mechanism, "BLURDYBLOOP");
        assert_eq!(
            auth.initial_response,
            Some(BASE64_STANDARD.decode("SW5pdGlhbCBSZXNwb25zZQ==").unwrap())
        );

        assert_eq!(
            auth.user_agent.id,
            "d4565fa7-4d72-4749-b3d3-740edbf87770"
                .parse::<Uuid>()
                .unwrap()
        );
        assert_eq!(auth.user_agent.software.as_deref(), Some("AwesomeXMPP"));
        assert_eq!(auth.user_agent.device.as_deref(), Some("Kiva's Phone"));

        assert_eq!(auth.payloads.len(), 1);
        let bind = auth.payloads.into_iter().next().unwrap();
        assert!(bind.is("megabind", "urn:example:megabind"));

        let mut bind_payloads = bind.children();
        let resource = bind_payloads.next().unwrap();
        assert_eq!(resource.name(), "resource");
        assert_eq!(&resource.text(), "this-one-please");
        assert_eq!(bind_payloads.next(), None);

        let elem: Element = r#"<challenge xmlns='urn:xmpp:sasl:2'>PDE4OTYuNjk3MTcwOTUyQHBvc3RvZmZpY2UucmVzdG9uLm1jaS5uZXQ+</challenge>"#
        .parse()
        .unwrap();
        let challenge = Challenge::try_from(elem).unwrap();
        assert_eq!(
            challenge.sasl_data,
            BASE64_STANDARD
                .decode("PDE4OTYuNjk3MTcwOTUyQHBvc3RvZmZpY2UucmVzdG9uLm1jaS5uZXQ+")
                .unwrap()
        );

        let elem: Element = r#"<response xmlns='urn:xmpp:sasl:2'>dGltIGI5MTNhNjAyYzdlZGE3YTQ5NWI0ZTZlNzMzNGQzODkw</response>"#
        .parse()
        .unwrap();
        let response = Response::try_from(elem).unwrap();
        assert_eq!(response.sasl_data, b"tim b913a602c7eda7a495b4e6e7334d3890");

        let elem: Element = r#"<continue xmlns='urn:xmpp:sasl:2'>
  <additional-data>QWRkaXRpb25hbCBEYXRh</additional-data>
  <tasks>
    <task>UNREALISTIC-2FA</task>
  </tasks>
</continue>"#
            .parse()
            .unwrap();
        let cont = Continue::try_from(elem).unwrap();
        assert_eq!(
            cont.additional_data,
            BASE64_STANDARD.decode("QWRkaXRpb25hbCBEYXRh").unwrap()
        );
        assert_eq!(cont.tasks.len(), 1);
        assert_eq!(cont.tasks.into_iter().next().unwrap(), "UNREALISTIC-2FA");

        let elem: Element = r#"<next xmlns='urn:xmpp:sasl:2' task='UNREALISTIC-2FA'>
  <parameters xmlns='urn:example:unrealistic2fa'>VW5yZWFsaXN0aWMgMkZBIElS</parameters>
</next>"#
            .parse()
            .unwrap();
        let next = Next::try_from(elem).unwrap();
        assert_eq!(next.payloads.len(), 1);
        let params = next.payloads.into_iter().next().unwrap();
        assert!(params.is("parameters", "urn:example:unrealistic2fa"));
        assert_eq!(&params.text(), "VW5yZWFsaXN0aWMgMkZBIElS");

        let elem: Element = r#"<task-data xmlns='urn:xmpp:sasl:2'>
  <question xmlns='urn:example:unrealistic2fa'>PDE4OTYuNjk3MTcwOTUyQHBvc3RvZmZpY2UucmVzdG9uLm1jaS5uZXQ+</question>
</task-data>"#
        .parse()
        .unwrap();
        let task_data = TaskData::try_from(elem).unwrap();
        assert_eq!(task_data.payloads.len(), 1);
        let question = task_data.payloads.into_iter().next().unwrap();
        assert!(question.is("question", "urn:example:unrealistic2fa"));
        assert_eq!(
            &question.text(),
            "PDE4OTYuNjk3MTcwOTUyQHBvc3RvZmZpY2UucmVzdG9uLm1jaS5uZXQ+"
        );

        let elem: Element = r#"<task-data xmlns='urn:xmpp:sasl:2'>
  <response xmlns='urn:example:unrealistic2fa'>dGltIGI5MTNhNjAyYzdlZGE3YTQ5NWI0ZTZlNzMzNGQzODkw</response>
</task-data>"#
        .parse()
        .unwrap();
        let task_data = TaskData::try_from(elem).unwrap();
        assert_eq!(task_data.payloads.len(), 1);
        let response = task_data.payloads.into_iter().next().unwrap();
        assert!(response.is("response", "urn:example:unrealistic2fa"));
        assert_eq!(
            &response.text(),
            "dGltIGI5MTNhNjAyYzdlZGE3YTQ5NWI0ZTZlNzMzNGQzODkw"
        );

        let elem: Element = r#"<success xmlns='urn:xmpp:sasl:2'>
  <result xmlns='urn:example:unrealistic2fa'>VW5yZWFsaXN0aWMgMkZBIG11dHVhbCBhdXRoIGRhdGE=</result>
  <authorization-identifier>alice@example.org/this-one-please</authorization-identifier>
</success>"#
            .parse()
            .unwrap();
        let success = Success::try_from(elem).unwrap();
        assert_eq!(
            success.authorization_identifier,
            Jid::new("alice@example.org/this-one-please").unwrap()
        );

        assert_eq!(success.payloads.len(), 1);
        let res = success.payloads.into_iter().next().unwrap();
        assert!(res.is("result", "urn:example:unrealistic2fa"));
        assert_eq!(&res.text(), "VW5yZWFsaXN0aWMgMkZBIG11dHVhbCBhdXRoIGRhdGE=");
    }
}
