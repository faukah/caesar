// SPDX-License-Identifier: EUPL-1.2
//! Request parsing into a small element tree and response writing.
//!
//! Requests are read with `NsReader`, so elements are identified by
//! namespace and local name regardless of the prefixes a client picked.
//! Responses use fixed prefixes declared on the root element.

use std::{fmt, io};

use quick_xml::{
  NsReader,
  Writer,
  XmlVersion,
  escape::{escape, resolve_predefined_entity},
  events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event},
  name::{Namespace, ResolveResult},
};

pub const DAV: &str = "DAV:";
pub const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
pub const CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";
pub const ICAL: &str = "http://apple.com/ns/ical/";

const PREFIXES: [(&str, &str); 4] =
  [(DAV, "d"), (CALDAV, "c"), (CARDDAV, "card"), (ICAL, "ical")];

const MAX_DEPTH: usize = 64;

/// A namespaced name known at compile time.
pub type QName = (&'static str, &'static str);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
  pub ns:    String,
  pub local: String,
}

impl Name {
  pub fn is(&self, ns: &str, local: &str) -> bool {
    self.ns == ns && self.local == local
  }
}

/// A parsed request element.
#[derive(Debug)]
pub struct Element {
  pub name:     Name,
  pub attrs:    Vec<(String, String)>,
  pub children: Vec<Self>,
  pub text:     String,
}

impl Element {
  pub fn is(&self, ns: &str, local: &str) -> bool {
    self.name.is(ns, local)
  }

  pub fn child(&self, ns: &str, local: &str) -> Option<&Self> {
    self.children.iter().find(|child| child.is(ns, local))
  }

  pub fn children_named<'a>(
    &'a self,
    ns: &'a str,
    local: &'a str,
  ) -> impl Iterator<Item = &'a Self> {
    self
      .children
      .iter()
      .filter(move |child| child.is(ns, local))
  }

  /// Value of an unqualified attribute.
  pub fn attr(&self, name: &str) -> Option<&str> {
    self
      .attrs
      .iter()
      .find(|(key, _)| key == name)
      .map(|(_, value)| value.as_str())
  }
}

#[derive(Debug)]
pub struct ParseError(String);

impl fmt::Display for ParseError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

impl From<quick_xml::Error> for ParseError {
  fn from(err: quick_xml::Error) -> Self {
    Self(err.to_string())
  }
}

impl From<quick_xml::events::attributes::AttrError> for ParseError {
  fn from(err: quick_xml::events::attributes::AttrError) -> Self {
    Self(err.to_string())
  }
}

fn error(message: &str) -> ParseError {
  ParseError(message.to_owned())
}

pub fn parse(body: &[u8]) -> Result<Element, ParseError> {
  let mut reader = NsReader::from_reader(body);
  reader.config_mut().trim_text(true);
  let mut stack: Vec<Element> = Vec::new();
  loop {
    let (ns, event) = reader.read_resolved_event()?;
    let ns = match ns {
      ResolveResult::Bound(Namespace(ns)) => ns.to_owned(),
      ResolveResult::Unbound => String::new(),
      ResolveResult::Unknown(_) => return Err(error("unknown prefix")),
    };
    let finished = match event {
      Event::Start(start) => {
        if stack.len() == MAX_DEPTH {
          return Err(error("nested too deeply"));
        }
        stack.push(element(ns, &start)?);
        None
      },
      Event::Empty(start) => Some(element(ns, &start)?),
      Event::End(_) => stack.pop(),
      Event::Text(text) => {
        push_text(&mut stack, &text.xml_content(XmlVersion::Implicit1_0));
        None
      },
      Event::CData(text) => {
        push_text(&mut stack, &text.xml_content(XmlVersion::Implicit1_0));
        None
      },
      Event::GeneralRef(reference) => {
        let resolved = match reference.resolve_char_ref()? {
          Some(ch) => ch.to_string(),
          None => {
            resolve_predefined_entity(
              &reference.xml_content(XmlVersion::Implicit1_0),
            )
            .ok_or_else(|| error("unknown entity"))?
            .to_owned()
          },
        };
        push_text(&mut stack, &resolved);
        None
      },
      Event::Eof => return Err(error("unexpected end of document")),
      _ => None,
    };
    if let Some(element) = finished {
      match stack.last_mut() {
        Some(parent) => parent.children.push(element),
        None => return Ok(element),
      }
    }
  }
}

