//! `config.yaml` in the config folder: where downloads go, which sources search
//! asks, and the preferences `torrential setup` saves to override what torrential
//! detects about the machine. The engine reads it when it starts, and clients read the
//! preferences before each search and download. A file that does not parse is an error
//! that names the line, never a silent fallback to the defaults.
//!
//! ```yaml
//! downloads:
//!   dir: ~/Downloads/torrents
//! sources:
//!   disabled: [nyaa]
//!   torznab:
//!     - name: jackett
//!       url: http://127.0.0.1:9117/api/v2.0/indexers/all/results/torznab/api
//!       api_key_env: JACKETT_API_KEY
//! preferences:
//!   screen: tv-4k
//!   max_size: 8GB
//!   playback: hevc-hdr
//! ```

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_saphyr::{Localizer, Location, SnippetMode, Spanned, UserMessageFormatter};
use torrential_core::units::{parse_size, size_text};
use torrential_core::{Error, ErrorCode, Playback, Preferences, Screen};
use torrential_search::{SearchConfig, TorznabConfig};

use crate::env::Env;
use crate::paths::Paths;
use crate::skill::{Writes, write_atomic};

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Config {
    /// Where `get` puts downloads without `--dir`. `None` keeps the default.
    pub downloads: Option<PathBuf>,
    /// The sources, with Torznab API keys read from the environment, and no cache folder.
    pub search: SearchConfig,
    /// The file these settings came from. `None` when there is none and the defaults apply.
    pub file: Option<PathBuf>,
    pub preferences: Preferences,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct File {
    downloads: Downloads,
    sources: Sources,
    preferences: Option<Overrides>,
}

/// The file as a client reads it. The other sections are the engine's to check, and can
/// name variables that only the engine's environment sets.
#[derive(Deserialize, Default)]
#[serde(default)]
struct PreferencesOnly {
    preferences: Option<Overrides>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Overrides {
    screen: Option<Screen>,
    /// A size such as `8GB`, or `none`.
    max_size: Option<Spanned<String>>,
    playback: Option<Playback>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Downloads {
    dir: Option<Spanned<String>>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Sources {
    disabled: Vec<String>,
    torznab: Vec<Torznab>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Torznab {
    name: String,
    url: String,
    /// The variable holding the API key, so the key stays out of the file.
    api_key_env: Option<Spanned<String>>,
}

/// A problem in the file, on a line when one is known.
type Problem = (Option<u64>, String);

/// Where the config file goes. `config.yml` is read when it is the only one.
pub fn path(paths: &Paths) -> PathBuf {
    paths.config.join("config.yaml")
}

impl Config {
    /// The defaults when there is no file.
    pub fn load(paths: &Paths, env: &Env) -> Result<Config, Error> {
        let mut config = match read(paths)? {
            None => Config::default(),
            Some((path, text)) => {
                let config = parse(&text, env).map_err(|(line, message)| invalid(&path, line, &message))?;
                Config { file: Some(path), ..config }
            }
        };
        config.search.endpoints = test_endpoints(env)?;
        Ok(config)
    }

    pub fn downloads_dir(&self, paths: &Paths) -> PathBuf {
        self.downloads.clone().unwrap_or_else(|| paths.downloads.clone())
    }
}

/// The file `setup` writes: the one that exists, or `config.yaml`.
pub fn file(paths: &Paths) -> Result<PathBuf, Error> {
    Ok(read(paths)?.map_or_else(|| path(paths), |(path, _)| path))
}

/// The preferences `torrential setup` saved, read fresh, since the engine reads the file
/// only when it starts.
pub fn preferences(paths: &Paths) -> Result<Preferences, Error> {
    let Some((path, text)) = read(paths)? else { return Ok(Preferences::default()) };
    read_preferences(&text).map_err(|(line, message)| invalid(&path, line, &message))
}

/// Writes `preferences` as the file's top-level `preferences` block, or removes the block
/// when they are empty. Every other line, comments included, stays as it was. Returns the
/// file and whether it changed.
pub fn save_preferences(paths: &Paths, preferences: &Preferences, writes: &Writes) -> Result<(PathBuf, bool), Error> {
    let (path, text) = read(paths)?.unwrap_or_else(|| (path(paths), String::new()));
    let edited = if preferences.is_empty() { without_block(&text) } else { with_block(&text, &block(preferences)) };
    if read_preferences(&edited).ok() != Some(*preferences) {
        return Err(invalid(&path, None, "setup cannot rewrite the preferences block in this file"));
    }
    if edited == text {
        return Ok((path, false));
    }
    // Writing through a link keeps a config that lives elsewhere, such as in a dotfiles repo.
    let target = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(|e| writes.error(dir, e))?;
    }
    write_atomic(&target, edited.as_bytes()).map_err(|e| writes.error(&target, e))?;
    Ok((path, true))
}

fn read_preferences(text: &str) -> Result<Preferences, Problem> {
    let file: PreferencesOnly = serde_saphyr::from_str(text).map_err(|e| problem(&e))?;
    file.preferences.unwrap_or_default().read()
}

impl Overrides {
    fn read(self) -> Result<Preferences, Problem> {
        let max_size = match self.max_size {
            Some(Spanned { value, referenced, .. }) if value.trim() != "none" => {
                Some(parse_size(&value).ok_or_else(|| {
                    let message = format!("preferences.max_size {value:?} is not a size; use a form like 8GB, or none");
                    (Some(referenced.line()), message)
                })?)
            }
            _ => None,
        };
        Ok(Preferences { screen: self.screen, max_size, playback: self.playback })
    }
}

/// The block `setup` writes, with only the preferences that are set.
fn block(preferences: &Preferences) -> String {
    let lines: Vec<String> = [
        preferences.screen.map(|s| format!("  screen: {}", s.as_str())),
        preferences.max_size.map(|max| format!("  max_size: {}", size_text(max))),
        preferences.playback.map(|p| format!("  playback: {}", p.as_str())),
    ]
    .into_iter()
    .flatten()
    .collect();
    format!("preferences:\n{}\n", lines.join("\n"))
}

/// Where the top-level `preferences` block starts and ends in `lines`: its key line and the
/// indented and blank lines after it, less trailing blank lines, so a comment at the left
/// margin stays with what follows it.
fn block_lines(lines: &[&str]) -> Option<(usize, usize)> {
    let is_key = |line: &&str| line.strip_prefix("preferences").is_some_and(|rest| rest.starts_with(':'));
    let start = lines.iter().position(is_key)?;
    let mut end = start + 1;
    while end < lines.len() && (lines[end].starts_with([' ', '\t']) || lines[end].trim().is_empty()) {
        end += 1;
    }
    while end > start + 1 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    Some((start, end))
}

/// `text` without its `preferences` block, and without the blank line that set it apart.
fn without_block(text: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let Some((start, mut end)) = block_lines(&lines) else { return text.to_string() };
    while end < lines.len() && lines[end].trim().is_empty() {
        end += 1;
    }
    let mut before = lines[..start].concat();
    if end == lines.len() {
        before.truncate(before.trim_end().len());
        if !before.is_empty() {
            before.push('\n');
        }
    }
    before + &lines[end..].concat()
}

/// `text` with `block` in place of its top-level `preferences` block, or after its last line.
fn with_block(text: &str, block: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let Some((start, end)) = block_lines(&lines) else {
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.trim().is_empty() && !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str(block);
        return out;
    };
    [lines[..start].concat(), block.to_string(), lines[end..].concat()].concat()
}

/// The file and its text. Both names at once is an error, since either could be the one the
/// user edits.
fn read(paths: &Paths) -> Result<Option<(PathBuf, String)>, Error> {
    let try_read = |path: PathBuf| match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some((path, text))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(invalid(&path, None, &format!("cannot be read: {e}"))),
    };
    match (try_read(path(paths))?, try_read(paths.config.join("config.yml"))?) {
        (Some((yaml, _)), Some((yml, _))) => Err(Error::new(
            ErrorCode::Usage,
            format!("{} and {} both exist, so torrential cannot tell which to read", yaml.display(), yml.display()),
        )
        .with_hint("move what you want to keep into one file and delete the other")),
        (found, None) | (None, found) => Ok(found),
    }
}

fn problem(error: &serde_saphyr::Error) -> Problem {
    (error.location().map(|at| at.line()), describe(error))
}

fn parse(text: &str, env: &Env) -> Result<Config, Problem> {
    let file: File = serde_saphyr::from_str(text).map_err(|e| problem(&e))?;
    let downloads = file
        .downloads
        .dir
        .map(|Spanned { value: raw, referenced, .. }| {
            let expanded = match (raw.strip_prefix('~'), env.var("HOME")) {
                (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
                    PathBuf::from(format!("{home}{rest}"))
                }
                _ => PathBuf::from(&raw),
            };
            if expanded.is_absolute() {
                Ok(expanded)
            } else {
                Err((
                    Some(referenced.line()),
                    format!("downloads.dir {raw:?} must be an absolute path or start with ~/"),
                ))
            }
        })
        .transpose()?;
    let torznab = file
        .sources
        .torznab
        .into_iter()
        .map(|Torznab { name, url, api_key_env }| {
            let api_key = match api_key_env {
                None => None,
                Some(Spanned { value: var, referenced, .. }) => match env.var(&var) {
                    Some(key) => Some(key.to_string()),
                    None => {
                        return Err((
                            Some(referenced.line()),
                            format!("the {name:?} indexer takes its API key from {var}, which is not set"),
                        ));
                    }
                },
            };
            Ok(TorznabConfig { name, url, api_key })
        })
        .collect::<Result<_, _>>()?;
    let search = SearchConfig { torznab, disabled: file.sources.disabled, ..SearchConfig::default() };
    let preferences = file.preferences.unwrap_or_default().read()?;
    Ok(Config { downloads, search, file: None, preferences })
}

/// The message without the line, which `invalid` adds, and without the excerpt of the file.
fn describe(error: &serde_saphyr::Error) -> String {
    struct NoLocation;
    impl Localizer for NoLocation {
        fn attach_location<'a>(&self, base: Cow<'a, str>, _: Location) -> Cow<'a, str> {
            base
        }
    }
    let formatter = UserMessageFormatter.with_localizer(&NoLocation);
    error.render_with_options(serde_saphyr::render_options! { formatter: &formatter, snippets: SnippetMode::Off })
}

fn invalid(path: &Path, line: Option<u64>, message: &str) -> Error {
    let place = match line {
        Some(line) => format!("{} line {line}", path.display()),
        None => path.display().to_string(),
    };
    Error::new(ErrorCode::Usage, format!("{place}: {message}")).with_hint(format!("fix {}", path.display()))
}

/// `TORRENTIAL_TEST_ENDPOINTS=knaben=http://127.0.0.1:8080/knaben/,…` points built-in sources at a
/// local server, for tests.
fn test_endpoints(env: &Env) -> Result<BTreeMap<String, String>, Error> {
    let Some(list) = env.var("TORRENTIAL_TEST_ENDPOINTS") else { return Ok(BTreeMap::new()) };
    list.split(',')
        .map(|pair| {
            pair.split_once('=').map(|(name, url)| (name.trim().to_string(), url.trim().to_string())).ok_or_else(|| {
                Error::new(ErrorCode::Usage, format!("TORRENTIAL_TEST_ENDPOINTS has {pair:?}, which is not name=url"))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Env {
        Env::from_pairs([("HOME", "/Users/ada"), ("JACKETT_KEY", "k3y")])
    }

    fn load(text: &str) -> Result<Config, Error> {
        load_files(&[("config.yaml", text)])
    }

    /// Loads with each named file in the config folder, which reads as `$TORRENTIAL_HOME/config`.
    fn load_files(files: &[(&str, &str)]) -> Result<Config, Error> {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().display().to_string();
        let env = Env::from_pairs([("HOME", "/Users/ada"), ("TORRENTIAL_HOME", home.as_str())]);
        let paths = Paths::resolve(&env, 501, None).unwrap();
        std::fs::create_dir_all(&paths.config).unwrap();
        for (name, text) in files {
            std::fs::write(paths.config.join(name), text).unwrap();
        }
        let relative = |text: String| text.replace(&home, "$TORRENTIAL_HOME");
        Config::load(&paths, &env)
            .map(|config| Config { file: config.file.map(|f| relative(f.display().to_string()).into()), ..config })
            .map_err(|e| Error { message: relative(e.message), ..e })
    }

    #[test]
    fn reads_downloads_disabled_sources_and_indexers() {
        let text = "
downloads:
  dir: ~/isos
sources:
  disabled: [nyaa, apibay]
  torznab:
    - name: jackett
      url: http://127.0.0.1:9117/api
      api_key_env: JACKETT_KEY
";
        let config = parse(text, &env()).unwrap();
        assert_eq!(config.downloads, Some(PathBuf::from("/Users/ada/isos")));
        assert_eq!(config.search.disabled, ["nyaa", "apibay"]);
        assert_eq!(
            config.search.torznab,
            [TorznabConfig {
                name: "jackett".into(),
                url: "http://127.0.0.1:9117/api".into(),
                api_key: Some("k3y".into())
            }]
        );
        assert_eq!(parse("", &env()).unwrap(), Config::default());
        assert_eq!(parse("# nothing yet\n", &env()).unwrap(), Config::default());
    }

    #[test]
    fn reads_config_yml_only_when_config_yaml_is_absent() {
        let text = "sources:\n  disabled: [nyaa]\n";
        let yaml = load_files(&[("config.yaml", text)]).unwrap();
        let yml = load_files(&[("config.yml", text)]).unwrap();
        let neither = load_files(&[]).unwrap();
        let both = load_files(&[("config.yaml", text), ("config.yml", text)]).unwrap_err();

        assert_eq!(
            [yaml, yml].map(|c| (c.file.unwrap().display().to_string(), c.search.disabled)),
            [
                ("$TORRENTIAL_HOME/config/config.yaml".to_string(), vec!["nyaa".to_string()]),
                ("$TORRENTIAL_HOME/config/config.yml".to_string(), vec!["nyaa".to_string()]),
            ]
        );
        assert_eq!(neither, Config::default());
        assert_eq!(
            (both.code, both.message.as_str(), both.hint.as_deref()),
            (
                ErrorCode::Usage,
                "$TORRENTIAL_HOME/config/config.yaml and $TORRENTIAL_HOME/config/config.yml both exist, so torrential \
                 cannot tell which to read",
                Some("move what you want to keep into one file and delete the other")
            )
        );
    }

    #[test]
    fn reads_preferences_and_an_absent_or_empty_block_as_none() {
        let all = "preferences:\n  screen: tv-4k\n  max_size: 8GB\n  playback: hevc-hdr\n";
        let read = |text: &str| parse(text, &env()).unwrap().preferences;
        assert_eq!(
            read(all),
            Preferences {
                screen: Some(Screen::Tv4k),
                max_size: Some(8_000_000_000),
                playback: Some(Playback::HevcHdr)
            }
        );
        for none in ["preferences: {}\n", "preferences:\n", "preferences:\n  max_size: none\n", "sources: {}\n"] {
            assert_eq!(read(none), Preferences::default(), "{none}");
        }
    }

    #[test]
    fn bad_preferences_name_the_line() {
        let cases = [
            (
                "preferences:\n  screen: tv4k\n",
                "line 2: unknown variant `tv4k`, expected one of phone, laptop, tv, tv-4k",
            ),
            (
                "preferences:\n  max_size: lots\n",
                "line 2: preferences.max_size \"lots\" is not a size; use a form like 8GB, or none",
            ),
            (
                "preferences:\n  speakers: 5.1\n",
                "line 2: unknown field `speakers`, expected one of screen, max_size, playback",
            ),
        ];
        for (text, expected) in cases {
            let error = load(text).unwrap_err();
            assert_eq!(error.message, format!("$TORRENTIAL_HOME/config/config.yaml {expected}"), "{text}");
        }
    }

    /// A config folder under a temporary `TORRENTIAL_HOME`.
    struct Folder {
        _dir: tempfile::TempDir,
        paths: Paths,
    }

    impl Folder {
        fn new() -> Folder {
            let dir = tempfile::tempdir().unwrap();
            let env = Env::from_pairs([("HOME", "/Users/ada"), ("TORRENTIAL_HOME", dir.path().to_str().unwrap())]);
            let paths = Paths::resolve(&env, 501, None).unwrap();
            Folder { _dir: dir, paths }
        }

        fn write(&self, text: &str) {
            std::fs::create_dir_all(&self.paths.config).unwrap();
            std::fs::write(path(&self.paths), text).unwrap();
        }

        fn text(&self) -> String {
            std::fs::read_to_string(path(&self.paths)).unwrap()
        }

        fn save(&self, preferences: Preferences) -> bool {
            let writes = Writes { sandboxed: false, rerun: "torrential setup".into() };
            save_preferences(&self.paths, &preferences, &writes).unwrap().1
        }
    }

    const LAPTOP: Preferences =
        Preferences { screen: Some(Screen::Laptop), max_size: Some(2_000_000_000), playback: Some(Playback::H264) };

    #[test]
    fn setup_replaces_only_the_preferences_block_and_keeps_comments() {
        let folder = Folder::new();
        folder.write(
            "# where films go\ndownloads:\n  dir: ~/films # big disk\n\npreferences:\n  # old answers\n  screen: tv\n  \
             max_size: 20GB\n\n# indexers\nsources:\n  disabled: [nyaa]\n",
        );

        let changed = folder.save(LAPTOP);

        assert!(changed);
        assert_eq!(
            folder.text(),
            "# where films go\ndownloads:\n  dir: ~/films # big disk\n\npreferences:\n  screen: laptop\n  \
             max_size: 2GB\n  playback: h264\n\n# indexers\nsources:\n  disabled: [nyaa]\n"
        );
        assert_eq!(preferences(&folder.paths).unwrap(), LAPTOP);
        assert!(!folder.save(LAPTOP), "saving the same preferences again changes nothing");
    }

    #[test]
    fn setup_adds_the_block_after_the_last_line_or_creates_the_file() {
        let existing = Folder::new();
        existing.write("sources:\n  disabled: [nyaa] # too slow");
        let missing = Folder::new();

        existing.save(LAPTOP);
        missing.save(LAPTOP);

        let block = "preferences:\n  screen: laptop\n  max_size: 2GB\n  playback: h264\n";
        assert_eq!(existing.text(), format!("sources:\n  disabled: [nyaa] # too slow\n\n{block}"));
        assert_eq!(missing.text(), block);
    }

    #[test]
    fn clearing_every_preference_removes_the_block_and_its_blank_line() {
        let cases = [
            ("preferences: {screen: tv}\ndownloads:\n  dir: /films\n", "downloads:\n  dir: /films\n"),
            (
                "downloads:\n  dir: /films\n\npreferences:\n  screen: tv\n\n# indexers\nsources: {}\n",
                "downloads:\n  dir: /films\n\n# indexers\nsources: {}\n",
            ),
            ("downloads:\n  dir: /films\n\npreferences:\n  screen: tv\n", "downloads:\n  dir: /films\n"),
            ("preferences:\n  screen: tv\n", ""),
        ];
        for (text, cleared) in cases {
            let folder = Folder::new();
            folder.write(text);
            assert!(folder.save(Preferences::default()), "{text}");
            assert_eq!(folder.text(), cleared, "{text}");
            assert_eq!(preferences(&folder.paths).unwrap(), Preferences::default());
        }
        let missing = Folder::new();
        assert!(!missing.save(Preferences::default()), "clearing a config that does not exist writes nothing");
    }

    #[test]
    fn setup_writes_through_a_linked_config() {
        let folder = Folder::new();
        let dotfiles = folder.paths.config.parent().unwrap().join("dotfiles.yaml");
        std::fs::create_dir_all(&folder.paths.config).unwrap();
        std::fs::write(&dotfiles, "sources:\n  disabled: [nyaa]\n").unwrap();
        std::os::unix::fs::symlink(&dotfiles, path(&folder.paths)).unwrap();

        folder.save(LAPTOP);

        assert!(path(&folder.paths).symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(&dotfiles).unwrap(),
            "sources:\n  disabled: [nyaa]\n\npreferences:\n  screen: laptop\n  max_size: 2GB\n  playback: h264\n"
        );
    }

    #[test]
    fn names_that_yaml_1_1_reads_as_booleans_stay_strings() {
        let text = "sources:\n  disabled: [no, off, yes]\n  torznab:\n    - name: no\n      url: http://x/api\n";
        let config = parse(text, &env()).unwrap();
        assert_eq!(config.search.disabled, ["no", "off", "yes"]);
        assert_eq!(config.search.torznab[0].name, "no");
    }

    #[test]
    fn a_bad_file_is_an_error_that_names_the_line() {
        let indexer = "sources:\n  torznab:\n    - name: j\n      url: http://x/api\n";
        let cases = [
            (
                "downloads:\n  dir: /tmp/x\nsources:\n  disable: [nyaa]\n".to_string(),
                "line 4: unknown field `disable`, expected one of disabled, torznab",
            ),
            ("sources:\n  disabled: [nyaa, apibay\n".to_string(), "line 2: unclosed bracket '['"),
            (
                "sources:\n  disabled: [nyaa]\n  disabled: [apibay]\n".to_string(),
                "line 3: duplicate mapping key: disabled not allowed here",
            ),
            ("sources:\n  torznab: 3\n".to_string(), "line 2: expected sequence start"),
            (
                "downloads:\n  dir: isos\n".to_string(),
                "line 2: downloads.dir \"isos\" must be an absolute path or start with ~/",
            ),
            (
                format!("{indexer}      api_key_env: NOPE\n"),
                "line 5: the \"j\" indexer takes its API key from NOPE, which is not set",
            ),
            (
                format!("{indexer}      api_key: k3y\n"),
                "line 5: unknown field `api_key`, expected one of name, url, api_key_env",
            ),
        ];
        for (text, expected) in cases {
            let error = load(&text).unwrap_err();
            assert_eq!(
                (error.code, error.message),
                (ErrorCode::Usage, format!("$TORRENTIAL_HOME/config/config.yaml {expected}")),
                "{text}"
            );
        }
    }
}
