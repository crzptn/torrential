//! Drawing. Everything here reads [`State`] and writes a buffer; nothing changes state.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use torrential_core::units::{format_duration, format_rate, format_size};
use torrential_core::{Flag, Job, JobStatus, Release, ReleaseId, SourceOutcome};

use crate::format::{age, ellipsize, home_path, middle, percent, seeds, width};
use crate::layout::{self, Block, HEADER, QUERY, SOURCES, Screen};
use crate::state::{Engine, Files, Focus, Item, Overlay, SearchPhase, State, Tone};
use crate::theme::*;
use crate::tree::{Check, NodeKind, Tree};

/// Where right-aligned content ends: one blank column, then the scrollbar's column.
fn edge(column: Rect) -> u16 {
    column.right() - 2
}

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
fn spans(buf: &mut Buffer, x: u16, y: u16, max: u16, parts: &[(&str, Style)]) -> u16 {
    let mut used = 0;
    for (text, style) in parts {
        used += put(buf, x + used, y, max.saturating_sub(used), text, *style);
    }
    used
}

fn spans_width(parts: &[(&str, Style)]) -> u16 {
    parts.iter().map(|(t, _)| width(t) as u16).sum()
}

fn fill(buf: &mut Buffer, rect: Rect, bg: Color) {
    let rect = rect.intersection(buf.area);
    for y in rect.top()..rect.bottom() {
        for x in rect.left()..rect.right() {
            buf[(x, y)].set_symbol(" ").set_bg(bg);
        }
    }
}

/// A progress bar in eighths (DESIGN-BRIEF 6.2). `None` draws an empty bar.
fn bar(buf: &mut Buffer, x: u16, y: u16, cells: u16, fraction: Option<f64>, color: Color) {
    const EIGHTHS: [&str; 9] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉", "█"];
    let eighths = (fraction.unwrap_or(0.0).clamp(0.0, 1.0) * f64::from(cells) * 8.0).floor() as u16;
    for i in 0..cells {
        let n = eighths.saturating_sub(i * 8).min(8);
        if x + i < buf.area.right() {
            buf[(x + i, y)].set_symbol(EIGHTHS[n as usize]).set_fg(color).set_bg(TRACK);
        }
    }
}

/// Key hints: bold keys, muted labels, three spaces apart. Pairs drop from the end, but the
/// last one stays.
fn hints(buf: &mut Buffer, x: u16, y: u16, max: u16, pairs: &[(&str, &str)]) {
    let mut pairs = pairs.to_vec();
    let length = |p: &[(&str, &str)]| {
        p.iter().map(|(k, l)| width(k) + width(l) + 1).sum::<usize>() + p.len().saturating_sub(1) * 3
    };
    while pairs.len() > 1 && length(&pairs) > max as usize {
        pairs.remove(pairs.len() - 2);
    }
    let mut at = x;
    for (i, (key, label)) in pairs.iter().enumerate() {
        if i > 0 {
            at += 3;
        }
        let end = x + max;
        at += put(buf, at, y, end.saturating_sub(at), key, bold(TEXT));
        at += put(buf, at, y, end.saturating_sub(at), &format!(" {label}"), fg(MUTED));
    }
}

pub fn render(buf: &mut Buffer, state: &State, mode: ColorMode) -> Option<Position> {
    let area = buf.area;
    fill(buf, area, BG);
    buf.set_style(area, fg(TEXT));
    let Some(screen) = layout::screen(state.size, state.help) else {
        too_small(buf, state.size);
        degrade(buf, mode);
        return None;
    };
    header(buf, state, screen.column);
    let mut cursor = query(buf, state, screen.column);
    sources(buf, state, screen.column);
    list(buf, state, screen.list);
    if let Some(rect) = screen.help {
        help(buf, rect);
    }
    footer(buf, state, &screen);
    if state.overlay.is_some() {
        if mode.paints_backgrounds() {
            dim(buf);
        }
        cursor = match &state.overlay {
            Some(Overlay::Palette(_)) => palette(buf, state),
            Some(Overlay::ConfirmRemove(id)) => {
                confirm(buf, state, *id);
                None
            }
            None => None,
        };
    }
    degrade(buf, mode);
    cursor
}

fn too_small(buf: &mut Buffer, (w, h): (u16, u16)) {
    put(buf, 2, 1, w.saturating_sub(4), "Make the window larger.", bold(TEXT));
    let (min_w, min_h) = layout::MIN_SIZE;
    let text = format!("torrential needs {min_w}×{min_h}. This one is {w}×{h}.");
    put(buf, 2, 3, w.saturating_sub(4), &text, fg(MUTED));
}

