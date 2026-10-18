//! Official Debian torrents for the current release from cdimage.debian.org.

use url::Url;

use super::listing;
use super::{Ctx, Failure, at};
use crate::observation::Observation;

const DIRS: [&str; 4] = ["amd64/bt-cd/", "amd64/bt-dvd/", "arm64/bt-cd/", "arm64/bt-dvd/"];

pub(super) async fn search(ctx: &Ctx<'_>, base: &Url) -> Result<Vec<Observation>, Failure> {
    let current = at(base, "debian-cd/current/");
    let dirs = DIRS.iter().map(|d| at(&current, d)).collect();
    let mut files = Vec::new();
    for (dir, entries) in listing::fetch_all(ctx, dirs).await? {
        files.extend(entries.into_iter().filter(|e| ctx.terms.covers(e.name())).map(|e| (dir.clone(), e)));
    }
    Ok(listing::resolve(ctx, files).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metainfo;
    use crate::sources::fixture;

    #[test]
    fn lists_current_torrents_and_hashes_them() {
        let page = String::from_utf8(fixture::read("debian/debian-cd-current-arm64-bt-cd.html")).unwrap();
        let entries = listing::torrents(&page);
        assert_eq!(
            entries,
            [listing::Entry { file: "debian-13.7.0-arm64-netinst.iso.torrent".into(), modified: Some(1_789_219_560) }]
        );
        let torrent = fixture::read("debian/debian-cd-current-arm64-bt-cd-debian-13.7.0-arm64-netinst.iso.torrent");
        let meta = metainfo::parse(&torrent).unwrap();
        assert_eq!(meta.hash.id().to_hex(), "bf7ed99351f7b08f0916b7b3e49cfa574d206879");
        assert_eq!(meta.size, Some(735_358_976));
        assert_eq!(meta.trackers, vec!["http://bttracker.debian.org:6969/announce".to_string()]);
    }
}
