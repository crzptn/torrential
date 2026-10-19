use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use torrential_core::{Error, ErrorCode};

use crate::env::Env;
use crate::paths::Paths;
use crate::skill::{Writes, sha256, write_atomic};

const RELEASES: &str = "https://github.com/nexxeln/torrential/releases";
const ISSUES: &str = "https://github.com/nexxeln/torrential/issues";
const CHECK_WITHIN: Duration = Duration::from_secs(10);
const MAX_ASSET: usize = 256 << 20;
const CHECKED: &str = "update.json";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    /// Say when a newer version is out
    #[default]
    Notify,
    /// Install newer versions in the background
    Auto,
    /// Never look for newer versions
    Off,
}

impl Policy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Notify => "notify",
            Self::Auto => "auto",
            Self::Off => "off",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    /// False for a pre-release such as 0.2.0-rc1, which comes before 0.2.0.
    release: bool,
}

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let numbers: Vec<u64> = core.split('.').map(str::parse).collect::<Result<_, _>>().ok()?;
        let [major, minor, patch] = numbers[..] else { return None };
        Some(Version { major, minor, patch, release: pre.is_none() })
    }

    pub fn newer_than(self, current: &str) -> bool {
        Version::parse(current).is_some_and(|current| self > current)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// GitHub answers `/releases/latest` with a redirect to `/releases/tag/<tag>`. Reading the
/// redirect avoids the REST API, which allows 60 requests an hour.
pub fn tag_version(location: &str) -> Option<Version> {
    let (_, tag) = location.trim_end_matches('/').rsplit_once("/tag/")?;
    Version::parse(tag)
}

pub fn target(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        _ => None,
    }
}

/// What the last look at the releases found, kept in the state folder.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Checked {
    pub latest: Option<String>,
    pub checked_at: Option<i64>,
    pub failed_at: Option<i64>,
    pub install_failed_at: Option<i64>,
}

impl Checked {
    fn read(paths: &Paths) -> Checked {
        read_json(&paths.state.join(CHECKED)).unwrap_or_default()
    }

    fn write(&self, paths: &Paths) {
        write_json(&paths.state.join(CHECKED), self);
    }
}

pub struct Releases {
    base: String,
}

impl Releases {
    /// `TORRENTIAL_TEST_RELEASES` replaces the GitHub releases URL, so a test can serve releases
    /// from its own server. A test engine, which has `TORRENTIAL_TEST_NETWORK` or
    /// `TORRENTIAL_TEST_ENGINE` set, never looks for releases without it.
    pub fn from_env(env: &Env) -> Option<Releases> {
        let base = match env.var("TORRENTIAL_TEST_RELEASES") {
            Some(url) => url,
            None if env.var("TORRENTIAL_TEST_NETWORK").is_some() || env.var("TORRENTIAL_TEST_ENGINE").is_some() => {
                return None;
            }
            None => RELEASES,
        };
        Some(Releases { base: base.trim_end_matches('/').to_string() })
    }

    pub fn required(env: &Env) -> Result<Releases, Error> {
        Releases::from_env(env).ok_or_else(|| {
            Error::new(ErrorCode::NetworkUnavailable, "this test environment does not look for releases")
                .with_hint("set TORRENTIAL_TEST_RELEASES to a release server")
        })
    }

    async fn latest(&self) -> Result<Version, Error> {
        let url = format!("{}/latest", self.base);
        let client =
            client(reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(CHECK_WITHIN))?;
        let response = client.get(&url).send().await.map_err(|e| unreachable(&url, &e))?;
        let location = response.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok());
        match location.and_then(tag_version) {
            Some(latest) if response.status().is_redirection() => Ok(latest),
            _ => Err(Error::new(
                ErrorCode::FetchFailed,
                format!("{url} answered {} without naming a release", response.status()),
            )
            .with_hint("run torrential update again later")),
        }
    }

    async fn download(&self, version: Version, asset: &str) -> Result<Vec<u8>, Error> {
        let url = format!("{}/download/v{version}/{asset}", self.base);
        let client = client(
            reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).read_timeout(Duration::from_secs(30)),
        )?;
        let mut response = client.get(&url).send().await.map_err(|e| unreachable(&url, &e))?;
        if !response.status().is_success() {
            return Err(Error::new(ErrorCode::FetchFailed, format!("{url} answered {}", response.status()))
                .with_hint("run torrential update again later"));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| unreachable(&url, &e))? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_ASSET {
                return Err(Error::new(ErrorCode::FetchFailed, format!("{url} is larger than 256 MiB")));
            }
        }
        Ok(body)
    }
}

