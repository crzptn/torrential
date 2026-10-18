//! Drawing. Everything here reads [`State`] and writes a buffer; nothing changes state.
//!
//! The page is a ledger. A margin on the left holds short labels, a mark beside the margin
//! shows where keys go, and every entry is one line on a fixed grid of right-aligned numbers.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use torrential_core::units::format_duration;
use torrential_core::{Flag, Job, JobStatus, Release, ReleaseId, SourceOutcome, TorrentFile};

use crate::format::{self, age, ellipsize, home_path, middle, percent, rate, seeds, size, width};
use crate::layout::{self, Block, CONTENT, DETAIL, MARGIN, MARK, MASTHEAD, SEARCH, Screen};
use crate::state::{Engine, Files, Focus, Item, SearchPhase, Sort, State, Tone, label};
use crate::theme::*;
use crate::tree::{Check, NodeKind, Tree};

fn fg(c: Color) -> Style {
    Style::new().fg(c)
}

fn bold(c: Color) -> Style {
    fg(c).add_modifier(Modifier::BOLD)
}

/// Writes `text` cut to `max` cells, and returns the cells written.
fn put(buf: &mut Buffer, x: u16, y: u16, max: u16, text: &str, style: Style) -> u16 {
    let area = buf.area;
    if max == 0 || y < area.y || y >= area.bottom() || x >= area.right() {
        return 0;
    }
    let max = max.min(area.right() - x);
    let text = ellipsize(text, max as usize);
    buf.set_stringn(x, y, &text, max as usize, style);
    width(&text) as u16
}

/// Writes `text` so it ends just before `right`, and returns where it starts.
fn put_right(buf: &mut Buffer, right: u16, y: u16, text: &str, style: Style) -> u16 {
    let x = right.saturating_sub(width(text) as u16);
    put(buf, x, y, right - x, text, style);
    x
}

/// Writes styled pieces one after another, and returns the cells written.
fn spans(buf: &mut Buffer, x: u16, y: u16, max: u16, parts: &[(impl AsRef<str>, Style)]) -> u16 {
    let mut used = 0;
    for (text, style) in parts {
        used += put(buf, x + used, y, max.saturating_sub(used), text.as_ref(), *style);
    }
    used
}

fn spans_width(parts: &[(impl AsRef<str>, Style)]) -> u16 {
    parts.iter().map(|(t, _)| width(t.as_ref()) as u16).sum()
}

/// Writes styled pieces so they end just before `right`, and returns where they start.
fn spans_right(buf: &mut Buffer, right: u16, y: u16, parts: &[(impl AsRef<str>, Style)]) -> u16 {
    let w = spans_width(parts);
    spans(buf, right.saturating_sub(w), y, w, parts);
    right.saturating_sub(w)
}

fn paint(buf: &mut Buffer) {
    for cell in &mut buf.content {
        cell.set_symbol(" ").set_bg(PAPER).set_fg(TEXT);
    }
}

/// A margin label, right-aligned against the margin's edge.
fn margin(buf: &mut Buffer, y: u16, text: &str, style: Style) {
    put_right(buf, MARGIN + 1, y, text, style);
}

/// The one mark on the page: where keys go.
fn mark(buf: &mut Buffer, y: u16) {
    put(buf, MARK, y, 1, "●", fg(ACTION));
}

/// The rule between the margin and the entries, on the rows from `top` up to `bottom`.
fn rule(buf: &mut Buffer, top: u16, bottom: u16) {
    for y in top..bottom {
        put(buf, MARK, y, 1, "│", fg(RULE));
    }
}

/// What a progress mark measures: the bytes verified out of a size, for a download or one of its
/// files, and whether the download is getting bytes.
#[derive(Clone, Copy)]
struct Meter {
    verified: u64,
    /// Unknown until a peer sends the file list.
    size: Option<u64>,
    downloading: bool,
}

impl Meter {
    fn job(job: &Job) -> Self {
        let p = &job.progress;
        Self { verified: p.verified, size: p.size, downloading: job.status == JobStatus::Downloading }
    }

    fn file(job: &Job, file: &TorrentFile) -> Self {
        Self { verified: file.verified, size: Some(file.size), ..Self::job(job) }
    }

    fn fraction(self) -> f64 {
        match self.size {
            Some(0) => 1.0,
            Some(size) => self.verified as f64 / size as f64,
            None => 0.0,
        }
    }
}

