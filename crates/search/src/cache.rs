use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use torrential_core::{HexPrefix, Release, ReleaseId};

use crate::observation::Observation;

/// Knaben's CDN caches searches for the same five minutes.
const TTL: Duration = Duration::from_secs(5 * 60);
const MAX_ANSWERS: usize = 512;
const MAX_RECENT: usize = 2048;

/// Each source's answer to a query, kept for five minutes, and every release a
/// search returned, so a short ID from a search resolves later.
///
/// Answers are kept per source and before filtering: a later search with the same
/// words reuses every source that answered, applies its own filters, sort, and
/// limit, and fetches only the sources that failed. Clones share one cache, so it
/// can outlive a `Search` rebuilt from new config.
#[derive(Clone, Default)]
pub struct ResultCache {
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    answers: HashMap<AnswerKey, (Instant, Arc<Vec<Observation>>)>,
    recent: HashMap<ReleaseId, (u64, Release)>,
    seq: u64,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct AnswerKey {
    pub source: String,
    /// A `Search` rebuilt with a new endpoint for the source must not reuse old answers.
    pub endpoint: String,
    pub text: String,
    pub adult: bool,
}

impl ResultCache {
    pub fn new() -> ResultCache {
        ResultCache::default()
    }

    /// Releases from recent searches whose ID starts with `prefix`, newest first.
    /// More than one match means the prefix is ambiguous.
    ///
    /// Unlike a `SearchResult`, these keep links from private sources, which can
    /// carry a Torznab API key or a tracker passkey: they are for downloading,
    /// not for printing.
    pub fn find(&self, prefix: &HexPrefix) -> Vec<Release> {
        let state = self.lock();
        let mut found: Vec<&(u64, Release)> = state.recent.values().filter(|(_, r)| prefix.matches(&r.id)).collect();
        found.sort_by_key(|(seq, _)| std::cmp::Reverse(*seq));
        found.into_iter().map(|(_, r)| r.clone()).collect()
    }

    pub(crate) fn answer(&self, key: &AnswerKey, now: Instant) -> Option<Arc<Vec<Observation>>> {
        let state = self.lock();
        let (at, rows) = state.answers.get(key)?;
        (now.saturating_duration_since(*at) < TTL).then(|| rows.clone())
    }

    pub(crate) fn store(&self, key: AnswerKey, rows: Arc<Vec<Observation>>, now: Instant) {
        let mut state = self.lock();
        if state.answers.len() >= MAX_ANSWERS {
            state.answers.retain(|_, (at, _)| now.saturating_duration_since(*at) < TTL);
        }
        if state.answers.len() >= MAX_ANSWERS
            && let Some(oldest) = state.answers.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| k.clone())
        {
            state.answers.remove(&oldest);
        }
        state.answers.insert(key, (now, rows));
    }

    pub(crate) fn remember(&self, releases: &[Release]) {
        let mut state = self.lock();
        for release in releases {
            state.seq += 1;
            let seq = state.seq;
            state.recent.insert(release.id, (seq, release.clone()));
        }
        if state.recent.len() > MAX_RECENT {
            let mut seqs: Vec<u64> = state.recent.values().map(|(seq, _)| *seq).collect();
            seqs.sort_unstable();
            let cutoff = seqs[seqs.len() - MAX_RECENT];
            state.recent.retain(|_, (seq, _)| *seq >= cutoff);
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::Identity;

    fn rows(name: &str) -> Arc<Vec<Observation>> {
        let hash = crate::observation::infohash(&"ab".repeat(20)).unwrap();
        Arc::new(vec![Observation::new(Identity::Hash { hash, torrent_url: None }, name).unwrap()])
    }

    #[test]
    fn answers_expire_after_five_minutes() {
        let cache = ResultCache::new();
        let key = AnswerKey {
            source: "knaben".into(),
            endpoint: "https://api.knaben.org/".into(),
            text: "ubuntu".into(),
            adult: false,
        };
        let t0 = Instant::now();
        cache.store(key.clone(), rows("Ubuntu 24.04"), t0);
        let hit = cache.answer(&key, t0 + Duration::from_secs(299)).map(|r| r[0].name().to_string());
        assert_eq!(hit.as_deref(), Some("Ubuntu 24.04"));
        assert!(cache.answer(&key, t0 + Duration::from_secs(301)).is_none());
        let adult = AnswerKey { adult: true, ..key };
        assert!(cache.answer(&adult, t0).is_none());
    }
}
