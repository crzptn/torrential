use std::collections::BTreeSet;
use std::sync::Arc;

use torrential_core::error::Details;
use torrential_core::{Category, ErrorCode, Flag, Release, ReleaseId, SearchQuery, Sort, SourceOutcome, SourceReport};

use super::*;
use crate::observation::{Identity, Observation, Peers, infohash};

fn source(name: &str, authority: u16, lawful: bool, public: bool) -> Arc<SourceMeta> {
    Arc::new(SourceMeta { name: name.into(), authority, lawful, public })
}

fn archive() -> Arc<SourceMeta> {
    source("archive.org", 3, true, true)
}
fn knaben() -> Arc<SourceMeta> {
    source("knaben", 5, false, true)
}
fn apibay() -> Arc<SourceMeta> {
    source("apibay", 6, false, true)
}

fn row(hash: &str, name: &str) -> Observation {
    Observation::new(Identity::Hash { hash: infohash(hash).unwrap(), torrent_url: None }, name).unwrap()
}

fn with(mut o: Observation, seeders: Option<u32>, size: Option<u64>, published: Option<i64>) -> Observation {
    o.peers = Peers::new(seeders, None);
    o.size = size;
    o.published = published;
    o
}

fn fresh(source: &Arc<SourceMeta>, rows: Vec<Observation>) -> Run {
    Run { source: source.clone(), answer: Answer::Fresh(Arc::new(rows)), elapsed_ms: 40 }
}

fn failed(source: &Arc<SourceMeta>, failure: Failure) -> Run {
    Run { source: source.clone(), answer: Answer::Failed(failure), elapsed_ms: 6000 }
}

fn search(query: &SearchQuery, runs: &[Run]) -> SearchResult {
    assemble(query, &Terms::parse(&query.text), runs, 1200).unwrap().0
}

fn names(result: &SearchResult) -> Vec<&str> {
    result.releases.iter().map(|r| r.name.as_str()).collect()
}

fn h(byte: u8) -> String {
    format!("{byte:02x}").repeat(20)
}

#[test]
fn merges_one_torrent_listed_by_two_sources() {
    let mut from_knaben = row(&h(0xaa), "Ubuntu 24.04 Desktop");
    from_knaben.peers = Some(Peers { seeders: 50, leechers: Some(5) });
    from_knaben.size = Some(100);
    from_knaben.published = Some(1000);
    from_knaben.category = Some(Category::Software);
    from_knaben.trackers = vec!["udp://a.example:1337/announce".into(), "udp://b.example:1337/announce".into()];
    let mut from_apibay = row(&h(0xaa).to_uppercase(), "Ubuntu 24.04 Desktop amd64 ISO").flag(Flag::Trusted, true);
    from_apibay.peers = Some(Peers { seeders: 80, leechers: None });
    from_apibay.size = Some(101);
    from_apibay.published = Some(900);
    from_apibay.trackers = vec!["udp://b.example:1337/announce".into(), "udp://c.example:1337/announce".into()];
    from_apibay.info_url = Some("https://thepiratebay.org/description.php?id=1".into());

    let result = search(
        &SearchQuery::new("ubuntu 24.04"),
        &[fresh(&apibay(), vec![from_apibay]), fresh(&knaben(), vec![from_knaben])],
    );

    let id = format!("{:02x}", 0xaa).repeat(20);
    assert_eq!(
        result.releases,
        [Release {
            id: ReleaseId::Torrent(id.parse().unwrap()),
            name: "Ubuntu 24.04 Desktop amd64 ISO".into(),
            size: Some(100),
            // Both counts come from apibay's one observation, never knaben's leechers.
            seeders: Some(80),
            leechers: None,
            published: Some(1000),
            category: Some(Category::Software),
            flags: BTreeSet::from([Flag::Trusted]),
            sources: vec!["knaben".into(), "apibay".into()],
            magnet: Some(format!(
                "magnet:?xt=urn:btih:{id}&dn=Ubuntu%2024.04%20Desktop%20amd64%20ISO\
                 &tr=udp%3A%2F%2Fa.example%3A1337%2Fannounce\
                 &tr=udp%3A%2F%2Fb.example%3A1337%2Fannounce\
                 &tr=udp%3A%2F%2Fc.example%3A1337%2Fannounce"
            )),
            torrent_url: None,
            info_url: Some("https://thepiratebay.org/description.php?id=1".into()),
        }]
    );
    assert_eq!(result.total, 1);
}

