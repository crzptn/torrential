//! `config.toml` in the config folder: where downloads go and which sources search
//! asks. The engine reads it when it starts. A file that does not parse is an error
//! that names the line, never a silent fallback to the defaults.
//!
//! ```toml
//! [downloads]
//! dir = "~/Downloads/torrents"
//!
//! [sources]
//! disabled = ["nyaa"]
//!
//! [[sources.torznab]]
//! name = "jackett"
//! url = "http://127.0.0.1:9117/api/v2.0/indexers/all/results/torznab/api"
//! api_key_env = "JACKETT_API_KEY"
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Spanned;
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

/// A problem in the file, at a byte offset when one is known.
type Problem = (Option<usize>, String);

pub fn path(paths: &Paths) -> PathBuf {
    paths.config.join("config.toml")
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
        let mut config = parse(&text, env).map_err(|(at, message)| {
            let line = at.map(|at| text[..at.min(text.len())].matches('\n').count() + 1);
            invalid(&path, line, &message)
        })?;
        config.search.endpoints = test_endpoints(env)?;
        Ok(config)
    }

    pub fn downloads_dir(&self, paths: &Paths) -> PathBuf {
        self.downloads.clone().unwrap_or_else(|| paths.downloads.clone())
    }
}

fn parse(text: &str, env: &Env) -> Result<Config, Problem> {
    let file: File = toml::from_str(text).map_err(|e| (e.span().map(|s| s.start), e.message().to_string()))?;
    let downloads = file
        .downloads
        .dir
        .map(|dir| {
            let at = dir.span().start;
            let raw = dir.into_inner();
            let expanded = match (raw.strip_prefix('~'), env.var("HOME")) {
                (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
                    PathBuf::from(format!("{home}{rest}"))
                }
                _ => PathBuf::from(&raw),
            };
            if expanded.is_absolute() {
                Ok(expanded)
            } else {
                Err((Some(at), format!("downloads.dir {raw:?} must be an absolute path or start with ~/")))
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
                Some(var) => match env.var(var.get_ref()) {
                    Some(key) => Some(key.to_string()),
                    None => {
                        return Err((
                            Some(var.span().start),
                            format!("the {name:?} indexer takes its API key from {}, which is not set", var.get_ref()),
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

fn invalid(path: &Path, line: Option<usize>, message: &str) -> Error {
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
        let text = r#"
[downloads]
dir = "~/isos"

[sources]
disabled = ["nyaa", "apibay"]

[[sources.torznab]]
name = "jackett"
url = "http://127.0.0.1:9117/api"
api_key_env = "JACKETT_KEY"
"#;
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
    }

    #[test]
    fn a_bad_file_is_an_error_that_names_the_line() {
        let typo = load("[downloads]\ndir = \"/tmp/x\"\n\n[sources]\ndisable = [\"nyaa\"]\n").unwrap_err();
        let syntax = load("[downloads\ndir = 3\n").unwrap_err();
        let relative = load("[downloads]\ndir = \"isos\"\n").unwrap_err();
        let unset =
            load("[[sources.torznab]]\nname = \"j\"\nurl = \"http://x/api\"\napi_key_env = \"NOPE\"\n").unwrap_err();
        let key_in_file =
            load("[[sources.torznab]]\nname = \"j\"\nurl = \"http://x/api\"\napi_key = \"k3y\"\n").unwrap_err();

        assert_eq!(
            (typo.code, typo.message.as_str()),
            (
                ErrorCode::Usage,
                "$TORRENTIAL_HOME/config/config.toml line 5: unknown field `disable`, expected `disabled` or `torznab`"
            )
        );
        assert_eq!(syntax.message, "$TORRENTIAL_HOME/config/config.toml line 1: unclosed table, expected `]`");
        assert_eq!(
            relative.message,
            "$TORRENTIAL_HOME/config/config.toml line 2: downloads.dir \"isos\" must be an absolute path or start with ~/"
        );
        assert_eq!(
            unset.message,
            "$TORRENTIAL_HOME/config/config.toml line 4: the \"j\" indexer takes its API key from NOPE, which is not set"
        );
        assert!(key_in_file.message.starts_with("$TORRENTIAL_HOME/config/config.toml line 4: unknown field `api_key`"));
        assert!(!key_in_file.message.contains("k3y"));
    }
}
