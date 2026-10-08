// SPDX-License-Identifier: EUPL-1.2
//! `GET`, `PUT`, `DELETE`, `PROPFIND`, `PROPPATCH`, `MKCOL` and
//! `MKCALENDAR`.

use axum::{
  http::{HeaderMap, HeaderValue, StatusCode, header},
  response::IntoResponse,
};

use super::{
  Ctx,
  Error,
  Result,
  empty,
  multistatus,
  properties::{self, PropRequest, propstat, quote, spec},
  target::Target,
  xml_response,
};
use crate::{
  props::Props,
  store::Kind,
  xml::{self, CALDAV, DAV, Element, ICAL, Node, QName, node},
};

pub fn get(ctx: &mut Ctx, target: &Target) -> Result {
  let Target::Item(id, name) = target else {
    return Err(Error::Status(StatusCode::METHOD_NOT_ALLOWED));
  };
  let headers = ctx.headers;
  let collection = ctx.collection(id)?;
  let item = collection.get(name).ok_or(Error::NOT_FOUND)?;
  let etag = (header::ETAG, quote(&item.etag));
  if let Some(status) = failed_precondition(headers, Some(&item.etag), true) {
    return Ok((status, [etag]).into_response());
  }
  let content_type =
    (header::CONTENT_TYPE, spec(id.kind).content_type.to_owned());
  Ok(([etag, content_type], collection.read(name)?).into_response())
}

pub fn put(ctx: &mut Ctx, target: &Target, body: &[u8]) -> Result {
  let Target::Item(id, name) = target else {
    return Err(Error::Status(StatusCode::METHOD_NOT_ALLOWED));
  };
  let collection = ctx
    .store
    .collection(id)?
    .ok_or(Error::Status(StatusCode::CONFLICT))?;
  let current = collection.get(name).map(|item| item.etag.clone());
  check_preconditions(ctx.headers, current.as_deref())?;
  let etag = collection.put(name, body)?;
  let status = if current.is_some() {
    StatusCode::NO_CONTENT
  } else {
    StatusCode::CREATED
  };
  Ok((status, [(header::ETAG, quote(&etag))]).into_response())
}

pub fn delete(ctx: &mut Ctx, target: &Target) -> Result {
  match target {
    Target::Item(id, name) => {
      let headers = ctx.headers;
      let collection = ctx.collection(id)?;
      let item = collection.get(name).ok_or(Error::NOT_FOUND)?;
      check_preconditions(headers, Some(&item.etag))?;
      collection.delete(name)?;
    },
    Target::Collection(id) => {
      if !ctx.store.trash(id)? {
        return Err(Error::NOT_FOUND);
      }
    },
    _ => return Err(Error::Status(StatusCode::FORBIDDEN)),
  }
  Ok(empty(StatusCode::NO_CONTENT))
}

pub fn propfind(ctx: &mut Ctx, target: &Target, body: &[u8]) -> Result {
  let depth = ctx.headers.get("Depth").map(HeaderValue::as_bytes);
  let recurse = match depth {
    Some(b"0") => false,
    Some(b"1") => true,
    _ => return Err(Error::Precondition(DAV, "propfind-finite-depth")),
  };
  let root = parse_body(body, DAV, "propfind", StatusCode::BAD_REQUEST)?;
  let request = PropRequest::new(root.as_ref(), ctx.headers);

  let props =
    properties::props_of(ctx, target, &request)?.ok_or(Error::NOT_FOUND)?;
  let mut responses = vec![request.response(&target.href(), props)];
  if recurse {
    for child in properties::children(ctx, target)? {
      if let Some(props) = properties::props_of(ctx, &child, &request)? {
        responses.push(request.response(&child.href(), props));
      }
    }
  }
  Ok(multistatus(responses))
}

