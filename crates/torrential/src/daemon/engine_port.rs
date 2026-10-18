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
use torrential_core::{Error, ErrorCode, InfoHash, PieceMap, Slice, TorrentId};

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
    /// `None` until the engine has the file list and the owner's selection.
    pub piece_map: Option<PieceMap>,
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
    /// Writes stopped and files are closed. A `Stats` with the final count comes first.
    Paused {
        id: TorrentId,
    },
    ResumeData {
        id: TorrentId,
        data: Vec<u8>,
        /// Wanted bytes verified when the data was taken, if the engine could count them.
        verified: Option<u64>,
        /// The piece map when the data was taken, if the engine could sample it.
        piece_map: Option<PieceMap>,
        /// Verified bytes in each file when the data was taken, counted with `verified`.
        files: Option<Vec<u64>>,
    },
    /// A recheck finished. Counts wanted pieces that failed it, and wanted bytes that passed.
    Checked {
        id: TorrentId,
        damaged_pieces: u32,
        verified: u64,
        piece_map: Option<PieceMap>,
        /// Verified bytes in each file, counted with `verified`.
        files: Vec<u64>,
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
    /// Wanted bytes the swarm has sent. They verify in whole pieces, as [`fake_landing`] lands them.
    arrived: f64,
    /// `arrived` at the last resume data.
    saved: f64,
    paused: bool,
    finished: bool,
    downloading: bool,
}

impl FakeTorrent {
    fn landing(&self) -> Option<Landing> {
        Some(fake_landing(self.metainfo.as_ref()?, self.wanted.as_deref()?, self.arrived as u64))
    }

    fn progress(&self) -> (u64, Option<PieceMap>) {
        self.landing().map_or((0, None), |l| (l.verified, l.piece_map))
    }

    fn files(&self) -> Vec<u64> {
        self.landing().map(|l| l.files).unwrap_or_default()
    }

