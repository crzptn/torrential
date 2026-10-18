use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use assert_cmd::Command;

const ARCH: &str = "9b521219bc48550140a666f74a29048ed78e933b";

/// One isolated torrential installation. Dropping it stops its engine.
struct Home {
    dir: tempfile::TempDir,
    vars: Vec<(&'static str, String)>,
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Home {
    fn new() -> Self {
        Self::with(&[])
    }

    fn with(vars: &[(&'static str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut all = vec![
            ("HOME", dir.path().display().to_string()),
            ("TORRENTIAL_HOME", dir.path().join("th").display().to_string()),
            ("TORRENTIAL_TEST_IDLE_TIMEOUT", "30s".to_string()),
            ("TORRENTIAL_TEST_NETWORK", "loopback".to_string()),
        ];
        all.extend(vars.iter().map(|(k, v)| (*k, v.to_string())));
        Self { dir, vars: all }
    }

    fn set(&mut self, key: &'static str, value: &str) {
        self.vars.retain(|(k, _)| *k != key);
        self.vars.push((key, value.to_string()));
    }

    fn unset(&mut self, key: &'static str) {
        self.vars.retain(|(k, _)| *k != key);
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::cargo_bin("torrential").unwrap();
        for key in [
            "CODEX_SANDBOX_NETWORK_DISABLED",
            "CODEX_THREAD_ID",
            "CODEX_CI",
            "TORRENTIAL_TEST_VERSION",
            "TORRENTIAL_TEST_ENGINE",
        ] {
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

    fn engine(&self) -> Option<(i32, String)> {
        let run = self.run(&["daemon", "status", "--json"]);
        let doc: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
        Some((doc["pid"].as_i64()? as i32, doc["version"].as_str()?.to_string()))
    }

    fn state(&self) -> PathBuf {
        self.dir.path().join("th/state")
    }

    fn downloads(&self) -> String {
        self.dir.path().join("dl").display().to_string()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.command(&["daemon", "stop"]).output();
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn eventually(what: &str, within: Duration, done: impl Fn() -> bool) {
    let deadline = Instant::now() + within;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn status_starts_an_engine_that_outlives_the_call() {
    let home = Home::new();
    let run = home.run(&["status"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "ID\tSTATE\tPROGRESS\tSIZE\tDOWN\tPEERS\tETA\tNAME\n# 0 downloads\nnext: torrential search <words>\n"
    );
    let (pid, version) = home.engine().expect("the engine is still running");
    assert!(alive(pid));
    assert_eq!(version, env!("CARGO_PKG_VERSION"));
    let lock = std::fs::read_to_string(home.state().join("engine.lock")).unwrap();
    assert_eq!(lock.trim(), pid.to_string());
    let log = std::fs::read_to_string(home.state().join("engine.log")).unwrap();
    assert!(log.contains(&format!("started, pid {pid}")), "{log}");
}

#[test]
fn concurrent_clients_share_one_engine() {
    let home = Home::new();
    let children: Vec<_> = (0..4).map(|_| home.command(&["status", "--json"]).output()).collect();
    for child in children {
        let out = child.unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let (pid, _) = home.engine().unwrap();
    assert_eq!(home.run(&["status"]).code, 0);
    assert_eq!(home.engine().unwrap().0, pid, "a second client attached to the first engine");
}

#[test]
fn a_version_mismatch_restarts_the_engine() {
    let mut home = Home::with(&[("TORRENTIAL_TEST_VERSION", "0.0.1-old")]);
    assert_eq!(home.run(&["status"]).code, 0);
    let (old_pid, old_version) = home.engine().unwrap();
    assert_eq!(old_version, "0.0.1-old");
    home.unset("TORRENTIAL_TEST_VERSION");
    let run = home.run(&["status"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let (new_pid, new_version) = home.engine().unwrap();
    assert_eq!(new_version, env!("CARGO_PKG_VERSION"));
    assert_ne!(new_pid, old_pid);
    assert!(!alive(old_pid), "the old engine exited after saving its state");
}

#[test]
fn a_sandboxed_client_refuses_to_start_an_engine() {
    let mut home = Home::with(&[("CODEX_SANDBOX_NETWORK_DISABLED", "1"), ("CODEX_THREAD_ID", "t-1")]);
    let run = home.run(&["status"]);
    assert_eq!(run.code, 6);
    assert_eq!(run.stdout, "");
    assert_eq!(
        run.stderr,
        "error[sandbox_network_blocked]: this sandbox blocks network access, so the engine could not reach any peer\n  \
         signal: CODEX_SANDBOX_NETWORK_DISABLED=1 (codex)\n\
         hint: in interactive Codex, rerun with sandbox_permissions \"require_escalated\" and prefix_rule [\"torrential\"]; \
         unattended Codex (codex exec) has approvals off, so torrential cannot work there\n"
    );
    let json = home.run(&["status", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&json.stdout).unwrap();
    assert_eq!((json.code, doc["error"]["code"].as_str()), (6, Some("sandbox_network_blocked")));
    home.unset("CODEX_SANDBOX_NETWORK_DISABLED");
    home.unset("CODEX_THREAD_ID");
    assert_eq!(home.engine(), None, "no engine was started");
}

#[test]
fn the_engine_exits_after_its_idle_timeout() {
    let mut home = Home::new();
    home.set("TORRENTIAL_TEST_IDLE_TIMEOUT", "1s");
    assert_eq!(home.run(&["status"]).code, 0);
    let (pid, _) = home.engine().unwrap();
    eventually("the idle engine exits", Duration::from_secs(10), || !alive(pid));
    assert!(!home.dir.path().join("th/run/engine.sock").exists(), "the socket was removed");
    assert!(std::fs::read_to_string(home.state().join("engine.log")).unwrap().contains("engine stopped"));
}

fn magnet(size: u64) -> String {
    format!("magnet:?xt=urn:btih:{ARCH}&dn=archlinux-2026.10.01-x86_64.iso&xl={size}")
}

#[test]
fn wait_times_out_with_exit_8_while_the_download_runs() {
    let home = Home::with(&[("TORRENTIAL_TEST_ENGINE", "fake"), ("TORRENTIAL_TEST_FAKE_RATE", "1MiB")]);
    let get = home.run(&["get", &magnet(10 << 30), "--dir", &home.downloads()]);
    assert_eq!(get.code, 0, "{}", get.stderr);
    assert!(get.stdout.starts_with("started 9b5212"), "{}", get.stdout);
    assert!(get.stdout.ends_with("next:  torrential wait 9b5212\n"), "{}", get.stdout);
    let wait = home.run(&["wait", "9b5212", "--timeout", "1s"]);
    assert_eq!(wait.code, 8, "{}", wait.stderr);
    let lines: Vec<&str> = wait.stdout.lines().collect();
    assert_eq!(lines[0], "ELAPSED\tID\tSTATE\tPROGRESS\tSIZE\tDOWN\tPEERS\tETA\tNAME");
    assert!(lines[1].starts_with("0s\t9b5212\t"), "{}", wait.stdout);
    assert!(lines[lines.len() - 2].starts_with("# still running after 1s · eta "), "{}", wait.stdout);
    assert_eq!(lines[lines.len() - 1], "next: torrential wait 9b5212");
    let json = home.run(&["wait", "9b5212", "--timeout", "1s", "--json"]);
    assert_eq!(json.code, 8);
    let last: serde_json::Value = serde_json::from_str(json.stdout.lines().last().unwrap()).unwrap();
    assert_eq!((last["type"].as_str(), last["outcome"].as_str()), (Some("result"), Some("timeout")));
}

#[test]
fn search_get_and_wait_to_done_in_four_calls() {
    let home = Home::with(&[("TORRENTIAL_TEST_ENGINE", "fake"), ("TORRENTIAL_TEST_FAKE_RATE", "100GiB")]);
    let search = home.run(&["search", "arch", "linux"]);
    assert_eq!(search.code, 0, "{}", search.stderr);
    assert_eq!(search.stdout.lines().nth(1).unwrap().split('\t').next(), Some("9b5212"));
    let get = home.run(&["get", "9b5212", "--dir", &home.downloads()]);
    assert_eq!(get.code, 0, "{}", get.stderr);
    let wait = home.run(&["wait", "9b5212"]);
    assert_eq!(wait.code, 0, "{}{}", wait.stdout, wait.stderr);
    let path = Path::new(&home.downloads()).join("archlinux-2026.10.01-x86_64.iso");
    assert!(wait.stdout.contains(&format!("· path {}", path.display())), "{}", wait.stdout);
    assert_eq!(wait.stdout.lines().last(), Some("next: none"));
    let again = home.run(&["get", "9b5212", "--dir", &home.downloads()]);
    assert!(again.stdout.starts_with("existing 9b5212"), "{}", again.stdout);
}

#[test]
fn a_killed_engine_comes_back_with_the_download_interrupted() {
    let home = Home::with(&[("TORRENTIAL_TEST_ENGINE", "fake"), ("TORRENTIAL_TEST_FAKE_RATE", "1MiB")]);
    assert_eq!(home.run(&["get", &magnet(10 << 30), "--dir", &home.downloads()]).code, 0);
    let (pid, _) = home.engine().unwrap();
    // SAFETY: kill only sends a signal, and this pid is the engine this test started.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    eventually("the killed engine is gone", Duration::from_secs(5), || !alive(pid));
    let status = home.run(&["status", "9b5212"]);
    assert_eq!(status.code, 0, "{}", status.stderr);
    assert!(status.stdout.contains("state:    interrupted"), "{}", status.stdout);
    assert!(status.stdout.ends_with("next:     torrential resume 9b5212\n"), "{}", status.stdout);
    let resume = home.run(&["resume", "9b5212"]);
    assert_eq!(
        resume.stdout,
        "ID\tRESULT\tNAME\n9b5212\tresumed\tarchlinux-2026.10.01-x86_64.iso\nnext: torrential wait 9b5212\n"
    );
}

#[test]
fn a_new_fake_installation_starts_with_downloads_that_survive_a_restart() {
    let home = Home::with(&[("TORRENTIAL_TEST_ENGINE", "fake")]);
    let states = || {
        let run = home.run(&["status", "--json"]);
        let doc: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
        let jobs = doc["jobs"].as_array().unwrap();
        jobs.iter().map(|j| format!("{} {}", &j["id"].as_str().unwrap()[..6], j["state"].as_str().unwrap())).collect()
    };
    let first: Vec<String> = states();
    assert_eq!(first[..4], ["c7b9e3 done", "5e02c8 done", "91fd3b paused", "a90d44 error"]);
    assert!(["6f2c81 checking", "6f2c81 downloading"].contains(&first[4].as_str()), "{first:?}");
    assert_eq!(home.run(&["daemon", "stop"]).code, 0);
    let again: Vec<String> = states();
    assert_eq!(again, ["c7b9e3 done", "5e02c8 done", "91fd3b paused", "a90d44 error", "6f2c81 interrupted"]);
}

#[test]
fn damaged_state_stops_the_engine_and_reaches_the_client_untouched() {
    let home = Home::new();
    std::fs::create_dir_all(home.state()).unwrap();
    let junk = b"not a database, but somebody's data";
    std::fs::write(home.state().join("state.db"), junk).unwrap();
    let run = home.run(&["status"]);
    assert_eq!(run.code, 6, "{}", run.stderr);
    let db = home.state().join("state.db");
    assert!(
        run.stderr.starts_with(&format!("error[state_corrupt]: the state database {} is damaged", db.display())),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("torrential never resets state on its own"), "{}", run.stderr);
    assert_eq!(std::fs::read(&db).unwrap(), junk);
    assert_eq!(home.engine(), None);
}

#[test]
fn usage_errors_exit_2_with_a_hint() {
    let home = Home::new();
    let run = home.run(&["wait"]);
    assert_eq!(run.code, 2);
    assert!(
        run.stderr.starts_with("error[usage]: the following required arguments were not provided:"),
        "{}",
        run.stderr
    );
    assert!(run.stderr.ends_with("hint: torrential wait --help\n"), "{}", run.stderr);
    let json = home.run(&["frobnicate", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&json.stdout).unwrap();
    assert_eq!((json.code, doc["error"]["hint"].as_str()), (2, Some("torrential --help")));
    let bare = home.run(&[]);
    assert_eq!(bare.code, 0);
    assert!(bare.stdout.starts_with("torrential searches torrent indexes"));
}

#[test]
fn daemon_stop_is_idempotent() {
    let home = Home::new();
    assert_eq!(home.run(&["status"]).code, 0);
    let (pid, _) = home.engine().unwrap();
    let stop = home.run(&["daemon", "stop"]);
    assert_eq!(stop.stdout, format!("result: stopped\npid:    {pid}\nnext:   none\n"));
    assert!(!alive(pid));
    assert_eq!(home.run(&["daemon", "stop"]).stdout, "result: already_stopped\nnext:   none\n");
}
