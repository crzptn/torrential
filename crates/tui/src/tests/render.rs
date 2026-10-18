//! Snapshots of whole screens at the three sizes the design is checked at, plus the colors
//! that carry meaning.

use crossterm::event::KeyCode;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use torrential_core::protocol::{GetOutcome, GetReply};

use super::*;
use crate::theme::{self, ColorMode};
use crate::view;

const SIZES: [(u16, u16); 3] = [(80, 24), (100, 30), (160, 45)];

fn shot(state: &State) -> String {
    let (w, h) = state.size;
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|frame| {
            view::render(frame.buffer_mut(), state, ColorMode::TrueColor);
        })
        .unwrap();
    terminal.backend().to_string()
}

fn snapshots(name: &str, scenario: impl Fn((u16, u16)) -> State) {
    for size in SIZES {
        insta::assert_snapshot!(format!("{name}_{}x{}", size.0, size.1), shot(&scenario(size)));
    }
}

fn idle(size: (u16, u16)) -> State {
    let mut state = with_downloads(size);
    state.update(Msg::Files { item: Item::Download(id("6f2c81")), result: Ok(wikipedia_files()) });
    state
}

fn streaming(size: (u16, u16)) -> State {
    let mut state = with_downloads(size);
    press(&mut state, "/");
    press(&mut state, "night of the living dead");
    state.update(key(KeyCode::Enter));
    let (partial, pending) = partial();
    state.update(Msg::SearchUpdate { search: 1, result: partial, pending });
    state
}

fn file_tree(size: (u16, u16)) -> State {
    let mut state = searched(size);
    press(&mut state, "j");
    state.update(key(KeyCode::Enter));
    state.update(Msg::Files { item: Item::Result(result_id("b7f20d")), result: Ok(dvd_files()) });
    press(&mut state, "jjjj jj ");
    state
}

fn download(size: (u16, u16)) -> State {
    let mut state = searched(size);
    press(&mut state, "d");
    state.update(Msg::Files { item: Item::Download(id("6f2c81")), result: Ok(wikipedia_files()) });
    state
}

#[test]
fn idle_screens() {
    snapshots("idle", idle);
}

#[test]
fn streaming_screens() {
    snapshots("streaming", streaming);
}

#[test]
fn file_tree_screens() {
    snapshots("file_tree", file_tree);
}

#[test]
fn download_screens() {
    snapshots("download", download);
}

#[test]
fn download_files_screens() {
    snapshots("download_files", |size| {
        let mut state = with_downloads(size);
        press(&mut state, "jj");
        state.update(Msg::Files { item: Item::Download(id("91fd3b")), result: Ok(stackexchange_files()) });
        state.update(key(KeyCode::Enter));
        press(&mut state, "jjjjjjjjjjjjjj");
        state
    });
}

#[test]
fn help_screens() {
    snapshots("help", |size| {
        let mut state = searched(size);
        press(&mut state, "?");
        state
    });
}

#[test]
fn a_download_says_stalled_only_after_a_quiet_spell() {
    let mut state = with_downloads((100, 30));
    let wikipedia = |rate: u64| {
        let mut job = jobs()[0].clone();
        job.progress.down_rate = rate;
        job.progress.eta = None;
        Msg::Watch(Event::Job { job: Box::new(job) })
    };
    state.update(wikipedia(600));
    let trickled = state.now;
    assert!(line(&render(&state), 4).ends_with("22%"), "bytes arrive without an ETA: {}", line(&render(&state), 4));
    later(&mut state, 20_000);
    state.update(wikipedia(0));
    assert!(line(&render(&state), 4).ends_with("22%"), "quiet for 20 s: {}", line(&render(&state), 4));
    assert_eq!(state.next_stall(), Some(trickled + Duration::from_secs(30)));
    later(&mut state, 10_000);
    state.update(Msg::Tick);
    assert!(line(&render(&state), 4).ends_with("22%       stalled"), "{}", line(&render(&state), 4));
    assert_eq!(state.next_stall(), None);
}