/// The one place progress is drawn: a rule `cells` wide that fills in half cells, sage while the
/// download gets bytes and pencil otherwise. A rain gauge can replace the rule here for rows
/// getting bytes once the engine reports which pieces have arrived.
fn progress_mark(buf: &mut Buffer, x: u16, y: u16, cells: u16, meter: Meter) {
    let ink = if meter.downloading { ACTION } else { PENCIL };
    let halves = (meter.fraction().clamp(0.0, 1.0) * f64::from(cells) * 2.0).floor() as u16;
    for i in 0..cells {
        let (glyph, color) = match halves.saturating_sub(i * 2) {
            0 => ("─", RULE),
            1 => ("╸", ink),
            _ => ("━", ink),
        };
        put(buf, x + i, y, 1, glyph, fg(color));
    }
}

fn hints_width(pairs: &[(&str, &str)]) -> u16 {
    let cells = pairs.iter().map(|(k, v)| width(k) + 1 + width(v)).sum::<usize>() + pairs.len().saturating_sub(1) * 3;
    cells as u16
}

/// Key hints as `key verb` pairs, most useful first, right-aligned before `right` within `room`
/// cells. Pairs drop from the end, so the first one stays.
fn hints(buf: &mut Buffer, right: u16, y: u16, room: u16, pairs: &[(&str, &str)]) {
    let mut pairs = pairs.to_vec();
    while pairs.len() > 1 && hints_width(&pairs) > room {
        pairs.pop();
    }
    let mut x = right.saturating_sub(hints_width(&pairs));
    for (i, (key, verb)) in pairs.iter().enumerate() {
        if i > 0 {
            x += 3;
        }
        x += put(buf, x, y, right.saturating_sub(x), key, fg(TEXT));
        x += put(buf, x, y, right.saturating_sub(x), &format!(" {verb}"), fg(PENCIL));
    }
}

pub fn render(buf: &mut Buffer, state: &State, mode: ColorMode) -> Option<Position> {
    paint(buf);
    let size = (buf.area.width, buf.area.height);
    let Some(screen) = layout::screen(size) else {
        let (w, h) = layout::MIN_SIZE;
        put(buf, 1, 0, size.0.saturating_sub(2), &format!("Make the window at least {w}×{h}."), fg(INK));
        degrade(buf, mode);
        return None;
    };
    masthead(buf, state, &screen);
    let caret = search(buf, state, &screen);
    rule(buf, SEARCH + 1, screen.list.y);
    if state.help {
        help(buf, &screen);
    } else {
        list(buf, state, &screen);
    }
    footer(buf, state, &screen);
    degrade(buf, mode);
    caret
}

/// The title in the margin, and on the right only what needs saying: bytes arriving, a disk
/// running out, or an engine that is not there.
fn masthead(buf: &mut Buffer, state: &State, screen: &Screen) {
    margin(buf, MASTHEAD, "torrential", bold(INK));
    let total = rate(state.rate());
    let free = state.free_space.map(|f| format!("low on space · {} free", size(f)));
    let mut parts: Vec<(&str, Style)> = Vec::new();
    match &state.engine {
        Engine::Starting => parts.extend([("starting the engine", fg(PENCIL)), (state.spinner(), fg(PENCIL))]),
        Engine::Unreachable(_) => parts.push(("engine unreachable", fg(ERROR))),
        Engine::Ready => {
            if state.rate() > 0 {
                parts.extend([("↓ ", fg(ACTION)), (total.as_str(), fg(TEXT))]);
            }
            if let Some(free) = free.as_deref().filter(|_| short_of_space(state)) {
                if !parts.is_empty() {
                    parts.push(("   ", fg(PENCIL)));
                }
                parts.push((free, fg(ERROR)));
            }
        }
    }
    spans_right(buf, screen.right, MASTHEAD, &parts);
}

/// Whether the disk has less room than the running downloads still need, or under 1 GiB.
fn short_of_space(state: &State) -> bool {
    let needed: u64 = state
        .downloads
        .iter()
        .filter(|j| crate::state::group(&j.status) == 0)
        .filter_map(|j| j.progress.size.map(|s| s.saturating_sub(j.progress.verified)))
        .sum();
    state.free_space.is_some_and(|free| free < needed.max(1 << 30))
}

