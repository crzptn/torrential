use std::net::SocketAddr;
use std::time::Duration;

use crate::alert::Alert;
use crate::bridge::{self, ffi};
use crate::config::{Config, Dht, DhtState};
use crate::error::{EngineError, ErrorKind};
use crate::identity::TorrentId;
use crate::torrent::{
    AddTorrent, Added, FileEntry, FileIndex, FilePriority, Metainfo, TorrentStatus,
};

/// One libtorrent session, which serves every torrent.
///
/// One thread owns a `Session`. It is `Send`, so it can be created and moved
/// to that thread, and not `Sync`, so no two threads call it at once. Other
/// threads talk to the owner through channels.
///
/// Calls that change a torrent return once libtorrent has queued the change.
/// The matching alert from `wait_alerts` confirms it. Dropping the session
/// blocks until libtorrent has shut down.
pub struct Session {
    native: cxx::UniquePtr<ffi::Session>,
}

impl Session {
    pub fn new(config: Config) -> Result<Session, EngineError> {
        if let (Dht::On { bootstrap_nodes }, Some(_)) = (&config.dht, &config.dht_state)
            && bootstrap_nodes.is_empty()
        {
            return Err(EngineError::new(
                ErrorKind::InvalidInput,
                "dht_state needs at least one DHT bootstrap node",
            ));
        }
        let native = ffi::new_session(&bridge::settings(&config)).map_err(bridge::error)?;
        Ok(Session { native })
    }

    pub fn add(&mut self, torrent: AddTorrent) -> Result<Added, EngineError> {
        let params = bridge::add_params(&torrent)?;
        let result = self
            .native
            .pin_mut()
            .add_torrent(&params)
            .map_err(bridge::error)?;
        let info_hash = bridge::info_hash(&result.hashes)?;
        Ok(if result.existed {
            Added::Existing(info_hash)
        } else {
            Added::New(info_hash)
        })
    }

    /// `Alert::TorrentPaused` follows once disk writes finish and the files are
    /// closed. Pausing a paused torrent changes nothing and posts no alert.
    pub fn pause(&mut self, id: TorrentId) -> Result<(), EngineError> {
        let r = self.native.pin_mut().pause(id.as_bytes());
        r.map_err(|e| with_id(e, id))
    }

    pub fn resume(&mut self, id: TorrentId) -> Result<(), EngineError> {
        let r = self.native.pin_mut().resume(id.as_bytes());
        r.map_err(|e| with_id(e, id))
    }

    /// Sets the listed files and leaves the rest as they are.
    /// `Alert::FilePrioritiesApplied` confirms the change. Needs metadata.
    pub fn set_file_priorities(
        &mut self,
        id: TorrentId,
        priorities: &[(FileIndex, FilePriority)],
    ) -> Result<(), EngineError> {
        let records: Vec<ffi::PriorityRecord> = priorities
            .iter()
            .map(|&(index, priority)| ffi::PriorityRecord {
                index: index.get(),
                priority: bridge::priority(priority),
            })
            .collect();
        let r = self
            .native
            .pin_mut()
            .set_file_priorities(id.as_bytes(), &records);
        r.map_err(|e| with_id(e, id))
    }

    /// Hashes every file on disk again. `Alert::TorrentChecked` follows, and
    /// pieces that fail are downloaded again once the torrent has peers.
    pub fn force_recheck(&mut self, id: TorrentId) -> Result<(), EngineError> {
        let r = self.native.pin_mut().force_recheck(id.as_bytes());
        r.map_err(|e| with_id(e, id))
    }

    /// Flushes the torrent's disk cache and asks for resume data, including
    /// the info dictionary. `Alert::ResumeDataSaved` or
    /// `Alert::ResumeDataFailed` follows.
    pub fn save_resume_data(&mut self, id: TorrentId) -> Result<(), EngineError> {
        let r = self.native.pin_mut().save_resume_data(id.as_bytes());
        r.map_err(|e| with_id(e, id))
    }

    /// Removes the torrent and keeps its files. `Alert::TorrentRemoved`
    /// follows.
    pub fn remove(&mut self, id: TorrentId) -> Result<(), EngineError> {
        let r = self.native.pin_mut().remove(id.as_bytes());
        r.map_err(|e| with_id(e, id))
    }

    pub fn connect_peer(&mut self, id: TorrentId, peer: SocketAddr) -> Result<(), EngineError> {
        let peer = bridge::endpoint(&peer);
        let r = self
            .native
            .pin_mut()
            .connect_peer(id.as_bytes(), &peer.address, peer.port);
        r.map_err(|e| with_id(e, id))
    }

    /// The info dictionary, plus piece layers for v2 and hybrid torrents, as
    /// a `.torrent` file. Trackers and web seeds are not included.
    pub fn metainfo(&self, id: TorrentId) -> Result<Metainfo, EngineError> {
        let bytes = self
            .native
            .metainfo(id.as_bytes())
            .map_err(|e| with_id(e, id))?;
        Metainfo::from_bytes(bytes)
    }

    /// Needs metadata.
    pub fn files(&self, id: TorrentId) -> Result<Vec<FileEntry>, EngineError> {
        let records = self
            .native
            .files(id.as_bytes())
            .map_err(|e| with_id(e, id))?;
        Ok(bridge::file_entries(records))
    }

    pub fn status(&self, id: TorrentId) -> Result<TorrentStatus, EngineError> {
        let record = self
            .native
            .status(id.as_bytes())
            .map_err(|e| with_id(e, id))?;
        bridge::status(record)
    }

    /// Asks for session counters. `Alert::SessionStats` delivers them.
    pub fn post_session_stats(&mut self) -> Result<(), EngineError> {
        self.native
            .pin_mut()
            .post_session_stats()
            .map_err(bridge::error)
    }

    /// The DHT routing table and node ids, for `Config::dht_state`.
    pub fn save_state(&self) -> Result<DhtState, EngineError> {
        let bytes = self.native.save_state().map_err(bridge::error)?;
        Ok(DhtState::from_bytes(bytes))
    }

    /// The port of the first listen socket that opened, or `None` while none
    /// has.
    pub fn listen_port(&self) -> Result<Option<u16>, EngineError> {
        let port = self.native.listen_port().map_err(bridge::error)?;
        Ok((port != 0).then_some(port))
    }

    /// Waits up to `timeout` for alerts, then returns every queued one. An
    /// empty list means the wait timed out.
    pub fn wait_alerts(&mut self, timeout: Duration) -> Result<Vec<Alert>, EngineError> {
        let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        let records = self
            .native
            .pin_mut()
            .wait_alerts(millis)
            .map_err(bridge::error)?;
        records.into_iter().map(bridge::alert).collect()
    }
}

fn with_id(e: cxx::Exception, id: TorrentId) -> EngineError {
    let e = bridge::error(e);
    match e.kind() {
        ErrorKind::UnknownTorrent | ErrorKind::NoMetadata if e.message().is_empty() => {
            EngineError::new(e.kind(), id.to_string())
        }
        _ => e,
    }
}
