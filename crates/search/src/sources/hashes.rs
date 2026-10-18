use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use torrential_core::InfoHash;

use crate::metainfo::Metainfo;

const FILE: &str = "torrent-hashes.json";
const KEEP: usize = 256;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Key {
    pub url: String,
    pub modified: Option<i64>,
}

pub(crate) struct Hashes {
    dir: Option<PathBuf>,
    known: Mutex<HashMap<Key, Metainfo>>,
    claimed: Mutex<HashSet<Key>>,
    unsaved: AtomicBool,
    saving: tokio::sync::Mutex<()>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    url: String,
    modified: Option<i64>,
    hash: InfoHash,
    size: Option<u64>,
    trackers: Vec<String>,
}

impl Hashes {
    pub fn new(dir: Option<PathBuf>) -> Hashes {
        let known = dir.as_deref().map(|d| parse(&std::fs::read(d.join(FILE)).unwrap_or_default())).unwrap_or_default();
        Hashes {
            dir,
            known: Mutex::new(known),
            claimed: Mutex::default(),
            unsaved: AtomicBool::new(false),
            saving: tokio::sync::Mutex::new(()),
        }
    }

    pub fn claim(&self, key: &Key) -> bool {
        !self.lock().contains_key(key) && self.claimed.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone())
    }

    pub fn get(&self, key: &Key) -> Option<Metainfo> {
        self.lock().get(key).cloned()
    }

    pub fn insert(&self, key: Key, meta: Metainfo) {
        let mut known = self.lock();
        known.insert(key, meta);
        trim(&mut known);
        self.unsaved.store(true, Ordering::Relaxed);
    }

    pub async fn save(&self) {
        let Some(dir) = &self.dir else { return };
        let _one = self.saving.lock().await;
        if !self.unsaved.swap(false, Ordering::Relaxed) {
            return;
        }
        let path = dir.join(FILE);
        let mut all = parse(&tokio::fs::read(&path).await.unwrap_or_default());
        all.extend(self.lock().iter().map(|(k, v)| (k.clone(), v.clone())));
        trim(&mut all);
        let mut stored: Vec<Stored> = all
            .into_iter()
            .map(|(Key { url, modified }, Metainfo { hash, size, trackers })| Stored {
                url,
                modified,
                hash,
                size,
                trackers,
            })
            .collect();
        stored.sort_by(|a, b| (&a.url, a.modified).cmp(&(&b.url, b.modified)));
        let body = serde_json::to_vec_pretty(&stored).expect("stored hashes serialize");
        if write(dir, &path, &body).await.is_err() {
            self.unsaved.store(true, Ordering::Relaxed);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Metainfo>> {
        self.known.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn parse(body: &[u8]) -> HashMap<Key, Metainfo> {
    let stored: Vec<Stored> = serde_json::from_slice(body).unwrap_or_default();
    stored
        .into_iter()
        .map(|s| {
            (Key { url: s.url, modified: s.modified }, Metainfo { hash: s.hash, size: s.size, trackers: s.trackers })
        })
        .collect()
}

fn trim(known: &mut HashMap<Key, Metainfo>) {
    if known.len() > KEEP {
        let mut keys: Vec<Key> = known.keys().cloned().collect();
        keys.sort_by_key(|k| std::cmp::Reverse(k.modified));
        for key in &keys[KEEP..] {
            known.remove(key);
        }
    }
}

async fn write(dir: &Path, path: &Path, body: &[u8]) -> std::io::Result<()> {
    let tmp = dir.join(format!("{FILE}.{}.tmp", std::process::id()));
    let written = async {
        tokio::fs::create_dir_all(dir).await?;
        tokio::fs::write(&tmp, body).await?;
        tokio::fs::rename(&tmp, path).await
    };
    let written = written.await;
    if written.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    written
}
