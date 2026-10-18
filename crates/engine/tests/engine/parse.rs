use std::collections::BTreeSet;

use tempfile::TempDir;
use torrential_engine::{
    AddTorrent, Config, Dht, DhtState, ErrorKind, FileIndex, FilePriority, FileSelection, Magnet,
    Metainfo, PieceSize, Progress, ResumeData, Session, TorrentFormat, TorrentId, TorrentSource,
    libtorrent_version,
};

use crate::support::{Fixture, Node, bencode, loopback, magnet};

#[test]
fn links_the_pinned_libtorrent() {
    assert_eq!(libtorrent_version().unwrap(), "2.1.2.0");
}

#[test]
fn malformed_inputs_are_invalid_input() {
    let kind = |e: torrential_engine::EngineError| e.kind();
    assert_eq!(
        Magnet::parse("https://example.com/a.torrent")
            .map_err(kind)
            .unwrap_err(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        Metainfo::from_bytes(b"d4:infoi1ee".to_vec())
            .map_err(kind)
            .unwrap_err(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        ResumeData::from_bytes(b"not bencode".to_vec())
            .map_err(kind)
            .unwrap_err(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        PieceSize::new(24 * 1024).map_err(kind).unwrap_err(),
        ErrorKind::InvalidInput
    );
    assert_eq!(PieceSize::new(32 * 1024).unwrap().get(), 32_768);
    let garbage = Config {
        dht_state: Some(DhtState::from_bytes(b"garbage".to_vec())),
        ..loopback()
    };
    assert_eq!(
        Session::new(garbage).map(|_| ()).map_err(kind).unwrap_err(),
        ErrorKind::InvalidInput
    );
}

#[test]
fn torrent_calls_name_what_is_missing() {
    let mut node = Node::new(loopback());
    let save = TempDir::new().unwrap();

    let unknown = TorrentId::from_bytes([7; 20]);
    let err = node.session.pause(unknown).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UnknownTorrent);
    assert_eq!(err.message(), "0707070707070707070707070707070707070707");

    let lonely = Magnet::parse(&magnet(Some([9; 20]), None)).unwrap();
    let id = node.add(AddTorrent::new(TorrentSource::Magnet(lonely), save.path()));
    assert_eq!(node.status(id).progress, Progress::AwaitingMetadata);
    assert_eq!(
        node.session.files(id).unwrap_err().kind(),
        ErrorKind::NoMetadata
    );
    assert_eq!(
        node.session.metainfo(id).unwrap_err().kind(),
        ErrorKind::NoMetadata
    );
    let skip = [(FileIndex::new(0), FilePriority::Skip)];
    assert_eq!(
        node.session
            .set_file_priorities(id, &skip)
            .unwrap_err()
            .kind(),
        ErrorKind::NoMetadata
    );

    let fixture = Fixture::new("index", &[("a.bin", 20_000), ("b.bin", 20_000)], 19);
    let mut add = AddTorrent::new(
        TorrentSource::Metainfo(fixture.torrent(TorrentFormat::V1)),
        save.path(),
    );
    add.files = FileSelection::Only(BTreeSet::from([FileIndex::new(2)]));
    assert_eq!(
        node.session.add(add).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
}

#[test]
fn saved_dht_state_keeps_the_node_id() {
    let config = || Config {
        dht: Dht::On {
            bootstrap_nodes: vec!["127.0.0.1:9".to_owned()],
        },
        ..loopback()
    };
    let node_id = |s: &Session| {
        let state = s.save_state().unwrap();
        let dht = bencode::dict_value(state.as_bytes(), b"dht state")?;
        bencode::dict_value(dht, b"node-id").map(<[u8]>::to_vec)
    };

    let mut first = Node::new(config());
    first.wait_until("a DHT node id", |s| node_id(s).is_some());
    let saved = node_id(&first.session).unwrap();
    let state = first.session.save_state().unwrap();
    drop(first);

    let mut restored = Node::new(Config {
        dht_state: Some(state),
        ..config()
    });
    restored.wait_until("a DHT node id", |s| node_id(s).is_some());
    assert_eq!(node_id(&restored.session), Some(saved.clone()));
    let state_again = restored.session.save_state().unwrap();

    let unbootstrapped = Config {
        dht: Dht::On {
            bootstrap_nodes: Vec::new(),
        },
        dht_state: Some(state_again),
        ..loopback()
    };
    assert_eq!(
        Session::new(unbootstrapped).map(|_| ()).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );

    let mut fresh = Node::new(config());
    fresh.wait_until("a DHT node id", |s| node_id(s).is_some());
    assert_ne!(
        node_id(&fresh.session),
        Some(saved),
        "a fresh session picks its own id"
    );
}