/// The search line. Returns where the terminal's cursor goes while typing.
fn search(buf: &mut Buffer, state: &State, screen: &Screen) -> Option<Position> {
    let focused = state.focus == Focus::Query;
    margin(buf, SEARCH, "search", fg(PENCIL));
    if focused {
        mark(buf, SEARCH);
    } else {
        rule(buf, SEARCH, SEARCH + 1);
    }
    let text = &state.query.text;
    let hint = if focused { "type a name, then press enter" } else { "press / to search" };
    let left = width(if text.is_empty() { hint } else { text }) as u16;
    // The status keeps its place and a long query is cut instead, so failed sources stay in view.
    let room = screen.right - CONTENT;
    let [long, short] = [false, true].map(|short| search_status(state, short));
    let status = if left + 3 + spans_width(&long) <= room { long } else { short };
    let used = spans_width(&status);
    spans_right(buf, screen.right, SEARCH, &status);
    let (x, room) = (CONTENT, if used == 0 { room } else { room.saturating_sub(used + 3) });
    if text.is_empty() {
        put(buf, x, SEARCH, room, hint, fg(PENCIL));
        return focused.then_some(Position::new(x, SEARCH));
    }
    if !focused {
        put(buf, x, SEARCH, room, text, fg(INK));
        return None;
    }
    let before: String = text.chars().take(state.query.caret).collect();
    let skip = (width(&before) as u16).saturating_sub(room.saturating_sub(1));
    let shown: String = {
        let mut cells = 0;
        text.chars()
            .skip_while(|c| {
                let skipping = cells < skip;
                cells += width(c.encode_utf8(&mut [0; 4])) as u16;
                skipping
            })
            .collect()
    };
    put(buf, x, SEARCH, room, &shown, fg(INK));
    Some(Position::new(x + width(&before) as u16 - skip, SEARCH))
}