fn header(buf: &mut Buffer, state: &State, col: Rect) {
    put(buf, col.x, HEADER, 10, "torrential", bold(TEXT));
    fill(buf, Rect::new(col.x + 10, HEADER, 1, 1), ACCENT);
    let right = edge(col);
    let sep = (" · ", fg(DIM));
    let rate = format_rate(state.rate());
    let active = format!("{} active", state.active());
    let free = state.free_space.map(|f| format!("{} free", format_size(f)));
    let engine: Vec<(&str, Style)> = match &state.engine {
        Engine::Starting => vec![(state.spinner(), fg(ACCENT)), (" starting engine", fg(MUTED))],
        Engine::Ready => vec![("●", fg(SUCCESS)), (" engine ready", fg(MUTED))],
        Engine::Unreachable(_) => vec![("✗", fg(ERROR)), (" engine unreachable", fg(ERROR))],
    };
    let ready = state.engine == Engine::Ready;
    let mut full: Vec<(&str, Style)> = Vec::new();
    if ready {
        full.extend([("↓ ", fg(ACCENT)), (rate.as_str(), fg(TEXT)), sep, (active.as_str(), fg(MUTED))]);
        if let Some(free) = &free {
            full.extend([sep, (free.as_str(), fg(MUTED))]);
        }
        full.push(sep);
    }
    full.extend(engine.iter().copied());
    let mut short: Vec<(&str, Style)> = Vec::new();
    if ready {
        short.extend([("↓ ", fg(ACCENT)), (rate.as_str(), fg(TEXT)), sep]);
    }
    short.extend(engine.iter().copied());
    let room = col.width.saturating_sub(13);
    let parts = if spans_width(&full) <= room { full } else { short };
    let w = spans_width(&parts);
    spans(buf, right.saturating_sub(w), HEADER, w, &parts);
}

/// The query band. Returns where the terminal's cursor goes while typing.
fn query(buf: &mut Buffer, state: &State, col: Rect) -> Option<Position> {
    let focused = state.focus == Focus::Query;
    fill(buf, Rect::new(col.x, QUERY, col.width, 1), ELEMENT);
    put(buf, col.x, QUERY, 1, "┃", fg(if focused { ACCENT } else { BORDER }));
    let x = col.x + 2;
    let room = col.width.saturating_sub(4);
    let text = &state.query.text;
    if text.is_empty() {
        put(buf, x, QUERY, room, "Search… \"night of the living dead\"", fg(MUTED));
        if !focused {
            put_right(buf, edge(col), QUERY, "/ search", fg(DIM));
        }
    } else {
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
        put(buf, x, QUERY, room, &shown, fg(TEXT));
        if focused {
            return Some(Position::new(x + width(&before) as u16 - skip, QUERY));
        }
    }
    focused.then_some(Position::new(x, QUERY))
}

fn reason(outcome: &SourceOutcome) -> String {
    match outcome {
        SourceOutcome::Ok { .. } | SourceOutcome::Cached { .. } => "ok".into(),
        SourceOutcome::Timeout => "timeout".into(),
        SourceOutcome::Http { status } => format!("http {status}"),
        SourceOutcome::Connect { .. } => "connect".into(),
        SourceOutcome::BadResponse { .. } => "bad response".into(),
        SourceOutcome::Irrelevant => "irrelevant".into(),
    }
}

/// The line under the query: source status while and after a search.
fn sources(buf: &mut Buffer, state: &State, col: Rect) {
    let (x, y, room) = (col.x + 2, SOURCES, edge(col) - col.x - 2);
    let Some(search) = &state.search else {
        put(buf, x, y, room, "Downloads stop at 100% and never seed.", fg(DIM));
        return;
    };
    if state.filtering() {
        let text = format!("{} of {} results match · enter searches", state.results().len(), search.releases.len());
        put(buf, x, y, room, &text, fg(DIM));
        return;
    }
    let answered = search.sources.len();
    match &search.phase {
        SearchPhase::Running { pending } => {
            let mut text = " searching".to_string();
            if answered + pending.len() > 0 {
                text += &format!(" · {answered} of {} sources", answered + pending.len());
            }
            spans(buf, x, y, room, &[(state.spinner(), fg(ACCENT)), (&text, fg(DIM))]);
        }
        SearchPhase::Done => {
            let ok = search.sources.iter().filter(|s| s.outcome.is_ok()).count();
            let summary = format!("{ok} of {answered} sources ok");
            let failures: Vec<String> = search
                .sources
                .iter()
                .filter(|s| !s.outcome.is_ok())
                .map(|s| format!("{}: {}", s.source, reason(&s.outcome)))
                .collect();
            let failures = failures.join(" · ");
            let elapsed = format!("{:.1}s", f64::from(search.elapsed_ms) / 1000.0);
            let mut parts = vec![(summary.as_str(), fg(MUTED))];
            if !failures.is_empty() {
                parts.extend([(" · ", fg(DIM)), (failures.as_str(), fg(ERROR))]);
            }
            let used = spans(buf, x, y, room.saturating_sub(width(&elapsed) as u16 + 2), &parts);
            if used + width(&elapsed) as u16 + 2 <= room {
                put_right(buf, edge(col), y, &elapsed, fg(DIM));
            }
        }
        SearchPhase::Failed(error) => {
            spans(buf, x, y, room, &[("✗ ", fg(ERROR)), (&error.message, fg(ERROR))]);
        }
    }
}

