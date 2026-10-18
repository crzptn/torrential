//! The HTTP client every source shares.

use std::sync::Mutex;
use std::time::Duration;

use tokio::time::{Instant, timeout_at};
use url::Url;

pub(crate) const USER_AGENT: &str = concat!("torrential/", env!("CARGO_PKG_VERSION"));

/// Response bodies by URL, in arrival order.
pub type Recorded = Vec<(String, Vec<u8>)>;

pub(crate) struct Http {
    client: reqwest::Client,
    /// Every body received, by URL, when recording fixtures.
    recorded: Option<Mutex<Recorded>>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum FetchError {
    Timeout,
    Status(u16),
    Connect(String),
    /// The body passed the cap. Reported as an error, never parsed truncated.
    TooLarge(usize),
    Decode(String),
}

impl FetchError {
    fn retryable(&self) -> bool {
        match self {
            Self::Timeout => true,
            Self::Status(status) => *status >= 500,
            Self::Connect(_) | Self::TooLarge(_) | Self::Decode(_) => false,
        }
    }
}

impl Http {
    pub fn new(record: bool) -> Result<Http, reqwest::Error> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .gzip(true)
            .brotli(true)
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        Ok(Http {
            client,
            recorded: record.then(Mutex::default),
        })
    }

    /// GETs `url`, giving up at `deadline`. A timeout or a 5xx is retried once when
    /// time remains: the first attempt gets three fifths of the time left so that a
    /// stalled connection still leaves room for the retry.
    pub async fn get(&self, url: &Url, cap: usize, deadline: Instant) -> Result<Vec<u8>, FetchError> {
        const RETRY_PAUSE: Duration = Duration::from_millis(200);
        const MIN_RETRY: Duration = Duration::from_millis(600);
        let first_deadline = Instant::now() + deadline.saturating_duration_since(Instant::now()) * 3 / 5;
        let first = self.attempt(url, cap, first_deadline).await;
        let body = match first {
            Err(e) if e.retryable() && deadline.saturating_duration_since(Instant::now()) > MIN_RETRY => {
                if matches!(e, FetchError::Status(_)) {
                    tokio::time::sleep(RETRY_PAUSE).await;
                }
                self.attempt(url, cap, deadline).await
            }
            other => other,
        }?;
        if let Some(recorded) = &self.recorded {
            recorded
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((url.to_string(), body.clone()));
        }
        Ok(body)
    }

    pub fn take_recorded(&self) -> Recorded {
        self.recorded
            .as_ref()
            .map(|r| std::mem::take(&mut *r.lock().unwrap_or_else(|e| e.into_inner())))
            .unwrap_or_default()
    }

    async fn attempt(&self, url: &Url, cap: usize, deadline: Instant) -> Result<Vec<u8>, FetchError> {
        let fetch = async {
            let mut response = self.client.get(url.clone()).send().await.map_err(classify)?;
            let status = response.status();
            if !status.is_success() {
                return Err(FetchError::Status(status.as_u16()));
            }
            if response.content_length().is_some_and(|len| len > cap as u64) {
                return Err(FetchError::TooLarge(cap));
            }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(classify)? {
                if body.len() + chunk.len() > cap {
                    return Err(FetchError::TooLarge(cap));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(body)
        };
        timeout_at(deadline, fetch).await.unwrap_or(Err(FetchError::Timeout))
    }
}

fn classify(err: reqwest::Error) -> FetchError {
    if err.is_timeout() {
        return FetchError::Timeout;
    }
    let decode = err.is_decode();
    // Drop the URL: a Torznab URL carries the user's API key.
    let err = err.without_url();
    let mut parts: Vec<String> = Vec::new();
    let mut cause: Option<&dyn std::error::Error> = Some(&err);
    while let Some(e) = cause {
        let text = e.to_string();
        if !parts.iter().any(|p| p.contains(&text)) {
            parts.push(text);
        }
        cause = e.source();
    }
    let reason = torrential_core::text::truncate(&torrential_core::text::sanitize(&parts.join(": ")), 160);
    if decode {
        FetchError::Decode(reason)
    } else {
        FetchError::Connect(reason)
    }
}
