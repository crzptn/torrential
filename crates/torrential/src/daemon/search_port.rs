//! The seam between the engine process and the search service. Searches run in the
//! connection's own task, never in the owner, so a slow source cannot delay a pause.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use torrential_core::{Error, ErrorCode, HexPrefix, Release, SearchQuery, SearchResult, SourceOutcome, SourceReport};
pub use torrential_search::Progress;
use torrential_search::{ResultCache, Search, SearchConfig};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait SearchPort: Send + Sync {
    /// Ranked results. Fails with `providers_unavailable` only when every source failed.
    fn search(&self, query: SearchQuery) -> BoxFuture<'_, Result<SearchResult, Error>>;
    /// Like [`SearchPort::search`], and sends the results so far each time a source answers.
    fn search_stream(
        &self,
        query: SearchQuery,
        progress: mpsc::UnboundedSender<Progress>,
    ) -> BoxFuture<'_, Result<SearchResult, Error>>;
    /// The bytes of a `.torrent` file a search result links to.
    fn fetch_torrent(&self, url: String) -> BoxFuture<'_, Result<Vec<u8>, Error>>;
    /// Web seeds on the servers that hold an archive.org torrent's files, to add to its own. `None`
    /// for a torrent that does not download from archive.org. See [`Search::mirrors`].
    fn mirrors(&self, name: &str, web_seeds: &[String]) -> Option<BoxFuture<'static, Result<Vec<String>, Error>>>;
    /// Releases from recent searches whose ID starts with `prefix`, newest first. They keep the
    /// links from private sources, which can carry an API key, so they are for downloading and
    /// never for printing.
    fn find(&self, prefix: &HexPrefix) -> Vec<Release>;
    /// Remembers what an earlier engine process showed, newest first. Called once, at start.
    fn restore(&self, releases: Vec<Release>);
}

/// The real search, with the one result cache the engine process owns.
pub struct Live {
    search: Search,
    cache: ResultCache,
}

impl Live {
    pub fn new(config: &SearchConfig) -> Result<Self, Error> {
        let cache = ResultCache::new();
        Ok(Self { search: Search::new(config, cache.clone())?, cache })
    }
}

impl SearchPort for Live {
    fn search(&self, query: SearchQuery) -> BoxFuture<'_, Result<SearchResult, Error>> {
        Box::pin(async move { self.search.search(&query).await })
    }

    fn search_stream(
        &self,
        query: SearchQuery,
        progress: mpsc::UnboundedSender<Progress>,
    ) -> BoxFuture<'_, Result<SearchResult, Error>> {
        Box::pin(async move {
            self.search
                .search_with_progress(&query, |update| {
                    let _ = progress.send(update);
                })
                .await
        })
    }

    fn fetch_torrent(&self, url: String) -> BoxFuture<'_, Result<Vec<u8>, Error>> {
        Box::pin(async move { self.search.fetch_torrent(&url).await })
    }

    fn mirrors(&self, name: &str, web_seeds: &[String]) -> Option<BoxFuture<'static, Result<Vec<String>, Error>>> {
        Some(Box::pin(self.search.mirrors(name, web_seeds)?))
    }

    fn find(&self, prefix: &HexPrefix) -> Vec<Release> {
        self.cache.find(prefix)
    }

    fn restore(&self, releases: Vec<Release>) {
        self.cache.restore(releases);
    }
}

#[derive(Clone, Default)]
pub struct Fake(Arc<Mutex<FakeState>>);

#[derive(Default)]
struct FakeState {
    catalog: Vec<Release>,
    torrents: BTreeMap<String, Vec<u8>>,
    delay: Duration,
    script: Vec<FakeSource>,
    /// What searches returned, newest first.
    recent: Vec<Release>,
}

/// One source of the fake search. It answers `after` a streaming search starts: with the
/// catalog releases it lists first, or with `failure`.
#[derive(Clone, Debug)]
pub struct FakeSource {
    pub name: String,
    pub after: Duration,
    pub failure: Option<SourceOutcome>,
}

impl FakeSource {
    pub fn answers(name: &str, after_ms: u64) -> Self {
        Self { name: name.into(), after: Duration::from_millis(after_ms), failure: None }
    }

    pub fn fails(name: &str, after_ms: u64, failure: SourceOutcome) -> Self {
        Self { name: name.into(), after: Duration::from_millis(after_ms), failure: Some(failure) }
    }
}

impl Fake {
    /// A fake with one source, `fake`, that answers at once.
    #[cfg(test)]
    pub fn new(catalog: Vec<Release>) -> Self {
        Self::scripted(catalog, vec![FakeSource::answers("fake", 0)], BTreeMap::new())
    }

    pub fn scripted(catalog: Vec<Release>, mut script: Vec<FakeSource>, torrents: BTreeMap<String, Vec<u8>>) -> Self {
        script.sort_by_key(|s| s.after);
        Self(Arc::new(Mutex::new(FakeState { catalog, torrents, delay: Duration::ZERO, script, recent: Vec::new() })))
    }

    #[cfg(test)]
    pub fn serve_torrent(&self, url: &str, bytes: Vec<u8>) {
        self.0.lock().unwrap().torrents.insert(url.to_string(), bytes);
    }