#[test]
fn a_cut_title_loses_to_the_whole_one() {
    let cut = row(&h(0xab), "Linux Command Line (Hands-On) Ubuntu Linux & Linux Scripting ...");
    let whole = row(&h(0xab), "Linux Command Line (Hands-On) Ubuntu Linux & Linux Scripting");
    let result = search(&SearchQuery::new("ubuntu"), &[fresh(&knaben(), vec![cut, whole])]);
    assert_eq!(names(&result), ["Linux Command Line (Hands-On) Ubuntu Linux & Linux Scripting"]);
}

#[test]
fn a_v2_listing_joins_the_hybrid_listing_of_the_same_torrent() {
    let (v1, v2) = (h(0x11), "22".repeat(32));
    let hybrid = Observation::new(
        Identity::Hash { hash: InfoHash::Hybrid { v1: [0x11; 20], v2: [0x22; 32] }, torrent_url: None },
        "ImageNet 2012",
    )
    .unwrap();
    let v2_only = row(&v2, "ImageNet 2012 train");
    let academic = source("academictorrents", 4, true, true);

    let result =
        search(&SearchQuery::new("imagenet"), &[fresh(&academic, vec![hybrid]), fresh(&knaben(), vec![v2_only])]);

    assert_eq!(result.releases.len(), 1);
    let release = &result.releases[0];
    assert_eq!(release.id.to_ref(), v1);
    assert_eq!(release.sources, ["academictorrents", "knaben"]);
    assert_eq!(
        release.magnet.as_deref(),
        Some(format!("magnet:?xt=urn:btih:{v1}&xt=urn:btmh:1220{v2}&dn=ImageNet%202012%20train").as_str())
    );
}

#[test]
fn conflicting_hybrids_resolve_the_same_way_in_any_order() {
    let hybrid = |v1: u8| {
        let hash = InfoHash::Hybrid { v1: [v1; 20], v2: [0x22; 32] };
        Observation::new(Identity::Hash { hash, torrent_url: None }, "ImageNet 2012").unwrap()
    };
    let q = SearchQuery::new("imagenet");
    let ids = |rows: Vec<Observation>| -> Vec<(String, String)> {
        search(&q, &[fresh(&knaben(), rows)]).releases.into_iter().map(|r| (r.id.to_ref(), r.name)).collect()
    };
    let v2_only = || row(&"22".repeat(32), "ImageNet 2012 train");
    let one_way = ids(vec![hybrid(0x02), hybrid(0x01), v2_only()]);
    let other_way = ids(vec![v2_only(), hybrid(0x01), hybrid(0x02)]);
    assert_eq!(one_way, [(h(0x01), "ImageNet 2012 train".to_string()), (h(0x02), "ImageNet 2012".to_string())]);
    assert_eq!(other_way, one_way);
}

#[test]
fn private_links_stay_out_of_results_and_with_their_own_listing() {
    let torznab = source("tracker", 8, false, false);
    let mut public = row(&h(0x33), "Debian 13 netinst");
    public.trackers = vec!["udp://public.example:6969/announce".into()];
    let mut private = row(&h(0x33), "Debian 13 netinst");
    private.trackers = vec!["https://private.example/announce?passkey=secret".into()];
    let mut private_only = Observation::new(
        Identity::Hash {
            hash: infohash(&h(0x44)).unwrap(),
            torrent_url: Some("http://127.0.0.1:9117/dl/x/?jackett_apikey=k3y".into()),
        },
        "Debian 13 DVD",
    )
    .unwrap();
    private_only.trackers = vec!["https://private.example/announce?passkey=secret".into()];
    let q = SearchQuery::new("debian 13");

    let (shown, linked) = assemble(
        &q,
        &Terms::parse(&q.text),
        &[fresh(&knaben(), vec![public]), fresh(&torznab, vec![private, private_only])],
        0,
    )
    .unwrap();

    let links = |releases: &[Release]| -> Vec<(Option<String>, Option<String>)> {
        releases.iter().map(|r| (r.magnet.clone(), r.torrent_url.clone())).collect()
    };
    let public_magnet = format!(
        "magnet:?xt=urn:btih:{}&dn=Debian%2013%20netinst&tr=udp%3A%2F%2Fpublic.example%3A6969%2Fannounce",
        h(0x33)
    );
    assert_eq!(
        links(&shown.releases),
        [
            (Some(public_magnet.clone()), None),
            (Some(format!("magnet:?xt=urn:btih:{}&dn=Debian%2013%20DVD", h(0x44))), None)
        ]
    );
    assert_eq!(
        links(&linked),
        [
            (Some(public_magnet), None),
            (
                Some(format!(
                    "magnet:?xt=urn:btih:{}&dn=Debian%2013%20DVD&tr=https%3A%2F%2Fprivate.example%2Fannounce%3Fpasskey%3Dsecret",
                    h(0x44)
                )),
                Some("http://127.0.0.1:9117/dl/x/?jackett_apikey=k3y".into())
            ),
        ]
    );
}

