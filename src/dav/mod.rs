// SPDX-License-Identifier: EUPL-1.2
//! WebDAV, CalDAV and CardDAV request handling.
//!
//! `handle` reads the body and takes the store lock; everything below it is
//! synchronous. `target` maps paths to resources, `properties` describes
//! them, `methods` and `report` implement the requests.

mod methods;
mod properties;
mod report;
mod target;

use std::{
  io,
  sync::{Mutex, PoisonError},
};

use axum::{
  extract::Request,
  http::{
    HeaderMap,
    HeaderName,
    HeaderValue,
    StatusCode,
    header,
    request::Parts,
  },
  response::{IntoResponse, Redirect, Response},
};
use tracing::{error, warn};

use self::{methods::Create, target::Target};
use crate::{
  store::{self, Collection, CollectionId, Store},
  xml::{self, DAV, Node, node},
};

/// Maximum request body and item size.
const MAX_BODY: usize = 10 * 1024 * 1024;

const DAV_HEADER: &str = "1, 3, calendar-access, addressbook, extended-mkcol";
const ALLOW: &str = "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, PROPPATCH, \
                     MKCOL, MKCALENDAR, REPORT";

pub async fn handle(store: &Mutex<Store>, request: Request) -> Response {
  let (parts, body) = request.into_parts();
  let Ok(body) = axum::body::to_bytes(body, MAX_BODY).await else {
    return empty(StatusCode::PAYLOAD_TOO_LARGE);
  };
  dispatch(store, &parts, &body).unwrap_or_else(Error::into_response)
}

fn dispatch(store: &Mutex<Store>, parts: &Parts, body: &[u8]) -> Result {
  let path = parts.uri.path();
  if target::is_well_known(path) {
    return Ok(Redirect::permanent("/").into_response());
  }
  if parts.method.as_str() == "OPTIONS" {
    return Ok(
      [
        (HeaderName::from_static("dav"), DAV_HEADER),
        (header::ALLOW, ALLOW),
      ]
      .into_response(),
    );
  }

  let target = Target::parse(path).ok_or(Error::NOT_FOUND)?;
  let user = remote_user(&parts.headers)?;
  if target.user().is_some_and(|owner| owner != user) {
    return Err(Error::Status(StatusCode::FORBIDDEN));
  }

  let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
  let response = run(&mut store, user, parts, &target, body);
  drop(store);
  let mut response = response?;
  if response.status() == StatusCode::MULTI_STATUS
    && properties::prefers_minimal(&parts.headers)
  {
    response.headers_mut().insert(
      "Preference-Applied",
      HeaderValue::from_static("return=minimal"),
    );
  }
  Ok(response)
}

/// Runs a method with the store lock held.
fn run(
  store: &mut Store,
  user: &str,
  parts: &Parts,
  target: &Target,
  body: &[u8],
) -> Result {
  store.provision(user)?;
  let mut ctx = Ctx {
    store,
    user,
    headers: &parts.headers,
  };
  match parts.method.as_str() {
    "GET" | "HEAD" => methods::get(&mut ctx, target),
    "PUT" => methods::put(&mut ctx, target, body),
    "DELETE" => methods::delete(&mut ctx, target),
    "PROPFIND" => methods::propfind(&mut ctx, target, body),
    "PROPPATCH" => methods::proppatch(&mut ctx, target, body),
    "MKCOL" => methods::create(&mut ctx, target, body, Create::Mkcol),
    "MKCALENDAR" => methods::create(&mut ctx, target, body, Create::Mkcalendar),
    "REPORT" => report::report(&mut ctx, target, body),
    _ => Err(Error::Status(StatusCode::METHOD_NOT_ALLOWED)),
  }
}

/// State of one request, holding the store lock.
struct Ctx<'a> {
  store:   &'a mut Store,
  user:    &'a str,
  headers: &'a HeaderMap,
}

impl Ctx<'_> {
  fn collection(&mut self, id: &CollectionId) -> Result<&mut Collection> {
    self.store.collection(id)?.ok_or(Error::NOT_FOUND)
  }
}

/// The username Caddy authenticated.
fn remote_user(headers: &HeaderMap) -> Result<&str> {
  let user = headers
    .get("Remote-User")
    .and_then(|user| user.to_str().ok())
    .filter(|user| store::valid_name(user));
  user.ok_or_else(|| {
    warn!("request without a valid Remote-User header");
    Error::Status(StatusCode::FORBIDDEN)
  })
}

enum Error {
  Status(StatusCode),
  /// A failed pre- or postcondition: 403 with a `DAV:error` body naming it.
  Precondition(&'static str, &'static str),
  Xml(xml::ParseError),
  Io(io::Error),
}

type Result<T = Response> = std::result::Result<T, Error>;

impl Error {
  const NOT_FOUND: Self = Self::Status(StatusCode::NOT_FOUND);

  fn into_response(self) -> Response {
    match self {
      Self::Status(status) => empty(status),
      Self::Precondition(ns, name) => {
        xml_response(
          StatusCode::FORBIDDEN,
          &node(DAV, "error").child(node(ns, name)),
        )
      },
      Self::Xml(err) => {
        warn!(%err, "malformed request body");
        empty(StatusCode::BAD_REQUEST)
      },
      Self::Io(err) => {
        error!(%err, "I/O error");
        empty(StatusCode::INTERNAL_SERVER_ERROR)
      },
    }
  }
}

impl From<io::Error> for Error {
  fn from(err: io::Error) -> Self {
    Self::Io(err)
  }
}

impl From<xml::ParseError> for Error {
  fn from(err: xml::ParseError) -> Self {
    Self::Xml(err)
  }
}

fn empty(status: StatusCode) -> Response {
  if status == StatusCode::METHOD_NOT_ALLOWED {
    (status, [(header::ALLOW, ALLOW)]).into_response()
  } else {
    status.into_response()
  }
}

fn xml_response(status: StatusCode, root: &Node) -> Response {
  match xml::document(root) {
    Ok(body) => {
      let content_type =
        (header::CONTENT_TYPE, "application/xml; charset=utf-8");
      (status, [content_type], body).into_response()
    },
    Err(err) => Error::Io(err).into_response(),
  }
}

fn multistatus(responses: Vec<Node>) -> Response {
  xml_response(
    StatusCode::MULTI_STATUS,
    &node(DAV, "multistatus").children(responses),
  )
}
