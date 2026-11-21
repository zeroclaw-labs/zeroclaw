// Copyright (C) 2019 Maxime “pep” Buquet <pep@bouah.net>
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use crate::data_forms::{DataForm, DataFormType, Field, FieldType};
use crate::ns;
use xso::error::Error;

/// Structure representing a `http://jabber.org/network/serverinfo` form type.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ServerInfo {
    /// Abuse addresses
    pub abuse: Vec<String>,

    /// Admin addresses
    pub admin: Vec<String>,

    /// Feedback addresses
    pub feedback: Vec<String>,

    /// Sales addresses
    pub sales: Vec<String>,

    /// Security addresses
    pub security: Vec<String>,

    /// Support addresses
    pub support: Vec<String>,
}

impl TryFrom<DataForm> for ServerInfo {
    type Error = Error;

    fn try_from(form: DataForm) -> Result<ServerInfo, Error> {
        if form.type_ != DataFormType::Result_ {
            return Err(Error::Other("Wrong type of form."));
        }
        if form.form_type() != Some(ns::SERVER_INFO) {
            return Err(Error::Other("Wrong FORM_TYPE for form."));
        }
        let mut server_info = ServerInfo::default();
        for field in form.fields {
            if field.var.as_deref().unwrap_or("") == "FORM_TYPE" {
                continue;
            }
            if field.type_ != FieldType::ListMulti {
                return Err(Error::Other("Field is not of the required type."));
            }
            if field.var.as_deref() == Some("abuse-addresses") {
                server_info.abuse = field.values;
            } else if field.var.as_deref() == Some("admin-addresses") {
                server_info.admin = field.values;
            } else if field.var.as_deref() == Some("feedback-addresses") {
                server_info.feedback = field.values;
            } else if field.var.as_deref() == Some("sales-addresses") {
                server_info.sales = field.values;
            } else if field.var.as_deref() == Some("security-addresses") {
                server_info.security = field.values;
            } else if field.var.as_deref() == Some("support-addresses") {
                server_info.support = field.values;
            } else {
                return Err(Error::Other("Unknown form field var."));
            }
        }

        Ok(server_info)
    }
}

impl From<ServerInfo> for DataForm {
    fn from(server_info: ServerInfo) -> DataForm {
        let mut form = DataForm {
            type_: DataFormType::Result_,
            title: None,
            instructions: None,
            fields: vec![
                generate_address_field("abuse-addresses", server_info.abuse),
                generate_address_field("admin-addresses", server_info.admin),
                generate_address_field("feedback-addresses", server_info.feedback),
                generate_address_field("sales-addresses", server_info.sales),
                generate_address_field("security-addresses", server_info.security),
                generate_address_field("support-addresses", server_info.support),
            ],
        };
        form.set_form_type(ns::SERVER_INFO.to_owned());
        form
    }
}

/// Generate `Field` for addresses
pub fn generate_address_field<S: Into<String>>(var: S, values: Vec<String>) -> Field {
    Field {
        var: Some(var.into()),
        type_: FieldType::ListMulti,
        label: None,
        required: false,
        desc: None,
        options: vec![],
        values,
        media: vec![],
        validate: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(ServerInfo, 72);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(ServerInfo, 144);
    }

    #[test]
    fn test_simple() {
        let form = DataForm::new(
            DataFormType::Result_,
            ns::SERVER_INFO,
            vec![
                Field::new("abuse-addresses", FieldType::ListMulti),
                Field::new("admin-addresses", FieldType::ListMulti)
                    .with_value("xmpp:admin@foo.bar")
                    .with_value("https://foo.bar/chat/")
                    .with_value("mailto:admin@foo.bar"),
                Field::new("feedback-addresses", FieldType::ListMulti),
                Field::new("sales-addresses", FieldType::ListMulti),
                Field::new("security-addresses", FieldType::ListMulti)
                    .with_value("xmpp:security@foo.bar")
                    .with_value("mailto:security@foo.bar"),
                Field::new("support-addresses", FieldType::ListMulti)
                    .with_value("mailto:support@foo.bar"),
            ],
        );

        let server_info = ServerInfo {
            abuse: vec![],
            admin: vec![
                String::from("xmpp:admin@foo.bar"),
                String::from("https://foo.bar/chat/"),
                String::from("mailto:admin@foo.bar"),
            ],
            feedback: vec![],
            sales: vec![],
            security: vec![
                String::from("xmpp:security@foo.bar"),
                String::from("mailto:security@foo.bar"),
            ],
            support: vec![String::from("mailto:support@foo.bar")],
        };

        // assert_eq!(DataForm::from(server_info), form);
        assert_eq!(ServerInfo::try_from(form).unwrap(), server_info);
    }
}
