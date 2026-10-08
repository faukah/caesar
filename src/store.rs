// SPDX-License-Identifier: EUPL-1.2
//! On-disk layout, per-collection index and sync log.

use std::{
  collections::{BTreeMap, HashMap, hash_map::Entry},
  fs::{self, File, OpenOptions},
  io::{self, Write},
  path::{Path, PathBuf},
  time::SystemTime,
};

use tracing::{info, warn};
use uuid::Uuid;

use crate::props::Props;

const LOG: &str = ".sync.log";
const TRASH: &str = ".trash";
const TOKEN_PREFIX: &str = "tag:caesar,2026:sync/";
const DEFAULT_COLLECTION: &str = "default";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
  Calendar,
  Contacts,
}

impl Kind {
  /// Directory name of the home set, which is also its URL segment.
  pub const fn dir(self) -> &'static str {
    match self {
      Self::Calendar => "calendars",
      Self::Contacts => "contacts",
    }
  }

  pub fn from_dir(dir: &str) -> Option<Self> {
    match dir {
      "calendars" => Some(Self::Calendar),
      "contacts" => Some(Self::Contacts),
      _ => None,
    }
  }
}

/// User, collection and item names: `[A-Za-z0-9._@-]+`, no leading dot (dot
/// files are metadata), at most 255 bytes.
pub fn valid_name(name: &str) -> bool {
  !name.is_empty()
    && name.len() <= 255
    && !name.starts_with('.')
    && name
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || b"._@-".contains(&b))
}

/// A calendar or address book of a user.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CollectionId {
  pub user: String,
  pub kind: Kind,
  pub name: String,
}

#[derive(Debug, Clone)]
pub struct Item {
  pub etag: String,
  pub size: u64,
}

impl Item {
  fn new(data: &[u8]) -> Self {
    Self {
      etag: blake3::hash(data).to_hex()[..32].to_owned(),
      size: data.len() as u64,
    }
  }
}

/// A loaded calendar or address book: its props, items and change history.
pub struct Collection {
  dir:        PathBuf,
  props:      Props,
  log:        File,
  generation: String,
  seq:        u64,
  items:      BTreeMap<String, Item>,
  /// Sequence number of the last change (put or delete) per name.
  changed:    HashMap<String, u64>,
}

/// Replayed contents of a `.sync.log`.
struct History {
  generation: String,
  seq:        u64,
  /// Last change per name: its sequence number, and the ETag if it was a
  /// put.
  last:       HashMap<String, (u64, Option<String>)>,
}

impl Collection {
  /// Loads the log, hashes every item and logs whatever changed on disk
  /// since the log was last written.
  fn load(dir: &Path) -> io::Result<Self> {
    let history = if let Some(history) = read_log(dir)? {
      history
    } else {
      let generation = Uuid::now_v7().to_string();
      info!(dir = %dir.display(), %generation, "starting new sync log");
      write_atomic(dir, LOG, format!("{generation}\n").as_bytes())?;
      History {
        generation,
        seq: 0,
        last: HashMap::new(),
      }
    };

    let mut collection = Self {
      dir:        dir.to_owned(),
      props:      Props::load(dir)?,
      log:        OpenOptions::new().append(true).open(dir.join(LOG))?,
      generation: history.generation,
      seq:        history.seq,
      items:      BTreeMap::new(),
      changed:    history
        .last
        .iter()
        .map(|(name, (seq, _))| (name.clone(), *seq))
        .collect(),
    };

    let items = scan(dir)?;
    for (name, item) in &items {
      let logged = history.last.get(name).and_then(|(_, etag)| etag.as_ref());
      if logged != Some(&item.etag) {
        collection.append(name, Some(&item.etag))?;
      }
    }
    for (name, (_, etag)) in &history.last {
      if etag.is_some() && !items.contains_key(name) {
        collection.append(name, None)?;
      }
    }
    collection.items = items;
    Ok(collection)
  }

  pub const fn props(&self) -> &Props {
    &self.props
  }

  pub fn set_props(&mut self, props: Props) -> io::Result<()> {
    props.save(&self.dir)?;
    self.props = props;
    Ok(())
  }

  pub fn token(&self) -> String {
    format!("{TOKEN_PREFIX}{}/{}", self.generation, self.seq)
  }

  pub const fn items(&self) -> &BTreeMap<String, Item> {
    &self.items
  }

  pub fn get(&self, name: &str) -> Option<&Item> {
    self.items.get(name)
  }

  pub fn read(&self, name: &str) -> io::Result<Vec<u8>> {
    fs::read(self.dir.join(name))
  }

