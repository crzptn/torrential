//! The seam between the engine process and the torrent engine. [`super::libtorrent`]
//! implements [`EnginePort`] over `torrential-engine`, and [`Fake`] simulates a swarm for
//! tests.
//!
//! The port runs on its own thread (see [`spawn`]). Every outcome, including a failed
//! call, comes back to the owner as an [`EngineEvent`].

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc as tokio_mpsc, oneshot};
use torrential_core::{Error, ErrorCode, InfoHash, TorrentId};

/// A torrent's file list. Only [`Metainfo::new`] builds one, so every path in it stays
/// inside the torrent's folder. A port that receives a hostile file list reports the
/// constructor's error as [`EngineEvent::Failed`].
#[derive(Clone, PartialEq, Debug)]
pub struct Metainfo {
    pub info_hash: InfoHash,
    pub name: String,
    pub files: Vec<MetaFile>,
    /// One file stored at `<dir>/<name>`, rather than a folder `<dir>/<name>/`.
    pub single_file: bool,
    /// The `.torrent` bytes, kept so a restart can add the torrent without asking peers.
    pub bytes: Vec<u8>,
    checked: (),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MetaFile {
    /// Relative to the torrent's folder, with `/` separators.
    pub path: String,
    pub size: u64,
}

impl Metainfo {
    pub fn new(
        info_hash: InfoHash,
        name: String,
        files: Vec<MetaFile>,
        single_file: bool,
        bytes: Vec<u8>,
    ) -> Result<Self, Error> {
        let escapes = |p: &str| p.is_empty() || p.starts_with('/') || p.split('/').any(|c| c == ".." || c.is_empty());
        if escapes(&name) || name.contains('/') || files.iter().any(|f| escapes(&f.path)) {
            return Err(Error::new(ErrorCode::InvalidTorrent, "the torrent has a file path that leaves its folder")
                .with_hint("this torrent is malformed or hostile; do not download it"));
        }
        Ok(Self { info_hash, name, files, single_file, bytes, checked: () })
    }

