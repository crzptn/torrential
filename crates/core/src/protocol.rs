//! Messages between a client (CLI or TUI) and the engine process over a Unix socket.
//!
//! A frame is a 4-byte big-endian length followed by that many bytes of JSON. The
//! client's first frame is `Request::Hello`. A `Watch` request turns the connection
//! into a stream of `Event` frames until the client disconnects.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::id::Target;
use crate::job::{Job, Selection, TorrentFile};
use crate::release::{SearchQuery, SearchResult};

pub const WIRE_VERSION: u32 = 1;
/// Version of every JSON document printed with `--json`.
pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_FRAME: usize = 32 * 1024 * 1024;

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum Request {
    Hello { client_version: String, wire: u32 },
    Search { query: SearchQuery },
    /// Lists a torrent's files, fetching metadata if needed, within `timeout_ms`.
    Files { target: Target, timeout_ms: u32 },
    Get(GetRequest),
    /// No refs means every download.
    Status { refs: Vec<String> },
    Pause { refs: Vec<String> },
    Resume { refs: Vec<String>, all: bool },
    Remove { refs: Vec<String> },
    Verify { reference: String, repair: bool },
    /// Streams `Event`s for these downloads, or for all downloads when empty.
    Watch { refs: Vec<String> },
    /// Asks the engine to save resume data and exit, for example before an upgrade.
    Shutdown,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct GetRequest {
    pub target: Target,
    /// `None` means the configured download folder.
    pub dir: Option<PathBuf>,
    /// Globs or exact paths. Empty means every file.
    pub files: Vec<String>,
    /// Overrides the per-download size cap.
    pub max_size: Option<u64>,
    pub dry_run: bool,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Response {
    Hello { engine_version: String, wire: u32, pid: u32 },
    Search { result: SearchResult },
    Files { files: FilesReply },
    Get { reply: Box<GetReply> },
    Jobs { jobs: Vec<Job> },
    Changed { changes: Vec<Change> },
    Ok,
    Error { error: Error },
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct FilesReply {
    pub id: String,
    pub name: Option<String>,
    pub total_size: u64,
    pub files: Vec<TorrentFile>,
    /// Where the file list came from: `download`, `cache`, `torrent_file`, `torrent_url`, or `peers`.
    pub metadata_source: String,
    pub elapsed_ms: u32,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct GetReply {
    pub outcome: GetOutcome,
    pub job: Job,
    /// Present for `--dry-run`, and when files are already known.
    pub plan: Option<DownloadPlan>,
    /// For `updated`: the file selection before this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_selection: Option<Selection>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GetOutcome {
    Started,
    Queued,
    /// The download already existed with the same selection. Nothing changed.
    Existing,
    /// The download already existed, and its file selection changed.
    Updated,
    /// `--dry-run`: nothing was started.
    Planned,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct DownloadPlan {
    pub selected_files: u32,
    pub total_files: u32,
    pub selected_size: u64,
    pub destination: PathBuf,
    pub free_space: u64,
    /// Paths that already exist and would block the download.
    pub conflicts: Vec<PathBuf>,
}

/// The result of a pause, resume, remove, or verify for one download.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Change {
    pub id: String,
    pub name: Option<String>,
    /// For example `paused`, `already_paused`, `resumed`, `removed`, `already_removed`, `checking`.
    pub result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum Event {
    /// The first event of every watch: the current state of each watched download.
    Snapshot { jobs: Vec<Job> },
    Job { job: Box<Job> },
    Removed { id: String },
    /// Sent when nothing changed for a while, so watchers know the engine is alive.
    Heartbeat { at: i64 },
}

/// Encodes one frame: a 4-byte big-endian length, then the JSON body.
pub fn encode_frame<T: Serialize>(message: &T) -> Result<Vec<u8>, serde_json::Error> {
    let body = serde_json::to_vec(message)?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_tagged_by_op() {
        let json = serde_json::to_value(Request::Pause { refs: vec!["9b5212".into()] }).unwrap();
        assert_eq!(json, serde_json::json!({"op": "pause", "refs": ["9b5212"]}));
    }

    #[test]
    fn frames_carry_a_big_endian_length() {
        let frame = encode_frame(&Response::Ok).unwrap();
        assert_eq!(&frame[..4], &[0, 0, 0, 13]);
        assert_eq!(&frame[4..], br#"{"type":"ok"}"#);
    }
}
