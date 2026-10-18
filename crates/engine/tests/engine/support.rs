use std::collections::BTreeMap;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha1::Digest as _;
use tempfile::TempDir;
use torrential_engine::{
    AddTorrent, Alert, Config, ConnectionLimits, Dht, InfoHash, Metainfo, PieceSize, Session,
    TorrentFormat, TorrentId, TorrentSource, TorrentStatus, UploadPolicy, create_torrent,
};

pub const PIECE: u32 = 16 * 1024;
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// A session on 127.0.0.1 with DHT, LSD, and port mapping off, so peers only
/// meet through `connect_peer` or `AddTorrent::peers`.
pub fn loopback() -> Config {
    Config {
        listen: vec![SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0)],
        dht: Dht::Off,
        lsd: false,
        upnp: false,
        natpmp: false,
        connections: ConnectionLimits {
            multiple_per_ip: true,
            ..ConnectionLimits::default()
        },
        ..Config::default()
    }
}

pub fn rate(bytes_per_sec: u32) -> Option<NonZeroU32> {
    NonZeroU32::new(bytes_per_sec)
}

pub fn upload(policy: UploadPolicy) -> Config {
    Config {
        upload: policy,
        ..loopback()
    }
}

/// A session plus every alert it has produced, so a test can wait for one
/// alert and later ask what else happened.
pub struct Node {
    pub session: Session,
    pub log: Vec<Alert>,
    port: u16,
}

impl Node {
    pub fn new(config: Config) -> Node {
        let session = Session::new(config).expect("session starts");
        let mut node = Node {
            session,
            log: Vec::new(),
            port: 0,
        };
        node.wait_for("listen socket", |a| matches!(a, Alert::ListenSucceeded { .. }));
        node.port = node.session.listen_port().unwrap().expect("listening");
        node
    }

    pub fn addr(&self) -> SocketAddr {
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), self.port)
    }

    pub fn add(&mut self, add: AddTorrent) -> TorrentId {
        self.session.add(add).expect("torrent added").id()
    }

    /// Adds `metainfo` over existing data in `save_path` and waits until it
    /// has checked the files.
    pub fn seed(&mut self, metainfo: &Metainfo, save_path: &Path) -> TorrentId {
        let id = self.add(AddTorrent::new(
            TorrentSource::Metainfo(metainfo.clone()),
            save_path,
        ));
        self.wait_until("seeding", |s| {
            s.status(id).unwrap().progress.verified_selected_bytes() == total_size(metainfo)
        });
        id
    }

    pub fn status(&self, id: TorrentId) -> TorrentStatus {
        self.session.status(id).expect("status")
    }

    fn pump(&mut self, timeout: Duration) -> Vec<Alert> {
        let alerts = self.session.wait_alerts(timeout).expect("alerts");
        self.log.extend(alerts.iter().cloned());
        alerts
    }

    /// Reads alerts until one matches, and returns it.
    pub fn wait_for(&mut self, what: &str, mut matches: impl FnMut(&Alert) -> bool) -> Alert {
        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            if let Some(found) = self.pump(Duration::from_millis(100)).into_iter().find(&mut matches) {
                return found;
            }
        }
        panic!("timed out waiting for {what}; alerts seen: {:#?}", self.log);
    }

    /// Polls the session, reading alerts in between, until `done` holds.
    pub fn wait_until(&mut self, what: &str, mut done: impl FnMut(&Session) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            if done(&self.session) {
                return;
            }
            self.pump(Duration::from_millis(50));
        }
        panic!("timed out waiting for {what}; alerts seen: {:#?}", self.log);
    }

    /// Reads alerts for `window` without waiting on anything.
    pub fn idle(&mut self, window: Duration) {
        let deadline = Instant::now() + window;
        while Instant::now() < deadline {
            self.pump(Duration::from_millis(50));
        }
    }
}

/// Files under one directory, named after the torrent they become.
pub struct Fixture {
    pub dir: TempDir,
    pub name: String,
    pub files: BTreeMap<String, Vec<u8>>,
}