fn reason(outcome: &SourceOutcome) -> String {
    match outcome {
        SourceOutcome::Ok { .. } | SourceOutcome::Cached { .. } => "ok".into(),
        SourceOutcome::Timeout => "timed out".into(),
        SourceOutcome::Http { status } => format!("answered {status}"),
        SourceOutcome::Connect { .. } => "did not connect".into(),
        SourceOutcome::BadResponse { .. } => "sent a bad answer".into(),
        SourceOutcome::Irrelevant => "had nothing relevant".into(),
    }
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The right end of the search line. `short` drops what can go when the query is long.
fn search_status(state: &State, short: bool) -> Vec<(String, Style)> {
    let Some(search) = &state.search else { return Vec::new() };
    if state.filtering() {
        let (shown, all) = (state.results().len(), search.releases.len());
        let text =
            if short { format!("{shown} of {all}") } else { format!("{shown} of {all} match · enter searches") };
        return vec![(text, fg(PENCIL))];
    }
    match &search.phase {
        SearchPhase::Running { pending } => {
            let (answered, asked) = (search.sources.len(), search.sources.len() + pending.len());
            let text = match asked {
                0 => "searching".to_string(),
                _ if short => format!("{answered} of {asked}"),
                _ => format!("searching · {answered} of {asked} sources"),
            };
            vec![(text, fg(PENCIL)), (state.spinner().into(), fg(PENCIL))]
        }
        SearchPhase::Done => {
            let found = search.releases.len();
            let secs = f64::from(search.elapsed_ms) / 1000.0;
            let count = if short { format!("{found} found") } else { format!("{found} found in {secs:.1}s") };
            let mut out = vec![(count, fg(PENCIL))];
            let failed: Vec<&str> =
                search.sources.iter().filter(|s| !s.outcome.is_ok()).map(|s| s.source.as_str()).collect();
            if !failed.is_empty() {
                let text =
                    if short { format!("{} failed", failed.len()) } else { format!("{} failed", and_list(&failed)) };
                out.extend([(" · ".into(), fg(PENCIL)), (text, fg(ERROR))]);
            }
            out
        }
        SearchPhase::Failed(_) => vec![("search failed".into(), fg(ERROR))],
    }
}

/// The download grid: name, a progress rule, the percent, then time left or a size.
struct Grid {
    rule: u16,
    rule_cells: u16,
    percent: u16,
    tail: u16,
    right: u16,
}

fn download_grid(right: u16) -> Grid {
    let rule_cells = match right - CONTENT {
        80.. => 16,
        70.. => 12,
        _ => 8,
    };
    let tail = right - 12;
    let percent = tail - 2;
    Grid { rule: percent - 6 - rule_cells, rule_cells, percent, tail, right }
}

/// The result grid: name, one flag, then size, seeds, and age, each right-aligned. The fields
/// hold where each column starts (`flag`) or ends (the rest).
struct Columns {
    flag: u16,
    size: u16,
    seeds: u16,
    age: u16,
}

fn result_columns(right: u16) -> Columns {
    let seeds = right - 6;
    let size = seeds - 7;
    Columns { flag: size - 18, size, seeds, age: right }
}

/// Where a file row's name must end: two cells before the size column.
fn before_size(c: &Columns) -> u16 {
    c.size - 10
}

fn list(buf: &mut Buffer, state: &State, screen: &Screen) {
    let area = screen.list;
    let placed = layout::blocks(state, area.height);
    let content = layout::total(&placed);
    let total = content.max(area.height);
    let mut sheet = Buffer::empty(Rect::new(0, 0, area.width, total));
    paint(&mut sheet);
    rule(&mut sheet, 0, total);
    // The label in force on each sheet row, so a group scrolled past still shows its name.
    let mut group: Vec<Option<&'static str>> = vec![None; usize::from(total)];
    let mut last: Option<&'static str> = None;
    let right = screen.right;
    for p in &placed {
        let y = p.top;
        match p.block {
            Block::Download(id) => {
                let Some(job) = state.job(id) else { continue };
                let word = if state.confirm == Some(id) { "remove?" } else { label(&job.status) };
                if last != Some(word) {
                    let style = if matches!(word, "remove?" | "failed") { fg(ERROR) } else { fg(PENCIL) };
                    margin(&mut sheet, y, word, style);
                }
                last = Some(word);
                download_row(&mut sheet, state, job, &download_grid(right), y);
            }
            Block::DownloadDetail(id) => {
                if layout::in_files(state, id) {
                    last = Some("files");
                }
                if let Some(job) = state.job(id) {
                    download_detail(&mut sheet, state, job, &download_grid(right), (y, area.height));
                }
            }
            Block::Folded(word) => {
                margin(&mut sheet, y, word, fg(PENCIL));
                last = Some(word);
                let n = state.downloads.iter().filter(|j| label(&j.status) == word).count();
                let text = if n == 1 { "1 download".to_string() } else { format!("{n} downloads") };
                put(&mut sheet, CONTENT, y, right - CONTENT, &text, fg(PENCIL));
            }
            Block::Gap => last = None,
            Block::Heads => {
                last = Some("found");
                heads(&mut sheet, state, &result_columns(right), y);
            }
            Block::Result(id) => {
                if let Some(release) = state.release(id) {
                    result_row(&mut sheet, state, release, &result_columns(right), y);
                }
            }
            Block::ResultDetail(id) => {
                if state.focus == Focus::Files {
                    last = Some("files");
                }
                result_detail(&mut sheet, state, id, &result_columns(right), y, p.height);
            }
            Block::Empty => empty(&mut sheet, state, right, y),
        }
        for row in y..y + p.height {
            group[usize::from(row)] = last;
        }
    }
    let scroll = state.scroll.min(total - area.height);
    for row in 0..area.height {
        for x in 0..area.width {
            buf[(x, area.y + row)] = sheet[(x, scroll + row)].clone();
        }
    }
    let blank_margin = (0..=MARGIN).all(|x| buf[(x, area.y)].symbol() == " ");
    if scroll > 0
        && blank_margin
        && let Some(word) = group[usize::from(scroll)]
    {
        margin(buf, area.y, word, fg(PENCIL));
    }
    let mut arrow = |y: u16, glyph: &str| {
        if buf[(MARK, y)].symbol() == "│" {
            put(buf, MARK, y, 1, glyph, fg(PENCIL));
        }
    };
    if scroll > 0 {
        arrow(area.y, "↑");
    }
    if scroll + area.height < content {
        arrow(area.bottom() - 1, "↓");
    }
}

fn selected(state: &State, item: Item) -> bool {
    state.cursor == Some(item) && state.focus != Focus::Query
}

fn download_row(buf: &mut Buffer, state: &State, job: &Job, g: &Grid, y: u16) {
    let is_selected = selected(state, Item::Download(job.id));
    if is_selected && state.focus == Focus::List {
        mark(buf, y);
    }
    let done = matches!(job.status, JobStatus::Done { .. });
    let ink = match (is_selected, done) {
        (true, _) => bold(INK),
        (false, true) => fg(PENCIL),
        (false, false) => fg(TEXT),
    };
    let name = job.name.clone().unwrap_or_else(|| job.id.to_hex());
    let p = &job.progress;
    if done {
        let room = g.tail - 2 - CONTENT;
        put(buf, CONTENT, y, room, &format::name(&name, room.into()), ink);
        put_right(buf, g.right, y, &p.size.map_or("–".into(), size), ink);
        return;
    }
    let room = g.rule - 2 - CONTENT;
    put(buf, CONTENT, y, room, &format::name(&name, room.into()), ink);
    progress_mark(buf, g.rule, y, g.rule_cells, Meter::job(job));
    let pct = p.size.map_or("–".to_string(), |size| percent(p.verified, size));
    put_right(buf, g.percent, y, &pct, ink);
    if let Some(action) = state.pending.get(&job.id) {
        spans(buf, g.tail, y, g.right - g.tail, &[(action.progressive(), fg(PENCIL)), (state.spinner(), fg(PENCIL))]);
        return;
    }
    let tail = match &job.status {
        JobStatus::Downloading => match p.eta {
            Some(secs) if p.down_rate >= 1024 => format!("{} left", format_duration(Duration::from_secs(secs))),
            _ if state.stalled(job) => "stalled".into(),
            _ => String::new(),
        },
        JobStatus::Metadata => format!("{} peers", p.peers),
        _ => String::new(),
    };
    put_right(buf, g.right, y, &tail, if is_selected { ink } else { fg(PENCIL) });
}

/// What opens under a selected download. `viewport` is the list's height, which caps the files.
fn download_detail(buf: &mut Buffer, state: &State, job: &Job, g: &Grid, (y, viewport): (u16, u16)) {
    let room = g.right - DETAIL;
    let home = state.home.as_deref();
    let mut row = y;
    if state.confirm == Some(job.id) {
        let text = format!("Its files stay in {}.", home_path(&job.dir, home));
        let keys = [("enter", fg(INK)), (" removes it   ", fg(PENCIL)), ("esc", fg(INK)), (" keeps it", fg(PENCIL))];
        let keys_x = spans_right(buf, g.right, row, &keys);
        put(buf, DETAIL, row, keys_x.saturating_sub(DETAIL + 2), &text, fg(TEXT));
        row += 1;
    }
    let path = job.path.as_ref().unwrap_or(&job.dir);
    put(buf, DETAIL, row, room, &middle(&home_path(path, home), room as usize), fg(PENCIL));
    let p = &job.progress;
    let mut facts = Vec::new();
    match (&job.status, p.size) {
        (JobStatus::Metadata, _) | (_, None) => facts.push("fetching the file list from peers".to_string()),
        (JobStatus::Done { finished_at }, Some(total)) => {
            facts.push(size(total));
            facts.push(format!("finished {} ago", age(*finished_at, state.clock)));
        }
        (_, Some(total)) => facts.push(format!("{} of {}", size(p.verified), size(total))),
    }
    if job.status == JobStatus::Downloading {
        facts.push(rate(p.down_rate));
    }
    if matches!(job.status, JobStatus::Downloading | JobStatus::Metadata) {
        facts.push(format!("{} peers", p.peers));
    }
    put(buf, DETAIL, row + 1, room, &facts.join(" · "), fg(PENCIL));
    row += 2;
    if let JobStatus::Error { error } = &job.status {
        put(buf, DETAIL, row, room, &error.message, fg(ERROR));
        row += 1;
        if let Some(hint) = &error.hint {
            put(buf, DETAIL, row, room, hint, fg(PENCIL));
            row += 1;
        }
    }
    let (shown, more) = layout::download_file_rows(state, job, viewport);
    let Some(files) = state.download_files.get(&job.id) else { return };
    let picked = files.picked();
    let inside = layout::in_files(state, job.id);
    let skip = if inside { files.scroll } else { 0 };
    let first = row;
    for (k, file) in picked.iter().enumerate().skip(skip).take(shown) {
        let here = inside && k == files.cursor;
        if inside && row == first {
            margin(buf, row, "files", fg(PENCIL));
        }
        if here {
            mark(buf, row);
        }
        let name = file.path.rsplit('/').next().unwrap_or(&file.path);
        let room = g.rule - 2 - DETAIL;
        put(buf, DETAIL, row, room, &middle(name, room as usize), if here { bold(INK) } else { fg(TEXT) });
        progress_mark(buf, g.rule, row, g.rule_cells, Meter::file(job, file));
        put_right(buf, g.percent, row, &percent(file.verified, file.size), fg(PENCIL));
        put_right(buf, g.right, row, &size(file.size), fg(PENCIL));
        row += 1;
    }
    let mut arrow = |y: u16, glyph: &str| {
        if buf[(MARK, y)].symbol() == "│" {
            put(buf, MARK, y, 1, glyph, fg(PENCIL));
        }
    };
    if skip > 0 {
        arrow(first, "↑");
    }
    if inside && skip + shown < picked.len() {
        arrow(row - 1, "↓");
    }
    if more {
        let text = format!("and {} more files · press enter to list them", picked.len() - shown);
        put(buf, DETAIL, row, room, &text, fg(PENCIL));
    }
}

fn heads(buf: &mut Buffer, state: &State, c: &Columns, y: u16) {
    margin(buf, y, "found", fg(PENCIL));
    let order = match state.sort {
        Sort::BestMatch => "best match first",
        Sort::Seeds => "most seeds first",
        Sort::Size => "largest first",
        Sort::Age => "newest first",
    };
    put(buf, CONTENT, y, c.flag.saturating_sub(CONTENT + 2), order, fg(PENCIL));
    put_right(buf, c.size, y, "size", fg(PENCIL));
    put_right(buf, c.seeds, y, "seeds", fg(PENCIL));
    put_right(buf, c.age, y, "age", fg(PENCIL));
}

/// The flag a row has room for: the warning first, then provenance.
fn first_flag(release: &Release) -> Option<Flag> {
    let rank = [Flag::Cam, Flag::Official, Flag::PublicDomain, Flag::CreativeCommons, Flag::Trusted, Flag::Adult];
    rank.into_iter().find(|f| release.flags.contains(f))
}

fn flag_words(flag: Flag) -> &'static str {
    match flag {
        Flag::Official => "official release",
        Flag::Trusted => "trusted uploader",
        Flag::PublicDomain => "public domain",
        Flag::CreativeCommons => "creative commons",
        Flag::Cam => "recorded in a cinema",
        Flag::Adult => "adult",
    }
}

