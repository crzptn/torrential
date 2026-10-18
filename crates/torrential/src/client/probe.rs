//! `doctor`'s config and source checks. With `--network`, this process searches each
//! enabled source once, built from the same config the engine reads.

use torrential_core::error::Details;
use torrential_core::{Error, SearchQuery, SourceOutcome, SourceReport};
use torrential_search::{ResultCache, Search, SearchConfig};

use crate::config::{self, Config};
use crate::output::views::{Check, CheckStatus, source_outcome};
use crate::paths::Paths;

/// Lawful, cheap for every source, and answered by the general indexes. It names no
/// official image, so the Ubuntu and Debian sources read their listings without
/// fetching any `.torrent`.
const PROBE: &str = "linux";

pub fn checks(paths: &Paths, settings: Result<Config, Error>, network: bool) -> Vec<Check> {
    let check = |check: &str, status, detail: String| Check { check: check.into(), status, detail };
    let path = config::path(paths);
    let search = settings.and_then(|settings| {
        let config = SearchConfig { cache_dir: Some(paths.cache.join("search")), ..settings.search };
        Search::new(&config, ResultCache::new())
    });
    let search = match search {
        Ok(search) => search,
        Err(error) => {
            return vec![
                check("config", CheckStatus::Fail, error.message),
                check("sources", CheckStatus::Skipped, "the config has an error".into()),
            ];
        }
    };
    let detail = if path.exists() {
        path.display().to_string()
    } else {
        format!("no {}, so the defaults apply", path.display())
    };
    let mut checks = vec![check("config", CheckStatus::Ok, detail)];
    let enabled: Vec<String> = search.sources().into_iter().filter(|s| s.default_on).map(|s| s.name).collect();
    if !network {
        let detail = format!("{} enabled; torrential doctor --network asks each one", enabled.len());
        checks.push(check("sources", CheckStatus::Skipped, detail));
        return checks;
    }
    match ask(&search, enabled) {
        Ok(reports) => checks.extend(reports.into_iter().map(|r| {
            let status = match r.outcome {
                ref ok if ok.is_ok() => CheckStatus::Ok,
                SourceOutcome::Irrelevant => CheckStatus::Warn,
                _ => CheckStatus::Fail,
            };
            let detail = format!("{}, {} ms", source_outcome(&r.outcome), r.elapsed_ms);
            check(&format!("source {}", r.source), status, detail)
        })),
        Err(error) => checks.push(check("sources", CheckStatus::Fail, error.message)),
    }
    checks
}

/// Every source's report, including when none answered.
fn ask(search: &Search, sources: Vec<String>) -> Result<Vec<SourceReport>, Error> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::internal(format!("cannot start the async runtime: {e}")))?;
    let query = SearchQuery { sources, fresh: true, ..SearchQuery::new(PROBE) };
    match runtime.block_on(search.search(&query)) {
        Ok(result) => Ok(result.sources),
        Err(Error { details: Some(Details::Sources(reports)), .. }) => Ok(reports),
        Err(error) => Err(error),
    }
}
