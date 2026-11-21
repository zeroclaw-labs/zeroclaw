// Copyright (c) 2017-2018 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

macro_rules! get_attr {
    ($elem:ident, $attr:tt, $type:tt) => {
        get_attr!(
            $elem,
            $attr,
            $type,
            value,
            value.parse().map_err(xso::error::Error::text_parse_error)?
        )
    };
    ($elem:ident, $attr:tt, Option, $value:ident, $func:expr) => {
        match $elem.attr($attr) {
            Some($value) => Some($func),
            None => None,
        }
    };
    ($elem:ident, $attr:tt, Required, $value:ident, $func:expr) => {
        match $elem.attr($attr) {
            Some($value) => $func,
            None => {
                return Err(xso::error::Error::Other(
                    concat!("Required attribute '", $attr, "' missing.").into(),
                )
                .into());
            }
        }
    };
    ($elem:ident, $attr:tt, Default, $value:ident, $func:expr) => {
        match $elem.attr($attr) {
            Some($value) => $func,
            None => ::core::default::Default::default(),
        }
    };
}

macro_rules! generate_attribute {
    ($(#[$meta:meta])* $elem:ident, $name:tt, {$($(#[$a_meta:meta])* $a:ident => $b:tt),+$(,)?}) => (
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        pub enum $elem {
            $(
                $(#[$a_meta])*
                $a
            ),+
        }
        impl ::core::str::FromStr for $elem {
            type Err = xso::error::Error;
            fn from_str(s: &str) -> Result<$elem, xso::error::Error> {
                Ok(match s {
                    $($b => $elem::$a),+,
                    _ => return Err(xso::error::Error::Other(concat!("Unknown value for '", $name, "' attribute.")).into()),
                })
            }
        }
        impl ::xso::FromXmlText for $elem {
            fn from_xml_text(s: String) -> Result<$elem, xso::error::Error> {
                s.parse().map_err(xso::error::Error::text_parse_error)
            }
        }
        impl core::fmt::Display for $elem {
            fn fmt(&self, fmt: &mut core::fmt::Formatter) -> Result<(), core::fmt::Error> {
                write!(fmt, "{}", match self {
                    $($elem::$a => $b),+
                })
            }
        }
        impl ::xso::AsXmlText for $elem {
            fn as_xml_text(&self) -> Result<::alloc::borrow::Cow<'_, str>, xso::error::Error> {
                match self {
                    $(
                        $elem::$a => Ok(::alloc::borrow::Cow::Borrowed($b))
                    ),+
                }
            }
        }
        impl ::minidom::IntoAttributeValue for $elem {
            fn into_attribute_value(self) -> Option<String> {
                Some(String::from(match self {
                    $($elem::$a => $b),+
                }))
            }
        }
    );
    ($(#[$meta:meta])* $elem:ident, $name:tt, {$($(#[$a_meta:meta])* $a:ident => $b:tt),+$(,)?}, Default = $default:ident) => (
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        pub enum $elem {
            $(
                $(#[$a_meta])*
                $a
            ),+
        }
        impl ::core::str::FromStr for $elem {
            type Err = xso::error::Error;
            fn from_str(s: &str) -> Result<$elem, xso::error::Error> {
                Ok(match s {
                    $($b => $elem::$a),+,
                    _ => return Err(xso::error::Error::Other(concat!("Unknown value for '", $name, "' attribute.")).into()),
                })
            }
        }
        impl ::xso::FromXmlText for $elem {
            fn from_xml_text(s: String) -> Result<$elem, xso::error::Error> {
                s.parse().map_err(xso::error::Error::text_parse_error)
            }
        }
        impl ::xso::AsXmlText for $elem {
            fn as_xml_text(&self) -> Result<alloc::borrow::Cow<'_, str>, xso::error::Error> {
                Ok(alloc::borrow::Cow::Borrowed(match self {
                    $($elem::$a => $b),+
                }))
            }

            #[allow(unreachable_patterns)]
            fn as_optional_xml_text(&self) -> Result<Option<alloc::borrow::Cow<'_, str>>, xso::error::Error> {
                Ok(Some(alloc::borrow::Cow::Borrowed(match self {
                    $elem::$default => return Ok(None),
                    $($elem::$a => $b),+
                })))
            }
        }
        impl ::minidom::IntoAttributeValue for $elem {
            #[allow(unreachable_patterns)]
            fn into_attribute_value(self) -> Option<String> {
                Some(String::from(match self {
                    $elem::$default => return None,
                    $($elem::$a => $b),+
                }))
            }
        }
        impl ::core::default::Default for $elem {
            fn default() -> $elem {
                $elem::$default
            }
        }
    );
    ($(#[$meta:meta])* $elem:ident, $name:tt, ($(#[$meta_symbol:meta])* $symbol:ident => $value:tt)) => (
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        pub enum $elem {
            $(#[$meta_symbol])*
            $symbol,
            /// Value when absent.
            None,
        }
        impl ::core::str::FromStr for $elem {
            type Err = xso::error::Error;
            fn from_str(s: &str) -> Result<Self, xso::error::Error> {
                Ok(match s {
                    $value => $elem::$symbol,
                    _ => return Err(xso::error::Error::Other(concat!("Unknown value for '", $name, "' attribute."))),
                })
            }
        }
        impl ::minidom::IntoAttributeValue for $elem {
            fn into_attribute_value(self) -> Option<String> {
                match self {
                    $elem::$symbol => Some(String::from($value)),
                    $elem::None => None
                }
            }
        }
        impl ::core::default::Default for $elem {
            fn default() -> $elem {
                $elem::None
            }
        }
        impl ::xso::FromXmlText for $elem {
            fn from_xml_text(s: String) -> Result<$elem, xso::error::Error> {
                s.parse().map_err(xso::error::Error::text_parse_error)
            }
        }
        impl ::xso::AsXmlText for $elem {
            fn as_xml_text(&self) -> Result<::alloc::borrow::Cow<'_, str>, xso::error::Error> {
                Ok(::alloc::borrow::Cow::Borrowed($value))
            }

            #[allow(unreachable_patterns)]
            fn as_optional_xml_text(&self) -> Result<Option<alloc::borrow::Cow<'_, str>>, xso::error::Error> {
                Ok(Some(alloc::borrow::Cow::Borrowed(match self {
                    $elem::$symbol => $value,
                    $elem::None => return Ok(None),
                })))
            }
        }
    );
    ($(#[$meta:meta])* $elem:ident, $name:tt, $type:tt, Default = $default:expr) => (
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        pub struct $elem(pub $type);
        impl ::core::str::FromStr for $elem {
            type Err = xso::error::Error;
            fn from_str(s: &str) -> Result<Self, xso::error::Error> {
                Ok($elem($type::from_str(s).map_err(xso::error::Error::text_parse_error)?))
            }
        }
        impl ::minidom::IntoAttributeValue for $elem {
            fn into_attribute_value(self) -> Option<String> {
                match self {
                    $elem($default) => None,
                    $elem(value) => Some(format!("{}", value)),
                }
            }
        }
        impl ::core::default::Default for $elem {
            fn default() -> $elem {
                $elem($default)
            }
        }
        impl ::xso::FromXmlText for $elem {
            fn from_xml_text(s: String) -> Result<$elem, xso::error::Error> {
                s.parse().map_err(xso::error::Error::text_parse_error)
            }
        }
        impl ::xso::AsXmlText for $elem {
            fn as_xml_text(&self) -> Result<::alloc::borrow::Cow<'_, str>, xso::error::Error> {
                Ok(::alloc::borrow::Cow::Owned(format!("{}", self.0)))
            }

            fn as_optional_xml_text(&self) -> Result<Option<::alloc::borrow::Cow<'_, str>>, xso::error::Error> {
                match self.0 {
                    $default => Ok(None),
                    _ => Ok(Some(::alloc::borrow::Cow::Owned(format!("{}", self.0)))),
                }
            }
        }
    );
}

