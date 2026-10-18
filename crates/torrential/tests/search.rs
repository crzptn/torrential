//! The real engine process with live search, against a local HTTP server that serves the
//! search crate's recorded responses. Nothing leaves the machine: every enabled source points
//! at the server, libtorrent stays on loopback, and the torrents it adds name no tracker.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use assert_cmd::Command;
use serde_json::Value;
use torrential_core::SearchQuery;
use torrential_core::protocol::{Request, WIRE_VERSION, encode_frame};

const BUILTIN: [&str; 8] =
    ["archlinux", "ubuntu", "debian", "archive.org", "academictorrents", "knaben", "apibay", "nyaa"];

#[derive(Clone)]
struct Reply {
    status: u16,
    body: Vec<u8>,
    delay: Duration,
}

fn ok(body: Vec<u8>) -> Reply {
    Reply { status: 200, body, delay: Duration::ZERO }
}

fn status(code: u16) -> Reply {
    Reply { status: code, body: b"no".to_vec(), delay: Duration::ZERO }
}

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../search/tests/fixtures").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A one-file torrent that names no tracker.
fn torrent(name: &str) -> Vec<u8> {
    let (length, piece) = (1usize << 20, 1usize << 18);
    let pieces = vec![0x5a; 20 * length / piece];
    let mut info =
        format!("d6:lengthi{length}e4:name{}:{name}12:piece lengthi{piece}e6:pieces{}:", name.len(), pieces.len())
            .into_bytes();
    info.extend_from_slice(&pieces);
    info.push(b'e');
    [b"d4:info".as_slice(), &info, b"e"].concat()
}

type Routes = Vec<(&'static str, Vec<Reply>)>;

/// Answers each path prefix with its replies in order, repeating the last one, and logs
/// every request path.
struct Server {
    base: String,
    paths: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start(routes: impl FnOnce(&str) -> Routes) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes = Arc::new(routes(&base));
        let paths: Arc<Mutex<Vec<String>>> = Arc::default();
        let counts: Arc<Mutex<HashMap<&'static str, usize>>> = Arc::default();
        let log = paths.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (routes, log, counts) = (routes.clone(), log.clone(), counts.clone());
                std::thread::spawn(move || answer(stream, &routes, &log, &counts));
            }
        });
        Server { base, paths }
    }

    fn asked(&self, prefix: &str) -> Vec<String> {
        self.paths.lock().unwrap().iter().filter(|p| p.starts_with(prefix)).cloned().collect()
    }
}

fn answer(mut stream: TcpStream, routes: &Routes, log: &Mutex<Vec<String>>, counts: &Mutex<HashMap<&str, usize>>) {
    let mut head = Vec::new();
    let mut buf = [0u8; 4096];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let path = String::from_utf8_lossy(&head).split_whitespace().nth(1).unwrap_or("/").to_string();
    log.lock().unwrap().push(path.clone());
    let reply = match routes.iter().find(|(prefix, _)| path.starts_with(prefix)) {
        Some((prefix, replies)) => {
            let mut counts = counts.lock().unwrap();
            let n = counts.entry(prefix).or_insert(0);
            *n += 1;
            replies[(*n - 1).min(replies.len() - 1)].clone()
        }
        None => status(404),
    };
    std::thread::sleep(reply.delay);
    let head =
        format!("HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reply.status, reply.body.len());
    let _ = stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(&reply.body));
}

/// One isolated installation whose config disables every built-in source but `enabled`,
/// which point at the server, and adds `sources` to its `sources` section. Dropping it
/// stops its engine.
struct Home {
    dir: tempfile::TempDir,
    vars: Vec<(&'static str, String)>,
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| panic!("{e}: {}{}", self.stdout, self.stderr))
    }
}

