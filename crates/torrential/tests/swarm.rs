//! The real binary and libtorrent against a seeder this test runs on 127.0.0.1, with discovery
//! off. `TORRENTIAL_TEST_PEERS` hands the engine the seeder's address, the way a tracker would.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use assert_cmd::Command;
use serde_json::Value;
use sha2::Digest;
use torrential_engine as lt;

const PIECE: u32 = 16 * 1024;
const SLOW: Option<NonZeroU32> = NonZeroU32::new(256 * 1024);
/// libtorrent waits `min_reconnect_time`, 60 seconds, before it dials a peer again after
/// disconnecting it, as on pause or a refused connection. With one seeder, nothing else fills
/// that gap.
const RECONNECT: &str = "4m";

struct Home {
    dir: tempfile::TempDir,
    seeder: SocketAddr,
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Home {
    fn new(seeder: &Seeder) -> Self {
        Self { dir: tempfile::tempdir().unwrap(), seeder: seeder.addr }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::cargo_bin("torrential").unwrap();
        for key in ["CODEX_SANDBOX_NETWORK_DISABLED", "CODEX_THREAD_ID", "CODEX_CI", "TORRENTIAL_TEST_ENGINE"] {
            cmd.env_remove(key);
        }
        cmd.args(args)
            .env("HOME", self.dir.path())
            .env("TORRENTIAL_HOME", self.dir.path().join("th"))
            .env("TORRENTIAL_TEST_IDLE_TIMEOUT", "60s")
            .env("TORRENTIAL_TEST_NETWORK", "loopback")
            .env("TORRENTIAL_TEST_PEERS", self.seeder.to_string())
            .env("TORRENTIAL_TEST_DISK", "roomy")
            .timeout(Duration::from_secs(300));
        cmd
    }

    fn run(&self, args: &[&str]) -> Run {
        let out = self.command(args).output().unwrap();
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8(out.stdout).unwrap(),
            stderr: String::from_utf8(out.stderr).unwrap(),
        }
    }

    fn ok(&self, args: &[&str]) -> Value {
        let mut args = args.to_vec();
        args.push("--json");
        let run = self.run(&args);
        assert_eq!(run.code, 0, "torrential {args:?}: {}{}", run.stdout, run.stderr);
        serde_json::from_str(&run.stdout).unwrap()
    }

    fn job(&self, id: &str) -> Value {
        self.ok(&["status", id])["jobs"][0].clone()
    }

    fn until(&self, id: &str, what: &str, done: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let job = self.job(id);
            if done(&job) {
                return job;
            }
            assert!(Instant::now() < deadline, "timed out waiting until {what}: {job:#}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn pid(&self) -> i32 {
        self.ok(&["daemon", "status"])["pid"].as_i64().expect("the engine runs") as i32
    }

    fn logged_addrs(&self) -> Vec<SocketAddr> {
        let log = std::fs::read_to_string(self.dir.path().join("th/state/engine.log")).unwrap_or_default();
        log.lines().filter_map(|l| l.split_once(" listening on ")?.1.strip_suffix(" (tcp)")?.parse().ok()).collect()
    }

    fn downloads(&self) -> PathBuf {
        self.dir.path().join("dl")
    }

    fn get(&self, target: &str, extra: &[&str]) -> Value {
        let dir = self.downloads().display().to_string();
        let mut args = vec!["get", target, "--dir", &dir];
        args.extend(extra);
        self.ok(&args)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.command(&["daemon", "stop"]).output();
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    name: &'static str,
    files: BTreeMap<&'static str, Vec<u8>>,
    torrent: lt::Metainfo,
}

impl Fixture {
    fn new(name: &'static str, files: &[(&'static str, usize)], seed: u64) -> Self {
        let dir = tempfile::tempdir().unwrap();
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
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            contents.insert(file, bytes);
        }
        let piece = lt::PieceSize::new(PIECE).unwrap();
        let torrent = lt::create_torrent(&dir.path().join(name), piece, lt::TorrentFormat::V1).unwrap();
        Self { dir, name, files: contents, torrent }
    }

    fn id(&self) -> String {
        hex(self.torrent.info_hash().v1().unwrap().as_bytes())
    }

    fn short(&self) -> String {
        self.id()[..6].to_string()
    }

    fn magnet(&self) -> String {
        format!("magnet:?xt=urn:btih:{}&dn={}", self.id(), self.name)
    }

    fn torrent_file(&self) -> String {
        let path = self.dir.path().join(format!("{}.torrent", self.name));
        std::fs::write(&path, self.torrent.as_bytes()).unwrap();
        path.display().to_string()
    }

    fn total(&self) -> u64 {
        self.files.values().map(|b| b.len() as u64).sum()
    }

    fn downloaded(&self, home: &Home, file: &str) -> PathBuf {
        home.downloads().join(self.name).join(file)
    }

    /// Whether the download of `file` holds exactly the seeded bytes.
    fn intact(&self, home: &Home, file: &str) -> bool {
        std::fs::read(self.downloaded(home, file)).is_ok_and(|b| sha256(&b) == sha256(&self.files[file]))
    }
}

