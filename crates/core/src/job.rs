use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::id::{InfoHash, TorrentId};

/// A download. Its ID is the torrent's ID, so a search result keeps its ID when downloaded.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: TorrentId,
    pub info_hash: InfoHash,
    /// Unknown until metadata arrives for a bare infohash.
    pub name: Option<String>,
    /// The folder the download goes into.
    pub dir: PathBuf,
    /// `dir` joined with the torrent's name, once the name is known.
    pub path: Option<PathBuf>,
    #[serde(flatten)]
    pub status: JobStatus,
    pub progress: Progress,
    pub selection: Selection,
    /// Unix seconds.
    pub added_at: i64,
}

/// Where a download is in its life. Each variant carries only what is true in that state.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum JobStatus {
    /// Waiting for a free download slot.
    Queued,
    /// Fetching the file list from peers.
    Metadata,
    /// Hashing data already on disk.
    Checking,
    Downloading,
    Paused,
    /// Every selected piece passed its hash check and was written. Nothing uploads after this.
    Done {
        finished_at: i64,
    },
    /// The engine stopped while the download was active, for example at a reboot.
    Interrupted,
    Error {
        error: Error,
    },
}

impl JobStatus {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Metadata => "metadata",
            Self::Checking => "checking",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Done { .. } => "done",
            Self::Interrupted => "interrupted",
            Self::Error { .. } => "error",
        }
    }

    /// No further progress happens without a user action.
    pub const fn is_stopped(&self) -> bool {
        matches!(self, Self::Paused | Self::Done { .. } | Self::Interrupted | Self::Error { .. })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Progress {
    /// Bytes selected for download. `None` until metadata arrives.
    pub size: Option<u64>,
    /// Selected bytes in pieces that passed their hash check.
    pub verified: u64,
    /// Payload bytes per second.
    pub down_rate: u64,
    pub up_rate: u64,
    pub peers: u32,
    pub seeds: u32,
    /// Seconds to completion at the current rate.
    pub eta: Option<u64>,
}

impl Progress {
    pub fn fraction(&self) -> Option<f64> {
        match self.size {
            Some(0) => Some(1.0),
            Some(size) => Some(self.verified as f64 / size as f64),
            None => None,
        }
    }
}

/// Which files to download. Patterns apply once the file list is known.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum Selection {
    #[default]
    All,
    /// Globs or exact relative paths. `*` also matches `/`.
    Patterns(Vec<String>),
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TorrentFile {
    pub index: u32,
    /// Relative to the torrent's folder, sanitized.
    pub path: String,
    pub size: u64,
    pub selected: bool,
    pub verified: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<FileRisk>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileRisk {
    /// A program or script inside a torrent that otherwise holds media.
    Executable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn status_flattens_into_the_job_document() {
        let status = JobStatus::Error { error: Error::new(ErrorCode::DiskFull, "disk full") };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["state"], "error");
        assert_eq!(json["error"]["code"], "disk_full");
        let done = serde_json::to_value(JobStatus::Done { finished_at: 7 }).unwrap();
        assert_eq!(done, serde_json::json!({"state": "done", "finished_at": 7}));
    }

    #[test]
    fn fraction_is_unknown_until_size_is_known() {
        let mut p = Progress { verified: 50, ..Progress::default() };
        assert_eq!(p.fraction(), None);
        p.size = Some(200);
        assert_eq!(p.fraction(), Some(0.25));
    }
}
