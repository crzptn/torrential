//! `torrential daemon run`: the engine process. It owns every download, the SQLite store,
//! the torrent engine, and the search service, and serves clients over a Unix socket.

pub mod admission;
pub mod book;
pub mod engine_port;
pub mod files;
pub mod magnet;
pub mod owner;
pub mod resolve;
pub mod search_port;
pub mod server;
pub mod store;

#[cfg(test)]
mod tests;

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UnixListener;
use tokio::sync::mpsc;
use torrential_core::{Error, ErrorCode};

use crate::env::Env;
use crate::paths::{self, Paths};
use admission::Disk;
use engine_port::{EnginePort, ParseTorrent};
use owner::{Owner, OwnerConfig};
use search_port::SearchPort;
use server::{Ctx, OwnerHandle};
use store::Store;

const DEFAULT_IDLE: Duration = Duration::from_secs(120);
pub const MAX_ACTIVE: usize = 3;

/// The pieces the engine process is built from, so tests can swap in fakes.
pub struct Ports {
    pub engine: Box<dyn EnginePort>,
    pub parse: ParseTorrent,
    pub search: Arc<dyn SearchPort>,
    pub disk: Box<dyn Disk>,
}

impl Ports {
    /// Real ports when torrential is built with them. Until the wiring round, the engine and
    /// search are unwired, and `TORRENTIAL_TEST_ENGINE=fake` simulates both for tests.
    fn from_env(env: &Env) -> Self {
        if env.var("TORRENTIAL_TEST_ENGINE") == Some("fake") {
            let rate =
                env.var("TORRENTIAL_TEST_FAKE_RATE").and_then(torrential_core::units::parse_size).unwrap_or(10 << 20);
            return Self {
                engine: Box::new(engine_port::Fake::new(engine_port::Swarm::inventive(rate))),
                parse: engine_port::parse_fake,
                search: Arc::new(search_port::Fake::new(search_port::fake_catalog())),
                disk: Box::new(admission::RoomyDisk),
            };
        }
        Self {
            engine: Box::new(engine_port::Unwired::default()),
            parse: engine_port::parse_unwired,
            search: Arc::new(search_port::Unwired),
            disk: Box::new(admission::RealDisk),
        }
    }
}

/// A running engine without its socket: the owner task, the engine thread, and the shared context.
pub struct Running {
    pub ctx: Arc<Ctx>,
    pub owner: tokio::task::JoinHandle<()>,
}

/// Starts the actors. Must run inside a Tokio runtime.
pub fn start(store: Store, ports: Ports, config: OwnerConfig, version: String) -> Result<Running, Error> {
    let (events_tx, events) = mpsc::unbounded_channel();
    let (engine, _thread) = engine_port::spawn(ports.engine, events_tx);
    let owner = Owner::new(store, engine.clone(), ports.parse, ports.disk, config)?;
    let (commands_tx, commands) = mpsc::unbounded_channel();
    let task = tokio::spawn(owner.run(commands, events));
    let ctx =
        Arc::new(Ctx { owner: OwnerHandle(commands_tx), search: ports.search, engine, parse: ports.parse, version });
    Ok(Running { ctx, owner: task })
}

pub fn log(message: &str) {
    eprintln!("{} {message}", crate::output::format::timestamp(owner::now()));
}

/// Returns the process exit code.
pub fn run(env: &Env, service: bool) -> u8 {
    let paths = match Paths::from_process(env) {
        Ok(paths) => paths,
        Err(e) => return fail_start(None, &e),
    };
    match serve(env, &paths, service) {
        Ok(()) => 0,
        Err(Startup::AlreadyRunning(message)) => {
            log(&message);
            ErrorCode::DaemonUnavailable.exit_code()
        }
        Err(Startup::Failed(e)) => fail_start(Some(&paths), &e),
    }
}

enum Startup {
    /// Another engine holds the lock. Not an error for the client that raced to start it.
    AlreadyRunning(String),
    Failed(Error),
}

impl From<Error> for Startup {
    fn from(e: Error) -> Self {
        Self::Failed(e)
    }
}

fn fail_start(paths: Option<&Paths>, error: &Error) -> u8 {
    log(&format!("engine failed to start: {error}"));
    if let Some(paths) = paths {
        let _ = std::fs::write(paths.startup_error(), serde_json::to_vec(error).expect("errors serialize"));
    }
    error.code.exit_code()
}

