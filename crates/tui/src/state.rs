//! Everything the screen shows, and the derived views that both drawing and key handling read.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use torrential_core::{Error, Job, JobStatus, Release, ReleaseId, SourceReport, TorrentFile, TorrentId};

use crate::tree::Tree;

/// The one thing the cursor rests on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Item {
    Download(TorrentId),
    Result(ReleaseId),
}

/// Where keys go. `Files` means the cursor is inside the selected result's file tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Query,
    List,
    Files,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Engine {
    Starting,
    Ready,
    Unreachable(Error),
}

#[derive(Clone, PartialEq, Debug)]
pub enum SearchPhase {
    Running { pending: Vec<String> },
    Done,
    Failed(Error),
}

#[derive(Clone, PartialEq, Debug)]
pub struct Search {
    /// Tells this search's replies from those of a search it replaced.
    pub id: u64,
    pub text: String,
    pub started: Instant,
    /// In the engine's ranking.
    pub releases: Vec<Release>,
    pub sources: Vec<SourceReport>,
    pub elapsed_ms: u32,
    pub phase: SearchPhase,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sort {
    BestMatch,
    Seeds,
    Size,
    Age,
}

impl Sort {
    pub fn label(self) -> &'static str {
        match self {
            Self::BestMatch => "best match",
            Self::Seeds => "seeds",
            Self::Size => "size",
            Self::Age => "age",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::BestMatch => Self::Seeds,
            Self::Seeds => Self::Size,
            Self::Size => Self::Age,
            Self::Age => Self::BestMatch,
        }
    }
}

/// A one-line text field. `caret` counts characters.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Input {
    pub text: String,
    pub caret: usize,
}

/// A search result's file list.
#[derive(Clone, PartialEq, Debug)]
pub enum Files {
    Loading { since: Instant, then_download: bool },
    Ready(Tree),
    Failed(Error),
}

/// A download's files with their progress, and when they were last asked for.
#[derive(Clone, PartialEq, Debug)]
pub struct DownloadFiles {
    pub files: Vec<TorrentFile>,
    pub asked: Instant,
}

/// An action sent to the engine whose answer has not come back yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobAction {
    Pause,
    Resume,
    Verify,
    Remove,
}

