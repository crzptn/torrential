use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use torrential_core::protocol::{Change, FilesReply, GetOutcome, GetReply, GetRequest, Request, Response};
use torrential_core::{Error, ErrorCode, InfoHash, Job, JobStatus, Selection, Target};

use super::admission::{FakeDisk, GIB, Space};
use super::engine_port::{Fake, FakeTorrentFile, MetaFile, Metainfo, Swarm, disagreement, parse_fake};
use super::owner::OwnerConfig;
use super::search_port::{self, fake_catalog};
use super::store::Store;
use super::{Ports, Running, server, start};

const ROOMY: Space = Space { device: 1, total: 1000 * GIB, free: 500 * GIB };
/// A 1 GiB fake torrent's piece size.
const GIB_PIECE: u64 = GIB / 1024;

struct Harness {
    dir: tempfile::TempDir,
    running: Running,
    swarm: Swarm,
    disk: FakeDisk,
    search: search_port::Fake,
}

fn config(dir: &std::path::Path) -> OwnerConfig {
    OwnerConfig {
        default_dir: dir.join("downloads"),
        probe_dir: dir.join("probes"),
        max_active: super::MAX_ACTIVE,
        idle_timeout: None,
        resume_on_start: false,
        save_resume_every: Duration::from_millis(100),
        stall_after: Duration::from_millis(500),
    }
}

impl Harness {
    fn new() -> Self {
        Self::with_catalog(fake_catalog())
    }

    fn with_catalog(catalog: Vec<torrential_core::Release>) -> Self {
        Self::with_search(search_port::Fake::new(catalog))
    }