/// Column positions shared by download rows and their per-file rows.
struct Columns {
    name: u16,
    bar: u16,
    bar_cells: u16,
    tail: u16,
    right: u16,
    wide: bool,
}

fn columns(x: u16, right: u16) -> Columns {
    let w = right - x;
    let (bar_cells, tail_cells, wide) = match w {
        96.. => (16, 18, true),
        74.. => (10, 18, true),
        _ => (8, 12, false),
    };
    let tail = right - tail_cells;
    let bar = tail - 7 - bar_cells;
    Columns { name: x + 10, bar, bar_cells, tail, right, wide }
}

struct Look {
    /// `None` shows the spinner.
    glyph: Option<&'static str>,
    color: Color,
    bar: Color,
}

fn look(status: &JobStatus) -> Look {
    let l = |glyph, color, bar| Look { glyph, color, bar };
    match status {
        JobStatus::Queued => l(Some("·"), DIM, DIM),
        JobStatus::Metadata | JobStatus::Checking => l(None, MUTED, MUTED),
        JobStatus::Downloading => l(Some("↓"), ACCENT, ACCENT),
        JobStatus::Paused => l(Some("‖"), DIM, DIM),
        JobStatus::Done { .. } => l(Some("✓"), SUCCESS, SUCCESS),
        JobStatus::Interrupted => l(Some("!"), WARNING, DIM),
        JobStatus::Error { .. } => l(Some("✗"), ERROR, DIM),
    }
}

fn list(buf: &mut Buffer, state: &State, area: Rect) {
    let placed = layout::blocks(state, area.height);
    let total = layout::total(&placed).max(area.height);
    let sheet_area = Rect::new(area.x, 0, area.width, total);
    let mut sheet = Buffer::empty(sheet_area);
    fill(&mut sheet, sheet_area, BG);
    let right = edge(area);
    for p in &placed {
        let (x, y) = (area.x, p.top);
        match p.block {
            Block::DownloadsLabel => {
                put(
                    &mut sheet,
                    x + 2,
                    y,
                    right - x - 2,
                    &format!("downloads · {}", state.downloads.len()),
                    bold(MUTED),
                );
            }
            Block::Download(id) => {
                if let Some(job) = state.job(id) {
                    download_row(&mut sheet, state, job, x, right, y);
                }
            }
            Block::DownloadDetail(id) => {
                if let Some(job) = state.job(id) {
                    download_detail(&mut sheet, state, job, x, right, y, p.height);
                }
            }
            Block::Folded => {
                if let Some((paused, done)) = state.folded() {
                    let mut parts = Vec::new();
                    if paused > 0 {
                        parts.push(format!("{paused} paused"));
                    }
                    if done > 0 {
                        parts.push(format!("{done} done"));
                    }
                    put(&mut sheet, x + 2, y, right - x - 2, &parts.join(" · "), fg(DIM));
                }
            }
            Block::Gap => {}
            Block::ResultsLabel => results_label(&mut sheet, state, x, right, y),
            Block::Result(id) => {
                if let Some(release) = state.release(id) {
                    result_row(&mut sheet, state, release, x, right, y);
                }
            }
            Block::ResultDetail(id) => result_detail(&mut sheet, state, id, x, right, y, p.height),
            Block::Empty => empty(&mut sheet, state, x, right, y),
        }
    }
    for row in 0..area.height {
        let from = state.scroll + row;
        if from >= sheet.area.height {
            break;
        }
        for col in area.x..area.right() {
            buf[(col, area.y + row)] = sheet[(col, from)].clone();
        }
    }
    let content = layout::total(&placed);
    if content > area.height {
        let h = area.height;
        let thumb = (u32::from(h) * u32::from(h) / u32::from(content)).max(1) as u16;
        let travel = u32::from(h - thumb);
        let at = (u32::from(state.scroll) * travel / u32::from(content - h)) as u16;
        let color = if state.focus == Focus::Query { FAINT } else { DIM };
        for row in at..(at + thumb).min(h) {
            put(buf, area.right() - 1, area.y + row, 1, "▐", fg(color));
        }
    }
}

/// The selected row: a full-width selection background and the gutter bar.
fn selected(buf: &mut Buffer, state: &State, x: u16, right: u16, y: u16, rows: u16) {
    fill(buf, Rect::new(x, y, right - x, rows), SELECTION);
    let bar = if state.focus == Focus::Query { FAINT } else { ACCENT };
    for row in y..y + rows {
        put(buf, x, row, 1, "┃", fg(bar));
    }
}

fn short(id: &str) -> &str {
    &id[..6.min(id.len())]
}

