//! The Pirate Bay's JSON backend. Every field is a string.

use serde::Deserialize;
use torrential_core::{Category, Flag};
use url::Url;

use super::{Ctx, Failure, at};
use crate::observation::{Identity, Observation, Peers, count, infohash, percent, tracker, unescape_html};

/// The trackers thepiratebay.org adds to the magnets it prints (`print_trackers` in
/// its `static/main.js`). apibay returns bare hashes.
const TRACKERS: [&str; 13] = [
    "udp://tracker.opentrackr.org:1337",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://tracker.bittor.pw:1337/announce",
    "udp://public.popcorn-tracker.org:6969/announce",
    "udp://tracker.dler.org:6969/announce",
    "udp://exodus.desync.com:6969",
    "udp://open.demonii.com:1337/announce",
    "udp://glotorrents.pw:6969/announce",
    "udp://tracker.coppersurfer.tk:6969",
    "udp://torrent.gresille.org:80/announce",
    "udp://p4p.arenabg.com:1337",
    "udp://tracker.internetwarriors.net:1337",
];

pub(super) async fn search(ctx: &Ctx<'_>, base: &Url) -> Result<Vec<Observation>, Failure> {
    let mut url = at(base, "q.php");
    url.set_query(Some(&format!("q={}&cat=0", percent(ctx.terms.text()))));
    let body = ctx.http.get(&url, 4 << 20, ctx.deadline).await?;
    parse(&body).map_err(Failure::BadResponse)
}

#[derive(Deserialize)]
struct Row {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    info_hash: String,
    #[serde(default)]
    seeders: String,
    #[serde(default)]
    leechers: String,
    #[serde(default)]
    size: String,
    #[serde(default)]
    added: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    category: String,
}

fn parse(body: &[u8]) -> Result<Vec<Observation>, String> {
    let rows: Vec<Row> = serde_json::from_slice(body).map_err(|e| format!("q.php JSON: {e}"))?;
    // No matches come back as one row with ID 0 and an all-zero hash.
    if let [only] = rows.as_slice()
        && only.id == "0"
        && only.info_hash.bytes().all(|b| b == b'0')
    {
        return Ok(Vec::new());
    }
    let trackers: Vec<String> = TRACKERS.iter().filter_map(|t| tracker(t)).collect();
    Ok(rows.into_iter().filter_map(|row| observation(row, &trackers)).collect())
}

fn observation(row: Row, trackers: &[String]) -> Option<Observation> {
    let hash = infohash(&row.info_hash)?;
    let code: u32 = row.category.trim().parse().unwrap_or(0);
    let trusted = matches!(row.status.as_str(), "vip" | "trusted");
    let mut obs = Observation::new(Identity::Hash { hash, torrent_url: None }, &unescape_html(&row.name))?
        .flag(Flag::Trusted, trusted)
        .flag(Flag::Adult, (500..600).contains(&code));
    obs.peers = Peers::new(count(&row.seeders), count(&row.leechers));
    obs.size = row.size.trim().parse().ok();
    obs.published = row.added.trim().parse().ok().filter(|t| *t > 0);
    obs.category = match code {
        100..200 => Some(Category::Audio),
        200..300 | 500..600 => Some(Category::Video),
        300..400 => Some(Category::Software),
        400..500 => Some(Category::Game),
        601 | 602 => Some(Category::Book),
        600..700 => Some(Category::Other),
        _ => None,
    };
    obs.trackers = trackers.to_vec();
    if row.id.bytes().all(|b| b.is_ascii_digit()) && !row.id.is_empty() {
        obs.info_url = Some(format!("https://thepiratebay.org/description.php?id={}", row.id));
    }
    Some(obs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::fixture;

    #[test]
    fn reads_string_typed_rows() {
        let all = parse(&fixture::read("apibay/q.php-ubuntu.json")).unwrap();
        assert_eq!(all.len(), 100);
        let lts = all.iter().find(|o| o.name() == "Ubuntu 22.04 LTS").unwrap();
        assert_eq!(
            lts.identity,
            Identity::Hash { hash: infohash("2c6b6858d61da9543d4231a71db4b1c9264b0685").unwrap(), torrent_url: None }
        );
        assert_eq!(lts.size, Some(3_654_957_056));
        assert_eq!(lts.published, Some(1_652_877_231));
        assert_eq!(lts.category, Some(Category::Software));
        assert_eq!(lts.flags.iter().collect::<Vec<_>>(), [&Flag::Trusted]);
        assert_eq!(lts.info_url.as_deref(), Some("https://thepiratebay.org/description.php?id=59191690"));
        assert_eq!(lts.trackers.len(), 13);

        let escaped = all.iter().find(|o| o.name().starts_with("Linux Command Line (Hands-On)")).unwrap();
        assert_eq!(escaped.name(), "Linux Command Line (Hands-On) Ubuntu Linux & Linux Scripting");
    }

    #[test]
    fn the_no_results_row_is_an_empty_answer() {
        let none = parse(&fixture::read("apibay/q.php-zzqqxxnotarealthing987.json")).unwrap();
        assert_eq!(none, Vec::new());
    }
}