pub fn proppatch(ctx: &mut Ctx, target: &Target, body: &[u8]) -> Result {
  let Target::Collection(id) = target else {
    return Err(Error::Status(StatusCode::FORBIDDEN));
  };
  let root = parse_body(body, DAV, "propertyupdate", StatusCode::BAD_REQUEST)?
    .ok_or(Error::Status(StatusCode::BAD_REQUEST))?;
  let collection = ctx.collection(id)?;

  let mut props = collection.props().clone();
  let mut outcome = Outcome::default();
  for update in &root.children {
    let set = update.is(DAV, "set");
    if set || update.is(DAV, "remove") {
      for prop in props_in(update) {
        let value = set.then(|| prop.text.clone());
        outcome.record(prop, set_prop(&mut props, id.kind, prop, value));
      }
    }
  }
  if outcome.succeeded() {
    collection.set_props(props)?;
  }
  Ok(multistatus(vec![
    node(DAV, "response")
      .child(xml::href(&target.href()))
      .children(outcome.propstats()),
  ]))
}

/// The two ways to create a collection. Their bodies only differ in element
/// names.
#[derive(Clone, Copy)]
pub enum Create {
  /// Extended MKCOL (RFC 5689), for calendars and address books.
  Mkcol,
  /// MKCALENDAR (RFC 4791), for calendars only.
  Mkcalendar,
}

impl Create {
  /// Request and response root elements.
  const fn elements(self) -> (QName, QName) {
    match self {
      Self::Mkcol => ((DAV, "mkcol"), (DAV, "mkcol-response")),
      Self::Mkcalendar => {
        ((CALDAV, "mkcalendar"), (CALDAV, "mkcalendar-response"))
      },
    }
  }
}

