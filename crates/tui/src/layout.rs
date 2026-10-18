//! Where things go: the page's columns and rows, and the list as blocks of known height. Drawing
//! and scrolling both read this, so they always agree.

use ratatui::layout::Rect;
use torrential_core::{Job, JobStatus, ReleaseId, TorrentId};

use crate::state::{Engine, Files, Focus, Item, ROWS, SearchPhase, State, label};

pub const MIN_SIZE: (u16, u16) = (60, 16);
/// Margin labels are right-aligned in cells 1 to 10. `torrential` is the longest.
pub const MARGIN: u16 = 10;
/// The column between the margin and the entries, where the selection mark sits.
pub const MARK: u16 = MARGIN + 2;
/// Where entries start.
pub const CONTENT: u16 = MARK + 2;
/// Where an entry's details start: indented under it, the way a ledger sets off a sub-entry.
pub const DETAIL: u16 = CONTENT + 2;
/// Entries never run wider than this, so a name and its numbers stay within one glance.
const MAX_CONTENT: u16 = 100;
/// Rows kept visible above and below the selection when scrolling.
const CONTEXT: u16 = 2;

pub const MASTHEAD: u16 = 0;
pub const SEARCH: u16 = 2;
const LIST_TOP: u16 = 4;

pub struct Screen {
    /// The last column entries may use.
    pub right: u16,
    /// Rows between the search line and the footer. Help draws here too.
    pub list: Rect,
    pub footer: u16,
}

/// `None` when the terminal is too small to draw.
pub fn screen(size: (u16, u16)) -> Option<Screen> {
    let (w, h) = size;
    if w < MIN_SIZE.0 || h < MIN_SIZE.1 {
        return None;
    }
    let right = (w - 2).min(CONTENT + MAX_CONTENT);
    let list = Rect::new(0, LIST_TOP, right, h - LIST_TOP - 2);
    Some(Screen { right, list, footer: h - 1 })
}

