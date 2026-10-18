//! Official `.torrent` files listed in Apache directory indexes, as
//! releases.ubuntu.com and cdimage.debian.org serve them.

use std::time::Duration;

use futures_util::{StreamExt, stream};
use tokio::time::Instant;
use torrential_core::{Category, Flag};
use url::Url;

use super::{Ctx, Failure};
use crate::metainfo::Metainfo;
use crate::observation::{Identity, Observation};

pub(super) const LISTING_CAP: usize = 1 << 20;

#[derive(Clone, PartialEq, Debug)]
pub(super) struct Entry {
    pub file: String,
    /// Unix seconds, from the listing's "Last modified" column.
    pub modified: Option<i64>,
}

impl Entry {
    pub fn name(&self) -> &str {
        self.file.strip_suffix(".torrent").unwrap_or(&self.file)
    }
}

/// Relative links in a listing, in page order.
pub(super) fn hrefs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find("href=\"") {
        rest = &rest[at + 6..];
        let Some(end) = rest.find('"') else { break };
        let link = rest[..end].replace("&amp;", "&");
        if !link.contains("://") && !link.starts_with(['/', '?', '#']) && !out.contains(&link) {
            out.push(link);
        }
        rest = &rest[end..];
    }
    out
}

/// The `.torrent` files in a listing, one per table row.
pub(super) fn torrents(html: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for row in html.split("<tr").skip(1) {
        let Some(file) = hrefs(row).into_iter().find(|h| h.ends_with(".torrent") && !h.contains('/')) else {
            continue;
        };
        if out.iter().any(|e| e.file == file) {
            continue;
        }
        let modified = cells(row).find_map(|cell| crate::dates::civil(&cell));
        out.push(Entry { file, modified });
    }
    out
}

fn cells(row: &str) -> impl Iterator<Item = String> + '_ {
    row.split("<td").skip(1).map(|cell| {
        let text = cell.split_once('>').map_or("", |(_, t)| t);
        let text = text.split("</td>").next().unwrap_or("");
        let mut plain = String::new();
        let mut in_tag = false;
        for c in text.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                c if !in_tag => plain.push(c),
                _ => {}
            }
        }
        plain.trim().to_string()
    })
}

/// Fetches every listing in `dirs` at once. Fails only when all of them fail.
pub(super) async fn fetch_all(ctx: &Ctx<'_>, dirs: Vec<Url>) -> Result<Vec<(Url, Vec<Entry>)>, Failure> {
    let pages = futures_util::future::join_all(dirs.into_iter().map(|dir| async move {
        let page = ctx.http.get(&dir, LISTING_CAP, ctx.deadline).await;
        (dir, page)
    }))
    .await;
    let mut first_error = None;
    let mut listings = Vec::new();
    for (dir, page) in pages {
        match page {
            Ok(body) => listings.push((dir, torrents(&String::from_utf8_lossy(&body)))),
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    match first_error {
        Some(e) if listings.is_empty() => Err(e.into()),
        _ => Ok(listings),
    }
}

/// Turns listed files into official releases. Each `.torrent` is fetched and hashed
/// so the release gets a real ID; one that cannot be read in time keeps its URL as
/// a locator.
pub(super) async fn resolve(ctx: &Ctx<'_>, files: Vec<(Url, Entry)>) -> Vec<Observation> {
    const CONCURRENT: usize = 3;
    // Leaves time to return the locators when the torrents are slow.
    let deadline = ctx.deadline.checked_sub(Duration::from_millis(300)).unwrap_or(ctx.deadline);
    stream::iter(files)
        .map(|(dir, entry)| async move {
            let url = super::at(&dir, &entry.file);
            let meta = tokio::time::timeout_at(deadline, metainfo(ctx, &url, deadline)).await.ok().flatten();
            official(entry, &dir, url, meta)
        })
        .buffer_unordered(CONCURRENT)
        .filter_map(|o| async move { o })
        .collect()
        .await
}

fn official(entry: Entry, dir: &Url, url: Url, meta: Option<Metainfo>) -> Option<Observation> {
    let torrent_url = url.to_string();
    let (identity, size, trackers) = match meta {
        Some(Metainfo { hash, size, trackers }) => {
            (Identity::Hash { hash, torrent_url: Some(torrent_url) }, size, trackers)
        }
        None => (Identity::Locator { torrent_url }, None, Vec::new()),
    };
    let mut obs = Observation::new(identity, entry.name())?.flag(Flag::Official, true);
    obs.size = size;
    obs.published = entry.modified;
    obs.category = Some(Category::Software);
    obs.trackers = trackers;
    obs.info_url = Some(dir.to_string());
    Some(obs)
}

async fn metainfo(ctx: &Ctx<'_>, url: &Url, deadline: Instant) -> Option<Metainfo> {
    const CAP: usize = 2 << 20;
    const KEEP: usize = 256;
    let key = url.to_string();
    if let Some(known) = ctx.stores.torrents.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Some(known.clone());
    }
    let body = ctx.http.get(url, CAP, deadline).await.ok()?;
    let meta = crate::metainfo::parse(&body).ok()?;
    let mut torrents = ctx.stores.torrents.lock().unwrap_or_else(|e| e.into_inner());
    if torrents.len() >= KEEP {
        torrents.clear();
    }
    torrents.insert(key, meta.clone());
    Some(meta)
}
