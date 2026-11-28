// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{error::Error, AsXml, FromXml};

use crate::data_forms_validate::Validate;
use crate::media_element::MediaElement;
use crate::ns;

/// Represents one of the possible values for a list- field.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::DATA_FORMS, name = "option")]
pub struct Option_ {
    /// The optional label to be displayed to the user for this option.
    #[xml(attribute(default))]
    pub label: Option<String>,

    /// The value returned to the server when selecting this option.
    #[xml(extract(fields(text)))]
    pub value: String,
}

generate_attribute!(
    /// The type of a [field](struct.Field.html) element.
    FieldType, "type", {
        /// This field can only take the values "0" or "false" for a false
        /// value, and "1" or "true" for a true value.
        Boolean => "boolean",

        /// This field describes data, it must not be sent back to the
        /// requester.
        Fixed => "fixed",

        /// This field is hidden, it should not be displayed to the user but
        /// should be sent back to the requester.
        Hidden => "hidden",

        /// This field accepts one or more [JIDs](../../jid/struct.Jid.html).
        /// A client may want to let the user autocomplete them based on their
        /// contacts list for instance.
        JidMulti => "jid-multi",

        /// This field accepts one [JID](../../jid/struct.Jid.html).  A client
        /// may want to let the user autocomplete it based on their contacts
        /// list for instance.
        JidSingle => "jid-single",

        /// This field accepts one or more values from the list provided as
        /// [options](struct.Option_.html).
        ListMulti => "list-multi",

        /// This field accepts one value from the list provided as
        /// [options](struct.Option_.html).
        ListSingle => "list-single",

        /// This field accepts one or more free form text lines.
        TextMulti => "text-multi",

        /// This field accepts one free form password, a client should hide it
        /// in its user interface.
        TextPrivate => "text-private",

        /// This field accepts one free form text line.
        TextSingle => "text-single",
    }, Default = TextSingle
);

fn validate_field(field: &mut Field) -> Result<(), Error> {
    if field.type_ != FieldType::Fixed && field.var.is_none() {
        return Err(Error::Other("Required attribute 'var' missing."));
    }

    if !field.is_list() && field.options.len() > 0 {
        return Err(Error::Other("Option element found in non-list field."));
    }

    Ok(())
}

/// Represents a field in a [data form](struct.DataForm.html).
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::DATA_FORMS, name = "field", deserialize_callback = validate_field)]
pub struct Field {
    /// The unique identifier for this field, in the form.
    #[xml(attribute(default))]
    pub var: Option<String>,

    /// The type of this field.
    #[xml(attribute(name = "type", default))]
    pub type_: FieldType,

    /// The label to be possibly displayed to the user for this field.
    #[xml(attribute(default))]
    pub label: Option<String>,

    /// The form will be rejected if this field isn’t present.
    #[xml(flag)]
    pub required: bool,

    /// The natural-language description of the field, intended for presentation in a user-agent
    #[xml(extract(default, fields(text(type_ = String))))]
    pub desc: Option<String>,

    /// A list of allowed values.
    #[xml(child(n = ..))]
    pub options: Vec<Option_>,

    /// The values provided for this field.
    #[xml(extract(n = .., name = "value", fields(text(type_ = String))))]
    pub values: Vec<String>,

    /// A list of media related to this field.
    #[xml(child(n = ..))]
    pub media: Vec<MediaElement>,

    /// Validation rules for this field.
    #[xml(child(default))]
    pub validate: Option<Validate>,
}

impl Field {
    /// Create a new Field, of the given var and type.
    pub fn new(var: &str, type_: FieldType) -> Field {
        Field {
            var: Some(String::from(var)),
            type_,
            label: None,
            required: false,
            desc: None,
            options: Vec::new(),
            media: Vec::new(),
            values: Vec::new(),
            validate: None,
        }
    }

    /// Set only one value in this Field.
    pub fn with_value(mut self, value: &str) -> Field {
        self.values.push(String::from(value));
        self
    }

    /// Create a text-single Field with the given var and unique value.
    pub fn text_single(var: &str, value: &str) -> Field {
        Field::new(var, FieldType::TextSingle).with_value(value)
    }

    fn is_list(&self) -> bool {
        self.type_ == FieldType::ListSingle || self.type_ == FieldType::ListMulti
    }

