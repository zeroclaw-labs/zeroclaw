// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::data_forms::DataForm;
use crate::iq::{IqGetPayload, IqResultPayload, IqSetPayload};
use crate::ns;

/// Entity requests registration fields from host.
#[derive(FromXml, AsXml, Debug, Clone)]
#[xml(namespace = ns::REGISTER, name = "query")]
pub struct FieldsQuery;

impl IqGetPayload for FieldsQuery {}

/// Entity requests cancellation of existing registration.
#[derive(FromXml, AsXml, Debug, Clone)]
#[xml(namespace = ns::REGISTER, name = "query")]
pub struct RemoveQuery {
    /// Whether to remove that registration.
    // TODO: we should be able to remove that flag and make this type a zero-sized type.
    #[xml(flag)]
    pub remove: bool,
}

impl IqSetPayload for RemoveQuery {}

/// Query for registering against a service, the legacy way.
#[derive(FromXml, AsXml, Debug, Clone, Default)]
#[xml(namespace = ns::REGISTER, name = "query")]
pub struct LegacyQuery {
    /// Whether this account is already registered
    #[xml(flag)]
    pub registered: bool,

    /// Instructions to be presented to entities implementing this legacy element.
    #[xml(extract(default, fields(text(type_ = String))))]
    pub instructions: Option<String>,

    /// Account name associated with the user
    #[xml(extract(default, fields(text(type_ = String))))]
    pub username: Option<String>,

    /// Familiar name of the user
    #[xml(extract(default, fields(text(type_ = String))))]
    pub nick: Option<String>,

    /// Password or secret for the user
    #[xml(extract(default, fields(text(type_ = String))))]
    pub password: Option<String>,

    /// Full name of the user
    #[xml(extract(default, fields(text(type_ = String))))]
    pub name: Option<String>,

    /// Given name of the user
    #[xml(extract(default, fields(text(type_ = String))))]
    pub first: Option<String>,

    /// Family name of the user
    #[xml(extract(default, fields(text(type_ = String))))]
    pub last: Option<String>,

    /// Email address of the user
    #[xml(extract(default, fields(text(type_ = String))))]
    pub email: Option<String>,

    /// Street portion of a physical or mailing address
    #[xml(extract(default, fields(text(type_ = String))))]
    pub address: Option<String>,

    /// Locality portion of a physical or mailing address
    #[xml(extract(default, fields(text(type_ = String))))]
    pub city: Option<String>,

    /// Region portion of a physical or mailing address
    #[xml(extract(default, fields(text(type_ = String))))]
    pub state: Option<String>,

    /// Postal code portion of a physical or mailing address
    #[xml(extract(default, fields(text(type_ = String))))]
    pub zip: Option<String>,

    /// Telephone number of the user
    #[xml(extract(default, fields(text(type_ = String))))]
    pub phone: Option<String>,

    /// URL to web page describing the user
    #[xml(extract(default, fields(text(type_ = String))))]
    pub url: Option<String>,

    /// Some date (e.g., birth date, hire date, sign-up date)
    #[xml(extract(default, fields(text(type_ = String))))]
    pub date: Option<String>,

    /// Free-form text field (obsolete)
    #[xml(extract(default, fields(text(type_ = String))))]
    pub misc: Option<String>,

    /// Free-form text field (obsolete)
    #[xml(extract(default, fields(text(type_ = String))))]
    pub text: Option<String>,

    /// Session key for transaction (obsolete)
    #[xml(extract(default, fields(text(type_ = String))))]
    pub key: Option<String>,
}

impl IqSetPayload for LegacyQuery {}
impl IqResultPayload for LegacyQuery {}

/// Query for registering against a service.
#[derive(FromXml, AsXml, Debug, Clone)]
#[xml(namespace = ns::REGISTER, name = "query")]
pub struct FormQuery {
    /// Legacy instructions fallback for entities which don’t understand the form.
    #[xml(extract(default, fields(text(type_ = String))))]
    pub instructions: Option<String>,

    /// A data form the user must fill before being allowed to register.
    #[xml(child)]
    pub form: DataForm,
    // Not yet implemented.
    //pub oob: Option<Oob>,
}

