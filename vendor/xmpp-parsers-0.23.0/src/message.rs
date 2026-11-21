// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use crate::ns;
use alloc::borrow::Cow;
use alloc::collections::BTreeMap;
use core::borrow::Borrow;
use core::fmt;
use core::ops::{Deref, DerefMut};
use jid::Jid;
use minidom::Element;
use xso::{
    error::{Error, FromElementError},
    AsOptionalXmlText, AsXml, FromXml, FromXmlText,
};

/// Should be implemented on every known payload of a `<message/>`.
pub trait MessagePayload: TryFrom<Element> + Into<Element> {}

generate_attribute!(
    /// The type of a message.
    MessageType, "type", {
        /// Standard instant messaging message.
        Chat => "chat",

        /// Notifies that an error happened.
        Error => "error",

        /// Standard group instant messaging message.
        Groupchat => "groupchat",

        /// Used by servers to notify users when things happen.
        Headline => "headline",

        /// This is an email-like message, it usually contains a
        /// [subject](struct.Subject.html).
        Normal => "normal",
    }, Default = Normal
);

generate_id!(
    /// Id field in a [`Message`], if any.
    ///
    /// This field is not mandatory on incoming messages, but may be useful for moderation/correction,
    /// especially for outgoing messages.
    Id
);

/// Wrapper type to represent an optional `xml:lang` attribute.
///
/// This is necessary because we do not want to emit empty `xml:lang`
/// attributes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Lang(pub String);

impl Deref for Lang {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Lang {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl fmt::Display for Lang {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromXmlText for Lang {
    fn from_xml_text(s: String) -> Result<Self, Error> {
        Ok(Self(s))
    }
}

impl AsOptionalXmlText for Lang {
    fn as_optional_xml_text(&self) -> Result<Option<Cow<'_, str>>, Error> {
        if self.0.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Cow::Borrowed(&self.0)))
        }
    }
}

impl Borrow<str> for Lang {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<String> for Lang {
    fn from(other: String) -> Self {
        Self(other)
    }
}

impl From<&str> for Lang {
    fn from(other: &str) -> Self {
        Self(other.to_owned())
    }
}

impl PartialEq<str> for Lang {
    fn eq(&self, rhs: &str) -> bool {
        self.0 == rhs
    }
}

impl PartialEq<&str> for Lang {
    fn eq(&self, rhs: &&str) -> bool {
        self.0 == *rhs
    }
}

impl Lang {
    /// Create a new, empty `Lang`.
    pub fn new() -> Self {
        Self(String::new())
    }
}

/// Threading meta-information.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::DEFAULT_NS, name = "thread")]
pub struct Thread {
    /// The parent of the thread, when creating a subthread.
    #[xml(attribute(default))]
    pub parent: Option<String>,

    /// A thread identifier, so that other people can specify to which message
    /// they are replying.
    #[xml(text)]
    pub id: String,
}

/// The main structure representing the `<message/>` stanza.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::DEFAULT_NS, name = "message")]
pub struct Message {
    /// The JID emitting this stanza.
    #[xml(attribute(default))]
    pub from: Option<Jid>,

    /// The recipient of this stanza.
    #[xml(attribute(default))]
    pub to: Option<Jid>,

    /// The @id attribute of this stanza, which is required in order to match a
    /// request with its response.
    #[xml(attribute(default))]
    pub id: Option<Id>,

    /// The type of this message.
    #[xml(attribute(name = "type", default))]
    pub type_: MessageType,

    /// A list of bodies, sorted per language.  Use
    /// [get_best_body()](#method.get_best_body) to access them on reception.
    #[xml(extract(n = .., name = "body", fields(
        lang(type_ = Lang, default),
        text(type_ = String),
    )))]
    pub bodies: BTreeMap<Lang, String>,

    /// A list of subjects, sorted per language.  Use
    /// [get_best_subject()](#method.get_best_subject) to access them on
    /// reception.
    #[xml(extract(n = .., name = "subject", fields(
        lang(type_ = Lang, default),
        text(type_ = String),
    )))]
    pub subjects: BTreeMap<Lang, String>,

    /// An optional thread identifier, so that other people can reply directly
    /// to this message.
    #[xml(child(default))]
    pub thread: Option<Thread>,

    /// A list of the extension payloads contained in this stanza.
    #[xml(element(n = ..))]
    pub payloads: Vec<Element>,
}