#[test]
fn locator_rows_get_a_stable_u_ref() {
    let url = "http://127.0.0.1:9117/dl/privatehd/?path=def";
    let locator = || Observation::new(Identity::Locator { torrent_url: url.into() }, "Ubuntu 26.04 LTS").unwrap();
    let torznab = source("tracker", 8, false, false);
    let q = SearchQuery::new("ubuntu");
    let (first, linked) = assemble(&q, &Terms::parse(&q.text), &[fresh(&torznab, vec![locator()])], 0).unwrap();
    let again = search(&q, &[fresh(&torznab, vec![locator(), locator()])]);
    let digest: [u8; 20] = Sha1::digest(url.as_bytes()).into();
    assert_eq!(first.releases[0].id, ReleaseId::Locator(LocatorId::from_bytes(digest)));
    assert!(first.releases[0].id.to_ref().starts_with('u'));
    assert_eq!((first.releases[0].torrent_url.as_deref(), first.releases[0].magnet.as_deref()), (None, None));
    assert_eq!(linked[0].torrent_url.as_deref(), Some(url));
    assert_eq!(again.releases, first.releases);
}

#[test]
fn the_guard_drops_a_source_that_answered_another_question() {
    let terms = Terms::parse("ubuntu");
    let latest = vec![
        row(&h(1), "Some Movie 2026 1080p"),
        row(&h(2), "Another Show S01E01"),
        row(&h(3), "Ubuntu 24.04"),
        row(&h(4), "Album 2026 FLAC"),
    ];
    assert_eq!(vet(&terms, latest), Err(Failure::Irrelevant));

    let search_rows = vec![row(&h(1), "Ubuntu 24.04"), row(&h(2), "ubuntu-26.04-desktop"), row(&h(3), "Unrelated")];
    let kept: Vec<String> = vet(&terms, search_rows).unwrap().iter().map(|o| o.name().to_string()).collect();
    assert_eq!(kept, ["Ubuntu 24.04", "ubuntu-26.04-desktop"]);
}

/// Five films for "night of the living dead 1968" with ties at every level.
fn films() -> Vec<Run> {
    let mut lawful = with(row(&h(0xa1), "Night of the Living Dead (1968)"), None, Some(5_000), Some(100));
    lawful.category = Some(Category::Video);
    let bluray = with(row(&h(0xb2), "Night of the Living Dead 1968 1080p BluRay"), Some(900), Some(4_000), Some(500));
    let remake = with(row(&h(0xc3), "Night of the Living Dead (1990)"), Some(5_000), Some(1_000), Some(300));
    let partial = with(row(&h(0xd4), "Living Dead Night Collection"), Some(10_000), Some(3_000), None);
    let mirror = with(row(&h(0xe5), "Night of the Living Dead 1968"), Some(900), Some(2_000), Some(500));
    vec![
        fresh(&archive(), vec![lawful]),
        fresh(&knaben(), vec![bluray, remake, partial]),
        fresh(&apibay(), vec![mirror]),
    ]
}

#[test]
fn ranks_relevance_before_popularity_then_lawful_seeders_size() {
    let result = search(&SearchQuery::new("night of the living dead 1968"), &films());
    assert_eq!(
        names(&result),
        [
            "Night of the Living Dead (1968)",
            "Night of the Living Dead 1968 1080p BluRay",
            "Night of the Living Dead 1968",
            "Night of the Living Dead (1990)",
            "Living Dead Night Collection",
        ]
    );
}

