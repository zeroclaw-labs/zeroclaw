// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use crate::message::MessagePayload;
use crate::ns;
use alloc::collections::BTreeMap;
use minidom::{Element, Node};
use xso::exports::rxml;
use xso::{
    error::{Error, FromElementError},
    exports::rxml::Namespace,
};

// TODO: Use a proper lang type.
type Lang = String;

/// Container for formatted text.
#[derive(Debug, Clone)]
pub struct XhtmlIm {
    /// Map of language to body element.
    bodies: BTreeMap<Lang, Body>,
}

impl XhtmlIm {
    /// Serialise formatted text to HTML.
    pub fn into_html(self) -> String {
        let mut html = Vec::new();
        // TODO: use the best language instead.
        // XXX: Remove this flag later when fixing the code below
        #[allow(clippy::never_loop)]
        for (lang, body) in self.bodies {
            if lang.is_empty() {
                assert!(body.xml_lang.is_none());
            } else {
                assert_eq!(Some(lang), body.xml_lang);
            }
            for tag in body.children {
                html.push(tag.into_html());
            }
            break;
        }
        html.concat()
    }

    /// Removes all unknown elements.
    fn flatten(self) -> XhtmlIm {
        let mut bodies = BTreeMap::new();
        for (lang, body) in self.bodies {
            let children = body.children.into_iter().fold(vec![], |mut acc, child| {
                match child {
                    Child::Tag(Tag::Unknown(children)) => acc.extend(children),
                    any => acc.push(any),
                }
                acc
            });
            let body = Body { children, ..body };
            bodies.insert(lang, body);
        }
        XhtmlIm { bodies }
    }
}

impl MessagePayload for XhtmlIm {}

impl TryFrom<Element> for XhtmlIm {
    type Error = FromElementError;

    fn try_from(elem: Element) -> Result<XhtmlIm, FromElementError> {
        check_self!(elem, "html", XHTML_IM);
        check_no_attributes!(elem, "html");

        let mut bodies = BTreeMap::new();
        for child in elem.children() {
            if child.is("body", ns::XHTML) {
                let child = child.clone();
                let lang = child
                    .attr_ns(rxml::Namespace::xml(), "lang")
                    .unwrap_or("")
                    .to_string();
                let body = Body::try_from(child)?;
                match bodies.insert(lang, body) {
                    None => (),
                    Some(_) => {
                        return Err(Error::Other(
                            "Two identical language bodies found in XHTML-IM.",
                        )
                        .into())
                    }
                }
            } else {
                return Err(Error::Other("Unknown element in XHTML-IM.").into());
            }
        }

        Ok(XhtmlIm { bodies }.flatten())
    }
}

impl From<XhtmlIm> for Element {
    fn from(wrapper: XhtmlIm) -> Element {
        Element::builder("html", ns::XHTML_IM)
            .append_all(wrapper.bodies.into_iter().map(|(lang, body)| {
                if lang.is_empty() {
                    assert!(body.xml_lang.is_none());
                } else {
                    assert_eq!(Some(lang), body.xml_lang);
                }
                Element::from(body)
            }))
            .build()
    }
}

#[derive(Debug, Clone)]
enum Child {
    Tag(Tag),
    Text(String),
}

impl Child {
    fn into_html(self) -> String {
        match self {
            Child::Tag(tag) => tag.into_html(),
            Child::Text(text) => text,
        }
    }
}

#[derive(Debug, Clone)]
struct Property {
    key: String,
    value: String,
}

type Css = Vec<Property>;

fn get_style_string(style: Css) -> Option<String> {
    let mut result = vec![];
    for Property { key, value } in style {
        result.push(format!("{}: {}", key, value));
    }
    if result.is_empty() {
        return None;
    }
    Some(result.join("; "))
}

#[derive(Debug, Clone)]
struct Body {
    style: Css,
    xml_lang: Option<String>,
    children: Vec<Child>,
}

impl TryFrom<Element> for Body {
    type Error = Error;

    fn try_from(elem: Element) -> Result<Body, Error> {
        let mut children = vec![];
        for child in elem.nodes() {
            match child {
                Node::Element(child) => children.push(Child::Tag(Tag::try_from(child.clone())?)),
                Node::Text(text) => children.push(Child::Text(text.clone())),
            }
        }

        Ok(Body {
            style: parse_css(elem.attr("style")),
            xml_lang: elem.attr_ns("xml", "lang").map(ToString::to_string),
            children,
        })
    }
}

