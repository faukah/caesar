// SPDX-License-Identifier: EUPL-1.2
//! Live properties of every resource and `DAV:response` building.

use axum::http::{HeaderMap, StatusCode};

use super::{
  Ctx,
  MAX_BODY,
  Result,
  target::{Target, home_href, principal_href},
};
use crate::{
  store::{Collection, CollectionId, Kind},
  xml::{self, CALDAV, CARDDAV, DAV, Element, ICAL, Node, QName, node},
};

pub const SYNC_COLLECTION: QName = (DAV, "sync-collection");

/// How calendars and address books differ on the wire.
pub struct Spec {
  /// Resource type of a collection, besides `DAV:collection`.
  pub collection_type: QName,
  pub description:     QName,
  /// Property carrying an item's raw data in reports.
  pub data:            QName,
  pub multiget:        QName,
  pub query:           QName,
  max_resource_size:   QName,
  pub content_type:    &'static str,
  home_name:           &'static str,
}

const CALENDAR: Spec = Spec {
  collection_type:   (CALDAV, "calendar"),
  description:       (CALDAV, "calendar-description"),
  data:              (CALDAV, "calendar-data"),
  multiget:          (CALDAV, "calendar-multiget"),
  query:             (CALDAV, "calendar-query"),
  max_resource_size: (CALDAV, "max-resource-size"),
  content_type:      "text/calendar; charset=utf-8",
  home_name:         "Calendars",
};

const CONTACTS: Spec = Spec {
  collection_type:   (CARDDAV, "addressbook"),
  description:       (CARDDAV, "addressbook-description"),
  data:              (CARDDAV, "address-data"),
  multiget:          (CARDDAV, "addressbook-multiget"),
  query:             (CARDDAV, "addressbook-query"),
  max_resource_size: (CARDDAV, "max-resource-size"),
  content_type:      "text/vcard; charset=utf-8",
  home_name:         "Contacts",
};

pub const fn spec(kind: Kind) -> &'static Spec {
  match kind {
    Kind::Calendar => &CALENDAR,
    Kind::Contacts => &CONTACTS,
  }
}

/// RFC 8144: `Prefer: return=minimal` drops 404 propstats.
pub fn prefers_minimal(headers: &HeaderMap) -> bool {
  headers
    .get_all("Prefer")
    .iter()
    .filter_map(|value| value.to_str().ok())
    .flat_map(|value| value.split(','))
    .any(|pref| pref.trim().eq_ignore_ascii_case("return=minimal"))
}

/// Which properties a `PROPFIND` or `REPORT` asks for, and how.
pub struct PropRequest {
  selection: Selection,
  minimal:   bool,
}

enum Selection {
  All,
  Names,
  Only(Vec<xml::Name>),
}

impl PropRequest {
  /// From the request's root element; `None` (an empty `PROPFIND` body)
  /// means all properties.
  pub fn new(root: Option<&Element>, headers: &HeaderMap) -> Self {
    let selection = match root {
      None => Selection::All,
      Some(root) if root.child(DAV, "propname").is_some() => Selection::Names,
      Some(root) => {
        root.child(DAV, "prop").map_or(Selection::All, |prop| {
          Selection::Only(
            prop.children.iter().map(|p| p.name.clone()).collect(),
          )
        })
      },
    };
    Self {
      selection,
      minimal: prefers_minimal(headers),
    }
  }

  /// Whether `name` was asked for explicitly. Expensive properties such as
  /// item data are only returned then, never for `allprop`.
  pub fn wants(&self, (ns, local): QName) -> bool {
    match &self.selection {
      Selection::Only(names) => names.iter().any(|name| name.is(ns, local)),
      Selection::All | Selection::Names => false,
    }
  }

  /// A `DAV:response` with the requested subset of `props`.
  pub fn response(&self, href: &str, props: Vec<Node>) -> Node {
    let (found, mut missing) = match &self.selection {
      Selection::All => (props, Vec::new()),
      Selection::Names => {
        (props.iter().map(Node::name_only).collect(), Vec::new())
      },
      Selection::Only(names) => {
        let mut found = Vec::new();
        let mut missing = Vec::new();
        for name in names {
          match props.iter().find(|prop| prop.name == *name) {
            Some(prop) => found.push(prop.clone()),
            None => missing.push(node(&name.ns, &name.local)),
          }
        }
        (found, missing)
      },
    };
    if self.minimal {
      missing.clear();
    }

    let mut response = node(DAV, "response").child(xml::href(href));
    if !found.is_empty() || missing.is_empty() {
      response = response.child(propstat(StatusCode::OK, found));
    }
    if !missing.is_empty() {
      response = response.child(propstat(StatusCode::NOT_FOUND, missing));
    }
    response
  }
}

pub fn propstat(status: StatusCode, props: Vec<Node>) -> Node {
  node(DAV, "propstat")
    .child(node(DAV, "prop").children(props))
    .child(status_node(status))
}

pub fn not_found_response(href: &str) -> Node {
  node(DAV, "response")
    .child(xml::href(href))
    .child(status_node(StatusCode::NOT_FOUND))
}

fn status_node(status: StatusCode) -> Node {
  node(DAV, "status").text(format!("HTTP/1.1 {status}"))
}

fn named((ns, local): QName) -> Node {
  node(ns, local)
}

fn resourcetype(types: &[QName]) -> Node {
  node(DAV, "resourcetype").children(types.iter().copied().map(named))
}

