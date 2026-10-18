//! The answers to `torrential setup`: what the user watches on, how big one download
//! may be, and what their player handles. Search ranks with them, and `get` takes its
//! size cap from them. Each answer is optional, and a skipped question is no preference.

use serde::{Deserialize, Serialize};

use crate::quality::{Codec, Quality, Resolution};
use crate::units::format_limit;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Preferences {
    #[serde(default)]
    pub screen: Option<Screen>,
    /// Bytes.
    #[serde(default)]
    pub max_size: Option<u64>,
    #[serde(default)]
    pub playback: Option<Playback>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Screen {
    #[serde(rename = "phone")]
    Phone,
    #[serde(rename = "laptop")]
    Laptop,
    #[serde(rename = "tv")]
    Tv,
    #[serde(rename = "tv-4k")]
    Tv4k,
}

impl Screen {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Phone => "phone",
            Self::Laptop => "laptop",
            Self::Tv => "tv",
            Self::Tv4k => "tv-4k",
        }
    }

    /// 0 for the best resolution on this screen, then one step lower, then higher,
    /// then further lower. A phone takes 720p as readily as 1080p.
    const fn rank(self, resolution: Resolution) -> u8 {
        use Resolution::{P720, P1080, P2160, Sd};
        match (self, resolution) {
            (Self::Phone, P1080 | P720) => 0,
            (Self::Phone, Sd) => 1,
            (Self::Phone, P2160) => 2,
            (Self::Laptop | Self::Tv, P1080) => 0,
            (Self::Laptop | Self::Tv, P720) => 1,
            (Self::Laptop | Self::Tv, P2160) => 2,
            (Self::Laptop | Self::Tv, Sd) => 3,
            (Self::Tv4k, P2160) => 0,
            (Self::Tv4k, P1080) => 1,
            (Self::Tv4k, P720) => 2,
            (Self::Tv4k, Sd) => 3,
        }
    }

    const fn phrase(self) -> &'static str {
        match self {
            Self::Phone => "a phone or tablet",
            Self::Laptop => "a laptop or monitor",
            Self::Tv => "a 1080p TV",
            Self::Tv4k => "a 4K TV",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Playback {
    #[serde(rename = "hevc-hdr")]
    HevcHdr,
    #[serde(rename = "hevc")]
    Hevc,
    /// The answer for a user who is not sure, since every player handles H.264.
    #[serde(rename = "h264")]
    H264,
}

impl Playback {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HevcHdr => "hevc-hdr",
            Self::Hevc => "hevc",
            Self::H264 => "h264",
        }
    }

    /// AV1 is newer than HEVC, so a player that may not handle HEVC may not handle it
    /// either. An unknown codec plays.
    const fn plays(self, quality: Quality) -> bool {
        let newer_codec = matches!(quality.codec, Some(Codec::Hevc | Codec::Av1));
        match self {
            Self::HevcHdr => true,
            Self::Hevc => !quality.hdr,
            Self::H264 => !quality.hdr && !newer_codec,
        }
    }

    const fn phrase(self) -> &'static str {
        match self {
            Self::HevcHdr => "plays HEVC and HDR",
            Self::Hevc => "plays HEVC, not HDR",
            Self::H264 => "avoids HEVC and HDR",
        }
    }
}

impl Preferences {
    /// False only for a release known to be larger than `max_size`, or known to need a
    /// codec or HDR the player may not handle. Unknown values fit.
    pub fn fits(&self, size: Option<u64>, quality: Quality) -> bool {
        let small = match (size, self.max_size) {
            (Some(size), Some(max)) => size <= max,
            _ => true,
        };
        small && self.playback.is_none_or(|p| p.plays(quality))
    }

