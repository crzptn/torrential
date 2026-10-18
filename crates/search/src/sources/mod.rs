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
use std::sync::{Arc, Mutex};

use tokio::time::Instant;
use torrential_core::{Error, ErrorCode, SourceOutcome};
use url::Url;

use crate::http::{FetchError, Http};
use crate::metainfo::Metainfo;
use crate::observation::Observation;
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
    Row {
        name: "knaben",
        default_on: true,
        lawful: false,
        base: "https://api.knaben.org/",
        adapter: Adapter::Knaben,
    },
    Row {
        name: "apibay",
        default_on: true,
        lawful: false,
        base: "https://apibay.org/",
        adapter: Adapter::Apibay,
    },
    Row {
        name: "nyaa",
        default_on: true,
        lawful: false,
        base: "https://nyaa.si/",
        adapter: Adapter::Nyaa,
    },
];

#[derive(Clone, Debug)]
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

/// What merging and ranking need to know about a source.
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

/// Why a source gave no usable answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Failure {
    Timeout,
    Http(u16),
    Connect(String),
    BadResponse(String),
    Irrelevant,
}

impl Failure {
    pub fn outcome(&self) -> SourceOutcome {
        match self {
            Self::Timeout => SourceOutcome::Timeout,
            Self::Http(status) => SourceOutcome::Http { status: *status },
            Self::Connect(reason) => SourceOutcome::Connect { reason: reason.clone() },
            Self::BadResponse(reason) => SourceOutcome::BadResponse { reason: reason.clone() },
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
        Stores {
            torrents: Mutex::default(),
            academic: academic::Catalog::new(cache_dir),
        }
    }
}

/// Everything an adapter may use during one search.
pub(crate) struct Ctx<'a> {
    pub http: &'a Http,
    pub stores: &'a Stores,
    pub terms: &'a Terms,
    pub adult: bool,
    pub deadline: Instant,
}

impl Source {
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

/// Builds the registry: the built-in rows, then the configured Torznab endpoints.
pub(crate) fn registry(config: &SearchConfig) -> Result<Vec<Arc<Source>>, Error> {
    for name in config.endpoints.keys() {
        if !BUILTIN.iter().any(|row| row.name == name) {
            return Err(config_error(format!("endpoints names an unknown source {name:?}")));
        }
    }
    let mut sources = Vec::new();
    for row in BUILTIN {
        let base = config.endpoints.get(row.name).map_or(row.base, String::as_str);
        let meta = SourceMeta {
            name: row.name.into(),
            authority: 0,
            lawful: row.lawful,
            public: true,
        };
        sources.push((meta, row.default_on, base_url(row.name, base)?, row.adapter));
    }
    for TorznabConfig { name, url, api_key } in &config.torznab {
        let valid = !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.".contains(&b));
        if !valid {
            return Err(config_error(format!(
                "torznab source name {name:?} must be lowercase letters, digits, '-', '_', or '.'"
            )));
        }
        if sources.iter().any(|(meta, ..)| meta.name == *name) {
            return Err(config_error(format!("source name {name:?} is used twice")));
        }
        let meta = SourceMeta {
            name: name.clone(),
            authority: 0,
            lawful: false,
            public: false,
        };
        let url = url::Url::parse(url)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https"))
            .ok_or_else(|| config_error(format!("torznab source {name:?} needs an http or https URL")))?;
        sources.push((
            meta,
            true,
            url,
            Adapter::Torznab {
                api_key: api_key.clone(),
            },
        ));
    }
    Ok(sources
        .into_iter()
        .enumerate()
        .map(|(i, (mut meta, default_on, base, adapter))| {
            meta.authority = u16::try_from(i).unwrap_or(u16::MAX);
            Arc::new(Source {
                meta: Arc::new(meta),
                default_on,
                base,
                adapter,
            })
        })
        .collect())
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
    Error::new(ErrorCode::Usage, message).with_hint("fix the [search] section of the torrential config")
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