#[test]
fn alternate_sorts_keep_the_same_tie_breaks() {
    let sorted = |sort| {
        let mut q = SearchQuery::new("night of the living dead 1968");
        q.sort = sort;
        names(&search(&q, &films())).into_iter().map(str::to_string).collect::<Vec<_>>()
    };
    assert_eq!(
        sorted(Sort::Seeds),
        [
            "Living Dead Night Collection",
            "Night of the Living Dead (1990)",
            "Night of the Living Dead 1968 1080p BluRay",
            "Night of the Living Dead 1968",
            "Night of the Living Dead (1968)",
        ]
    );
    assert_eq!(
        sorted(Sort::Size),
        [
            "Night of the Living Dead (1968)",
            "Night of the Living Dead 1968 1080p BluRay",
            "Living Dead Night Collection",
            "Night of the Living Dead 1968",
            "Night of the Living Dead (1990)",
        ]
    );
    assert_eq!(
        sorted(Sort::Age),
        [
            "Night of the Living Dead 1968 1080p BluRay",
            "Night of the Living Dead 1968",
            "Night of the Living Dead (1990)",
            "Night of the Living Dead (1968)",
            "Living Dead Night Collection",
        ]
    );
}

#[test]
fn filters_exclude_unknown_values_and_count_before_the_limit() {
    let mut q = SearchQuery::new("night of the living dead 1968");
    let mut runs = films();
    runs.push(fresh(
        &source("nyaa", 7, false, true),
        vec![with(row(&h(0xf6), "Night of the Living Dead 1968 REMUX"), Some(40), None, None)],
    ));

    q.max_size = Some(3_000);
    let small = search(&q, &runs);
    assert_eq!(
        names(&small),
        ["Night of the Living Dead 1968", "Night of the Living Dead (1990)", "Living Dead Night Collection"]
    );

    q.max_size = None;
    q.min_seeds = Some(1_000);
    q.limit = 1;
    let seeded = search(&q, &runs);
    assert_eq!(names(&seeded), ["Night of the Living Dead (1990)"]);
    assert_eq!(seeded.total, 2);

    q.min_seeds = None;
    q.limit = 10;
    q.category = Some(Category::Video);
    assert_eq!(names(&search(&q, &runs)), ["Night of the Living Dead (1968)"]);
}

#[test]
fn hides_adult_releases_without_eating_lookalike_titles() {
    let mut film = row(&h(0x01), "xXx (2002) 1080p BluRay");
    film.category = Some(Category::Video);
    let by_category = row(&h(0x02), "xXx 2002 scene pack").flag(Flag::Adult, true);
    let by_name = row(&h(0x03), "Brazzers xXx 2002");
    let show = row(&h(0x04), "Sex Education S04 1080p");
    let runs = [fresh(&knaben(), vec![film, by_category, by_name, show])];

    let hidden = search(&SearchQuery::new("xxx 2002 sex education"), &runs);
    assert_eq!(names(&hidden), ["xXx (2002) 1080p BluRay", "Sex Education S04 1080p"]);
    assert_eq!((hidden.total, hidden.hidden_adult), (2, 2));

    let mut q = SearchQuery::new("xxx 2002 sex education");
    q.adult = true;
    let shown = search(&q, &runs);
    let adult: Vec<&str> =
        shown.releases.iter().filter(|r| r.flags.contains(&Flag::Adult)).map(|r| r.name.as_str()).collect();
    assert_eq!(adult, ["xXx 2002 scene pack", "Brazzers xXx 2002"]);
    assert_eq!((shown.total, shown.hidden_adult), (4, 0));
}

#[test]
fn flags_camera_releases_from_any_listing_name() {
    let mut cam = row(&h(0x05), "Some Movie 2024 HDCAM x264");
    cam.category = Some(Category::Video);
    let result = search(&SearchQuery::new("some movie 2024"), &[fresh(&knaben(), vec![cam])]);
    assert_eq!(result.releases[0].flags, BTreeSet::from([Flag::Cam]));
}

#[test]
fn every_source_failing_is_an_error_not_an_empty_list() {
    let q = SearchQuery::new("ubuntu");
    let runs = [
        failed(&apibay(), Failure::Http(503)),
        failed(&knaben(), Failure::Timeout),
        failed(&archive(), Failure::Irrelevant),
    ];
    let err = assemble(&q, &Terms::parse("ubuntu"), &runs, 6000).unwrap_err();
    assert_eq!(err.code, ErrorCode::ProvidersUnavailable);
    assert_eq!(err.message, "no source answered (archive.org, knaben, apibay)");
    assert_eq!(
        err.details,
        Some(Details::Sources(vec![
            SourceReport { source: "archive.org".into(), outcome: SourceOutcome::Irrelevant, elapsed_ms: 6000 },
            SourceReport { source: "knaben".into(), outcome: SourceOutcome::Timeout, elapsed_ms: 6000 },
            SourceReport { source: "apibay".into(), outcome: SourceOutcome::Http { status: 503 }, elapsed_ms: 6000 },
        ]))
    );
}

