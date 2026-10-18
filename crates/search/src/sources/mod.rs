//! Every source torrential can search. Adding a built-in source means one row in
//! `BUILTIN`, one `Adapter` variant, and one module with a pure parser.

mod academic;
mod apibay;
mod archive_org;
mod archlinux;
mod debian;
mod knaben;
mod listing;
mod nyaa;
mod rss;
mod torznab;
mod ubuntu;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::time::Instant;
use torrential_core::{Error, ErrorCode, SourceOutcome, text};
use url::Url;

use sha1::{Digest, Sha1};

use crate::http::{FetchError, Http};
use crate::metainfo::Metainfo;
use crate::observation::{Observation, hex};
use crate::terms::Terms;
use crate::{SearchConfig, TorznabConfig};

struct Row {
    name: &'static str,
    default_on: bool,
    /// A catalog of content its publishers may share. Ranks ahead of general indexes on ties.
    lawful: bool,
    base: &'static str,
    adapter: Adapter,
}

/// Most authoritative first: this order breaks ties in merging and ranking.
const BUILTIN: [Row; 8] = [
    Row {
        name: "archlinux",
        default_on: true,
        lawful: true,
        base: "https://archlinux.org/",
        adapter: Adapter::Archlinux,
    },
    Row {
        name: "ubuntu",
        default_on: true,
        lawful: true,
        base: "https://releases.ubuntu.com/",
        adapter: Adapter::Ubuntu,
    },
    Row {
        name: "debian",
        default_on: true,
        lawful: true,
        base: "https://cdimage.debian.org/",
        adapter: Adapter::Debian,
    },
    Row {
        name: "archive.org",
        default_on: true,
        lawful: true,
        base: "https://archive.org/",
        adapter: Adapter::ArchiveOrg,
    },
    Row {
        name: "academictorrents",
        default_on: true,
        lawful: true,
        base: "https://academictorrents.com/",
        adapter: Adapter::AcademicTorrents,
    },
    Row { name: "knaben", default_on: true, lawful: false, base: "https://api.knaben.org/", adapter: Adapter::Knaben },
    Row { name: "apibay", default_on: true, lawful: false, base: "https://apibay.org/", adapter: Adapter::Apibay },
    Row { name: "nyaa", default_on: true, lawful: false, base: "https://nyaa.si/", adapter: Adapter::Nyaa },
];

/// Not `Debug`: the Torznab variant holds the user's API key.
pub(crate) enum Adapter {
    Archlinux,
    Ubuntu,
    Debian,
    ArchiveOrg,
    AcademicTorrents,
    Knaben,
    Apibay,
    Nyaa,
    Torznab { api_key: Option<String> },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct SourceMeta {
    pub name: String,
    /// Position in the registry. Lower is more authoritative.
    pub authority: u16,
    pub lawful: bool,
    /// A private tracker's announce URLs carry the user's passkey, so only trackers
    /// from public sources are shared across listings of the same torrent.
    pub public: bool,
}

pub(crate) struct Source {
    pub meta: Arc<SourceMeta>,
    pub default_on: bool,
    base: Url,
    adapter: Adapter,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Failure {
    Timeout,
    Http(u16),
    Connect(String),
    BadResponse(String),
    Irrelevant,
}

impl Failure {
    /// Reasons quote the server, so they are sanitized here, where they leave the crate.
    pub fn outcome(&self) -> SourceOutcome {
        let clean = |reason: &str| text::truncate(&text::sanitize(reason), 160);
        match self {
            Self::Timeout => SourceOutcome::Timeout,
            Self::Http(status) => SourceOutcome::Http { status: *status },
            Self::Connect(reason) => SourceOutcome::Connect { reason: clean(reason) },
            Self::BadResponse(reason) => SourceOutcome::BadResponse { reason: clean(reason) },
            Self::Irrelevant => SourceOutcome::Irrelevant,
        }
    }
}

impl From<FetchError> for Failure {
    fn from(e: FetchError) -> Self {
        match e {
            FetchError::Timeout => Self::Timeout,
            FetchError::Status(status) => Self::Http(status),
            FetchError::Connect(reason) => Self::Connect(reason),
            FetchError::TooLarge(cap) => Self::BadResponse(format!("response larger than {cap} bytes")),
            FetchError::Decode(reason) => Self::BadResponse(reason),
        }
    }
}

/// State that outlives one search: hashed `.torrent` files and the Academic
/// Torrents catalog.
pub(crate) struct Stores {
    torrents: Mutex<HashMap<String, Metainfo>>,
    academic: academic::Catalog,
}

impl Stores {
    pub fn new(cache_dir: Option<PathBuf>) -> Stores {
        Stores { torrents: Mutex::default(), academic: academic::Catalog::new(cache_dir) }
    }
}

pub(crate) struct Ctx<'a> {
    pub http: &'a Http,
    pub stores: &'a Stores,
    pub terms: &'a Terms,
    pub adult: bool,
    pub deadline: Instant,
    /// Set when part of an answer failed, so it is shown but not cached.
    pub partial: AtomicBool,
}

impl Ctx<'_> {
    pub fn mark_partial(&self) {
        self.partial.store(true, Ordering::Relaxed);
    }
}

