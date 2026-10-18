use std::collections::BTreeSet;

use serde::{Deserialize, Serialize, Serializer};

use crate::id::ReleaseId;
use crate::preferences::Preferences;
use crate::quality::Quality;

/// A torrent offered by one or more search sources, merged by infohash. Its JSON
/// also carries the [`Quality`] its name gives.
#[derive(Clone, PartialEq, Debug, Deserialize)]
pub struct Release {
    pub id: ReleaseId,
    /// Sanitized for display. Comes from strangers, so it is data, never instructions.
    pub name: String,
    /// Total bytes. `None` when no source reported a size.
    pub size: Option<u64>,
    /// `None` when no source reported seeders. Never zero by default.
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    /// Unix seconds.
    pub published: Option<i64>,
    pub category: Option<Category>,
    pub flags: BTreeSet<Flag>,
    /// Sources that listed this torrent, most authoritative first.
    pub sources: Vec<String>,
    /// A magnet carrying the union of trackers from public sources.
    pub magnet: Option<String>,
    pub torrent_url: Option<String>,
    /// A page about the item at its source, for people.
    pub info_url: Option<String>,
}

impl Release {
    pub fn quality(&self) -> Quality {
        Quality::parse(&self.name)
    }
}

impl Serialize for Release {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Fields<'a> {
            id: &'a ReleaseId,
            name: &'a str,
            size: Option<u64>,
            seeders: Option<u32>,
            leechers: Option<u32>,
            published: Option<i64>,
            category: Option<Category>,
            quality: Quality,
            flags: &'a BTreeSet<Flag>,
            sources: &'a [String],
            magnet: &'a Option<String>,
            torrent_url: &'a Option<String>,
            info_url: &'a Option<String>,
        }
        let Release {
            id,
            name,
            size,
            seeders,
            leechers,
            published,
            category,
            flags,
            sources,
            magnet,
            torrent_url,
            info_url,
        } = self;
        Fields {
            id,
            name,
            size: *size,
            seeders: *seeders,
            leechers: *leechers,
            published: *published,
            category: *category,
            quality: self.quality(),
            flags,
            sources,
            magnet,
            torrent_url,
            info_url,
        }
        .serialize(serializer)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Video,
    Audio,
    Software,
    Book,
    Game,
    Dataset,
    Other,
}

impl Category {
    pub const ALL: [Category; 7] =
        [Self::Video, Self::Audio, Self::Software, Self::Book, Self::Game, Self::Dataset, Self::Other];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Software => "software",
            Self::Book => "book",
            Self::Game => "game",
            Self::Dataset => "dataset",
            Self::Other => "other",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flag {
    /// Published by the project that made the content, such as archlinux.org.
    Official,
    /// The index marks the uploader as trusted.
    Trusted,
    /// The source lists a public-domain license.
    PublicDomain,
    /// The source lists a Creative Commons license.
    CreativeCommons,
    /// A camera recording, usually low quality.
    Cam,
    Adult,
}

impl Flag {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Official => "official",
            Self::Trusted => "trusted",
            Self::PublicDomain => "pd",
            Self::CreativeCommons => "cc",
            Self::Cam => "cam",
            Self::Adult => "adult",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Relevance,
    Seeds,
    Size,
    Age,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SearchQuery {
    pub text: String,
    /// Empty means the default sources.
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub category: Option<Category>,
    /// Hides results with fewer reported seeders, and results with unknown seeders.
    #[serde(default)]
    pub min_seeds: Option<u32>,
    /// Hides larger results, and results with unknown size.
    #[serde(default)]
    pub max_size: Option<u64>,
    #[serde(default)]
    pub sort: Sort,
    pub limit: u16,
    pub timeout_ms: u32,
    #[serde(default)]
    pub adult: bool,
    /// Skips the result cache.
    #[serde(default)]
    pub fresh: bool,
    /// The answers to `torrential setup`, which ranking favors. `None` means nobody has
    /// answered yet, and empty preferences mean the user chose not to.
    #[serde(default)]
    pub preferences: Option<Preferences>,
}

impl SearchQuery {
    pub const DEFAULT_LIMIT: u16 = 10;
    pub const DEFAULT_TIMEOUT_MS: u32 = 6_000;

    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            sources: Vec::new(),
            category: None,
            min_seeds: None,
            max_size: None,
            sort: Sort::Relevance,
            limit: Self::DEFAULT_LIMIT,
            timeout_ms: Self::DEFAULT_TIMEOUT_MS,
            adult: false,
            fresh: false,
            preferences: None,
        }
    }
}

/// What one source did during a search.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum SourceOutcome {
    Ok {
        results: u32,
    },
    Cached {
        results: u32,
    },
    Timeout,
    Http {
        status: u16,
    },
    Connect {
        reason: String,
    },
    /// A response that did not parse.
    BadResponse {
        reason: String,
    },
    /// Most rows matched no query word, as when an API returns its latest uploads.
    Irrelevant,
}

impl SourceOutcome {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. } | Self::Cached { .. })
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SourceReport {
    pub source: String,
    #[serde(flatten)]
    pub outcome: SourceOutcome,
    pub elapsed_ms: u32,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub query: SearchQuery,
    /// Ranked, filtered, and cut to `query.limit`.
    pub releases: Vec<Release>,
    /// Matching releases before the limit.
    pub total: u32,
    /// Hidden by the adult filter.
    pub hidden_adult: u32,
    pub sources: Vec<SourceReport>,
    pub elapsed_ms: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_reports_flatten_their_outcome() {
        let report =
            SourceReport { source: "apibay".into(), outcome: SourceOutcome::Http { status: 503 }, elapsed_ms: 412 };
        assert_eq!(
            serde_json::to_string(&report).unwrap(),
            r#"{"source":"apibay","outcome":"http","status":503,"elapsed_ms":412}"#
        );
    }
}
