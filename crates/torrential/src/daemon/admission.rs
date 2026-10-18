//! Disk state comes in through [`Disk`], so tests run the checks against a fake filesystem.

use std::path::{Path, PathBuf};

use torrential_core::error::Details;
use torrential_core::protocol::DownloadPlan;
use torrential_core::units::format_size;
use torrential_core::{Error, ErrorCode, Selection};

use super::engine_port::Metainfo;
use super::files;

pub const GIB: u64 = 1024 * 1024 * 1024;
pub const DEFAULT_MAX_SIZE: u64 = 50 * GIB;
const MIN_RESERVE: u64 = 2 * GIB;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Space {
    /// Identifies the filesystem, so downloads sharing a disk share its free space.
    pub device: u64,
    pub total: u64,
    pub free: u64,
}

pub trait Disk: Send {
    /// The filesystem holding `path`, or its nearest existing ancestor.
    fn space(&self, path: &Path) -> std::io::Result<Space>;
    fn exists(&self, path: &Path) -> bool;
}

pub fn reserve(total: u64) -> u64 {
    (total / 20).max(MIN_RESERVE)
}

#[derive(Clone, PartialEq, Debug)]
pub struct Plan {
    pub wanted: Vec<bool>,
    pub selected_size: u64,
    pub destination: PathBuf,
    pub conflicts: Vec<PathBuf>,
    pub space: Space,
}

impl Plan {
    pub fn public(&self) -> DownloadPlan {
        DownloadPlan {
            selected_files: self.wanted.iter().filter(|w| **w).count() as u32,
            total_files: self.wanted.len() as u32,
            selected_size: self.selected_size,
            destination: self.destination.clone(),
            free_space: self.space.free,
            conflicts: self.conflicts.clone(),
        }
    }
}

/// Files in `already_wanted` were written by this download, so they are not conflicts.
pub fn plan(
    metainfo: &Metainfo,
    selection: &Selection,
    dir: &Path,
    before: Option<&[bool]>,
    disk: &dyn Disk,
) -> Result<Plan, Error> {
    let wanted = files::wanted(metainfo, selection)?;
    let selected_size = metainfo.files.iter().zip(&wanted).filter(|(_, w)| **w).map(|(f, _)| f.size).sum();
    let new: Vec<bool> = match before {
        Some(before) => wanted.iter().zip(before).map(|(now, was)| *now && !*was).collect(),
        None => wanted.clone(),
    };
    let conflicts = files::targets(metainfo, dir, &new).into_iter().filter(|p| disk.exists(p)).collect();
    let space = disk.space(dir).map_err(|e| {
        Error::new(ErrorCode::StorageError, format!("cannot read free space at {}: {e}", dir.display()))
    })?;
    Ok(Plan { wanted, selected_size, destination: dir.join(&metainfo.name), conflicts, space })
}

pub fn check_size(plan: &Plan, max_size: u64) -> Result<(), Error> {
    if plan.selected_size <= max_size {
        return Ok(());
    }
    Err(Error::new(
        ErrorCode::TooLarge,
        format!("the selection is {}, over the {} cap", format_size(plan.selected_size), format_size(max_size)),
    )
    .with_hint(format!(
        "add --max-size {}GiB to allow it, or pick fewer files with --files",
        plan.selected_size.div_ceil(GIB)
    )))
}

pub fn check_conflicts(plan: &Plan) -> Result<(), Error> {
    let n = plan.conflicts.len();
    if n == 0 {
        return Ok(());
    }
    Err(Error::new(
        ErrorCode::PathConflict,
        format!(
            "{n} {} under {}",
            if n == 1 { "file already exists" } else { "files already exist" },
            plan.destination.display()
        ),
    )
    .with_details(Details::Paths(plan.conflicts.iter().map(|p| p.display().to_string()).collect()))
    .with_hint("choose another folder with --dir, or move the existing files; torrential never overwrites"))
}

pub fn check_disk(space: Space, needed: u64, committed: u64) -> Result<(), Error> {
    let keep = reserve(space.total);
    let required = needed + committed + keep;
    if required <= space.free {
        return Ok(());
    }
    let mut message = format!("needs {} more on this disk", format_size(needed));
    if committed > 0 {
        message.push_str(&format!(" plus {} for other downloads", format_size(committed)));
    }
    message.push_str(&format!(
        ", but only {} is free and {} stays in reserve",
        format_size(space.free),
        format_size(keep)
    ));
    Err(Error::new(ErrorCode::DiskFull, message)
        .with_details(Details::Space { required, available: space.free })
        .with_hint("free some space, or choose another disk with --dir"))
}

pub struct RealDisk;