impl JobAction {
    pub fn progressive(self) -> &'static str {
        match self {
            Self::Pause => "pausing",
            Self::Resume => "resuming",
            Self::Verify => "verifying",
            Self::Remove => "removing",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Palette {
    pub filter: String,
    pub selected: usize,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Overlay {
    Palette(Palette),
    ConfirmRemove(TorrentId),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Toast {
    pub text: String,
    pub tone: Tone,
    pub until: Instant,
}

/// The order downloads are listed in (DESIGN-BRIEF 4.4): active, then needing attention,
/// then paused, then done.
pub fn group(status: &JobStatus) -> u8 {
    match status {
        JobStatus::Downloading | JobStatus::Checking | JobStatus::Metadata | JobStatus::Queued => 0,
        JobStatus::Error { .. } | JobStatus::Interrupted => 1,
        JobStatus::Paused => 2,
        JobStatus::Done { .. } => 3,
    }
}

/// Folded downloads stay in the list but share one summary line.
pub fn folds(status: &JobStatus) -> bool {
    matches!(status, JobStatus::Paused | JobStatus::Done { .. })
}

pub struct State {
    pub now: Instant,
    /// Unix seconds, for ages.
    pub clock: i64,
    pub started: Instant,
    pub size: (u16, u16),
    pub home: Option<PathBuf>,
    pub engine: Engine,
    /// In display order, which changes only when the cursor moves or a download comes or goes,
    /// so a row never jumps away from under the cursor.
    pub downloads: Vec<Job>,
    pub download_files: HashMap<TorrentId, DownloadFiles>,
    pub pending: HashMap<TorrentId, JobAction>,
    pub free_space: Option<u64>,
    pub free_space_at: Option<Instant>,
    pub query: Input,
    pub search: Option<Search>,
    pub sort: Sort,
    pub files: HashMap<ReleaseId, Files>,
    pub focus: Focus,
    pub cursor: Option<Item>,
    /// The selected item's details are hidden.
    pub collapsed: bool,
    /// Until the user moves, the cursor follows the top result as results stream in.
    pub follow_top: bool,
    /// Whether a key was pressed yet. The first snapshot moves focus only before that.
    pub touched: bool,
    /// A result whose file list is fetched once the cursor has rested on it until this time.
    pub prefetch: Option<(ReleaseId, Instant)>,
    /// The first list line shown.
    pub scroll: u16,
    pub help: bool,
    pub overlay: Option<Overlay>,
    /// The footer's message. A new one replaces it.
    pub toast: Option<Toast>,
    pub reduced_motion: bool,
    pub quit: bool,
    pub next_search: u64,
}

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub const SPIN_EVERY: Duration = Duration::from_millis(80);
/// How long the cursor rests on a result before its file list is fetched.
pub const PREFETCH_AFTER: Duration = Duration::from_millis(300);

impl State {
    pub fn new(now: Instant, clock: i64, size: (u16, u16), home: Option<PathBuf>) -> Self {
        Self {
            now,
            clock,
            started: now,
            size,
            home,
            engine: Engine::Starting,
            downloads: Vec::new(),
            download_files: HashMap::new(),
            pending: HashMap::new(),
            free_space: None,
            free_space_at: None,
            query: Input::default(),
            search: None,
            sort: Sort::BestMatch,
            files: HashMap::new(),
            focus: Focus::Query,
            cursor: None,
            collapsed: false,
            follow_top: false,
            touched: false,
            prefetch: None,
            scroll: 0,
            help: false,
            overlay: None,
            toast: None,
            reduced_motion: false,
            quit: false,
            next_search: 1,
        }
    }

    pub fn spinner(&self) -> &'static str {
        if self.reduced_motion {
            return "…";
        }
        let frames = self.now.duration_since(self.started).as_millis() / SPIN_EVERY.as_millis();
        SPINNER[frames as usize % SPINNER.len()]
    }

    pub fn job(&self, id: TorrentId) -> Option<&Job> {
        self.downloads.iter().find(|j| j.id == id)
    }

    pub fn release(&self, id: ReleaseId) -> Option<&Release> {
        self.search.as_ref()?.releases.iter().find(|r| r.id == id)
    }

    /// Whether the query differs from the search it ran, so typing filters loaded results.
    pub fn filtering(&self) -> bool {
        let typed = self.query.text.trim();
        !typed.is_empty() && self.search.as_ref().is_some_and(|s| s.text != typed)
    }

    /// The results shown, filtered by what is typed and in the chosen sort.
    pub fn results(&self) -> Vec<&Release> {
        let Some(search) = &self.search else { return Vec::new() };
        let words: Vec<String> = if self.filtering() {
            self.query.text.split_whitespace().map(str::to_lowercase).collect()
        } else {
            Vec::new()
        };
        let mut shown: Vec<&Release> = search
            .releases
            .iter()
            .filter(|r| words.iter().all(|w| r.name.to_lowercase().contains(w.as_str())))
            .collect();
        match self.sort {
            Sort::BestMatch => {}
            Sort::Seeds => shown.sort_by_key(|r| std::cmp::Reverse(r.seeders)),
            Sort::Size => shown.sort_by_key(|r| std::cmp::Reverse(r.size)),
            Sort::Age => shown.sort_by_key(|r| std::cmp::Reverse(r.published)),
        }
        shown
    }

    pub fn in_downloads(&self) -> bool {
        matches!(self.cursor, Some(Item::Download(_))) && self.focus != Focus::Query
    }

    /// The downloads listed one per row. The rest fold into the summary line unless the
    /// cursor is in the downloads section.
    pub fn download_rows(&self) -> Vec<&Job> {
        let unfold = self.in_downloads();
        self.downloads.iter().filter(|j| unfold || !folds(&j.status)).collect()
    }

    /// Paused and done counts, when they fold.
    pub fn folded(&self) -> Option<(usize, usize)> {
        if self.in_downloads() {
            return None;
        }
        let paused = self.downloads.iter().filter(|j| j.status == JobStatus::Paused).count();
        let done = self.downloads.iter().filter(|j| matches!(j.status, JobStatus::Done { .. })).count();
        (paused + done > 0).then_some((paused, done))
    }

    /// Every item the cursor can rest on, top to bottom, with downloads unfolded.
    pub fn items(&self) -> Vec<Item> {
        let downloads = self.downloads.iter().map(|j| Item::Download(j.id));
        downloads.chain(self.results().into_iter().map(|r| Item::Result(r.id))).collect()
    }

    pub fn active(&self) -> usize {
        self.downloads
            .iter()
            .filter(|j| matches!(j.status, JobStatus::Downloading | JobStatus::Metadata | JobStatus::Checking))
            .count()
    }

    pub fn rate(&self) -> u64 {
        self.downloads.iter().map(|j| j.progress.down_rate).sum()
    }

    /// Sorts downloads into display order. Called when the cursor moves or the set changes.
    pub fn resort(&mut self) {
        self.downloads.sort_by_key(|j| (group(&j.status), std::cmp::Reverse(j.added_at)));
    }

    /// Whether anything on screen animates, so the spinner tick should run.
    pub fn spinning(&self) -> bool {
        if self.reduced_motion {
            return false;
        }
        let starting = self.engine == Engine::Starting;
        let searching = self.search.as_ref().is_some_and(|s| matches!(s.phase, SearchPhase::Running { .. }));
        let loading = self.files.values().any(|f| matches!(f, Files::Loading { .. }));
        let busy_job = self.downloads.iter().any(|j| matches!(j.status, JobStatus::Metadata | JobStatus::Checking));
        starting || searching || loading || busy_job || !self.pending.is_empty()
    }

    pub fn say(&mut self, tone: Tone, text: impl Into<String>) {
        let lasts = if tone == Tone::Error { Duration::from_secs(8) } else { Duration::from_secs(5) };
        self.toast = Some(Toast { text: text.into(), tone, until: self.now + lasts });
    }
}
