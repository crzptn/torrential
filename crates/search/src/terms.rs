//! The words of a query, and how well a release name covers them.

use std::collections::HashSet;

/// Words that say little on their own. They count toward coverage, but a row that
/// matches only these is not evidence that a source searched for the query.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "at", "by", "de", "der", "die", "das", "el", "for", "in", "la", "le", "of", "on", "or", "the",
    "to", "with",
];

/// A query split into lowercase words, never with words dropped, so "The Office"
/// keeps "the".
#[derive(Clone, Debug)]
pub(crate) struct Terms {
    text: String,
    words: Vec<String>,
    year: Option<u16>,
}

/// How a name relates to the year in the query. Ordered worst to best.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum YearMatch {
    /// The name carries a different year.
    Other,
    /// The name carries no year, or the query has none.
    Absent,
    Same,
}

/// Compared field by field: more query words first, then the year.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct Relevance {
    pub matched: u16,
    pub year: YearMatch,
}

impl Terms {
    pub fn parse(text: &str) -> Terms {
        let text = torrential_core::text::sanitize(text).to_lowercase();
        let mut words: Vec<String> = Vec::new();
        for word in tokens(&text) {
            if !words.contains(&word) {
                words.push(word);
            }
        }
        let year = words.iter().find_map(|w| year(w));
        Terms { text, words, year }
    }

    /// The query as sent to sources, and the cache key: sanitized and lowercased.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn words(&self) -> &[String] {
        &self.words
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn relevance(&self, name: &str) -> Relevance {
        let name_tokens = tokens(name);
        let matched = self.matches(name, &name_tokens).iter().filter(|hit| **hit).count();
        let year = match self.year {
            None => YearMatch::Absent,
            Some(wanted) => {
                let years: Vec<u16> = name_tokens.iter().filter_map(|t| year(t)).collect();
                if years.contains(&wanted) {
                    YearMatch::Same
                } else if years.is_empty() {
                    YearMatch::Absent
                } else {
                    YearMatch::Other
                }
            }
        };
        Relevance {
            matched: u16::try_from(matched).unwrap_or(u16::MAX),
            year,
        }
    }

    /// True when the name matches a query word that is not a stopword, or any word
    /// when the query is only stopwords.
    pub fn is_relevant(&self, name: &str) -> bool {
        let hits = self.matches(name, &tokens(name));
        let all_stopwords = self.words.iter().all(|w| STOPWORDS.contains(&w.as_str()));
        self.words
            .iter()
            .zip(hits)
            .any(|(word, hit)| hit && (all_stopwords || !STOPWORDS.contains(&word.as_str())))
    }

    /// True when the name matches every query word. Catalogs searched locally use this.
    pub fn covers(&self, name: &str) -> bool {
        !self.words.is_empty() && self.matches(name, &tokens(name)).iter().all(|hit| *hit)
    }

    fn matches(&self, name: &str, name_tokens: &[String]) -> Vec<bool> {
        let set: HashSet<&str> = name_tokens.iter().map(String::as_str).collect();
        // Scripts written without spaces never split into whole words, so a
        // non-ASCII word matches anywhere in the name.
        let folded = name.to_lowercase();
        let mut hits: Vec<bool> = self
            .words
            .iter()
            .map(|w| {
                if w.is_ascii() {
                    set.contains(w.as_str())
                } else {
                    folded.contains(w.as_str())
                }
            })
            .collect();
        // Names often join words the query separates: "arch linux" in "archlinux".
        for run in 2..=3 {
            for (start, window) in self.words.windows(run).enumerate() {
                if set.contains(window.concat().as_str()) {
                    hits[start..start + run].fill(true);
                }
            }
        }
        hits
    }
}

fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn year(token: &str) -> Option<u16> {
    let n: u16 = token.parse().ok().filter(|_| token.len() == 4)?;
    (1900..=2099).contains(&n).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_words_and_joined_words() {
        let terms = Terms::parse("Arch Linux ISO");
        let r = terms.relevance("archlinux-2026.10.01-x86_64.iso");
        assert_eq!(
            r,
            Relevance {
                matched: 3,
                year: YearMatch::Absent
            }
        );
        assert!(terms.covers("archlinux-2026.10.01-x86_64.iso"));
        assert!(!terms.covers("ubuntu-24.04.5.1-desktop-amd64.iso"));
    }

    #[test]
    fn ranks_the_query_year_above_a_missing_one_above_another() {
        let terms = Terms::parse("night of the living dead 1968");
        let same = terms.relevance("Night of the Living Dead (1968) 1080p");
        let absent = terms.relevance("Night of the Living Dead 1080p");
        let other = terms.relevance("Night of the Living Dead (1990) 1080p");
        assert_eq!(
            same,
            Relevance {
                matched: 6,
                year: YearMatch::Same
            }
        );
        assert_eq!(
            absent,
            Relevance {
                matched: 5,
                year: YearMatch::Absent
            }
        );
        assert_eq!(
            other,
            Relevance {
                matched: 5,
                year: YearMatch::Other
            }
        );
        assert!(same > absent && absent > other);
    }

    #[test]
    fn stopwords_alone_do_not_make_a_row_relevant() {
        let terms = Terms::parse("the office");
        assert!(terms.is_relevant("The Office S01 720p"));
        assert!(!terms.is_relevant("The Matrix 1999"));
        assert!(Terms::parse("the").is_relevant("The Matrix 1999"));
    }

    #[test]
    fn non_latin_words_match_inside_longer_runs() {
        let terms = Terms::parse("進撃の巨人");
        assert!(terms.covers("[Group] 進撃の巨人 The Final Season 01"));
        assert_eq!(Terms::parse("  Ubuntu\t24.04 ").text(), "ubuntu 24.04");
    }
}
