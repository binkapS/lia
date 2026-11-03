//! A minimal XML tree, enough to edit `word/document.xml` in place.
//!
//! Names and attributes are kept exactly as written (including namespace
//! prefixes and `xmlns` declarations), whitespace in text is preserved, and
//! comments and processing instructions are dropped. Entity references other
//! than the five predefined ones and numeric references are rejected, and
//! documents with a DTD are refused, so no entity can be expanded.

use quick_xml::Reader;
use quick_xml::events::Event;

use super::{DocError, Result};

const MAX_DEPTH: usize = 200;
const MAX_NODES: usize = 2_000_000;

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    El(Element),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

fn bad(why: &str) -> DocError {
    DocError::Invalid(format!("malformed XML: {why}"))
}

impl Element {
    pub fn new(name: &str) -> Element {
        Element {
            name: name.to_owned(),
            ..Element::default()
        }
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn set_attr(&mut self, name: &str, value: &str) {
        match self.attrs.iter_mut().find(|(k, _)| k == name) {
            Some((_, v)) => *v = value.to_owned(),
            None => self.attrs.push((name.to_owned(), value.to_owned())),
        }
    }

    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find_map(|n| match n {
            Node::El(e) if e.name == name => Some(e),
            _ => None,
        })
    }

    pub fn child_mut(&mut self, name: &str) -> Option<&mut Element> {
        self.children.iter_mut().find_map(|n| match n {
            Node::El(e) if e.name == name => Some(e),
            _ => None,
        })
    }

    /// The text directly inside this element.
    pub fn own_text(&self) -> String {
        self.children
            .iter()
            .filter_map(|n| match n {
                Node::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    pub fn set_text(&mut self, text: &str) {
        self.children = vec![Node::Text(text.to_owned())];
        if text.starts_with(char::is_whitespace) || text.ends_with(char::is_whitespace) {
            self.set_attr("xml:space", "preserve");
        }
    }
}

/// Parse a whole document and return its root element.
pub fn parse(xml: &[u8]) -> Result<Element> {
    let mut reader = Reader::from_reader(xml);
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let mut nodes = 0usize;
    loop {
        let event = reader.read_event().map_err(|_| bad("unreadable"))?;
        nodes += 1;
        if nodes > MAX_NODES {
            return Err(bad("too large"));
        }
        match event {
            Event::Start(start) => {
                if stack.len() >= MAX_DEPTH {
                    return Err(bad("too deeply nested"));
                }
                stack.push(element_of(&start)?);
            }
            Event::Empty(start) => attach(&mut stack, &mut root, Node::El(element_of(&start)?))?,
            Event::End(_) => {
                let done = stack.pop().ok_or_else(|| bad("unbalanced tags"))?;
                attach(&mut stack, &mut root, Node::El(done))?;
            }
            Event::Text(text) => {
                let text = text.decode().map_err(|_| bad("bad text"))?;
                push_text(&mut stack, &text);
            }
            Event::CData(text) => {
                let text = text.decode().map_err(|_| bad("bad text"))?;
                push_text(&mut stack, &text);
            }
            Event::GeneralRef(reference) => {
                let mut buf = [0u8; 4];
                let text: &str = if let Ok(Some(c)) = reference.resolve_char_ref() {
                    c.encode_utf8(&mut buf)
                } else {
                    match &*reference.decode().map_err(|_| bad("bad entity"))? {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        _ => return Err(bad("unsupported entity")),
                    }
                };
                push_text(&mut stack, text);
            }
            Event::DocType(_) => return Err(bad("DTDs are not supported")),
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(bad("unclosed tags"));
    }
    root.ok_or_else(|| bad("no root element"))
}

fn element_of(start: &quick_xml::events::BytesStart<'_>) -> Result<Element> {
    let name = std::str::from_utf8(start.name().as_ref())
        .map_err(|_| bad("bad name"))?
        .to_owned();
    let mut attrs = Vec::new();
    for attr in start.attributes() {
        let attr = attr.map_err(|_| bad("bad attribute"))?;
        let key = std::str::from_utf8(attr.key.as_ref())
            .map_err(|_| bad("bad attribute name"))?
            .to_owned();
        let value = attr
            .unescape_value()
            .map_err(|_| bad("bad attribute value"))?
            .into_owned();
        attrs.push((key, value));
    }
    Ok(Element {
        name,
        attrs,
        children: Vec::new(),
    })
}

fn attach(stack: &mut [Element], root: &mut Option<Element>, node: Node) -> Result<()> {
    match stack.last_mut() {
        Some(parent) => parent.children.push(node),
        None => match node {
            Node::El(e) if root.is_none() => *root = Some(e),
            Node::El(_) => return Err(bad("more than one root")),
            Node::Text(_) => {}
        },
    }
    Ok(())
}

fn push_text(stack: &mut [Element], text: &str) {
    if let Some(parent) = stack.last_mut() {
        if let Some(Node::Text(last)) = parent.children.last_mut() {
            last.push_str(text);
        } else {
            parent.children.push(Node::Text(text.to_owned()));
        }
    }
}

pub fn escape_into(out: &mut String, text: &str, attribute: bool) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            // XML 1.0 forbids most control characters, even escaped.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(&mut out, text, true);
    out
}

/// Serialise an element (not including an XML declaration).
pub fn write(element: &Element, out: &mut String) {
    out.push('<');
    out.push_str(&element.name);
    for (key, value) in &element.attrs {
        out.push(' ');
        out.push_str(key);
        out.push_str("=\"");
        escape_into(out, value, true);
        out.push('"');
    }
    if element.children.is_empty() {
        out.push_str("/>");
        return;
    }
    out.push('>');
    for child in &element.children {
        match child {
            Node::El(e) => write(e, out),
            Node::Text(t) => escape_into(out, t, false),
        }
    }
    out.push_str("</");
    out.push_str(&element.name);
    out.push('>');
}

pub fn to_document(root: &Element) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    write(root, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_names_attributes_text_and_entities() {
        let src = r#"<w:p xmlns:w="u" a="x&amp;y"><w:t xml:space="preserve"> a &lt; b &#233; </w:t><w:br/></w:p>"#;
        let root = parse(src.as_bytes()).unwrap();
        assert_eq!(root.attr("a"), Some("x&y"));
        assert_eq!(root.child("w:t").unwrap().own_text(), " a < b é ");
        let mut out = String::new();
        write(&root, &mut out);
        let again = parse(out.as_bytes()).unwrap();
        assert_eq!(root, again);
    }

    #[test]
    fn dtds_and_unknown_entities_are_refused() {
        assert!(parse(b"<!DOCTYPE a [<!ENTITY x 'y'>]><a>&x;</a>").is_err());
        assert!(parse(b"<a>&x;</a>").is_err());
        assert!(parse(b"<a><b></a>").is_err());
    }
}