fn result_row(buf: &mut Buffer, state: &State, release: &Release, c: &Columns, y: u16) {
    let is_selected = selected(state, Item::Result(release.id));
    if is_selected && state.focus == Focus::List {
        mark(buf, y);
    }
    let pick = |plain: Color| if is_selected { bold(INK) } else { fg(plain) };
    let room = c.flag - 2 - CONTENT;
    put(buf, CONTENT, y, room, &format::name(&release.name, room.into()), pick(TEXT));
    if let Some(flag) = first_flag(release) {
        let style = match flag {
            Flag::Cam => fg(ERROR),
            Flag::Trusted | Flag::Adult => pick(PENCIL),
            _ => pick(TEXT),
        };
        put(buf, c.flag, y, 8, flag.as_str(), style);
    }
    put_right(buf, c.size, y, &release.size.map_or("–".into(), size), pick(TEXT));
    let seeds_style = match release.seeders {
        Some(0) => fg(ERROR),
        None | Some(1..=9) => pick(PENCIL),
        Some(_) => pick(TEXT),
    };
    put_right(buf, c.seeds, y, &seeds(release.seeders), seeds_style);
    let published = release.published.map_or("–".into(), |p| age(p, state.clock));
    put_right(buf, c.age, y, &published, pick(PENCIL));
}