    /// Return true if this field is a valid form type specifier as per
    /// [XEP-0068](https://xmpp.org/extensions/xep-0068.html).
    ///
    /// This function requires knowledge of the form's type attribute as the
    /// criteria differ slightly among form types.
    pub fn is_form_type(&self, ty: &DataFormType) -> bool {
        // 1. A field must have the var FORM_TYPE
        if self.var.as_deref() != Some("FORM_TYPE") {
            return false;
        }

        match ty {
            // https://xmpp.org/extensions/xep-0068.html#usecases-incorrect
            // > If the FORM_TYPE field is not hidden in a form with
            // > type="form" or type="result", it MUST be ignored as a context
            // > indicator.
            DataFormType::Form | DataFormType::Result_ => self.type_ == FieldType::Hidden,

            // https://xmpp.org/extensions/xep-0068.html#impl
            // > Data forms with the type "submit" are free to omit any
            // > explicit field type declaration (as per Data Forms (XEP-0004)
            // > § 3.2), as the type is implied by the corresponding
            // > "form"-type data form. As consequence, implementations MUST
            // > treat a FORM_TYPE field without an explicit type attribute,
            // > in data forms of type "submit", as the FORM_TYPE field with
            // > the special meaning defined herein.
            DataFormType::Submit => matches!(self.type_, FieldType::Hidden | FieldType::TextSingle),

            // XEP-0068 does not explicitly mention cancel type forms.
            // However, XEP-0004 states:
            // > a data form of type "cancel" SHOULD NOT contain any <field/>
            // > elements.
            // thus we ignore those.
            DataFormType::Cancel => false,
        }
    }
}

generate_attribute!(
    /// Represents the type of a [data form](struct.DataForm.html).
    DataFormType, "type", {
        /// This is a cancel request for a prior type="form" data form.
        Cancel => "cancel",

        /// This is a request for the recipient to fill this form and send it
        /// back as type="submit".
        Form => "form",

        /// This is a result form, which contains what the requester asked for.
        Result_ => "result",

        /// This is a complete response to a form received before.
        Submit => "submit",
    }
);

/// This is a form to be sent to another entity for filling.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::DATA_FORMS, name = "x", deserialize_callback = patch_form)]
pub struct DataForm {
    /// The type of this form, telling the other party which action to execute.
    #[xml(attribute = "type")]
    pub type_: DataFormType,

    /// The title of this form.
    #[xml(extract(fields(text(type_ = String)), default))]
    pub title: Option<String>,

    /// The instructions given with this form.
    #[xml(extract(fields(text(type_ = String)), default))]
    pub instructions: Option<String>,

    /// A list of fields comprising this form.
    #[xml(child(n = ..))]
    pub fields: Vec<Field>,
}

fn patch_form(form: &mut DataForm) -> Result<(), Error> {
    // Sort the FORM_TYPE field, if any, to the first position.
    let mut form_type_index = None;
    for (i, field) in form.fields.iter().enumerate() {
        if field.is_form_type(&form.type_) {
            if form_type_index.is_some() {
                return Err(Error::Other("More than one FORM_TYPE in a data form."));
            }

            if field.values.len() != 1 {
                return Err(Error::Other("Wrong number of values in FORM_TYPE."));
            }

            form_type_index = Some(i);
        }
    }

    if let Some(index) = form_type_index {
        let field = form.fields.remove(index);
        form.fields.insert(0, field);
    }
    Ok(())
}

impl DataForm {
    /// Create a new DataForm.
    pub fn new(type_: DataFormType, form_type: &str, fields: Vec<Field>) -> DataForm {
        let mut form = DataForm {
            type_,
            title: None,
            instructions: None,
            fields,
        };
        form.set_form_type(form_type.to_owned());
        form
    }

    /// Return the value of the `FORM_TYPE` field, if any.
    ///
    /// An easy accessor for the FORM_TYPE of this form, see
    /// [XEP-0068](https://xmpp.org/extensions/xep-0068.html) for more
    /// information.
    pub fn form_type(&self) -> Option<&str> {
        for field in self.fields.iter() {
            if field.is_form_type(&self.type_) {
                return field.values.first().map(|x| x.as_str());
            }
        }
        None
    }

