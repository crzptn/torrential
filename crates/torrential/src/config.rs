//! `config.yaml` in the config folder: where downloads go and which sources search
//! asks. The engine reads it when it starts. A file that does not parse is an error
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
//! ```

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_saphyr::{Localizer, Location, SnippetMode, Spanned, UserMessageFormatter};
use torrential_core::{Error, ErrorCode};
use torrential_search::{SearchConfig, TorznabConfig};

use crate::env::Env;
use crate::paths::Paths;

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Config {
    /// Where `get` puts downloads without `--dir`. `None` keeps the default.
    pub downloads: Option<PathBuf>,
    /// The sources, with Torznab API keys read from the environment, and no cache folder.
    pub search: SearchConfig,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct File {
    downloads: Downloads,
    sources: Sources,
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

pub fn path(paths: &Paths) -> PathBuf {
    paths.config.join("config.yaml")
}

impl Config {
    /// The defaults when the file does not exist.
    pub fn load(paths: &Paths, env: &Env) -> Result<Config, Error> {
        let path = path(paths);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(invalid(&path, None, &format!("cannot be read: {e}"))),
        };
        let mut config = parse(&text, env).map_err(|(line, message)| invalid(&path, line, &message))?;
        config.search.endpoints = test_endpoints(env)?;
        Ok(config)
    }

    pub fn downloads_dir(&self, paths: &Paths) -> PathBuf {
        self.downloads.clone().unwrap_or_else(|| paths.downloads.clone())
    }
}

fn parse(text: &str, env: &Env) -> Result<Config, Problem> {
    let file: File = serde_saphyr::from_str(text).map_err(|e| (e.location().map(|at| at.line()), describe(&e)))?;
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
    Ok(Config { downloads, search })
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
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().display().to_string();
        let env = Env::from_pairs([("HOME", "/Users/ada"), ("TORRENTIAL_HOME", home.as_str())]);
        let paths = Paths::resolve(&env, 501, None).unwrap();
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(path(&paths), text).unwrap();
        Config::load(&paths, &env).map_err(|mut e| {
            e.message = e.message.replace(&home, "$TORRENTIAL_HOME");
            e
        })
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
