//! Records the raw responses one source returns for a query, as test fixtures.
//!
//! ```text
//! cargo run -p torrential-search --example record -- <source> <query words...>
//! ```
//!
//! Each body lands in `tests/fixtures/<source>/`, named after its URL path, with
//! the query appended for URLs that carry one. Bodies over 256 KiB are skipped,
//! except RSS feeds, which keep their first 40 items so the Academic Torrents
//! catalog fits.

use std::path::Path;

use torrential_search::SearchConfig;

const MAX_BODY: usize = 256 << 10;
const RSS_ITEMS: usize = 40;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(source), query) = (args.next(), args.collect::<Vec<_>>().join(" ")) else {
        eprintln!("usage: record <source> <query words...>");
        std::process::exit(2);
    };
    let (outcome, bodies) =
        torrential_search::record(&SearchConfig::default(), &source, &query).await.unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        });
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(&source);
    std::fs::create_dir_all(&dir).expect("create the fixture directory");
    for (url, body) in bodies {
        let body = trim_rss(body);
        let name = file_name(&url, &query, &body);
        if body.len() > MAX_BODY {
            println!("skipped {url} ({} bytes)", body.len());
            continue;
        }
        std::fs::write(dir.join(&name), &body).expect("write the fixture");
        println!("{url} -> {source}/{name} ({} bytes)", body.len());
    }
    println!("{source}: {outcome}");
}

fn file_name(url: &str, query: &str, body: &[u8]) -> String {
    let url = url::Url::parse(url).expect("recorded URLs parse");
    let mut stem = url.path().trim_matches('/').replace('/', "-");
    if stem.is_empty() {
        stem = "index".into();
    }
    if url.query().is_some() {
        stem = format!("{stem}-{}", slug(query));
    }
    let start = String::from_utf8_lossy(&body[..body.len().min(256)]).trim_start().to_ascii_lowercase();
    let ext =
        if stem.ends_with(".torrent") || stem.ends_with(".json") || stem.ends_with(".xml") || stem.ends_with(".html") {
            ""
        } else if start.starts_with('{') || start.starts_with('[') {
            ".json"
        } else if start.starts_with("<?xml") || start.starts_with("<rss") {
            ".xml"
        } else if start.starts_with('d') && stem.contains("torrent") {
            ".torrent"
        } else {
            ".html"
        };
    format!("{stem}{ext}")
}

fn slug(s: &str) -> String {
    let words: Vec<String> =
        s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
    words.join("-")
}

fn trim_rss(body: Vec<u8>) -> Vec<u8> {
    let text = String::from_utf8_lossy(&body);
    if !text.trim_start().starts_with("<?xml") {
        return body;
    }
    let mut end = 0;
    for _ in 0..RSS_ITEMS {
        match text[end..].find("</item>") {
            Some(at) => end += at + "</item>".len(),
            None => return body,
        }
    }
    if !text[end..].contains("<item>") {
        return body;
    }
    format!("{}\n</channel>\n</rss>\n", &text[..end]).into_bytes()
}
