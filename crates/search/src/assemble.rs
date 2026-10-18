//! From what each source answered to one ranked `SearchResult`. Pure, and
//! independent of answer and row order, so a human and an agent get the same list.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use sha1::{Digest, Sha1};
use torrential_core::error::Details;
use torrential_core::{
    Error, ErrorCode, Flag, InfoHash, LocatorId, Release, ReleaseId, SearchQuery, SearchResult, Sort, SourceReport,
    TorrentId,
};

use crate::classify;
use crate::observation::{Identity, Observation, build_magnet};
use crate::sources::{Failure, SourceMeta};
use crate::terms::{Relevance, Terms};

const MAX_TRACKERS: usize = 32;

pub(crate) struct Run {
    pub source: Arc<SourceMeta>,
    pub answer: Answer,
    pub elapsed_ms: u32,
}

pub(crate) enum Answer {
    Fresh(Arc<Vec<Observation>>),
    Cached(Arc<Vec<Observation>>),
    Failed(Failure),
}

/// Keeps the rows whose name matches a query word. When most rows match none, the
/// source answered something other than the query, such as its latest uploads, and
/// none of its rows are kept.
pub(crate) fn vet(terms: &Terms, rows: Vec<Observation>) -> Result<Vec<Observation>, Failure> {
    let total = rows.len();
    let kept: Vec<Observation> = rows.into_iter().filter(|o| terms.is_relevant(o.name())).collect();
    if (total - kept.len()) * 2 > total {
        Err(Failure::Irrelevant)
    } else {
        Ok(kept)
    }
}

/// Fails with `providers_unavailable` when no source answered. An answer with no
/// rows is still an answer.
pub(crate) fn assemble(
    query: &SearchQuery,
    terms: &Terms,
    runs: &[Run],
    elapsed_ms: u32,
) -> Result<SearchResult, Error> {
    let mut runs: Vec<&Run> = runs.iter().collect();
    runs.sort_by_key(|r| r.source.authority);
    let sources: Vec<SourceReport> = runs.iter().map(|r| report(r)).collect();
    if !sources.iter().any(|r| r.outcome.is_ok()) {
        return Err(all_failed(sources));
    }
    let sightings = runs.iter().flat_map(|run| {
        let rows: &[Observation] = match &run.answer {
            Answer::Fresh(rows) | Answer::Cached(rows) => rows,
            Answer::Failed(_) => &[],
        };
        rows.iter().map(|row| (&*run.source, row))
    });
    let (mut kept, hidden_adult) = filter(merge(sightings, terms), query);
    rank(&mut kept, query.sort);
    let total = u32::try_from(kept.len()).unwrap_or(u32::MAX);
    kept.truncate(query.limit.into());
    Ok(SearchResult {
        query: query.clone(),
        releases: kept.into_iter().map(|c| c.release).collect(),
        total,
        hidden_adult,
        sources,
        elapsed_ms,
    })
}

fn report(run: &Run) -> SourceReport {
    let count = |rows: &Vec<Observation>| u32::try_from(rows.len()).unwrap_or(u32::MAX);
    let outcome = match &run.answer {
        Answer::Fresh(rows) => torrential_core::SourceOutcome::Ok { results: count(rows) },
        Answer::Cached(rows) => torrential_core::SourceOutcome::Cached { results: count(rows) },
        Answer::Failed(failure) => failure.outcome(),
    };
    SourceReport {
        source: run.source.name.clone(),
        outcome,
        elapsed_ms: run.elapsed_ms,
    }
}

fn all_failed(sources: Vec<SourceReport>) -> Error {
    let names: Vec<&str> = sources.iter().map(|r| r.source.as_str()).collect();
    Error::new(
        ErrorCode::ProvidersUnavailable,
        format!("no source answered ({})", names.join(", ")),
    )
    .with_hint("check the network connection, or retry with a longer --timeout")
    .with_details(Details::Sources(sources))
}

/// A merged release, with what ranking needs that `Release` does not carry.
pub(crate) struct Candidate {
    pub release: Release,
    relevance: Relevance,
    lawful: bool,
}

type Sighting<'a> = (&'a SourceMeta, &'a Observation);

/// Groups listings by torrent. A hybrid listing ties its v2 ID to its v1 ID, so a
/// listing that only knows the v2 hash joins the same release.
fn merge<'a>(sightings: impl Iterator<Item = Sighting<'a>>, terms: &Terms) -> Vec<Candidate> {
    let sightings: Vec<Sighting<'a>> = sightings.collect();
    let aliases: HashMap<TorrentId, TorrentId> = sightings
        .iter()
        .filter_map(|(_, o)| match o.identity {
            Identity::Hash {
                hash: InfoHash::Hybrid { v1, v2 },
                ..
            } => Some((InfoHash::V2(v2).id(), TorrentId::from_bytes(v1))),
            _ => None,
        })
        .collect();
    let mut groups: HashMap<ReleaseId, Vec<Sighting<'a>>> = HashMap::new();
    for sighting in sightings {
        let id = match &sighting.1.identity {
            Identity::Hash { hash, .. } => {
                let id = hash.id();
                ReleaseId::Torrent(aliases.get(&id).copied().unwrap_or(id))
            }
            Identity::Locator { torrent_url } => {
                ReleaseId::Locator(LocatorId::from_bytes(Sha1::digest(torrent_url.as_bytes()).into()))
            }
        };
        groups.entry(id).or_default().push(sighting);
    }
    groups
        .into_iter()
        .map(|(id, group)| combine(id, &group, terms))
        .collect()
}