fn download_row(buf: &mut Buffer, state: &State, job: &Job, x: u16, right: u16, y: u16) {
    let is_selected = state.cursor == Some(Item::Download(job.id));
    if is_selected {
        selected(buf, state, x, right, y, 1);
    }
    let c = columns(x, right);
    put(buf, x + 2, y, 6, short(&job.id.to_hex()), fg(MINT));
    let name = job.name.clone().unwrap_or_else(|| job.id.to_hex());
    let style = if is_selected && state.focus != Focus::Query { bold(ACCENT) } else { fg(TEXT) };
    put(buf, c.name, y, c.bar.saturating_sub(c.name + 2), &name, style);
    let look = look(&job.status);
    bar(buf, c.bar, y, c.bar_cells, job.progress.fraction(), look.bar);
    let pct = job.progress.size.map_or("–".to_string(), |size| percent(job.progress.verified, size));
    put_right(buf, c.tail - 2, y, &pct, fg(if job.status == JobStatus::Downloading { TEXT } else { MUTED }));
    tail(buf, state, job, &c, y);
}

/// The right end of a download row: rate and time left while downloading, else the state.
fn tail(buf: &mut Buffer, state: &State, job: &Job, c: &Columns, y: u16) {
    if let Some(action) = state.pending.get(&job.id) {
        spans_right(buf, c.right, y, &[(state.spinner(), fg(ACCENT)), (" ", fg(DIM)), (action.progressive(), fg(DIM))]);
        return;
    }
    if job.status == JobStatus::Downloading {
        let rate = format_rate(job.progress.down_rate);
        if c.wide {
            let eta = match job.progress.eta {
                Some(secs) if job.progress.down_rate >= 1024 => format_duration(Duration::from_secs(secs)),
                _ => "–".into(),
            };
            put_right(buf, c.right - 6, y, &rate, fg(TEXT));
            put_right(buf, c.right, y, &eta, fg(MUTED));
        } else {
            put_right(buf, c.right, y, &rate, fg(TEXT));
        }
        return;
    }
    let look = look(&job.status);
    let (glyph, glyph_color) = match look.glyph {
        Some(glyph) => (glyph, look.color),
        None => (state.spinner(), ACCENT),
    };
    spans_right(buf, c.right, y, &[(glyph, fg(glyph_color)), (" ", fg(DIM)), (job.status.name(), fg(look.color))]);
}

fn spans_right(buf: &mut Buffer, right: u16, y: u16, parts: &[(&str, Style)]) {
    let w = spans_width(parts);
    spans(buf, right.saturating_sub(w), y, w, parts);
}

fn download_detail(buf: &mut Buffer, state: &State, job: &Job, x: u16, right: u16, y: u16, height: u16) {
    fill(buf, Rect::new(x + 2, y, right - x - 2, height), PANEL);
    let c = columns(x, right);
    let room = right.saturating_sub(c.name + 1);
    let home = state.home.as_deref();
    let path = job.path.as_ref().unwrap_or(&job.dir);
    put(buf, c.name, y, room, &middle(&home_path(path, home), room as usize), fg(DIM));
    let p = &job.progress;
    let mut facts = Vec::new();
    match (&job.status, p.size) {
        (JobStatus::Metadata, _) | (_, None) => facts.push("Fetching the file list from peers".to_string()),
        (JobStatus::Done { finished_at }, Some(size)) => {
            facts.push(format_size(size));
            facts.push(format!("finished {} ago", age(*finished_at, state.clock)));
        }
        (_, Some(size)) => facts.push(format!("{} of {}", format_size(p.verified), format_size(size))),
    }
    if matches!(job.status, JobStatus::Downloading | JobStatus::Metadata) {
        facts.push(format!("{} peers", p.peers));
    }
    if let (JobStatus::Downloading, Some(eta)) = (&job.status, p.eta) {
        facts.push(format!("{} left", format_duration(Duration::from_secs(eta))));
    }
    if let Some(files) = state.download_files.get(&job.id).filter(|f| !f.files.is_empty()) {
        let (picked, n) = (files.files.iter().filter(|f| f.selected).count(), files.files.len());
        facts.push(match (picked, n) {
            (1, 1) => "1 file".into(),
            (p, n) if p == n => format!("{n} files"),
            (p, n) => format!("{p} of {n} files"),
        });
    }
    put(buf, c.name, y + 1, room, &facts.join(" · "), fg(MUTED));
    let mut row = y + 2;
    if let JobStatus::Error { error } = &job.status {
        put(buf, c.name, row, room, &format!("✗ {}", error.message), fg(ERROR));
        row += 1;
        if let Some(hint) = &error.hint {
            put(buf, c.name, row, room, hint, fg(DIM));
            row += 1;
        }
    }
    let (shown, more) = layout::download_file_rows(state, job, layout::list_height(state));
    let Some(files) = state.download_files.get(&job.id) else { return };
    let picked: Vec<_> = files.files.iter().filter(|f| f.selected).collect();
    let any_progress = picked.iter().any(|f| f.verified > 0) || job.progress.verified == 0;
    for file in picked.iter().take(shown) {
        let name = file.path.rsplit('/').next().unwrap_or(&file.path);
        let room = c.bar.saturating_sub(c.name + 2);
        put(buf, c.name, row, room, &middle(name, room as usize), fg(TEXT));
        if any_progress {
            let fraction = if file.size == 0 { 1.0 } else { file.verified as f64 / file.size as f64 };
            bar(buf, c.bar, row, c.bar_cells, Some(fraction), look(&job.status).bar);
            put_right(buf, c.tail - 2, row, &percent(file.verified, file.size), fg(MUTED));
            put_right(buf, c.right, row, &format_size(file.size), fg(DIM));
        } else {
            put_right(buf, c.right, row, &format_size(file.size), fg(DIM));
        }
        row += 1;
    }
    if more {
        let text = format!("+{} more files", picked.len() - shown);
        put(buf, c.name, row, room, &text, fg(DIM));
    }
}