    #[cfg(test)]
    pub fn set_delay(&self, delay: Duration) {
        self.0.lock().unwrap().delay = delay;
    }

    fn parts(&self) -> (Vec<Release>, Vec<FakeSource>, Duration) {
        let state = self.0.lock().unwrap();
        (state.catalog.clone(), state.script.clone(), state.delay)
    }

    fn remember(&self, result: SearchResult) -> SearchResult {
        let mut state = self.0.lock().unwrap();
        state.recent.retain(|r| !result.releases.iter().any(|n| n.id == r.id));
        state.recent.splice(0..0, result.releases.iter().cloned());
        result
    }
}

/// What fake mode finds: magnets whose `xl` gives the fake engine a size to download.
pub fn fake_catalog() -> Vec<Release> {
    let release = |hex: &str, name: &str, size: u64, seeders: u32| Release {
        id: torrential_core::ReleaseId::Torrent(hex.parse().expect("catalog IDs are valid")),
        name: name.to_string(),
        size: Some(size),
        seeders: Some(seeders),
        leechers: None,
        published: None,
        category: Some(torrential_core::Category::Software),
        flags: Default::default(),
        sources: vec!["fake".into()],
        magnet: Some(format!("magnet:?xt=urn:btih:{hex}&dn={name}&xl={size}")),
        torrent_url: None,
        info_url: None,
    };
    vec![
        release("9b521219bc48550140a666f74a29048ed78e933b", "archlinux-2026.10.01-x86_64.iso", 1_640_497_152, 412),
        release("e3a1f06d2b1c8a4f5e6d7c8b9a0f1e2d3c4b5a69", "ubuntu-24.04.5-desktop-amd64.iso", 6_345_887_744, 1873),
    ]
}

/// The result once the `answered` sources have answered. Catalog order is the ranking.
fn compose(catalog: &[Release], answered: &[FakeSource], query: SearchQuery, elapsed: Duration) -> SearchResult {
    let words: Vec<String> = query.text.split_whitespace().map(str::to_lowercase).collect();
    let matches = |r: &Release| words.iter().all(|w| r.name.to_lowercase().contains(w.as_str()));
    let listed_by = |r: &Release, source: &FakeSource| r.sources.first() == Some(&source.name);
    let sources = answered
        .iter()
        .map(|source| {
            let outcome = source.failure.clone().unwrap_or_else(|| SourceOutcome::Ok {
                results: catalog.iter().filter(|r| listed_by(r, source) && matches(r)).count() as u32,
            });
            SourceReport { source: source.name.clone(), outcome, elapsed_ms: source.after.as_millis() as u32 }
        })
        .collect();
    let matching: Vec<Release> = catalog
        .iter()
        .filter(|r| matches(r) && answered.iter().any(|s| s.failure.is_none() && listed_by(r, s)))
        .cloned()
        .collect();
    let total = matching.len() as u32;
    let releases = matching.into_iter().take(query.limit as usize).collect();
    SearchResult { query, releases, total, hidden_adult: 0, sources, elapsed_ms: elapsed.as_millis() as u32 }
}

impl SearchPort for Fake {
    fn search(&self, query: SearchQuery) -> BoxFuture<'_, Result<SearchResult, Error>> {
        Box::pin(async move {
            let (catalog, script, delay) = self.parts();
            tokio::time::sleep(delay).await;
            Ok(self.remember(compose(&catalog, &script, query, delay)))
        })
    }

    fn search_stream(
        &self,
        query: SearchQuery,
        progress: mpsc::UnboundedSender<Progress>,
    ) -> BoxFuture<'_, Result<SearchResult, Error>> {
        Box::pin(async move {
            let (catalog, script, _) = self.parts();
            let started = tokio::time::Instant::now();
            for answered in 1..script.len() {
                tokio::time::sleep_until(started + script[answered - 1].after).await;
                let result = self.remember(compose(&catalog, &script[..answered], query.clone(), started.elapsed()));
                let pending = script[answered..].iter().map(|s| s.name.clone()).collect();
                let _ = progress.send(Progress { result, pending });
            }
            if let Some(last) = script.last() {
                tokio::time::sleep_until(started + last.after).await;
            }
            Ok(self.remember(compose(&catalog, &script, query, started.elapsed())))
        })
    }

    fn fetch_torrent(&self, url: String) -> BoxFuture<'_, Result<Vec<u8>, Error>> {
        Box::pin(async move {
            self.0.lock().unwrap().torrents.get(&url).cloned().ok_or_else(|| {
                Error::new(ErrorCode::FetchFailed, format!("could not fetch {url}: 404"))
                    .with_hint("try the magnet link instead")
            })
        })
    }

    fn mirrors(&self, _: &str, _: &[String]) -> Option<BoxFuture<'static, Result<Vec<String>, Error>>> {
        None
    }

    fn find(&self, prefix: &HexPrefix) -> Vec<Release> {
        self.0.lock().unwrap().recent.iter().filter(|r| prefix.matches(&r.id)).cloned().collect()
    }

    fn restore(&self, releases: Vec<Release>) {
        let mut state = self.0.lock().unwrap();
        for release in releases {
            if !state.recent.iter().any(|r| r.id == release.id) {
                state.recent.push(release);
            }
        }
    }
}