    pub fn total(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

pub type ParseTorrent = fn(&[u8]) -> Result<Metainfo, Error>;

#[derive(Clone, PartialEq, Debug)]
pub enum AddSource {
    Magnet(String),
    Metainfo(Vec<u8>),
}

#[derive(Clone, PartialEq, Debug)]
pub struct AddTorrent {
    pub id: TorrentId,
    pub source: AddSource,
    /// The download folder. The engine writes into `<save_path>/<name>`.
    pub save_path: PathBuf,
    /// One flag per file. `None` fetches no payload: for a magnet whose file list is
    /// not known yet, or a metadata probe.
    pub wanted: Option<Vec<bool>>,
    pub resume_data: Option<Vec<u8>>,
    pub paused: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Activity {
    Metadata,
    Checking,
    Downloading,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Stats {
    /// Wanted bytes in pieces that passed their hash check.
    pub verified: u64,
    pub down_rate: u64,
    pub up_rate: u64,
    pub peers: u32,
    pub seeds: u32,
}

#[derive(Clone, PartialEq, Debug)]
pub enum EngineEvent {
    Metadata {
        id: TorrentId,
        metainfo: Metainfo,
    },
    Activity {
        id: TorrentId,
        activity: Activity,
    },
    Stats {
        id: TorrentId,
        stats: Stats,
    },
    /// Every wanted piece passed its hash check.
    Finished {
        id: TorrentId,
    },
    /// Writes stopped and files are closed.
    Paused {
        id: TorrentId,
    },
    ResumeData {
        id: TorrentId,
        data: Vec<u8>,
    },
    /// A recheck finished. Counts wanted pieces that failed it, and wanted bytes that passed.
    Checked {
        id: TorrentId,
        damaged_pieces: u32,
        verified: u64,
    },
    /// A command failed, or the torrent stopped with an error such as a full disk.
    Failed {
        id: TorrentId,
        error: Error,
    },
    /// The torrent left the engine. Adding the same torrent again before this fails.
    Removed {
        id: TorrentId,
    },
}

pub trait EnginePort: Send {
    fn add(&mut self, torrent: AddTorrent) -> Result<(), Error>;
    fn select(&mut self, id: TorrentId, wanted: &[bool]) -> Result<(), Error>;
    fn pause(&mut self, id: TorrentId) -> Result<(), Error>;
    fn resume(&mut self, id: TorrentId) -> Result<(), Error>;
    fn recheck(&mut self, id: TorrentId) -> Result<(), Error>;
    fn save_resume_data(&mut self, id: TorrentId) -> Result<(), Error>;
    /// Saves only if the torrent downloaded or checked data since its last save. Otherwise no event follows.
    fn save_resume_data_if_progressed(&mut self, id: TorrentId) -> Result<(), Error>;
    /// Removes the torrent and keeps its files.
    fn remove(&mut self, id: TorrentId) -> Result<(), Error>;
    /// Verified bytes per file.
    fn file_progress(&mut self, id: TorrentId) -> Result<Vec<u64>, Error>;
    /// Waits up to `max_wait` for events.
    fn poll(&mut self, max_wait: Duration) -> Vec<EngineEvent>;
}

pub enum EngineCommand {
    Add(AddTorrent),
    Select(TorrentId, Vec<bool>),
    Pause(TorrentId),
    Resume(TorrentId),
    Recheck(TorrentId),
    SaveResume(TorrentId),
    SaveResumeIfProgressed(TorrentId),
    Remove(TorrentId),
    FileProgress(TorrentId, oneshot::Sender<Result<Vec<u64>, Error>>),
}

/// Sends commands to the engine thread. Dropping every handle stops the thread.
#[derive(Clone)]
pub struct EngineHandle {
    commands: mpsc::Sender<EngineCommand>,
}

impl EngineHandle {
    pub fn send(&self, command: EngineCommand) {
        // A closed channel means the engine thread is gone, which only happens at exit.
        let _ = self.commands.send(command);
    }

    pub async fn file_progress(&self, id: TorrentId) -> Result<Vec<u64>, Error> {
        let (reply, answer) = oneshot::channel();
        self.send(EngineCommand::FileProgress(id, reply));
        answer.await.map_err(|_| Error::internal("the engine thread stopped"))?
    }
}

/// How long the thread waits on the engine between command checks, which bounds command latency.
const POLL: Duration = Duration::from_millis(50);

pub fn spawn(
    mut port: Box<dyn EnginePort>,
    events: tokio_mpsc::UnboundedSender<EngineEvent>,
) -> (EngineHandle, std::thread::JoinHandle<()>) {
    let (commands, inbox) = mpsc::channel::<EngineCommand>();
    let thread = std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            loop {
                loop {
                    match inbox.try_recv() {
                        Ok(command) => {
                            if let Some(event) = apply(port.as_mut(), command) {
                                let _ = events.send(event);
                            }
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => return,
                    }
                }
                for event in port.poll(POLL) {
                    if events.send(event).is_err() {
                        return;
                    }
                }
            }
        })
        .expect("spawning the engine thread");
    (EngineHandle { commands }, thread)
}

fn apply(port: &mut dyn EnginePort, command: EngineCommand) -> Option<EngineEvent> {
    let (id, result) = match command {
        EngineCommand::Add(torrent) => (torrent.id, port.add(torrent)),
        EngineCommand::Select(id, wanted) => (id, port.select(id, &wanted)),
        EngineCommand::Pause(id) => (id, port.pause(id)),
        EngineCommand::Resume(id) => (id, port.resume(id)),
        EngineCommand::Recheck(id) => (id, port.recheck(id)),
        EngineCommand::SaveResume(id) => (id, port.save_resume_data(id)),
        EngineCommand::SaveResumeIfProgressed(id) => (id, port.save_resume_data_if_progressed(id)),
        EngineCommand::Remove(id) => (id, port.remove(id)),
        EngineCommand::FileProgress(id, reply) => {
            let _ = reply.send(port.file_progress(id));
            return None;
        }
    };
    result.err().map(|error| EngineEvent::Failed { id, error })
}

/// The `.torrent` format the fake engine reads: JSON instead of bencode.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct FakeTorrentFile {
    pub info_hash: InfoHash,
    pub name: String,
    pub files: Vec<(String, u64)>,
}

impl FakeTorrentFile {
    pub fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("fake torrents serialize")
    }

    pub fn metainfo(&self) -> Result<Metainfo, Error> {
        let files = self.files.iter().map(|(path, size)| MetaFile { path: path.clone(), size: *size }).collect();
        let single_file = self.files.len() == 1 && self.files[0].0 == self.name;
        Metainfo::new(self.info_hash, self.name.clone(), files, single_file, self.bytes())
    }
}

pub fn parse_fake(bytes: &[u8]) -> Result<Metainfo, Error> {
    serde_json::from_slice::<FakeTorrentFile>(bytes)
        .map_err(|e| Error::new(ErrorCode::InvalidTorrent, format!("not a torrent file: {e}")))?
        .metainfo()
}

#[derive(Clone, Default)]
pub struct Swarm(Arc<Mutex<SwarmState>>);

#[derive(Default)]
struct SwarmState {
    published: BTreeMap<TorrentId, FakeTorrentFile>,
    /// Without a published torrent, a magnet's `dn` and `xl` describe a one-file torrent.
    invent_from_magnets: bool,
    rate: u64,
    damaged: BTreeMap<TorrentId, u32>,
}

impl Swarm {
    pub fn new(rate: u64) -> Self {
        let swarm = Self::default();
        swarm.0.lock().unwrap().rate = rate;
        swarm
    }