fn results_label(buf: &mut Buffer, state: &State, x: u16, right: u16, y: u16) {
    let label = match &state.search {
        Some(_) => format!("results · {}", state.results().len()),
        None => "results".into(),
    };
    put(buf, x + 2, y, right - x - 2, &label, bold(MUTED));
    if state.search.is_some() {
        put_right(buf, right, y, state.sort.label(), fg(DIM));
    }
}

fn flag_color(flag: Flag) -> Color {
    match flag {
        Flag::Official | Flag::PublicDomain | Flag::CreativeCommons => MINT,
        Flag::Trusted => MUTED,
        Flag::Cam => WARNING,
        Flag::Adult => DIM,
    }
}

fn seeds_color(n: Option<u32>) -> Color {
    match n {
        None => FAINT,
        Some(0) => ERROR,
        Some(1..=9) => DIM,
        Some(_) => TEXT,
    }
}

fn result_row(buf: &mut Buffer, state: &State, release: &Release, x: u16, right: u16, y: u16) {
    let is_selected = state.cursor == Some(Item::Result(release.id));
    if is_selected {
        selected(buf, state, x, right, y, 2);
    }
    let name_x = x + 10;
    put(buf, x + 2, y, 6, short(&release.id.to_ref()), fg(MINT));
    let size = release.size.map_or("–".to_string(), format_size);
    let size_x = put_right(buf, right, y, &size, fg(TEXT));
    let style = if is_selected && state.focus != Focus::Query { bold(ACCENT) } else { fg(TEXT) };
    put(buf, name_x, y, size_x.saturating_sub(name_x + 2), &release.name, style);
    let seeds_text = format!("{} seeds", seeds(release.seeders));
    let seeds_x = put_right(buf, right, y + 1, &seeds_text, fg(seeds_color(release.seeders)));
    let room = seeds_x.saturating_sub(name_x + 2);
    let mut at = name_x;
    for flag in &release.flags {
        let w = put(buf, at, y + 1, (name_x + room).saturating_sub(at), flag.as_str(), fg(flag_color(*flag)));
        at += w + 1;
    }
    if !release.flags.is_empty() {
        at += 1;
    }
    let mut facts: Vec<String> = release.sources.first().cloned().into_iter().collect();
    if let Some(published) = release.published {
        facts.push(age(published, state.clock));
    }
    put(buf, at, y + 1, (name_x + room).saturating_sub(at), &facts.join(" · "), fg(DIM));
}

fn result_detail(buf: &mut Buffer, state: &State, id: ReleaseId, x: u16, right: u16, y: u16, height: u16) {
    fill(buf, Rect::new(x + 2, y, right - x - 2, height), PANEL);
    let (name_x, room) = (x + 10, right.saturating_sub(x + 11));
    let files = state.files.get(&id);
    if state.focus == Focus::Files {
        match files {
            Some(Files::Ready(tree)) => file_tree(buf, state, tree, x, right, y, height),
            Some(Files::Loading { since, .. }) => {
                let secs = state.now.duration_since(*since).as_secs();
                let text = format!(" Fetching the file list from peers · {secs}s");
                spans(buf, x + 4, y, room, &[(state.spinner(), fg(ACCENT)), (&text, fg(MUTED))]);
                let note = "Magnet links need a peer to send the file list before you can pick files.";
                put(buf, x + 4, y + 1, right - x - 5, note, fg(DIM));
            }
            Some(Files::Failed(error)) => {
                put(buf, x + 4, y, right - x - 5, &format!("✗ {}", error.message), fg(ERROR));
                put(
                    buf,
                    x + 4,
                    y + 1,
                    right - x - 5,
                    error.hint.as_deref().unwrap_or("Press enter to try again."),
                    fg(DIM),
                );
            }
            None => {}
        }
        return;
    }
    let downloaded = matches!(id, ReleaseId::Torrent(t) if state.job(t).is_some());
    if downloaded {
        put(buf, name_x, y, room, "In downloads. Press d to see it.", fg(MUTED));
        return;
    }
    match files {
        Some(Files::Ready(tree)) => {
            let n = tree.files.len();
            let facts = format!(
                "{n} {} · {} picked · {}   ",
                if n == 1 { "file" } else { "files" },
                tree.picked_count(),
                format_size(tree.picked_size())
            );
            let used = put(buf, name_x, y, room, &facts, fg(DIM));
            hints(buf, name_x + used, y, room.saturating_sub(used), &[("enter", "choose files"), ("D", "download")]);
        }
        Some(Files::Loading { .. }) => {
            spans(buf, name_x, y, room, &[(state.spinner(), fg(ACCENT)), (" Fetching the file list", fg(DIM))]);
        }
        Some(Files::Failed(error)) => {
            put(buf, name_x, y, room, &format!("✗ {}", error.message), fg(ERROR));
        }
        None => hints(buf, name_x, y, room, &[("enter", "choose files"), ("D", "download")]),
    }
}

