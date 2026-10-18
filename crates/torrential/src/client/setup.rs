//! `torrential setup`: three questions, asked once, whose answers search ranks with and
//! `get` takes its size cap from.

use std::io::{self, BufRead, IsTerminal, Write};

use serde::Serialize;
use torrential_core::preferences::QUESTIONS;
use torrential_core::units::size_text;
use torrential_core::{Error, ErrorCode, Preferences};

use crate::cli::{Limit, SetupArgs};
use crate::config;
use crate::env::Env;
use crate::output::{Mode, Typed, views};
use crate::paths::Paths;
use crate::skill::Writes;

#[derive(Serialize)]
struct Report<'a> {
    /// `saved`, `unchanged`, or `shown`.
    result: &'static str,
    /// `None` when nobody has answered.
    preferences: Option<&'a Preferences>,
    file: String,
}

/// Flags change only the answers they name. `--skip` and the questions replace them all.
pub fn run(args: SetupArgs, mode: Mode, env: &Env) -> Result<u8, Error> {
    let paths = Paths::from_process(env)?;
    let current = config::preferences(&paths)?;
    let flagged = args.screen.is_some() || args.max_size.is_some() || args.playback.is_some();
    let wanted = if args.skip {
        Some(Preferences::default())
    } else if flagged {
        let mut wanted = current.unwrap_or_default();
        if let Some(screen) = args.screen {
            wanted.screen = Some(screen.into());
        }
        if let Some(Limit(max)) = args.max_size {
            wanted.max_size = max;
        }
        if let Some(playback) = args.playback {
            wanted.playback = Some(playback.into());
        }
        Some(wanted)
    } else if mode == Mode::Human && io::stdin().is_terminal() {
        let answered = ask(&mut io::stdin().lock(), &mut io::stdout().lock())
            .map_err(|e| Error::internal(format!("cannot read the answers: {e}")))?;
        Some(answered.ok_or_else(|| {
            Error::new(ErrorCode::Usage, "setup stopped before the last answer, so nothing was saved")
                .with_hint("run torrential setup again")
        })?)
    } else {
        None
    };
    let (file, result) = match &wanted {
        None => (config::file(&paths)?, "shown"),
        Some(preferences) => {
            let writes = Writes::new(env, command(preferences));
            let (file, changed) = config::save_preferences(&paths, preferences, &writes)?;
            (file, if changed { "saved" } else { "unchanged" })
        }
    };
    let preferences = wanted.or(current);
    let doc = views::setup(preferences.as_ref(), &file, result);
    let report = Report { result, preferences: preferences.as_ref(), file: file.display().to_string() };
    Ok(super::emit(mode, &doc, &Typed { kind: "setup", body: report }))
}

/// The command that saves these answers, for a person to run where torrential can write.
fn command(preferences: &Preferences) -> String {
    if preferences.is_empty() {
        return "torrential setup --skip".to_string();
    }
    let mut words = vec!["torrential setup".to_string()];
    if let Some(screen) = preferences.screen {
        words.push(format!("--screen {}", screen.as_str()));
    }
    words.push(format!("--max-size {}", preferences.max_size.map_or_else(|| "none".to_string(), size_text)));
    if let Some(playback) = preferences.playback {
        words.push(format!("--playback {}", playback.as_str()));
    }
    words.join(" ")
}

/// Asks each question with numbered choices. `None` when the input ends first.
fn ask(input: &mut impl BufRead, out: &mut impl Write) -> io::Result<Option<Preferences>> {
    writeln!(out, "Three questions, asked once. Search ranks films and shows by the answers, and get stops at")?;
    writeln!(out, "the size limit unless you pass --max-size. Press Enter to skip a question.")?;
    let mut preferences = Preferences::default();
    for question in &QUESTIONS {
        writeln!(out, "\n{}", question.text)?;
        for (n, (label, _)) in question.choices.iter().enumerate() {
            writeln!(out, "  {}  {label}", n + 1)?;
        }
        let last = question.choices.len();
        loop {
            write!(out, "Answer 1-{last}: ")?;
            out.flush()?;
            let mut line = String::new();
            if input.read_line(&mut line)? == 0 {
                writeln!(out)?;
                return Ok(None);
            }
            let line = line.trim();
            if line.is_empty() {
                break;
            }
            let choice =
                line.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).and_then(|i| question.choices.get(i));
            match choice {
                Some((_, answer)) => {
                    preferences.answer(*answer);
                    break;
                }
                None => writeln!(out, "Type a number from 1 to {last}, or press Enter to skip.")?,
            }
        }
    }
    writeln!(out)?;
    Ok(Some(preferences))
}

#[cfg(test)]
mod tests {
    use super::*;
    use torrential_core::{Playback, Screen};

    fn answers(typed: &str) -> (Option<Preferences>, String) {
        let mut out = Vec::new();
        let answered = ask(&mut typed.as_bytes(), &mut out).unwrap();
        (answered, String::from_utf8(out).unwrap())
    }

    #[test]
    fn numbered_answers_fill_in_each_question() {
        let (answered, out) = answers("4\n2\n3\n");
        assert_eq!(
            answered,
            Some(Preferences {
                screen: Some(Screen::Tv4k),
                max_size: Some(8_000_000_000),
                playback: Some(Playback::H264)
            })
        );
        assert!(out.contains("\nWhat do you usually watch on?\n  1  Phone or tablet\n"), "{out}");
        assert!(out.contains("\nCan your player handle HEVC (x265) and HDR?\n"), "{out}");
    }

    #[test]
    fn a_bad_answer_asks_again_and_enter_skips() {
        let (answered, out) = answers("9\nlaptop\n2\n\n1\n");
        assert_eq!(
            answered,
            Some(Preferences { screen: Some(Screen::Laptop), max_size: None, playback: Some(Playback::HevcHdr) })
        );
        assert_eq!(out.matches("Type a number from 1 to 4, or press Enter to skip.").count(), 2, "{out}");
    }

    #[test]
    fn input_that_ends_early_saves_nothing() {
        assert_eq!(answers("1\n").0, None);
    }

    #[test]
    fn the_rerun_command_saves_the_same_answers() {
        let laptop =
            Preferences { screen: Some(Screen::Laptop), max_size: Some(2_000_000_000), playback: Some(Playback::H264) };
        let unlimited = Preferences { playback: Some(Playback::Hevc), ..Preferences::default() };
        assert_eq!(command(&laptop), "torrential setup --screen laptop --max-size 2GB --playback h264");
        assert_eq!(command(&unlimited), "torrential setup --max-size none --playback hevc");
        assert_eq!(command(&Preferences::default()), "torrential setup --skip");
    }
}
