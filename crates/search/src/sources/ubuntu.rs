use url::Url;

use super::listing::{self, Entry};
use super::{Ctx, Failure, at, pages};
use crate::observation::Observation;

const SERIES: usize = 5;

pub(super) async fn search(ctx: &Ctx<'_>, base: &Url) -> Result<Vec<Observation>, Failure> {
    let has_series: pages::Check = |index| !current_series(&String::from_utf8_lossy(index)).is_empty();
    let index = ctx.stores.pages.get(ctx, base, listing::LISTING_CAP, has_series).await?;
    let series = current_series(&String::from_utf8_lossy(&index));
    if series.is_empty() {
        return Err(Failure::BadResponse("no release directories in the index".into()));
    }
    let dirs = series.iter().map(|s| at(base, &format!("{s}/"))).collect();
    let mut files = Vec::new();
    for (dir, entries) in listing::fetch_all(ctx, dirs).await? {
        files.extend(newest_point_releases(entries).into_iter().map(|entry| (dir.clone(), entry)));
    }
    Ok(listing::resolve(ctx, files).await)
}

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

fn newest_point_releases(entries: Vec<Entry>) -> Vec<Entry> {
    let mut newest: Vec<(String, (Vec<u32>, bool), Entry)> = Vec::new();
    for entry in entries {
        let (image, rank) = match entry.name().split_once('-').and_then(|(_, rest)| rest.split_once('-')) {
            Some((v, image)) => {
                let (image, final_build) = image.strip_prefix("beta-").map_or((image, true), |rest| (rest, false));
                (image.to_string(), (version(v).unwrap_or_default(), final_build))
            }
            None => (entry.name().to_string(), (Vec::new(), true)),
        };
        match newest.iter_mut().find(|(i, ..)| *i == image) {
            Some(slot) if slot.1 < rank => *slot = (image, rank, entry),
            Some(_) => {}
            None => newest.push((image, rank, entry)),
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

    #[test]
    fn drops_betas_once_the_final_build_is_out() {
        let page = String::from_utf8(fixture::read("ubuntu/26.10.html")).unwrap();
        let kept: Vec<String> = newest_point_releases(listing::torrents(&page)).into_iter().map(|e| e.file).collect();
        assert_eq!(kept, ["ubuntu-26.10-desktop-amd64.iso.torrent", "ubuntu-26.10-live-server-amd64.iso.torrent"]);
        let beta_only: Vec<Entry> =
            listing::torrents(&page).into_iter().filter(|e| e.file.contains("-beta-")).collect();
        let kept: Vec<String> = newest_point_releases(beta_only).into_iter().map(|e| e.file).collect();
        assert_eq!(
            kept,
            ["ubuntu-26.10-beta-desktop-amd64.iso.torrent", "ubuntu-26.10-beta-live-server-amd64.iso.torrent"]
        );
    }
}