/// All properties of `target`, or `None` if it doesn't exist.
pub fn props_of(
  ctx: &mut Ctx,
  target: &Target,
  request: &PropRequest,
) -> Result<Option<Vec<Node>>> {
  let mut props = match target {
    Target::Item(id, item) => {
      let collection = ctx.collection(id)?;
      return Ok(item_props(collection, id.kind, item, request)?);
    },
    Target::Collection(id) => collection_props(id, ctx.collection(id)?),
    Target::Root => vec![resourcetype(&[(DAV, "collection")])],
    Target::Principal(user) => {
      vec![
        resourcetype(&[(DAV, "collection"), (DAV, "principal")]),
        node(DAV, "displayname").text(user),
        node(DAV, "principal-URL").child(xml::href(&principal_href(user))),
        node(CALDAV, "calendar-home-set")
          .child(xml::href(&home_href(user, Kind::Calendar))),
        node(CARDDAV, "addressbook-home-set")
          .child(xml::href(&home_href(user, Kind::Contacts))),
      ]
    },
    Target::Home(_, kind) => {
      vec![
        resourcetype(&[(DAV, "collection")]),
        node(DAV, "displayname").text(spec(*kind).home_name),
      ]
    },
  };
  props.extend(common_props(ctx.user));
  Ok(Some(props))
}

/// Members of `target` for `Depth: 1`.
pub fn children(ctx: &mut Ctx, target: &Target) -> Result<Vec<Target>> {
  Ok(match target {
    Target::Root => vec![Target::Principal(ctx.user.to_owned())],
    Target::Principal(user) => {
      vec![
        Target::Home(user.clone(), Kind::Calendar),
        Target::Home(user.clone(), Kind::Contacts),
      ]
    },
    Target::Home(user, kind) => {
      ctx
        .store
        .list(user, *kind)?
        .into_iter()
        .map(|name| {
          Target::Collection(CollectionId {
            user: user.clone(),
            kind: *kind,
            name,
          })
        })
        .collect()
    },
    Target::Collection(id) => {
      ctx
        .collection(id)?
        .items()
        .keys()
        .map(|item| Target::Item(id.clone(), item.clone()))
        .collect()
    },
    Target::Item(..) => Vec::new(),
  })
}

/// Properties of an item, including its data if asked for. `None` if it
/// doesn't exist.
pub fn item_props(
  collection: &Collection,
  kind: Kind,
  name: &str,
  request: &PropRequest,
) -> std::io::Result<Option<Vec<Node>>> {
  let Some(item) = collection.get(name) else {
    return Ok(None);
  };
  let spec = spec(kind);
  let mut props = vec![
    resourcetype(&[]),
    node(DAV, "getetag").text(quote(&item.etag)),
    node(DAV, "getcontenttype").text(spec.content_type),
    node(DAV, "getcontentlength").text(item.size.to_string()),
  ];
  if request.wants(spec.data) {
    let data = collection.read(name)?;
    props.push(named(spec.data).text(String::from_utf8_lossy(&data)));
  }
  Ok(Some(props))
}

pub fn quote(etag: &str) -> String {
  format!("\"{etag}\"")
}

/// Properties of every resource except items.
fn common_props(user: &str) -> [Node; 2] {
  // ACLs aren't implemented; report full access so clients don't treat
  // collections as read-only.
  let privileges = [
    "all",
    "read",
    "write",
    "write-properties",
    "write-content",
    "bind",
    "unbind",
  ];
  [
    node(DAV, "current-user-principal").child(xml::href(&principal_href(user))),
    node(DAV, "current-user-privilege-set").children(
      privileges
        .map(|privilege| node(DAV, "privilege").child(node(DAV, privilege))),
    ),
  ]
}

fn collection_props(id: &CollectionId, collection: &Collection) -> Vec<Node> {
  let spec = spec(id.kind);
  let props = collection.props();
  let reports = [SYNC_COLLECTION, spec.multiget, spec.query].map(|report| {
    node(DAV, "supported-report")
      .child(node(DAV, "report").child(named(report)))
  });

  let mut out = vec![
    resourcetype(&[(DAV, "collection"), spec.collection_type]),
    node(DAV, "displayname")
      .text(props.displayname.as_deref().unwrap_or(&id.name)),
    node(DAV, "sync-token").text(collection.token()),
    node(DAV, "supported-report-set").children(reports),
    named(spec.max_resource_size).text(MAX_BODY.to_string()),
  ];
  if let Some(description) = &props.description {
    out.push(named(spec.description).text(description));
  }
  if let Some(color) = &props.color {
    out.push(node(ICAL, "calendar-color").text(color));
  }
  match id.kind {
    Kind::Calendar => {
      out.push(
        node(CALDAV, "supported-calendar-component-set").children(
          props
            .components()
            .into_iter()
            .map(|comp| node(CALDAV, "comp").attr("name", comp)),
        ),
      );
      out.push(
        node(CALDAV, "supported-calendar-data").child(
          node(CALDAV, "calendar-data")
            .attr("content-type", "text/calendar")
            .attr("version", "2.0"),
        ),
      );
    },
    Kind::Contacts => {
      out.push(
        node(CARDDAV, "supported-address-data").child(
          node(CARDDAV, "address-data-type")
            .attr("content-type", "text/vcard")
            .attr("version", "4.0"),
        ),
      );
    },
  }
  out
}
