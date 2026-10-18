use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use torrential_core::protocol::{Change, Event, FilesReply, GetReply, GetRequest, Request, Response};
use torrential_core::{Error, ErrorCode, Machine, Preferences, Profile, SearchQuery, SearchResult, Target};

use torrential_tui::Edit;

use super::connect::{self, Conn, DEFAULT_REPLY_WITHIN};
use crate::config::{self, Config};
use crate::daemon::admission::{Disk, RealDisk};
use crate::daemon::server::METADATA_WAIT;
use crate::env::Env;
use crate::paths::Paths;
use crate::skill::Writes;
use crate::wire;

pub fn run(env: &Env) -> Result<u8, Error> {
    let paths = Paths::from_process(env)?;
    let downloads = Config::load(&paths, env).map_or_else(|_| paths.downloads.clone(), |c| c.downloads_dir(&paths));
    let backend = EngineBackend { paths, downloads, env: env.clone() };
    torrential_tui::run(backend).map_err(|e| Error::internal(format!("the terminal UI stopped: {e}")))?;
    Ok(0)
}

struct EngineBackend {
    paths: Paths,
    downloads: PathBuf,
    env: Env,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SearchFrame {
    Update(Event),
    Reply(Response),
}

impl EngineBackend {
    fn conn(&self) -> Result<Conn, Error> {
        connect::ensure(&self.paths, &self.env)
    }

    fn changes(&self, request: Request, within: Duration) -> Result<Vec<Change>, Error> {
        match self.conn()?.call_within(&request, within)? {
            Response::Changed { changes } => Ok(changes),
            Response::Error { error } => Err(error),
            other => Err(connect::unexpected(&other)),
        }
    }
}

impl torrential_tui::Backend for EngineBackend {
    fn search(
        &self,
        query: SearchQuery,
        partial: &mut dyn FnMut(SearchResult, Vec<String>),
    ) -> Result<SearchResult, Error> {
        let preferences = config::preferences(&self.paths)?;
        let query = SearchQuery { profile: Profile { preferences, ..Profile::default() }, ..query };
        let within = Duration::from_millis(query.timeout_ms.into()) + Duration::from_secs(10);
        let mut conn = self.conn()?;
        conn.send(&Request::Search { query, stream: true })?;
        loop {
            match conn.read_within::<SearchFrame>(within)? {
                SearchFrame::Update(Event::SearchUpdate { result, pending }) => partial(*result, pending),
                SearchFrame::Update(_) => {}
                SearchFrame::Reply(Response::Search { result }) => return Ok(result),
                SearchFrame::Reply(Response::Error { error }) => return Err(error),
                SearchFrame::Reply(other) => return Err(connect::unexpected(&other)),
            }
        }
    }

    fn files(&self, reference: &str) -> Result<FilesReply, Error> {
        let request = Request::Files {
            target: Target::Prefix(reference.to_string()),
            timeout_ms: METADATA_WAIT.as_millis() as u32,
        };
        match self.conn()?.call_within(&request, METADATA_WAIT + Duration::from_secs(15))? {
            Response::Files { files } => Ok(files),
            Response::Error { error } => Err(error),
            other => Err(connect::unexpected(&other)),
        }
    }

    fn get(&self, reference: &str, files: Option<Vec<String>>) -> Result<GetReply, Error> {
        let request = Request::Get(GetRequest {
            target: Target::Prefix(reference.to_string()),
            dir: None,
            files: files.unwrap_or_default().iter().map(|path| globset::escape(path)).collect(),
            max_size: None,
            setup_max_size: config::preferences(&self.paths)?.max_size,
            dry_run: false,
        });
        match self.conn()?.call(&request)? {
            Response::Get { reply } => Ok(*reply),
            Response::Error { error } => Err(error),
            other => Err(connect::unexpected(&other)),
        }
    }

    fn pause(&self, id: &str) -> Result<Vec<Change>, Error> {
        self.changes(Request::Pause { refs: vec![id.to_string()] }, Duration::from_secs(20))
    }

    fn resume(&self, id: &str) -> Result<Vec<Change>, Error> {
        self.changes(Request::Resume { refs: vec![id.to_string()], all: false }, DEFAULT_REPLY_WITHIN)
    }

    fn remove(&self, id: &str) -> Result<Vec<Change>, Error> {
        self.changes(Request::Remove { refs: vec![id.to_string()] }, DEFAULT_REPLY_WITHIN)
    }

    fn verify(&self, id: &str) -> Result<Vec<Change>, Error> {
        self.changes(Request::Verify { reference: id.to_string(), repair: false }, DEFAULT_REPLY_WITHIN)
    }

    fn watch(&self, event: &mut dyn FnMut(Event)) -> Result<(), Error> {
        let mut conn = self.conn()?;
        conn.send(&Request::Watch { refs: Vec::new() })?;
        let mut stream = conn.into_stream();
        let lost = |e: std::io::Error| Error::new(ErrorCode::DaemonUnavailable, format!("lost the engine: {e}"));
        stream.set_read_timeout(None).map_err(lost)?;
        loop {
            match wire::read::<Event>(&mut stream).map_err(lost)? {
                Some(e) => event(e),
                None => return Ok(()),
            }
        }
    }

    fn free_space(&self) -> Option<u64> {
        RealDisk.space(&self.downloads).ok().map(|space| space.free)
    }

    fn preferences(&self) -> Result<Preferences, Error> {
        config::preferences(&self.paths)
    }

    fn machine(&self) -> Option<Machine> {
        crate::machine::detect(&self.env).ok()
    }

    fn save_preferences(&self, edit: Edit) -> Result<Preferences, Error> {
        let preferences = edit.apply(config::preferences(&self.paths)?);
        let writes = Writes::new(&self.env, "torrential setup".into());
        config::save_preferences(&self.paths, &preferences, &writes)?;
        Ok(preferences)
    }
}