impl From<Body> for Element {
    fn from(body: Body) -> Element {
        Element::builder("body", ns::XHTML)
            .attr(
                rxml::xml_ncname!("style").into(),
                get_style_string(body.style),
            )
            .attr_ns(
                Into::<Namespace>::into(String::from("xml")),
                rxml::xml_ncname!("lang").into(),
                body.xml_lang,
            )
            .append_all(children_to_nodes(body.children))
            .build()
    }
}

#[derive(Debug, Clone)]
enum Tag {
    A {
        href: Option<String>,
        style: Css,
        type_: Option<String>,
        children: Vec<Child>,
    },
    Blockquote {
        style: Css,
        children: Vec<Child>,
    },
    Br,
    Cite {
        style: Css,
        children: Vec<Child>,
    },
    Em {
        children: Vec<Child>,
    },
    Img {
        src: Option<String>,
        alt: Option<String>,
    }, // TODO: height, width, style
    Li {
        style: Css,
        children: Vec<Child>,
    },
    Ol {
        style: Css,
        children: Vec<Child>,
    },
    P {
        style: Css,
        children: Vec<Child>,
    },
    Span {
        style: Css,
        children: Vec<Child>,
    },
    Strong {
        children: Vec<Child>,
    },
    Ul {
        style: Css,
        children: Vec<Child>,
    },
    Unknown(Vec<Child>),
}

impl Tag {
    fn into_html(self) -> String {
        match self {
            Tag::A {
                href,
                style,
                type_,
                children,
            } => {
                let href = write_attr(href, "href");
                let style = write_attr(get_style_string(style), "style");
                let type_ = write_attr(type_, "type");
                format!(
                    "<a{}{}{}>{}</a>",
                    href,
                    style,
                    type_,
                    children_to_html(children)
                )
            }
            Tag::Blockquote { style, children } => {
                let style = write_attr(get_style_string(style), "style");
                format!(
                    "<blockquote{}>{}</blockquote>",
                    style,
                    children_to_html(children)
                )
            }
            Tag::Br => String::from("<br>"),
            Tag::Cite { style, children } => {
                let style = write_attr(get_style_string(style), "style");
                format!("<cite{}>{}</cite>", style, children_to_html(children))
            }
            Tag::Em { children } => format!("<em>{}</em>", children_to_html(children)),
            Tag::Img { src, alt } => {
                let src = write_attr(src, "src");
                let alt = write_attr(alt, "alt");
                format!("<img{}{}>", src, alt)
            }
            Tag::Li { style, children } => {
                let style = write_attr(get_style_string(style), "style");
                format!("<li{}>{}</li>", style, children_to_html(children))
            }
            Tag::Ol { style, children } => {
                let style = write_attr(get_style_string(style), "style");
                format!("<ol{}>{}</ol>", style, children_to_html(children))
            }
            Tag::P { style, children } => {
                let style = write_attr(get_style_string(style), "style");
                format!("<p{}>{}</p>", style, children_to_html(children))
            }
            Tag::Span { style, children } => {
                let style = write_attr(get_style_string(style), "style");
                format!("<span{}>{}</span>", style, children_to_html(children))
            }
            Tag::Strong { children } => format!("<strong>{}</strong>", children_to_html(children)),
            Tag::Ul { style, children } => {
                let style = write_attr(get_style_string(style), "style");
                format!("<ul{}>{}</ul>", style, children_to_html(children))
            }
            Tag::Unknown(_) => {
                panic!("No unknown element should be present in XHTML-IM after parsing.")
            }
        }
    }
}

impl TryFrom<Element> for Tag {
    type Error = Error;

    fn try_from(elem: Element) -> Result<Tag, Error> {
        let mut children = vec![];
        for child in elem.nodes() {
            match child {
                Node::Element(child) => children.push(Child::Tag(Tag::try_from(child.clone())?)),
                Node::Text(text) => children.push(Child::Text(text.clone())),
            }
        }

        Ok(match elem.name() {
            "a" => Tag::A {
                href: elem.attr("href").map(ToString::to_string),
                style: parse_css(elem.attr(rxml::xml_ncname!("style"))),
                type_: elem.attr("type").map(ToString::to_string),
                children,
            },
            "blockquote" => Tag::Blockquote {
                style: parse_css(elem.attr("style")),
                children,
            },
            "br" => Tag::Br,
            "cite" => Tag::Cite {
                style: parse_css(elem.attr("style")),
                children,
            },
            "em" => Tag::Em { children },
            "img" => Tag::Img {
                src: elem.attr("src").map(ToString::to_string),
                alt: elem.attr("alt").map(ToString::to_string),
            },
            "li" => Tag::Li {
                style: parse_css(elem.attr("style")),
                children,
            },
            "ol" => Tag::Ol {
                style: parse_css(elem.attr("style")),
                children,
            },
            "p" => Tag::P {
                style: parse_css(elem.attr("style")),
                children,
            },
            "span" => Tag::Span {
                style: parse_css(elem.attr("style")),
                children,
            },
            "strong" => Tag::Strong { children },
            "ul" => Tag::Ul {
                style: parse_css(elem.attr("style")),
                children,
            },
            _ => Tag::Unknown(children),
        })
    }
}

