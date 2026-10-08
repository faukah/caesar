// SPDX-License-Identifier: EUPL-1.2
//! Collection metadata stored in `.props.toml`.

use std::{fs, io, path::Path};

use serde::{Deserialize, Serialize};

use crate::store;

const FILE: &str = ".props.toml";

const DEFAULT_COMPONENTS: [&str; 2] = ["VEVENT", "VTODO"];

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Props {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub displayname: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub color:       Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub components:  Option<Vec<String>>,
}

impl Props {
  /// Loads the props of the collection in `dir`. A missing file means
  /// defaults.
  pub fn load(dir: &Path) -> io::Result<Self> {
    match fs::read_to_string(dir.join(FILE)) {
      Ok(text) => toml::from_str(&text).map_err(io::Error::other),
      Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
      Err(err) => Err(err),
    }
  }

  pub fn save(&self, dir: &Path) -> io::Result<()> {
    let text = toml::to_string(self).map_err(io::Error::other)?;
    store::write_atomic(dir, FILE, text.as_bytes())
  }

  /// Calendar components the collection advertises.
  pub fn components(&self) -> Vec<&str> {
    self.components.as_ref().map_or_else(
      || DEFAULT_COMPONENTS.to_vec(),
      |components| components.iter().map(String::as_str).collect(),
    )
  }
}
