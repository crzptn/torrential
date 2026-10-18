use serde::Deserialize;
use torrential_core::{Category, Flag};
use url::Url;

use super::{Ctx, Failure, at, pages};
use crate::observation::{Identity, Observation, infohash, parse_magnet, web_url};

#[derive(Deserialize)]
struct Feed {
    releases: Vec<Release>,
}

#[derive(Deserialize)]
struct Release {
    version: String,
    release_date: Option<String>,
    #[serde(default)]
    available: bool,
    sha256_sum: Option<String>,
    magnet_uri: Option<String>,
    torrent_url: Option<String>,
    torrent: Option<Torrent>,
}

#[derive(Deserialize)]
struct Torrent {
    file_name: Option<String>,
    file_length: Option<u64>,
    info_hash: Option<String>,
}

pub(super) async fn search(ctx: &Ctx<'_>, base: &Url) -> Result<Vec<Observation>, Failure> {
    let is_feed: pages::Check = |body| serde_json::from_slice::<Feed>(body).is_ok();
    let body = ctx.stores.pages.get(ctx, &at(base, "releng/releases/json/"), 2 << 20, is_feed).await?;
    let all = parse(&body, base).map_err(Failure::BadResponse)?;
    Ok(all.into_iter().filter(|o| ctx.terms.covers(o.name())).collect())
}

fn parse(body: &[u8], base: &Url) -> Result<Vec<Observation>, String> {
    let feed: Feed = serde_json::from_slice(body).map_err(|e| format!("releng JSON: {e}"))?;
    Ok(feed
        .releases
        .into_iter()
        .filter(|r| r.available && r.sha256_sum.as_deref().is_some_and(|s| !s.is_empty()))
        .filter_map(|r| {
            let magnet = r.magnet_uri.as_deref().and_then(parse_magnet);
            let torrent = r.torrent.as_ref();
            let hash =
                torrent.and_then(|t| t.info_hash.as_deref()).and_then(infohash).or(magnet.as_ref().map(|m| m.hash))?;
            let torrent_url =
                r.torrent_url.as_deref().and_then(|p| web_url(at(base, p.trim_start_matches('/')).as_str()));
            let default_name = format!("archlinux-{}-x86_64.iso", r.version);
            let name = torrent.and_then(|t| t.file_name.as_deref()).unwrap_or(&default_name);
            let mut obs = Observation::new(Identity::Hash { hash, torrent_url }, name)?.flag(Flag::Official, true);
            obs.size = torrent.and_then(|t| t.file_length);
            obs.published = r.release_date.as_deref().and_then(crate::dates::civil);
            obs.category = Some(Category::Software);
            obs.trackers = magnet.map(|m| m.trackers).unwrap_or_default();
            obs.info_url = web_url(at(base, &format!("releng/releases/{}/", r.version)).as_str());
            Some(obs)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::Peers;
    use crate::sources::fixture;

    #[test]
    fn lists_the_releases_still_on_the_mirrors() {
        let base = Url::parse("https://archlinux.org/").unwrap();
        let all = parse(&fixture::read("archlinux/releng-releases-json.json"), &base).unwrap();
        let names: Vec<&str> = all.iter().map(Observation::name).collect();
        assert_eq!(
            names,
            ["archlinux-2026.10.01-x86_64.iso", "archlinux-2026.09.01-x86_64.iso", "archlinux-2026.08.01-x86_64.iso"]
        );
        let newest = &all[0];
        assert_eq!(
            newest.identity,
            Identity::Hash {
                hash: infohash("9b521219bc48550140a666f74a29048ed78e933b").unwrap(),
                torrent_url: Some("https://archlinux.org/releng/releases/2026.10.01/torrent/".into()),
            }
        );
        assert_eq!(newest.size, Some(1_640_497_152));
        assert_eq!(newest.peers, None::<Peers>);
        assert_eq!(newest.published, Some(1_790_812_800));
        assert_eq!(newest.flags.iter().collect::<Vec<_>>(), [&Flag::Official]);
        assert_eq!(newest.info_url.as_deref(), Some("https://archlinux.org/releng/releases/2026.10.01/"));
    }
}
