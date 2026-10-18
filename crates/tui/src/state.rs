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

/// Where keys go. `Files` means the cursor is inside the selected item's files: a result's tree,
/// or the list of a download's files.
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

/// A download's files with their progress, when they were last asked for, and where the
/// cursor is among the picked ones.
#[derive(Clone, PartialEq, Debug)]
pub struct DownloadFiles {
    pub files: Vec<TorrentFile>,
    pub asked: Instant,
    /// An index into [`DownloadFiles::picked`].
    pub cursor: usize,
    /// The first picked file shown while the cursor is inside the list.
    pub scroll: usize,
}

impl DownloadFiles {
    pub fn new(asked: Instant) -> Self {
        Self { files: Vec::new(), asked, cursor: 0, scroll: 0 }
    }

    /// The files the download gets, which are the ones it lists.
    pub fn picked(&self) -> Vec<&TorrentFile> {
        self.files.iter().filter(|f| f.selected).collect()
    }

    pub fn move_by(&mut self, delta: isize) {
        self.cursor = self.cursor.saturating_add_signed(delta);
    }

    /// Keeps the cursor on a file and inside the `rows` shown, with a file past it in view so
    /// the arrow saying more lie beyond stays visible.
    pub fn settle(&mut self, rows: usize) {
        let count = self.picked().len();
        self.cursor = self.cursor.min(count.saturating_sub(1));
        let context = usize::from(rows > 2);
        if self.cursor < self.scroll + context {
            self.scroll = self.cursor.saturating_sub(context);
        } else if rows > 0 && self.cursor + context >= self.scroll + rows {
            self.scroll = self.cursor + context + 1 - rows;
        }
        self.scroll = self.scroll.min(count.saturating_sub(rows.max(1)));
    }
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

/// The margin's word for a download in each state.
pub fn label(status: &JobStatus) -> &'static str {
    match status {
        JobStatus::Downloading | JobStatus::Metadata => "getting",
        JobStatus::Checking => "checking",
        JobStatus::Queued => "waiting",
        JobStatus::Error { .. } => "failed",
        JobStatus::Interrupted => "stopped",
        JobStatus::Paused => "paused",
        JobStatus::Done { .. } => "done",
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
    /// When bytes last arrived for each download, or when it last started trying to get them.
    pub arrived: HashMap<TorrentId, Instant>,
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
    /// A download whose removal waits for the user to confirm it.
    pub confirm: Option<TorrentId>,
    /// The footer's message. A new one replaces it.
    pub toast: Option<Toast>,
    pub quit: bool,
    pub next_search: u64,
}

/// An ellipsis that writes itself out after the word it follows. Every frame is three cells, so
/// nothing after it moves.
pub const SPINNER: [&str; 4] = ["   ", ".  ", ".. ", "..."];
/// Four frames a second, the most any motion on the screen may use.
pub const SPIN_EVERY: Duration = Duration::from_millis(250);
/// How long a download goes without bytes before it counts as stalled.
pub const STALL_AFTER: Duration = Duration::from_secs(30);
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
            arrived: HashMap::new(),
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
            confirm: None,
            toast: None,
            quit: false,
            next_search: 1,
        }
    }

    fn frames(&self) -> u128 {
        self.now.duration_since(self.started).as_millis() / SPIN_EVERY.as_millis()
    }

    pub fn spinner(&self) -> &'static str {
        SPINNER[self.frames() as usize % SPINNER.len()]
    }

    /// When the spinner's next frame is due. Counted from the start, so messages arriving in
    /// between do not push it back.
    pub fn next_frame(&self) -> Instant {
        self.started + SPIN_EVERY * (self.frames() as u32 + 1)
    }

    pub fn job(&self, id: TorrentId) -> Option<&Job> {
        self.downloads.iter().find(|j| j.id == id)
    }

    /// Whether a download has gone [`STALL_AFTER`] without bytes.
    pub fn stalled(&self, job: &Job) -> bool {
        self.quiet(job).is_some_and(|at| at + STALL_AFTER <= self.now)
    }

    /// When the next quiet download counts as stalled, so the screen can say so unprompted.
    pub fn next_stall(&self) -> Option<Instant> {
        self.downloads.iter().filter_map(|j| self.quiet(j)).map(|at| at + STALL_AFTER).filter(|at| *at > self.now).min()
    }

    /// When bytes last arrived for a download that wants them and gets none.
    fn quiet(&self, job: &Job) -> Option<Instant> {
        let waiting = job.status == JobStatus::Downloading && job.progress.down_rate == 0;
        self.arrived.get(&job.id).copied().filter(|_| waiting)
    }

    /// Notes bytes arriving, or a download starting to want them, which restarts the wait. Call
    /// it before `job` replaces the state's copy, which tells whether it was downloading.
    pub fn note_arrival(&mut self, job: &Job) {
        let was_downloading = self.job(job.id).is_some_and(|j| j.status == JobStatus::Downloading);
        if job.progress.down_rate > 0 || !was_downloading {
            self.arrived.insert(job.id, self.now);
        }
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