pub fn list_height(state: &State) -> u16 {
    screen(state.size).map_or(0, |s| s.list.height)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Block {
    Download(TorrentId),
    DownloadDetail(TorrentId),
    /// Paused or done downloads counted on one line, under the margin label they share.
    Folded(&'static str),
    Gap,
    /// The ranking panel: what search would rank for, then a row for each override.
    Ranking,
    /// The results' margin label, the order they are in, and the column heads.
    Heads,
    Result(ReleaseId),
    ResultDetail(ReleaseId),
    /// What the results say when there are none to list.
    Empty,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Placed {
    pub block: Block,
    pub top: u16,
    pub height: u16,
}

/// Whether the item under the cursor shows its details. They close while the ranking panel has
/// the keys.
pub fn expanded(state: &State, item: Item) -> bool {
    state.cursor == Some(item) && !state.collapsed && state.focus != Focus::Query && state.ranking.is_none()
}

/// The list from top to bottom, for a list area `viewport` rows tall.
pub fn blocks(state: &State, viewport: u16) -> Vec<Placed> {
    let mut out = Vec::new();
    let mut top = 0;
    let mut push = |block, height| {
        out.push(Placed { block, top, height });
        top += height;
    };
    if !state.downloads.is_empty() {
        for job in state.download_rows() {
            push(Block::Download(job.id), 1);
            if expanded(state, Item::Download(job.id)) || state.confirm == Some(job.id) {
                push(Block::DownloadDetail(job.id), download_detail_height(state, job, viewport));
            }
        }
        if state.folded().is_some() {
            for fold in ["paused", "done"] {
                if state.downloads.iter().any(|j| label(&j.status) == fold) {
                    push(Block::Folded(fold), 1);
                }
            }
        }
        push(Block::Gap, 1);
    }
    if state.ranking.is_some() {
        push(Block::Ranking, 1 + ROWS.len() as u16);
        push(Block::Gap, 1);
    }
    let results = state.results();
    if results.is_empty() {
        if empty_message(state) {
            push(Block::Empty, 2);
        }
        return out;
    }
    push(Block::Heads, 1);
    for release in results {
        push(Block::Result(release.id), 1);
        if expanded(state, Item::Result(release.id)) {
            push(Block::ResultDetail(release.id), result_detail_height(state, release.id, viewport));
        }
    }
    out
}

/// Whether the empty results have something to say: the engine is down, nothing matched, or
/// the search failed. A search still running says so on the search line instead.
fn empty_message(state: &State) -> bool {
    match &state.search {
        None => matches!(state.engine, Engine::Unreachable(_)),
        Some(search) => state.filtering() || !matches!(search.phase, SearchPhase::Running { .. }),
    }
}

/// Whether keys go to this download's list of files.
pub fn in_files(state: &State, id: TorrentId) -> bool {
    state.focus == Focus::Files && state.cursor == Some(Item::Download(id))
}

/// How many of a download's files its details list, and whether an `and N more` line follows.
/// With the cursor inside the list, it takes the rows the list has to spare, the way a
/// result's file tree does, and scrolls.
pub fn download_file_rows(state: &State, job: &Job, viewport: u16) -> (usize, bool) {
    let n = state.download_files.get(&job.id).map_or(0, |f| f.picked().len());
    if n <= 1 {
        return (0, false);
    }
    if in_files(state, job.id) {
        let fixed = 5 + error_lines(job) + u16::from(state.confirm == Some(job.id));
        return (n.min(usize::from(viewport.saturating_sub(fixed).max(1))), false);
    }
    let cap = match viewport {
        24.. => 8,
        16.. => 5,
        _ => 3,
    };
    (n.min(cap), n > cap)
}

fn error_lines(job: &Job) -> u16 {
    match &job.status {
        JobStatus::Error { error } => 1 + u16::from(error.hint.is_some()),
        _ => 0,
    }
}

fn download_detail_height(state: &State, job: &Job, viewport: u16) -> u16 {
    let confirm = u16::from(state.confirm == Some(job.id));
    let (files, more) = download_file_rows(state, job, viewport);
    2 + error_lines(job) + confirm + files as u16 + u16::from(more)
}

/// File rows shown in an open tree: as many as fit beside the result row and the total.
pub fn tree_rows(nodes: usize, viewport: u16) -> usize {
    nodes.min(usize::from(viewport.saturating_sub(5).max(1)))
}

fn result_detail_height(state: &State, id: ReleaseId, viewport: u16) -> u16 {
    if state.focus != Focus::Files {
        return 1;
    }
    match state.files.get(&id) {
        Some(Files::Ready(tree)) => tree_rows(tree.visible().len(), viewport) as u16 + 2,
        _ => 2,
    }
}

/// The lines an item takes, its details included.
pub fn span(placed: &[Placed], item: Item) -> Option<(u16, u16)> {
    let row = |b: &Placed| match (b.block, item) {
        (Block::Download(id), Item::Download(want)) => id == want,
        (Block::Result(id), Item::Result(want)) => id == want,
        _ => false,
    };
    let at = placed.iter().position(row)?;
    let detail = placed.get(at + 1).filter(|b| matches!(b.block, Block::DownloadDetail(_) | Block::ResultDetail(_)));
    Some((placed[at].top, placed[at].height + detail.map_or(0, |d| d.height)))
}

/// The rows the ranking panel takes.
pub fn ranking_span(placed: &[Placed]) -> Option<(u16, u16)> {
    placed.iter().find(|b| b.block == Block::Ranking).map(|b| (b.top, b.height))
}

pub fn total(placed: &[Placed]) -> u16 {
    placed.last().map_or(0, |b| b.top + b.height)
}

/// The scroll that shows `lines`, a top row and a height, with context, starting from `scroll`.
pub fn settle(placed: &[Placed], lines: Option<(u16, u16)>, scroll: i32, viewport: u16) -> u16 {
    let viewport = i32::from(viewport);
    let max = (i32::from(total(placed)) - viewport).max(0);
    let mut scroll = scroll.clamp(0, max);
    if let Some((top, height)) = lines {
        let context = i32::from(CONTEXT).min((viewport - i32::from(height)).max(0) / 2);
        let (top, bottom) = (i32::from(top), i32::from(top + height));
        if bottom + context > scroll + viewport {
            scroll = bottom + context - viewport;
        }
        if top - context < scroll {
            scroll = top - context;
        }
    }
    scroll.clamp(0, max) as u16
}