/// Wikipedia as the engine sends it, at 100×30, and the row it lands on.
fn wikipedia_with(status: JobStatus, map: Option<&str>) -> (State, Buffer, u16) {
    let mut state = with_downloads((100, 30));
    let mut job = jobs()[0].clone();
    job.status = status;
    job.progress.piece_map = map.map(|m| m.parse().unwrap());
    state.update(Msg::Watch(Event::Job { job: Box::new(job) }));
    let buf = render(&state);
    let (_, y) = find(&buf, "wikipedia-en-2026-09-text");
    (state, buf, y)
}

/// Thirty-two wanted slices among unwanted ones, two to each cell of a 16-cell gauge.
const SCATTERED: &str = concat!(
    "8-8-", "--44", "0--0", "-01-", "8--7", "12--", "----", "6666", "-22-", "3-4-", "----", "8888", "0-0-", "--0-",
    "-0--", "4444",
);

/// One small file in a large torrent: five wanted slices.
const ONE_FILE: &str = concat!("----------------------------------------", "84026", "-------------------");

#[test]
fn a_download_getting_bytes_shows_where_its_pieces_sit() {
    let (state, buf, y) = wikipedia_with(JobStatus::Downloading, Some(SCATTERED));
    assert_eq!(
        line(&buf, y),
        "    getting ● wikipedia-en-2026-09-text                       █▄▁▁▇▂▆▆▂▄██▁▁▄▄   22%      30m left"
    );
    let (x, _) = find(&buf, "█▄▁▁▇");
    let plain = render_in(&state, ColorMode::NoColor);
    assert!(plain[(x + 2, y)].modifier.contains(Modifier::DIM), "nothing has arrived: a faint floor");
    assert!(!plain[(x + 3, y)].modifier.contains(Modifier::DIM), "an eighth has arrived: a full-ink floor");
}

#[test]
fn a_gauge_spreads_a_few_wanted_slices_across_every_cell() {
    let (_, buf, y) = wikipedia_with(JobStatus::Downloading, Some(ONE_FILE));
    assert_eq!(
        line(&buf, y),
        "    getting ● wikipedia-en-2026-09-text                       ████▄▄▄▁▁▁▂▂▂▆▆▆   22%      30m left"
    );
    let (_, buf, y) = wikipedia_with(JobStatus::Downloading, Some(&"-".repeat(64)));
    assert!(line(&buf, y).contains(" ━━━╸──────────── "), "nothing wanted: {}", line(&buf, y));
}

#[test]
fn a_stalled_download_draws_the_rule_until_bytes_arrive_again() {
    let mut state = with_downloads((100, 30));
    state.update(Msg::Files { item: Item::Download(id("6f2c81")), result: Ok(wikipedia_files()) });
    let wikipedia = |rate: u64| {
        let mut job = jobs()[0].clone();
        job.progress.down_rate = rate;
        job.progress.eta = None;
        Msg::Watch(Event::Job { job: Box::new(job) })
    };
    let gauge = "    getting ● wikipedia-en-2026-09-text                       ▃▂▁▄▂▂▁▃▂▁▃▂▂▂▁▁   22%";
    let file_ink = |buf: &Buffer, x: u16| buf[(x, find(buf, "s-multistream.xml.bz2").1)].fg;
    state.update(wikipedia(600));
    let buf = render(&state);
    let (x, _) = find(&buf, "▃▂▁▄");
    assert_eq!((line(&buf, 4), file_ink(&buf, x)), (gauge.to_string(), theme::ACTION), "bytes arrive");
    later(&mut state, 20_000);
    state.update(wikipedia(0));
    assert_eq!(line(&render(&state), 4), gauge, "quiet for 20 s");
    later(&mut state, 10_000);
    state.update(Msg::Tick);
    let buf = render(&state);
    let stalled = "    getting ● wikipedia-en-2026-09-text                       ━━━╸────────────   22%       stalled";
    assert_eq!(line(&buf, 4), stalled);
    assert_eq!((buf[(x, 4)].fg, file_ink(&buf, x)), (theme::PENCIL, theme::PENCIL), "no bytes: pencil, not sage");
    state.update(wikipedia(600));
    assert_eq!(line(&render(&state), 4), gauge, "bytes again");
}