    /// Create or modify the `FORM_TYPE` field.
    ///
    /// If the form has no `FORM_TYPE` field, this function creates a new
    /// field and inserts it at the beginning of the field list. Otherwise, it
    /// returns a mutable reference to the existing field.
    ///
    /// # Panics
    ///
    /// If the type of the form is [`DataFormType::Cancel`], this function
    /// panics. Such forms should not have any fields and thus no form type.
    pub fn set_form_type(&mut self, ty: String) -> &mut Field {
        if self.type_ == DataFormType::Cancel {
            panic!("cannot add FORM_TYPE field to type='cancel' form");
        }

        // Awkward index enum to work around borrowck limitations.
        let mut index = None;
        for (i, field) in self.fields.iter().enumerate() {
            if field.is_form_type(&self.type_) {
                index = Some(i);
                break;
            }
        }

        let field = if let Some(index) = index {
            &mut self.fields[index]
        } else {
            let field = Field::new("FORM_TYPE", FieldType::Hidden);
            assert!(field.is_form_type(&self.type_));
            self.fields.insert(0, field);
            &mut self.fields[0]
        };
        field.values.clear();
        field.values.push(ty);
        field
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_forms_validate::{Datatype, Validate};
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Option_, 24);
        assert_size!(FieldType, 1);
        assert_size!(Field, 140);
        assert_size!(DataFormType, 1);
        assert_size!(DataForm, 40);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Option_, 48);
        assert_size!(FieldType, 1);
        assert_size!(Field, 264);
        assert_size!(DataFormType, 1);
        assert_size!(DataForm, 80);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<x xmlns='jabber:x:data' type='result'/>".parse().unwrap();
        let form = DataForm::try_from(elem).unwrap();
        assert_eq!(form.type_, DataFormType::Result_);
        assert!(form.form_type().is_none());
        assert!(form.fields.is_empty());
    }

    #[test]
    fn test_missing_var() {
        let elem: Element =
            "<x xmlns='jabber:x:data' type='form'><field type='text-single' label='The name of your bot'/></x>"
                .parse()
                .unwrap();
        let error = DataForm::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Required attribute 'var' missing.");
    }

    #[test]
    fn test_fixed_field() {
        let elem: Element =
            "<x xmlns='jabber:x:data' type='form'><field type='fixed'><value>Section 1: Bot Info</value></field></x>"
                .parse()
                .unwrap();
        let form = DataForm::try_from(elem).unwrap();
        assert_eq!(form.type_, DataFormType::Form);
        assert!(form.form_type().is_none());
        assert_eq!(
            form.fields,
            vec![Field {
                var: None,
                type_: FieldType::Fixed,
                label: None,
                required: false,
                desc: None,
                options: vec![],
                values: vec!["Section 1: Bot Info".to_string()],
                media: vec![],
                validate: None,
            }]
        );
    }

    #[test]
    fn test_desc() {
        let elem: Element =
            "<x xmlns='jabber:x:data' type='form'><field type='jid-multi' label='People to invite' var='invitelist'><desc>Tell all your friends about your new bot!</desc></field></x>"
                .parse()
                .unwrap();
        let form = DataForm::try_from(elem).unwrap();
        assert_eq!(form.type_, DataFormType::Form);
        assert!(form.form_type().is_none());
        assert_eq!(
            form.fields,
            vec![Field {
                var: Some("invitelist".to_string()),
                type_: FieldType::JidMulti,
                label: Some("People to invite".to_string()),
                required: false,
                desc: Some("Tell all your friends about your new bot!".to_string()),
                options: vec![],
                values: vec![],
                media: vec![],
                validate: None,
            }]
        );
    }

    #[test]
    fn test_validate() {
        let elem: Element = r#"<x xmlns='jabber:x:data' type='form'>
                <field var='evt.date' type='text-single' label='Event Date/Time'>
                    <validate xmlns='http://jabber.org/protocol/xdata-validate' datatype='xs:dateTime'/>
                    <value>2003-10-06T11:22:00-07:00</value>
                </field>
            </x>"#
            .parse()
            .unwrap();
        let form = DataForm::try_from(elem).unwrap();
        assert_eq!(form.type_, DataFormType::Form);
        assert!(form.form_type().is_none());
        assert_eq!(
            form.fields,
            vec![Field {
                var: Some("evt.date".to_string()),
                type_: FieldType::TextSingle,
                label: Some("Event Date/Time".to_string()),
                required: false,
                desc: None,
                options: vec![],
                values: vec!["2003-10-06T11:22:00-07:00".to_string()],
                media: vec![],
                validate: Some(Validate {
                    datatype: Some(Datatype::DateTime),
                    method: None,
                    list_range: None,
                }),
            }]
        );
    }

    #[test]
    fn test_invalid_field() {
        let elem: Element = "<field xmlns='jabber:x:data' type='text-single' var='foo'><option><value>foo</value></option></field>".parse().unwrap();
        let error = Field::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Option element found in non-list field.");
    }

    #[test]
    fn test_invalid() {
        let elem: Element = "<x xmlns='jabber:x:data'/>".parse().unwrap();
        let error = DataForm::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Required attribute field 'type_' on DataForm element missing."
        );

        let elem: Element = "<x xmlns='jabber:x:data' type='coucou'/>".parse().unwrap();
        let error = DataForm::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::TextParseError(string)) => string,
            other => panic!("unexpected result: {:?}", other),
        };
        assert_eq!(message.to_string(), "Unknown value for 'type' attribute.");
    }

    #[test]
    #[cfg(feature = "pedantic")]
    fn test_wrong_child() {
        let elem: Element = "<x xmlns='jabber:x:data' type='cancel'><coucou/></x>"
            .parse()
            .unwrap();
        let error = DataForm::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in DataForm element.");
    }

    #[test]
    fn option() {
        let elem: Element =
            "<option xmlns='jabber:x:data' label='Coucou !'><value>coucou</value></option>"
                .parse()
                .unwrap();
        let option = Option_::try_from(elem).unwrap();
        assert_eq!(&option.label.unwrap(), "Coucou !");
        assert_eq!(&option.value, "coucou");

        let elem: Element = "<option xmlns='jabber:x:data' label='Coucou !'/>"
            .parse()
            .unwrap();
        let error = Option_::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Missing child field 'value' in Option_ element.");

        let elem: Element = "<option xmlns='jabber:x:data' label='Coucou !'><value>coucou</value><value>error</value></option>".parse().unwrap();
        let error = Option_::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(
            message,
            "Option_ element must not have more than one child in field 'value'."
        );
    }

    #[test]
    fn test_ignore_form_type_field_if_field_type_mismatches_in_form_typed_forms() {
        // https://xmpp.org/extensions/xep-0068.html#usecases-incorrect
        // […] it MUST be ignored as a context indicator
        let elem: Element = "<x xmlns='jabber:x:data' type='form'><field var='FORM_TYPE' type='text-single'><value>foo</value></field></x>".parse().unwrap();
        match DataForm::try_from(elem) {
            Ok(form) => {
                match form.form_type() {
                    None => (),
                    other => panic!("unexpected extracted form type: {:?}", other),
                };
            }
            other => panic!("unexpected result: {:?}", other),
        }
    }

    #[test]
    fn test_ignore_form_type_field_if_field_type_mismatches_in_result_typed_forms() {
        // https://xmpp.org/extensions/xep-0068.html#usecases-incorrect
        // […] it MUST be ignored as a context indicator
        let elem: Element = "<x xmlns='jabber:x:data' type='result'><field var='FORM_TYPE' type='text-single'><value>foo</value></field></x>".parse().unwrap();
        match DataForm::try_from(elem) {
            Ok(form) => {
                match form.form_type() {
                    None => (),
                    other => panic!("unexpected extracted form type: {:?}", other),
                };
            }
            other => panic!("unexpected result: {:?}", other),
        }
    }

    #[test]
    fn test_accept_form_type_field_without_type_attribute_in_submit_typed_forms() {
        let elem: Element = "<x xmlns='jabber:x:data' type='submit'><field var='FORM_TYPE'><value>foo</value></field></x>".parse().unwrap();
        match DataForm::try_from(elem) {
            Ok(form) => {
                match form.form_type() {
                    Some(ty) => assert_eq!(ty, "foo"),
                    other => panic!("unexpected extracted form type: {:?}", other),
                };
            }
            other => panic!("unexpected result: {:?}", other),
        }
    }

    #[test]
    fn test_accept_form_type_field_with_type_hidden_in_submit_typed_forms() {
        let elem: Element = "<x xmlns='jabber:x:data' type='submit'><field var='FORM_TYPE' type='hidden'><value>foo</value></field></x>".parse().unwrap();
        match DataForm::try_from(elem) {
            Ok(form) => {
                match form.form_type() {
                    Some(ty) => assert_eq!(ty, "foo"),
                    other => panic!("unexpected extracted form type: {:?}", other),
                };
            }
            other => panic!("unexpected result: {:?}", other),
        }
    }

    #[test]
    fn test_accept_form_type_field_with_type_hidden_in_result_typed_forms() {
        let elem: Element = "<x xmlns='jabber:x:data' type='result'><field var='FORM_TYPE' type='hidden'><value>foo</value></field></x>".parse().unwrap();
        match DataForm::try_from(elem) {
            Ok(form) => {
                match form.form_type() {
                    Some(ty) => assert_eq!(ty, "foo"),
                    other => panic!("unexpected extracted form type: {:?}", other),
                };
            }
            other => panic!("unexpected result: {:?}", other),
        }
    }

    #[test]
    fn test_accept_form_type_field_with_type_hidden_in_form_typed_forms() {
        let elem: Element = "<x xmlns='jabber:x:data' type='form'><field var='FORM_TYPE' type='hidden'><value>foo</value></field></x>".parse().unwrap();
        match DataForm::try_from(elem) {
            Ok(form) => {
                match form.form_type() {
                    Some(ty) => assert_eq!(ty, "foo"),
                    other => panic!("unexpected extracted form type: {:?}", other),
                };
            }
            other => panic!("unexpected result: {:?}", other),
        }
    }
}