impl Message {
    /// Creates a new `<message/>` stanza of type Chat for the given recipient.
    /// This is equivalent to the [`Message::chat`] method.
    pub fn new<J: Into<Option<Jid>>>(to: J) -> Message {
        Message {
            from: None,
            to: to.into(),
            id: None,
            type_: MessageType::Chat,
            bodies: BTreeMap::new(),
            subjects: BTreeMap::new(),
            thread: None,
            payloads: vec![],
        }
    }

    /// Creates a new `<message/>` stanza of a certain type for the given recipient.
    pub fn new_with_type<J: Into<Option<Jid>>>(type_: MessageType, to: J) -> Message {
        Message {
            from: None,
            to: to.into(),
            id: None,
            type_,
            bodies: BTreeMap::new(),
            subjects: BTreeMap::new(),
            thread: None,
            payloads: vec![],
        }
    }

    /// Creates a Message of type Chat
    pub fn chat<J: Into<Option<Jid>>>(to: J) -> Message {
        Self::new_with_type(MessageType::Chat, to)
    }

    /// Creates a Message of type Error
    pub fn error<J: Into<Option<Jid>>>(to: J) -> Message {
        Self::new_with_type(MessageType::Error, to)
    }

    /// Creates a Message of type Groupchat
    pub fn groupchat<J: Into<Option<Jid>>>(to: J) -> Message {
        Self::new_with_type(MessageType::Groupchat, to)
    }

    /// Creates a Message of type Headline
    pub fn headline<J: Into<Option<Jid>>>(to: J) -> Message {
        Self::new_with_type(MessageType::Headline, to)
    }

    /// Creates a Message of type Normal
    pub fn normal<J: Into<Option<Jid>>>(to: J) -> Message {
        Self::new_with_type(MessageType::Normal, to)
    }

    /// Appends a body in given lang to the Message
    pub fn with_body(mut self, lang: Lang, body: String) -> Message {
        self.bodies.insert(lang, body);
        self
    }

    /// Set a payload inside this message.
    pub fn with_payload<P: MessagePayload>(mut self, payload: P) -> Message {
        self.payloads.push(payload.into());
        self
    }

    /// Set the payloads of this message.
    pub fn with_payloads(mut self, payloads: Vec<Element>) -> Message {
        self.payloads = payloads;
        self
    }

