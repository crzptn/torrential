//! The owner task: the only holder of the job book and the only SQLite writer. Connection
//! tasks and the engine thread reach it through messages, and it never awaits I/O, so a
//! slow search or fetch cannot delay a pause.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, mpsc, oneshot};
use torrential_core::protocol::{Change, GetOutcome, GetReply};
use torrential_core::{Error, ErrorCode, HexPrefix, InfoHash, Job, Release, Selection, Target, TorrentId};

use super::admission::{self, DEFAULT_MAX_SIZE, Disk};
use super::book::{self, Intent, JobEntry, JobRecord, Observed, Phase, Verify};
use super::engine_port::{
    Activity, AddSource, AddTorrent, EngineCommand, EngineEvent, EngineHandle, Metainfo, ParseTorrent,
};
use super::magnet::{self, Magnet};
use super::resolve::{self, Found, Scope};
use super::store::{Store, Tombstone};

#[derive(Clone, Debug)]
pub struct OwnerConfig {
    pub default_dir: PathBuf,
    /// Metadata probes add torrents here. They want no files, so nothing is written.
    pub probe_dir: PathBuf,
    pub max_active: usize,
    /// `None` never exits when idle, for a service-managed engine.
    pub idle_timeout: Option<Duration>,
    pub resume_on_start: bool,
}

/// How long pause and shutdown wait for the engine to confirm that writes stopped.
const WRITES_STOP_WITHIN: Duration = Duration::from_secs(10);

pub type Reply<T> = oneshot::Sender<Result<T, Error>>;

