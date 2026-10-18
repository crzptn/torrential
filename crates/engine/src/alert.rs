use std::net::SocketAddr;
use std::path::PathBuf;

use crate::error::NativeError;
use crate::identity::InfoHash;
use crate::torrent::{ResumeData, TorrentState};

/// A libtorrent event, read with `Session::wait_alerts`. Torrent events carry
/// the torrent's current info hash, which gains its v1 half when a v2-only
/// magnet's metadata arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alert {
    MetadataReceived {
        torrent: InfoHash,
    },
    MetadataFailed {
        torrent: InfoHash,
        error: NativeError,
    },
    /// Every selected piece passed its hash check.
    TorrentFinished {
        torrent: InfoHash,
    },
    /// Outstanding disk writes completed and the torrent's files are closed.
    TorrentPaused {
        torrent: InfoHash,
    },
    TorrentResumed {
        torrent: InfoHash,
    },
    /// A file check, from adding the torrent or `force_recheck`, completed.
    TorrentChecked {
        torrent: InfoHash,
    },
    StateChanged {
        torrent: InfoHash,
        from: TorrentState,
        to: TorrentState,
    },
    ResumeDataSaved {
        torrent: InfoHash,
        data: ResumeData,
    },
    ResumeDataFailed {
        torrent: InfoHash,
        error: NativeError,
    },
    /// The torrent stopped with an error. libtorrent pauses it.
    TorrentError {
        torrent: InfoHash,
        file: Option<PathBuf>,
        error: NativeError,
    },
    FileError {
        torrent: InfoHash,
        file: PathBuf,
        operation: String,
        error: NativeError,
    },
    /// A downloaded piece failed its hash check and will be downloaded again.
    HashFailed {
        torrent: InfoHash,
        piece: u32,
    },
    TorrentRemoved {
        torrent: InfoHash,
    },
    FilePrioritiesApplied {
        torrent: InfoHash,
    },
    FilePrioritiesFailed {
        torrent: InfoHash,
        error: NativeError,
    },
    ListenSucceeded {
        address: SocketAddr,
        transport: Transport,
    },
    ListenFailed {
        /// The interface as written in `Config::listen`.
        interface: String,
        port: u16,
        transport: Transport,
        operation: String,
        error: NativeError,
    },
    DhtBootstrapped,
    SessionStats(SessionStats),
}

impl Alert {
    /// The torrent this alert is about, if it is about one.
    pub fn torrent(&self) -> Option<InfoHash> {
        match self {
            Alert::MetadataReceived { torrent }
            | Alert::MetadataFailed { torrent, .. }
            | Alert::TorrentFinished { torrent }
            | Alert::TorrentPaused { torrent }
            | Alert::TorrentResumed { torrent }
            | Alert::TorrentChecked { torrent }
            | Alert::StateChanged { torrent, .. }
            | Alert::ResumeDataSaved { torrent, .. }
            | Alert::ResumeDataFailed { torrent, .. }
            | Alert::TorrentError { torrent, .. }
            | Alert::FileError { torrent, .. }
            | Alert::HashFailed { torrent, .. }
            | Alert::TorrentRemoved { torrent }
            | Alert::FilePrioritiesApplied { torrent }
            | Alert::FilePrioritiesFailed { torrent, .. } => Some(*torrent),
            Alert::ListenSucceeded { .. }
            | Alert::ListenFailed { .. }
            | Alert::DhtBootstrapped
            | Alert::SessionStats(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Tcp,
    TcpSsl,
    Utp,
    UtpSsl,
    Socks5,
    Socks5Ssl,
    Http,
    HttpSsl,
    I2p,
    Rtc,
}

/// Session-wide counters since the session started, from
/// `Session::post_session_stats`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionStats {
    pub sent_payload_bytes: u64,
    pub received_payload_bytes: u64,
    /// Payload plus protocol overhead.
    pub sent_bytes: u64,
    pub received_bytes: u64,
    pub dht_nodes: u64,
    pub peers_connected: u64,
}
