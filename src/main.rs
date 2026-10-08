// SPDX-License-Identifier: EUPL-1.2
//! caesar: a small CalDAV and CardDAV server for personal use.

mod dav;
mod props;
mod store;
mod xml;

use std::{
  env,
  fs,
  io::{self, IsTerminal},
  os::{fd::FromRawFd, unix::net::UnixListener as StdUnixListener},
  path::PathBuf,
  sync::{Arc, Mutex},
  time::Instant,
};

use axum::{
  Router,
  extract::{Request, State},
  response::Response,
};
use tokio::{
  net::UnixListener,
  signal::unix::{Signal, SignalKind, signal},
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::store::Store;

/// First file descriptor passed by systemd socket activation.
const SD_LISTEN_FDS_START: i32 = 3;
const DEFAULT_SOCKET: &str = "caesar.sock";

#[tokio::main(flavor = "current_thread")]
async fn main() -> io::Result<()> {
  tracing_subscriber::fmt()
    .without_time()
    .with_writer(io::stderr)
    .with_ansi(io::stderr().is_terminal())
    .with_env_filter(
      EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info")),
    )
    .init();

  let data = data_dir();
  fs::create_dir_all(&data)?;
  info!(data = %data.display(), "starting");
  let listener = match systemd_listener()? {
    Some(listener) => listener,
    None => bind()?,
  };
  let app = Router::new()
    .fallback(handle)
    .with_state(Arc::new(Mutex::new(Store::new(data))));
  let terminate = signal(SignalKind::terminate())?;
  axum::serve(listener, app)
    .with_graceful_shutdown(shutdown(terminate))
    .await
}

/// `$CAESAR_DATA_DIR`, else systemd's `$STATE_DIRECTORY`, else `./data`.
fn data_dir() -> PathBuf {
  env::var_os("CAESAR_DATA_DIR")
    .or_else(|| env::var_os("STATE_DIRECTORY"))
    .map_or_else(|| PathBuf::from("data"), PathBuf::from)
}

/// The socket passed by systemd, if we were socket-activated.
fn systemd_listener() -> io::Result<Option<UnixListener>> {
  let ours = env::var("LISTEN_PID")
    .ok()
    .and_then(|pid| pid.parse::<u32>().ok())
    == Some(std::process::id());
  let count: u32 = env::var("LISTEN_FDS")
    .ok()
    .and_then(|count| count.parse().ok())
    .unwrap_or(0);
  if !ours || count == 0 {
    return Ok(None);
  }
  if count > 1 {
    warn!(count, "only using the first socket passed by systemd");
  }
  // SAFETY: LISTEN_PID names this process, so systemd handed us ownership of
  // the listening socket at fd 3.
  let listener = unsafe { StdUnixListener::from_raw_fd(SD_LISTEN_FDS_START) };
  listener.set_nonblocking(true)?;
  info!("listening on socket from systemd");
  UnixListener::from_std(listener).map(Some)
}

/// Binds `$CAESAR_SOCKET`, else `./caesar.sock`, replacing a stale socket.
fn bind() -> io::Result<UnixListener> {
  let path = env::var_os("CAESAR_SOCKET")
    .map_or_else(|| PathBuf::from(DEFAULT_SOCKET), PathBuf::from);
  match fs::remove_file(&path) {
    Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
    _ => {},
  }
  let listener = UnixListener::bind(&path)?;
  info!(path = %path.display(), "listening");
  Ok(listener)
}

async fn shutdown(mut terminate: Signal) {
  tokio::select! {
    _ = terminate.recv() => {},
    _ = tokio::signal::ctrl_c() => {},
  }
  info!("shutting down");
}

async fn handle(
  State(store): State<Arc<Mutex<Store>>>,
  request: Request,
) -> Response {
  let start = Instant::now();
  let method = request.method().clone();
  let path = request.uri().path().to_owned();
  let response = dav::handle(&store, request).await;
  info!(
    %method,
    path,
    status = response.status().as_u16(),
    ms = start.elapsed().as_millis(),
  );
  response
}