impl Fixture {
    /// Pseudo-random contents, seeded so every run builds the same torrent.
    pub fn new(name: &str, files: &[(&str, usize)], seed: u64) -> Fixture {
        let dir = TempDir::new().unwrap();
        let mut state = seed | 1;
        let mut contents = BTreeMap::new();
        for &(file, size) in files {
            let bytes: Vec<u8> = (0..size)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (state >> 24) as u8
                })
                .collect();
            let path = dir.path().join(name).join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &bytes).unwrap();
            contents.insert(file.to_owned(), bytes);
        }
        Fixture {
            dir,
            name: name.to_owned(),
            files: contents,
        }
    }

    pub fn save_path(&self) -> &Path {
        self.dir.path()
    }

    pub fn torrent(&self, format: TorrentFormat) -> Metainfo {
        create_torrent(
            &self.dir.path().join(&self.name),
            PieceSize::new(PIECE).unwrap(),
            format,
        )
        .expect("torrent created")
    }

    pub fn path_in(&self, save_path: &Path, file: &str) -> PathBuf {
        save_path.join(&self.name).join(file)
    }

    pub fn sha256(&self, file: &str) -> [u8; 32] {
        sha256(&self.files[file])
    }
}

pub fn total_size(metainfo: &Metainfo) -> u64 {
    metainfo.files().iter().map(|f| f.size).sum()
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    sha2::Sha256::digest(bytes).into()
}

pub fn sha256_file(path: &Path) -> Option<[u8; 32]> {
    fs::read(path).ok().map(|b| sha256(&b))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A magnet built from hashes the test computed itself, with no tracker.
pub fn magnet(v1: Option<[u8; 20]>, v2: Option<[u8; 32]>) -> String {
    let mut uri = String::from("magnet:?");
    if let Some(v1) = v1 {
        uri += &format!("xt=urn:btih:{}&", hex(&v1));
    }
    if let Some(v2) = v2 {
        uri += &format!("xt=urn:btmh:1220{}&", hex(&v2));
    }
    uri.pop();
    uri
}

/// The info hashes of a `.torrent`, computed from its raw info dictionary
/// without libtorrent.
pub fn expected_hashes(torrent: &[u8]) -> ([u8; 20], [u8; 32]) {
    let info = bencode::dict_value(torrent, b"info").expect("info dictionary");
    (sha1::Sha1::digest(info).into(), sha256(info))
}

pub fn info_hash(hashes: ([u8; 20], [u8; 32]), format: TorrentFormat) -> InfoHash {
    let v1 = torrential_engine::InfoHashV1::from_bytes(hashes.0);
    match format {
        TorrentFormat::V1 => InfoHash::V1(v1),
        TorrentFormat::Hybrid => InfoHash::Hybrid {
            v1,
            v2: torrential_engine::InfoHashV2::from_bytes(hashes.1),
        },
    }
}

/// Enough of a bencode reader to find raw values by key.
pub mod bencode {
    /// The raw bytes of `key`'s value in the dictionary that starts `data`.
    pub fn dict_value<'a>(data: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
        if data.first() != Some(&b'd') {
            return None;
        }
        let mut pos = 1;
        while data.get(pos)? != &b'e' {
            let key_end = skip(data, pos)?;
            let k = string(&data[pos..key_end])?;
            let value_end = skip(data, key_end)?;
            if k == key {
                return Some(&data[key_end..value_end]);
            }
            pos = value_end;
        }
        None
    }

    /// The contents of a raw bencoded string.
    pub fn string(raw: &[u8]) -> Option<&[u8]> {
        let colon = raw.iter().position(|&b| b == b':')?;
        let len: usize = std::str::from_utf8(&raw[..colon]).ok()?.parse().ok()?;
        raw.get(colon + 1..colon + 1 + len)
    }

    fn skip(data: &[u8], pos: usize) -> Option<usize> {
        match *data.get(pos)? {
            b'i' => Some(pos + data[pos..].iter().position(|&b| b == b'e')? + 1),
            b'l' | b'd' => {
                let mut p = pos + 1;
                while *data.get(p)? != b'e' {
                    p = skip(data, p)?;
                }
                Some(p + 1)
            }
            b'0'..=b'9' => {
                let colon = pos + data[pos..].iter().position(|&b| b == b':')?;
                let len: usize = std::str::from_utf8(&data[pos..colon]).ok()?.parse().ok()?;
                Some(colon + 1 + len)
            }
            _ => None,
        }
    }
}
