//! A Torznab endpoint the user configured: Jackett, Prowlarr, or bitmagnet. Its
//! indexers may be private trackers, so a row that only links a `.torrent` keeps the
//! link as a locator. Fetching it here could count as a grab.

use torrential_core::{Category, Flag};
use url::Url;

use super::{Ctx, Failure, rss};
use crate::observation::{Identity, Observation, Peers, count, infohash, parse_magnet, web_url};

pub(super) async fn search(ctx: &Ctx<'_>, endpoint: &Url, api_key: Option<&str>) -> Result<Vec<Observation>, Failure> {
    let mut url = endpoint.clone();
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("t", "search").append_pair("q", ctx.terms.text());
        if let Some(key) = api_key {
            pairs.append_pair("apikey", key);
        }
    }
    let body = ctx.http.get(&url, 8 << 20, ctx.deadline).await?;
    parse(&body).map_err(Failure::BadResponse)
}

fn parse(body: &[u8]) -> Result<Vec<Observation>, String> {
    Ok(rss::items(body)?.into_iter().filter_map(observation).collect())
}

fn observation(item: rss::Item) -> Option<Observation> {
    let magnet = item
        .attr("magneturl")
        .into_iter()
        .chain(item.enclosure.as_deref())
        .chain(item.get("link"))
        .find_map(parse_magnet);
    let torrent_url = item.enclosure.as_deref().into_iter().chain(item.get("link")).find_map(web_url);
    let hash = item.attr("infohash").and_then(infohash).or(magnet.as_ref().map(|m| m.hash));
    let identity = match (hash, torrent_url) {
        (Some(hash), torrent_url) => Identity::Hash { hash, torrent_url },
        (None, Some(torrent_url)) => Identity::Locator { torrent_url },
        (None, None) => return None,
    };
    let ids: Vec<u32> =
        item.attrs("category").chain(item.all("category")).filter_map(|c| c.trim().parse().ok()).collect();
    // Scan every category: an adult one can follow a harmless one.
    let adult = ids.iter().any(|id| (6000..7000).contains(id));
    let mut obs = Observation::new(identity, item.get("title")?)?.flag(Flag::Adult, adult);
    let seeders = item.attr("seeders").and_then(count);
    // Torznab's `peers` counts seeders and leechers together.
    let leechers = item
        .attr("leechers")
        .and_then(count)
        .or_else(|| item.attr("peers").and_then(count).zip(seeders).and_then(|(p, s)| p.checked_sub(s)));
    obs.peers = Peers::new(seeders, leechers);
    obs.size = item.attr("size").or(item.get("size")).and_then(|s| s.trim().parse().ok());
    obs.published = item.get("pubDate").and_then(crate::dates::rfc2822);
    obs.category = ids.iter().find_map(|id| category(*id));
    obs.trackers = magnet.map(|m| m.trackers).unwrap_or_default();
    obs.info_url = item.get("comments").and_then(web_url);
    Some(obs)
}

/// Newznab's standard categories. Indexer-specific IDs (100000 and up) are skipped.
fn category(id: u32) -> Option<Category> {
    Some(match id {
        1000..2000 => Category::Game,
        4050 => Category::Game,
        2000..3000 | 5000..7000 => Category::Video,
        3000..4000 => Category::Audio,
        4000..5000 => Category::Software,
        7000..8000 => Category::Book,
        8000..9000 => Category::Other,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::fixture;

    #[test]
    fn reads_hash_rows_and_locator_rows() {
        let all = parse(&fixture::read("torznab/jackett-ubuntu.xml")).unwrap();
        assert_eq!(all.len(), 3);

        let hashed = &all[0];
        assert_eq!(hashed.name(), "ubuntu-26.04-desktop-amd64.iso");
        assert_eq!(
            hashed.identity,
            Identity::Hash {
                hash: infohash("dafc8c076ca2f3ed376eeae7c76a0d6be2415c45").unwrap(),
                torrent_url: Some(
                    "http://127.0.0.1:9117/dl/thepiratebay/?jackett_apikey=k3y&path=abc&file=ubuntu".into()
                ),
            }
        );
        assert_eq!(hashed.peers, Some(Peers { seeders: 140, leechers: Some(25) }));
        assert_eq!(hashed.size, Some(6_517_612_871));
        assert_eq!(hashed.category, Some(Category::Software));
        assert_eq!(hashed.trackers, vec!["udp://tracker.opentrackr.org:1337/announce".to_string()]);

        let locator = &all[1];
        assert_eq!(
            locator.identity,
            Identity::Locator {
                torrent_url: "http://127.0.0.1:9117/dl/privatehd/?jackett_apikey=k3y&path=def&file=ubuntu".into()
            }
        );
        assert_eq!(locator.peers, Some(Peers { seeders: 3, leechers: None }));

        let adult = &all[2];
        assert_eq!(adult.flags.iter().collect::<Vec<_>>(), [&Flag::Adult]);
        assert_eq!(adult.category, Some(Category::Video));
    }
}
