//! Turns a [`SearchQuery`] into a [`SearchResult`]: every selected source runs at
//! once under the query's deadline, listings merge by torrent, and one ranking
//! orders them, so a human and an agent see the same list.
//!
//! The engine process owns one [`Search`] and one [`ResultCache`]. Search never
//! starts a download.

mod assemble;
mod cache;
mod classify;
mod dates;
mod http;
mod metainfo;
mod observation;
mod sources;
mod terms;

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout_at};
use torrential_core::{Error, ErrorCode, SearchQuery, SearchResult};

use crate::assemble::{Answer, Run, assemble, vet};
use crate::cache::AnswerKey;
pub use crate::cache::ResultCache;
use crate::http::Http;
use crate::sources::{Ctx, Failure, Source, Stores};
use crate::terms::Terms;

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchConfig {
    /// Where the Academic Torrents catalog is kept between processes. `None` keeps
    /// it in memory only.
    pub cache_dir: Option<PathBuf>,
    /// Searched after the built-in sources, in this order.
    pub torznab: Vec<TorznabConfig>,
    /// Replacement base URLs for built-in sources, by source name, such as a
    /// mirror. torrential never adds mirrors on its own.
    pub endpoints: BTreeMap<String, String>,
}

/// A Jackett, Prowlarr, or bitmagnet Torznab endpoint, such as
/// `http://127.0.0.1:9117/api/v2.0/indexers/all/results/torznab/api`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TorznabConfig {
    /// The `--source` name: lowercase letters, digits, `-`, `_`, and `.`.
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub api_key: Option<String>,
}