#[test]
fn a_download_keeps_the_rule_until_it_has_a_map_and_while_it_is_not_getting_bytes() {
    let (_, buf, y) = wikipedia_with(JobStatus::Downloading, None);
    assert!(line(&buf, y).contains(" ━━━╸──────────── "), "no map yet: {}", line(&buf, y));
    let (_, buf, y) = wikipedia_with(JobStatus::Paused, Some(SCATTERED));
    assert!(line(&buf, y).contains(" ━━━╸──────────── "), "paused: {}", line(&buf, y));
}

#[test]
fn a_long_query_is_cut_so_failed_sources_stay_in_view() {
    let mut state = with_downloads((80, 24));
    press(&mut state, "/night of the living dead 1968 1080p bluray x264");
    state.update(key(KeyCode::Enter));
    state.update(Msg::SearchDone { search: 1, result: Ok(result(night()[2..3].to_vec(), final_sources())) });
    let buf = render(&state);
    assert_eq!(line(&buf, 2), "     search │ night of the living dead 1968 1080p bluray…   1 found · 2 failed");
    let (x, y) = find(&buf, "2 failed");
    assert_eq!(buf[(x, y)].fg, theme::ERROR);
    press(&mut state, "/");
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
    let caret = view::render(&mut buf, &state, ColorMode::TrueColor);
    assert_eq!(line(&buf, 2), "     search ●  of the living dead 1968 1080p bluray x264    1 found · 2 failed");
    assert_eq!(caret, Some(ratatui::layout::Position::new(56, 2)));
}

#[test]
fn a_toast_never_pushes_out_the_first_hint() {
    let started = "Started Night of the Living Dead (1968) [DVD]";
    let mut state = searched((100, 30));
    press(&mut state, "d");
    state.say(crate::state::Tone::Success, started);
    let footer = line(&render(&state), 29);
    assert_eq!(footer, format!("{:14}{started}{:8}space pause   x remove   o open", "", ""));
    state.update(Msg::Resize(60, 16));
    let footer = line(&render(&state), 15);
    assert_eq!(footer, format!("{:14}Started Night of the Living…     space pause", ""));
}

#[test]
fn every_file_of_a_download_is_one_key_away() {
    let mut state = with_downloads((80, 24));
    press(&mut state, "jj");
    state.update(Msg::Files { item: Item::Download(id("91fd3b")), result: Ok(stackexchange_files()) });
    let screen = shot(&state);
    assert!(screen.contains("and 15 more files · press enter to list them"), "{screen}");
    state.update(key(KeyCode::Enter));
    press(&mut state, "G");
    let buf = render(&state);
    let (_, y) = find(&buf, "part20.7z");
    assert_eq!(buf[(MARK, y)].symbol(), "●");
    assert!(line(&buf, y).ends_with("100%       100 MiB"), "{}", line(&buf, y));
    assert_eq!(buf[(MARK, y - 12)].symbol(), "↑", "more files above");
    assert!(line(&buf, 23).ends_with("space resume   esc back   o open   ? keys"), "{}", line(&buf, 23));
    state.update(key(KeyCode::Esc));
    assert_eq!(state.focus, crate::state::Focus::List);
}

#[test]
fn a_typing_user_sees_that_f1_opens_the_keys() {
    let mut state = with_downloads((100, 30));
    press(&mut state, "/");
    assert!(line(&render(&state), 29).ends_with("enter search   ↓ list   f1 keys"), "{}", line(&render(&state), 29));
    press(&mut state, "night");
    state.update(key(KeyCode::F(1)));
    assert!(shot(&state).contains("     always │ ? f1 keys"), "{}", shot(&state));
}

#[test]
fn a_cut_name_keeps_its_end() {
    let screen = shot(&searched((80, 24)));
    assert!(screen.contains("Night of the…Dead (1968) [DVD]"), "{screen}");
    assert!(screen.contains("Night.of.the…1080p.BluRay.x264"), "{screen}");
}

#[test]
fn a_small_window_asks_to_grow() {
    let state = app((52, 14));
    assert!(shot(&state).contains("Make the window at least 60×16."));
}

fn render(state: &State) -> Buffer {
    render_in(state, ColorMode::TrueColor)
}