#[test]
fn one_empty_answer_is_a_successful_search_with_no_results() {
    let runs = [failed(&apibay(), Failure::Http(503)), fresh(&knaben(), Vec::new())];
    let result = search(&SearchQuery::new("zzqqxx"), &runs);
    assert_eq!(result.releases, []);
    assert_eq!(
        result.sources,
        [
            SourceReport { source: "knaben".into(), outcome: SourceOutcome::Ok { results: 0 }, elapsed_ms: 40 },
            SourceReport { source: "apibay".into(), outcome: SourceOutcome::Http { status: 503 }, elapsed_ms: 6000 },
        ]
    );
}

/// A tiny deterministic generator, so the shuffles are the same on every run.
struct XorShift(u64);

impl XorShift {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i + 1));
        }
    }
}

/// Sixty listings with ties in relevance, seeders, and size, the same torrent from
/// several sources and twice from one source, a hybrid pair, and locators.
fn crowded() -> Vec<(Arc<SourceMeta>, Vec<Observation>)> {
    let titles = [
        "Night of the Living Dead 1968",
        "Night of the Living Dead (1968) 1080p",
        "Night of the Living Dead 1990",
        "Living Dead",
        "Night of the Living Dead",
    ];
    let sources = [archive(), knaben(), apibay(), source("tracker", 8, false, false)];
    let mut by_source: Vec<(Arc<SourceMeta>, Vec<Observation>)> =
        sources.iter().map(|s| (s.clone(), Vec::new())).collect();
    for i in 0..60u32 {
        let id = u8::try_from(i % 23 + 1).unwrap();
        let mut o = row(&h(id), titles[i as usize % titles.len()]);
        o.peers = [None, Some(Peers { seeders: 10, leechers: Some(1) }), Some(Peers { seeders: 10, leechers: None })]
            [i as usize % 3];
        o.size = [None, Some(700), Some(700), Some(1400)][i as usize % 4];
        o.published = [None, Some(50), Some(50)][i as usize % 3];
        o.trackers = vec![format!("udp://t{}.example:80/announce", i % 5)];
        if i % 11 == 0 {
            o.identity = Identity::Locator { torrent_url: format!("https://example.org/{}.torrent", i % 2) };
        }
        by_source[i as usize % sources.len()].1.push(o);
    }
    let hybrid = Observation::new(
        Identity::Hash { hash: InfoHash::Hybrid { v1: [0x01; 20], v2: [0xfe; 32] }, torrent_url: None },
        "Night of the Living Dead 1968 hybrid",
    )
    .unwrap();
    by_source[0].1.push(hybrid);
    by_source[1].1.push(row(&"fe".repeat(32), "Night of the Living Dead 1968"));
    by_source
}

#[test]
fn a_thousand_shuffles_give_one_order() {
    let query = SearchQuery { limit: 100, ..SearchQuery::new("night of the living dead 1968") };
    let terms = Terms::parse(&query.text);
    let base = crowded();
    let runs_from = |groups: &[(Arc<SourceMeta>, Vec<Observation>)]| -> Vec<Run> {
        groups.iter().map(|(s, rows)| fresh(s, rows.clone())).collect()
    };
    let (expected, expected_linked) = assemble(&query, &terms, &runs_from(&base), 0).unwrap();
    assert_eq!(expected.releases.len(), 25);
    // The hybrid's group joins its v2 listing and keeps its longest listing name.
    let hybrid = expected.releases.iter().find(|r| r.id.to_ref() == h(0x01)).unwrap();
    assert_eq!(hybrid.name, "Night of the Living Dead (1968) 1080p");
    assert_eq!(hybrid.sources, ["archive.org", "knaben", "apibay", "tracker"]);
    assert!(hybrid.magnet.as_deref().unwrap().contains(&format!("xt=urn:btmh:1220{}", "fe".repeat(32))));

    let mut rng = XorShift(0x9e37_79b9_7f4a_7c15);
    for _ in 0..1000 {
        let mut groups = base.clone();
        rng.shuffle(&mut groups);
        for (_, rows) in &mut groups {
            rng.shuffle(rows);
        }
        let (shuffled, linked) = assemble(&query, &terms, &runs_from(&groups), 0).unwrap();
        assert_eq!(shuffled.releases, expected.releases);
        assert_eq!(linked, expected_linked);
        assert_eq!(shuffled.sources, expected.sources);
    }
}
