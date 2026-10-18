//! `Search` driven the way the engine drives it, against a local HTTP server that
//! serves recorded responses.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use torrential_core::error::Details;
use torrential_core::{ErrorCode, Flag, HexPrefix, SearchQuery, SourceOutcome};
use torrential_search::{ResultCache, Search, SearchConfig};

#[derive(Clone)]
struct Reply {
    status: u16,
    body: Vec<u8>,
    delay: Duration,
    /// Without a Content-Length the client learns the size only while reading.
    content_length: bool,
}

fn ok(body: Vec<u8>) -> Reply {
    Reply { status: 200, body, delay: Duration::ZERO, content_length: true }
}

fn status(code: u16) -> Reply {
    Reply { status: code, body: b"no".to_vec(), delay: Duration::ZERO, content_length: true }
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
}

/// Answers each path prefix with its replies in order, repeating the last one.
struct Server {
    base: String,
    hits: Arc<Mutex<HashMap<&'static str, usize>>>,
}

impl Server {
    async fn start(routes: Vec<(&'static str, Vec<Reply>)>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits: Arc<Mutex<HashMap<&'static str, usize>>> = Arc::default();
        let (routes, counter) = (Arc::new(routes), hits.clone());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let (routes, counter) = (routes.clone(), counter.clone());
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    let path = String::from_utf8_lossy(&head).split_whitespace().nth(1).unwrap_or("/").to_string();
                    let reply = match routes.iter().find(|(prefix, _)| path.starts_with(prefix)) {
                        Some((prefix, replies)) => {
                            let mut counter = counter.lock().unwrap();
                            let n = counter.entry(*prefix).or_insert(0);
                            *n += 1;
                            replies[(*n - 1).min(replies.len() - 1)].clone()
                        }
                        None => status(404),
                    };
                    tokio::time::sleep(reply.delay).await;
                    let mut response = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", reply.status);
                    if reply.content_length {
                        response.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
                    }
                    response.push_str("\r\n");
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.write_all(&reply.body).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Server { base, hits }
    }

    fn hits(&self, prefix: &str) -> usize {
        self.hits.lock().unwrap().get(prefix).copied().unwrap_or(0)
    }

    /// Points each named source at `/<name>/` on this server.
    fn config(&self, sources: &[&str]) -> SearchConfig {
        let endpoints: BTreeMap<String, String> =
            sources.iter().map(|s| (s.to_string(), format!("{}/{s}/", self.base))).collect();
        SearchConfig { endpoints, ..SearchConfig::default() }
    }
}

fn query(text: &str, sources: &[&str]) -> SearchQuery {
    SearchQuery { sources: sources.iter().map(|s| s.to_string()).collect(), timeout_ms: 3000, ..SearchQuery::new(text) }
}

fn outcomes(result: &torrential_core::SearchResult) -> Vec<(String, SourceOutcome)> {
    result.sources.iter().map(|r| (r.source.clone(), r.outcome.clone())).collect()
}

#[tokio::test]
async fn merges_two_indexes_into_one_ranked_list() {
    let server = Server::start(vec![
        ("/knaben/v2/search", vec![ok(fixture("knaben/v2-search-ubuntu.json"))]),
        ("/apibay/q.php", vec![ok(fixture("apibay/q.php-ubuntu.json"))]),
    ])
    .await;
    let search = Search::new(&server.config(&["knaben", "apibay"]), ResultCache::new()).unwrap();
    let mut q = query("ubuntu", &["knaben", "apibay"]);
    q.limit = 200;

    let result = search.search(&q).await.unwrap();

    assert_eq!(
        outcomes(&result),
        [("knaben".into(), SourceOutcome::Ok { results: 98 }), ("apibay".into(), SourceOutcome::Ok { results: 100 })]
    );
    // Knaben lists c426b415 twice, from two origin trackers.
    assert_eq!(result.total, 158);
    let lts = result.releases.iter().find(|r| r.name == "Ubuntu 22.04 LTS").unwrap();
    assert_eq!(lts.id.to_ref(), "2c6b6858d61da9543d4231a71db4b1c9264b0685");
    assert_eq!(lts.sources, ["knaben", "apibay"]);
    assert_eq!((lts.seeders, lts.leechers), (Some(37), Some(5)));
    assert_eq!(lts.size, Some(3_654_957_056));
    assert_eq!(lts.flags.iter().collect::<Vec<_>>(), [&Flag::Trusted]);
    let escaped = result.releases.iter().find(|r| r.id.to_ref().starts_with("c426b415")).unwrap();
    assert_eq!(escaped.name, "Linux Command Line (Hands-On) Ubuntu Linux & Linux Scripting");
    assert_eq!(escaped.size, Some(2_464_274_774));
    assert_eq!((escaped.seeders, escaped.leechers), (Some(13), Some(3)));
}

#[tokio::test]
async fn every_source_failing_is_providers_unavailable() {
    let refused = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
    let server = Server::start(vec![
        ("/knaben/", vec![status(503)]),
        ("/archive.org/", vec![ok(b"<html>Just a moment...</html>".to_vec())]),
    ])
    .await;
    let mut config = server.config(&["knaben", "archive.org"]);
    config.endpoints.insert("apibay".into(), format!("http://{refused}/"));
    let search = Search::new(&config, ResultCache::new()).unwrap();

    let err = search.search(&query("ubuntu", &["knaben", "apibay", "archive.org"])).await.unwrap_err();

    assert_eq!(err.code, ErrorCode::ProvidersUnavailable);
    assert_eq!(err.message, "no source answered (archive.org, knaben, apibay)");
    let Some(Details::Sources(reports)) = err.details else { panic!("no source reports") };
    let kinds: Vec<(&str, &str)> = reports
        .iter()
        .map(|r| {
            let kind = match &r.outcome {
                SourceOutcome::Http { status: 503 } => "http 503",
                SourceOutcome::Connect { .. } => "connect",
                SourceOutcome::BadResponse { .. } => "bad_response",
                _ => "other",
            };
            (r.source.as_str(), kind)
        })
        .collect();
    assert_eq!(kinds, [("archive.org", "bad_response"), ("knaben", "http 503"), ("apibay", "connect")]);
    assert_eq!(server.hits("/knaben/"), 2);
}

#[tokio::test]
async fn retries_once_after_a_5xx_but_not_after_a_4xx() {
    let server = Server::start(vec![
        ("/apibay/", vec![status(502), ok(fixture("apibay/q.php-ubuntu.json"))]),
        ("/knaben/", vec![status(404)]),
    ])
    .await;
    let search = Search::new(&server.config(&["knaben", "apibay"]), ResultCache::new()).unwrap();

    let result = search.search(&query("ubuntu", &["knaben", "apibay"])).await.unwrap();

    assert_eq!(
        outcomes(&result),
        [("knaben".into(), SourceOutcome::Http { status: 404 }), ("apibay".into(), SourceOutcome::Ok { results: 100 })]
    );
    assert_eq!((server.hits("/apibay/"), server.hits("/knaben/")), (2, 1));
}

#[tokio::test]
async fn an_oversized_answer_is_an_error_not_a_truncated_list() {
    let mut huge = fixture("apibay/q.php-ubuntu.json");
    huge.resize(5 << 20, b' ');
    let server = Server::start(vec![
        ("/apibay/", vec![Reply { content_length: false, ..ok(huge) }]),
        ("/knaben/", vec![ok(fixture("knaben/v2-search-ubuntu.json"))]),
    ])
    .await;
    let search = Search::new(&server.config(&["knaben", "apibay"]), ResultCache::new()).unwrap();

    let result = search.search(&query("ubuntu", &["knaben", "apibay"])).await.unwrap();

    assert_eq!(
        outcomes(&result)[1],
        ("apibay".into(), SourceOutcome::BadResponse { reason: "response larger than 4194304 bytes".into() })
    );
}

#[tokio::test]
async fn a_slow_source_times_out_by_the_deadline() {
    let slow = Reply { delay: Duration::from_secs(10), ..ok(fixture("knaben/v2-search-ubuntu.json")) };
    let server =
        Server::start(vec![("/knaben/", vec![slow]), ("/apibay/", vec![ok(fixture("apibay/q.php-ubuntu.json"))])])
            .await;
    let search = Search::new(&server.config(&["knaben", "apibay"]), ResultCache::new()).unwrap();
    let mut q = query("ubuntu", &["knaben", "apibay"]);
    q.timeout_ms = 1500;

    let started = Instant::now();
    let result = search.search(&q).await.unwrap();
    let wall = started.elapsed();

    assert_eq!(outcomes(&result)[0], ("knaben".into(), SourceOutcome::Timeout));
    assert_eq!(outcomes(&result)[1], ("apibay".into(), SourceOutcome::Ok { results: 100 }));
    assert!(wall < Duration::from_millis(1700), "returned after {wall:?}");
    assert!(result.elapsed_ms < 1700, "reported {} ms", result.elapsed_ms);
}

#[tokio::test]
async fn a_repeat_search_is_served_from_the_cache_unless_fresh() {
    let server = Server::start(vec![
        ("/apibay/", vec![ok(fixture("apibay/q.php-ubuntu.json"))]),
        ("/knaben/", vec![status(503)]),
    ])
    .await;
    let search = Search::new(&server.config(&["knaben", "apibay"]), ResultCache::new()).unwrap();
    let q = query("Ubuntu", &["knaben", "apibay"]);

    let first = search.search(&q).await.unwrap();
    let mut narrower = query("ubuntu", &["knaben", "apibay"]);
    narrower.min_seeds = Some(30);
    let second = search.search(&narrower).await.unwrap();
    let fresh = search.search(&SearchQuery { fresh: true, ..q.clone() }).await.unwrap();

    assert_eq!(outcomes(&first)[1], ("apibay".into(), SourceOutcome::Ok { results: 100 }));
    assert_eq!(outcomes(&second)[1], ("apibay".into(), SourceOutcome::Cached { results: 100 }));
    let seeded: Vec<(&str, Option<u32>)> = second.releases.iter().map(|r| (r.name.as_str(), r.seeders)).collect();
    assert_eq!(seeded, [("ubuntu-24.04.1-desktop-amd64.iso", Some(51)), ("Ubuntu 22.04 LTS", Some(37))]);
    assert_eq!(outcomes(&fresh)[1], ("apibay".into(), SourceOutcome::Ok { results: 100 }));
    // The failing source is asked again every time rather than cached.
    assert_eq!((server.hits("/apibay/"), server.hits("/knaben/")), (2, 6));
}

#[tokio::test]
async fn returned_releases_resolve_by_id_prefix() {
    let server = Server::start(vec![("/apibay/", vec![ok(fixture("apibay/q.php-ubuntu.json"))])]).await;
    let cache = ResultCache::new();
    let search = Search::new(&server.config(&["apibay"]), cache.clone()).unwrap();

    search.search(&query("ubuntu 22.04", &["apibay"])).await.unwrap();

    let prefix: HexPrefix = "2c6b68".parse().unwrap();
    let found: Vec<String> = cache.find(&prefix).into_iter().map(|r| r.name).collect();
    assert_eq!(found, ["Ubuntu 22.04 LTS"]);
    assert_eq!(cache.find(&"ffff00".parse().unwrap()), []);
}

#[tokio::test]
async fn official_torrents_get_their_hash_or_a_locator() {
    let listing = fixture("debian/debian-cd-current-arm64-bt-cd.html");
    let torrent = fixture("debian/debian-cd-current-arm64-bt-cd-debian-13.7.0-arm64-netinst.iso.torrent");
    let dir = "/debian/debian-cd/current/arm64/bt-cd/";
    let file = "/debian/debian-cd/current/arm64/bt-cd/debian-13.7.0-arm64-netinst.iso.torrent";
    let hashed = Server::start(vec![(file, vec![ok(torrent)]), (dir, vec![ok(listing.clone())])]).await;
    let broken = Server::start(vec![(file, vec![status(500)]), (dir, vec![ok(listing)])]).await;
    let q = query("debian 13 arm64 netinst", &["debian"]);

    let found = Search::new(&hashed.config(&["debian"]), ResultCache::new()).unwrap().search(&q).await.unwrap();
    let fallback = Search::new(&broken.config(&["debian"]), ResultCache::new()).unwrap().search(&q).await.unwrap();

    let release = &found.releases[0];
    assert_eq!(release.id.to_ref(), "bf7ed99351f7b08f0916b7b3e49cfa574d206879");
    assert_eq!(release.name, "debian-13.7.0-arm64-netinst.iso");
    assert_eq!(release.size, Some(735_358_976));
    assert_eq!(release.flags.iter().collect::<Vec<_>>(), [&Flag::Official]);
    assert_eq!(
        release.magnet.as_deref(),
        Some(
            "magnet:?xt=urn:btih:bf7ed99351f7b08f0916b7b3e49cfa574d206879&dn=debian-13.7.0-arm64-netinst.iso\
             &tr=http%3A%2F%2Fbttracker.debian.org%3A6969%2Fannounce"
        )
    );
    let locator = &fallback.releases[0];
    assert!(locator.id.to_ref().starts_with('u'));
    assert_eq!(locator.torrent_url.as_deref(), Some(format!("{}{file}", broken.base).as_str()));
    assert_eq!(locator.magnet, None);
}

#[tokio::test]
async fn the_academic_catalog_survives_a_restart() {
    let dir = scratch_dir("academic");
    let up =
        Server::start(vec![("/academictorrents/database.xml", vec![ok(fixture("academictorrents/database.xml"))])])
            .await;
    let down = Server::start(vec![("/academictorrents/", vec![status(503)])]).await;
    let q = query("stack exchange", &["academictorrents"]);
    let with_dir =
        |server: &Server| SearchConfig { cache_dir: Some(dir.clone()), ..server.config(&["academictorrents"]) };

    let first = Search::new(&with_dir(&up), ResultCache::new()).unwrap().search(&q).await.unwrap();
    let after_restart = Search::new(&with_dir(&down), ResultCache::new()).unwrap().search(&q).await.unwrap();

    let listed = |r: &torrential_core::SearchResult| -> Vec<(String, String)> {
        r.releases.iter().map(|r| (r.id.to_ref(), r.name.clone())).collect()
    };
    assert_eq!(
        listed(&first),
        [
            ("c32cdd0306ee7717cd63f98f8df6486af55f79c8".into(), "Stack Exchange Data Dump (2026-09-30)".into()),
            ("794176de24c33cc0e8b629b45a746cd870f3e95f".into(), "Stack Exchange Data Dump (2026-06-30)".into()),
        ]
    );
    assert_eq!(listed(&after_restart), listed(&first));
    assert_eq!(down.hits("/academictorrents/"), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn rejects_unknown_sources_and_queries_without_words() {
    let search = Search::new(&SearchConfig::default(), ResultCache::new()).unwrap();

    let unknown = search.search(&query("ubuntu", &["1337x"])).await.unwrap_err();
    let empty = search.search(&SearchQuery::new(" -- ")).await.unwrap_err();

    assert_eq!((unknown.code, unknown.message.as_str()), (ErrorCode::Usage, "unknown source \"1337x\""));
    assert_eq!(
        unknown.hint.as_deref(),
        Some("sources are archlinux, ubuntu, debian, archive.org, academictorrents, knaben, apibay, nyaa")
    );
    assert_eq!((empty.code, empty.message.as_str()), (ErrorCode::Usage, "a search needs at least one word"));
}

fn scratch_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("torrential-search-{name}-{}-{nanos}", std::process::id()))
}