impl IqSetPayload for FormQuery {}
impl IqResultPayload for FormQuery {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_forms::DataFormType;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(RemoveQuery, 1);
        assert_size!(LegacyQuery, 220);
        assert_size!(FormQuery, 52);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(RemoveQuery, 1);
        assert_size!(LegacyQuery, 440);
        assert_size!(FormQuery, 104);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<query xmlns='jabber:iq:register'/>".parse().unwrap();
        FieldsQuery::try_from(elem).unwrap();

        let elem: Element = "<query xmlns='jabber:iq:register'><remove/></query>"
            .parse()
            .unwrap();
        let query = RemoveQuery::try_from(elem).unwrap();
        assert_eq!(query.remove, true);

        let elem: Element = "<query xmlns='jabber:iq:register'/>".parse().unwrap();
        let query = LegacyQuery::try_from(elem).unwrap();
        assert_eq!(query.first, None);

        let elem: Element =
            "<query xmlns='jabber:iq:register'><x xmlns='jabber:x:data' type='submit'/></query>"
                .parse()
                .unwrap();
        let query = FormQuery::try_from(elem).unwrap();
        assert!(query.instructions.is_none());
        assert!(query.form.fields.is_empty());
    }

    #[test]
    fn test_ex2() {
        let elem: Element = r#"<query xmlns='jabber:iq:register'>
  <instructions>
    Choose a username and password for use with this service.
    Please also provide your email address.
  </instructions>
  <username/>
  <password/>
  <email/>
</query>
"#
        .parse()
        .unwrap();
        let query = LegacyQuery::try_from(elem).unwrap();
        assert_eq!(query.registered, false);
        assert_eq!(query.instructions.unwrap(), "\n    Choose a username and password for use with this service.\n    Please also provide your email address.\n  ");
        assert_eq!(query.username.unwrap(), "");
        assert_eq!(query.password.unwrap(), "");
        assert_eq!(query.email.unwrap(), "");
        assert_eq!(query.name, None);

        // FIXME: HashMap doesn’t keep the order right.
        //let elem2 = query.into();
        //assert_eq!(elem, elem2);
    }

    #[test]
    fn test_ex9() {
        let elem: Element = "<query xmlns='jabber:iq:register'><instructions>Use the enclosed form to register. If your Jabber client does not support Data Forms, visit http://www.shakespeare.lit/contests.php</instructions><x xmlns='jabber:x:data' type='form'><title>Contest Registration</title><instructions>Please provide the following information to sign up for our special contests!</instructions><field type='hidden' var='FORM_TYPE'><value>jabber:iq:register</value></field><field label='Given Name' var='first'><required/></field><field label='Family Name' var='last'><required/></field><field label='Email Address' var='email'><required/></field><field type='list-single' label='Gender' var='x-gender'><option label='Male'><value>M</value></option><option label='Female'><value>F</value></option></field></x></query>"
        .parse()
        .unwrap();
        let elem1 = elem.clone();
        let query = FormQuery::try_from(elem).unwrap();
        assert!(query.instructions.is_some());
        let form = query.form.clone();
        assert_eq!(
            form.instructions.unwrap(),
            "Please provide the following information to sign up for our special contests!"
        );
        assert_eq!(form.type_, DataFormType::Form);
        let elem2 = query.into();
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn test_ex10() {
        let elem: Element = "<query xmlns='jabber:iq:register'><x xmlns='jabber:x:data' type='submit'><field type='hidden' var='FORM_TYPE'><value>jabber:iq:register</value></field><field label='Given Name' var='first'><value>Juliet</value></field><field label='Family Name' var='last'><value>Capulet</value></field><field label='Email Address' var='email'><value>juliet@capulet.com</value></field><field type='list-single' label='Gender' var='x-gender'><value>F</value></field></x></query>"
        .parse()
        .unwrap();
        let elem1 = elem.clone();
        let query = FormQuery::try_from(elem).unwrap();
        let elem2 = query.into();
        assert_eq!(elem1, elem2);
    }
}