/// A libtorrent session in this process that seeds one torrent.
struct Seeder {
    session: lt::Session,
    id: lt::TorrentId,
    addr: SocketAddr,
}

impl Seeder {
    fn start(fixture: &Fixture, rate: Option<NonZeroU32>) -> Self {
        let config = lt::Config {
            listen: vec![SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0)],
            dht: lt::Dht::Off,
            lsd: false,
            upnp: false,
            natpmp: false,
            upload: rate.map_or(lt::UploadPolicy::Normal, lt::UploadPolicy::Capped),
            connections: lt::ConnectionLimits { multiple_per_ip: true, ..lt::ConnectionLimits::default() },
            ..lt::Config::default()
        };
        let mut session = lt::Session::new(config).unwrap();
        let source = lt::TorrentSource::Metainfo(fixture.torrent.clone());
        let id = session.add(lt::AddTorrent::new(source, fixture.dir.path())).unwrap().id();
        let deadline = Instant::now() + Duration::from_secs(20);
        let seeding = |s: &lt::Session| s.status(id).unwrap().progress.verified_selected_bytes() == fixture.total();
        let port = loop {
            if let (true, Some(port)) = (seeding(&session), session.listen_port().unwrap()) {
                break port;
            }
            assert!(Instant::now() < deadline, "the seeder never verified its files");
            std::thread::sleep(Duration::from_millis(20));
        };
        Self { session, id, addr: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port) }
    }

    fn uploaded(&self) -> u64 {
        self.session.status(self.id).unwrap().uploaded_payload
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    sha2::Sha256::digest(bytes).into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn verified(job: &Value) -> u64 {
    job["progress"]["verified"].as_u64().unwrap()
}

fn state(job: &Value) -> &str {
    job["state"].as_str().unwrap()
}

/// Size, modification time, and contents of every file under `dir`.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (u64, std::time::SystemTime, [u8; 32])> {
    let mut files = BTreeMap::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = std::fs::metadata(&path).unwrap();
            if meta.is_dir() {
                pending.push(path);
            } else {
                files.insert(
                    path.clone(),
                    (meta.len(), meta.modified().unwrap(), sha256(&std::fs::read(&path).unwrap())),
                );
            }
        }
    }
    files
}

const SET: [(&str, usize); 3] = [("a.bin", 300_000), ("b.bin", 200_000), ("docs/readme.txt", 70_000)];
const BIG: [(&str, usize); 2] = [("a.bin", 1_200_000), ("b.bin", 900_000)];

#[test]
fn a_torrent_file_downloads_and_every_file_matches_its_source() {
    let fixture = Fixture::new("set", &SET, 1);
    let seeder = Seeder::start(&fixture, None);
    let home = Home::new(&seeder);
    let get = home.get(&fixture.torrent_file(), &[]);
    assert_eq!(get["reply"]["outcome"], "started", "{get:#}");
    let wait = home.run(&["wait", &fixture.short()]);
    assert_eq!(wait.code, 0, "{}{}", wait.stdout, wait.stderr);
    let job = home.job(&fixture.id());
    assert_eq!((state(&job), verified(&job)), ("done", fixture.total()));
    for file in fixture.files.keys() {
        assert!(fixture.intact(&home, file), "{file} matches its source");
    }
}

#[test]
fn a_magnet_downloads_and_every_file_matches_its_source() {
    let fixture = Fixture::new("set", &SET, 2);
    let seeder = Seeder::start(&fixture, None);
    let home = Home::new(&seeder);
    home.get(&fixture.magnet(), &[]);
    let wait = home.run(&["wait", &fixture.short()]);
    assert_eq!(wait.code, 0, "{}{}", wait.stdout, wait.stderr);
    assert_eq!(state(&home.job(&fixture.id())), "done");
    for file in fixture.files.keys() {
        assert!(fixture.intact(&home, file), "{file} matches its source");
    }
}

#[test]
fn a_files_glob_downloads_only_the_matching_files() {
    let fixture = Fixture::new("set", &SET, 3);
    let seeder = Seeder::start(&fixture, None);
    let home = Home::new(&seeder);
    home.get(&fixture.magnet(), &["--files", "*.bin"]);
    let wait = home.run(&["wait", &fixture.short()]);
    assert_eq!(wait.code, 0, "{}{}", wait.stdout, wait.stderr);
    let job = home.job(&fixture.id());
    assert_eq!((state(&job), job["progress"]["size"].as_u64()), ("done", Some(500_000)));
    assert!(fixture.intact(&home, "a.bin") && fixture.intact(&home, "b.bin"));
    assert!(!fixture.intact(&home, "docs/readme.txt"), "the unselected file was not downloaded");
    let files = home.ok(&["files", &fixture.short()]);
    let selected: Vec<(&str, bool)> = files["files"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["path"].as_str().unwrap(), f["selected"].as_bool().unwrap()))
        .collect();
    assert_eq!(selected, [("a.bin", true), ("b.bin", true), ("docs/readme.txt", false)]);
}