impl Disk for RealDisk {
    fn space(&self, path: &Path) -> std::io::Result<Space> {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::MetadataExt;
        let existing = path.ancestors().find(|p| p.exists()).unwrap_or(Path::new("/"));
        let device = std::fs::metadata(existing)?.dev();
        let c_path = std::ffi::CString::new(existing.as_os_str().as_bytes())?;
        // SAFETY: statvfs is plain data, so all zeros is a valid value. The call writes only into it,
        // and c_path is NUL-terminated.
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let block = stat.f_frsize as u64;
        Ok(Space { device: device as u64, total: stat.f_blocks as u64 * block, free: stat.f_bavail as u64 * block })
    }

    fn exists(&self, path: &Path) -> bool {
        path.symlink_metadata().is_ok()
    }
}

/// For the fake engine, which writes nothing: plenty of space, and the real filesystem for conflicts.
pub struct RoomyDisk;

impl Disk for RoomyDisk {
    fn space(&self, _: &Path) -> std::io::Result<Space> {
        Ok(Space { device: 0, total: 1 << 50, free: 1 << 49 })
    }

    fn exists(&self, path: &Path) -> bool {
        RealDisk.exists(path)
    }
}

#[cfg(test)]
#[derive(Clone)]
pub struct FakeDisk(std::sync::Arc<std::sync::Mutex<(Space, std::collections::BTreeSet<PathBuf>)>>);

#[cfg(test)]
impl FakeDisk {
    pub fn new(space: Space) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new((space, Default::default()))))
    }

    pub fn create(&self, path: impl Into<PathBuf>) {
        self.0.lock().unwrap().1.insert(path.into());
    }

    pub fn set_free(&self, free: u64) {
        self.0.lock().unwrap().0.free = free;
    }
}

#[cfg(test)]
impl Disk for FakeDisk {
    fn space(&self, _: &Path) -> std::io::Result<Space> {
        Ok(self.0.lock().unwrap().0)
    }

    fn exists(&self, path: &Path) -> bool {
        self.0.lock().unwrap().1.contains(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::engine_port::FakeTorrentFile;
    use torrential_core::InfoHash;

    const DISK: Space = Space { device: 1, total: 100 * GIB, free: 20 * GIB };

    fn torrent(size: u64) -> Metainfo {
        FakeTorrentFile {
            info_hash: InfoHash::V1([7; 20]),
            name: "set".into(),
            files: vec![("a.bin".into(), size), ("b.bin".into(), GIB)],
        }
        .metainfo()
    }

    fn plan_for(size: u64, disk: &FakeDisk) -> Plan {
        plan(&torrent(size), &Selection::All, Path::new("/dl"), None, disk).unwrap()
    }

    #[test]
    fn reserve_is_five_percent_with_a_two_gib_floor() {
        assert_eq!(reserve(100 * GIB), 5 * GIB);
        assert_eq!(reserve(10 * GIB), 2 * GIB);
    }

    #[test]
    fn admits_what_fits_above_the_reserve() {
        let disk = FakeDisk::new(DISK);
        let plan = plan_for(14 * GIB, &disk);
        assert_eq!(plan.selected_size, 15 * GIB);
        assert_eq!(check_disk(plan.space, plan.selected_size, 0), Ok(()));
    }

    #[test]
    fn refuses_what_would_eat_the_reserve_counting_other_downloads() {
        let disk = FakeDisk::new(DISK);
        let plan = plan_for(13 * GIB, &disk);
        let err = check_disk(plan.space, plan.selected_size, 2 * GIB).unwrap_err();
        assert_eq!(err.code, ErrorCode::DiskFull);
        assert_eq!(
            err.message,
            "needs 14.0 GiB more on this disk plus 2.0 GiB for other downloads, but only 20.0 GiB is free and 5.0 GiB stays in reserve"
        );
        assert_eq!(err.details, Some(Details::Space { required: 21 * GIB, available: 20 * GIB }));
    }

    #[test]
    fn refuses_past_the_size_cap_unless_raised() {
        let disk = FakeDisk::new(Space { free: 500 * GIB, total: 1000 * GIB, ..DISK });
        let plan = plan_for(60 * GIB, &disk);
        let err = check_size(&plan, DEFAULT_MAX_SIZE).unwrap_err();
        assert_eq!(err.code, ErrorCode::TooLarge);
        assert_eq!(err.message, "the selection is 61.0 GiB, over the 50.0 GiB cap");
        assert_eq!(err.hint.as_deref(), Some("add --max-size 61GiB to allow it, or pick fewer files with --files"));
        assert_eq!(check_size(&plan, 61 * GIB), Ok(()));
    }

    #[test]
    fn existing_files_conflict_unless_the_download_wrote_them() {
        let disk = FakeDisk::new(DISK);
        disk.create("/dl/set/b.bin");
        let plan = plan_for(GIB, &disk);
        assert_eq!(plan.conflicts, [PathBuf::from("/dl/set/b.bin")]);
        assert_eq!(check_conflicts(&plan).unwrap_err().code, ErrorCode::PathConflict);
        let update = super::plan(&torrent(GIB), &Selection::All, Path::new("/dl"), Some(&[true, true]), &disk).unwrap();
        assert!(update.conflicts.is_empty());
    }
}