impl fmt::Debug for TorznabConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TorznabConfig")
            .field("name", &self.name)
            .field("url", &self.url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct SourceInfo {
    pub name: String,
    /// Searched when a query names no sources.
    pub default_on: bool,
    /// A catalog of content its publishers share, ranked ahead of general indexes.
    pub lawful: bool,
}

#[derive(Clone)]
pub struct Search {
    inner: Arc<Inner>,
}

struct Inner {
    http: Http,
    sources: Vec<Arc<Source>>,
    stores: Stores,
    cache: ResultCache,
}

impl Search {
    /// Fails with `usage` when the config names an unknown source or a bad URL.
    pub fn new(config: &SearchConfig, cache: ResultCache) -> Result<Search, Error> {
        Inner::new(config, cache, false).map(|inner| Search { inner: Arc::new(inner) })
    }

    /// Every source, most authoritative first.
    pub fn sources(&self) -> Vec<SourceInfo> {
        self.inner
            .sources
            .iter()
            .map(|s| SourceInfo { name: s.meta.name.clone(), default_on: s.default_on, lawful: s.meta.lawful })
            .collect()
    }

    /// Returns by the query's deadline. A source that has not answered by then is
    /// reported as `timeout`. Fails with `providers_unavailable` when no source
    /// answered, and with `usage` for a query without words or an unknown source.
    pub async fn search(&self, query: &SearchQuery) -> Result<SearchResult, Error> {
        let started = Instant::now();
        let terms = Arc::new(Terms::parse(&query.text));
        if terms.is_empty() {
            return Err(
                Error::new(ErrorCode::Usage, "a search needs at least one word").with_hint("torrential search <words>")
            );
        }
        let deadline = started + Duration::from_millis(query.timeout_ms.into());
        let mut runs = Vec::new();
        let mut tasks = JoinSet::new();
        let mut pending = HashMap::new();
        for source in self.select(&query.sources)? {
            let key =
                AnswerKey { source: source.meta.name.clone(), text: terms.text().to_string(), adult: query.adult };
            if !query.fresh
                && let Some(rows) = self.inner.cache.answer(&key, std::time::Instant::now())
            {
                runs.push(Run { source: source.meta.clone(), answer: Answer::Cached(rows), elapsed_ms: 0 });
                continue;
            }
            let meta = source.meta.clone();
            let (inner, terms, adult) = (self.inner.clone(), terms.clone(), query.adult);
            let task = tasks.spawn(async move { inner.run(&source, &terms, adult, deadline).await });
            pending.insert(task.id(), (meta, key));
        }
        while let Some(joined) = tasks.join_next_with_id().await {
            let (id, answer) = match joined {
                Ok((id, answer)) => (id, answer),
                // A parser bug on hostile input costs that source, not the search.
                Err(e) => (e.id(), Answer::Failed(Failure::BadResponse("the source adapter crashed".into()))),
            };
            let Some((meta, key)) = pending.remove(&id) else {
                continue;
            };
            if let Answer::Fresh(rows) = &answer {
                self.inner.cache.store(key, rows.clone(), std::time::Instant::now());
            }
            runs.push(Run { source: meta, answer, elapsed_ms: millis(started.elapsed()) });
        }
        let result = assemble(query, &terms, &runs, millis(started.elapsed()))?;
        self.inner.cache.remember(&result.releases);
        Ok(result)
    }

    fn select(&self, names: &[String]) -> Result<Vec<Arc<Source>>, Error> {
        let all = &self.inner.sources;
        if names.is_empty() {
            return Ok(all.iter().filter(|s| s.default_on).cloned().collect());
        }
        let mut chosen: Vec<Arc<Source>> = Vec::new();
        for name in names {
            let Some(source) = all.iter().find(|s| s.meta.name.eq_ignore_ascii_case(name.trim())) else {
                let known: Vec<&str> = all.iter().map(|s| s.meta.name.as_str()).collect();
                return Err(Error::new(ErrorCode::Usage, format!("unknown source {name:?}"))
                    .with_hint(format!("sources are {}", known.join(", "))));
            };
            if !chosen.iter().any(|c| Arc::ptr_eq(c, source)) {
                chosen.push(source.clone());
            }
        }
        Ok(chosen)
    }
}

impl Inner {
    fn new(config: &SearchConfig, cache: ResultCache, record: bool) -> Result<Inner, Error> {
        let http = Http::new(record).map_err(|e| Error::internal(format!("cannot build the HTTP client: {e}")))?;
        let sources = sources::registry(config)?;
        Ok(Inner { http, sources, stores: Stores::new(config.cache_dir.clone()), cache })
    }

    async fn run(&self, source: &Source, terms: &Terms, adult: bool, deadline: Instant) -> Answer {
        let ctx = Ctx { http: &self.http, stores: &self.stores, terms, adult, deadline };
        let rows = timeout_at(deadline, source.search(&ctx)).await.unwrap_or(Err(Failure::Timeout));
        match rows.and_then(|rows| vet(terms, rows)) {
            Ok(rows) => Answer::Fresh(Arc::new(rows)),
            Err(failure) => Answer::Failed(failure),
        }
    }
}

fn millis(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}

/// Runs one source the way a search does, with a 60-second deadline, and returns
/// every response body it received by URL, plus how the source fared. The
/// `record` example writes these as test fixtures.
#[doc(hidden)]
pub async fn record(config: &SearchConfig, source: &str, query: &str) -> Result<(String, http::Recorded), Error> {
    let inner = Inner::new(config, ResultCache::new(), true)?;
    let source = inner
        .sources
        .iter()
        .find(|s| s.meta.name == source)
        .ok_or_else(|| Error::new(ErrorCode::Usage, format!("unknown source {source:?}")))?;
    let terms = Terms::parse(query);
    let ctx = Ctx {
        http: &inner.http,
        stores: &inner.stores,
        terms: &terms,
        adult: false,
        deadline: Instant::now() + Duration::from_secs(60),
    };
    let outcome = match source.search(&ctx).await {
        Ok(rows) => format!("{} rows", rows.len()),
        Err(failure) => format!("{failure:?}"),
    };
    Ok((outcome, inner.http.take_recorded()))
}