  /// Stores `data` as `name` and returns its new ETag.
  pub fn put(&mut self, name: &str, data: &[u8]) -> io::Result<String> {
    let item = Item::new(data);
    let tmp = write_temp(&self.dir, name, data)?;
    // Log before the rename: a crash in between costs a spurious re-fetch,
    // the other order could lose a change.
    if let Err(err) = self.append(name, Some(&item.etag)) {
      let _ = fs::remove_file(&tmp);
      return Err(err);
    }
    fs::rename(&tmp, self.dir.join(name))?;
    sync_dir(&self.dir)?;
    let etag = item.etag.clone();
    self.items.insert(name.to_owned(), item);
    Ok(etag)
  }

  pub fn delete(&mut self, name: &str) -> io::Result<()> {
    self.append(name, None)?;
    fs::remove_file(self.dir.join(name))?;
    sync_dir(&self.dir)?;
    self.items.remove(name);
    Ok(())
  }

  /// Names put or deleted since `token`, sorted. An empty token means the
  /// initial sync. `None` if the token isn't valid for this collection.
  pub fn changes_since(&self, token: &str) -> Option<Vec<&str>> {
    if token.is_empty() {
      return Some(self.items.keys().map(String::as_str).collect());
    }
    let (generation, seq) =
      token.strip_prefix(TOKEN_PREFIX)?.split_once('/')?;
    let seq: u64 = seq.parse().ok()?;
    if generation != self.generation || seq > self.seq {
      return None;
    }
    let mut changes: Vec<_> = self
      .changed
      .iter()
      .filter(|(_, changed)| **changed > seq)
      .map(|(name, _)| name.as_str())
      .collect();
    changes.sort_unstable();
    Some(changes)
  }

  fn append(&mut self, name: &str, etag: Option<&str>) -> io::Result<()> {
    let seq = self.seq + 1;
    let line = etag.map_or_else(
      || format!("{seq} del {name}\n"),
      |etag| format!("{seq} put {etag} {name}\n"),
    );
    self.log.write_all(line.as_bytes())?;
    self.log.sync_data()?;
    self.seq = seq;
    self.changed.insert(name.to_owned(), seq);
    Ok(())
  }
}

/// Replays `.sync.log`. `None` if it is missing or corrupt.
fn read_log(dir: &Path) -> io::Result<Option<History>> {
  let path = dir.join(LOG);
  let text = match fs::read_to_string(&path) {
    Ok(text) => text,
    Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(err) if err.kind() == io::ErrorKind::InvalidData => {
      warn!(path = %path.display(), "sync log is not UTF-8");
      return Ok(None);
    },
    Err(err) => return Err(err),
  };

  // Drop a partial last line left by a crash mid-append.
  let Some(end) = text.rfind('\n') else {
    return Ok(None);
  };
  if end + 1 != text.len() {
    warn!(path = %path.display(), "truncating partial sync log line");
    OpenOptions::new()
      .write(true)
      .open(&path)?
      .set_len(end as u64 + 1)?;
  }

  let mut lines = text[..end].lines();
  let Some(generation) = lines.next().filter(|g| Uuid::parse_str(g).is_ok())
  else {
    warn!(path = %path.display(), "sync log has no valid generation");
    return Ok(None);
  };
  let mut history = History {
    generation: generation.to_owned(),
    seq:        0,
    last:       HashMap::new(),
  };
  for line in lines {
    let Some((seq, name, etag)) =
      parse_line(line).filter(|(seq, ..)| *seq > history.seq)
    else {
      warn!(path = %path.display(), line, "corrupt sync log");
      return Ok(None);
    };
    history.seq = seq;
    history
      .last
      .insert(name.to_owned(), (seq, etag.map(str::to_owned)));
  }
  Ok(Some(history))
}

fn parse_line(line: &str) -> Option<(u64, &str, Option<&str>)> {
  let mut fields = line.split(' ');
  let seq = fields.next()?.parse().ok()?;
  let (name, etag) = match fields.next()? {
    "put" => {
      let etag = fields.next()?;
      (fields.next()?, Some(etag))
    },
    "del" => (fields.next()?, None),
    _ => return None,
  };
  (fields.next().is_none() && valid_name(name)).then_some((seq, name, etag))
}

/// Hashes every item in `dir` and removes temp files left by a crash.
fn scan(dir: &Path) -> io::Result<BTreeMap<String, Item>> {
  let mut items = BTreeMap::new();
  for entry in fs::read_dir(dir)? {
    let entry = entry?;
    let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
      continue;
    };
    let is_temp = name.starts_with('.')
      && Path::new(&name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tmp"));
    if is_temp {
      fs::remove_file(entry.path())?;
      continue;
    }
    #[expect(
      clippy::filetype_is_file,
      reason = "only regular files are items; reading a FIFO would block"
    )]
    let is_file = entry.file_type()?.is_file();
    if valid_name(&name) && is_file {
      let item = Item::new(&fs::read(entry.path())?);
      items.insert(name, item);
    }
  }
  Ok(items)
}