pub fn create(
  ctx: &mut Ctx,
  target: &Target,
  body: &[u8],
  method: Create,
) -> Result {
  let Target::Collection(id) = target else {
    return Err(Error::Status(StatusCode::FORBIDDEN));
  };
  if matches!(method, Create::Mkcalendar) && id.kind != Kind::Calendar {
    return Err(Error::Status(StatusCode::FORBIDDEN));
  }
  let ((root_ns, root_name), (response_ns, response_name)) = method.elements();
  let mut props = Props::default();
  let root =
    parse_body(body, root_ns, root_name, StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
  if let Some(root) = root {
    let mut outcome = Outcome::default();
    for prop in root.children_named(DAV, "set").flat_map(props_in) {
      outcome.record(prop, set_initial_prop(&mut props, id.kind, prop));
    }
    if !outcome.succeeded() {
      return Ok(xml_response(
        StatusCode::FORBIDDEN,
        &node(response_ns, response_name).children(outcome.propstats()),
      ));
    }
  }

  if ctx.store.create(id, &props)? {
    Ok(empty(StatusCode::CREATED))
  } else {
    Err(Error::Status(StatusCode::METHOD_NOT_ALLOWED))
  }
}

/// Parses a request body that must be empty or have the given root element.
fn parse_body(
  body: &[u8],
  ns: &str,
  local: &str,
  wrong_root: StatusCode,
) -> Result<Option<Element>> {
  if body.trim_ascii().is_empty() {
    return Ok(None);
  }
  let root = xml::parse(body)?;
  if !root.is(ns, local) {
    return Err(Error::Status(wrong_root));
  }
  Ok(Some(root))
}

/// The properties inside an element's `DAV:prop` children.
fn props_in(element: &Element) -> impl Iterator<Item = &Element> {
  element
    .children_named(DAV, "prop")
    .flat_map(|prop| &prop.children)
}

/// Sets (or with `None` removes) a writable collection property. `false` if
/// it isn't writable.
fn set_prop(
  props: &mut Props,
  kind: Kind,
  prop: &Element,
  value: Option<String>,
) -> bool {
  let name = (prop.name.ns.as_str(), prop.name.local.as_str());
  let field = if name == (DAV, "displayname") {
    &mut props.displayname
  } else if name == spec(kind).description {
    &mut props.description
  } else if name == (ICAL, "calendar-color") {
    &mut props.color
  } else {
    return false;
  };
  *field = value;
  true
}

/// Like `set_prop`, plus the properties only collection creation may set.
fn set_initial_prop(props: &mut Props, kind: Kind, prop: &Element) -> bool {
  let collection_type = spec(kind).collection_type;
  if prop.is(DAV, "resourcetype") {
    prop.child(collection_type.0, collection_type.1).is_some()
      && prop.children.iter().all(|resource| {
        resource.is(DAV, "collection")
          || resource.is(collection_type.0, collection_type.1)
      })
  } else if kind == Kind::Calendar
    && prop.is(CALDAV, "supported-calendar-component-set")
  {
    let components = prop
      .children_named(CALDAV, "comp")
      .filter_map(|comp| comp.attr("name"))
      .map(str::to_owned)
      .collect();
    props.components = Some(components);
    true
  } else if kind == Kind::Calendar
    && (prop.is(CALDAV, "calendar-timezone")
      || prop.is(CALDAV, "calendar-timezone-id"))
  {
    // DAVx⁵ sends the device's time zone. It's only a default for floating
    // times, which caesar never interprets, so it's accepted and dropped.
    true
  } else {
    set_prop(props, kind, prop, Some(prop.text.clone()))
  }
}

/// Per-property results of an all-or-nothing `PROPPATCH`, `MKCOL` or
/// `MKCALENDAR`.
#[derive(Default)]
struct Outcome {
  accepted: Vec<Node>,
  rejected: Vec<Node>,
}

impl Outcome {
  fn record(&mut self, prop: &Element, accepted: bool) {
    let name = node(&prop.name.ns, &prop.name.local);
    if accepted {
      self.accepted.push(name);
    } else {
      self.rejected.push(name);
    }
  }

  const fn succeeded(&self) -> bool {
    self.rejected.is_empty()
  }

  /// 200 for everything, or 403 for the rejected properties and 424 for
  /// the ones that failed along with them.
  fn propstats(self) -> Vec<Node> {
    if self.succeeded() {
      return vec![propstat(StatusCode::OK, self.accepted)];
    }
    let mut propstats = vec![propstat(StatusCode::FORBIDDEN, self.rejected)];
    if !self.accepted.is_empty() {
      propstats.push(propstat(StatusCode::FAILED_DEPENDENCY, self.accepted));
    }
    propstats
  }
}

/// RFC 9110 §13.2.2: evaluates `If-Match`, then `If-None-Match`, against
/// the current ETag. Returns the status to fail with; a matching
/// `If-None-Match` on a safe method means 304.
fn failed_precondition(
  headers: &HeaderMap,
  current: Option<&str>,
  safe: bool,
) -> Option<StatusCode> {
  let header = |name| headers.get(name).and_then(|value| value.to_str().ok());
  if header(header::IF_MATCH).is_some_and(|tags| !matches(tags, current, false))
  {
    return Some(StatusCode::PRECONDITION_FAILED);
  }
  if header(header::IF_NONE_MATCH)
    .is_some_and(|tags| matches(tags, current, true))
  {
    return Some(if safe {
      StatusCode::NOT_MODIFIED
    } else {
      StatusCode::PRECONDITION_FAILED
    });
  }
  None
}

fn check_preconditions(
  headers: &HeaderMap,
  current: Option<&str>,
) -> Result<()> {
  failed_precondition(headers, current, false)
    .map_or(Ok(()), |status| Err(Error::Status(status)))
}

/// Whether an ETag list matches the current ETag. `If-Match` uses strong
/// comparison, `If-None-Match` weak comparison.
fn matches(tags: &str, current: Option<&str>, weak: bool) -> bool {
  let Some(current) = current else {
    return false;
  };
  let current = quote(current);
  tags.split(',').map(str::trim).any(|tag| {
    let tag = if weak {
      tag.strip_prefix("W/").unwrap_or(tag)
    } else {
      tag
    };
    tag == "*" || tag == current
  })
}
