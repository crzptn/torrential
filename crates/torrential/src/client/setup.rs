//! `torrential setup`: shows what search ranks for on this machine, and saves preferences
//! that override what torrential detected, for someone who watches somewhere else.

use serde::Serialize;
use torrential_core::units::size_text;
use torrential_core::{Error, Preferences, Profile};

use crate::cli::{Limit, PlaybackArg, ScreenArg, SetupArgs};
use crate::config;
use crate::env::Env;
use crate::output::{Mode, Typed, views};
use crate::paths::Paths;
use crate::skill::Writes;

#[derive(Serialize)]
struct Report<'a> {
    /// `saved`, `unchanged`, or `shown`.
    result: &'static str,
    profile: &'a Profile,
    file: String,
}

/// Each flag changes only the preference it names.
pub fn run(args: SetupArgs, mode: Mode, env: &Env) -> Result<u8, Error> {
    let paths = Paths::from_process(env)?;
    let current = config::preferences(&paths)?;
    let wanted = Preferences {
        screen: args.screen.map_or(current.screen, ScreenArg::screen),
        max_size: args.max_size.map_or(current.max_size, |Limit(max)| max),
        playback: args.playback.map_or(current.playback, PlaybackArg::playback),
    };
    let flagged = args.screen.is_some() || args.max_size.is_some() || args.playback.is_some();
    let (file, result) = if flagged {
        let writes = Writes::new(env, command(&wanted));
        let (file, changed) = config::save_preferences(&paths, &wanted, &writes)?;
        (file, if changed { "saved" } else { "unchanged" })
    } else {
        (config::file(&paths)?, "shown")
    };
    let profile = Profile { detected: Some(crate::machine::detect(env)?), preferences: wanted, room: None };
    let doc = views::setup(&profile, &file, result);
    let report = Report { result, profile: &profile, file: file.display().to_string() };
    Ok(super::emit(mode, &doc, &Typed { kind: "setup", body: report }))
}

/// The command that saves these preferences, for a person to run where torrential can write.
fn command(preferences: &Preferences) -> String {
    format!(
        "torrential setup --screen {} --max-size {} --playback {}",
        preferences.screen.map_or("auto", |s| s.as_str()),
        preferences.max_size.map_or_else(|| "none".to_string(), size_text),
        preferences.playback.map_or("auto", |p| p.as_str()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use torrential_core::{Playback, Screen};

    #[test]
    fn the_rerun_command_names_each_preference_or_what_clears_it() {
        let laptop =
            Preferences { screen: Some(Screen::Laptop), max_size: Some(2_000_000_000), playback: Some(Playback::H264) };
        assert_eq!(command(&laptop), "torrential setup --screen laptop --max-size 2GB --playback h264");
        assert_eq!(command(&Preferences::default()), "torrential setup --screen auto --max-size none --playback auto");
    }
}