/// The `data/` directory. Collections are loaded lazily and cached.
pub struct Store {
  root:        PathBuf,
  collections: HashMap<CollectionId, Collection>,
}

impl Store {
  pub fn new(root: PathBuf) -> Self {
    Self {
      root,
      collections: HashMap::new(),
    }
  }

  fn home_dir(&self, user: &str, kind: Kind) -> PathBuf {
    self.root.join(user).join(kind.dir())
  }

  fn dir(&self, id: &CollectionId) -> PathBuf {
    self.home_dir(&id.user, id.kind).join(&id.name)
  }

  /// Creates a new user's directory with a default calendar and address
  /// book. Built under a temporary name and renamed into place.
  pub fn provision(&self, user: &str) -> io::Result<()> {
    let dir = self.root.join(user);
    if dir.exists() {
      return Ok(());
    }
    let tmp = self.root.join(format!(".{user}.tmp"));
    if tmp.exists() {
      fs::remove_dir_all(&tmp)?;
    }
    for (kind, displayname) in
      [(Kind::Calendar, "Calendar"), (Kind::Contacts, "Contacts")]
    {
      let collection = tmp.join(kind.dir()).join(DEFAULT_COLLECTION);
      fs::create_dir_all(&collection)?;
      Props {
        displayname: Some(displayname.to_owned()),
        ..Props::default()
      }
      .save(&collection)?;
    }
    fs::rename(&tmp, &dir)?;
    sync_dir(&self.root)?;
    info!(user, "provisioned new user");
    Ok(())
  }

  /// Names of the user's collections of `kind`, sorted.
  pub fn list(&self, user: &str, kind: Kind) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(self.home_dir(user, kind))? {
      let entry = entry?;
      if let Some(name) = entry.file_name().to_str()
        && valid_name(name)
        && entry.file_type()?.is_dir()
      {
        names.push(name.to_owned());
      }
    }
    names.sort();
    Ok(names)
  }

  /// The collection, loading it on first access. `None` if it doesn't exist.
  pub fn collection(
    &mut self,
    id: &CollectionId,
  ) -> io::Result<Option<&mut Collection>> {
    let dir = self.dir(id);
    if !dir.is_dir() {
      self.collections.remove(id);
      return Ok(None);
    }
    Ok(Some(match self.collections.entry(id.clone()) {
      Entry::Occupied(entry) => entry.into_mut(),
      Entry::Vacant(entry) => entry.insert(Collection::load(&dir)?),
    }))
  }

  /// Creates a collection. `false` if it already exists.
  pub fn create(&self, id: &CollectionId, props: &Props) -> io::Result<bool> {
    let dir = self.dir(id);
    match fs::create_dir(&dir) {
      Ok(()) => {},
      Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
        return Ok(false);
      },
      Err(err) => return Err(err),
    }
    props.save(&dir)?;
    sync_dir(&self.home_dir(&id.user, id.kind))?;
    info!(?id, "created collection");
    Ok(true)
  }

  /// Moves a collection into the user's trash. `false` if it doesn't exist.
  pub fn trash(&mut self, id: &CollectionId) -> io::Result<bool> {
    let dir = self.dir(id);
    if !dir.is_dir() {
      return Ok(false);
    }
    let trash = self.root.join(&id.user).join(TRASH);
    fs::create_dir_all(&trash)?;
    let stamp = humantime::format_rfc3339_seconds(SystemTime::now());
    fs::rename(
      &dir,
      trash.join(format!("{}-{}-{stamp}", id.kind.dir(), id.name)),
    )?;
    sync_dir(&self.home_dir(&id.user, id.kind))?;
    self.collections.remove(id);
    info!(?id, "moved collection to trash");
    Ok(true)
  }
}

/// Writes `.<name>.tmp` in `dir` and fsyncs it.
fn write_temp(dir: &Path, name: &str, data: &[u8]) -> io::Result<PathBuf> {
  let tmp = dir.join(format!(".{name}.tmp"));
  let mut file = File::create(&tmp)?;
  file.write_all(data)?;
  file.sync_all()?;
  Ok(tmp)
}

/// Replaces `dir/name` with `data` atomically.
pub fn write_atomic(dir: &Path, name: &str, data: &[u8]) -> io::Result<()> {
  let tmp = write_temp(dir, name, data)?;
  fs::rename(tmp, dir.join(name))?;
  sync_dir(dir)
}

fn sync_dir(dir: &Path) -> io::Result<()> {
  File::open(dir)?.sync_all()
}