macro_rules! generate_attribute_enum {
    ($(#[$meta:meta])* $elem:ident, $name:tt, $ns:ident, $attr:tt, {$($(#[$enum_meta:meta])* $enum:ident => $enum_name:tt),+$(,)?}) => (
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        pub enum $elem {
            $(
                $(#[$enum_meta])*
                $enum
            ),+
        }
        impl ::core::convert::TryFrom<minidom::Element> for $elem {
            type Error = xso::error::FromElementError;
            fn try_from(elem: minidom::Element) -> Result<$elem, xso::error::FromElementError> {
                check_ns_only!(elem, $name, $ns);
                check_no_children!(elem, $name);
                check_no_unknown_attributes!(elem, $name, [$attr]);
                Ok(match get_attr!(elem, $attr, Required) {
                    $($enum_name => $elem::$enum,)+
                    _ => return Err(xso::error::Error::Other(concat!("Invalid ", $name, " ", $attr, " value.")).into()),
                })
            }
        }

        impl ::xso::FromXml for $elem {
            type Builder = ::xso::minidom_compat::FromEventsViaElement<$elem>;

            fn from_events(
                qname: ::xso::exports::rxml::QName,
                attrs: ::xso::exports::rxml::AttrMap,
                _ctx: &::xso::Context<'_>,
            ) -> Result<Self::Builder, ::xso::error::FromEventsError> {
                if qname.0 != crate::ns::$ns || qname.1 != $name {
                    return Err(::xso::error::FromEventsError::Mismatch {
                        name: qname,
                        attrs,
                    })
                }
                Self::Builder::new(qname, attrs)
            }
        }

        impl From<$elem> for minidom::Element {
            fn from(elem: $elem) -> minidom::Element {
                minidom::Element::builder($name, crate::ns::$ns)
                    .attr(::xso::exports::rxml::xml_ncname!($attr).into(), match elem {
                         $($elem::$enum => $enum_name,)+
                     })
                     .build()
            }
        }

        impl ::xso::AsXml for $elem {
            type ItemIter<'x> = ::xso::minidom_compat::AsItemsViaElement<'x>;

            fn as_xml_iter(&self) -> Result<Self::ItemIter<'_>, ::xso::error::Error> {
                ::xso::minidom_compat::AsItemsViaElement::new(self.clone())
            }
        }
    );
}

macro_rules! check_self {
    ($elem:ident, $name:tt, $ns:ident) => {
        check_self!($elem, $name, $ns, $name);
    };
    ($elem:ident, $name:tt, $ns:ident, $pretty_name:tt) => {
        if !$elem.is($name, crate::ns::$ns) {
            return Err(xso::error::FromElementError::Mismatch($elem));
        }
    };
}

macro_rules! check_ns_only {
    ($elem:ident, $name:tt, $ns:ident) => {
        if !$elem.has_ns(crate::ns::$ns) {
            return Err(xso::error::Error::Other(
                concat!("This is not a ", $name, " element.").into(),
            )
            .into());
        }
    };
}

macro_rules! check_no_children {
    ($elem:ident, $name:tt) => {
        #[cfg(feature = "pedantic")]
        for _ in $elem.children() {
            return Err(xso::error::Error::Other(
                concat!("Unknown child in ", $name, " element.").into(),
            )
            .into());
        }
    };
}

macro_rules! check_no_attributes {
    ($elem:ident, $name:tt) => {
        #[cfg(feature = "pedantic")]
        for _ in $elem.attrs() {
            return Err(xso::error::Error::Other(
                concat!("Unknown attribute in ", $name, " element.").into(),
            )
            .into());
        }
    };
}

macro_rules! check_no_unknown_attributes {
    ($elem:ident, $name:tt, [$($attr:tt),*]) => (
        #[cfg(feature = "pedantic")]
        for ((ns, attr), _) in $elem.attrs() {
            $(
                if *ns == ::xso::exports::rxml::Namespace::NONE && attr == $attr {
                    continue;
                }
            )*
            return Err(xso::error::Error::Other(concat!("Unknown attribute in ", $name, " element.")).into());
        }
    );
}

macro_rules! generate_id {
    ($(#[$meta:meta])* $elem:ident) => (
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $elem(pub String);
        impl ::core::str::FromStr for $elem {
            type Err = xso::error::Error;
            fn from_str(s: &str) -> Result<$elem, xso::error::Error> {
                // TODO: add a way to parse that differently when needed.
                Ok($elem(String::from(s)))
            }
        }
        impl ::xso::FromXmlText for $elem {
            fn from_xml_text(s: String) -> Result<$elem, xso::error::Error> {
                Ok(Self(s))
            }
        }
        impl ::xso::AsXmlText for $elem {
            fn as_xml_text(&self) ->Result<::alloc::borrow::Cow<'_, str>, xso::error::Error> {
                Ok(::alloc::borrow::Cow::Borrowed(self.0.as_str()))
            }
        }
        impl ::minidom::IntoAttributeValue for $elem {
            fn into_attribute_value(self) -> Option<String> {
                Some(self.0)
            }
        }
    );
}

macro_rules! generate_elem_id {
    ($(#[$meta:meta])* $elem:ident, $name:literal, $ns:ident) => (
        generate_elem_id!($(#[$meta])* $elem, $name, $ns, String);
        impl ::core::str::FromStr for $elem {
            type Err = xso::error::Error;
            fn from_str(s: &str) -> Result<$elem, xso::error::Error> {
                // TODO: add a way to parse that differently when needed.
                Ok($elem(String::from(s)))
            }
        }
    );
    ($(#[$meta:meta])* $elem:ident, $name:literal, $ns:ident, $type:ty) => (
        $(#[$meta])*
        #[derive(xso::FromXml, xso::AsXml, Debug, Clone, PartialEq, Eq, Hash)]
        #[xml(namespace = crate::ns::$ns, name = $name)]
        pub struct $elem(#[xml(text)] pub $type);
    );
}

#[cfg(test)]
macro_rules! assert_size (
    ($t:ty, $sz:expr) => (
        assert_eq!(::core::mem::size_of::<$t>(), $sz);
    );
);
