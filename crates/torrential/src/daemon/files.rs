//! Which files of a torrent a download wants, and what is risky about them.

use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use torrential_core::{Error, ErrorCode, FileRisk, Selection};

use super::engine_port::Metainfo;

/// Refuses a torrent whose name or file paths would leave its folder. Every torrent from the
/// engine or a `.torrent` file passes through here before anything uses its paths.
pub fn check_paths(metainfo: &Metainfo) -> Result<(), Error> {
    let escapes = |p: &str| p.is_empty() || p.starts_with('/') || p.split('/').any(|c| c == ".." || c.is_empty());
    if escapes(&metainfo.name) || metainfo.name.contains('/') || metainfo.files.iter().any(|f| escapes(&f.path)) {
        return Err(Error::new(ErrorCode::InvalidTorrent, "the torrent has a file path that leaves its folder")
            .with_hint("this torrent is malformed or hostile; do not download it"));
    }
    Ok(())
}

/// `*` matches `/` too, and matching ignores case, so `*.MKV` finds `Season 1/e01.mkv`.
pub fn compile(patterns: &[String]) -> Result<GlobSet, Error> {
    let mut set = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(pattern).case_insensitive(true).literal_separator(false).build().map_err(|e| {
            Error::new(ErrorCode::Usage, format!("--files {pattern:?} is not a valid glob: {}", e.kind()))
                .with_hint("use a glob such as '*.mkv' or an exact path from torrential files")
        })?;
        set.add(glob);
    }
    set.build().map_err(|e| Error::new(ErrorCode::Usage, format!("--files patterns do not compile: {e}")))
}

/// One flag per file. Fails with `no_selection` when patterns match nothing.
pub fn wanted(metainfo: &Metainfo, selection: &Selection) -> Result<Vec<bool>, Error> {
    let patterns = match selection {
        Selection::All => return Ok(vec![true; metainfo.files.len()]),
        Selection::Patterns(patterns) => patterns,
    };
    let set = compile(patterns)?;
    let wanted: Vec<bool> = metainfo.files.iter().map(|f| set.is_match(&f.path)).collect();
    if wanted.contains(&true) {
        Ok(wanted)
    } else {
        let quoted: Vec<String> = patterns.iter().map(|p| format!("{p:?}")).collect();
        Err(Error::new(ErrorCode::NoSelection, format!("no file in {} matches {}", metainfo.name, quoted.join(", ")))
            .with_hint(format!("run torrential files {} to see the paths", &metainfo.info_hash.id().to_hex()[..6])))
    }
}

/// Where each wanted file lands: `<dir>/<name>` for a single file, `<dir>/<name>/<path>` otherwise.
pub fn targets(metainfo: &Metainfo, dir: &Path, wanted: &[bool]) -> Vec<PathBuf> {
    let root = dir.join(&metainfo.name);
    if metainfo.single_file {
        return if wanted.first() == Some(&true) { vec![root] } else { Vec::new() };
    }
    metainfo.files.iter().zip(wanted).filter(|(_, w)| **w).map(|(f, _)| root.join(&f.path)).collect()
}

const EXECUTABLE: [&str; 25] = [
    "exe", "msi", "bat", "cmd", "com", "scr", "pif", "ps1", "vbs", "vbe", "js", "jse", "wsf", "hta", "jar", "sh",
    "command", "app", "dmg", "pkg", "apk", "lnk", "cpl", "run", "reg",
];
const MEDIA: [&str; 22] = [
    "mkv", "mp4", "avi", "mov", "wmv", "flv", "webm", "m4v", "mpg", "mpeg", "ts", "m2ts", "mp3", "flac", "aac", "ogg",
    "opus", "wav", "m4a", "wma", "srt", "ass",
];

