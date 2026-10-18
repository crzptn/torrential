//! Where things go: the screen's regions, and the list as blocks of known height. Drawing and
//! scrolling both read this, so they always agree.

use ratatui::layout::Rect;
use torrential_core::{Job, ReleaseId, TorrentId};

use crate::state::{Files, Focus, Item, State};

pub const MIN_SIZE: (u16, u16) = (60, 16);
const MAX_COLUMN: u16 = 110;
const HELP_ROWS: u16 = 10;
/// Rows kept visible above and below the selection when scrolling (DESIGN-BRIEF 3.7).
const CONTEXT: u16 = 2;

pub struct Screen {
    /// The centered content column, full height.
    pub column: Rect,
    pub list: Rect,
    pub help: Option<Rect>,
    pub status: u16,
    pub hints: u16,
}

pub const HEADER: u16 = 0;
pub const QUERY: u16 = 1;
pub const SOURCES: u16 = 2;
const LIST_TOP: u16 = 4;

/// `None` when the terminal is too small to draw.
pub fn screen(size: (u16, u16), help: bool) -> Option<Screen> {
    let (w, h) = size;
    if w < MIN_SIZE.0 || h < MIN_SIZE.1 {
        return None;
    }
    let width = (w - 4).min(MAX_COLUMN);
    let column = Rect::new((w - width) / 2, 0, width, h);
    let status = h - 2;
    let room = status - LIST_TOP;
    let help_rows = if help { HELP_ROWS.min(room.saturating_sub(4)) } else { 0 };
    let list = Rect::new(column.x, LIST_TOP, width, room - help_rows);
    let help = help.then(|| Rect::new(column.x, LIST_TOP + list.height, width, help_rows));
    Some(Screen { column, list, help, status, hints: h - 1 })
}

pub fn list_height(state: &State) -> u16 {
    screen(state.size, state.help).map_or(0, |s| s.list.height)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Block {
    DownloadsLabel,
    Download(TorrentId),
    DownloadDetail(TorrentId),
    /// The summary line for folded downloads.
    Folded,
    Gap,
    ResultsLabel,
    Result(ReleaseId),
    ResultDetail(ReleaseId),
    /// What the results section says when it has nothing to list.
    Empty,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Placed {
    pub block: Block,
    pub top: u16,
    pub height: u16,
}

/// Whether the item under the cursor shows its details.
pub fn expanded(state: &State, item: Item) -> bool {
    state.cursor == Some(item) && !state.collapsed && state.focus != Focus::Query
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
        push(Block::DownloadsLabel, 1);
        for job in state.download_rows() {
            push(Block::Download(job.id), 1);
            if expanded(state, Item::Download(job.id)) {
                push(Block::DownloadDetail(job.id), download_detail_height(state, job, viewport));
            }
        }
        if state.folded().is_some() {
            push(Block::Folded, 1);
        }
        push(Block::Gap, 1);
    }
    push(Block::ResultsLabel, 1);
    let results = state.results();
    if results.is_empty() {
        push(Block::Empty, 3);
    }
    for release in results {
        push(Block::Result(release.id), 2);
        if expanded(state, Item::Result(release.id)) {
            push(Block::ResultDetail(release.id), result_detail_height(state, release.id, viewport));
        }
    }
    out
}

/// How many of a download's files its details list, and whether a `+N more` line follows.
pub fn download_file_rows(state: &State, job: &Job, viewport: u16) -> (usize, bool) {
    let cap = match viewport {
        30.. => 8,
        20.. => 5,
        _ => 3,
    };
    let picked = state.download_files.get(&job.id).map(|f| f.files.iter().filter(|f| f.selected).count());
    match picked {
        Some(n) if n > 1 => (n.min(cap), n > cap),
        _ => (0, false),
    }
}

fn download_detail_height(state: &State, job: &Job, viewport: u16) -> u16 {
    let error = match &job.status {
        torrential_core::JobStatus::Error { error } => 1 + u16::from(error.hint.is_some()),
        _ => 0,
    };
    let (files, more) = download_file_rows(state, job, viewport);
    2 + error + files as u16 + u16::from(more)
}

/// File rows shown in an open tree: as many as fit beside the result and the tree's own two lines.
pub fn tree_rows(nodes: usize, viewport: u16) -> usize {
    nodes.min(usize::from(viewport.saturating_sub(4).max(1)))
}

fn result_detail_height(state: &State, id: ReleaseId, viewport: u16) -> u16 {
    if state.focus != Focus::Files {
        return 1;
    }
    match state.files.get(&id) {
        Some(Files::Ready(tree)) => 2 + tree_rows(tree.visible().len(), viewport) as u16,
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

pub fn total(placed: &[Placed]) -> u16 {
    placed.last().map_or(0, |b| b.top + b.height)
}

/// The scroll that shows the cursor's lines with context, starting from `scroll`.
pub fn settle(placed: &[Placed], cursor: Option<Item>, scroll: i32, viewport: u16) -> u16 {
    let viewport = i32::from(viewport);
    let max = (i32::from(total(placed)) - viewport).max(0);
    let mut scroll = scroll.clamp(0, max);
    if let Some((top, height)) = cursor.and_then(|item| span(placed, item)) {
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