fn file_tree(buf: &mut Buffer, state: &State, tree: &Tree, x: u16, right: u16, y: u16, height: u16) {
    let visible = tree.visible();
    let rows = usize::from(height.saturating_sub(2));
    let inner = x + 4;
    let end = right - 1;
    let first = tree.scroll + 1;
    let last = (tree.scroll + rows).min(visible.len());
    let range = if visible.len() > rows {
        format!("files · {first}–{last} of {}", visible.len())
    } else {
        format!("files · {}", tree.files.len())
    };
    put(buf, inner, y, end - inner, &range, fg(MUTED));
    let left_out = tree.risky_left_out();
    if left_out > 0 {
        let text = format!("{left_out} executable{} excluded", if left_out == 1 { "" } else { "s" });
        put_right(buf, end, y, &text, fg(WARNING));
    }
    for (k, &n) in visible.iter().skip(tree.scroll).take(rows).enumerate() {
        let row = y + 1 + k as u16;
        let node = &tree.nodes[n];
        let here = n == tree.cursor;
        if here {
            fill(buf, Rect::new(x + 2, row, right - x - 2, 1), SELECTION);
            put(buf, x + 2, row, 1, "┃", fg(ACCENT));
        }
        let at = inner + node.depth * 2;
        let (glyph, color) = match tree.check(n) {
            Check::All => ("●", ACCENT),
            Check::Some => ("◐", ACCENT),
            Check::None => ("○", FAINT),
        };
        put(buf, at, row, 1, glyph, fg(color));
        let (fold, size, risky) = match node.kind {
            NodeKind::Folder { .. } => (if tree.folded.contains(&n) { "▸" } else { "▾" }, None, false),
            NodeKind::File(i) => ("", Some(tree.files[i].size), tree.is_risky(i)),
        };
        put(buf, at + 2, row, 1, fold, fg(DIM));
        let mut tail_x = end;
        if let Some(size) = size {
            tail_x = put_right(buf, end, row, &format_size(size), fg(DIM));
        }
        if risky {
            tail_x = put_right(buf, tail_x - 2, row, "! executable", fg(WARNING));
        }
        let style = match (here, &node.kind) {
            (true, _) => fg(ACCENT),
            (false, NodeKind::Folder { .. }) => fg(MUTED),
            (false, NodeKind::File(_)) => fg(TEXT),
        };
        let name_x = at + 4;
        let room = tail_x.saturating_sub(name_x + 2);
        put(buf, name_x, row, room, &middle(&node.name, room as usize), style);
    }
    let action = y + height - 1;
    let picked = tree.picked_count();
    let mut summary = format!(
        "{picked} of {} files · {} of {}",
        tree.files.len(),
        format_size(tree.picked_size()),
        format_size(tree.total_size())
    );
    let button = if picked == 0 {
        " nothing picked ".to_string()
    } else {
        format!(" download {} ", format_size(tree.picked_size()))
    };
    let key_x = put_right(buf, end, action, " enter", fg(MUTED));
    let button_x = key_x.saturating_sub(width(&button) as u16);
    if picked == 0 {
        put(buf, button_x, action, key_x - button_x, &button, fg(BG).bg(FAINT));
    } else {
        put(buf, button_x, action, key_x - button_x, &button, bold(ON_ACCENT).bg(ACCENT));
    }
    let room = button_x.saturating_sub(inner + 2);
    if let Some(free) = state.free_space.map(|f| format!(" · {} free", format_size(f)))
        && width(&summary) + width(&free) <= room as usize
    {
        summary += &free;
    }
    put(buf, inner, action, room, &summary, fg(MUTED));
}