fn client(builder: reqwest::ClientBuilder) -> Result<reqwest::Client, Error> {
    builder
        .user_agent(concat!("torrential/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| Error::internal(format!("cannot build an HTTP client: {e}")))
}

fn unreachable(url: &str, e: &reqwest::Error) -> Error {
    let mut cause: &dyn std::error::Error = e;
    while let Some(source) = cause.source() {
        cause = source;
    }
    Error::new(ErrorCode::NetworkUnavailable, format!("cannot reach {url}: {cause}"))
        .with_hint("check the network connection, then run torrential update again")
}

pub fn runtime() -> Result<tokio::runtime::Runtime, Error> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::internal(format!("cannot start the async runtime: {e}")))
}

/// Asks for the latest release and records the answer, or the failure, in the state folder.
pub async fn check(releases: &Releases, paths: &Paths, now: i64) -> Result<Version, Error> {
    let mut checked = Checked::read(paths);
    let latest = releases.latest().await;
    match &latest {
        Ok(version) => {
            checked.latest = Some(version.to_string());
            checked.checked_at = Some(now);
        }
        Err(_) => checked.failed_at = Some(now),
    }
    checked.write(paths);
    latest
}

pub enum Outcome {
    Latest(Version),
    Updated(Version),
}

/// Checks again, then replaces this binary with the latest release when it is newer.
pub async fn install(releases: &Releases, paths: &Paths, env: &Env, now: i64) -> Result<Outcome, Error> {
    let latest = check(releases, paths, now).await?;
    if !latest.newer_than(&env.version()) {
        return Ok(Outcome::Latest(latest));
    }
    let Some(target) = target(std::env::consts::OS, std::env::consts::ARCH) else {
        return Err(Error::new(
            ErrorCode::NotFound,
            format!("there is no prebuilt torrential for {} {}", std::env::consts::OS, std::env::consts::ARCH),
        )
        .with_hint("build it from source as https://github.com/nexxeln/torrential#install shows"));
    };
    let exe = executable()?;
    if exe.starts_with("/nix/store") {
        return Err(Error::new(
            ErrorCode::StorageError,
            format!("torrential runs from {}, which Nix manages", exe.display()),
        )
        .with_hint("update it with Nix, for example nix profile upgrade torrential"));
    }
    let writes = Writes::new(env, "torrential update".into());
    let partial = Partial::create(&exe, &writes)?;
    let asset = format!("torrential-{target}.tar.gz");
    let tarball = releases.download(latest, &asset).await?;
    let sums = releases.download(latest, &format!("{asset}.sha256")).await?;
    verify(&asset, &tarball, &sums)?;
    partial.replace(&exe, &unpack(&asset, &tarball)?, &writes)?;
    let _ = Command::new(&exe)
        .args(["skill", "refresh"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    Ok(Outcome::Updated(latest))
}

pub fn executable() -> Result<PathBuf, Error> {
    std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| Error::new(ErrorCode::StorageError, format!("cannot find the torrential binary: {e}")))
}

fn verify(asset: &str, tarball: &[u8], sums: &[u8]) -> Result<(), Error> {
    let published = std::str::from_utf8(sums).ok().and_then(|s| s.split_whitespace().next()).unwrap_or_default();
    if published.eq_ignore_ascii_case(&sha256(tarball)) {
        return Ok(());
    }
    Err(Error::new(
        ErrorCode::VerificationFailed,
        format!("{asset} does not match its published checksum, so the installed torrential is unchanged"),
    )
    .with_hint(format!("run torrential update again later; if it fails again, report it at {ISSUES}")))
}

fn unpack(asset: &str, tarball: &[u8]) -> Result<Vec<u8>, Error> {
    let bad = |e: std::io::Error| Error::new(ErrorCode::FetchFailed, format!("cannot unpack {asset}: {e}"));
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tarball));
    for entry in archive.entries().map_err(bad)? {
        let mut entry = entry.map_err(bad)?;
        let path = entry.path().map_err(bad)?.into_owned();
        if entry.header().entry_type().is_file() && path.strip_prefix(".").unwrap_or(&path) == Path::new("torrential") {
            let mut binary = Vec::new();
            entry.read_to_end(&mut binary).map_err(bad)?;
            return Ok(binary);
        }
    }
    Err(Error::new(ErrorCode::FetchFailed, format!("{asset} holds no torrential binary")))
}

