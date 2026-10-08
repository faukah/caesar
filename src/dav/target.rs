// SPDX-License-Identifier: EUPL-1.2
//! Mapping between request paths, resources and hrefs.
//!
//! ```text
//! /                               Root
//! /<user>/                        Principal
//! /<user>/<kind>/                 Home
//! /<user>/<kind>/<name>/          Collection
//! /<user>/<kind>/<name>/<item>    Item
//! ```

use crate::store::{CollectionId, Kind, valid_name};

pub enum Target {
  Root,
  Principal(String),
  Home(String, Kind),
  Collection(CollectionId),
  Item(CollectionId, String),
}

impl Target {
  pub fn parse(path: &str) -> Option<Self> {
    let segments = path
      .split('/')
      .filter(|segment| !segment.is_empty())
      .map(percent_decode)
      .collect::<Option<Vec<_>>>()?;
    if !segments.iter().all(|segment| valid_name(segment)) {
      return None;
    }
    let mut segments = segments.into_iter();
    let Some(user) = segments.next() else {
      return Some(Self::Root);
    };
    let Some(kind) = segments.next() else {
      return Some(Self::Principal(user));
    };
    let kind = Kind::from_dir(&kind)?;
    let Some(name) = segments.next() else {
      return Some(Self::Home(user, kind));
    };
    let id = CollectionId { user, kind, name };
    match (segments.next(), segments.next()) {
      (None, _) => Some(Self::Collection(id)),
      (Some(item), None) => Some(Self::Item(id, item)),
      (Some(_), Some(_)) => None,
    }
  }

  pub fn user(&self) -> Option<&str> {
    match self {
      Self::Root => None,
      Self::Principal(user) | Self::Home(user, _) => Some(user),
      Self::Collection(id) | Self::Item(id, _) => Some(&id.user),
    }
  }

  pub fn href(&self) -> String {
    match self {
      Self::Root => "/".to_owned(),
      Self::Principal(user) => principal_href(user),
      Self::Home(user, kind) => home_href(user, *kind),
      Self::Collection(id) => collection_href(id),
      Self::Item(id, item) => item_href(id, item),
    }
  }
}

pub fn is_well_known(path: &str) -> bool {
  matches!(
    path.trim_end_matches('/'),
    "/.well-known/caldav" | "/.well-known/carddav"
  )
}

pub fn principal_href(user: &str) -> String {
  format!("/{user}/")
}

pub fn home_href(user: &str, kind: Kind) -> String {
  format!("/{user}/{}/", kind.dir())
}

pub fn collection_href(id: &CollectionId) -> String {
  format!("/{}/{}/{}/", id.user, id.kind.dir(), id.name)
}

pub fn item_href(id: &CollectionId, item: &str) -> String {
  format!("{}{item}", collection_href(id))
}

/// The path of an href, which may be a full URL.
pub fn href_path(href: &str) -> &str {
  match href.split_once("://") {
    Some((_, rest)) => rest.find('/').map_or("/", |start| &rest[start..]),
    None => href,
  }
}

fn percent_decode(segment: &str) -> Option<String> {
  let bytes = segment.as_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'%' {
      let hex = segment.get(i + 1..i + 3)?;
      out.push(u8::from_str_radix(hex, 16).ok()?);
      i += 3;
    } else {
      out.push(bytes[i]);
      i += 1;
    }
  }
  String::from_utf8(out).ok()
}