fn serve(env: &Env, paths: &Paths, service: bool) -> Result<(), Startup> {
    let io = |what: &str, e: std::io::Error| Error::new(ErrorCode::StateCorrupt, format!("cannot {what}: {e}"));
    std::fs::create_dir_all(&paths.state).map_err(|e| io(&format!("create {}", paths.state.display()), e))?;
    paths::check_state_filesystem(&paths.state)?;
    let _lock = match OwnerLock::acquire(&paths.lock())? {
        Some(lock) => lock,
        None => return Err(Startup::AlreadyRunning(format!("another engine owns {}", paths.state.display()))),
    };
    let _ = std::fs::remove_file(paths.startup_error());
    prepare_runtime_dir(&paths.runtime)?;
    let store = Store::open(&paths.database())?;
    let idle_timeout = env
        .var("TORRENTIAL_TEST_IDLE_TIMEOUT")
        .and_then(torrential_core::units::parse_duration)
        .unwrap_or(DEFAULT_IDLE);
    let config = OwnerConfig {
        default_dir: paths.downloads.clone(),
        probe_dir: paths.cache.join("probes"),
        max_active: MAX_ACTIVE,
        idle_timeout: (!service).then_some(idle_timeout),
        resume_on_start: service,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::internal(format!("cannot start the async runtime: {e}")))?;
    let version = env.version();
    let ports = Ports::from_env(env);
    runtime.block_on(async {
        let socket = paths.socket();
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).map_err(|e| {
            Error::new(ErrorCode::DaemonUnavailable, format!("cannot listen on {}: {e}", socket.display()))
        })?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| io(&format!("protect {}", socket.display()), e))?;
        let running = start(store, ports, config, version.clone())?;
        log(&format!("engine {version} started, pid {}, socket {}", std::process::id(), socket.display()));
        let ctx = running.ctx.clone();
        let accept = tokio::spawn(async move {
            // SAFETY: geteuid has no preconditions and cannot fail.
            let uid = unsafe { libc::geteuid() };
            while let Ok((stream, _)) = listener.accept().await {
                if stream.peer_cred().is_ok_and(|cred| cred.uid() == uid) {
                    tokio::spawn(server::connection(ctx.clone(), stream));
                }
            }
        });
        let signals = tokio::spawn(stop_on_signal(running.ctx.owner.clone()));
        let _ = running.owner.await;
        // Lets a connection that asked for the shutdown write its reply before the process ends.
        tokio::time::sleep(Duration::from_millis(100)).await;
        accept.abort();
        signals.abort();
        let _ = std::fs::remove_file(&socket);
        log("engine stopped");
        Ok::<(), Error>(())
    })?;
    Ok(())
}

async fn stop_on_signal(owner: OwnerHandle) {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else {
        return;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
    log("stopping on a signal");
    let (reply, _) = tokio::sync::oneshot::channel();
    owner.tell(owner::Command::Shutdown { reply });
}

/// Mode 0700 and owned by this user, because anyone who can reach the socket controls downloads.
fn prepare_runtime_dir(dir: &Path) -> Result<(), Error> {
    let refuse =
        |why: String| Error::new(ErrorCode::DaemonUnavailable, why).with_hint("remove it, or set XDG_RUNTIME_DIR");
    std::fs::create_dir_all(dir).map_err(|e| refuse(format!("cannot create {}: {e}", dir.display())))?;
    let meta = dir.symlink_metadata().map_err(|e| refuse(format!("cannot inspect {}: {e}", dir.display())))?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    if !meta.is_dir() || meta.uid() != uid {
        return Err(refuse(format!("{} is not a directory owned by you", dir.display())));
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| refuse(format!("cannot protect {}: {e}", dir.display())))
}

/// An exclusive `flock` held for the process lifetime: one engine per state directory.
/// The kernel releases it when the process dies, so a crash leaves nothing stale.
pub struct OwnerLock {
    _file: File,
}

impl OwnerLock {
    pub fn acquire(path: &Path) -> Result<Option<Self>, Error> {
        use std::os::fd::AsRawFd;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
            .map_err(|e| Error::new(ErrorCode::StateCorrupt, format!("cannot open {}: {e}", path.display())))?;
        // SAFETY: flock only reads the descriptor, which `file` keeps open.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
                return Ok(None);
            }
            return Err(Error::new(ErrorCode::StateCorrupt, format!("cannot lock {}: {e}", path.display())));
        }
        let mut file = file;
        let _ = file.set_len(0).and_then(|()| writeln!(file, "{}", std::process::id()));
        Ok(Some(Self { _file: file }))
    }
}
