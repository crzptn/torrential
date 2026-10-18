//! Nyaa's RSS feed, with seeders and a trusted mark in the `nyaa:` namespace.

use torrential_core::{Category, Flag};
use url::Url;

use super::{Ctx, Failure, rss};
use crate::observation::{Identity, Observation, Peers, count, infohash, percent, tracker, web_url};

/// The trackers nyaa.si puts in its own magnet links.
const TRACKERS: [&str; 5] = [
    "http://nyaa.tracker.wf:7777/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://tracker.torrent.eu.org:451/announce",
];

pub(super) async fn search(ctx: &Ctx<'_>, base: &Url) -> Result<Vec<Observation>, Failure> {
    let mut url = base.clone();
    url.set_query(Some(&format!("page=rss&q={}&c=0_0&f=0", percent(ctx.terms.text()))));
    let body = ctx.http.get(&url, 4 << 20, ctx.deadline).await?;
    parse(&body).map_err(Failure::BadResponse)
}

/// DDoS-Guard sometimes answers 200 with an empty body; that is a failure, not zero results.
fn parse(body: &[u8]) -> Result<Vec<Observation>, String> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Err("empty body".into());
    }
    let trackers: Vec<String> = TRACKERS.iter().filter_map(|t| tracker(t)).collect();
    Ok(rss::items(body)?
        .into_iter()
        .filter_map(|item| {
            let hash = infohash(item.get("infoHash")?)?;
            let torrent_url = item.get("link").and_then(web_url);
            let mut obs = Observation::new(Identity::Hash { hash, torrent_url }, item.get("title")?)?
                .flag(Flag::Trusted, item.get("trusted") == Some("Yes"));
            obs.peers = Peers::new(
                item.get("seeders").and_then(count),
                item.get("leechers").and_then(count),
            );
            obs.size = item.get("size").and_then(torrential_core::units::parse_size);
            obs.published = item.get("pubDate").and_then(crate::dates::rfc2822);
            obs.category = item.get("categoryId").and_then(category);
            obs.trackers = trackers.clone();
            obs.info_url = item.get("guid").and_then(web_url);
            Some(obs)
        })
        .collect())
}

fn category(id: &str) -> Option<Category> {
    Some(match id.trim() {
        "6_2" => Category::Game,
        id => match id.split('_').next()? {
            "1" | "4" => Category::Video,
            "2" => Category::Audio,
            "3" => Category::Book,
            "6" => Category::Software,
            _ => Category::Other,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::fixture;

    #[test]
    fn reads_the_nyaa_namespace() {
        let all = parse(&fixture::read("nyaa/index-linux.xml")).unwrap();
        let epub = all.iter().find(|o| o.name().starts_with("EPUBShelf")).unwrap();
        assert_eq!(
            epub.name(),
            "EPUBShelf v2.0.0 [A light novel manager][Linux, Windows, macOS]"
        );
        assert_eq!(
            epub.identity,
            Identity::Hash {
                hash: infohash("0e6072b0b0f27d2f18da10ebad2cb18d6c8d4219").unwrap(),
                torrent_url: Some("https://nyaa.si/download/2174277.torrent".into()),
            }
        );
        assert_eq!(
            epub.peers,
            Some(Peers {
                seeders: 4,
                leechers: Some(5)
            })
        );
        assert_eq!(epub.size, Some(433_376_461));
        assert_eq!(epub.published, Some(1_792_072_832));
        assert_eq!(epub.category, Some(Category::Software));
        assert_eq!(epub.info_url.as_deref(), Some("https://nyaa.si/view/2174277"));
        assert!(epub.flags.is_empty());
        assert_eq!(epub.trackers.len(), 5);
    }

    #[test]
    fn an_empty_body_is_not_zero_results() {
        assert_eq!(parse(b"").unwrap_err(), "empty body");
    }
}