    fn with_search(search: search_port::Fake) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (swarm, disk) = (Swarm::new(0), FakeDisk::new(ROOMY));
        let running = Self::boot(&dir, &swarm, &disk, &search);
        Self { dir, running, swarm, disk, search }
    }

    /// The downloads `TORRENTIAL_TEST_ENGINE=fake` starts with, in an engine that has loaded none of them.
    fn seeded() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("state.db")).unwrap();
        super::fixture::seed(&mut store, &dir.path().join("downloads"), 0).unwrap();
        drop(store);
        let (swarm, disk, search) = (Swarm::new(0), FakeDisk::new(ROOMY), search_port::Fake::new(fake_catalog()));
        let running = Self::boot(&dir, &swarm, &disk, &search);
        Self { dir, running, swarm, disk, search }
    }

    fn boot(dir: &tempfile::TempDir, swarm: &Swarm, disk: &FakeDisk, search: &search_port::Fake) -> Running {
        let ports = Ports {
            engine: Box::new(Fake::new(swarm.clone())),
            parse: parse_fake,
            search: Arc::new(search.clone()),
            disk: Box::new(disk.clone()),
        };
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        start(&tokio::runtime::Handle::current(), store, ports, config(dir.path()), "0.1.0".into()).unwrap()
    }

    /// Kills the engine without letting it save anything, then starts a new one on the same state.
    async fn crash_and_restart(self) -> Self {
        let Self { dir, running, swarm, disk, search } = self;
        running.owner.abort();
        let _ = running.owner.await;
        drop(running.ctx);
        let running = Self::boot(&dir, &swarm, &disk, &search);
        Self { dir, running, swarm, disk, search }
    }

    async fn ask(&self, request: Request) -> Response {
        server::handle(&self.running.ctx, request).await
    }

    async fn get(&self, target: Target, files: &[&str]) -> Result<GetReply, Error> {
        self.get_with(GetRequest {
            target,
            dir: Some(self.dir.path().join("downloads")),
            files: files.iter().map(|f| f.to_string()).collect(),
            max_size: None,
            dry_run: false,
        })
        .await
    }

    async fn get_with(&self, request: GetRequest) -> Result<GetReply, Error> {
        match self.ask(Request::Get(request)).await {
            Response::Get { reply } => Ok(*reply),
            Response::Error { error } => Err(error),
            other => panic!("unexpected {other:?}"),
        }
    }

    async fn changes(&self, request: Request) -> Result<Vec<(String, String)>, Error> {
        match self.ask(request).await {
            Response::Changed { changes } => {
                Ok(changes.into_iter().map(|c: Change| (c.id[..6].to_string(), c.result)).collect())
            }
            Response::Error { error } => Err(error),
            other => panic!("unexpected {other:?}"),
        }
    }

    async fn status(&self) -> Vec<Job> {
        match self.ask(Request::Status { refs: Vec::new() }).await {
            Response::Jobs { jobs } => jobs,
            other => panic!("unexpected {other:?}"),
        }
    }

    async fn states(&self) -> Vec<&'static str> {
        self.status().await.iter().map(|j| j.status.name()).collect()
    }

    async fn until(&self, what: &str, done: impl Fn(&[Job]) -> bool) -> Vec<Job> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let jobs = self.status().await;
            if done(&jobs) {
                return jobs;
            }
            assert!(Instant::now() < deadline, "timed out waiting until {what}: {jobs:#?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn files(&self, prefix: &str) -> FilesReply {
        match self.ask(Request::Files { target: Target::Prefix(prefix.into()), timeout_ms: 1_000 }).await {
            Response::Files { files } => files,
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Each file's verified bytes, as `files` reports them.
    async fn file_bytes(&self, t: &FakeTorrentFile) -> Vec<u64> {
        self.files(&prefix(t)).await.files.iter().map(|f| f.verified).collect()
    }

    fn torrent_file(&self, torrent: &FakeTorrentFile) -> Target {
        let path = self.dir.path().join(format!("{}.torrent", torrent.name));
        std::fs::write(&path, torrent.bytes()).unwrap();
        Target::TorrentFile(path)
    }

    fn downloads(&self) -> PathBuf {
        self.dir.path().join("downloads")
    }
}

fn torrent(n: u8, name: &str, files: &[(&str, u64)]) -> FakeTorrentFile {
    FakeTorrentFile {
        info_hash: InfoHash::V1([n; 20]),
        name: name.into(),
        files: files.iter().map(|(p, s)| (p.to_string(), *s)).collect(),
    }
}

fn magnet(torrent: &FakeTorrentFile) -> Target {
    Target::Magnet(format!("magnet:?xt=urn:btih:{}&dn={}", torrent.info_hash.id().to_hex(), torrent.name))
}

fn prefix(t: &FakeTorrentFile) -> String {
    t.info_hash.id().to_hex()[..6].to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_after_the_intent_commit_leaves_one_interrupted_download() {
    let h = Harness::new();
    let film = torrent(1, "film", &[("film.mkv", GIB)]);
    let reply = h.get(magnet(&film), &[]).await.unwrap();
    assert_eq!(reply.outcome, GetOutcome::Started);
    let h = h.crash_and_restart().await;
    let jobs = h.status().await;
    assert_eq!(jobs.len(), 1);
    assert_eq!((jobs[0].id, jobs[0].status.name()), (film.info_hash.id(), "interrupted"));
    assert_eq!(
        h.changes(Request::Resume { refs: vec![prefix(&film)], all: false }).await.unwrap(),
        [("010101".into(), "resumed".into())]
    );
    assert_eq!(h.states().await, ["metadata"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_get_brings_back_an_interrupted_download() {
    let h = Harness::new();
    let film = torrent(2, "film", &[("film.mkv", GIB)]);
    h.get(magnet(&film), &[]).await.unwrap();
    let h = h.crash_and_restart().await;
    assert_eq!(h.states().await, ["interrupted"]);
    assert_eq!(h.get(magnet(&film), &[]).await.unwrap().outcome, GetOutcome::Existing);
    assert_eq!(h.states().await, ["metadata"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_download_is_done_and_stays_done_after_a_restart() {
    let h = Harness::new();
    h.swarm.set_rate(100 * GIB);
    let iso = torrent(2, "arch.iso", &[("arch.iso", GIB)]);
    h.get(h.torrent_file(&iso), &[]).await.unwrap();
    let jobs = h.until("done", |jobs| jobs[0].status.name() == "done").await;
    assert_eq!(jobs[0].progress.verified, GIB);
    assert_eq!(jobs[0].path, Some(h.downloads().join("arch.iso")));
    let h = h.crash_and_restart().await;
    assert_eq!(h.states().await, ["done"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn get_is_idempotent_and_reports_a_changed_selection() {
    let h = Harness::new();
    let film = torrent(3, "film", &[("film.mkv", GIB), ("film.srt", 50_000), ("extras.mkv", GIB)]);
    h.swarm.publish(film.clone());
    assert_eq!(h.get(magnet(&film), &[]).await.unwrap().outcome, GetOutcome::Started);
    h.until("metadata arrived", |jobs| jobs[0].progress.size.is_some()).await;
    let again = h.get(magnet(&film), &[]).await.unwrap();
    assert_eq!(again.outcome, GetOutcome::Existing);
    let updated = h.get(Target::Prefix(prefix(&film)), &["film.*"]).await.unwrap();
    assert_eq!(updated.outcome, GetOutcome::Updated);
    assert_eq!(updated.previous_selection, Some(Selection::All));
    assert_eq!(updated.job.selection, Selection::Patterns(vec!["film.*".into()]));
    assert_eq!(updated.job.progress.size, Some(GIB + 50_000));
    assert_eq!(h.status().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_and_rm_are_idempotent_and_rm_keeps_the_path() {
    let h = Harness::new();
    let iso = torrent(4, "arch.iso", &[("arch.iso", GIB)]);
    h.get(h.torrent_file(&iso), &[]).await.unwrap();
    let id = prefix(&iso);
    let pause = || Request::Pause { refs: vec![id.clone()] };
    assert_eq!(h.changes(pause()).await.unwrap(), [(id.clone(), "paused".into())]);
    assert_eq!(h.changes(pause()).await.unwrap(), [(id.clone(), "already_paused".into())]);
    assert_eq!(h.states().await, ["paused"]);
    let rm = || Request::Remove { refs: vec![id.clone()] };
    let Response::Changed { changes } = h.ask(rm()).await else { panic!("expected changes") };
    assert_eq!(
        (changes[0].result.as_str(), changes[0].path.clone()),
        ("removed", Some(h.downloads().join("arch.iso")))
    );
    let Response::Changed { changes } = h.ask(rm()).await else { panic!("expected changes") };
    assert_eq!(
        (changes[0].result.as_str(), changes[0].path.clone()),
        ("already_removed", Some(h.downloads().join("arch.iso")))
    );
    assert!(h.status().await.is_empty());
    let unknown = h.changes(Request::Remove { refs: vec!["abcdef".into()] }).await.unwrap_err();
    assert_eq!(unknown.code, ErrorCode::NotFound);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ambiguous_prefix_lists_the_candidates() {
    let mut catalog = fake_catalog();
    let mut twin = catalog[1].clone();
    twin.id = torrential_core::ReleaseId::Torrent("e3a1c4aaaa000000000000000000000000000000".parse().unwrap());
    twin.name = "Ubuntu 24.04.5 desktop (repack)".into();
    catalog.push(twin);
    let h = Harness::with_catalog(catalog);
    let Response::Search { result } =
        h.ask(Request::Search { query: torrential_core::SearchQuery::new("ubuntu"), stream: false }).await
    else {
        panic!("expected results")
    };
    assert_eq!(result.releases.len(), 2);
    let err = h.get(Target::Prefix("e3a1".into()), &[]).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::AmbiguousRef);
    let Some(torrential_core::error::Details::Candidates(candidates)) = err.details else {
        panic!("expected candidates")
    };
    let ids: Vec<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["e3a1f0", "e3a1c4"]);
    assert_eq!(h.get(Target::Prefix("e3a1f0".into()), &[]).await.unwrap().outcome, GetOutcome::Started);
}

#[tokio::test(flavor = "multi_thread")]
async fn disk_size_and_existing_files_refuse_a_download() {
    let h = Harness::new();
    let big = torrent(5, "big", &[("a.bin", 40 * GIB), ("b.bin", 20 * GIB)]);
    let err = h.get(h.torrent_file(&big), &[]).await.unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        (ErrorCode::TooLarge, "the selection is 60.0 GiB, over the 50 GiB cap")
    );
    let raised = GetRequest { max_size: Some(60 * GIB), ..request(&h, h.torrent_file(&big)) };
    h.disk.set_free(69 * GIB);
    let err = h.get_with(raised.clone()).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::DiskFull);
    assert_eq!(err.details, Some(torrential_core::error::Details::Space { required: 70 * GIB, available: 69 * GIB }));
    h.disk.set_free(500 * GIB);
    h.disk.create(h.downloads().join("big/b.bin"));
    let err = h.get_with(raised.clone()).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::PathConflict);
    assert_eq!(
        err.details,
        Some(torrential_core::error::Details::Paths(vec![h.downloads().join("big/b.bin").display().to_string()]))
    );
    assert!(h.status().await.is_empty());
    let only_a = GetRequest { files: vec!["a.bin".into()], ..raised };
    assert_eq!(h.get_with(only_a).await.unwrap().outcome, GetOutcome::Started);
}

fn request(h: &Harness, target: Target) -> GetRequest {
    GetRequest { target, dir: Some(h.downloads()), files: Vec::new(), max_size: None, dry_run: false }
}

#[tokio::test(flavor = "multi_thread")]
async fn other_downloads_on_the_same_disk_count_against_free_space() {
    let h = Harness::new();
    h.disk.set_free(90 * GIB);
    let first = torrent(6, "first", &[("x", 45 * GIB)]);
    let second = torrent(7, "second", &[("y", 45 * GIB)]);
    h.get(h.torrent_file(&first), &[]).await.unwrap();
    let err = h.get(h.torrent_file(&second), &[]).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::DiskFull);
    assert!(err.message.contains("plus 45.0 GiB for other downloads"), "{}", err.message);
}

#[tokio::test(flavor = "multi_thread")]
async fn three_downloads_run_at_once_and_the_rest_queue() {
    let h = Harness::new();
    let torrents: Vec<FakeTorrentFile> = (10..14).map(|n| torrent(n, &format!("t{n}"), &[("f", GIB)])).collect();
    let mut outcomes = Vec::new();
    for t in &torrents {
        outcomes.push(h.get(h.torrent_file(t), &[]).await.unwrap().outcome);
    }
    assert_eq!(outcomes, [GetOutcome::Started, GetOutcome::Started, GetOutcome::Started, GetOutcome::Queued]);
    h.until("three downloading", |jobs| jobs.iter().filter(|j| j.status.name() == "downloading").count() == 3).await;
    assert_eq!(h.states().await[3], "queued");
    h.changes(Request::Pause { refs: vec![prefix(&torrents[0])] }).await.unwrap();
    let jobs = h.until("the queued one started", |jobs| jobs[3].status.name() == "downloading").await;
    assert_eq!(
        jobs.iter().map(|j| j.status.name()).collect::<Vec<_>>(),
        ["paused", "downloading", "downloading", "downloading"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn two_concurrent_gets_of_one_torrent_make_one_download() {
    let h = Harness::new();
    let film = torrent(20, "film", &[("film.mkv", GIB)]);
    let (a, b) = tokio::join!(h.get(magnet(&film), &[]), h.get(magnet(&film), &[]));
    let mut outcomes = [a.unwrap().outcome, b.unwrap().outcome];
    outcomes.sort_by_key(|o| format!("{o:?}"));
    assert_eq!(outcomes, [GetOutcome::Existing, GetOutcome::Started]);
    assert_eq!(h.status().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn both_hashes_of_a_hybrid_torrent_name_one_download() {
    let h = Harness::new();
    let v2 = [0x77; 32];
    let hybrid = FakeTorrentFile {
        info_hash: InfoHash::Hybrid { v1: [0x21; 20], v2 },
        name: "hybrid".into(),
        files: vec![("hybrid".into(), GIB)],
    };
    h.swarm.publish(hybrid.clone());
    h.get(magnet(&hybrid), &[]).await.unwrap();
    h.until("metadata arrived", |jobs| jobs[0].progress.size.is_some()).await;
    let by_v2 = Target::Magnet(format!("magnet:?xt=urn:btmh:1220{}", "77".repeat(32)));
    assert_eq!(h.get(by_v2, &[]).await.unwrap().outcome, GetOutcome::Existing);
    assert_eq!(h.status().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_downloads_that_turn_out_to_be_one_hybrid_torrent_converge() {
    let h = Harness::new();
    let hybrid = FakeTorrentFile {
        info_hash: InfoHash::Hybrid { v1: [0x31; 20], v2: [0x32; 32] },
        name: "hybrid".into(),
        files: vec![("hybrid".into(), GIB)],
    };
    let by_v2 = Target::Magnet(format!("magnet:?xt=urn:btmh:1220{}", "32".repeat(32)));
    let original = h.get(by_v2, &[]).await.unwrap().job.id;
    h.get(Target::Magnet(format!("magnet:?xt=urn:btih:{}", "31".repeat(20))), &[]).await.unwrap();
    assert_eq!(h.status().await.len(), 2);
    h.swarm.publish(hybrid);
    let jobs = h.until("one job is left", |jobs| jobs.len() == 1).await;
    assert_eq!(jobs[0].id, original);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pattern_matching_no_file_stops_the_download_when_metadata_arrives() {
    let h = Harness::new();
    let film = torrent(22, "film", &[("film.mp4", GIB)]);
    h.swarm.publish(film.clone());
    h.get(magnet(&film), &["*.mkv"]).await.unwrap();
    let jobs = h.until("stopped", |jobs| jobs[0].status.name() == "error").await;
    let JobStatus::Error { error } = &jobs[0].status else { unreachable!() };
    assert_eq!((error.code, error.message.as_str()), (ErrorCode::NoSelection, "no file in film matches \"*.mkv\""));
}

#[tokio::test(flavor = "multi_thread")]
async fn files_fetch_metadata_from_peers_and_get_reuses_it() {
    let h = Harness::new();
    let film = torrent(23, "film", &[("film.mkv", GIB), ("setup.exe", 1000)]);
    h.swarm.publish(film.clone());
    let Response::Files { files } = h.ask(Request::Files { target: magnet(&film), timeout_ms: 5_000 }).await else {
        panic!("expected files")
    };
    assert_eq!(files.metadata_source, "peers");
    let paths: Vec<(&str, bool)> = files.files.iter().map(|f| (f.path.as_str(), f.risk.is_some())).collect();
    assert_eq!(paths, [("film.mkv", false), ("setup.exe", true)]);
    let dry = GetRequest { files: vec!["*.mkv".into()], dry_run: true, ..request(&h, magnet(&film)) };
    let reply = h.get_with(dry).await.unwrap();
    assert_eq!(reply.outcome, GetOutcome::Planned);
    assert_eq!(reply.plan.map(|p| (p.selected_files, p.total_files, p.selected_size)), Some((1, 2, GIB)));
    assert!(h.status().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn files_without_peers_time_out() {
    let h = Harness::new();
    let lonely = torrent(24, "lonely", &[("x", 1)]);
    let response = h.ask(Request::Files { target: magnet(&lonely), timeout_ms: 200 }).await;
    let Response::Error { error } = response else { panic!("expected an error, got {response:?}") };
    assert_eq!(error.code, ErrorCode::MetadataTimeout);
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_reports_damage_and_repair_downloads_it_again() {
    let h = Harness::new();
    h.swarm.set_rate(100 * GIB);
    let iso = torrent(25, "arch.iso", &[("arch.iso", GIB)]);
    h.get(h.torrent_file(&iso), &[]).await.unwrap();
    h.until("done", |jobs| jobs[0].status.name() == "done").await;
    h.swarm.damage(iso.info_hash.id(), 3);
    let verify = |repair| Request::Verify { reference: prefix(&iso), repair };
    assert_eq!(h.changes(verify(false)).await.unwrap(), [(prefix(&iso), "checking".into())]);
    let jobs = h.until("damage reported", |jobs| jobs[0].status.name() == "error").await;
    let JobStatus::Error { error } = &jobs[0].status else { unreachable!() };
    assert_eq!(
        (error.code, error.message.as_str()),
        (ErrorCode::VerificationFailed, "3 damaged pieces in the selected files")
    );
    h.swarm.damage(iso.info_hash.id(), 3);
    h.changes(verify(true)).await.unwrap();
    h.until("repaired", |jobs| jobs[0].status.name() == "done").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn repair_counts_the_damaged_pieces_as_missing_until_they_download_again() {
    let h = Harness::new();
    h.swarm.set_rate(100 * GIB);
    let iso = torrent(28, "arch.iso", &[("arch.iso", GIB)]);
    h.get(h.torrent_file(&iso), &[]).await.unwrap();
    h.until("done", |jobs| jobs[0].status.name() == "done").await;
    h.swarm.set_rate(0);
    h.swarm.damage(iso.info_hash.id(), 3);
    h.changes(Request::Verify { reference: prefix(&iso), repair: true }).await.unwrap();
    let jobs = h.until("repairing", |jobs| jobs[0].status.name() == "downloading").await;
    assert_eq!(jobs[0].progress.verified, GIB - 3 * GIB_PIECE);
    h.swarm.set_rate(100 * GIB);
    let jobs = h.until("repaired", |jobs| jobs[0].status.name() == "done").await;
    assert_eq!(jobs[0].progress.verified, GIB);
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_after_a_failed_verify_downloads_the_damaged_pieces_again() {
    let h = Harness::new();
    h.swarm.set_rate(100 * GIB);
    let iso = torrent(29, "arch.iso", &[("arch.iso", GIB)]);
    h.get(h.torrent_file(&iso), &[]).await.unwrap();
    h.until("done", |jobs| jobs[0].status.name() == "done").await;
    h.swarm.set_rate(0);
    h.swarm.damage(iso.info_hash.id(), 2);
    h.changes(Request::Verify { reference: prefix(&iso), repair: false }).await.unwrap();
    let jobs = h.until("damage reported", |jobs| jobs[0].status.name() == "error").await;
    assert_eq!(jobs[0].progress.verified, GIB - 2 * GIB_PIECE);
    h.changes(Request::Resume { refs: vec![prefix(&iso)], all: false }).await.unwrap();
    let jobs = h.until("downloading", |jobs| jobs[0].status.name() == "downloading").await;
    assert_eq!(jobs[0].progress.verified, GIB - 2 * GIB_PIECE);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_keeps_the_progress_of_the_last_periodic_save() {
    let h = Harness::new();
    h.swarm.set_rate(GIB / 2);
    let iso = torrent(30, "arch.iso", &[("arch.iso", GIB)]);
    h.get(h.torrent_file(&iso), &[]).await.unwrap();
    h.until("a quarter is verified", |jobs| jobs[0].progress.verified >= GIB / 4).await;
    h.swarm.set_rate(0);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = h.status().await[0].progress.verified;
    let h = h.crash_and_restart().await;
    let jobs = h.status().await;
    assert_eq!((jobs[0].status.name(), jobs[0].progress.verified), ("interrupted", before));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_keeps_the_piece_map_of_the_last_save_and_done_drops_it() {
    let h = Harness::new();
    h.swarm.set_rate(GIB / 2);
    let set = torrent(31, "set", &[("a.bin", GIB), ("b.bin", GIB)]);
    h.get(h.torrent_file(&set), &["a.bin"]).await.unwrap();
    h.until("a quarter is verified", |jobs| jobs[0].progress.verified >= GIB / 4).await;
    h.swarm.set_rate(0);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let map = h.status().await[0].progress.piece_map.expect("a running download has a piece map").to_string();
    assert_eq!(map[32..], "-".repeat(32), "b.bin is not selected");
    let h = h.crash_and_restart().await;
    let jobs = h.status().await;
    assert_eq!((jobs[0].status.name(), jobs[0].progress.piece_map.map(|m| m.to_string())), ("interrupted", Some(map)));
    h.swarm.set_rate(100 * GIB);
    h.changes(Request::Resume { refs: vec![prefix(&set)], all: false }).await.unwrap();
    let jobs = h.until("done", |jobs| jobs[0].status.name() == "done").await;
    assert_eq!(jobs[0].progress.piece_map, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_downloads_files_agree_with_its_piece_map() {
    let h = Harness::new();
    h.swarm.set_rate(GIB / 2);
    let set = torrent(32, "set", &[("a.bin", GIB), ("b.bin", GIB / 3), ("c.bin", GIB / 5)]);
    h.get(h.torrent_file(&set), &["a.bin", "c.bin"]).await.unwrap();
    h.until("a third is verified", |jobs| jobs[0].progress.verified >= GIB / 3).await;
    h.swarm.set_rate(0);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let progress = h.status().await[0].progress;
    let files = h.file_bytes(&set).await;
    let map = progress.piece_map.expect("a running download has a piece map");
    let wanted = [true, false, true];
    assert_eq!(disagreement(&set.metainfo().unwrap(), &wanted, progress.verified, &files, &map), None, "{map}");
    assert!(files[2] > 0, "c.bin, last in the torrent, gets pieces too: {files:?}");
}

/// Fails unless the file bytes `files` reports sum to the download's verified bytes and agree with its map.
fn assert_files_agree(job: &Job, reply: &FilesReply) {
    let list = reply.files.iter().map(|f| MetaFile { path: f.path.clone(), size: f.size }).collect();
    let metainfo = Metainfo::new(job.info_hash, reply.name.clone().unwrap(), list, false, Vec::new()).unwrap();
    let wanted: Vec<bool> = reply.files.iter().map(|f| f.selected).collect();
    let bytes: Vec<u64> = reply.files.iter().map(|f| f.verified).collect();
    let map = job.progress.piece_map.expect("an unfinished download has a piece map");
    let wrong = disagreement(&metainfo, &wanted, job.progress.verified, &bytes, &map);
    assert_eq!(wrong, None, "{:?} {} {map}", job.name, job.status.name());
}

#[tokio::test(flavor = "multi_thread")]
async fn each_unfinished_test_download_reports_file_bytes_that_match_its_progress() {
    let h = Harness::seeded();
    let mut checked = Vec::new();
    for job in h.status().await.iter().filter(|j| !matches!(j.status, JobStatus::Done { .. })) {
        assert_files_agree(job, &h.files(&job.id.to_hex()[..6]).await);
        checked.push((job.name.clone().unwrap(), job.status.name()));
    }
    let expected =
        [("stackexchange", "paused"), ("openwebtext-2019", "error"), ("wikipedia-en-2026-09-text", "interrupted")];
    assert_eq!(checked, expected.map(|(name, state)| (name.to_string(), state)));
    h.changes(Request::Resume { refs: vec!["6f2c81".into()], all: false }).await.unwrap();
    let jobs =
        h.until("wikipedia is in the engine", |jobs| jobs.iter().any(|j| j.status.name() == "downloading")).await;
    let running = jobs.iter().find(|j| j.status.name() == "downloading").unwrap();
    assert_files_agree(running, &h.files("6f2c81").await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paused_download_keeps_its_file_bytes_across_a_restart() {
    let h = Harness::new();
    h.swarm.set_rate(GIB / 2);
    let set = torrent(33, "set", &[("a.bin", GIB), ("b.bin", GIB / 3), ("c.bin", GIB / 5)]);
    h.get(h.torrent_file(&set), &["a.bin", "c.bin"]).await.unwrap();
    h.until("a third is verified", |jobs| jobs[0].progress.verified >= GIB / 3).await;
    h.changes(Request::Pause { refs: vec![prefix(&set)] }).await.unwrap();
    let paused = h.file_bytes(&set).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let h = h.crash_and_restart().await;
    let job = h.status().await.remove(0);
    assert_eq!((job.status.name(), paused[0] + paused[2]), ("paused", job.progress.verified));
    assert_eq!(h.file_bytes(&set).await, paused, "the restarted engine reports the bytes it saved at the pause");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_whose_missing_pieces_have_no_source_stops_and_resume_looks_again() {
    let h = Harness::new();
    h.swarm.set_rate(100 * GIB);
    let film = torrent(34, "film", &[("film.mp4", GIB), ("film.ogv", GIB)]);
    h.swarm.withhold(film.info_hash.id(), 2);
    h.get(h.torrent_file(&film), &["film.mp4"]).await.unwrap();
    let jobs = h.until("stopped", |jobs| jobs[0].status.name() == "error").await;
    let JobStatus::Error { error } = &jobs[0].status else { unreachable!() };
    let piece = 2 * GIB_PIECE;
    assert_eq!(jobs[0].progress.verified, GIB - 2 * piece);
    assert_eq!(
        (error.code, error.message.as_str(), error.hint.as_deref()),
        (
            ErrorCode::NoSource,
            "no peer or web seed has had the last 2 pieces (4.0 MiB) for 0s",
            Some("torrential resume 222222 looks for them again; the 1020.0 MiB verified stays")
        )
    );
    h.swarm.withhold(film.info_hash.id(), 0);
    h.changes(Request::Resume { refs: vec![prefix(&film)], all: false }).await.unwrap();
    h.until("done", |jobs| jobs[0].status.name() == "done").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_that_gets_nothing_from_peers_that_have_its_pieces_keeps_waiting() {
    let h = Harness::new();
    let film = torrent(35, "film", &[("film.mp4", GIB)]);
    h.get(h.torrent_file(&film), &[]).await.unwrap();
    h.until("downloading", |jobs| jobs[0].status.name() == "downloading").await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let jobs = h.status().await;
    assert_eq!((jobs[0].status.name(), jobs[0].progress.verified), ("downloading", 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_result_with_only_a_torrent_url_is_fetched_once_found() {
    let iso = torrent(27, "debian.iso", &[("debian.iso", GIB)]);
    let url = "https://cdimage.example/debian.iso.torrent";
    let row = torrential_core::Release {
        id: torrential_core::ReleaseId::Locator(torrential_core::LocatorId::from_bytes([0xab; 20])),
        name: "debian.iso".into(),
        magnet: None,
        torrent_url: Some(url.into()),
        ..fake_catalog().remove(0)
    };
    let h = Harness::with_catalog(vec![row]);
    h.search.serve_torrent(url, iso.bytes());
    let Response::Search { result } =
        h.ask(Request::Search { query: torrential_core::SearchQuery::new("debian"), stream: false }).await
    else {
        panic!("expected results")
    };
    assert_eq!(result.releases[0].id.to_ref(), format!("u{}", "ab".repeat(20)));
    let reply = h.get(Target::Prefix("uabab".into()), &[]).await.unwrap();
    assert_eq!((reply.job.id, reply.plan.map(|p| p.selected_size)), (iso.info_hash.id(), Some(GIB)));
    let Response::Files { files } =
        h.ask(Request::Files { target: Target::Prefix(prefix(&iso)), timeout_ms: 1_000 }).await
    else {
        panic!("expected files")
    };
    assert_eq!(files.metadata_source, "download");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_search_does_not_delay_a_pause() {
    let h = Harness::new();
    let iso = torrent(26, "arch.iso", &[("arch.iso", GIB)]);
    h.get(h.torrent_file(&iso), &[]).await.unwrap();
    h.search.set_delay(Duration::from_secs(3));
    let ctx = h.running.ctx.clone();
    let search = tokio::spawn(async move {
        server::handle(&ctx, Request::Search { query: torrential_core::SearchQuery::new("arch"), stream: false }).await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let started = Instant::now();
    h.changes(Request::Pause { refs: vec![prefix(&iso)] }).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(1), "pause took {:?}", started.elapsed());
    assert!(!search.is_finished());
    search.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_streaming_search_sends_results_as_sources_answer() {
    use search_port::FakeSource;
    let listed = |hex: &str, name: &str, source: &str| torrential_core::Release {
        id: torrential_core::ReleaseId::Torrent(hex.repeat(20).parse().unwrap()),
        name: name.into(),
        sources: vec![source.into()],
        ..fake_catalog().remove(0)
    };
    let catalog = vec![listed("aa", "dead \u{1b}[31mred", "slow"), listed("bb", "dead fast", "quick")];
    let script = vec![
        FakeSource::answers("quick", 0),
        FakeSource::fails("broken", 20, torrential_core::SourceOutcome::Http { status: 503 }),
        FakeSource::answers("slow", 60),
    ];
    let h = Harness::with_search(search_port::Fake::scripted(catalog, script, Default::default()));
    let (client, served) = tokio::net::UnixStream::pair().unwrap();
    tokio::spawn(server::connection(h.running.ctx.clone(), served));
    let (mut rd, mut wr) = client.into_split();
    let hello = Request::Hello { client_version: "0.1.0".into(), wire: torrential_core::protocol::WIRE_VERSION };
    crate::wire::write_async(&mut wr, &hello).await.unwrap();
    crate::wire::read_async::<Response>(&mut rd).await.unwrap();
    let search = Request::Search { query: torrential_core::SearchQuery::new("dead"), stream: true };
    crate::wire::write_async(&mut wr, &search).await.unwrap();
    let mut frames = Vec::new();
    loop {
        let frame: serde_json::Value = crate::wire::read_async(&mut rd).await.unwrap().unwrap();
        let names: Vec<&str> =
            frame["result"]["releases"].as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap()).collect();
        let sources: Vec<String> = frame["result"]["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| format!("{} {}", s["source"].as_str().unwrap(), s["outcome"].as_str().unwrap()))
            .collect();
        let kind = frame["event"].as_str().or(frame["type"].as_str()).unwrap().to_string();
        frames.push(format!("{kind}: {names:?} {sources:?} pending {}", frame["pending"]));
        if kind != "search_update" {
            break;
        }
    }
    assert_eq!(
        frames,
        [
            r#"search_update: ["dead fast"] ["quick ok"] pending ["broken","slow"]"#,
            r#"search_update: ["dead fast"] ["quick ok", "broken http"] pending ["slow"]"#,
            r#"search: ["dead red", "dead fast"] ["quick ok", "broken http", "slow ok"] pending null"#,
        ]
    );
    let reply = h.get(Target::Prefix("bbbbbb".into()), &[]).await.unwrap();
    assert_eq!(reply.outcome, GetOutcome::Started);
}

#[tokio::test(flavor = "multi_thread")]
async fn watching_every_download_includes_ones_started_later() {
    let h = Harness::new();
    let before = torrent(30, "before.iso", &[("before.iso", GIB)]);
    h.get(h.torrent_file(&before), &[]).await.unwrap();
    let (client, served) = tokio::net::UnixStream::pair().unwrap();
    tokio::spawn(server::connection(h.running.ctx.clone(), served));
    let (mut rd, mut wr) = client.into_split();
    let hello = Request::Hello { client_version: "0.1.0".into(), wire: torrential_core::protocol::WIRE_VERSION };
    crate::wire::write_async(&mut wr, &hello).await.unwrap();
    crate::wire::read_async::<Response>(&mut rd).await.unwrap();
    crate::wire::write_async(&mut wr, &Request::Watch { refs: Vec::new() }).await.unwrap();
    let first: torrential_core::protocol::Event = crate::wire::read_async(&mut rd).await.unwrap().unwrap();
    assert!(matches!(first, torrential_core::protocol::Event::Snapshot { ref jobs } if jobs.len() == 1));
    let later = torrent(31, "later.iso", &[("later.iso", GIB)]);
    h.get(h.torrent_file(&later), &[]).await.unwrap();
    let seen = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match crate::wire::read_async(&mut rd).await.unwrap().unwrap() {
                torrential_core::protocol::Event::Job { job } if job.id == later.info_hash.id() => return job.name,
                _ => continue,
            }
        }
    })
    .await
    .expect("an event for the later download");
    assert_eq!(seen.as_deref(), Some("later.iso"));
}