fn render_in(state: &State, mode: ColorMode) -> Buffer {
    let (w, h) = state.size;
    let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
    view::render(&mut buf, state, mode);
    buf
}

fn find(buf: &Buffer, text: &str) -> (u16, u16) {
    let w = buf.area.width;
    for y in 0..buf.area.height {
        let line: String = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
        if let Some(byte) = line.find(text) {
            return (line[..byte].chars().count() as u16, y);
        }
    }
    panic!("{text:?} is not on screen");
}

fn line(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string()
}

const MARK: u16 = crate::layout::MARK;

#[test]
fn the_selected_row_is_bright_and_bold_with_the_mark_beside_it() {
    let mut state = searched((100, 30));
    press(&mut state, "j");
    let buf = render(&state);
    let (x, y) = find(&buf, "Night of the Living Dead (1968) [DVD]");
    assert_eq!((buf[(x, y)].fg, buf[(x, y)].modifier.contains(Modifier::BOLD)), (theme::INK, true));
    assert_eq!((buf[(MARK, y)].symbol(), buf[(MARK, y)].fg), ("●", theme::ACTION));
    let (x, y) = find(&buf, "Night.of.the.Living.Dead.1968.1080p");
    assert_eq!((buf[(x, y)].fg, buf[(MARK, y)].symbol()), (theme::TEXT, "│"));
}

#[test]
fn the_mark_shows_where_keys_go() {
    let mut state = searched((100, 30));
    press(&mut state, "/");
    let buf = render(&state);
    let marks: Vec<u16> = (0..30).filter(|&y| buf[(MARK, y)].symbol() == "●").collect();
    assert_eq!(marks, [crate::layout::SEARCH], "typing: the mark is on the search line");
    let (x, y) = find(&buf, "Night of the Living Dead (1968)");
    assert_eq!(buf[(x, y)].fg, theme::TEXT, "no row is selected while typing");
}

#[test]
fn the_margin_labels_each_group_once() {
    let state = searched((100, 30));
    let buf = render(&state);
    let margins: Vec<String> =
        (0..30).map(|y| line(&buf, y).chars().take(11).collect::<String>().trim().to_string()).collect();
    let labels: Vec<&str> = margins.iter().map(String::as_str).filter(|m| !m.is_empty()).collect();
    assert_eq!(labels, ["torrential", "search", "getting", "failed", "paused", "done", "found"]);
    let (x, y) = find(&buf, "failed │");
    assert_eq!((x, buf[(x, y)].fg), (5, theme::ERROR));
}

#[test]
fn errors_and_warnings_take_the_error_ink() {
    let state = searched((100, 30));
    let buf = render(&state);
    let (x, y) = find(&buf, "apibay and nyaa failed");
    assert_eq!(buf[(x, y)].fg, theme::ERROR);
    let (x, y) = find(&buf, "cam ");
    assert_eq!(buf[(x, y)].fg, theme::ERROR);
    let (_, y) = find(&buf, "720p WEB");
    let zero = (0..100).rev().find(|&x| buf[(x, y)].symbol() == "0").unwrap();
    assert_eq!(buf[(zero, y)].fg, theme::ERROR, "no seeds is in the red");
}

#[test]
fn remove_asks_in_the_margin_and_says_the_files_stay() {
    let mut state = searched((100, 30));
    press(&mut state, "djjx");
    let buf = render(&state);
    let (x, y) = find(&buf, "remove?");
    assert_eq!((x, buf[(x, y)].fg), (4, theme::ERROR));
    assert!(line(&buf, y).contains("● stackexchange"), "{}", line(&buf, y));
    assert!(line(&buf, y + 1).contains("Its files stay in ~/Downloads/torrential."), "{}", line(&buf, y + 1));
    assert!(line(&buf, y + 1).ends_with("enter removes it   esc keeps it"));
    assert_eq!(line(&buf, 29), "", "the footer does not repeat the keys");
}