    pub fn inventive(rate: u64) -> Self {
        let swarm = Self::new(rate);
        swarm.0.lock().unwrap().invent_from_magnets = true;
        swarm
    }

    pub fn publish(&self, torrent: FakeTorrentFile) {
        self.0.lock().unwrap().published.insert(torrent.info_hash.id(), torrent);
    }

    #[cfg(test)]
    pub fn set_rate(&self, rate: u64) {
        self.0.lock().unwrap().rate = rate;
    }

    #[cfg(test)]
    pub fn damage(&self, id: TorrentId, pieces: u32) {
        self.0.lock().unwrap().damaged.insert(id, pieces);
    }
}

pub struct Fake {
    swarm: Swarm,
    torrents: BTreeMap<TorrentId, FakeTorrent>,
    events: VecDeque<EngineEvent>,
    last_poll: Instant,
    last_stats: Instant,
}

const FAKE_STATS_EVERY: Duration = Duration::from_millis(100);

struct FakeTorrent {
    magnet: Option<String>,
    metainfo: Option<Metainfo>,
    wanted: Option<Vec<bool>>,
    verified: f64,
    /// `verified` at the last resume data.
    saved: f64,
    paused: bool,
    finished: bool,
    downloading: bool,
}

impl FakeTorrent {
    fn wanted_size(&self) -> Option<u64> {
        let (m, wanted) = (self.metainfo.as_ref()?, self.wanted.as_ref()?);
        Some(m.files.iter().zip(wanted).filter(|(_, w)| **w).map(|(f, _)| f.size).sum())
    }

    fn file_progress(&self) -> Vec<u64> {
        let (Some(m), Some(wanted)) = (&self.metainfo, &self.wanted) else { return Vec::new() };
        let mut left = self.verified as u64;
        m.files
            .iter()
            .zip(wanted)
            .map(|(f, w)| {
                let done = if *w { f.size.min(left) } else { 0 };
                left -= done;
                done
            })
            .collect()
    }

    fn stop_downloading(&mut self) {
        self.finished = false;
        self.downloading = false;
    }
}

impl Swarm {
    fn metadata(&self, id: TorrentId, magnet: Option<&str>) -> Option<Metainfo> {
        let state = self.0.lock().unwrap();
        if let Some(t) = state.published.get(&id) {
            return t.metainfo().ok();
        }
        if !state.invent_from_magnets {
            return None;
        }
        let magnet = super::magnet::parse(magnet?).ok()?;
        let size = magnet.exact_length.unwrap_or(1 << 30);
        let name = magnet.name.unwrap_or_else(|| id.to_hex());
        FakeTorrentFile { info_hash: magnet.info_hash, name: name.clone(), files: vec![(name, size)] }.metainfo().ok()
    }
}

impl Fake {
    pub fn new(swarm: Swarm) -> Self {
        let now = Instant::now();
        Self { swarm, torrents: BTreeMap::new(), events: VecDeque::new(), last_poll: now, last_stats: now }
    }

    fn torrent(&mut self, id: TorrentId) -> Result<&mut FakeTorrent, Error> {
        self.torrents.get_mut(&id).ok_or_else(|| Error::internal(format!("the engine has no torrent {id}")))
    }
}

impl EnginePort for Fake {
    fn add(&mut self, torrent: AddTorrent) -> Result<(), Error> {
        if self.torrents.contains_key(&torrent.id) {
            return Err(Error::internal(format!("the engine already has torrent {}", torrent.id)));
        }
        let (magnet, metainfo) = match torrent.source {
            AddSource::Magnet(uri) => (Some(uri), None),
            AddSource::Metainfo(bytes) => (None, Some(parse_fake(&bytes)?)),
        };
        let activity = if metainfo.is_some() { Activity::Checking } else { Activity::Metadata };
        let verified =
            torrent.resume_data.as_deref().and_then(|d| std::str::from_utf8(d).ok()?.parse().ok()).unwrap_or(0.0);
        self.events.push_back(EngineEvent::Activity { id: torrent.id, activity });
        let t = FakeTorrent {
            magnet,
            metainfo,
            wanted: torrent.wanted,
            verified,
            saved: verified,
            paused: torrent.paused,
            finished: false,
            downloading: false,
        };
        self.torrents.insert(torrent.id, t);
        Ok(())
    }

