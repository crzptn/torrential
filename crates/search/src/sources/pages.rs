//! The pages official sources list their releases on, kept on disk by URL. They change a
//! few times a year and each takes over a second to fetch, so a search reads them from disk
//! and fetches a copy that has gone stale in the background.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use sha1::{Digest, Sha1};
use tokio::time::Instant;
use url::Url;

use super::{Ctx, Failure};
use crate::http::Http;
use crate::observation::hex;

/// How long a copy answers with no request at all. Distributions publish a few images a
/// year, so one that shows up a day late costs little.
const FRESH_FOR: Duration = Duration::from_secs(24 * 60 * 60);
/// A copy this old is fetched again before answering, so an engine unused for weeks does not
/// offer last month's images.
const REFETCH_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const REFRESH_WITHIN: Duration = Duration::from_secs(30);

/// Whether a body is worth keeping: a block page served with status 200 is not.
pub(super) type Check = fn(&[u8]) -> bool;

#[derive(Clone)]
pub(crate) struct Pages {
    dir: Option<PathBuf>,
    refreshing: Arc<Mutex<HashSet<Url>>>,
}

impl Pages {
    pub fn new(cache_dir: Option<PathBuf>) -> Pages {
        Pages { dir: cache_dir.map(|d| d.join("pages")), refreshing: Arc::default() }
    }

    /// The body at `url`, from disk while the copy there is a day old or younger. An older copy
    /// still answers, marked partial so the answer is not cached, while it is fetched again in
    /// the background. A `--fresh` search always fetches.
    pub async fn get(&self, ctx: &Ctx<'_>, url: &Url, cap: usize, check: Check) -> Result<Vec<u8>, Failure> {
        let kept = if ctx.fresh { None } else { self.read(url, check).await };
        match kept {
            Some((age, body)) if age < FRESH_FOR => return Ok(body),
            Some((age, body)) if age < REFETCH_AFTER => {
                self.refresh(ctx.http, url, cap, check);
                ctx.mark_partial();
                return Ok(body);
            }
            _ => {}
        }
        match (ctx.http.get(url, cap, ctx.deadline).await, kept) {
            (Ok(body), _) if check(&body) => {
                self.write(url, &body).await;
                Ok(body)
            }
            (_, Some((_, old))) => {
                ctx.mark_partial();
                Ok(old)
            }
            // The source's own parser names what is wrong with a body that fails the check.
            (fetched, None) => Ok(fetched?),
        }
    }

    fn refresh(&self, http: &Http, url: &Url, cap: usize, check: Check) {
        if !self.lock().insert(url.clone()) {
            return;
        }
        let (pages, http, url) = (self.clone(), http.detached(), url.clone());
        tokio::spawn(async move {
            if let Ok(body) = http.get(&url, cap, Instant::now() + REFRESH_WITHIN).await
                && check(&body)
            {
                pages.write(&url, &body).await;
            }
            pages.lock().remove(&url);
        });
    }

    /// A damaged copy, or one from a clock set back, counts as none.
    async fn read(&self, url: &Url, check: Check) -> Option<(Duration, Vec<u8>)> {
        let path = self.dir.as_ref()?.join(file_name(url));
        let modified = tokio::fs::metadata(&path).await.ok()?.modified().ok()?;
        let body = tokio::fs::read(&path).await.ok()?;
        let age = SystemTime::now().duration_since(modified).unwrap_or(Duration::MAX);
        check(&body).then_some((age, body))
    }

    /// Best effort, like the hash store: a read-only cache directory only costs fetches.
    async fn write(&self, url: &Url, body: &[u8]) {
        // A --fresh search and a background refresh can write the same page at once.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let Some(dir) = &self.dir else { return };
        let name = file_name(url);
        let tmp = dir.join(format!("{name}.{}.{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let written = async {
            tokio::fs::create_dir_all(dir).await?;
            tokio::fs::write(&tmp, body).await?;
            tokio::fs::rename(&tmp, dir.join(&name)).await
        };
        if written.await.is_err() {
            let _ = tokio::fs::remove_file(&tmp).await;
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashSet<Url>> {
        self.refreshing.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn file_name(url: &Url) -> String {
    hex(&Sha1::digest(url.as_str().as_bytes()))
}
