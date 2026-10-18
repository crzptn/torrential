//! Official Ubuntu torrents from releases.ubuntu.com.

use url::Url;

use super::listing::{self, Entry, LISTING_CAP};
use super::{Ctx, Failure, at};
use crate::observation::Observation;

/// The newest release series kept on the site: every supported LTS and interim
/// release fits in five, and older ones are past standard support.
const SERIES: usize = 5;

pub(super) async fn search(ctx: &Ctx<'_>, base: &Url) -> Result<Vec<Observation>, Failure> {
    let index = ctx.http.get(base, LISTING_CAP, ctx.deadline).await?;
    let series = current_series(&String::from_utf8_lossy(&index));
    if series.is_empty() {
        return Err(Failure::BadResponse("no release directories in the index".into()));
    }
    let dirs = series.iter().map(|s| at(base, &format!("{s}/"))).collect();
    let mut files = Vec::new();
    for (dir, entries) in listing::fetch_all(ctx, dirs).await? {
        for entry in newest_point_releases(entries) {
            if ctx.terms.covers(entry.name()) {
                files.push((dir.clone(), entry));
            }
        }
    }
    Ok(listing::resolve(ctx, files).await)
}

/// Series directories such as `24.04/`, newest first.
fn current_series(index: &str) -> Vec<String> {
    let mut series: Vec<(Vec<u32>, String)> = listing::hrefs(index)
        .into_iter()
        .filter_map(|href| {
            let name = href.strip_suffix('/')?;
            let parts = version(name)?;
            (parts.len() == 2).then(|| (parts, name.to_string()))
        })
        .collect();
    series.sort_by(|a, b| b.0.cmp(&a.0));
    series.into_iter().take(SERIES).map(|(_, name)| name).collect()
}

/// A series directory keeps every point release; only the newest of each image
/// (`desktop-amd64`, `live-server-amd64`) is worth offering.
fn newest_point_releases(entries: Vec<Entry>) -> Vec<Entry> {
    let mut newest: Vec<(String, Vec<u32>, Entry)> = Vec::new();
    for entry in entries {
        let (image, v) = match entry.name().split_once('-').and_then(|(_, rest)| rest.split_once('-')) {
            Some((v, image)) => (image.to_string(), version(v).unwrap_or_default()),
            None => (entry.name().to_string(), Vec::new()),
        };
        match newest.iter_mut().find(|(i, ..)| *i == image) {
            Some(slot) if slot.1 < v => *slot = (image, v, entry),
            Some(_) => {}
            None => newest.push((image, v, entry)),
        }
    }
    newest.into_iter().map(|(.., entry)| entry).collect()
}

fn version(s: &str) -> Option<Vec<u32>> {
    s.split('.').map(|p| p.parse().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::fixture;

    #[test]
    fn keeps_the_five_newest_series() {
        let index = String::from_utf8(fixture::read("ubuntu/index.html")).unwrap();
        assert_eq!(current_series(&index), ["26.10", "26.04", "25.10", "24.04", "22.04"]);
    }

    #[test]
    fn keeps_the_newest_point_release_of_each_image() {
        let page = String::from_utf8(fixture::read("ubuntu/24.04.html")).unwrap();
        let kept: Vec<(String, Option<i64>)> =
            newest_point_releases(listing::torrents(&page)).into_iter().map(|e| (e.file, e.modified)).collect();
        assert_eq!(
            kept,
            [
                ("ubuntu-24.04.5.1-desktop-amd64.iso.torrent".to_string(), Some(1_789_499_460)),
                ("ubuntu-24.04.5-live-server-amd64.iso.torrent".to_string(), Some(1_789_059_420)),
            ]
        );
    }
}
