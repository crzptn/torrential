//! Internet Archive items that have a torrent, through the advancedsearch JSON API.

use serde::Deserialize;
use torrential_core::{Category, Flag};
use url::Url;

use super::{Ctx, Failure, at};
use crate::observation::{Identity, Observation, infohash, web_url};

const FIELDS: [&str; 8] = [
    "identifier",
    "title",
    "btih",
    "item_size",
    "publicdate",
    "downloads",
    "licenseurl",
    "mediatype",
];

pub(super) async fn search(ctx: &Ctx<'_>, base: &Url) -> Result<Vec<Observation>, Failure> {
    let mut url = at(base, "advancedsearch.php");
    {
        // Query words are letters and digits only, so they cannot form Lucene syntax.
        let q = format!(
            "title:({}) AND btih:* AND NOT mediatype:collection",
            ctx.terms.words().join(" ")
        );
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", &q);
        for field in FIELDS {
            pairs.append_pair("fl[]", field);
        }
        pairs
            .append_pair("sort[]", "downloads desc")
            .append_pair("rows", "50")
            .append_pair("output", "json");
    }
    let body = ctx.http.get(&url, 4 << 20, ctx.deadline).await?;
    parse(&body, base).map_err(Failure::BadResponse)
}

#[derive(Deserialize)]
struct Feed {
    response: Response,
}

#[derive(Deserialize)]
struct Response {
    docs: Vec<Doc>,
}

#[derive(Deserialize)]
struct Doc {
    identifier: String,
    title: Option<Text>,
    btih: Option<Text>,
    item_size: Option<u64>,
    publicdate: Option<Text>,
    licenseurl: Option<Text>,
    mediatype: Option<Text>,
}

/// Archive.org returns a list where an item has several values for a field.
#[derive(Deserialize)]
#[serde(untagged)]
enum Text {
    One(String),
    Many(Vec<String>),
}

impl Text {
    fn first(&self) -> Option<&str> {
        match self {
            Text::One(s) => Some(s),
            Text::Many(v) => v.first().map(String::as_str),
        }
    }
}

fn parse(body: &[u8], base: &Url) -> Result<Vec<Observation>, String> {
    let feed: Feed = serde_json::from_slice(body).map_err(|e| format!("advancedsearch JSON: {e}"))?;
    Ok(feed
        .response
        .docs
        .into_iter()
        .filter_map(|doc| observation(doc, base))
        .collect())
}

fn observation(doc: Doc, base: &Url) -> Option<Observation> {
    let id = doc.identifier;
    if !id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) {
        return None;
    }
    let hash = doc.btih.as_ref().and_then(Text::first).and_then(infohash)?;
    let torrent_url = web_url(at(base, &format!("download/{id}/{id}_archive.torrent")).as_str());
    let name = doc.title.as_ref().and_then(Text::first).unwrap_or(&id);
    let license = doc
        .licenseurl
        .as_ref()
        .and_then(Text::first)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut obs = Observation::new(Identity::Hash { hash, torrent_url }, name)?
        .flag(Flag::PublicDomain, license.contains("publicdomain"))
        .flag(
            Flag::CreativeCommons,
            !license.contains("publicdomain") && license.contains("creativecommons.org/licenses"),
        );
    obs.size = doc.item_size;
    obs.published = doc
        .publicdate
        .as_ref()
        .and_then(Text::first)
        .and_then(crate::dates::rfc3339);
    obs.category = doc.mediatype.as_ref().and_then(Text::first).map(category);
    obs.info_url = web_url(at(base, &format!("details/{id}")).as_str());
    Some(obs)
}

fn category(mediatype: &str) -> Category {
    match mediatype {
        "movies" => Category::Video,
        "audio" | "etree" => Category::Audio,
        "software" => Category::Software,
        "texts" => Category::Book,
        "data" => Category::Dataset,
        _ => Category::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::fixture;

    #[test]
    fn reads_items_with_their_licenses() {
        let base = Url::parse("https://archive.org/").unwrap();
        let all = parse(
            &fixture::read("archive.org/advancedsearch.php-night-of-the-living-dead.json"),
            &base,
        )
        .unwrap();
        assert_eq!(all.len(), 50);
        let dvd = all
            .iter()
            .find(|o| o.info_url.as_deref() == Some("https://archive.org/details/night_of_the_living_dead_dvd"))
            .unwrap();
        assert_eq!(dvd.name(), "Night of the Living Dead");
        assert_eq!(
            dvd.identity,
            Identity::Hash {
                hash: infohash("6bc1af23a17fd62156ab2f0e41d3c86769c2c66c").unwrap(),
                torrent_url: Some(
                    "https://archive.org/download/night_of_the_living_dead_dvd/night_of_the_living_dead_dvd_archive.torrent".into()
                ),
            }
        );
        assert_eq!(dvd.size, Some(5_003_733_941));
        assert_eq!(dvd.peers, None);
        assert_eq!(dvd.category, Some(Category::Video));
        assert_eq!(dvd.flags.iter().collect::<Vec<_>>(), [&Flag::PublicDomain]);
        assert_eq!(dvd.published, Some(1_177_371_285));
    }
}