impl Home {
    fn new(server: &Server, enabled: &[&str], sources: &str) -> Home {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("th");
        let disabled: Vec<String> = BUILTIN.iter().filter(|s| !enabled.contains(s)).map(|s| format!("{s:?}")).collect();
        let endpoints: Vec<String> = enabled.iter().map(|s| format!("{s}={}/{s}/", server.base)).collect();
        let home = Home {
            vars: vec![
                ("HOME", dir.path().display().to_string()),
                ("TORRENTIAL_HOME", root.display().to_string()),
                ("TORRENTIAL_TEST_IDLE_TIMEOUT", "30s".into()),
                ("TORRENTIAL_TEST_NETWORK", "loopback".into()),
                ("TORRENTIAL_TEST_DISK", "roomy".into()),
                ("TORRENTIAL_TEST_ENDPOINTS", endpoints.join(",")),
            ],
            dir,
        };
        home.write_config(&format!("sources:\n  disabled: [{}]\n{sources}", disabled.join(", ")));
        home
    }

    fn write_config(&self, text: &str) {
        let dir = self.dir.path().join("th/config");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.yaml"), text).unwrap();
    }

    fn set(&mut self, key: &'static str, value: &str) {
        self.vars.push((key, value.into()));
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::cargo_bin("torrential").unwrap();
        for key in ["CODEX_SANDBOX_NETWORK_DISABLED", "CODEX_THREAD_ID", "CODEX_CI", "TORRENTIAL_TEST_ENGINE"] {
            cmd.env_remove(key);
        }
        cmd.args(args).envs(self.vars.iter().map(|(k, v)| (*k, v.as_str()))).timeout(Duration::from_secs(60));
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

    fn path(&self, rest: &str) -> PathBuf {
        self.dir.path().join(rest)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.command(&["daemon", "stop"]).output();
    }
}

#[test]
fn search_ranks_what_the_sources_answered() {
    let server = Server::start(|_| {
        vec![
            ("/knaben/", vec![ok(fixture("knaben/v2-search-ubuntu.json"))]),
            ("/apibay/", vec![ok(fixture("apibay/q.php-ubuntu.json"))]),
        ]
    });
    let home = Home::new(&server, &["knaben", "apibay"], "");

    let plain = home.run(&["search", "ubuntu", "--limit", "3"]);
    let json = home.run(&["search", "ubuntu", "--limit", "3", "--json"]).json();

    assert_eq!(plain.code, 0, "{}", plain.stderr);
    // Every column but AGE, which counts from today.
    let table: Vec<String> = plain
        .stdout
        .lines()
        .take_while(|l| !l.starts_with('#'))
        .map(|l| l.split('\t').enumerate().filter(|(i, _)| *i != 3).map(|(_, c)| c).collect::<Vec<_>>().join(" | "))
        .collect();
    assert_eq!(
        table,
        [
            "ID | SEEDS | SIZE | SOURCE | FLAGS | NAME",
            "dafc8c | 143 | 6.1 GiB | knaben | - | ubuntu-26.04-desktop-amd64.iso",
            "4a3f5e | 52 | 5.8 GiB | knaben+1 | trusted | ubuntu-24.04.1-desktop-amd64.iso",
            "2c6b68 | 37 | 3.4 GiB | knaben+1 | trusted | Ubuntu 22.04 LTS",
        ]
    );
    let result = &json["result"];
    let ids: Vec<&str> = result["releases"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        [
            "dafc8c076ca2f3ed376eeae7c76a0d6be2415c45",
            "4a3f5e08bcef825718eda30637230585e3330599",
            "2c6b6858d61da9543d4231a71db4b1c9264b0685"
        ]
    );
    assert_eq!(result["total"], 158);
    let sources: Vec<String> =
        result["sources"].as_array().unwrap().iter().map(|s| format!("{} {}", s["source"], s["outcome"])).collect();
    // The engine process kept both answers from the first search.
    assert_eq!(sources, [r#""knaben" "cached""#, r#""apibay" "cached""#]);
    assert_eq!((server.asked("/knaben/").len(), server.asked("/apibay/").len()), (1, 1));
}

const NETINST: &str = "/debian/debian-cd/current/arm64/bt-cd/debian-13.7.0-arm64-netinst.iso.torrent";

fn debian(netinst: Vec<Reply>) -> impl FnOnce(&str) -> Routes {
    |_| {
        vec![
            (NETINST, netinst),
            ("/debian/debian-cd/current/arm64/bt-cd/", vec![ok(fixture("debian/debian-cd-current-arm64-bt-cd.html"))]),
            (
                "/debian/debian-cd/current/arm64/bt-dvd/",
                vec![ok(fixture("debian/debian-cd-current-arm64-bt-dvd.html"))],
            ),
            ("/debian/debian-cd/current/amd64/bt-cd/", vec![ok(fixture("debian/debian-cd-current-amd64-bt-cd.html"))]),
            (
                "/debian/debian-cd/current/amd64/bt-dvd/",
                vec![ok(fixture("debian/debian-cd-current-amd64-bt-dvd.html"))],
            ),
        ]
    }
}

#[test]
fn get_on_a_search_result_downloads_into_the_configured_folder() {
    let server = Server::start(debian(vec![ok(torrent("debian-13.7.0-arm64-netinst.iso"))]));
    let home = Home::new(&server, &["debian"], "");
    let isos = home.path("isos");
    home.write_config(&format!(
        "{}downloads:\n  dir: \"{}\"\n",
        std::fs::read_to_string(home.path("th/config/config.yaml")).unwrap(),
        isos.display()
    ));

    let found = home.run(&["search", "debian", "13", "arm64", "netinst", "--json"]).json();
    let release = &found["result"]["releases"][0];
    let id = release["id"].as_str().unwrap();
    let get = home.run(&["get", &id[..6]]);
    let status = home.run(&["status", &id[..6], "--json"]).json();

    assert_eq!(
        (release["name"].as_str(), release["flags"].clone()),
        (Some("debian-13.7.0-arm64-netinst.iso"), serde_json::json!(["official"]))
    );
    assert_eq!(get.code, 0, "{}{}", get.stdout, get.stderr);
    assert!(
        get.stdout.starts_with(&format!("started {} debian-13.7.0-arm64-netinst.iso\n", &id[..6])),
        "{}",
        get.stdout
    );
    let job = &status["jobs"][0];
    assert_eq!((job["id"].as_str(), job["dir"].as_str()), (Some(id), isos.to_str()));
    assert_eq!(job["name"], "debian-13.7.0-arm64-netinst.iso");
    // Hashed once by the search, and fetched again for the download.
    assert_eq!(server.asked(NETINST).len(), 2);
}

#[test]
fn private_indexer_links_never_reach_the_output() {
    let server = Server::start(|base| {
        let feed =
            String::from_utf8(fixture("torznab/jackett-ubuntu.xml")).unwrap().replace("http://127.0.0.1:9117", base);
        vec![
            ("/torznab/api", vec![ok(feed.into_bytes())]),
            ("/dl/privatehd/", vec![status(404), status(404), ok(torrent("ubuntu-private.iso"))]),
        ]
    });
    let sources = format!(
        "  torznab:\n    - name: jackett\n      url: {}/torznab/api\n      api_key_env: TEST_JACKETT_KEY\n",
        server.base
    );
    let mut home = Home::new(&server, &[], &sources);
    home.set("TEST_JACKETT_KEY", "k3y");

    let plain = home.run(&["search", "ubuntu"]);
    let json = home.run(&["search", "ubuntu", "--json"]);
    let locator: String = json.json()["result"]["releases"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["id"].as_str())
        .find(|id| id.starts_with('u'))
        .unwrap()[..7]
        .to_string();
    let refused = home.run(&["get", &locator]);
    let refused_json = home.run(&["get", &locator, "--json"]);
    let started = home.run(&["get", &locator]);
    let job = started.stdout.split_whitespace().nth(1).unwrap_or_default().to_string();
    let status = home.run(&["status"]);
    let status_json = home.run(&["status", "--json"]);
    let files = home.run(&["files", &job]);
    let files_json = home.run(&["files", &job, "--json"]);

    assert_eq!(plain.code, 0, "{}", plain.stderr);
    assert_eq!(refused.code, 5);
    assert!(refused.stderr.contains("could not fetch the .torrent file: http 404"), "{}", refused.stderr);
    assert_eq!(refused_json.json()["error"]["code"], "fetch_failed");
    assert_eq!(started.code, 0, "{}{}", started.stdout, started.stderr);
    assert_eq!(files_json.json()["files"]["name"], "ubuntu-private.iso");
    let runs = [&plain, &json, &refused, &refused_json, &started, &status, &status_json, &files, &files_json];
    for run in runs {
        let out = format!("{}{}", run.stdout, run.stderr);
        assert!(!out.contains("k3y") && !out.contains("apikey"), "a private link reached the output: {out}");
    }
    assert_eq!(server.asked("/torznab/api"), ["/torznab/api?t=search&q=ubuntu&apikey=k3y"]);
    assert_eq!(server.asked("/dl/privatehd/"), vec!["/dl/privatehd/?jackett_apikey=k3y&path=def&file=ubuntu"; 3]);
}

fn send(socket: &mut UnixStream, request: &Request) {
    socket.write_all(&encode_frame(request).unwrap()).unwrap();
}

fn receive(socket: &mut UnixStream) -> Value {
    let mut len = [0u8; 4];
    socket.read_exact(&mut len).unwrap();
    let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
    socket.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn a_streaming_search_sends_updates_before_the_result() {
    let server = Server::start(|_| {
        vec![
            ("/knaben/", vec![ok(fixture("knaben/v2-search-ubuntu.json"))]),
            ("/apibay/", vec![Reply { delay: Duration::from_millis(800), ..ok(fixture("apibay/q.php-ubuntu.json")) }]),
        ]
    });
    let home = Home::new(&server, &["knaben", "apibay"], "");
    assert_eq!(home.run(&["status"]).code, 0);

    let mut socket = UnixStream::connect(home.path("th/run/engine.sock")).unwrap();
    send(&mut socket, &Request::Hello { client_version: "test".into(), wire: WIRE_VERSION });
    receive(&mut socket);
    send(&mut socket, &Request::Search { query: SearchQuery::new("ubuntu"), stream: true });
    let mut frames = Vec::new();
    loop {
        let frame = receive(&mut socket);
        let kind = frame["event"].as_str().or(frame["type"].as_str()).unwrap().to_string();
        let sources: Vec<String> = frame["result"]["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| format!("{} {}", s["source"].as_str().unwrap(), s["outcome"].as_str().unwrap()))
            .collect();
        frames.push(format!("{kind}: {sources:?} total {} pending {}", frame["result"]["total"], frame["pending"]));
        if kind != "search_update" {
            break;
        }
    }

    assert_eq!(
        frames,
        [
            r#"search_update: ["knaben ok"] total 97 pending ["apibay"]"#,
            r#"search: ["knaben ok", "apibay ok"] total 158 pending null"#,
        ]
    );
}

#[test]
fn the_engine_refuses_a_config_that_does_not_parse() {
    let server = Server::start(|_| Vec::new());
    let home = Home::new(&server, &[], "  torznab: 3\n");

    let refused = home.run(&["status"]);
    let doctor = home.run(&["doctor", "--json"]).json();
    home.write_config("sources:\n  disabled: [nyaa]\n");
    let fixed = home.run(&["status"]);

    let message = format!("{} line 3: expected sequence start", home.path("th/config/config.yaml").display());
    assert_eq!(refused.code, 2, "{}{}", refused.stdout, refused.stderr);
    assert!(refused.stderr.contains(&message), "{}", refused.stderr);
    let config = doctor["checks"].as_array().unwrap().iter().find(|c| c["check"] == "config").unwrap();
    assert_eq!((config["status"].as_str(), config["detail"].as_str()), (Some("fail"), Some(message.as_str())));
    assert_eq!(fixed.code, 0, "{}", fixed.stderr);
}

#[test]
fn doctor_network_asks_each_enabled_source() {
    let server = Server::start(|_| {
        vec![
            ("/knaben/", vec![status(503)]),
            ("/apibay/", vec![ok(fixture("apibay/q.php-zzqqxxnotarealthing987.json"))]),
            ("/nyaa/", vec![ok(fixture("nyaa/index-linux.xml"))]),
        ]
    });
    let home = Home::new(&server, &["knaben", "apibay", "nyaa"], "");

    let doctor = home.run(&["doctor", "--network", "--json"]).json();

    let sources: Vec<(String, String, String)> = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["check"].as_str().unwrap().starts_with("source"))
        .map(|c| {
            let detail = c["detail"].as_str().unwrap();
            let (outcome, took) = detail.rsplit_once(", ").unwrap();
            assert!(took.ends_with(" ms") && took[..took.len() - 3].parse::<u32>().is_ok(), "{detail}");
            (c["check"].as_str().unwrap().to_string(), c["status"].as_str().unwrap().to_string(), outcome.to_string())
        })
        .collect();
    let expected = [
        ("source knaben", "fail", "http 503"),
        ("source apibay", "ok", "0 results"),
        ("source nyaa", "ok", "30 results"),
    ];
    assert_eq!(sources, expected.map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string())));
    assert_eq!(server.asked("/nyaa/"), ["/nyaa/?page=rss&q=linux&c=0_0&f=0"]);
}

#[test]
fn files_on_an_archive_org_result_reads_its_torrent_url_not_the_bare_magnet() {
    let item = "/archive.org/download/night_of_the_living_dead_dvd/night_of_the_living_dead_dvd_archive.torrent";
    let server = Server::start(|_| {
        vec![
            (
                "/archive.org/advancedsearch.php",
                vec![ok(fixture("archive.org/advancedsearch.php-night-of-the-living-dead.json"))],
            ),
            (item, vec![ok(torrent("night_of_the_living_dead_dvd"))]),
            ("/archive.org/download/Night_Of_The_Living_Dead_raw_HD_WS/", vec![status(503)]),
        ]
    });
    let home = Home::new(&server, &["archive.org"], "");

    let found = home.run(&["search", "night", "of", "the", "living", "dead", "--limit", "50", "--json"]).json();
    let files = home.run(&["files", "6bc1af", "--json"]).json();
    let asked = std::time::Instant::now();
    let unreachable = home.run(&["files", "27f32a"]);
    let took = asked.elapsed();

    let release = found["result"]["releases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "6bc1af23a17fd62156ab2f0e41d3c86769c2c66c")
        .unwrap();
    assert_eq!(
        release["magnet"],
        "magnet:?xt=urn:btih:6bc1af23a17fd62156ab2f0e41d3c86769c2c66c&dn=Night%20of%20the%20Living%20Dead"
    );
    assert_eq!(
        (files["files"]["metadata_source"].as_str(), files["files"]["name"].as_str()),
        (Some("torrent_url"), Some("night_of_the_living_dead_dvd"))
    );
    assert_eq!(server.asked(item).len(), 1);
    // Rather than a minute waiting for DHT peers that a magnet without trackers may never find.
    assert_eq!(unreachable.code, 5, "{}", unreachable.stderr);
    assert!(
        unreachable.stderr.starts_with("error[fetch_failed]: could not fetch the .torrent file: http 503\n"),
        "{}",
        unreachable.stderr
    );
    assert!(took < Duration::from_secs(15), "took {took:?}");
}