#[test]
fn pause_stops_writes_and_resume_finishes() {
    let fixture = Fixture::new("big", &BIG, 4);
    let seeder = Seeder::start(&fixture, SLOW);
    let home = Home::new(&seeder);
    home.get(&fixture.torrent_file(), &[]);
    home.until(&fixture.id(), "a quarter is verified", |j| verified(j) >= fixture.total() / 4);
    let pause = home.ok(&["pause", &fixture.short()]);
    assert_eq!(pause["changes"][0]["result"], "paused");
    let paused = home.job(&fixture.id());
    let before = snapshot(&home.downloads());
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(snapshot(&home.downloads()), before, "no file changed after pause returned");
    let still = home.job(&fixture.id());
    assert_eq!((state(&still), verified(&still)), ("paused", verified(&paused)));
    assert!(verified(&still) < fixture.total(), "paused mid-download");
    assert_eq!(home.ok(&["resume", &fixture.short()])["changes"][0]["result"], "resumed");
    let wait = home.run(&["wait", &fixture.short(), "--timeout", RECONNECT]);
    assert_eq!(wait.code, 0, "{}{}", wait.stdout, wait.stderr);
    for file in fixture.files.keys() {
        assert!(fixture.intact(&home, file), "{file} matches its source");
    }
}

#[test]
fn a_killed_engine_resumes_without_downloading_verified_pieces_again() {
    let fixture = Fixture::new("big", &BIG, 5);
    let seeder = Seeder::start(&fixture, SLOW);
    let home = Home::new(&seeder);
    home.get(&fixture.torrent_file(), &[]);
    let job = home.until(&fixture.id(), "a third is verified", |j| verified(j) >= fixture.total() / 3);
    let before_kill = verified(&job);
    let addr = home.logged_addrs().pop().expect("the engine logged its listen address");
    std::thread::sleep(Duration::from_secs(1));
    let pid = home.pid();
    // SAFETY: kill only sends a signal, and this pid is the engine this test started.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    let deadline = Instant::now() + Duration::from_secs(5);
    // SAFETY: signal 0 only checks that the process exists.
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(Instant::now() < deadline, "the killed engine is still alive");
        std::thread::sleep(Duration::from_millis(20));
    }
    let sent_before_restart = seeder.uploaded();

    assert_eq!(state(&home.job(&fixture.id())), "interrupted");
    assert_eq!(home.logged_addrs().last(), Some(&addr), "the new engine listens on the same port");
    assert_eq!(home.ok(&["resume", &fixture.short()])["changes"][0]["result"], "resumed");
    let wait = home.run(&["wait", &fixture.short(), "--timeout", RECONNECT]);
    assert_eq!(wait.code, 0, "{}{}", wait.stdout, wait.stderr);
    let resent = seeder.uploaded() - sent_before_restart;
    assert!(
        resent <= fixture.total() - before_kill,
        "the restarted engine downloaded {resent} bytes, more than the {} it had not verified",
        fixture.total() - before_kill
    );
    for file in fixture.files.keys() {
        assert!(fixture.intact(&home, file), "{file} matches its source");
    }
}

#[test]
fn verify_reports_a_corrupted_byte_and_repair_fixes_it() {
    let fixture = Fixture::new("set", &SET, 6);
    let seeder = Seeder::start(&fixture, None);
    let home = Home::new(&seeder);
    home.get(&fixture.torrent_file(), &[]);
    assert_eq!(home.run(&["wait", &fixture.short()]).code, 0);
    let damaged = fixture.downloaded(&home, "a.bin");
    let offset = 3 * u64::from(PIECE) + 100;
    let mut file = OpenOptions::new().write(true).open(&damaged).unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(&[!fixture.files["a.bin"][offset as usize]]).unwrap();
    drop(file);

    assert_eq!(home.ok(&["verify", &fixture.short()])["changes"][0]["result"], "checking");
    let job = home.until(&fixture.id(), "the check reports", |j| state(j) != "checking");
    assert_eq!(job["error"]["code"], "verification_failed", "{job:#}");
    assert_eq!(job["error"]["message"], "1 damaged piece in the selected files");
    assert!(!fixture.intact(&home, "a.bin"), "verify alone repairs nothing");

    assert_eq!(home.ok(&["verify", &fixture.short(), "--repair"])["changes"][0]["result"], "checking");
    let wait = home.run(&["wait", &fixture.short()]);
    assert_eq!(wait.code, 0, "{}{}", wait.stdout, wait.stderr);
    assert!(fixture.intact(&home, "a.bin"), "repair downloaded the damaged piece again");
}

#[test]
fn wait_exits_8_on_a_short_timeout_and_0_when_done() {
    let fixture = Fixture::new("big", &BIG, 7);
    let seeder = Seeder::start(&fixture, SLOW);
    let home = Home::new(&seeder);
    home.get(&fixture.magnet(), &[]);
    let short = home.run(&["wait", &fixture.short(), "--timeout", "1s"]);
    assert_eq!(short.code, 8, "{}{}", short.stdout, short.stderr);
    let wait = home.run(&["wait", &fixture.short()]);
    assert_eq!(wait.code, 0, "{}{}", wait.stdout, wait.stderr);
    assert_eq!(state(&home.job(&fixture.id())), "done");
}
