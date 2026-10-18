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

use std::collections::BTreeMap;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::stream::FuturesUnordered;
use futures_util::{FutureExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, timeout_at};
use torrential_core::{Error, ErrorCode, SearchQuery, SearchResult, text};
use url::Url;

use crate::assemble::{Answer, Run, assemble, merged, vet};
use crate::cache::AnswerKey;
pub use crate::cache::ResultCache;
use crate::http::{FetchError, Http, Retry};
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
    /// Sources never searched, by name. A query that names one fails.
    pub disabled: Vec<String>,
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

/// A search under way: the sources that have answered, merged and ranked the way
/// the final result is, and the sources still to answer, most authoritative first.
#[derive(Clone, PartialEq, Debug)]
pub struct Progress {
    pub result: SearchResult,
    pub pending: Vec<String>,
}

#[derive(Clone)]
pub struct Search {
    inner: Arc<Inner>,
}

struct Inner {
    http: Http,
    /// The enabled sources.
    sources: Vec<Arc<Source>>,
    disabled: Vec<String>,
    stores: Stores,
    cache: ResultCache,
}

impl Search {
    /// Fails with `usage` when the config names an unknown source or a bad URL.
    pub fn new(config: &SearchConfig, cache: ResultCache) -> Result<Search, Error> {
        Inner::new(config, cache, false).map(|inner| Search { inner: Arc::new(inner) })
    }

    /// Every enabled source, most authoritative first.
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
        self.search_with_progress(query, |_| {}).await
    }

    /// Like [`Search::search`], and calls `progress` each time sources answer while
    /// others are still pending. Answers that arrive together make one call.
    pub async fn search_with_progress(
        &self,
        query: &SearchQuery,
        mut progress: impl FnMut(Progress) + Send,
    ) -> Result<SearchResult, Error> {
        let started = Instant::now();
        let terms = Terms::parse(&query.text);
        if terms.is_empty() {
            return Err(
                Error::new(ErrorCode::Usage, "a search needs at least one word").with_hint("torrential search <words>")
            );
        }
        let deadline = started + Duration::from_millis(query.timeout_ms.into());
        let cache = &self.inner.cache;
        let selected = self.select(&query.sources)?;
        let lookups = selected.iter().cloned().map(|source| {
            let key = AnswerKey {
                source: source.meta.name.clone(),
                scope: source.scope(),
                text: terms.text().to_string(),
                adult: query.adult,
            };
            let cached = if query.fresh { None } else { cache.answer(&key, std::time::Instant::now()) };
            let terms = &terms;
            async move {
                if let Some(rows) = cached {
                    return Run { source: source.meta.clone(), answer: Answer::Cached(rows), elapsed_ms: 0 };
                }
                let (answer, complete) = AssertUnwindSafe(self.inner.run(&source, terms, query.adult, deadline))
                    .catch_unwind()
                    .await
                    // A parser bug on hostile input costs that source, not the search.
                    .unwrap_or_else(|_| {
                        (Answer::Failed(Failure::BadResponse("the source adapter crashed".into())), false)
                    });
                if let Answer::Fresh(rows) = &answer
                    && complete
                {
                    cache.store(key, rows.clone(), std::time::Instant::now());
                }
                Run { source: source.meta.clone(), answer, elapsed_ms: millis(started.elapsed()) }
            }
        });
        let mut lookups: FuturesUnordered<_> = lookups.collect();
        let mut runs: Vec<Run> = Vec::with_capacity(selected.len());
        while let Some(run) = lookups.next().await {
            runs.push(run);
            while let Some(Some(run)) = lookups.next().now_or_never() {
                runs.push(run);
            }
            if runs.len() < selected.len() {
                let (result, linked) = merged(query, &terms, &runs, millis(started.elapsed()));
                cache.remember(&linked);
                let pending = selected
                    .iter()
                    .filter(|s| !runs.iter().any(|r| r.source.name == s.meta.name))
                    .map(|s| s.meta.name.clone())
                    .collect();
                progress(Progress { result, pending });
            }
        }
        let (result, linked) = assemble(query, &terms, &runs, millis(started.elapsed()))?;
        cache.remember(&linked);
        Ok(result)
    }

    /// The `.torrent` file at `url`, which may be a private link from
    /// [`ResultCache::find`], so errors never quote it.
    pub async fn fetch_torrent(&self, url: &str) -> Result<Vec<u8>, Error> {
        const CAP: usize = 16 << 20;
        const WITHIN: Duration = Duration::from_secs(20);
        let url = Url::parse(url)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https"))
            .ok_or_else(|| Error::new(ErrorCode::InvalidRef, "a .torrent URL must start with http:// or https://"))?;
        // A private tracker may count each request for a .torrent as a grab.
        let body = self.inner.http.fetch(&url, CAP, Instant::now() + WITHIN, Retry::Never).await;
        body.map_err(|e| {
            let why = match e {
                FetchError::Timeout => format!("no answer within {} seconds", WITHIN.as_secs()),
                FetchError::Status(status) => format!("http {status}"),
                FetchError::Connect(reason) | FetchError::Decode(reason) => {
                    text::truncate(&text::sanitize(&reason), 160)
                }
                FetchError::TooLarge(cap) => format!("larger than {} MiB", cap >> 20),
            };
            Error::new(ErrorCode::FetchFailed, format!("could not fetch the .torrent file: {why}"))
                .with_hint("check the network connection, or download by magnet link")
        })
    }

    fn select(&self, names: &[String]) -> Result<Vec<Arc<Source>>, Error> {
        let all = &self.inner.sources;
        if names.is_empty() {
            return Ok(all.iter().filter(|s| s.default_on).cloned().collect());
        }
        let mut chosen: Vec<Arc<Source>> = Vec::new();
        for name in names {
            let Some(source) = all.iter().find(|s| s.meta.name.eq_ignore_ascii_case(name.trim())) else {
                if let Some(off) = self.inner.disabled.iter().find(|d| d.eq_ignore_ascii_case(name.trim())) {
                    return Err(Error::new(ErrorCode::Usage, format!("source {off:?} is disabled"))
                        .with_hint("remove it from sources.disabled in the torrential config"));
                }
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
        let disabled = config.disabled.clone();
        Ok(Inner { http, sources, disabled, stores: Stores::new(config.cache_dir.clone()), cache })
    }

    /// Also says whether the answer is complete enough to cache.
    async fn run(&self, source: &Source, terms: &Terms, adult: bool, deadline: Instant) -> (Answer, bool) {
        let ctx =
            Ctx { http: &self.http, stores: &self.stores, terms, adult, deadline, partial: AtomicBool::new(false) };
        let rows = timeout_at(deadline, source.search(&ctx)).await.unwrap_or(Err(Failure::Timeout));
        let answer = match rows.and_then(|rows| vet(terms, rows)) {
            Ok(rows) => Answer::Fresh(Arc::new(rows)),
            Err(failure) => Answer::Failed(failure),
        };
        (answer, !ctx.partial.load(Ordering::Relaxed))
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
        partial: AtomicBool::new(false),
    };
    let outcome = match source.search(&ctx).await {
        Ok(rows) => format!("{} rows", rows.len()),
        Err(failure) => format!("{failure:?}"),
    };
    Ok((outcome, inner.http.take_recorded()))
}