/// Each field comes from a rule that depends only on the set of listings, never on
/// their order: the most authoritative source wins, and equal authority falls back
/// to comparing values.
fn combine(id: ReleaseId, group: &[Sighting<'_>], terms: &Terms) -> Candidate {
    fn first<T: Ord + Clone>(group: &[Sighting<'_>], field: impl Fn(&Observation) -> Option<T>) -> Option<T> {
        group
            .iter()
            .filter_map(|(s, o)| field(o).map(|v| (s.authority, v)))
            .min()
            .map(|(_, v)| v)
    }
    let name = group
        .iter()
        .map(|(s, o)| (std::cmp::Reverse(o.name().chars().count()), s.authority, o.name()))
        .min()
        .map(|(.., name)| name.to_string())
        .unwrap_or_default();
    let category = first(group, |o| o.category);
    let peers = group.iter().filter_map(|(_, o)| o.peers).max();
    let mut flags: BTreeSet<Flag> = group.iter().flat_map(|(_, o)| o.flags.iter().copied()).collect();
    if group.iter().any(|(_, o)| classify::is_cam(o.name(), category)) {
        flags.insert(Flag::Cam);
    }
    if group.iter().any(|(_, o)| classify::is_adult(o.name())) {
        flags.insert(Flag::Adult);
    }
    let mut authorities: Vec<(u16, &str)> = group.iter().map(|(s, _)| (s.authority, s.name.as_str())).collect();
    authorities.sort();
    authorities.dedup();
    let any_public = group.iter().any(|(s, _)| s.public);
    let trackers: BTreeSet<&String> = group
        .iter()
        .filter(|(s, _)| s.public || !any_public)
        .flat_map(|(_, o)| &o.trackers)
        .collect();
    let trackers: Vec<String> = trackers.into_iter().take(MAX_TRACKERS).cloned().collect();
    let hash = combined_hash(group);
    let relevance = group
        .iter()
        .map(|(_, o)| terms.relevance(o.name()))
        .max()
        .unwrap_or(terms.relevance(""));
    Candidate {
        release: Release {
            id,
            magnet: hash.map(|h| build_magnet(&h, &name, &trackers)),
            name,
            size: first(group, |o| o.size),
            seeders: peers.map(|p| p.seeders),
            leechers: peers.and_then(|p| p.leechers),
            published: first(group, |o| o.published),
            category,
            flags,
            sources: authorities.into_iter().map(|(_, name)| name.to_string()).collect(),
            torrent_url: first(group, |o| o.torrent_url().map(str::to_string)),
            info_url: first(group, |o| o.info_url.clone()),
        },
        relevance,
        lawful: group.iter().any(|(s, _)| s.lawful),
    }
}

fn combined_hash(group: &[Sighting<'_>]) -> Option<InfoHash> {
    let hashes = group.iter().filter_map(|(_, o)| match &o.identity {
        Identity::Hash { hash, .. } => Some(*hash),
        Identity::Locator { .. } => None,
    });
    let (v1, v2) = hashes.fold((None, None), |(v1, v2), h| (v1.max(h.v1()), v2.max(h.v2())));
    match (v1, v2) {
        (Some(v1), Some(v2)) => Some(InfoHash::Hybrid { v1, v2 }),
        (Some(v1), None) => Some(InfoHash::V1(v1)),
        (None, Some(v2)) => Some(InfoHash::V2(v2)),
        (None, None) => None,
    }
}

/// Filters are strict: a release with an unknown size or seeder count fails a
/// `max_size` or `min_seeds` filter. Adult releases are hidden last, so
/// `hidden_adult` counts only those every other filter let through.
fn filter(candidates: Vec<Candidate>, q: &SearchQuery) -> (Vec<Candidate>, u32) {
    let mut hidden_adult = 0;
    let kept = candidates
        .into_iter()
        .filter(|c| {
            let r = &c.release;
            q.category.is_none_or(|want| r.category == Some(want))
                && q.min_seeds.is_none_or(|min| r.seeders.is_some_and(|s| s >= min))
                && q.max_size.is_none_or(|max| r.size.is_some_and(|s| s <= max))
        })
        .filter(|c| {
            let hide = !q.adult && c.release.flags.contains(&Flag::Adult);
            hidden_adult += u32::from(hide);
            !hide
        })
        .collect();
    (kept, hidden_adult)
}

/// One total order. `relevance` sorts by query relevance, then official and lawful
/// sources, then seeders, then size, then ID. The other sorts put their key first
/// and fall back to that order. Unknown values sort last.
fn rank(candidates: &mut [Candidate], sort: Sort) {
    candidates.sort_by(|a, b| {
        let (ra, rb) = (&a.release, &b.release);
        let primary = match sort {
            Sort::Relevance => Ordering::Equal,
            Sort::Seeds => known_first_desc(ra.seeders, rb.seeders),
            Sort::Size => known_first_desc(ra.size, rb.size),
            Sort::Age => known_first_desc(ra.published, rb.published),
        };
        primary
            .then_with(|| b.relevance.cmp(&a.relevance))
            .then_with(|| standing(b).cmp(&standing(a)))
            .then_with(|| known_first_desc(ra.seeders, rb.seeders))
            .then_with(|| known_first_desc(ra.size, rb.size))
            .then_with(|| ra.id.to_ref().cmp(&rb.id.to_ref()))
    });
}

fn standing(c: &Candidate) -> u8 {
    if c.release.flags.contains(&Flag::Official) {
        2
    } else {
        u8::from(c.lawful)
    }
}

fn known_first_desc<T: Ord>(a: Option<T>, b: Option<T>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests;