fn empty(buf: &mut Buffer, state: &State, x: u16, right: u16, y: u16) {
    let (tx, room) = (x + 10, right.saturating_sub(x + 11));
    let Some(search) = &state.search else {
        put(buf, tx, y + 1, room, "Search catalogs and indexes without leaving this list.", fg(MUTED));
        hints(buf, tx, y + 2, room, &[("/", "search"), ("ctrl+p", "commands")]);
        return;
    };
    if state.filtering() {
        put(buf, tx, y + 1, room, &format!("Nothing loaded matches \"{}\".", state.query.text.trim()), fg(TEXT));
        put(buf, tx, y + 2, room, "Press enter to search for it.", fg(DIM));
        return;
    }
    match &search.phase {
        SearchPhase::Running { .. } => {}
        SearchPhase::Done => {
            put(buf, tx, y + 1, room, &format!("No results for \"{}\".", search.text), fg(TEXT));
            put(buf, tx, y + 2, room, "Try fewer or different words, or check the spelling.", fg(DIM));
        }
        SearchPhase::Failed(error) => {
            put(buf, tx, y + 1, room, "✗ No source answered.", fg(ERROR));
            let hint = error.hint.as_deref().unwrap_or("Check your connection, then press enter to search again.");
            put(buf, tx, y + 2, room, hint, fg(DIM));
        }
    }
}

const HELP: [(&str, &str, &str, &str); 8] = [
    ("j k ↑ ↓", "move", "/", "search"),
    ("enter l →", "open · choose files", "tab", "next section"),
    ("h ←", "collapse · fold", "d  r", "downloads · results"),
    ("space", "pick a file · pause or resume", "a", "pick all or none"),
    ("D", "download", "s", "sort results"),
    ("v  x", "verify · remove", "o  y", "open folder · copy path"),
    ("g G", "first · last", "ctrl+p", "commands"),
    ("esc", "back", "q", "quit · downloads keep running"),
];

fn help(buf: &mut Buffer, area: Rect) {
    fill(buf, area, PANEL);
    let (x, right) = (area.x + 2, edge(area));
    put(buf, x, area.y, 4, "Help", bold(ACCENT));
    put_right(buf, right, area.y, "esc close", fg(DIM));
    let w = right - x;
    let split = if w >= 90 { w / 2 } else { w * 3 / 5 };
    for (i, (k1, l1, k2, l2)) in HELP.iter().enumerate() {
        let y = area.y + 1 + i as u16;
        if y >= area.bottom() {
            return;
        }
        put(buf, x, y, 11, k1, bold(TEXT));
        put(buf, x + 12, y, split.saturating_sub(13), l1, fg(MUTED));
        put(buf, x + split, y, 8, k2, bold(TEXT));
        put(buf, x + split + 9, y, w.saturating_sub(split + 9), l2, fg(MUTED));
    }
    let note = area.y + 1 + HELP.len() as u16;
    if note < area.bottom() {
        put(buf, x, note, w, "Downloads stop at 100% and never seed.", fg(DIM));
    }
}

fn footer(buf: &mut Buffer, state: &State, screen: &Screen) {
    let col = screen.column;
    let (x, room) = (col.x + 2, edge(col) - col.x - 2);
    match &state.toast {
        Some(toast) => {
            let color = match toast.tone {
                Tone::Info => MUTED,
                Tone::Success => SUCCESS,
                Tone::Warning => WARNING,
                Tone::Error => ERROR,
            };
            put(buf, x, screen.status, room, &toast.text, fg(color));
        }
        None => {
            put(buf, x, screen.status, room, &status(state), fg(DIM));
        }
    }
    let pause = match state.cursor.and_then(|item| match item {
        Item::Download(id) => state.job(id),
        Item::Result(_) => None,
    }) {
        Some(job) if matches!(job.status, JobStatus::Paused | JobStatus::Interrupted | JobStatus::Error { .. }) => {
            "resume"
        }
        _ => "pause",
    };
    let pairs: Vec<(&str, &str)> = match (state.focus, state.cursor) {
        (Focus::Query, _) => {
            vec![("enter", "search"), ("↓", "list"), ("ctrl+u", "clear"), ("ctrl+p", "commands"), ("esc", "back")]
        }
        (Focus::Files, _) => {
            vec![("space", "pick"), ("a", "all"), ("←→", "fold"), ("enter", "download"), ("esc", "back"), ("?", "help")]
        }
        (Focus::List, Some(Item::Download(_))) => vec![
            ("↑↓", "move"),
            ("space", pause),
            ("v", "verify"),
            ("x", "remove"),
            ("o", "open"),
            ("y", "copy path"),
            ("?", "help"),
        ],
        (Focus::List, Some(Item::Result(_))) => {
            vec![("↑↓", "move"), ("enter", "files"), ("D", "download"), ("s", "sort"), ("/", "search"), ("?", "help")]
        }
        (Focus::List, None) => vec![("/", "search"), ("ctrl+p", "commands"), ("?", "help"), ("q", "quit")],
    };
    hints(buf, x, screen.hints, room, &pairs);
}

