//! The job book's domain model. A job's public status is derived from two facts kept
//! apart: what the user asked for ([`Intent`], stored) and what the engine is doing
//! ([`Observed`], in memory only).

use std::path::PathBuf;
use std::sync::Arc;

use torrential_core::{Error, InfoHash, Job, JobStatus, Progress, Selection, TorrentId};

use super::engine_port::{Metainfo, Stats};
use super::files;

#[derive(Clone, PartialEq, Debug)]
pub enum Intent {
    Running,
    Paused,
    Done { finished_at: i64 },
    Failed { error: Error },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Observed {
    /// Not in the engine. A running download here waits for a slot.
    Absent,
    /// Not in the engine since the engine restarted. Waits for `resume` or a new `get`.
    Parked,
    InEngine(Phase),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Metadata,
    Checking,
    Downloading,
    /// Asked to pause; writes have not stopped yet.
    Pausing,
    Paused,
    /// Every wanted piece verified. Waits for writes to stop, then for the final resume data.
    Finishing {
        writes_stopped: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verify {
    Report,
    Repair,
}

pub fn status(intent: &Intent, observed: Observed) -> JobStatus {
    match (intent, observed) {
        (_, Observed::InEngine(Phase::Checking)) => JobStatus::Checking,
        (Intent::Done { finished_at }, _) => JobStatus::Done { finished_at: *finished_at },
        (Intent::Failed { error }, _) => JobStatus::Error { error: error.clone() },
        (Intent::Paused, _) => JobStatus::Paused,
        (Intent::Running, Observed::Absent | Observed::InEngine(Phase::Paused)) => JobStatus::Queued,
        (Intent::Running, Observed::Parked) => JobStatus::Interrupted,
        (Intent::Running, Observed::InEngine(Phase::Metadata)) => JobStatus::Metadata,
        (Intent::Running, Observed::InEngine(Phase::Downloading | Phase::Pausing | Phase::Finishing { .. })) => {
            JobStatus::Downloading
        }
    }
}

impl Observed {
    pub fn occupies_slot(self) -> bool {
        matches!(
            self,
            Observed::InEngine(Phase::Metadata | Phase::Checking | Phase::Downloading | Phase::Finishing { .. })
        )
    }

    pub fn in_engine(self) -> bool {
        matches!(self, Observed::InEngine(_))
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct JobRecord {
    pub id: TorrentId,
    pub info_hash: InfoHash,
    pub name: Option<String>,
    pub dir: PathBuf,
    pub selection: Selection,
    pub max_size: u64,
    /// How to add the torrent before its metadata is known.
    pub magnet: Option<String>,
    pub added_at: i64,
    pub intent: Intent,
    /// Verified bytes at the last save, so progress survives a restart.
    pub verified: u64,
}

#[derive(Clone, Debug)]
pub struct JobEntry {
    pub record: JobRecord,
    pub metainfo: Option<Arc<Metainfo>>,
    pub observed: Observed,
    pub stats: Stats,
    pub verify: Option<Verify>,
}

impl JobEntry {
    pub fn new(record: JobRecord, metainfo: Option<Arc<Metainfo>>, observed: Observed) -> Self {
        Self { record, metainfo, observed, stats: Stats::default(), verify: None }
    }

    pub fn id(&self) -> TorrentId {
        self.record.id
    }

    pub fn wanted(&self) -> Option<Vec<bool>> {
        files::wanted(self.metainfo.as_deref()?, &self.record.selection).ok()
    }

    pub fn selected_size(&self) -> Option<u64> {
        let metainfo = self.metainfo.as_deref()?;
        let wanted = self.wanted()?;
        Some(metainfo.files.iter().zip(wanted).filter(|(_, w)| *w).map(|(f, _)| f.size).sum())
    }

    pub fn verified(&self) -> u64 {
        match (&self.record.intent, self.observed) {
            (Intent::Done { .. }, _) => self.selected_size().unwrap_or(self.record.verified),
            (_, Observed::InEngine(_)) => self.stats.verified.max(self.record.verified),
            _ => self.record.verified,
        }
    }

    pub fn remaining(&self) -> u64 {
        match self.record.intent {
            Intent::Running => self.selected_size().map_or(0, |size| size.saturating_sub(self.verified())),
            _ => 0,
        }
    }

    pub fn view(&self) -> Job {
        let status = status(&self.record.intent, self.observed);
        let transferring = matches!(status, JobStatus::Metadata | JobStatus::Downloading);
        let size = self.selected_size();
        let verified = self.verified();
        let s = if transferring { self.stats } else { Stats::default() };
        let eta = match size {
            Some(size) if s.down_rate > 0 => Some(size.saturating_sub(verified).div_ceil(s.down_rate)),
            _ => None,
        };
        Job {
            id: self.record.id,
            info_hash: self.record.info_hash,
            name: self.record.name.as_deref().map(torrential_core::text::sanitize),
            dir: self.record.dir.clone(),
            path: self.record.name.as_ref().map(|n| self.record.dir.join(n)),
            status,
            progress: Progress {
                size,
                verified,
                down_rate: s.down_rate,
                up_rate: s.up_rate,
                peers: s.peers,
                seeds: s.seeds,
                eta,
            },
            selection: self.record.selection.clone(),
            added_at: self.record.added_at,
        }
    }
}

pub fn to_start<'a>(jobs: impl Iterator<Item = &'a JobEntry>, max_active: usize) -> Vec<TorrentId> {
    let mut waiting = Vec::new();
    let mut active = 0;
    for job in jobs {
        if job.observed.occupies_slot() {
            active += 1;
        } else if job.record.intent == Intent::Running
            && matches!(job.observed, Observed::Absent | Observed::InEngine(Phase::Paused))
        {
            waiting.push((job.record.added_at, job.id()));
        }
    }
    waiting.sort();
    waiting.into_iter().take(max_active.saturating_sub(active)).map(|(_, id)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use torrential_core::ErrorCode;

    #[test]
    fn status_derivation_table() {
        let failed = Intent::Failed { error: Error::new(ErrorCode::DiskFull, "disk full") };
        let done = Intent::Done { finished_at: 7 };
        let engine = Observed::InEngine;
        let table: [(&Intent, Observed, &str); 20] = [
            (&Intent::Running, Observed::Absent, "queued"),
            (&Intent::Running, Observed::Parked, "interrupted"),
            (&Intent::Running, engine(Phase::Metadata), "metadata"),
            (&Intent::Running, engine(Phase::Checking), "checking"),
            (&Intent::Running, engine(Phase::Downloading), "downloading"),
            (&Intent::Running, engine(Phase::Pausing), "downloading"),
            (&Intent::Running, engine(Phase::Paused), "queued"),
            (&Intent::Running, engine(Phase::Finishing { writes_stopped: false }), "downloading"),
            (&Intent::Paused, Observed::Absent, "paused"),
            (&Intent::Paused, Observed::Parked, "paused"),
            (&Intent::Paused, engine(Phase::Pausing), "paused"),
            (&Intent::Paused, engine(Phase::Paused), "paused"),
            (&Intent::Paused, engine(Phase::Checking), "checking"),
            (&done, Observed::Absent, "done"),
            (&done, Observed::Parked, "done"),
            (&done, engine(Phase::Checking), "checking"),
            (&done, engine(Phase::Paused), "done"),
            (&failed, Observed::Absent, "error"),
            (&failed, engine(Phase::Checking), "checking"),
            (&failed, engine(Phase::Downloading), "error"),
        ];
        for (intent, observed, expected) in table {
            assert_eq!(status(intent, observed).name(), expected, "{intent:?} {observed:?}");
        }
        assert_eq!(status(&done, Observed::Absent), JobStatus::Done { finished_at: 7 });
    }

    fn entry(n: u8, added_at: i64, intent: Intent, observed: Observed) -> JobEntry {
        let id = TorrentId::from_bytes([n; 20]);
        let record = JobRecord {
            id,
            info_hash: InfoHash::V1([n; 20]),
            name: None,
            dir: PathBuf::from("/d"),
            selection: Selection::All,
            max_size: 1,
            magnet: None,
            added_at,
            intent,
            verified: 0,
        };
        JobEntry::new(record, None, observed)
    }

    #[test]
    fn starts_the_oldest_waiting_jobs_into_free_slots() {
        let jobs = [
            entry(1, 10, Intent::Running, Observed::InEngine(Phase::Downloading)),
            entry(2, 30, Intent::Running, Observed::Absent),
            entry(3, 20, Intent::Running, Observed::Absent),
            entry(4, 5, Intent::Paused, Observed::Absent),
            entry(5, 1, Intent::Running, Observed::Parked),
            entry(6, 40, Intent::Running, Observed::InEngine(Phase::Paused)),
        ];
        let ids: Vec<u8> = to_start(jobs.iter(), 3).iter().map(|id| id.as_bytes()[0]).collect();
        assert_eq!(ids, [3, 2]);
        let ids: Vec<u8> = to_start(jobs.iter(), 4).iter().map(|id| id.as_bytes()[0]).collect();
        assert_eq!(ids, [3, 2, 6]);
    }
}
