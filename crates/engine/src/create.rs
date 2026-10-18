use std::path::Path;

use crate::bridge::{self, ffi};
use crate::error::{EngineError, ErrorKind};
use crate::torrent::Metainfo;

/// A piece size libtorrent accepts for both v1 and v2: a power of two of at
/// least 16 KiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PieceSize(u32);

impl PieceSize {
    pub const MIN: PieceSize = PieceSize(16 * 1024);

    pub fn new(bytes: u32) -> Result<PieceSize, EngineError> {
        if bytes.is_power_of_two() && bytes >= Self::MIN.0 {
            Ok(PieceSize(bytes))
        } else {
            Err(EngineError::new(
                ErrorKind::InvalidInput,
                format!("piece size {bytes} is not a power of two of at least 16 KiB"),
            ))
        }
    }

    pub fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TorrentFormat {
    V1,
    /// v1 and v2 info in one info dictionary, with pad files that align every
    /// file to a piece boundary.
    Hybrid,
}

/// Hashes a file, or every file under a directory, into a `.torrent`. The
/// torrent's name is the path's last component, and files are ordered by path
/// so the same tree always produces the same info hash.
pub fn create_torrent(
    path: &Path,
    piece_size: PieceSize,
    format: TorrentFormat,
) -> Result<Metainfo, EngineError> {
    let path = path.canonicalize().map_err(|e| {
        EngineError::new(ErrorKind::InvalidInput, format!("{}: {e}", path.display()))
    })?;
    let bytes = ffi::create_torrent(
        bridge::path_str(&path)?,
        piece_size.get(),
        format == TorrentFormat::Hybrid,
    )
    .map_err(bridge::error)?;
    Metainfo::from_bytes(bytes)
}