    /// Lower is closer to the screen's best resolution. Without a screen every release
    /// ties at 0. With one, an unknown resolution comes after every known one, since a
    /// name without a tag gives no sign that it suits the screen.
    pub fn closeness(&self, resolution: Option<Resolution>) -> u8 {
        match (self.screen, resolution) {
            (None, _) => 0,
            (Some(screen), Some(resolution)) => screen.rank(resolution),
            (Some(_), None) => u8::MAX,
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Preferences::default()
    }

    /// `a 4K TV, up to 8 GB, plays HEVC and HDR`, naming what each unanswered
    /// question leaves open.
    pub fn describe(&self) -> String {
        let screen = self.screen.map_or("any screen", Screen::phrase);
        let size = self.max_size.map_or_else(|| "any size".to_string(), |max| format!("up to {}", format_limit(max)));
        let playback = self.playback.map_or("plays anything", Playback::phrase);
        format!("{screen}, {size}, {playback}")
    }

    pub fn answer(&mut self, answer: Answer) {
        match answer {
            Answer::Screen(screen) => self.screen = Some(screen),
            Answer::MaxSize(max) => self.max_size = max,
            Answer::Playback(playback) => self.playback = Some(playback),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
    Screen(Screen),
    /// `None` is no limit.
    MaxSize(Option<u64>),
    Playback(Playback),
}

pub struct Question {
    pub text: &'static str,
    pub choices: &'static [(&'static str, Answer)],
}

/// What `torrential setup` asks, in order, with the label of each choice.
pub const QUESTIONS: [Question; 3] = [
    Question {
        text: "What do you usually watch on?",
        choices: &[
            ("Phone or tablet", Answer::Screen(Screen::Phone)),
            ("Laptop or monitor", Answer::Screen(Screen::Laptop)),
            ("1080p TV", Answer::Screen(Screen::Tv)),
            ("4K TV", Answer::Screen(Screen::Tv4k)),
        ],
    },
    Question {
        text: "How big can one download be?",
        choices: &[
            ("2 GB", Answer::MaxSize(Some(2_000_000_000))),
            ("8 GB", Answer::MaxSize(Some(8_000_000_000))),
            ("20 GB", Answer::MaxSize(Some(20_000_000_000))),
            ("No limit", Answer::MaxSize(None)),
        ],
    },
    Question {
        text: "Can your player handle HEVC (x265) and HDR?",
        choices: &[
            ("Yes to both", Answer::Playback(Playback::HevcHdr)),
            ("HEVC but not HDR", Answer::Playback(Playback::Hevc)),
            ("Not sure", Answer::Playback(Playback::H264)),
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn quality(name: &str) -> Quality {
        Quality::parse(name)
    }

    #[test]
    fn a_player_that_may_not_handle_hevc_or_hdr_fits_only_what_it_plays() {
        let names = ["Film 1080p x264", "Film 2160p x265", "Film 2160p HDR x265", "Film 1080p AV1", "Film"];
        let fits = |playback| {
            let prefs = Preferences { playback, ..Preferences::default() };
            names.map(|name| prefs.fits(None, quality(name)))
        };
        assert_eq!(fits(None), [true, true, true, true, true]);
        assert_eq!(fits(Some(Playback::HevcHdr)), [true, true, true, true, true]);
        assert_eq!(fits(Some(Playback::Hevc)), [true, true, false, true, true]);
        assert_eq!(fits(Some(Playback::H264)), [true, false, false, false, true]);
    }

    #[test]
    fn a_size_fits_up_to_the_limit_and_an_unknown_size_fits() {
        let prefs = Preferences { max_size: Some(2_000_000_000), ..Preferences::default() };
        let sizes = [Some(1_999_999_999), Some(2_000_000_000), Some(2_000_000_001), None];
        assert_eq!(sizes.map(|size| prefs.fits(size, Quality::default())), [true, true, false, true]);
    }

    #[test]
    fn closeness_puts_the_screens_resolution_first_and_unknown_last() {
        use Resolution::{P720, P1080, P2160, Sd};
        let order = |screen| {
            let prefs = Preferences { screen, ..Preferences::default() };
            [Some(P2160), Some(P1080), Some(P720), Some(Sd), None].map(|r| prefs.closeness(r))
        };
        assert_eq!(order(None), [0, 0, 0, 0, 0]);
        assert_eq!(order(Some(Screen::Phone)), [2, 0, 0, 1, u8::MAX]);
        assert_eq!(order(Some(Screen::Laptop)), [2, 0, 1, 3, u8::MAX]);
        assert_eq!(order(Some(Screen::Tv)), [2, 0, 1, 3, u8::MAX]);
        assert_eq!(order(Some(Screen::Tv4k)), [0, 1, 2, 3, u8::MAX]);
    }

    #[test]
    fn describes_answers_and_what_skipped_ones_leave_open() {
        let all = Preferences {
            screen: Some(Screen::Tv4k),
            max_size: Some(8_000_000_000),
            playback: Some(Playback::HevcHdr),
        };
        let some = Preferences { screen: Some(Screen::Laptop), ..Preferences::default() };
        assert_eq!(all.describe(), "a 4K TV, up to 8 GB, plays HEVC and HDR");
        assert_eq!(some.describe(), "a laptop or monitor, any size, plays anything");
    }

    #[test]
    fn answers_fill_in_preferences_and_no_limit_clears_the_size() {
        let mut prefs = Preferences { max_size: Some(1), ..Preferences::default() };
        for (question, choice) in QUESTIONS.iter().zip([3, 3, 2]) {
            prefs.answer(question.choices[choice].1);
        }
        assert_eq!(prefs, Preferences { screen: Some(Screen::Tv4k), max_size: None, playback: Some(Playback::H264) });
    }
}
