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
fn help_screens() {
    snapshots("help", |size| {
        let mut state = searched(size);
        press(&mut state, "?");
        state
    });
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