fn result_detail(buf: &mut Buffer, state: &State, id: ReleaseId, c: &Columns, y: u16, height: u16) {
    let room = c.age - DETAIL;
    let files = state.files.get(&id);
    if state.focus == Focus::Files {
        match files {
            Some(Files::Ready(tree)) => file_tree(buf, state, tree, c, y, height),
            Some(Files::Loading { since, .. }) => {
                margin(buf, y, "files", fg(PENCIL));
                let secs = state.now.duration_since(*since).as_secs();
                let parts = [("fetching the file list", fg(TEXT)), (state.spinner(), fg(TEXT))];
                let used = spans(buf, DETAIL, y, room, &parts);
                put(buf, DETAIL + used, y, room.saturating_sub(used), &format!(" {secs}s"), fg(PENCIL));
                let note = "A magnet link has no file list until a peer sends one.";
                put(buf, DETAIL, y + 1, room, note, fg(PENCIL));
            }
            Some(Files::Failed(error)) => {
                margin(buf, y, "files", fg(ERROR));
                put(buf, DETAIL, y, room, &error.message, fg(ERROR));
                put(buf, DETAIL, y + 1, room, error.hint.as_deref().unwrap_or("Press enter to try again."), fg(PENCIL));
            }
            None => {}
        }
        return;
    }
    if matches!(id, ReleaseId::Torrent(t) if state.job(t).is_some()) {
        put(buf, DETAIL, y, room, "in downloads · press d to see it", fg(PENCIL));
        return;
    }
    let Some(release) = state.release(id) else { return };
    let mut facts: Vec<String> = release.sources.clone();
    facts.extend(release.flags.iter().map(|f| flag_words(*f).to_string()));
    match files {
        Some(Files::Ready(tree)) => {
            let n = tree.files.len();
            facts.push(if n == 1 { "1 file".into() } else { format!("{n} files") });
        }
        Some(Files::Loading { .. }) => facts.push(format!("counting files{}", state.spinner())),
        Some(Files::Failed(_)) | None => {}
    }
    let used = put(buf, DETAIL, y, room, &facts.join(" · "), fg(PENCIL));
    if let Some(Files::Failed(error)) = files {
        put(buf, CONTENT + used + 3, y, room.saturating_sub(used + 3), &error.message, fg(ERROR));
    }
}

