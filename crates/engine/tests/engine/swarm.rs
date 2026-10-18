use std::collections::BTreeSet;
use std::path::PathBuf;

use tempfile::TempDir;
use torrential_engine::{
    AddTorrent, Added, Alert, FileEntry, FileIndex, FileSelection, InfoHash, Magnet, Progress,
    TorrentFormat, TorrentSource,
};

use crate::support::{
    Fixture, Node, bencode, expected_hashes, info_hash, loopback, magnet, sha256_file,
};

/// Sizes chosen so each file boundary falls inside a 16 KiB piece: piece 2
/// holds the end of a.bin and the start of b.bin, and piece 4 holds the end
/// of b.bin and the start of c.bin.
const FILES: [(&str, usize); 3] = [("a.bin", 40_000), ("b.bin", 30_000), ("c.bin", 50_000)];

#[test]
fn magnet_leecher_fetches_metadata_and_only_the_selected_files() {
    let fixture = Fixture::new("swarm", &FILES, 1);
    let torrent = fixture.torrent(TorrentFormat::V1);
    let hashes = expected_hashes(torrent.as_bytes());
    assert_eq!(torrent.info_hash(), info_hash(hashes, TorrentFormat::V1));
    assert_eq!(
        torrent.files(),
        [
            entry(0, "swarm/a.bin", 40_000),
            entry(1, "swarm/b.bin", 30_000),
            entry(2, "swarm/c.bin", 50_000),
        ]
    );

    let mut seeder = Node::new(loopback());
    seeder.seed(&torrent, fixture.save_path());

    let mut leecher = Node::new(loopback());
    let download = TempDir::new().unwrap();
    let magnet = Magnet::parse(&magnet(Some(hashes.0), None)).unwrap();
    let mut add = AddTorrent::new(TorrentSource::Magnet(magnet.clone()), download.path());
    add.files = FileSelection::Only(BTreeSet::from([FileIndex::new(0), FileIndex::new(2)]));
    add.peers = vec![seeder.addr()];
    let added = leecher.session.add(add.clone()).unwrap();
    assert_eq!(added, Added::New(torrent.info_hash()));
    let id = added.id();
    assert_eq!(
        leecher.session.add(add).unwrap(),
        Added::Existing(torrent.info_hash())
    );

    leecher.wait_for("metadata", |a| matches!(a, Alert::MetadataReceived { .. }));
    leecher.finish(id);

    for file in ["a.bin", "c.bin"] {
        assert_eq!(
            sha256_file(&fixture.path_in(download.path(), file)),
            Some(fixture.sha256(file)),
            "{file} matches the original"
        );
    }
    assert_ne!(
        sha256_file(&fixture.path_in(download.path(), "b.bin")),
        Some(fixture.sha256("b.bin")),
        "the unselected file is not complete on disk"
    );

    let status = leecher.status(id);
    assert_eq!(
        status.info_hash,
        InfoHash::V1(torrent.info_hash().v1().unwrap())
    );
    let Progress::Known(files) = &status.progress else {
        panic!("metadata is known: {status:?}");
    };
    let verified: Vec<(u32, u64)> = files.iter().map(|f| (f.index.get(), f.verified)).collect();
    // b.bin shares pieces 2 and 4 with its neighbours: 49_152 - 40_000 bytes
    // and 70_000 - 65_536 bytes.
    assert_eq!(verified, [(0, 40_000), (1, 9_152 + 4_464), (2, 50_000)]);
    assert_eq!(status.progress.selected_bytes(), 90_000);
    assert_eq!(status.progress.verified_selected_bytes(), 90_000);

    assert_eq!(leecher.session.files(id).unwrap(), torrent.files());
    let cached = leecher.session.metainfo(id).unwrap();
    assert_eq!(cached.info_hash(), torrent.info_hash());
    assert_eq!(
        bencode::dict_value(cached.as_bytes(), b"info"),
        bencode::dict_value(torrent.as_bytes(), b"info")
    );
}

fn entry(index: u32, path: &str, size: u64) -> FileEntry {
    FileEntry {
        index: FileIndex::new(index),
        path: PathBuf::from(path),
        size,
    }
}
