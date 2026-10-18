//! The command definition. Help, completions, and `torrential schema` all come from it.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use clap_complete::engine::ArgValueCandidates;
use torrential_core::{Category, Sort};

use crate::completions::complete_download_ids;

#[derive(Parser, Debug)]
#[command(
    name = "torrential",
    version,
    about = "Search torrent indexes and download torrents",
    disable_help_subcommand = true,
    after_help = "Examples:\n  torrential search arch linux iso\n  torrential get 9b5212 --dir ~/isos\n  torrential wait 9b5212"
)]
pub struct Cli {
    /// Print JSON on stdout instead of text
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Search the enabled sources
    #[command(
        after_help = "Examples:\n  torrential search ubuntu 24.04 desktop --limit 5\n  torrential search night of the living dead --source archive.org"
    )]
    Search(SearchArgs),
    /// List the files in a torrent, fetching its metadata if needed
    Files(FilesArgs),
    /// Start a download, or change its file selection
    #[command(
        after_help = "Examples:\n  torrential get 9b5212\n  torrential get 'magnet:?xt=urn:btih:…' --dir ~/isos\n  torrential get 6bc1af --files '*.mp4' --dry-run"
    )]
    Get(GetArgs),
    /// List downloads, or show one in detail
    Status(StatusArgs),
    /// Follow downloads until they finish or the timeout passes
    #[command(
        after_help = "Examples:\n  torrential wait 9b5212\n  torrential wait 9b5212 --timeout 110m\n  torrential wait 9b5212 --until metadata"
    )]
    Wait(WaitArgs),
    /// Pause downloads, returning once writes stop
    Pause(RefsArgs),
    /// Resume downloads, or retry failed ones
    Resume(ResumeArgs),
    /// Stop tracking downloads and keep their files
    Rm(RefsArgs),
    /// Re-hash the data on disk and report damaged pieces
    Verify(VerifyArgs),
    /// Diagnose sandbox, disk, state, engine, config, and sources
    Doctor(DoctorArgs),
    /// Open the terminal UI, which `torrential` alone also opens on a terminal
    Tui,
    /// Inspect or manage the engine process
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Print every command, flag, exit code, and error code as JSON
    Schema,
    /// Print shell completion setup
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Words to search for
    #[arg(value_name = "WORDS", required = true, num_args = 1..)]
    pub words: Vec<String>,
    /// Search only this source. Repeat to add more
    #[arg(long, value_name = "NAME")]
    pub source: Vec<String>,
    /// Show only this kind of content
    #[arg(long, value_enum)]
    pub category: Option<CategoryArg>,
    /// Hide results with fewer reported seeders, and results with unknown seeders
    #[arg(long, value_name = "N")]
    pub min_seeds: Option<u32>,
    /// Hide results larger than this, for example 8GiB, and results with unknown size
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub max_size: Option<u64>,
    /// Order of the results. Relevance comes before popularity
    #[arg(long, value_enum, default_value_t = SortArg::Relevance)]
    pub sort: SortArg,
    /// Number of results to show
    #[arg(long, value_name = "N", default_value_t = 10, value_parser = clap::value_parser!(u16).range(1..))]
    pub limit: u16,
    /// Stop waiting for slow sources after this long
    #[arg(long, value_name = "DURATION", default_value = "6s", value_parser = parse_duration)]
    pub timeout: Duration,
    /// Include adult results
    #[arg(long)]
    pub adult: bool,
    /// Skip the result cache
    #[arg(long)]
    pub fresh: bool,
}

#[derive(Args, Debug)]
pub struct DoctorArgs {
    /// Also search each enabled source once, and report how it answered and how long it took
    #[arg(long)]
    pub network: bool,
}

#[derive(Args, Debug)]
pub struct FilesArgs {
    /// An ID prefix, infohash, magnet link, .torrent file, or .torrent URL
    #[arg(value_name = "REF", add = ArgValueCandidates::new(complete_download_ids))]
    pub reference: String,
}

#[derive(Args, Debug)]
pub struct GetArgs {
    /// An ID prefix, infohash, magnet link, .torrent file, or .torrent URL
    #[arg(value_name = "REF", add = ArgValueCandidates::new(complete_download_ids))]
    pub reference: String,
    /// Download only files matching this glob or exact path. `*` crosses folders and case is ignored. Repeat to add more
    #[arg(long, value_name = "GLOB")]
    pub files: Vec<String>,
    /// The folder the download goes into
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
    /// The largest download allowed, for example 120GiB. Without it the cap is 50 GiB
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub max_size: Option<u64>,
    /// Print the selected size, destination, free space, and conflicts without downloading
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Download IDs from torrential status. None lists every download
    #[arg(value_name = "ID", add = ArgValueCandidates::new(complete_download_ids))]
    pub references: Vec<String>,
}