impl From<Tag> for Element {
    fn from(tag: Tag) -> Element {
        let (name, attrs, children) = match tag {
            Tag::A {
                href,
                style,
                type_,
                children,
            } => (
                "a",
                {
                    let mut attrs = vec![];
                    if let Some(href) = href {
                        attrs.push((rxml::xml_ncname!("href"), href));
                    }
                    if let Some(style) = get_style_string(style) {
                        attrs.push((rxml::xml_ncname!("style"), style));
                    }
                    if let Some(type_) = type_ {
                        attrs.push((rxml::xml_ncname!("type"), type_));
                    }
                    attrs
                },
                children,
            ),
            Tag::Blockquote { style, children } => (
                "blockquote",
                match get_style_string(style) {
                    Some(style) => vec![(rxml::xml_ncname!("style"), style)],
                    None => vec![],
                },
                children,
            ),
            Tag::Br => ("br", vec![], vec![]),
            Tag::Cite { style, children } => (
                "cite",
                match get_style_string(style) {
                    Some(style) => vec![(rxml::xml_ncname!("style"), style)],
                    None => vec![],
                },
                children,
            ),
            Tag::Em { children } => ("em", vec![], children),
            Tag::Img { src, alt } => {
                let mut attrs = vec![];
                if let Some(src) = src {
                    attrs.push((rxml::xml_ncname!("src"), src));
                }
                if let Some(alt) = alt {
                    attrs.push((rxml::xml_ncname!("alt"), alt));
                }
                ("img", attrs, vec![])
            }
            Tag::Li { style, children } => (
                "li",
                match get_style_string(style) {
                    Some(style) => vec![(rxml::xml_ncname!("style"), style)],
                    None => vec![],
                },
                children,
            ),
            Tag::Ol { style, children } => (
                "ol",
                match get_style_string(style) {
                    Some(style) => vec![(rxml::xml_ncname!("style"), style)],
                    None => vec![],
                },
                children,
            ),
            Tag::P { style, children } => (
                "p",
                match get_style_string(style) {
                    Some(style) => vec![(rxml::xml_ncname!("style"), style)],
                    None => vec![],
                },
                children,
            ),
            Tag::Span { style, children } => (
                "span",
                match get_style_string(style) {
                    Some(style) => vec![(rxml::xml_ncname!("style"), style)],
                    None => vec![],
                },
                children,
            ),
            Tag::Strong { children } => ("strong", vec![], children),
            Tag::Ul { style, children } => (
                "ul",
                match get_style_string(style) {
                    Some(style) => vec![(rxml::xml_ncname!("style"), style)],
                    None => vec![],
                },
                children,
            ),
            Tag::Unknown(_) => {
                panic!("No unknown element should be present in XHTML-IM after parsing.")
            }
        };
        let mut builder = Element::builder(name, ns::XHTML).append_all(children_to_nodes(children));
        for (key, value) in attrs {
            builder = builder.attr(key.into(), value);
        }
        builder.build()
    }
}

fn children_to_nodes(children: Vec<Child>) -> impl IntoIterator<Item = Node> {
    children.into_iter().map(|child| match child {
        Child::Tag(tag) => Node::Element(Element::from(tag)),
        Child::Text(text) => Node::Text(text),
    })
}

fn children_to_html(children: Vec<Child>) -> String {
    children
        .into_iter()
        .map(|child| child.into_html())
        .collect::<Vec<_>>()
        .concat()
}

fn write_attr(attr: Option<String>, name: &str) -> String {
    match attr {
        Some(attr) => format!(" {}='{}'", name, attr),
        None => String::new(),
    }
}

