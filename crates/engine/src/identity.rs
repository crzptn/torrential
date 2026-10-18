use std::fmt;

/// A BitTorrent v1 info hash, the SHA-1 of the info dictionary.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InfoHashV1([u8; 20]);

/// A BitTorrent v2 info hash, the SHA-256 of the info dictionary.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InfoHashV2([u8; 32]);

/// The 20-byte key that names one torrent in a session: the v1 hash when the
/// torrent has one, otherwise the v2 hash truncated to 20 bytes.
///
/// A v2-only magnet for a hybrid torrent starts out keyed by the truncated v2
/// hash and learns its v1 hash when metadata arrives. Both keys then find it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TorrentId([u8; 20]);

/// Which protocol versions a torrent's info hash covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InfoHash {
    V1(InfoHashV1),
    V2(InfoHashV2),
    Hybrid { v1: InfoHashV1, v2: InfoHashV2 },
}

impl InfoHash {
    pub fn id(&self) -> TorrentId {
        match self {
            InfoHash::V1(v1) | InfoHash::Hybrid { v1, .. } => TorrentId(v1.0),
            InfoHash::V2(v2) => {
                let mut id = [0; 20];
                id.copy_from_slice(&v2.0[..20]);
                TorrentId(id)
            }
        }
    }

    pub fn v1(&self) -> Option<InfoHashV1> {
        match self {
            InfoHash::V1(v1) | InfoHash::Hybrid { v1, .. } => Some(*v1),
            InfoHash::V2(_) => None,
        }
    }

    pub fn v2(&self) -> Option<InfoHashV2> {
        match self {
            InfoHash::V2(v2) | InfoHash::Hybrid { v2, .. } => Some(*v2),
            InfoHash::V1(_) => None,
        }
    }
}

impl InfoHashV1 {
    pub const fn from_bytes(bytes: [u8; 20]) -> Self {
        InfoHashV1(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }
}

impl InfoHashV2 {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        InfoHashV2(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl TorrentId {
    pub const fn from_bytes(bytes: [u8; 20]) -> Self {
        TorrentId(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }
}

fn write_hex(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    bytes.iter().try_for_each(|b| write!(f, "{b:02x}"))
}

macro_rules! hex_formatting {
    ($($t:ty),*) => {$(
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write_hex(f, &self.0)
            }
        }

        impl fmt::Debug for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}(", stringify!($t))?;
                write_hex(f, &self.0)?;
                f.write_str(")")
            }
        }
    )*};
}

hex_formatting!(InfoHashV1, InfoHashV2, TorrentId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_prefers_v1_and_truncates_v2() {
        let v1 = InfoHashV1::from_bytes([0xab; 20]);
        let mut v2_bytes = [0; 32];
        v2_bytes[..20].copy_from_slice(&[0x11; 20]);
        v2_bytes[20..].copy_from_slice(&[0x22; 12]);
        let v2 = InfoHashV2::from_bytes(v2_bytes);

        assert_eq!(InfoHash::Hybrid { v1, v2 }.id(), TorrentId::from_bytes([0xab; 20]));
        assert_eq!(InfoHash::V2(v2).id(), TorrentId::from_bytes([0x11; 20]));
        assert_eq!(
            InfoHash::V1(v1).id().to_string(),
            "abababababababababababababababababababab"
        );
    }
}