/// The footer's left status when no toast shows: where the cursor is.
fn status(state: &State) -> String {
    if state.focus == Focus::Files {
        return "Executables stay excluded unless you pick them.".into();
    }
    let Some(cursor) = state.cursor.filter(|_| state.focus == Focus::List) else { return String::new() };
    match cursor {
        Item::Download(id) => {
            let at = state.downloads.iter().position(|j| j.id == id).map_or(0, |i| i + 1);
            format!("download {at} of {}", state.downloads.len())
        }
        Item::Result(id) => {
            let results = state.results();
            let at = results.iter().position(|r| r.id == id).map_or(0, |i| i + 1);
            format!("result {at} of {}", results.len())
        }
    }
}

/// Draws the frame of a dialog `w` wide and `h` tall, and returns its rectangle.
fn dialog(buf: &mut Buffer, w: u16, h: u16) -> Rect {
    let area = buf.area;
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    let top = (area.height / 4).min(area.height.saturating_sub(h + 1)).max(1);
    let rect = Rect::new(area.x + (area.width - w) / 2, top, w, h);
    fill(buf, rect, PANEL);
    let line = "─".repeat(usize::from(w - 2));
    put(buf, rect.x, rect.y, w, &format!("╭{line}╮"), fg(BORDER));
    put(buf, rect.x, rect.bottom() - 1, w, &format!("╰{line}╯"), fg(BORDER));
    for y in rect.y + 1..rect.bottom() - 1 {
        put(buf, rect.x, y, 1, "│", fg(BORDER));
        put(buf, rect.right() - 1, y, 1, "│", fg(BORDER));
    }
    rect
}

fn palette(buf: &mut Buffer, state: &State) -> Option<Position> {
    let Some(Overlay::Palette(p)) = &state.overlay else { return None };
    let commands = state.palette_matches();
    let max_rows = buf.area.height.saturating_sub(12).max(3);
    let rows = (commands.len().max(1) as u16).min(max_rows);
    let more = commands.len() as u16 > rows;
    let r = dialog(buf, 60, 8 + rows + u16::from(more));
    let (x, inner) = (r.x + 3, r.width - 6);
    put(buf, x, r.y + 1, inner, "Commands", bold(TEXT));
    put_right(buf, r.right() - 3, r.y + 1, "esc", fg(MUTED));
    fill(buf, Rect::new(x, r.y + 3, inner, 1), ELEMENT);
    let text = &p.filter;
    if text.is_empty() {
        put(buf, x + 1, r.y + 3, inner - 2, "Search commands", fg(DIM));
    } else {
        put(buf, x + 1, r.y + 3, inner - 2, text, fg(TEXT));
    }
    put(buf, x, r.y + 5, inner, "Suggested", bold(ACCENT));
    if commands.is_empty() {
        put(buf, x, r.y + 6, inner, "No results found", fg(DIM));
    }
    let offset = p.selected.saturating_sub(usize::from(rows) - 1);
    for (k, command) in commands.iter().enumerate().skip(offset).take(usize::from(rows)) {
        let y = r.y + 6 + (k - offset) as u16;
        let active = k == p.selected;
        if active {
            fill(buf, Rect::new(x - 1, y, inner + 2, 1), ACCENT);
        }
        let label = if active { bold(ON_ACCENT) } else { fg(TEXT) };
        let key = if active { bold(ON_ACCENT) } else { fg(MUTED) };
        let key_x = put_right(buf, r.right() - 3, y, command.key, key);
        put(buf, x, y, key_x.saturating_sub(x + 1), &command.label, label);
    }
    if more {
        put(buf, x, r.y + 6 + rows, inner, "…", fg(FAINT));
    }
    let caret = x + 1 + width(text).min(usize::from(inner) - 2) as u16;
    Some(Position::new(caret, r.y + 3))
}

fn confirm(buf: &mut Buffer, state: &State, id: torrential_core::TorrentId) {
    let r = dialog(buf, 60, 9);
    let (x, inner) = (r.x + 3, r.width - 6);
    put(buf, x, r.y + 1, inner, "Remove from the list?", bold(TEXT));
    put_right(buf, r.right() - 3, r.y + 1, "esc", fg(MUTED));
    let job = state.job(id);
    let name = job.and_then(|j| j.name.clone()).unwrap_or_else(|| id.to_hex());
    put(buf, x, r.y + 3, inner, &name, fg(MUTED));
    let dir = job.map_or("~".to_string(), |j| home_path(&j.dir, state.home.as_deref()));
    put(buf, x, r.y + 4, inner, &format!("Its files stay in {dir}."), fg(DIM));
    let y = r.y + 7;
    spans(buf, x, y, inner, &[("cancel", fg(TEXT)), (" esc", fg(MUTED))]);
    let key_x = put_right(buf, r.right() - 3, y, " enter", fg(MUTED));
    let label = " remove ";
    put(buf, key_x - width(label) as u16, y, width(label) as u16, label, bold(ON_ACCENT).bg(ACCENT));
}