    fn wanted_size(&self) -> Option<u64> {
        let (m, wanted) = (self.metainfo.as_ref()?, self.wanted.as_ref()?);
        Some(m.files.iter().zip(wanted).filter(|(_, w)| **w).map(|(f, _)| f.size).sum())
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
        let arrived =
            torrent.resume_data.as_deref().and_then(|d| std::str::from_utf8(d).ok()?.parse().ok()).unwrap_or(0.0);
        self.events.push_back(EngineEvent::Activity { id: torrent.id, activity });
        let t = FakeTorrent {
            magnet,
            metainfo,
            wanted: torrent.wanted,
            arrived,
            saved: arrived,
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
        let (verified, piece_map) = t.progress();
        self.events.push_back(EngineEvent::Stats { id, stats: Stats { verified, piece_map, ..Stats::default() } });
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
        let piece = t.metainfo.as_ref().map_or(0, |m| fake_piece_size(m.total()));
        t.arrived = (t.arrived - f64::from(damaged) * piece as f64).max(0.0);
        t.stop_downloading();
        let (verified, piece_map) = t.progress();
        let files = t.files();
        self.events.push_back(EngineEvent::Activity { id, activity: Activity::Checking });
        self.events.push_back(EngineEvent::Checked { id, damaged_pieces: damaged, verified, piece_map, files });
        Ok(())
    }

    fn save_resume_data(&mut self, id: TorrentId) -> Result<(), Error> {
        let t = self.torrent(id)?;
        t.saved = t.arrived;
        let (verified, piece_map) = t.progress();
        let (data, files) = (t.arrived.to_string().into_bytes(), Some(t.files()));
        self.events.push_back(EngineEvent::ResumeData { id, data, verified: Some(verified), piece_map, files });
        Ok(())
    }

    fn save_resume_data_if_progressed(&mut self, id: TorrentId) -> Result<(), Error> {
        let t = self.torrent(id)?;
        if t.arrived != t.saved {
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
        Ok(self.torrent(id)?.files())
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
            t.arrived = (t.arrived + rate as f64 * dt).min(size as f64);
            let (verified, piece_map) = t.progress();
            let done = verified >= size;
            if stats_due || done {
                let down_rate = if done { 0 } else { rate };
                let stats = Stats { verified, piece_map, down_rate, up_rate: 0, peers: 3, seeds: 2 };
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

/// The fake engine's torrents have about this many pieces, whatever their size.
const FAKE_PIECES: u64 = 1024;
/// Pieces arrive in runs this long, a quarter of a slice, at shuffled places.
const FAKE_RUN: usize = 4;

/// Where a fake torrent's data has landed.
#[derive(Clone, PartialEq, Debug)]
pub struct Landing {
    /// Bytes of wanted files in landed pieces.
    pub verified: u64,
    /// Bytes of each file in landed pieces, wanted or not.
    pub files: Vec<u64>,
    pub piece_map: Option<PieceMap>,
}

fn fake_piece_size(total: u64) -> u64 {
    total.div_ceil(FAKE_PIECES)
}

/// Lands `arrived` wanted bytes as whole pieces, the way a real engine verifies them. Runs of
/// pieces land in an order shuffled by the info hash, the way a swarm scatters them, so a slice
/// only ever fills and the same bytes always land the same pieces. The map and the files are
/// counted from the same pieces, so they agree.
pub fn fake_landing(metainfo: &Metainfo, wanted: &[bool], arrived: u64) -> Landing {
    let total = metainfo.total();
    let mut files = vec![0; metainfo.files.len()];
    if total == 0 {
        return Landing { verified: 0, files, piece_map: None };
    }
    let piece = fake_piece_size(total);
    let pieces = total.div_ceil(piece) as usize;
    let mut want = vec![0; pieces];
    for (file, p, bytes) in overlaps(metainfo, piece) {
        if wanted[file] {
            want[p] += bytes;
        }
    }
    let mut runs: Vec<usize> = (0..pieces.div_ceil(FAKE_RUN)).collect();
    let mut seed = u64::from_le_bytes(metainfo.info_hash.id().as_bytes()[..8].try_into().expect("an ID has 20 bytes"));
    for i in (1..runs.len()).rev() {
        runs.swap(i, (splitmix(&mut seed) % (i as u64 + 1)) as usize);
    }
    let mut have = vec![false; pieces];
    let mut left = arrived;
    let order = runs.iter().flat_map(|run| run * FAKE_RUN..((run + 1) * FAKE_RUN).min(pieces));
    for p in order.filter(|&p| want[p] > 0) {
        if want[p] > left {
            break;
        }
        have[p] = true;
        left -= want[p];
    }
    for (file, p, bytes) in overlaps(metainfo, piece) {
        if have[p] {
            files[file] += bytes;
        }
    }
    let piece_map = PieceMap::new(std::array::from_fn(|s| {
        let begin = s * pieces / PieceMap::SLICES;
        let run = begin..(begin + 1).max((s + 1) * pieces / PieceMap::SLICES);
        Slice::of(run.clone().filter(|&p| have[p]).count() as u32, run.filter(|&p| want[p] > 0).count() as u32)
    }));
    Landing { verified: arrived - left, files, piece_map: Some(piece_map) }
}

/// Each file's bytes in each piece it touches, as `(file, piece, bytes)`.
fn overlaps(metainfo: &Metainfo, piece: u64) -> impl Iterator<Item = (usize, usize, u64)> + '_ {
    let mut start = 0;
    metainfo.files.iter().enumerate().flat_map(move |(file, f)| {
        let (begin, end) = (start, start + f.size);
        start = end;
        (begin / piece..end.div_ceil(piece))
            .map(move |p| (file, p as usize, end.min((p + 1) * piece) - begin.max(p * piece)))
    })
}

/// Why a fake download's file bytes disagree with its verified count or its map, if they do. A
/// slice says something only about a file it lies inside: a full slice's bytes landed in that
/// file, an empty one's did not, and a partly filled one holds a landed piece and a missing one.
#[cfg(test)]
pub fn disagreement(
    metainfo: &Metainfo,
    wanted: &[bool],
    verified: u64,
    files: &[u64],
    map: &PieceMap,
) -> Option<String> {
    let selected: u64 = files.iter().zip(wanted).filter(|(_, w)| **w).map(|(bytes, _)| bytes).sum();
    if selected != verified {
        return Some(format!("the selected files hold {selected} bytes, but {verified} are verified"));
    }
    let total = metainfo.total();
    let piece = fake_piece_size(total);
    let pieces = total.div_ceil(piece) as usize;
    let mut start = 0;
    let spans: Vec<(u64, u64)> = metainfo
        .files
        .iter()
        .map(|f| {
            start += f.size;
            (start - f.size, start)
        })
        .collect();
    let (mut landed, mut missing) = (vec![0; files.len()], vec![0; files.len()]);
    for (s, slice) in map.slices().iter().enumerate() {
        let Slice::Wanted(eighths) = slice else { continue };
        let begin = s * pieces / PieceMap::SLICES;
        let end = (begin + 1).max((s + 1) * pieces / PieceMap::SLICES);
        let (lo, hi) = (begin as u64 * piece, (end as u64 * piece).min(total));
        let Some(file) = spans.iter().position(|&(a, b)| a <= lo && hi <= b) else { continue };
        let smallest = piece.min(hi - (end as u64 - 1) * piece);
        match eighths.get() {
            8 => landed[file] += hi - lo,
            0 => missing[file] += hi - lo,
            _ => {
                landed[file] += smallest;
                missing[file] += smallest;
            }
        }
    }
    metainfo.files.iter().enumerate().find_map(|(i, f)| {
        (files[i] < landed[i] || f.size - files[i] < missing[i]).then(|| {
            format!(
                "{} holds {} of {} bytes, but the map says at least {} landed and {} did not",
                f.path, files[i], f.size, landed[i], missing[i]
            )
        })
    })
}

/// SplitMix64, so the shuffle needs no random-number crate.
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fake_map_fills_out_of_order_and_leaves_skipped_files_unwanted() {
        let set = FakeTorrentFile {
            info_hash: InfoHash::V1([7; 20]),
            name: "set".into(),
            files: vec![("a.bin".into(), 1 << 30), ("b.bin".into(), 1 << 30)],
        };
        let metainfo = set.metainfo().unwrap();
        let map = |percent: u64| fake_landing(&metainfo, &[true, false], (1 << 30) * percent / 100).piece_map.unwrap();
        let half = map(50).to_string();
        assert_eq!(half[32..], "-".repeat(32), "b.bin is skipped");
        assert!(half.as_bytes()[..32].windows(2).any(|w| w[0] < w[1]), "{half} does not fill from the start");
        let eighths: u32 = half[..32].bytes().map(|b| u32::from(b - b'0')).sum();
        assert_eq!(eighths, 32 * 4, "{half} holds half of a.bin");
        let maps: Vec<String> = (0..=100).step_by(10).map(|percent| map(percent).to_string()).collect();
        for pair in maps.windows(2) {
            assert!(pair[0].bytes().zip(pair[1].bytes()).all(|(a, b)| a <= b), "a slice emptied: {pair:?}");
        }
        assert_eq!(maps[0], format!("{}{}", "0".repeat(32), "-".repeat(32)));
        assert_eq!(maps[10], format!("{}{}", "8".repeat(32), "-".repeat(32)));
    }

    #[test]
    fn a_fake_landing_counts_files_from_the_pieces_its_map_draws() {
        let set = FakeTorrentFile {
            info_hash: InfoHash::V1([9; 20]),
            name: "set".into(),
            files: vec![("a.bin".into(), 700_000_123), ("b.bin".into(), 300_000_017), ("c.bin".into(), 500_000_311)],
        };
        let metainfo = set.metainfo().unwrap();
        let wanted = [true, false, true];
        for arrived in [0, 1, 123_456_789, 600_000_000, 1_199_000_000] {
            let landing = fake_landing(&metainfo, &wanted, arrived);
            assert!(landing.verified <= arrived, "{} verified from {arrived} arrived", landing.verified);
            let map = landing.piece_map.unwrap();
            assert_eq!(disagreement(&metainfo, &wanted, landing.verified, &landing.files, &map), None, "{map}");
        }
        let full = fake_landing(&metainfo, &wanted, 1_200_000_434);
        assert_eq!(full.verified, 1_200_000_434);
        assert_eq!(full.files, [700_000_123, 195_787 + 975_850, 500_000_311], "b.bin shares two pieces");
    }

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
