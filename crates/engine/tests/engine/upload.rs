use std::time::Duration;

use tempfile::TempDir;
use torrential_engine::{
    AddTorrent, Alert, SessionStats, TorrentFormat, TorrentSource, UploadPolicy,
};

use crate::support::{Fixture, Node, loopback, sha256_file, total_size, upload};

const FILES: [(&str, usize); 2] = [("a.bin", 60_000), ("b.bin", 40_000)];

struct Served {
    leecher_uploaded: u64,
    leecher_stats: SessionStats,
    peer_verified: u64,
    total: u64,
}

fn serve_third_peer(policy: UploadPolicy, name: &str) -> Served {
    let fixture = Fixture::new(name, &FILES, 7);
    let torrent = fixture.torrent(TorrentFormat::V1);
    let total = total_size(&torrent);

    let mut seeder = Node::new(loopback());
    seeder.seed(&torrent, fixture.save_path());

    let mut leecher = Node::new(upload(policy));
    let leecher_dir = TempDir::new().unwrap();
    let mut add = AddTorrent::new(TorrentSource::Metainfo(torrent.clone()), leecher_dir.path());
    add.peers = vec![seeder.addr()];
    let leecher_id = leecher.add(add);
    leecher.wait_for("leecher finish", |a| {
        matches!(a, Alert::TorrentFinished { .. })
    });
    drop(seeder);

    let mut peer = Node::new(loopback());
    let peer_dir = TempDir::new().unwrap();
    let mut add = AddTorrent::new(TorrentSource::Metainfo(torrent.clone()), peer_dir.path());
    add.peers = vec![leecher.addr()];
    let peer_id = peer.add(add);
    match policy {
        UploadPolicy::Off => {
            peer.wait_until("connected to the leecher", |s| {
                s.status(peer_id).unwrap().seeds == 1
            });
            peer.idle(Duration::from_secs(3));
            assert_eq!(
                peer.status(peer_id).seeds,
                1,
                "the leecher stayed connected"
            );
        }
        _ => {
            peer.finish(peer_id);
            for file in fixture.files.keys() {
                assert_eq!(
                    sha256_file(&fixture.path_in(peer_dir.path(), file)),
                    Some(fixture.sha256(file)),
                    "{file} reached the third peer intact"
                );
            }
        }
    }

    leecher.session.post_session_stats().unwrap();
    let Alert::SessionStats(leecher_stats) =
        leecher.wait_for("session stats", |a| matches!(a, Alert::SessionStats(_)))
    else {
        unreachable!()
    };
    Served {
        leecher_uploaded: leecher.status(leecher_id).uploaded_payload,
        leecher_stats,
        peer_verified: peer.status(peer_id).progress.verified_selected_bytes(),
        total,
    }
}

#[test]
fn upload_off_sends_no_payload_to_a_peer_that_needs_it() {
    let served = serve_third_peer(UploadPolicy::Off, "off");
    assert_eq!(served.leecher_uploaded, 0);
    assert_eq!(served.leecher_stats.sent_payload_bytes, 0);
    assert_eq!(served.peer_verified, 0);
    assert_eq!(served.leecher_stats.received_payload_bytes, served.total);
}

#[test]
fn upload_normal_serves_the_same_peer() {
    let served = serve_third_peer(UploadPolicy::Normal, "normal");
    assert_eq!(served.peer_verified, served.total);
    assert!(
        served.leecher_uploaded >= served.total,
        "uploaded {} of {}",
        served.leecher_uploaded,
        served.total
    );
    assert!(served.leecher_stats.sent_payload_bytes >= served.total);
}