/// The new binary, written beside the old one and renamed over it, so the path always holds a
/// whole binary. It is created before the download, so a folder you cannot write fails first.
struct Partial {
    path: PathBuf,
    file: File,
}

impl Partial {
    fn create(exe: &Path, writes: &Writes) -> Result<Partial, Error> {
        let dir = exe.parent().unwrap_or(Path::new("/"));
        let path = dir.join(format!(".torrential.{}.update", std::process::id()));
        let file = OpenOptions::new().write(true).create(true).truncate(true).mode(0o755).open(&path).map_err(|e| {
            let error = writes.error(dir, e);
            match error.code {
                ErrorCode::StorageError => error.with_hint(format!(
                    "reinstall torrential in a folder you can write, such as ~/.local/bin, or run torrential update \
                     as a user who can write {}",
                    dir.display()
                )),
                _ => error,
            }
        })?;
        Ok(Partial { path, file })
    }

    fn replace(mut self, exe: &Path, binary: &[u8], writes: &Writes) -> Result<(), Error> {
        self.file
            .write_all(binary)
            .and_then(|()| self.file.set_permissions(std::fs::Permissions::from_mode(0o755)))
            .and_then(|()| self.file.sync_all())
            .and_then(|()| std::fs::rename(&self.path, exe))
            .map_err(|e| writes.error(exe, e))
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write_json(path: &Path, value: &impl Serialize) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = write_atomic(path, &serde_json::to_vec(value).expect("update records serialize"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn versions_parse_with_or_without_a_v_and_order_numerically() {
        assert_eq!(version("v0.1.10").to_string(), "0.1.10");
        assert!(version("0.1.10") > version("0.1.9"));
        assert!(version("1.0.0") > version("0.99.99"));
        assert!(version("0.2.0-rc1") < version("0.2.0"));
        assert!(version("0.2.0-rc1") > version("0.1.9"));
        for bad in ["", "0.1", "0.1.0.0", "x.1.0", "latest"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_latest_redirect_names_the_tag() {
        let found = |location: &str| tag_version(location).map(|v| v.to_string());
        assert_eq!(found("https://github.com/nexxeln/torrential/releases/tag/v0.1.1").as_deref(), Some("0.1.1"));
        assert_eq!(found("/nexxeln/torrential/releases/tag/v2.0.3/").as_deref(), Some("2.0.3"));
        assert_eq!(found("https://github.com/nexxeln/torrential/releases"), None);
        assert_eq!(found("https://github.com/nexxeln/torrential/releases/tag/nightly"), None);
    }

    #[test]
    fn only_two_platforms_have_a_prebuilt_binary() {
        assert_eq!(target("macos", "aarch64"), Some("aarch64-apple-darwin"));
        assert_eq!(target("linux", "x86_64"), Some("x86_64-unknown-linux-musl"));
        assert_eq!(target("macos", "x86_64"), None);
        assert_eq!(target("linux", "aarch64"), None);
    }
}
