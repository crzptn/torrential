//! Official `.torrent` files listed in Apache directory indexes, as
//! releases.ubuntu.com and cdimage.debian.org serve them.

use std::time::Duration;

use futures_util::{StreamExt, stream};
use tokio::time::Instant;
use torrential_core::{Category, Flag};
use url::Url;

use super::hashes::Key;
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
    // Apache and lighttpd put each file in a table row; nginx prints one per line.
    let rows: Vec<&str> =
        if html.contains("<tr") { html.split("<tr").skip(1).collect() } else { html.lines().collect() };
    for row in rows {
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

/// Fetches every listing in `dirs` at once. Fails only when all of them fail, and
/// marks the answer partial when some do.
pub(super) async fn fetch_all(ctx: &Ctx<'_>, dirs: Vec<Url>) -> Result<Vec<(Url, Vec<Entry>)>, Failure> {
    let pages = futures_util::future::join_all(dirs.into_iter().map(|dir| async move {
        let page = ctx.http.get(&dir, LISTING_CAP, ctx.deadline).await;
        (dir, page)
    }))
    .await;
    let mut first_error = None;
    let mut listings = Vec::new();
    for (dir, page) in pages {
        match page.map_err(Failure::from).and_then(|body| listing(&body)) {
            Ok(entries) => listings.push((dir, entries)),
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    match first_error {
        Some(e) if listings.is_empty() => Err(e),
        Some(_) => {
            ctx.mark_partial();
            Ok(listings)
        }
        None => Ok(listings),
    }
}

/// A block page or an error page served with status 200 must not read as an
/// empty directory. Apache's index links its column sorts, and nginx and lighttpd
/// title theirs "Index of".
pub(super) fn listing(body: &[u8]) -> Result<Vec<Entry>, Failure> {
    let html = String::from_utf8_lossy(body);
    if !html.contains("?C=N;O=") && !html.contains("Index of") {
        return Err(Failure::BadResponse("not a directory listing".into()));
    }
    Ok(torrents(&html))
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
            let key = Key { url: url.to_string(), modified: entry.modified };
            let meta = tokio::time::timeout_at(deadline, metainfo(ctx, &url, key, deadline)).await.ok().flatten();
            if meta.is_none() {
                ctx.mark_partial();
            }
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

async fn metainfo(ctx: &Ctx<'_>, url: &Url, key: Key, deadline: Instant) -> Option<Metainfo> {
    const CAP: usize = 2 << 20;
    if let Some(known) = ctx.stores.hashes.get(&key) {
        return Some(known);
    }
    let body = ctx.http.get(url, CAP, deadline).await.ok()?;
    let meta = crate::metainfo::parse(&body).ok()?;
    ctx.stores.hashes.insert(key, meta.clone());
    Some(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "debian-13.7.0-arm64-netinst.iso.torrent";

    fn files(page: &str) -> Vec<String> {
        listing(page.as_bytes()).unwrap().into_iter().map(|e| e.file).collect()
    }

    #[test]
    fn reads_nginx_and_lighttpd_mirrors() {
        let nginx = format!(
            "<html>\n<head><title>Index of /debian-cd/current/arm64/bt-cd/</title></head>\n<body>\n\
             <h1>Index of /debian-cd/current/arm64/bt-cd/</h1><hr><pre><a href=\"../\">../</a>\n\
             <a href=\"{FILE}\">{FILE}</a>    12-Sep-2026 13:26    56588\n</pre><hr></body>\n</html>\n"
        );
        let lighttpd = format!(
            "<html><head><title>Index of /bt-cd/</title></head><body><h2>Index of /bt-cd/</h2>\
             <table summary=\"Directory Listing\"><thead><tr><th class=\"n\">Name</th></tr></thead><tbody>\
             <tr class=\"d\"><td class=\"n\"><a href=\"../\">Parent Directory</a>/</td></tr>\
             <tr><td class=\"n\"><a href=\"{FILE}\">{FILE}</a></td><td class=\"m\">2026-Sep-12 13:26:00</td></tr>\
             </tbody></table></body></html>"
        );
        assert_eq!(files(&nginx), [FILE]);
        assert_eq!(files(&lighttpd), [FILE]);
    }

    #[test]
    fn a_page_that_is_no_listing_fails() {
        let block = format!("<html><body>Access denied. <a href=\"{FILE}\">retry</a></body></html>");
        assert_eq!(listing(block.as_bytes()), Err(Failure::BadResponse("not a directory listing".into())));
    }
}