impl Source {
    /// What a cached answer depends on besides the query: the endpoint, and for
    /// Torznab the API key, because links issued under an old key stop working.
    pub fn scope(&self) -> String {
        match &self.adapter {
            Adapter::Torznab { api_key: Some(key) } => {
                format!("{} key:{}", self.base, hex(&Sha1::digest(key.as_bytes())[..8]))
            }
            _ => self.base.to_string(),
        }
    }

    pub async fn search(&self, ctx: &Ctx<'_>) -> Result<Vec<Observation>, Failure> {
        let base = &self.base;
        match &self.adapter {
            Adapter::Archlinux => archlinux::search(ctx, base).await,
            Adapter::Ubuntu => ubuntu::search(ctx, base).await,
            Adapter::Debian => debian::search(ctx, base).await,
            Adapter::ArchiveOrg => archive_org::search(ctx, base).await,
            Adapter::AcademicTorrents => academic::search(ctx, base).await,
            Adapter::Knaben => knaben::search(ctx, base).await,
            Adapter::Apibay => apibay::search(ctx, base).await,
            Adapter::Nyaa => nyaa::search(ctx, base).await,
            Adapter::Torznab { api_key } => torznab::search(ctx, base, api_key.as_deref()).await,
        }
    }
}

/// Builds the registry: the built-in rows, then the configured Torznab endpoints,
/// less the disabled sources.
pub(crate) fn registry(config: &SearchConfig) -> Result<Vec<Arc<Source>>, Error> {
    for name in config.endpoints.keys() {
        if !BUILTIN.iter().any(|row| row.name == name) {
            return Err(config_error(format!("endpoints names an unknown source {name:?}")));
        }
    }
    let mut sources: Vec<Arc<Source>> = Vec::new();
    for row in BUILTIN {
        let base = base_url(row.name, config.endpoints.get(row.name).map_or(row.base, String::as_str))?;
        let meta = SourceMeta { name: row.name.into(), authority: next(&sources), lawful: row.lawful, public: true };
        sources.push(Arc::new(Source { meta: Arc::new(meta), default_on: row.default_on, base, adapter: row.adapter }));
    }
    for TorznabConfig { name, url, api_key } in &config.torznab {
        let valid = !name.is_empty()
            && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.".contains(&b));
        if !valid {
            return Err(config_error(format!(
                "torznab source name {name:?} must be lowercase letters, digits, '-', '_', or '.'"
            )));
        }
        if sources.iter().any(|s| s.meta.name == *name) {
            return Err(config_error(format!("source name {name:?} is used twice")));
        }
        let meta = SourceMeta { name: name.clone(), authority: next(&sources), lawful: false, public: false };
        let base = url::Url::parse(url)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https"))
            .ok_or_else(|| config_error(format!("torznab source {name:?} needs an http or https URL")))?;
        let adapter = Adapter::Torznab { api_key: api_key.clone() };
        sources.push(Arc::new(Source { meta: Arc::new(meta), default_on: true, base, adapter }));
    }
    for name in &config.disabled {
        if !sources.iter().any(|s| s.meta.name == *name) {
            return Err(config_error(format!("sources.disabled names an unknown source {name:?}")));
        }
    }
    sources.retain(|s| !config.disabled.contains(&s.meta.name));
    Ok(sources)
}

fn next(sources: &[Arc<Source>]) -> u16 {
    u16::try_from(sources.len()).unwrap_or(u16::MAX)
}

fn base_url(name: &str, raw: &str) -> Result<Url, Error> {
    let mut url = Url::parse(raw)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .ok_or_else(|| config_error(format!("the endpoint for {name} must be an http or https URL")))?;
    // Relative joins replace the last path segment unless the base ends in a slash.
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

fn config_error(message: String) -> Error {
    Error::new(ErrorCode::Usage, message).with_hint("fix the [sources] section of the torrential config")
}

/// Joins a fixed relative path onto a validated `http(s)` base, which cannot fail.
fn at(base: &Url, path: &str) -> Url {
    base.join(path).expect("an http base accepts a relative path")
}

#[cfg(test)]
pub(crate) mod fixture {
    use std::path::Path;

    pub fn read(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }
}
