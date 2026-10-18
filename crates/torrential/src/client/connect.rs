use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use torrential_core::protocol::{Request, Response, WIRE_VERSION};
use torrential_core::{Error, ErrorCode};

use super::launch::{self, Blocked};
use crate::env::Env;
use crate::paths::Paths;
use crate::wire;

/// Most answers take milliseconds. This bounds a wedged engine, not a slow one.
pub const DEFAULT_REPLY_WITHIN: Duration = Duration::from_secs(30);

pub struct Conn {
    stream: UnixStream,
    pub engine_version: String,
    pub wire: u32,
    pub pid: u32,
    log: String,
}

impl Conn {
    pub fn call(&mut self, request: &Request) -> Result<Response, Error> {
        self.call_within(request, DEFAULT_REPLY_WITHIN)
    }

    pub fn call_within(&mut self, request: &Request, within: Duration) -> Result<Response, Error> {
        self.send(request)?;
        self.stream.set_read_timeout(Some(within)).map_err(|e| self.lost(e))?;
        let response = wire::read(&mut self.stream).map_err(|e| self.lost(e))?;
        response.ok_or_else(|| self.lost(std::io::Error::from(ErrorKind::UnexpectedEof)))
    }

    pub fn send(&mut self, request: &Request) -> Result<(), Error> {
        wire::write(&mut self.stream, request).map_err(|e| self.lost(e))
    }

    pub fn into_stream(self) -> UnixStream {
        self.stream
    }

    fn lost(&self, e: std::io::Error) -> Error {
        let message = match e.kind() {
            ErrorKind::WouldBlock | ErrorKind::TimedOut => "the engine did not answer in time".to_string(),
            _ => format!("lost the connection to the engine: {e}"),
        };
        Error::new(ErrorCode::DaemonUnavailable, message).with_hint(format!("see {}", self.log))
    }
}

pub fn existing(paths: &Paths, env: &Env) -> Result<Option<Conn>, Error> {
    let socket = paths.socket();
    let stream = match UnixStream::connect(&socket) {
        Ok(stream) => stream,
        Err(e) if matches!(e.raw_os_error(), Some(libc::EPERM | libc::EACCES)) => {
            return Err(launch::sandbox_error(Blocked::Ipc { socket }, env));
        }
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => return Ok(None),
        Err(e) => {
            return Err(Error::new(
                ErrorCode::DaemonUnavailable,
                format!("cannot reach the engine at {}: {e}", socket.display()),
            ));
        }
    };
    let mut conn =
        Conn { stream, engine_version: String::new(), wire: 0, pid: 0, log: paths.log().display().to_string() };
    match conn
        .call_within(&Request::Hello { client_version: env.version(), wire: WIRE_VERSION }, Duration::from_secs(5))?
    {
        Response::Hello { engine_version, wire, pid } => {
            conn.engine_version = engine_version;
            conn.wire = wire;
            conn.pid = pid;
            Ok(Some(conn))
        }
        Response::Error { error } => Err(error),
        other => Err(unexpected(&other)),
    }
}

/// The engine, started if needed. An engine from another version is stopped first, after it
/// saves its state, so an upgrade takes effect on the next command.
pub fn ensure(paths: &Paths, env: &Env) -> Result<Conn, Error> {
    if let Some(conn) = existing(paths, env)? {
        if conn.engine_version == env.version() && conn.wire == WIRE_VERSION {
            return Ok(conn);
        }
        stop(conn)?;
    }
    launch::start(paths, env)
}

pub fn stop(mut conn: Conn) -> Result<u32, Error> {
    let pid = conn.pid;
    // The engine may exit before its reply arrives; the process check below is what counts.
    let _ = conn.call_within(&Request::Shutdown, Duration::from_secs(20));
    let deadline = Instant::now() + Duration::from_secs(20);
    while alive(pid) {
        if Instant::now() > deadline {
            return Err(Error::new(ErrorCode::DaemonUnavailable, format!("the engine (pid {pid}) did not exit"))
                .with_hint(format!("kill {pid}, then run the command again")));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(pid)
}

pub fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

pub fn unexpected(response: &Response) -> Error {
    Error::internal(format!(
        "the engine sent an unexpected reply: {}",
        serde_json::to_string(response).unwrap_or_default()
    ))
}
