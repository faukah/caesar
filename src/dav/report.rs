// SPDX-License-Identifier: EUPL-1.2
//! `REPORT`: `sync-collection`, `*-multiget` and `*-query`.

use super::{
  Ctx,
  Error,
  Result,
  multistatus,
  properties::{
    PropRequest,
    SYNC_COLLECTION,
    item_props,
    not_found_response,
    spec,
  },
  target::{Target, href_path, item_href},
};
use crate::{
  store::{Collection, CollectionId, Kind},
  xml::{self, CALDAV, DAV, Element, Node, node},
};

const UNSUPPORTED: Error = Error::Precondition(DAV, "supported-report");

pub fn report(ctx: &mut Ctx, target: &Target, body: &[u8]) -> Result {
  let Target::Collection(id) = target else {
    return Err(UNSUPPORTED);
  };
  let root = xml::parse(body)?;
  let request = PropRequest::new(Some(&root), ctx.headers);
  let collection = ctx.collection(id)?;
  let report = Report {
    id,
    collection,
    request: &request,
  };

  let spec = spec(id.kind);
  let name = (root.name.ns.as_str(), root.name.local.as_str());
  let responses = if name == SYNC_COLLECTION {
    report.sync_collection(&root)?
  } else if name == spec.multiget {
    report.multiget(&root)?
  } else if name == spec.query {
    report.query(&root)?
  } else {
    return Err(UNSUPPORTED);
  };
  Ok(multistatus(responses))
}

struct Report<'a> {
  id:         &'a CollectionId,
  collection: &'a Collection,
  request:    &'a PropRequest,
}

impl Report<'_> {
  /// Changed items with their properties, deleted ones as 404, then the new
  /// token.
  fn sync_collection(&self, root: &Element) -> Result<Vec<Node>> {
    let token = root
      .child(DAV, "sync-token")
      .map_or("", |token| token.text.trim());
    let changes = self
      .collection
      .changes_since(token)
      .ok_or(Error::Precondition(DAV, "valid-sync-token"))?;
    let mut responses = changes
      .into_iter()
      .map(|item| self.respond(&item_href(self.id, item), item))
      .collect::<Result<Vec<_>>>()?;
    responses.push(node(DAV, "sync-token").text(self.collection.token()));
    Ok(responses)
  }

  /// The requested hrefs, echoed back as sent.
  fn multiget(&self, root: &Element) -> Result<Vec<Node>> {
    root
      .children_named(DAV, "href")
      .map(|href| {
        let href = href.text.trim();
        match Target::parse(href_path(href)) {
          Some(Target::Item(id, item)) if id == *self.id => {
            self.respond(href, &item)
          },
          _ => Ok(not_found_response(href)),
        }
      })
      .collect()
  }

  /// Every item, filtered only by the component type a calendar query
  /// names.
  fn query(&self, root: &Element) -> Result<Vec<Node>> {
    let components = match self.id.kind {
      Kind::Calendar => query_components(root),
      Kind::Contacts => Vec::new(),
    };
    let mut responses = Vec::new();
    for item in self.collection.items().keys() {
      if !components.is_empty() {
        let data = self.collection.read(item)?;
        if !components.iter().any(|comp| has_component(&data, comp)) {
          continue;
        }
      }
      responses.push(self.respond(&item_href(self.id, item), item)?);
    }
    Ok(responses)
  }

  fn respond(&self, href: &str, item: &str) -> Result<Node> {
    let props = item_props(self.collection, self.id.kind, item, self.request)?;
    Ok(props.map_or_else(
      || not_found_response(href),
      |props| self.request.response(href, props),
    ))
  }
}

/// Component names from a `calendar-query` filter's `VCALENDAR` level. All
/// other filtering is ignored.
fn query_components(root: &Element) -> Vec<&str> {
  root
    .child(CALDAV, "filter")
    .and_then(|filter| filter.child(CALDAV, "comp-filter"))
    .filter(|calendar| calendar.attr("name") == Some("VCALENDAR"))
    .map(|calendar| {
      calendar
        .children_named(CALDAV, "comp-filter")
        .filter_map(|comp| comp.attr("name"))
        .collect()
    })
    .unwrap_or_default()
}

/// Whether raw iCalendar data contains a `BEGIN:<component>` line.
fn has_component(data: &[u8], component: &str) -> bool {
  let begin = format!("BEGIN:{component}");
  data
    .split(|&b| b == b'\n')
    .any(|line| line.trim_ascii().eq_ignore_ascii_case(begin.as_bytes()))
}
