//! Knaben's v2 search API, a meta-index of public trackers. Only `/v2/search` is a
//! search: v1 without a request body returns the latest uploads instead.

use serde::Deserialize;
use torrential_core::{Category, Flag};
use url::Url;

use super::{Ctx, Failure, at};
use crate::observation::{Identity, Observation, Peers, infohash, parse_magnet, percent, web_url};

pub(super) async fn search(ctx: &Ctx<'_>, base: &Url) -> Result<Vec<Observation>, Failure> {
    let mut url = at(base, "v2/search");
    let xxx = if ctx.adult { "&xxx" } else { "" };
    url.set_query(Some(&format!("q={}&o=seeders&s=100{xxx}", percent(ctx.terms.text()))));
    let body = ctx.http.get(&url, 4 << 20, ctx.deadline).await?;
    parse(&body).map_err(Failure::BadResponse)
}

#[derive(Deserialize)]
struct Feed {
    hits: Vec<Hit>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Hit {
    title: Option<String>,
    hash: Option<String>,
    bytes: Option<u64>,
    seeders: Option<i64>,
    peers: Option<i64>,
    live_seeders: Option<i64>,
    live_peers: Option<i64>,
    #[serde(default)]
    category_id: Vec<u64>,
    date: Option<String>,
    magnet_url: Option<String>,
    link: Option<String>,
    details: Option<String>,
}

fn parse(body: &[u8]) -> Result<Vec<Observation>, String> {
    let feed: Feed = serde_json::from_slice(body).map_err(|e| format!("search JSON: {e}"))?;
    Ok(feed.hits.into_iter().filter_map(observation).collect())
}

fn observation(hit: Hit) -> Option<Observation> {
    let magnet = hit.magnet_url.as_deref().and_then(parse_magnet);
    let hash = hit
        .hash
        .as_deref()
        .and_then(infohash)
        .or(magnet.as_ref().map(|m| m.hash));
    let torrent_url = hit.link.as_deref().and_then(web_url);
    // RuTracker rows come with a `.torrent` link through Knaben and no hash.
    let identity = match (hash, torrent_url) {
        (Some(hash), torrent_url) => Identity::Hash { hash, torrent_url },
        (None, Some(torrent_url)) => Identity::Locator { torrent_url },
        (None, None) => return None,
    };
    let (category, adult) = category(&hit.category_id);
    let mut obs = Observation::new(identity, hit.title.as_deref()?)?.flag(Flag::Adult, adult);
    let count = |n: Option<i64>| n.and_then(|n| u32::try_from(n).ok());
    // Knaben's own scrape is fresher than the count the origin index reported.
    obs.peers = match hit.live_seeders {
        Some(_) => Peers::new(count(hit.live_seeders), count(hit.live_peers)),
        None => Peers::new(count(hit.seeders), count(hit.peers)),
    };
    obs.size = hit.bytes;
    obs.published = hit.date.as_deref().and_then(crate::dates::rfc3339);
    obs.category = category;
    obs.trackers = magnet.map(|m| m.trackers).unwrap_or_default();
    obs.info_url = hit.details.as_deref().and_then(web_url);
    Some(obs)
}

/// Knaben IDs are `TTSSS000`: a top-level category times a million plus a subcategory.
fn category(ids: &[u64]) -> (Option<Category>, bool) {
    let adult = ids.iter().any(|id| id / 1_000_000 == 5);
    let specific = ids.iter().copied().filter(|id| id % 1_000_000 != 0).min();
    let Some(id) = specific.or_else(|| ids.iter().copied().min()) else {
        return (None, adult);
    };
    let category = match (id / 1_000_000, id) {
        (1, _) => Category::Audio,
        (2 | 3 | 5, _) => Category::Video,
        (4, 4_001_000) => Category::Game,
        (4 | 8, _) => Category::Software,
        (6, 6_006_000) => Category::Book,
        (6, 6_007_000) => Category::Audio,
        (6, _) => Category::Video,
        (7, _) => Category::Game,
        (9, _) => Category::Book,
        _ => Category::Other,
    };
    (Some(category), adult)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::fixture;

    #[test]
    fn reads_hits_with_one_peer_count_pair() {
        let all = parse(&fixture::read("knaben/v2-search-ubuntu.json")).unwrap();
        assert_eq!(all.len(), 100);
        let desktop = &all[0];
        assert_eq!(desktop.name(), "ubuntu-26.04-desktop-amd64.iso");
        assert_eq!(
            desktop.identity,
            Identity::Hash {
                hash: infohash("dafc8c076ca2f3ed376eeae7c76a0d6be2415c45").unwrap(),
                torrent_url: None
            }
        );
        assert_eq!(desktop.size, Some(6_517_612_871));
        assert_eq!(
            desktop.peers,
            Some(Peers {
                seeders: 143,
                leechers: Some(32)
            })
        );
        assert_eq!(desktop.category, Some(Category::Software));
        assert_eq!(desktop.trackers.len(), 18);
        assert_eq!(desktop.trackers[0], "http://p4p.arenabg.com:1337/announce");
        assert!(desktop.flags.is_empty());

        let pack = all
            .iter()
            .find(|o| o.name() == "[amd64] Ubuntu*Pack 24.04.1 GNOME (июль 2026)")
            .unwrap();
        assert!(
            matches!(&pack.identity, Identity::Locator { torrent_url } if torrent_url.starts_with("https://knaben.org/live/dl/rutracker/"))
        );
    }

    #[test]
    fn maps_categories_and_marks_adult_ones() {
        assert_eq!(category(&[4_000_000, 4_004_000]), (Some(Category::Software), false));
        assert_eq!(category(&[9_001_000, 9_000_000]), (Some(Category::Book), false));
        assert_eq!(category(&[5_001_000]), (Some(Category::Video), true));
        assert_eq!(category(&[]), (None, false));
    }
}