fn parse_css(style: Option<&str>) -> Css {
    let mut properties = vec![];
    if let Some(style) = style {
        // TODO: make that parser a bit more resilient to things.
        for declaration in style.split(';') {
            let (key, value) = declaration.split_once(':').unwrap();
            properties.push(Property {
                key: key.to_string(),
                value: value.to_string(),
            });
        }
    }
    properties
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(XhtmlIm, 12);
        assert_size!(Child, 48);
        assert_size!(Tag, 48);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(XhtmlIm, 24);
        assert_size!(Child, 96);
        assert_size!(Tag, 96);
    }

    #[test]
    fn test_empty() {
        let elem: Element = "<html xmlns='http://jabber.org/protocol/xhtml-im'/>"
            .parse()
            .unwrap();
        let xhtml = XhtmlIm::try_from(elem).unwrap();
        assert_eq!(xhtml.bodies.len(), 0);

        let elem: Element = "<html xmlns='http://jabber.org/protocol/xhtml-im'><body xmlns='http://www.w3.org/1999/xhtml'/></html>"
            .parse()
            .unwrap();
        let xhtml = XhtmlIm::try_from(elem).unwrap();
        assert_eq!(xhtml.bodies.len(), 1);

        let elem: Element = "<html xmlns='http://jabber.org/protocol/xhtml-im' xmlns:html='http://www.w3.org/1999/xhtml'><html:body xml:lang='fr'/><html:body xml:lang='en'/></html>"
            .parse()
            .unwrap();
        let xhtml = XhtmlIm::try_from(elem).unwrap();
        assert_eq!(xhtml.bodies.len(), 2);
    }

    #[test]
    fn invalid_two_same_langs() {
        let elem: Element = "<html xmlns='http://jabber.org/protocol/xhtml-im' xmlns:html='http://www.w3.org/1999/xhtml'><html:body/><html:body/></html>"
            .parse()
            .unwrap();
        let error = XhtmlIm::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Two identical language bodies found in XHTML-IM.");
    }

    #[test]
    fn test_tag() {
        let elem: Element = "<body xmlns='http://www.w3.org/1999/xhtml'/>"
            .parse()
            .unwrap();
        let body = Body::try_from(elem).unwrap();
        assert_eq!(body.children.len(), 0);

        let elem: Element = "<body xmlns='http://www.w3.org/1999/xhtml'><p>Hello world!</p></body>"
            .parse()
            .unwrap();
        let mut body = Body::try_from(elem).unwrap();
        assert_eq!(body.style.len(), 0);
        assert_eq!(body.xml_lang, None);
        assert_eq!(body.children.len(), 1);
        let p = match body.children.pop() {
            Some(Child::Tag(tag)) => tag,
            _ => panic!(),
        };
        let mut children = match p {
            Tag::P { style, children } => {
                assert_eq!(style.len(), 0);
                assert_eq!(children.len(), 1);
                children
            }
            _ => panic!(),
        };
        let text = match children.pop() {
            Some(Child::Text(text)) => text,
            _ => panic!(),
        };
        assert_eq!(text, "Hello world!");
    }

    #[test]
    fn test_unknown_element() {
        let elem: Element = "<html xmlns='http://jabber.org/protocol/xhtml-im'><body xmlns='http://www.w3.org/1999/xhtml'><coucou>Hello world!</coucou></body></html>"
            .parse()
            .unwrap();
        let parsed = XhtmlIm::try_from(elem).unwrap();
        let parsed2 = parsed.clone();
        let html = parsed.into_html();
        assert_eq!(html, "Hello world!");

        let elem = Element::from(parsed2);
        assert_eq!(String::from(&elem), "<html xmlns='http://jabber.org/protocol/xhtml-im'><body xmlns='http://www.w3.org/1999/xhtml'>Hello world!</body></html>");
    }

    #[test]
    fn test_generate_html() {
        let elem: Element = "<html xmlns='http://jabber.org/protocol/xhtml-im'><body xmlns='http://www.w3.org/1999/xhtml'><p>Hello world!</p></body></html>"
            .parse()
            .unwrap();
        let xhtml_im = XhtmlIm::try_from(elem).unwrap();
        let html = xhtml_im.into_html();
        assert_eq!(html, "<p>Hello world!</p>");

        let elem: Element = "<html xmlns='http://jabber.org/protocol/xhtml-im'><body xmlns='http://www.w3.org/1999/xhtml'><p>Hello <strong>world</strong>!</p></body></html>"
            .parse()
            .unwrap();
        let xhtml_im = XhtmlIm::try_from(elem).unwrap();
        let html = xhtml_im.into_html();
        assert_eq!(html, "<p>Hello <strong>world</strong>!</p>");
    }

    #[test]
    fn generate_tree() {
        let world = "world".to_string();

        Body {
            style: vec![],
            xml_lang: Some("en".to_string()),
            children: vec![Child::Tag(Tag::P {
                style: vec![],
                children: vec![
                    Child::Text("Hello ".to_string()),
                    Child::Tag(Tag::Strong {
                        children: vec![Child::Text(world)],
                    }),
                    Child::Text("!".to_string()),
                ],
            })],
        };
    }
}