#[test]
fn a_result_in_downloads_offers_the_way_there() {
    let mut state = searched((100, 30));
    let job = torrential_core::Job { id: id("4c1e9a"), ..jobs()[0].clone() };
    let reply = GetReply { outcome: GetOutcome::Started, job, plan: None, previous_selection: None };
    state.update(Msg::Started { release: result_id("4c1e9a"), result: Ok(Box::new(reply)) });
    state.toast = None;
    let buf = render(&state);
    assert!(line(&buf, 29).ends_with("d downloads   s sort   ? keys"), "{}", line(&buf, 29));
    assert!(shot(&state).contains("in downloads · press d to see it"));
}

#[test]
fn without_color_the_mark_the_ticks_and_the_words_still_carry_meaning() {
    let mut state = searched((100, 30));
    press(&mut state, "j");
    state.update(key(KeyCode::Enter));
    state.update(Msg::Files { item: Item::Result(result_id("b7f20d")), result: Ok(dvd_files()) });
    press(&mut state, "jj ");
    let buf = render_in(&state, ColorMode::NoColor);
    assert!(buf.content.iter().all(|c| c.fg == Color::Reset && c.bg == Color::Reset), "no color at all");
    let (x, y) = find(&buf, "Night.asr.srt");
    assert_eq!(buf[(MARK, y)].symbol(), "●");
    assert!(buf[(x, y)].modifier.contains(Modifier::BOLD));
    assert!(line(&buf, y).contains("· Night.asr.srt"), "space left it out: {}", line(&buf, y));
    assert!(line(&buf, y + 1).contains("✓ Night.gif"), "{}", line(&buf, y + 1));
    let (x, y) = find(&buf, "best match first");
    assert!(buf[(x, y)].modifier.contains(Modifier::DIM), "pencil turns faint");
    let screen = shot(&state);
    assert!(screen.contains("1 executable left out"), "{screen}");
}

#[test]
fn low_disk_space_is_said_in_words() {
    let mut state = searched((100, 30));
    press(&mut state, "j");
    state.update(key(KeyCode::Enter));
    state.update(Msg::Files { item: Item::Result(result_id("b7f20d")), result: Ok(dvd_files()) });
    state.update(Msg::FreeSpace(Some(3 << 30)));
    let buf = render_in(&state, ColorMode::NoColor);
    assert!(line(&buf, 0).ends_with("↓ 10.0 MiB/s   low on space · 3.0 GiB free"), "{}", line(&buf, 0));
    let (_, y) = find(&buf, "11 of 12 files");
    assert!(line(&buf, y).contains("11 of 12 files · 398 MiB short of space"), "{}", line(&buf, y));
    state.update(Msg::FreeSpace(Some(40 << 30)));
    let buf = render_in(&state, ColorMode::NoColor);
    assert!(line(&buf, 0).ends_with("     ↓ 10.0 MiB/s"), "nothing follows the rate: {}", line(&buf, 0));
    assert!(line(&buf, y).contains("11 of 12 files · 40.0 GiB free"), "{}", line(&buf, y));
}

#[test]
fn an_unreachable_engine_says_so_with_its_hint() {
    let mut state = app((100, 30));
    let error = Error::new(ErrorCode::DaemonUnavailable, "the engine did not start within 10s")
        .with_hint("see ~/.local/state/torrential/engine.log");
    state.update(Msg::WatchLost(error));
    let screen = shot(&state);
    assert!(screen.contains("engine unreachable"), "{screen}");
    assert!(screen.contains("engine │ Cannot reach the engine."), "{screen}");
    assert!(screen.contains("see ~/.local/state/torrential/engine.log"), "{screen}");
    assert!(screen.contains("Cannot reach the engine: the engine did not start within 10s"), "{screen}");
}

#[test]
fn a_frame_drawn_before_the_resize_message_arrives_still_fits() {
    let mut state = searched((160, 45));
    press(&mut state, "d");
    state.update(Msg::Files { item: Item::Download(id("6f2c81")), result: Ok(wikipedia_files()) });
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
    view::render(&mut buf, &state, ColorMode::TrueColor);
    let mut tiny = Buffer::empty(Rect::new(0, 0, 61, 16));
    view::render(&mut tiny, &state, ColorMode::TrueColor);
    assert_eq!(find(&buf, "torrential"), (1, 0));
}
