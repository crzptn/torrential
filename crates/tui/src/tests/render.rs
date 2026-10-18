//! Snapshots of whole screens at the three sizes the design is checked at, plus the colors
//! that carry meaning.

use crossterm::event::KeyCode;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

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
fn a_small_window_asks_to_grow() {
    let state = app((52, 14));
    assert!(shot(&state).contains("torrential needs 60×16. This one is 52×14."));
}

fn render(state: &State) -> Buffer {
    let (w, h) = state.size;
    let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
    view::render(&mut buf, state, ColorMode::TrueColor);
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

#[test]
fn colors_carry_focus_failure_and_provenance() {
    let mut state = searched((100, 30));
    press(&mut state, "j");
    let buf = render(&state);
    let (x, y) = find(&buf, "Night of the Living Dead (1968) [DVD]");
    assert_eq!((buf[(x, y)].fg, buf[(x, y)].bg), (theme::ACCENT, theme::SELECTION));
    assert_eq!(buf[(x - 8, y)].fg, theme::MINT, "IDs are mint");
    let (x, y) = find(&buf, "apibay: http 503");
    assert_eq!(buf[(x, y)].fg, theme::ERROR);
    let (x, y) = find(&buf, "cam");
    assert_eq!(buf[(x, y)].fg, theme::WARNING);
    let (x, y) = find(&buf, "0 seeds");
    assert_eq!(buf[(x, y)].fg, theme::ERROR);
}

#[test]
fn a_dialog_dims_the_screen_behind_it() {
    let mut state = searched((100, 30));
    let (x, y) = find(&render(&state), "torrential");
    assert_eq!(render(&state)[(x, y)].fg, theme::TEXT);
    press(&mut state, "dx");
    assert_eq!(render(&state)[(x, y)].fg, ratatui::style::Color::Rgb(0x69, 0x69, 0x69));
}

#[test]
fn an_unreachable_engine_says_so_with_its_hint() {
    let mut state = app((100, 30));
    let error = Error::new(ErrorCode::DaemonUnavailable, "the engine did not start within 10s")
        .with_hint("see ~/.local/state/torrential/engine.log");
    state.update(Msg::WatchLost(error));
    let screen = shot(&state);
    assert!(screen.contains("● engine unreachable"), "{screen}");
    assert!(screen.contains("✗ Cannot reach the engine."), "{screen}");
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
    assert_eq!(find(&buf, "torrential"), (2, 0));
}