/// The file picker under a result: tick marks, sizes under the result's size column, and a
/// total ruled off below them.
fn file_tree(buf: &mut Buffer, state: &State, tree: &Tree, c: &Columns, y: u16, height: u16) {
    let rows = usize::from(height - 2);
    for (k, &n) in tree.visible().iter().skip(tree.scroll).take(rows).enumerate() {
        let row = y + k as u16;
        if k == 0 {
            margin(buf, row, "files", fg(PENCIL));
        }
        let node = &tree.nodes[n];
        let here = n == tree.cursor;
        if here {
            mark(buf, row);
        }
        let at = DETAIL + node.depth * 2;
        let check = tree.check(n);
        let (tick, tick_color) = match check {
            Check::All => ("✓", ACTION),
            Check::Some => ("–", ACTION),
            Check::None => ("·", PENCIL),
        };
        put(buf, at, row, 1, tick, fg(tick_color));
        let mut end = before_size(c);
        let name_x = match node.kind {
            NodeKind::Folder { .. } => {
                put(buf, at + 2, row, 1, if tree.folded.contains(&n) { "▸" } else { "▾" }, fg(PENCIL));
                at + 4
            }
            NodeKind::File(i) => {
                put_right(buf, c.size, row, &size(tree.files[i].size), fg(PENCIL));
                if tree.is_risky(i) {
                    end = put_right(buf, end, row, "executable", fg(ERROR)).saturating_sub(2);
                }
                at + 2
            }
        };
        let style = match (here, check) {
            (true, _) => bold(INK),
            (false, Check::None) => fg(PENCIL),
            (false, _) => fg(TEXT),
        };
        let room = end.saturating_sub(name_x);
        put(buf, name_x, row, room, &middle(&node.name, room as usize), style);
    }
    let ruled = y + rows as u16;
    let left_out = tree.risky_left_out();
    if left_out > 0 {
        let text = format!("{left_out} executable{} left out", if left_out == 1 { "" } else { "s" });
        put(buf, DETAIL, ruled, before_size(c) - DETAIL, &text, fg(ERROR));
    }
    put_right(buf, c.size, ruled, "────────", fg(PENCIL));
    let total = ruled + 1;
    let picked = tree.picked_count();
    let summary =
        if picked == 0 { "nothing picked".to_string() } else { format!("{picked} of {} files", tree.files.len()) };
    let used = put(buf, DETAIL, total, before_size(c) - DETAIL, &summary, fg(TEXT));
    if let Some(free) = state.free_space {
        let picked = tree.picked_size();
        let (text, style) = match picked.checked_sub(free) {
            Some(short) if short > 0 => (format!(" · {} short of space", size(short)), fg(ERROR)),
            _ => (format!(" · {} free", size(free)), fg(PENCIL)),
        };
        put(buf, DETAIL + used, total, before_size(c).saturating_sub(DETAIL + used), &text, style);
    }
    put_right(buf, c.size, total, &size(tree.picked_size()), bold(INK));
}

fn empty(buf: &mut Buffer, state: &State, right: u16, y: u16) {
    let room = right - CONTENT;
    let mut lines = |word: &str, first: &str, alarm: bool, second: &str, second_color: Color| {
        margin(buf, y, word, fg(if alarm { ERROR } else { PENCIL }));
        put(buf, CONTENT, y, room, first, fg(if alarm { ERROR } else { TEXT }));
        put(buf, CONTENT, y + 1, room, second, fg(second_color));
    };
    if let (Engine::Unreachable(error), None) = (&state.engine, &state.search) {
        let why = error.hint.as_deref().unwrap_or("Downloads run in the engine, so this screen needs it.");
        lines("engine", "Cannot reach the engine.", true, why, PENCIL);
        return;
    }
    let Some(search) = &state.search else { return };
    if state.filtering() {
        let first = format!("Nothing loaded matches \"{}\".", state.query.text.trim());
        lines("found", &first, false, "Press enter to search for it.", PENCIL);
        return;
    }
    let failures: Vec<String> = search
        .sources
        .iter()
        .filter(|s| !s.outcome.is_ok())
        .map(|s| format!("{} {}", s.source, reason(&s.outcome)))
        .collect();
    let nothing = format!("Nothing found for \"{}\".", search.text);
    match &search.phase {
        SearchPhase::Running { .. } => {}
        SearchPhase::Done if failures.is_empty() => {
            lines("found", &nothing, false, "Try fewer or different words.", PENCIL)
        }
        SearchPhase::Done => lines("found", &nothing, false, &failures.join(" · "), ERROR),
        SearchPhase::Failed(error) => {
            let hint = error.hint.as_deref().unwrap_or("Check the connection, then press enter to search again.");
            lines("found", "No source answered.", true, hint, PENCIL);
        }
    }
}