fn element(ns: String, start: &BytesStart) -> Result<Element, ParseError> {
  let mut attrs = Vec::new();
  for attr in start.attributes() {
    let attr = attr?;
    let key = attr.key.local_name().as_ref().to_owned();
    let value = attr.normalized_value(XmlVersion::Implicit1_0)?.into_owned();
    attrs.push((key, value));
  }
  Ok(Element {
    name: Name {
      ns,
      local: start.local_name().as_ref().to_owned(),
    },
    attrs,
    children: Vec::new(),
    text: String::new(),
  })
}

fn push_text(stack: &mut [Element], text: &str) {
  if let Some(top) = stack.last_mut() {
    top.text.push_str(text);
  }
}

/// A response element.
#[derive(Debug, Clone)]
pub struct Node {
  pub name: Name,
  attrs:    Vec<(&'static str, String)>,
  children: Vec<Self>,
  text:     Option<String>,
}

pub fn node(ns: &str, local: &str) -> Node {
  Node {
    name:     Name {
      ns:    ns.to_owned(),
      local: local.to_owned(),
    },
    attrs:    Vec::new(),
    children: Vec::new(),
    text:     None,
  }
}

pub fn href(href: &str) -> Node {
  node(DAV, "href").text(href)
}

impl Node {
  pub fn child(mut self, child: Self) -> Self {
    self.children.push(child);
    self
  }

  pub fn children(mut self, children: impl IntoIterator<Item = Self>) -> Self {
    self.children.extend(children);
    self
  }

  pub fn text(mut self, text: impl Into<String>) -> Self {
    self.text = Some(text.into());
    self
  }

  pub fn attr(mut self, key: &'static str, value: impl Into<String>) -> Self {
    self.attrs.push((key, value.into()));
    self
  }

  /// An empty element with the same name.
  pub fn name_only(&self) -> Self {
    node(&self.name.ns, &self.name.local)
  }
}

/// Serializes `root` as a complete document.
pub fn document(root: &Node) -> io::Result<Vec<u8>> {
  let mut writer = Writer::new(Vec::new());
  writer.write_event(Event::Decl(BytesDecl::new(
    "1.0",
    Some("utf-8"),
    None,
  )))?;
  write(&mut writer, root, true)?;
  Ok(writer.into_inner())
}

fn write(
  writer: &mut Writer<Vec<u8>>,
  node: &Node,
  root: bool,
) -> io::Result<()> {
  let prefix = PREFIXES
    .iter()
    .find(|(ns, _)| *ns == node.name.ns)
    .map(|(_, prefix)| *prefix);
  let qname = prefix.map_or_else(
    || node.name.local.clone(),
    |prefix| format!("{prefix}:{}", node.name.local),
  );

  let mut start = BytesStart::new(qname.as_str());
  if root {
    for (ns, prefix) in PREFIXES {
      start.push_attribute((format!("xmlns:{prefix}").as_str(), ns));
    }
  }
  if prefix.is_none() {
    // Unknown namespace, e.g. an unsupported property echoed back in a 404
    // propstat. These never have children.
    start.push_attribute(("xmlns", node.name.ns.as_str()));
  }
  for (key, value) in &node.attrs {
    start.push_attribute((*key, value.as_str()));
  }

  if node.children.is_empty() && node.text.is_none() {
    return writer.write_event(Event::Empty(start));
  }
  writer.write_event(Event::Start(start))?;
  if let Some(text) = &node.text {
    // Escape CR too, otherwise the client's XML parser turns the CRLF line
    // endings of iCalendar and vCard data into LF.
    let escaped = escape(text.as_str()).replace('\r', "&#13;");
    writer.write_event(Event::Text(BytesText::from_escaped(escaped)))?;
  }
  for child in &node.children {
    write(writer, child, false)?;
  }
  writer.write_event(Event::End(BytesEnd::new(qname.as_str())))
}
