//! Domain types and the wire protocol shared by the torrential engine process,
//! the CLI, and the TUI. Everything here is plain data plus pure functions.

pub mod error;
pub mod id;
pub mod job;
pub mod protocol;
pub mod quality;
pub mod release;
pub mod text;
pub mod units;

pub use error::{Error, ErrorCode};
pub use id::{HexPrefix, InfoHash, LocatorId, ReleaseId, Target, TorrentId};
pub use job::{Eighths, FileRisk, Job, JobStatus, PieceMap, Progress, Selection, Slice, TorrentFile};
pub use quality::{Codec, Quality, Resolution};
pub use release::{Category, Flag, Release, SearchQuery, SearchResult, Sort, SourceOutcome, SourceReport};