#[derive(Args, Debug)]
pub struct WaitArgs {
    /// Download IDs from torrential status
    #[arg(value_name = "ID", required = true, num_args = 1.., add = ArgValueCandidates::new(complete_download_ids))]
    pub references: Vec<String>,
    /// Stop when the download is done, or as soon as its file list is known
    #[arg(long, value_enum, default_value_t = Until::Done)]
    pub until: Until,
    /// Give up after this long. Defaults to 100s, or no limit when a person watches in a terminal
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    pub timeout: Option<Duration>,
}

#[derive(Args, Debug)]
pub struct RefsArgs {
    /// Download IDs from torrential status
    #[arg(value_name = "ID", required = true, num_args = 1.., add = ArgValueCandidates::new(complete_download_ids))]
    pub references: Vec<String>,
}

#[derive(Args, Debug)]
pub struct ResumeArgs {
    /// Download IDs from torrential status
    #[arg(value_name = "ID", required_unless_present = "all", add = ArgValueCandidates::new(complete_download_ids))]
    pub references: Vec<String>,
    /// Resume every paused, interrupted, or failed download
    #[arg(long, conflicts_with = "references")]
    pub all: bool,
}

#[derive(Args, Debug)]
pub struct VerifyArgs {
    /// A download ID from torrential status
    #[arg(value_name = "ID", add = ArgValueCandidates::new(complete_download_ids))]
    pub reference: String,
    /// Download damaged pieces again
    #[arg(long)]
    pub repair: bool,
}

#[derive(Subcommand, Debug)]
pub enum DaemonCommand {
    /// Show whether the engine is running
    Status,
    /// Ask the engine to save its state and exit
    Stop,
    /// Start the engine at login with launchd or systemd
    Install,
    /// Remove the login service
    Uninstall,
    /// Run the engine in the foreground
    #[command(hide = true)]
    Run {
        /// Started by launchd or systemd: resume downloads at startup and never exit when idle
        #[arg(long)]
        service: bool,
    },
    /// The short-lived middle process of the detached start
    #[command(hide = true)]
    Launch,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum Until {
    Done,
    Metadata,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Elvish,
    Powershell,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum CategoryArg {
    Video,
    Audio,
    Software,
    Book,
    Game,
    Dataset,
    Other,
}

impl From<CategoryArg> for Category {
    fn from(arg: CategoryArg) -> Self {
        match arg {
            CategoryArg::Video => Category::Video,
            CategoryArg::Audio => Category::Audio,
            CategoryArg::Software => Category::Software,
            CategoryArg::Book => Category::Book,
            CategoryArg::Game => Category::Game,
            CategoryArg::Dataset => Category::Dataset,
            CategoryArg::Other => Category::Other,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum SortArg {
    Relevance,
    Seeds,
    Size,
    Age,
}

impl From<SortArg> for Sort {
    fn from(arg: SortArg) -> Self {
        match arg {
            SortArg::Relevance => Sort::Relevance,
            SortArg::Seeds => Sort::Seeds,
            SortArg::Size => Sort::Size,
            SortArg::Age => Sort::Age,
        }
    }
}

fn parse_size(s: &str) -> Result<u64, String> {
    torrential_core::units::parse_size(s).ok_or_else(|| format!("{s:?} is not a size; use a form like 8GiB or 700MB"))
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    torrential_core::units::parse_duration(s)
        .ok_or_else(|| format!("{s:?} is not a duration; use a form like 100s, 5m, or 1h30m"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn get_parses_repeated_globs_and_sizes() {
        let cli = Cli::try_parse_from([
            "torrential",
            "get",
            "6bc1af",
            "--files",
            "*.mp4",
            "--files",
            "*.srt",
            "--max-size",
            "60GiB",
        ])
        .unwrap();
        let Some(Command::Get(args)) = cli.command else { panic!("expected get") };
        assert_eq!(args.files, ["*.mp4", "*.srt"]);
        assert_eq!(args.max_size, Some(60 * 1024 * 1024 * 1024));
    }

    #[test]
    fn json_is_accepted_after_the_subcommand() {
        let cli = Cli::try_parse_from(["torrential", "status", "--json"]).unwrap();
        assert!(cli.json);
    }

    #[test]
    fn bad_durations_name_the_expected_form() {
        let err = Cli::try_parse_from(["torrential", "wait", "9b5212", "--timeout", "soon"]).unwrap_err();
        assert!(err.to_string().contains("use a form like 100s"), "{err}");
    }
}