    fn get_best<'a, T>(
        map: &'a BTreeMap<Lang, T>,
        preferred_langs: Vec<&str>,
    ) -> Option<(Lang, &'a T)> {
        if map.is_empty() {
            return None;
        }
        for lang in preferred_langs {
            if let Some(value) = map.get(lang) {
                return Some((Lang::from(lang), value));
            }
        }
        if let Some(value) = map.get("") {
            return Some((Lang::new(), value));
        }
        map.iter().map(|(lang, value)| (lang.clone(), value)).next()
    }

    fn get_best_cloned<T: ToOwned<Owned = T>>(
        map: &BTreeMap<Lang, T>,
        preferred_langs: Vec<&str>,
    ) -> Option<(Lang, T)> {
        if let Some((lang, item)) = Self::get_best::<T>(map, preferred_langs) {
            Some((lang, item.to_owned()))
        } else {
            None
        }
    }

    /// Returns the best matching body from a list of languages.
    ///
    /// For instance, if a message contains both an xml:lang='de', an xml:lang='fr' and an English
    /// body without an xml:lang attribute, and you pass ["fr", "en"] as your preferred languages,
    /// `Some(("fr", the_second_body))` will be returned.
    ///
    /// If no body matches, an undefined body will be returned.
    pub fn get_best_body(&self, preferred_langs: Vec<&str>) -> Option<(Lang, &String)> {
        Message::get_best::<String>(&self.bodies, preferred_langs)
    }

    /// Cloned variant of [`Message::get_best_body`]
    pub fn get_best_body_cloned(&self, preferred_langs: Vec<&str>) -> Option<(Lang, String)> {
        Message::get_best_cloned::<String>(&self.bodies, preferred_langs)
    }

    /// Returns the best matching subject from a list of languages.
    ///
    /// For instance, if a message contains both an xml:lang='de', an xml:lang='fr' and an English
    /// subject without an xml:lang attribute, and you pass ["fr", "en"] as your preferred
    /// languages, `Some(("fr", the_second_subject))` will be returned.
    ///
    /// If no subject matches, an undefined subject will be returned.
    pub fn get_best_subject(&self, preferred_langs: Vec<&str>) -> Option<(Lang, &String)> {
        Message::get_best::<String>(&self.subjects, preferred_langs)
    }

    /// Cloned variant of [`Message::get_best_subject`]
    pub fn get_best_subject_cloned(&self, preferred_langs: Vec<&str>) -> Option<(Lang, String)> {
        Message::get_best_cloned::<String>(&self.subjects, preferred_langs)
    }

    /// Try to extract the given payload type from the message's payloads.
    ///
    /// Returns the first matching payload element as parsed struct or its
    /// parse error. If no element matches, `Ok(None)` is returned. If an
    /// element matches, but fails to parse, it is nonetheless removed from
    /// the message.
    ///
    /// Elements which do not match the given type are not removed.
    pub fn extract_payload<T: TryFrom<Element, Error = FromElementError>>(
        &mut self,
    ) -> Result<Option<T>, Error> {
        let mut buf = Vec::with_capacity(self.payloads.len());
        let mut iter = self.payloads.drain(..);
        let mut result = Ok(None);
        for item in &mut iter {
            match T::try_from(item) {
                Ok(v) => {
                    result = Ok(Some(v));
                    break;
                }
                Err(FromElementError::Mismatch(residual)) => {
                    buf.push(residual);
                }
                Err(FromElementError::Invalid(other)) => {
                    result = Err(other);
                    break;
                }
            }
        }
        buf.extend(iter);
        core::mem::swap(&mut buf, &mut self.payloads);
        result
    }

    /// Tries to extract the payload, warning when parsing fails.
    ///
    /// This method uses [`Message::extract_payload`], but removes the error
    /// case by simply warning to the current logger.
    #[cfg(feature = "log")]
    pub fn extract_valid_payload<T: TryFrom<Element, Error = FromElementError>>(
        &mut self,
    ) -> Option<T> {
        match self.extract_payload::<T>() {
            Ok(opt) => opt,
            Err(e) => {
                // TODO: xso should support human-readable name for T
                log::warn!("Failed to parse payload: {e}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(MessageType, 1);
        assert_size!(Thread, 24);
        assert_size!(Message, 108);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(MessageType, 1);
        assert_size!(Thread, 48);
        assert_size!(Message, 216);
    }

    #[test]
    fn test_simple() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<message xmlns='jabber:client'/>".parse().unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<message xmlns='jabber:component:accept'/>"
            .parse()
            .unwrap();
        let message = Message::try_from(elem).unwrap();
        assert_eq!(message.from, None);
        assert_eq!(message.to, None);
        assert_eq!(message.id, None);
        assert_eq!(message.type_, MessageType::Normal);
        assert!(message.payloads.is_empty());
    }

    #[test]
    fn test_serialise() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<message xmlns='jabber:client'/>".parse().unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<message xmlns='jabber:component:accept'/>"
            .parse()
            .unwrap();
        let mut message = Message::new(None);
        message.type_ = MessageType::Normal;
        let elem2 = message.into();
        assert_eq!(elem, elem2);
    }

    #[test]
    fn test_body() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<message xmlns='jabber:client' to='coucou@example.org' type='chat'><body>Hello world!</body></message>".parse().unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<message xmlns='jabber:component:accept' to='coucou@example.org' type='chat'><body>Hello world!</body></message>".parse().unwrap();
        let elem1 = elem.clone();
        let message = Message::try_from(elem).unwrap();
        assert_eq!(message.bodies[""], "Hello world!");

        {
            let (lang, body) = message.get_best_body(vec!["en"]).unwrap();
            assert_eq!(lang, "");
            assert_eq!(body, &"Hello world!");
        }

        let elem2 = message.into();
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn test_serialise_body() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<message xmlns='jabber:client' to='coucou@example.org' type='chat'><body>Hello world!</body></message>".parse().unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<message xmlns='jabber:component:accept' to='coucou@example.org' type='chat'><body>Hello world!</body></message>".parse().unwrap();
        let mut message = Message::new(Jid::new("coucou@example.org").unwrap());
        message
            .bodies
            .insert(Lang::from(""), "Hello world!".to_owned());
        let elem2 = message.into();
        assert_eq!(elem, elem2);
    }

    #[test]
    fn test_subject() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<message xmlns='jabber:client' to='coucou@example.org' type='chat'><subject>Hello world!</subject></message>".parse().unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<message xmlns='jabber:component:accept' to='coucou@example.org' type='chat'><subject>Hello world!</subject></message>".parse().unwrap();
        let elem1 = elem.clone();
        let message = Message::try_from(elem).unwrap();
        assert_eq!(message.subjects[""], "Hello world!",);

        {
            let (lang, subject) = message.get_best_subject(vec!["en"]).unwrap();
            assert_eq!(lang, "");
            assert_eq!(subject, "Hello world!");
        }

        // Test cloned variant.
        {
            let (lang, subject) = message.get_best_subject_cloned(vec!["en"]).unwrap();
            assert_eq!(lang, "");
            assert_eq!(subject, "Hello world!");
        }

        let elem2 = message.into();
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn get_best_body() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<message xmlns='jabber:client' to='coucou@example.org' type='chat'><body xml:lang='de'>Hallo Welt!</body><body xml:lang='fr'>Salut le monde !</body><body>Hello world!</body></message>".parse().unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<message xmlns='jabber:component:accept' to='coucou@example.org' type='chat'><body xml:lang='de'>Hallo Welt!</body><body xml:lang='fr'>Salut le monde !</body><body>Hello world!</body></message>".parse().unwrap();
        let message = Message::try_from(elem).unwrap();

        // Tests basic feature.
        {
            let (lang, body) = message.get_best_body(vec!["fr"]).unwrap();
            assert_eq!(lang, "fr");
            assert_eq!(body, "Salut le monde !");
        }

        // Tests order.
        {
            let (lang, body) = message.get_best_body(vec!["en", "de"]).unwrap();
            assert_eq!(lang, "de");
            assert_eq!(body, "Hallo Welt!");
        }

        // Tests fallback.
        {
            let (lang, body) = message.get_best_body(vec![]).unwrap();
            assert_eq!(lang, "");
            assert_eq!(body, "Hello world!");
        }

        // Tests fallback.
        {
            let (lang, body) = message.get_best_body(vec!["ja"]).unwrap();
            assert_eq!(lang, "");
            assert_eq!(body, "Hello world!");
        }

        // Test cloned variant.
        {
            let (lang, body) = message.get_best_body_cloned(vec!["ja"]).unwrap();
            assert_eq!(lang, "");
            assert_eq!(body, "Hello world!");
        }

        let message = Message::new(None);

        // Tests without a body.
        assert_eq!(message.get_best_body(vec!("ja")), None);
    }

    #[test]
    fn test_attention() {
        #[cfg(not(feature = "component"))]
        let elem: Element = "<message xmlns='jabber:client' to='coucou@example.org' type='chat'><attention xmlns='urn:xmpp:attention:0'/></message>".parse().unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<message xmlns='jabber:component:accept' to='coucou@example.org' type='chat'><attention xmlns='urn:xmpp:attention:0'/></message>".parse().unwrap();
        let elem1 = elem.clone();
        let message = Message::try_from(elem).unwrap();
        let elem2 = message.into();
        assert_eq!(elem1, elem2);
    }

    #[test]
    fn test_extract_payload() {
        use super::super::attention::Attention;
        use super::super::pubsub;

        #[cfg(not(feature = "component"))]
        let elem: Element = "<message xmlns='jabber:client' to='coucou@example.org' type='chat'><attention xmlns='urn:xmpp:attention:0'/></message>".parse().unwrap();
        #[cfg(feature = "component")]
        let elem: Element = "<message xmlns='jabber:component:accept' to='coucou@example.org' type='chat'><attention xmlns='urn:xmpp:attention:0'/></message>".parse().unwrap();
        let mut message = Message::try_from(elem).unwrap();
        assert_eq!(message.payloads.len(), 1);
        match message.extract_payload::<pubsub::Event>() {
            Ok(None) => (),
            other => panic!("unexpected result: {:?}", other),
        };
        assert_eq!(message.payloads.len(), 1);
        match message.extract_payload::<Attention>() {
            Ok(Some(_)) => (),
            other => panic!("unexpected result: {:?}", other),
        };
        assert_eq!(message.payloads.len(), 0);
    }
}