#[derive(Clone, Debug)]
pub enum Source {
    Download(TorrentId),
    Magnet(Magnet),
    Torrent { metainfo: Metainfo, origin: Origin },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    File,
    Url,
    Cache,
    Peers,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "torrent_file",
            Self::Url => "torrent_url",
            Self::Cache => "cache",
            Self::Peers => "peers",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Resolved {
    Job(TorrentId),
    Release { release: Box<Release>, cached: Option<Arc<Metainfo>> },
}

#[derive(Clone, Debug)]
pub struct FilesInfo {
    pub id: TorrentId,
    pub metainfo: Arc<Metainfo>,
    pub source: &'static str,
    pub download: Option<DownloadFiles>,
}

#[derive(Clone, Debug)]
pub struct DownloadFiles {
    pub wanted: Vec<bool>,
    pub done: bool,
    pub in_engine: bool,
}

#[derive(Clone, Debug)]
pub enum MetaTarget {
    Job(TorrentId),
    Magnet(Magnet),
}

pub struct GetCommand {
    pub source: Source,
    pub dir: Option<PathBuf>,
    pub files: Vec<String>,
    pub max_size: Option<u64>,
    pub dry_run: bool,
}

pub enum Command {
    Resolve { prefix: HexPrefix, results: Vec<Release>, command: &'static str, reply: Reply<Resolved> },
    Get { get: Box<GetCommand>, reply: Reply<GetReply> },
    Metadata { target: MetaTarget, deadline: Instant, reply: Reply<FilesInfo> },
    Cache { metainfo: Metainfo },
    Status { refs: Vec<String>, reply: Reply<Vec<Job>> },
    Pause { refs: Vec<String>, reply: Reply<Vec<Change>> },
    Resume { refs: Vec<String>, all: bool, reply: Reply<Vec<Change>> },
    Remove { refs: Vec<String>, reply: Reply<Vec<Change>> },
    Verify { reference: String, repair: bool, reply: Reply<Vec<Change>> },
    Watch { refs: Vec<String>, reply: Reply<(Vec<Job>, broadcast::Receiver<Update>)> },
    RecordResults(Vec<Release>),
    Connected,
    Disconnected,
    Shutdown { reply: oneshot::Sender<()> },
}

#[derive(Clone, Debug)]
pub enum Update {
    Job(Box<Job>),
    Removed(TorrentId),
}

struct MetadataWaiter {
    id: TorrentId,
    deadline: Instant,
    reply: Reply<FilesInfo>,
}

struct PendingPause {
    waiting: BTreeSet<TorrentId>,
    changes: Vec<Change>,
    deadline: Instant,
    reply: Reply<Vec<Change>>,
}

struct Stopping {
    waiting: BTreeSet<TorrentId>,
    deadline: Instant,
    replies: Vec<oneshot::Sender<()>>,
}

pub struct Owner {
    store: Store,
    jobs: BTreeMap<TorrentId, JobEntry>,
    tombstones: Vec<Tombstone>,
    /// Torrents in the engine only to fetch their file list for `files`.
    probes: BTreeSet<TorrentId>,
    /// Torrents asked to leave the engine, which cannot be added again until they have.
    draining: BTreeSet<TorrentId>,
    metadata_waiters: Vec<MetadataWaiter>,
    pauses: Vec<PendingPause>,
    stopping: Option<Stopping>,
    stopped: bool,
    engine: EngineHandle,
    parse: ParseTorrent,
    disk: Box<dyn Disk>,
    updates: broadcast::Sender<Update>,
    config: OwnerConfig,
    connections: usize,
    idle_since: Instant,
}

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl Owner {
    pub fn new(
        store: Store,
        engine: EngineHandle,
        parse: ParseTorrent,
        disk: Box<dyn Disk>,
        config: OwnerConfig,
    ) -> Result<Self, Error> {
        let mut jobs = BTreeMap::new();
        for stored in store.jobs()? {
            let metainfo = match &stored.metainfo {
                Some(bytes) => Some(Arc::new(parse(bytes).map_err(|e| {
                    Error::new(
                        ErrorCode::StateCorrupt,
                        format!("the stored file list of {} does not parse: {}", stored.record.id, e.message),
                    )
                })?)),
                None => None,
            };
            let observed = match stored.record.intent {
                Intent::Running if !config.resume_on_start => Observed::Parked,
                _ => Observed::Absent,
            };
            jobs.insert(stored.record.id, JobEntry::new(stored.record, metainfo, observed));
        }
        let now = now();
        let (updates, _) = broadcast::channel(256);
        let mut owner = Self {
            tombstones: store.tombstones(now)?,
            store,
            jobs,
            probes: BTreeSet::new(),
            draining: BTreeSet::new(),
            metadata_waiters: Vec::new(),
            pauses: Vec::new(),
            stopping: None,
            stopped: false,
            engine,
            parse,
            disk,
            updates,
            config,
            connections: 0,
            idle_since: Instant::now(),
        };
        owner.schedule();
        Ok(owner)
    }

    pub async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<Command>,
        mut events: mpsc::UnboundedReceiver<EngineEvent>,
    ) {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        while !self.stopped {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => self.command(command),
                    None => self.begin_stop(None),
                },
                event = events.recv() => match event {
                    Some(event) => self.engine_event(event),
                    None => {
                        super::log("the engine thread stopped, so the engine process is exiting");
                        self.stopped = true;
                    }
                },
                _ = tick.tick() => self.tick(),
            }
        }
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Resolve { prefix, results, command, reply } => {
                let _ = reply.send(self.resolve_any(&prefix, &results, command));
            }
            Command::Get { get, reply } => {
                let _ = reply.send(self.get(*get));
            }
            Command::Metadata { target, deadline, reply } => self.metadata(target, deadline, reply),
            Command::Cache { metainfo } => {
                let _ = self.store.cache_metainfo(metainfo.info_hash.id(), &metainfo.bytes, now());
            }
            Command::Status { refs, reply } => {
                let _ = reply.send(self.resolve_jobs(&refs, "status").map(|ids| self.views(&ids)));
            }
            Command::Pause { refs, reply } => self.pause(refs, reply),
            Command::Resume { refs, all, reply } => {
                let _ = reply.send(self.resume(&refs, all));
            }
            Command::Remove { refs, reply } => {
                let _ = reply.send(self.remove(&refs));
            }
            Command::Verify { reference, repair, reply } => {
                let _ = reply.send(self.verify(&reference, repair));
            }
            Command::Watch { refs, reply } => {
                let watch = self.resolve_jobs(&refs, "wait").map(|ids| (self.views(&ids), self.updates.subscribe()));
                let _ = reply.send(watch);
            }
            Command::RecordResults(releases) => self.record_results(releases),
            Command::Connected => self.connections += 1,
            Command::Disconnected => self.connections = self.connections.saturating_sub(1),
            Command::Shutdown { reply } => self.begin_stop(Some(reply)),
        }
    }

    fn views(&self, ids: &[TorrentId]) -> Vec<Job> {
        ids.iter().filter_map(|id| self.jobs.get(id)).map(JobEntry::view).collect()
    }

    fn publish(&self, id: TorrentId) {
        if let Some(job) = self.jobs.get(&id) {
            let _ = self.updates.send(Update::Job(Box::new(job.view())));
        }
    }

    fn resolve_jobs(&self, refs: &[String], command: &'static str) -> Result<Vec<TorrentId>, Error> {
        if refs.is_empty() {
            let mut all: Vec<&JobEntry> = self.jobs.values().collect();
            all.sort_by_key(|j| (j.record.added_at, j.id()));
            return Ok(all.into_iter().map(JobEntry::id).collect());
        }
        let mut ids = Vec::new();
        for reference in refs {
            let id = self.resolve_job(reference, command)?;
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    fn resolve_job(&self, reference: &str, command: &'static str) -> Result<TorrentId, Error> {
        match Target::parse(reference, Path::new("/"))? {
            Target::Prefix(prefix) => {
                let prefix: HexPrefix = prefix.parse()?;
                let jobs: Vec<(TorrentId, Option<&str>)> =
                    self.jobs.values().map(|j| (j.id(), j.record.name.as_deref())).collect();
                match resolve::prefix(&prefix, &jobs, &[], Scope::Downloads, command)? {
                    Found::Job(id) => Ok(id),
                    Found::Release(_) => unreachable!("downloads scope finds only downloads"),
                }
            }
            Target::InfoHash(hash) => self.find_job(&hash).ok_or_else(|| no_download(reference)),
            Target::Magnet(uri) => self.find_job(&magnet::parse(&uri)?.info_hash).ok_or_else(|| no_download(reference)),
            Target::TorrentUrl(_) | Target::TorrentFile(_) => Err(Error::new(
                ErrorCode::InvalidRef,
                format!("{command} takes a download ID, not a .torrent file or URL"),
            )
            .with_hint("run torrential status to list download IDs")),
        }
    }

    fn find_job(&self, hash: &InfoHash) -> Option<TorrentId> {
        if self.jobs.contains_key(&hash.id()) {
            return Some(hash.id());
        }
        self.store.alias_owners(hash).ok()?.into_iter().find(|id| self.jobs.contains_key(id))
    }

    /// `results` are the recent search results the prefix matches, with their private links.
    fn resolve_any(&self, prefix: &HexPrefix, results: &[Release], command: &'static str) -> Result<Resolved, Error> {
        let jobs: Vec<(TorrentId, Option<&str>)> =
            self.jobs.values().map(|j| (j.id(), j.record.name.as_deref())).collect();
        match resolve::prefix(prefix, &jobs, results, Scope::DownloadsAndResults, command)? {
            Found::Job(id) => Ok(Resolved::Job(id)),
            Found::Release(release) => {
                let cached = match release.id {
                    torrential_core::ReleaseId::Torrent(id) => self.cached(id),
                    torrential_core::ReleaseId::Locator(_) => None,
                };
                Ok(Resolved::Release { release: Box::new(release.clone()), cached })
            }
        }
    }

    fn cached(&self, id: TorrentId) -> Option<Arc<Metainfo>> {
        let bytes = self.store.metainfo(id).ok().flatten()?;
        (self.parse)(&bytes).ok().map(Arc::new)
    }

    /// Saves search results as shown, for the next engine process to restore.
    fn record_results(&mut self, releases: Vec<Release>) {
        let _ = self.store.record_results(&releases, now());
    }

    fn others_remaining(&self, dir: &Path, except: TorrentId) -> u64 {
        let Ok(space) = self.disk.space(dir) else { return 0 };
        self.jobs
            .values()
            .filter(|j| j.id() != except && j.record.intent == Intent::Running)
            .filter(|j| self.disk.space(&j.record.dir).is_ok_and(|s| s.device == space.device))
            .map(JobEntry::remaining)
            .sum()
    }

    fn get(&mut self, get: GetCommand) -> Result<GetReply, Error> {
        let GetCommand { source, dir, files, max_size, dry_run } = get;
        if !files.is_empty() {
            super::files::compile(&files)?;
        }
        let selection = if files.is_empty() { Selection::All } else { Selection::Patterns(files) };
        let info_hash = match &source {
            Source::Download(id) if self.jobs.contains_key(id) => {
                return self.get_existing(*id, source, selection, dry_run);
            }
            Source::Download(id) => return Err(no_download(&id.to_hex())),
            Source::Magnet(m) => m.info_hash,
            Source::Torrent { metainfo, .. } => metainfo.info_hash,
        };
        if let Some(id) = self.find_job(&info_hash) {
            return self.get_existing(id, source, selection, dry_run);
        }
        let id = info_hash.id();
        let (metainfo, magnet) = match source {
            Source::Torrent { metainfo, .. } => (Some(Arc::new(metainfo)), None),
            Source::Magnet(m) => (self.cached(id), Some(m)),
            Source::Download(_) => unreachable!("handled above"),
        };
        let name = metainfo.as_ref().map(|m| m.name.clone()).or_else(|| magnet.as_ref().and_then(|m| m.name.clone()));
        let dir = dir.unwrap_or_else(|| self.config.default_dir.clone());
        let max_size = max_size.unwrap_or(DEFAULT_MAX_SIZE);
        let plan =
            metainfo.as_deref().map(|m| admission::plan(m, &selection, &dir, None, self.disk.as_ref())).transpose()?;
        if let Some(plan) = &plan {
            admission::check_size(plan, max_size)?;
            if !dry_run {
                admission::check_conflicts(plan)?;
            }
            admission::check_disk(plan.space, plan.selected_size, self.others_remaining(&dir, id))?;
        }
        let record = JobRecord {
            id,
            info_hash: metainfo.as_ref().map_or(info_hash, |m| m.info_hash),
            name,
            dir,
            selection,
            max_size,
            magnet: magnet.map(|m| m.uri),
            added_at: now(),
            intent: Intent::Running,
            verified: 0,
        };
        if dry_run {
            let job = JobEntry::new(record, metainfo, Observed::Absent).view();
            return Ok(GetReply {
                outcome: GetOutcome::Planned,
                job,
                plan: plan.map(|p| p.public()),
                previous_selection: None,
            });
        }
        self.store.insert(&record, metainfo.as_ref().map(|m| m.bytes.as_slice()), record.added_at)?;
        self.tombstones.retain(|t| t.id != id);
        if self.probes.remove(&id) {
            self.engine.send(EngineCommand::Remove(id));
            self.draining.insert(id);
        }
        self.jobs.insert(id, JobEntry::new(record, metainfo, Observed::Absent));
        self.schedule();
        self.publish(id);
        let job = &self.jobs[&id];
        let outcome = if job.observed.in_engine() { GetOutcome::Started } else { GetOutcome::Queued };
        Ok(GetReply { outcome, job: job.view(), plan: plan.map(|p| p.public()), previous_selection: None })
    }

    fn get_existing(
        &mut self,
        id: TorrentId,
        source: Source,
        selection: Selection,
        dry_run: bool,
    ) -> Result<GetReply, Error> {
        if let Source::Torrent { metainfo, .. } = source
            && self.jobs[&id].metainfo.is_none()
        {
            self.job_metadata(id, metainfo);
        }
        if !dry_run && self.jobs[&id].observed == Observed::Parked {
            self.jobs.get_mut(&id).expect("found above").observed = Observed::Absent;
            self.schedule();
            self.publish(id);
        }
        let job = &self.jobs[&id];
        let unchanged = selection == Selection::All || selection == job.record.selection;
        if unchanged {
            let plan = self.current_plan(job);
            return Ok(GetReply { outcome: GetOutcome::Existing, job: job.view(), plan, previous_selection: None });
        }
        let previous = job.record.selection.clone();
        let Some(metainfo) = job.metainfo.clone() else {
            if dry_run {
                return Ok(GetReply {
                    outcome: GetOutcome::Planned,
                    job: job.view(),
                    plan: None,
                    previous_selection: Some(previous),
                });
            }
            self.store.set_selection(id, &selection)?;
            self.jobs.get_mut(&id).expect("found above").record.selection = selection;
            self.publish(id);
            let job = self.jobs[&id].view();
            return Ok(GetReply { outcome: GetOutcome::Updated, job, plan: None, previous_selection: Some(previous) });
        };
        let before = job.wanted();
        let plan = admission::plan(&metainfo, &selection, &job.record.dir, before.as_deref(), self.disk.as_ref())?;
        admission::check_size(&plan, job.record.max_size)?;
        if !dry_run {
            admission::check_conflicts(&plan)?;
        }
        let needed = plan.selected_size.saturating_sub(job.verified());
        admission::check_disk(plan.space, needed, self.others_remaining(&job.record.dir, id))?;
        if dry_run {
            let mut preview = job.clone();
            preview.record.selection = selection;
            let job = preview.view();
            return Ok(GetReply {
                outcome: GetOutcome::Planned,
                job,
                plan: Some(plan.public()),
                previous_selection: Some(previous),
            });
        }
        self.store.set_selection(id, &selection)?;
        let reopen = matches!(job.record.intent, Intent::Done { .. });
        if reopen {
            self.store.set_intent(id, &Intent::Running)?;
        }
        let job = self.jobs.get_mut(&id).expect("found above");
        job.record.selection = selection;
        if reopen {
            job.record.intent = Intent::Running;
        }
        if job.observed.in_engine() {
            self.engine.send(EngineCommand::Select(id, plan.wanted.clone()));
        }
        self.schedule();
        self.publish(id);
        let job = self.jobs[&id].view();
        Ok(GetReply {
            outcome: GetOutcome::Updated,
            job,
            plan: Some(plan.public()),
            previous_selection: Some(previous),
        })
    }

    fn current_plan(&self, job: &JobEntry) -> Option<torrential_core::protocol::DownloadPlan> {
        let metainfo = job.metainfo.as_deref()?;
        let wanted = job.wanted()?;
        let plan = admission::plan(metainfo, &job.record.selection, &job.record.dir, Some(&wanted), self.disk.as_ref())
            .ok()?;
        Some(plan.public())
    }

    fn metadata(&mut self, target: MetaTarget, deadline: Instant, reply: Reply<FilesInfo>) {
        let id = match target {
            MetaTarget::Job(id) => id,
            MetaTarget::Magnet(magnet) => match self.find_job(&magnet.info_hash) {
                Some(id) => id,
                None => {
                    let id = magnet.info_hash.id();
                    if let Some(metainfo) = self.cached(id) {
                        let _ =
                            reply.send(Ok(FilesInfo { id, metainfo, source: Origin::Cache.as_str(), download: None }));
                        return;
                    }
                    if !self.probes.contains(&id) && !self.draining.contains(&id) {
                        self.probes.insert(id);
                        self.engine.send(EngineCommand::Add(AddTorrent {
                            id,
                            source: AddSource::Magnet(magnet.uri),
                            save_path: self.config.probe_dir.clone(),
                            wanted: None,
                            resume_data: None,
                            paused: false,
                        }));
                    }
                    self.metadata_waiters.push(MetadataWaiter { id, deadline, reply });
                    return;
                }
            },
        };
        match self.job_files(id) {
            Some(info) => {
                let _ = reply.send(Ok(info));
            }
            None => self.metadata_waiters.push(MetadataWaiter { id, deadline, reply }),
        }
    }

    fn job_files(&self, id: TorrentId) -> Option<FilesInfo> {
        let job = self.jobs.get(&id)?;
        let metainfo = job.metainfo.clone()?;
        let download = DownloadFiles {
            wanted: job.wanted().unwrap_or_else(|| vec![false; metainfo.files.len()]),
            done: matches!(job.record.intent, Intent::Done { .. }),
            in_engine: job.observed.in_engine(),
        };
        Some(FilesInfo { id, metainfo, source: "download", download: Some(download) })
    }

    fn reply_metadata_waiters(&mut self, id: TorrentId, info: Result<FilesInfo, Error>) {
        let (ready, rest): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.metadata_waiters).into_iter().partition(|w| w.id == id);
        self.metadata_waiters = rest;
        for waiter in ready {
            let _ = waiter.reply.send(info.clone());
        }
    }

    fn pause(&mut self, refs: Vec<String>, reply: Reply<Vec<Change>>) {
        let ids = match self.resolve_jobs(&refs, "pause") {
            Ok(ids) => ids,
            Err(e) => {
                let _ = reply.send(Err(e));
                return;
            }
        };
        let mut changes = Vec::new();
        let mut waiting = BTreeSet::new();
        for id in ids {
            let job = &self.jobs[&id];
            let result = match &job.record.intent {
                Intent::Paused => "already_paused",
                Intent::Done { .. } => "already_done",
                Intent::Failed { .. } => "already_stopped",
                Intent::Running => {
                    if let Err(e) = self.store.set_intent(id, &Intent::Paused) {
                        let _ = reply.send(Err(e));
                        return;
                    }
                    let job = self.jobs.get_mut(&id).expect("resolved above");
                    job.record.intent = Intent::Paused;
                    match job.observed {
                        Observed::InEngine(Phase::Paused) => {}
                        Observed::InEngine(Phase::Pausing) => {
                            waiting.insert(id);
                        }
                        Observed::InEngine(_) => {
                            job.observed = Observed::InEngine(Phase::Pausing);
                            self.engine.send(EngineCommand::Pause(id));
                            waiting.insert(id);
                        }
                        Observed::Absent | Observed::Parked => job.observed = Observed::Absent,
                    }
                    self.publish(id);
                    "paused"
                }
            };
            changes.push(self.change(id, result));
        }
        self.schedule();
        if waiting.is_empty() {
            let _ = reply.send(Ok(changes));
        } else {
            self.pauses.push(PendingPause { waiting, changes, deadline: Instant::now() + WRITES_STOP_WITHIN, reply });
        }
    }

    fn change(&self, id: TorrentId, result: &str) -> Change {
        let job = &self.jobs[&id];
        Change { id: id.to_hex(), name: job.record.name.clone(), result: result.to_string(), path: None }
    }

    fn resume(&mut self, refs: &[String], all: bool) -> Result<Vec<Change>, Error> {
        let ids = if all {
            self.jobs
                .values()
                .filter(|j| {
                    matches!(j.record.intent, Intent::Paused | Intent::Failed { .. })
                        || (j.record.intent == Intent::Running && j.observed == Observed::Parked)
                })
                .map(JobEntry::id)
                .collect()
        } else {
            self.resolve_jobs(refs, "resume")?
        };
        let mut changes = Vec::new();
        for id in ids {
            let job = &self.jobs[&id];
            let result = match (&job.record.intent, job.observed) {
                (Intent::Done { .. }, _) => "already_done",
                (Intent::Running, Observed::Parked) => {
                    self.jobs.get_mut(&id).expect("resolved above").observed = Observed::Absent;
                    "resumed"
                }
                (Intent::Running, _) => "already_running",
                (Intent::Paused | Intent::Failed { .. }, observed) => {
                    self.store.set_intent(id, &Intent::Running)?;
                    let job = self.jobs.get_mut(&id).expect("resolved above");
                    job.record.intent = Intent::Running;
                    if observed == Observed::Parked {
                        job.observed = Observed::Absent;
                    }
                    "resumed"
                }
            };
            changes.push(self.change(id, result));
        }
        self.schedule();
        for change in &changes {
            if let Ok(id) = change.id.parse() {
                self.publish(id);
            }
        }
        Ok(changes)
    }

    fn remove(&mut self, refs: &[String]) -> Result<Vec<Change>, Error> {
        let mut targets = Vec::new();
        for reference in refs {
            match self.resolve_job(reference, "rm") {
                Ok(id) => targets.push(Ok(id)),
                Err(e) if e.code == ErrorCode::NotFound => match self.tombstone(reference) {
                    Some(tomb) => targets.push(Err(tomb.clone())),
                    None => return Err(e),
                },
                Err(e) => return Err(e),
            }
        }
        let now = now();
        let mut changes = Vec::new();
        for target in targets {
            match target {
                Err(tomb) => changes.push(Change {
                    id: tomb.id.to_hex(),
                    name: tomb.name,
                    result: "already_removed".into(),
                    path: tomb.path,
                }),
                Ok(id) if !self.jobs.contains_key(&id) => {}
                Ok(id) => {
                    let job = &self.jobs[&id];
                    let view = job.view();
                    let tomb =
                        Tombstone { id, name: job.record.name.clone(), path: view.path.clone(), removed_at: now };
                    self.store.remove(&tomb)?;
                    let job = self.jobs.remove(&id).expect("resolved above");
                    if job.observed.in_engine() {
                        self.engine.send(EngineCommand::Remove(id));
                        self.draining.insert(id);
                    }
                    self.pauses.iter_mut().for_each(|p| {
                        p.waiting.remove(&id);
                    });
                    self.tombstones.retain(|t| t.id != id);
                    self.tombstones.push(tomb);
                    let _ = self.updates.send(Update::Removed(id));
                    changes.push(Change {
                        id: id.to_hex(),
                        name: job.record.name,
                        result: "removed".into(),
                        path: view.path,
                    });
                }
            }
        }
        self.finish_pauses();
        self.schedule();
        Ok(changes)
    }

    fn tombstone(&self, reference: &str) -> Option<&Tombstone> {
        match Target::parse(reference, Path::new("/")).ok()? {
            Target::Prefix(p) => {
                let prefix: HexPrefix = p.parse().ok()?;
                let mut matching = self.tombstones.iter().filter(|t| t.id.has_prefix(&prefix));
                let first = matching.next()?;
                matching.next().is_none().then_some(first)
            }
            Target::InfoHash(hash) => self.tombstones.iter().find(|t| t.id == hash.id()),
            _ => None,
        }
    }

    fn verify(&mut self, reference: &str, repair: bool) -> Result<Vec<Change>, Error> {
        let id = self.resolve_job(reference, "verify")?;
        let job = &self.jobs[&id];
        let short = &id.to_hex()[..6];
        let Some(metainfo) = job.metainfo.clone() else {
            return Err(Error::new(
                ErrorCode::JobConflict,
                format!("{short} has no file list yet, so there is nothing to verify"),
            )
            .with_hint(format!("torrential wait {short} --until metadata")));
        };
        if job.verify.is_some() || job.observed == Observed::InEngine(Phase::Checking) {
            return Ok(vec![self.change(id, "checking")]);
        }
        if matches!(job.observed, Observed::InEngine(Phase::Pausing | Phase::Finishing { .. })) {
            return Err(Error::new(ErrorCode::JobConflict, format!("{short} is busy saving its state"))
                .with_hint(format!("try torrential verify {short} again in a few seconds")));
        }
        let wanted = job.wanted().unwrap_or_else(|| vec![true; metainfo.files.len()]);
        let in_engine = job.observed.in_engine();
        let dir = job.record.dir.clone();
        if !in_engine {
            let resume_data = self.store.resume(id)?;
            self.engine.send(EngineCommand::Add(AddTorrent {
                id,
                source: AddSource::Metainfo(metainfo.bytes.clone()),
                save_path: dir,
                wanted: Some(wanted),
                resume_data,
                paused: true,
            }));
        }
        self.engine.send(EngineCommand::Recheck(id));
        let job = self.jobs.get_mut(&id).expect("resolved above");
        job.verify = Some(if repair { Verify::Repair } else { Verify::Report });
        job.observed = Observed::InEngine(Phase::Checking);
        self.publish(id);
        Ok(vec![self.change(id, "checking")])
    }

    fn engine_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Metadata { id, metainfo } => {
                if self.probes.remove(&id) {
                    let _ = self.store.cache_metainfo(id, &metainfo.bytes, now());
                    self.engine.send(EngineCommand::Remove(id));
                    self.draining.insert(id);
                    let info =
                        FilesInfo { id, metainfo: Arc::new(metainfo), source: Origin::Peers.as_str(), download: None };
                    self.reply_metadata_waiters(id, Ok(info));
                } else if self.jobs.get(&id).is_some_and(|j| j.metainfo.is_none()) {
                    self.job_metadata(id, metainfo);
                }
            }
            EngineEvent::Activity { id, activity } => {
                let Some(job) = self.jobs.get_mut(&id) else { return };
                if matches!(job.observed, Observed::InEngine(Phase::Metadata | Phase::Checking | Phase::Downloading))
                    && job.verify.is_none()
                {
                    job.observed = Observed::InEngine(match activity {
                        Activity::Metadata => Phase::Metadata,
                        Activity::Checking => Phase::Checking,
                        Activity::Downloading => Phase::Downloading,
                    });
                    self.publish(id);
                }
            }
            EngineEvent::Stats { id, stats } => {
                if let Some(job) = self.jobs.get_mut(&id) {
                    job.stats = stats;
                    self.publish(id);
                }
            }
            EngineEvent::Finished { id } => {
                let Some(job) = self.jobs.get_mut(&id) else { return };
                if job.record.intent == Intent::Running
                    && job.verify.is_none()
                    && matches!(
                        job.observed,
                        Observed::InEngine(Phase::Metadata | Phase::Checking | Phase::Downloading)
                    )
                {
                    job.observed = Observed::InEngine(Phase::Finishing { writes_stopped: false });
                    self.engine.send(EngineCommand::Pause(id));
                }
            }
            EngineEvent::Paused { id } => self.paused(id),
            EngineEvent::ResumeData { id, data } => self.resume_data(id, data),
            EngineEvent::Checked { id, damaged_pieces, verified } => self.checked(id, damaged_pieces, verified),
            EngineEvent::Failed { id, error } => {
                if self.probes.remove(&id) {
                    self.engine.send(EngineCommand::Remove(id));
                    self.draining.insert(id);
                    self.reply_metadata_waiters(id, Err(error));
                } else if self.jobs.contains_key(&id) {
                    self.fail(id, error);
                }
            }
            EngineEvent::Removed { id } => {
                self.draining.remove(&id);
            }
        }
        self.finish_stop_if_done();
        self.schedule();
    }

    fn job_metadata(&mut self, id: TorrentId, metainfo: Metainfo) {
        let claimed = self
            .store
            .alias_owners(&metainfo.info_hash)
            .unwrap_or_default()
            .into_iter()
            .find(|owner| *owner != id && self.jobs.contains_key(owner));
        if let Some(original) = claimed {
            self.drop_duplicate(id, original);
            return;
        }
        if let Err(error) = self.store.learn(id, &metainfo.name, &metainfo.info_hash, &metainfo.bytes, now()) {
            self.fail(id, error);
            return;
        }
        let metainfo = Arc::new(metainfo);
        let job = self.jobs.get_mut(&id).expect("caller checked");
        job.record.name = Some(metainfo.name.clone());
        job.record.info_hash = metainfo.info_hash;
        job.metainfo = Some(metainfo.clone());
        let (dir, max_size, selection) = (job.record.dir.clone(), job.record.max_size, job.record.selection.clone());
        let admitted = admission::plan(&metainfo, &selection, &dir, None, self.disk.as_ref()).and_then(|plan| {
            admission::check_size(&plan, max_size)?;
            admission::check_conflicts(&plan)?;
            admission::check_disk(plan.space, plan.selected_size, self.others_remaining(&dir, id))?;
            Ok(plan)
        });
        match admitted {
            Ok(plan) => {
                if self.jobs[&id].observed.in_engine() {
                    self.engine.send(EngineCommand::Select(id, plan.wanted));
                }
                self.publish(id);
                let info = self.job_files(id).expect("metadata just stored");
                self.reply_metadata_waiters(id, Ok(info));
            }
            Err(error) => {
                let info = self.job_files(id).expect("metadata just stored");
                self.reply_metadata_waiters(id, Ok(info));
                self.fail(id, error);
            }
        }
    }

    /// Two downloads turned out to be one hybrid torrent. The one that claimed the hash first stays.
    fn drop_duplicate(&mut self, id: TorrentId, original: TorrentId) {
        let _ = self.store.remove_duplicate(id);
        if let Some(job) = self.jobs.remove(&id)
            && job.observed.in_engine()
        {
            self.engine.send(EngineCommand::Remove(id));
            self.draining.insert(id);
        }
        let _ = self.updates.send(Update::Removed(id));
        let info = self.job_files(original);
        for mut waiter in std::mem::take(&mut self.metadata_waiters) {
            if waiter.id == id {
                waiter.id = original;
                if let Some(info) = &info {
                    let _ = waiter.reply.send(Ok(info.clone()));
                    continue;
                }
            }
            self.metadata_waiters.push(waiter);
        }
    }

    fn fail(&mut self, id: TorrentId, error: Error) {
        let intent = Intent::Failed { error };
        let _ = self.store.set_intent(id, &intent);
        let Some(job) = self.jobs.get_mut(&id) else { return };
        job.record.intent = intent;
        job.verify = None;
        if job.observed.in_engine() {
            self.engine.send(EngineCommand::Remove(id));
            self.draining.insert(id);
        }
        job.observed = Observed::Absent;
        self.publish(id);
    }

    fn paused(&mut self, id: TorrentId) {
        let Some(job) = self.jobs.get_mut(&id) else { return };
        match job.observed {
            Observed::InEngine(Phase::Finishing { writes_stopped: false }) => {
                job.observed = Observed::InEngine(Phase::Finishing { writes_stopped: true });
            }
            Observed::InEngine(Phase::Finishing { writes_stopped: true }) => return,
            Observed::InEngine(Phase::Checking) if job.verify.is_some() => return,
            Observed::InEngine(_) => job.observed = Observed::InEngine(Phase::Paused),
            Observed::Absent | Observed::Parked => return,
        }
        self.engine.send(EngineCommand::SaveResume(id));
        for pause in &mut self.pauses {
            pause.waiting.remove(&id);
        }
        self.finish_pauses();
        self.publish(id);
    }

    fn finish_pauses(&mut self) {
        let now = Instant::now();
        let (done, rest): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.pauses).into_iter().partition(|p| p.waiting.is_empty() || p.deadline <= now);
        self.pauses = rest;
        for pause in done {
            let _ = pause.reply.send(Ok(pause.changes));
        }
    }

    fn resume_data(&mut self, id: TorrentId, data: Vec<u8>) {
        if let Some(stopping) = &mut self.stopping {
            stopping.waiting.remove(&id);
        }
        let Some(job) = self.jobs.get(&id) else { return };
        let verified = job.verified();
        if job.observed == Observed::InEngine(Phase::Finishing { writes_stopped: true }) {
            let finished_at = now();
            let size = job.selected_size().unwrap_or(verified);
            if let Err(error) = self.store.finish(id, finished_at, &data, size) {
                self.fail(id, error);
                return;
            }
            let job = self.jobs.get_mut(&id).expect("checked above");
            job.record.intent = Intent::Done { finished_at };
            job.record.verified = size;
            job.observed = Observed::Absent;
            self.engine.send(EngineCommand::Remove(id));
            self.draining.insert(id);
            self.publish(id);
        } else if self.store.save_resume(id, &data, verified, now()).is_ok() {
            self.jobs.get_mut(&id).expect("checked above").record.verified = verified;
        }
    }

    fn checked(&mut self, id: TorrentId, damaged: u32, verified: u64) {
        let Some(job) = self.jobs.get_mut(&id) else { return };
        let Some(mode) = job.verify.take() else { return };
        // The check replaces what the last save recorded, so progress and the next restart start from it.
        job.record.verified = verified;
        job.stats.verified = verified;
        self.engine.send(EngineCommand::SaveResume(id));
        let short = id.to_hex()[..6].to_string();
        let intent = job.record.intent.clone();
        match (intent, damaged, mode) {
            (Intent::Running, ..) => {
                job.observed = Observed::InEngine(Phase::Downloading);
                self.engine.send(EngineCommand::Resume(id));
            }
            (Intent::Paused, ..) => job.observed = Observed::InEngine(Phase::Paused),
            (Intent::Done { .. } | Intent::Failed { .. }, 0, _) => {
                let finished_at = match job.record.intent {
                    Intent::Done { finished_at } => finished_at,
                    _ => now(),
                };
                let done = Intent::Done { finished_at };
                let _ = self.store.set_intent(id, &done);
                let job = self.jobs.get_mut(&id).expect("checked above");
                job.record.intent = done;
                job.observed = Observed::Absent;
                self.engine.send(EngineCommand::Remove(id));
                self.draining.insert(id);
            }
            (Intent::Done { .. } | Intent::Failed { .. }, _, Verify::Repair) => {
                let _ = self.store.set_intent(id, &Intent::Running);
                job.record.intent = Intent::Running;
                job.observed = Observed::InEngine(Phase::Downloading);
                self.engine.send(EngineCommand::Resume(id));
            }
            (Intent::Done { .. } | Intent::Failed { .. }, n, Verify::Report) => {
                let error = Error::new(
                    ErrorCode::VerificationFailed,
                    format!("{n} damaged {} in the selected files", if n == 1 { "piece" } else { "pieces" }),
                )
                .with_hint(format!("torrential verify {short} --repair downloads them again"));
                self.fail(id, error);
                return;
            }
        }
        self.publish(id);
    }

    fn schedule(&mut self) {
        if self.stopping.is_some() {
            return;
        }
        let ready = self.jobs.values().filter(|j| !self.draining.contains(&j.id()));
        for id in book::to_start(ready, self.config.max_active) {
            self.start(id);
        }
    }

    fn start(&mut self, id: TorrentId) {
        let job = &self.jobs[&id];
        if job.observed == Observed::InEngine(Phase::Paused) {
            self.engine.send(EngineCommand::Resume(id));
            self.jobs.get_mut(&id).expect("scheduled").observed = Observed::InEngine(Phase::Downloading);
            self.publish(id);
            return;
        }
        if job.metainfo.is_some()
            && let Ok(space) = self.disk.space(&job.record.dir)
            && let Err(error) =
                admission::check_disk(space, job.remaining(), self.others_remaining(&job.record.dir, id))
        {
            self.fail(id, error);
            return;
        }
        let resume_data = self.store.resume(id).ok().flatten();
        let job = &self.jobs[&id];
        let source = match (&job.metainfo, &job.record.magnet) {
            (Some(m), _) => AddSource::Metainfo(m.bytes.clone()),
            (None, Some(uri)) => AddSource::Magnet(uri.clone()),
            (None, None) => AddSource::Magnet(magnet::from_hash(job.record.info_hash).uri),
        };
        let phase = if job.metainfo.is_some() { Phase::Checking } else { Phase::Metadata };
        self.engine.send(EngineCommand::Add(AddTorrent {
            id,
            source,
            save_path: job.record.dir.clone(),
            wanted: job.wanted(),
            resume_data,
            paused: false,
        }));
        self.jobs.get_mut(&id).expect("scheduled").observed = Observed::InEngine(phase);
        self.publish(id);
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let (expired, waiting): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.metadata_waiters).into_iter().partition(|w| w.deadline <= now);
        self.metadata_waiters = waiting;
        for waiter in expired {
            let short = waiter.id.to_hex()[..6].to_string();
            let _ = waiter.reply.send(Err(Error::new(
                ErrorCode::MetadataTimeout,
                format!("no peer sent the file list for {short} in time"),
            )
            .with_hint(format!("try again later, or run torrential get {short} to keep looking in the background"))));
        }
        let abandoned: Vec<TorrentId> =
            self.probes.iter().copied().filter(|id| !self.metadata_waiters.iter().any(|w| w.id == *id)).collect();
        for id in abandoned {
            self.probes.remove(&id);
            self.engine.send(EngineCommand::Remove(id));
            self.draining.insert(id);
        }
        self.finish_pauses();
        self.finish_stop_if_done();
        self.check_idle(now);
    }

    fn busy(&self) -> bool {
        self.jobs.values().any(|j| {
            j.observed.occupies_slot() || j.observed == Observed::InEngine(Phase::Pausing) || j.verify.is_some()
        }) || !self.probes.is_empty()
            || !self.pauses.is_empty()
            || !self.draining.is_empty()
    }

    fn check_idle(&mut self, now: Instant) {
        let Some(timeout) = self.config.idle_timeout else { return };
        if self.connections > 0 || self.busy() || self.stopping.is_some() {
            self.idle_since = now;
        } else if now.duration_since(self.idle_since) >= timeout {
            self.begin_stop(None);
        }
    }

    fn begin_stop(&mut self, reply: Option<oneshot::Sender<()>>) {
        if let Some(stopping) = &mut self.stopping {
            stopping.replies.extend(reply);
            return;
        }
        let mut waiting = BTreeSet::new();
        for job in self.jobs.values_mut() {
            let id = job.record.id;
            match job.observed {
                Observed::InEngine(Phase::Paused) => {
                    self.engine.send(EngineCommand::SaveResume(id));
                    waiting.insert(id);
                }
                Observed::InEngine(Phase::Pausing | Phase::Finishing { .. }) => {
                    waiting.insert(id);
                }
                Observed::InEngine(_) => {
                    job.observed = Observed::InEngine(Phase::Pausing);
                    self.engine.send(EngineCommand::Pause(id));
                    waiting.insert(id);
                }
                Observed::Absent | Observed::Parked => {}
            }
        }
        self.stopping = Some(Stopping {
            waiting,
            deadline: Instant::now() + WRITES_STOP_WITHIN,
            replies: reply.into_iter().collect(),
        });
        self.finish_stop_if_done();
    }

    fn finish_stop_if_done(&mut self) {
        let Some(stopping) = &self.stopping else { return };
        if !stopping.waiting.is_empty() && Instant::now() < stopping.deadline {
            return;
        }
        let stopping = self.stopping.take().expect("checked above");
        for reply in stopping.replies {
            let _ = reply.send(());
        }
        self.stopped = true;
    }
}

fn no_download(reference: &str) -> Error {
    Error::new(ErrorCode::NotFound, format!("no download matches {reference}"))
        .with_hint("run torrential status to list downloads")
}