    fn select(&mut self, id: TorrentId, wanted: &[bool]) -> Result<(), Error> {
        let t = self.torrent(id)?;
        t.wanted = Some(wanted.to_vec());
        t.stop_downloading();
        Ok(())
    }

    fn pause(&mut self, id: TorrentId) -> Result<(), Error> {
        let t = self.torrent(id)?;
        t.paused = true;
        t.stop_downloading();
        self.events.push_back(EngineEvent::Paused { id });
        Ok(())
    }

    fn resume(&mut self, id: TorrentId) -> Result<(), Error> {
        let t = self.torrent(id)?;
        t.paused = false;
        t.stop_downloading();
        Ok(())
    }

    fn recheck(&mut self, id: TorrentId) -> Result<(), Error> {
        let damaged = self.swarm.0.lock().unwrap().damaged.remove(&id).unwrap_or(0);
        let t = self.torrent(id)?;
        t.verified = (t.verified - f64::from(damaged) * 16_384.0).max(0.0);
        t.stop_downloading();
        let verified = t.verified as u64;
        self.events.push_back(EngineEvent::Activity { id, activity: Activity::Checking });
        self.events.push_back(EngineEvent::Checked { id, damaged_pieces: damaged, verified });
        Ok(())
    }

    fn save_resume_data(&mut self, id: TorrentId) -> Result<(), Error> {
        let t = self.torrent(id)?;
        t.saved = t.verified;
        let data = t.verified.to_string().into_bytes();
        self.events.push_back(EngineEvent::ResumeData { id, data });
        Ok(())
    }

    fn save_resume_data_if_progressed(&mut self, id: TorrentId) -> Result<(), Error> {
        let t = self.torrent(id)?;
        if t.verified != t.saved {
            self.save_resume_data(id)?;
        }
        Ok(())
    }

    fn remove(&mut self, id: TorrentId) -> Result<(), Error> {
        self.torrents.remove(&id);
        self.events.push_back(EngineEvent::Removed { id });
        Ok(())
    }

    fn file_progress(&mut self, id: TorrentId) -> Result<Vec<u64>, Error> {
        Ok(self.torrent(id)?.file_progress())
    }

    fn poll(&mut self, max_wait: Duration) -> Vec<EngineEvent> {
        if self.events.is_empty() {
            std::thread::sleep(max_wait.min(Duration::from_millis(20)));
        }
        let dt = self.last_poll.elapsed().as_secs_f64();
        self.last_poll = Instant::now();
        let stats_due = self.last_stats.elapsed() >= FAKE_STATS_EVERY;
        if stats_due {
            self.last_stats = Instant::now();
        }
        let rate = self.swarm.0.lock().unwrap().rate;
        for (id, t) in &mut self.torrents {
            let id = *id;
            if t.metainfo.is_none() {
                if let Some(metainfo) = self.swarm.metadata(id, t.magnet.as_deref()) {
                    t.metainfo = Some(metainfo.clone());
                    self.events.push_back(EngineEvent::Metadata { id, metainfo });
                }
                continue;
            }
            let Some(size) = t.wanted_size() else { continue };
            if t.paused || t.finished {
                continue;
            }
            if !t.downloading {
                t.downloading = true;
                self.events.push_back(EngineEvent::Activity { id, activity: Activity::Downloading });
            }
            t.verified = (t.verified + rate as f64 * dt).min(size as f64);
            let done = t.verified as u64 >= size;
            if stats_due || done {
                let down_rate = if done { 0 } else { rate };
                let stats = Stats { verified: t.verified as u64, down_rate, up_rate: 0, peers: 3, seeds: 2 };
                self.events.push_back(EngineEvent::Stats { id, stats });
            }
            if done {
                t.finished = true;
                self.events.push_back(EngineEvent::Finished { id });
            }
        }
        self.events.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_that_leave_the_folder_are_refused() {
        let with = |name: &str, path: &str| {
            FakeTorrentFile { info_hash: InfoHash::V1([1; 20]), name: name.into(), files: vec![(path.into(), 1)] }
                .metainfo()
        };
        assert_eq!(with("set", "a/b.mkv").unwrap().files[0].path, "a/b.mkv");
        for (name, path) in
            [("set", "../etc/passwd"), ("set", "/etc/passwd"), ("..", "a"), ("a/b", "c"), ("set", "a//b")]
        {
            assert_eq!(with(name, path).unwrap_err().code, ErrorCode::InvalidTorrent, "{name} {path}");
        }
    }
}