const HELP: [(&str, &[(&str, &str)]); 6] = [
    ("move", &[("j k", "up, down"), ("g G", "first, last"), ("pgup pgdn", "a page"), ("d r", "downloads, results")]),
    ("typing", &[("/", "search"), ("enter", "search again"), ("ctrl+u", "clear"), ("tab", "into the list")]),
    ("results", &[("enter", "files"), ("D", "download"), ("s", "sort"), ("h l", "close, open")]),
    ("files", &[("space", "pick"), ("a", "all or none"), ("h l", "fold folders"), ("enter", "download")]),
    (
        "download",
        &[
            ("enter", "files"),
            ("space", "pause, resume"),
            ("v", "verify"),
            ("x", "remove"),
            ("o", "open"),
            ("y", "copy path"),
        ],
    ),
    ("always", &[("? f1", "keys"), ("esc", "back"), ("q", "quit"), ("ctrl+c", "quit")]),
];

const NOTES: [&str; 4] = [
    "Typing filters the results already loaded.",
    "Quitting leaves downloads running.",
    "Downloads stop at 100% and never seed.",
    "Removing a download keeps its files.",
];

/// The keys, in place of the list, as a ledger of their own.
fn help(buf: &mut Buffer, screen: &Screen) {
    let area = screen.list;
    rule(buf, area.y, area.bottom());
    let room = screen.right - CONTENT;
    let mut y = area.y;
    for (word, pairs) in HELP {
        if y >= area.bottom() {
            return;
        }
        margin(buf, y, word, fg(PENCIL));
        let mut x = CONTENT;
        for (key, verb) in pairs {
            let w = (width(key) + 1 + width(verb)) as u16;
            if x > CONTENT && x + w > CONTENT + room {
                y += 1;
                x = CONTENT;
            }
            x += put(buf, x, y, room, key, fg(INK));
            x += put(buf, x, y, room, &format!(" {verb}"), fg(PENCIL)) + 3;
        }
        y += 1;
    }
    for (k, note) in NOTES.iter().enumerate() {
        put(buf, CONTENT, y + 1 + k as u16, room, note, fg(PENCIL));
    }
}

fn footer(buf: &mut Buffer, state: &State, screen: &Screen) {
    let y = screen.footer;
    let pause = match state.cursor.and_then(|item| match item {
        Item::Download(id) => state.job(id),
        Item::Result(_) => None,
    }) {
        Some(job) if matches!(job.status, JobStatus::Paused | JobStatus::Interrupted | JobStatus::Error { .. }) => {
            "resume"
        }
        _ => "pause",
    };
    let downloaded = matches!(state.cursor, Some(Item::Result(ReleaseId::Torrent(t))) if state.job(t).is_some());
    let pairs: Vec<(&str, &str)> = match (state.focus, state.cursor) {
        _ if state.confirm.is_some() => Vec::new(),
        _ if state.help => vec![("esc", "close")],
        (Focus::Query, _) if state.items().is_empty() => vec![("enter", "search"), ("f1", "keys")],
        (Focus::Query, _) => vec![("enter", "search"), ("↓", "list"), ("f1", "keys")],
        (Focus::Files, Some(Item::Download(_))) => {
            vec![("space", pause), ("esc", "back"), ("o", "open"), ("?", "keys")]
        }
        (Focus::Files, _) => vec![("space", "pick"), ("a", "all"), ("enter", "download"), ("esc", "back")],
        (Focus::List, Some(Item::Download(_))) => vec![("space", pause), ("x", "remove"), ("o", "open"), ("?", "keys")],
        (Focus::List, Some(Item::Result(_))) if downloaded => vec![("d", "downloads"), ("s", "sort"), ("?", "keys")],
        (Focus::List, Some(Item::Result(_))) => {
            vec![("enter", "files"), ("D", "download"), ("s", "sort"), ("?", "keys")]
        }
        (Focus::List, None) => vec![("/", "search"), ("?", "keys")],
    };
    // The toast gets the room the first hint leaves, so a message never hides the first key.
    let mut room = screen.right - CONTENT;
    let first = pairs.first().map_or(0, |pair| hints_width(&[*pair]) + 4);
    if let Some(toast) = &state.toast {
        let color = match toast.tone {
            Tone::Info => TEXT,
            Tone::Success => ACTION,
            Tone::Warning | Tone::Error => ERROR,
        };
        let used = put(buf, CONTENT, y, room.saturating_sub(first), &toast.text, fg(color));
        room = room.saturating_sub(used + 4);
    }
    if !pairs.is_empty() {
        hints(buf, screen.right, y, room, &pairs);
    }
}