fn extension(path: &str) -> Option<String> {
    Path::new(path).extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// Marks programs and scripts in a torrent whose bytes are mostly media, a common malware disguise.
pub fn risks(metainfo: &Metainfo) -> Vec<Option<FileRisk>> {
    let is = |list: &[&str], path: &str| extension(path).is_some_and(|e| list.contains(&e.as_str()));
    let media: u64 = metainfo.files.iter().filter(|f| is(&MEDIA, &f.path)).map(|f| f.size).sum();
    let mostly_media = media * 2 > metainfo.total();
    metainfo.files.iter().map(|f| (mostly_media && is(&EXECUTABLE, &f.path)).then_some(FileRisk::Executable)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::engine_port::FakeTorrentFile;
    use torrential_core::InfoHash;

    fn film() -> Metainfo {
        FakeTorrentFile {
            info_hash: InfoHash::V1([0x6b; 20]),
            name: "night_of_the_living_dead_dvd".into(),
            files: vec![
                ("Night.mp4".into(), 596_399_542),
                ("Night.asr.srt".into(), 52_633),
                ("extras/VTS_01_1.MP4".into(), 262_144_000),
                ("Codec Installer.exe".into(), 1_200_000),
            ],
        }
        .metainfo()
    }

    #[test]
    fn globs_match_across_folders_and_ignore_case() {
        let pick =
            |p: &[&str]| wanted(&film(), &Selection::Patterns(p.iter().map(|s| s.to_string()).collect())).unwrap();
        assert_eq!(pick(&["*.mp4"]), [true, false, true, false]);
        assert_eq!(pick(&["Night.mp4", "night.asr.srt"]), [true, true, false, false]);
        assert_eq!(wanted(&film(), &Selection::All).unwrap(), [true; 4]);
    }

    #[test]
    fn patterns_that_match_nothing_are_refused() {
        let err = wanted(&film(), &Selection::Patterns(vec!["*.mkv".into()])).unwrap_err();
        assert_eq!(err.code, ErrorCode::NoSelection);
        assert_eq!(err.message, "no file in night_of_the_living_dead_dvd matches \"*.mkv\"");
        assert_eq!(err.hint.as_deref(), Some("run torrential files 6b6b6b to see the paths"));
        assert_eq!(compile(&["[".into()]).unwrap_err().code, ErrorCode::Usage);
    }

    #[test]
    fn executables_in_media_torrents_are_marked() {
        assert_eq!(risks(&film()), [None, None, None, Some(FileRisk::Executable)]);
        let mut tools = film();
        tools.files.retain(|f| f.path.ends_with(".exe"));
        assert_eq!(risks(&tools), [None]);
    }

    #[test]
    fn paths_that_leave_the_folder_are_refused() {
        let with = |name: &str, path: &str| {
            FakeTorrentFile { info_hash: InfoHash::V1([1; 20]), name: name.into(), files: vec![(path.into(), 1)] }
                .metainfo()
        };
        assert_eq!(check_paths(&with("set", "a/b.mkv")), Ok(()));
        for (name, path) in
            [("set", "../etc/passwd"), ("set", "/etc/passwd"), ("..", "a"), ("a/b", "c"), ("set", "a//b")]
        {
            assert_eq!(check_paths(&with(name, path)).unwrap_err().code, ErrorCode::InvalidTorrent, "{name} {path}");
        }
    }

    #[test]
    fn single_files_land_beside_folders() {
        let dir = Path::new("/d");
        assert_eq!(
            targets(&film(), dir, &[true, false, true, false]),
            [
                PathBuf::from("/d/night_of_the_living_dead_dvd/Night.mp4"),
                PathBuf::from("/d/night_of_the_living_dead_dvd/extras/VTS_01_1.MP4")
            ]
        );
        let iso = FakeTorrentFile {
            info_hash: InfoHash::V1([1; 20]),
            name: "a.iso".into(),
            files: vec![("a.iso".into(), 5)],
        };
        assert_eq!(targets(&iso.metainfo(), dir, &[true]), [PathBuf::from("/d/a.iso")]);
    }
}
