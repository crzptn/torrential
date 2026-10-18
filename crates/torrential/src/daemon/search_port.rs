//! The seam between the engine process and the search service. `torrential-search`
//! implements [`SearchPort`] in the wiring round. Searches run in the connection's own
//! task, never in the owner, so a slow source cannot delay a pause.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use torrential_core::{Error, ErrorCode, Release, SearchQuery, SearchResult, SourceOutcome, SourceReport};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait SearchPort: Send + Sync {
    /// Ranked results. Fails with `providers_unavailable` only when every source failed.
    fn search(&self, query: SearchQuery) -> BoxFuture<'_, Result<SearchResult, Error>>;
    /// The bytes of a `.torrent` file a search result links to.
    fn fetch_torrent(&self, url: String) -> BoxFuture<'_, Result<Vec<u8>, Error>>;
}

pub struct Unwired;

fn unwired() -> Error {
    Error::internal("this build of torrential has no search service yet")
        .with_hint("install a release build of torrential")
}

impl SearchPort for Unwired {
    fn search(&self, _: SearchQuery) -> BoxFuture<'_, Result<SearchResult, Error>> {
        Box::pin(async { Err(unwired()) })
    }

    fn fetch_torrent(&self, _: String) -> BoxFuture<'_, Result<Vec<u8>, Error>> {
        Box::pin(async { Err(unwired()) })
    }
}

/// A search service with a fixed catalog, shared with tests so they can change it.
#[derive(Clone, Default)]
pub struct Fake(Arc<Mutex<FakeState>>);

#[derive(Default)]
struct FakeState {
    catalog: Vec<Release>,
    torrents: BTreeMap<String, Vec<u8>>,
    delay: Duration,
}

impl Fake {
    pub fn new(catalog: Vec<Release>) -> Self {
        let fake = Self::default();
        fake.0.lock().unwrap().catalog = catalog;
        fake
    }

    #[cfg(test)]
    pub fn serve_torrent(&self, url: &str, bytes: Vec<u8>) {
        self.0.lock().unwrap().torrents.insert(url.to_string(), bytes);
    }

    /// Makes every search take this long, to show it never holds up other commands.
    #[cfg(test)]
    pub fn set_delay(&self, delay: Duration) {
        self.0.lock().unwrap().delay = delay;
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

impl SearchPort for Fake {
    fn search(&self, query: SearchQuery) -> BoxFuture<'_, Result<SearchResult, Error>> {
        Box::pin(async move {
            let (catalog, delay) = {
                let state = self.0.lock().unwrap();
                (state.catalog.clone(), state.delay)
            };
            tokio::time::sleep(delay).await;
            let words: Vec<String> = query.text.split_whitespace().map(str::to_lowercase).collect();
            let matching: Vec<Release> = catalog
                .into_iter()
                .filter(|r| words.iter().all(|w| r.name.to_lowercase().contains(w.as_str())))
                .collect();
            let total = matching.len() as u32;
            let releases = matching.into_iter().take(query.limit as usize).collect();
            let sources = vec![SourceReport {
                source: "fake".into(),
                outcome: SourceOutcome::Ok { results: total },
                elapsed_ms: 1,
            }];
            Ok(SearchResult { query, releases, total, hidden_adult: 0, sources, elapsed_ms: 1 })
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
}
