//! Just enough bencode to identify a `.torrent` file: its infohashes, size, and trackers.

use sha1::{Digest, Sha1};
use sha2::Sha256;
use torrential_core::InfoHash;

use crate::observation::tracker;

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Metainfo {
    pub hash: InfoHash,
    /// Total bytes of a v1 file list. `None` for v2-only torrents.
    pub size: Option<u64>,
    pub trackers: Vec<String>,
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Metainfo, String> {
    let mut parser = Parser { bytes, pos: 0, depth: 0 };
    let root = parser.value()?;
    if parser.pos != bytes.len() {
        return Err("trailing bytes after the torrent".into());
    }
    let info = root.get(b"info").ok_or("no info dictionary")?;
    if !matches!(info.kind, Kind::Dict(_)) {
        return Err("info is not a dictionary".into());
    }
    let info_bytes = &bytes[info.span.0..info.span.1];
    let v2 = info.get(b"meta version").and_then(Value::int) == Some(2);
    let v1 = info.get(b"pieces").is_some();
    let sha1 = || -> [u8; 20] { Sha1::digest(info_bytes).into() };
    let sha256 = || -> [u8; 32] { Sha256::digest(info_bytes).into() };
    let hash = match (v1, v2) {
        (true, true) => InfoHash::Hybrid { v1: sha1(), v2: sha256() },
        (false, true) => InfoHash::V2(sha256()),
        _ => InfoHash::V1(sha1()),
    };
    let size = match (info.get(b"length"), info.get(b"files")) {
        (Some(length), _) => length.int().and_then(|n| u64::try_from(n).ok()),
        (None, Some(Value { kind: Kind::List(files), .. })) => files.iter().try_fold(0u64, |total, f| {
            let length = f.get(b"length").and_then(Value::int).and_then(|n| u64::try_from(n).ok())?;
            total.checked_add(length)
        }),
        _ => None,
    };
    let mut trackers: Vec<String> = Vec::new();
    let announce_list = root.get(b"announce-list").map(Value::list).unwrap_or_default();
    let announce = root.get(b"announce").into_iter();
    for url in announce.chain(announce_list.iter().flat_map(|tier| tier.list())) {
        if let Some(t) = url.text().and_then(tracker)
            && !trackers.contains(&t)
        {
            trackers.push(t);
        }
    }
    Ok(Metainfo { hash, size, trackers })
}

struct Value<'a> {
    kind: Kind<'a>,
    span: (usize, usize),
}

enum Kind<'a> {
    Int(i64),
    Bytes(&'a [u8]),
    List(Vec<Value<'a>>),
    Dict(Vec<(&'a [u8], Value<'a>)>),
}

impl<'a> Value<'a> {
    fn get(&self, key: &[u8]) -> Option<&Value<'a>> {
        match &self.kind {
            Kind::Dict(entries) => entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn int(&self) -> Option<i64> {
        match self.kind {
            Kind::Int(n) => Some(n),
            _ => None,
        }
    }

    fn text(&self) -> Option<&'a str> {
        match self.kind {
            Kind::Bytes(b) => std::str::from_utf8(b).ok(),
            _ => None,
        }
    }

    fn list(&self) -> &[Value<'a>] {
        match &self.kind {
            Kind::List(items) => items,
            _ => &[],
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    const MAX_DEPTH: usize = 64;

    fn value(&mut self) -> Result<Value<'a>, String> {
        let start = self.pos;
        let kind = match self.peek()? {
            b'i' => {
                self.pos += 1;
                Kind::Int(self.int_until(b'e')?)
            }
            b'l' | b'd' => {
                self.depth += 1;
                if self.depth > Self::MAX_DEPTH {
                    return Err("nested too deeply".into());
                }
                let dict = self.peek()? == b'd';
                self.pos += 1;
                let mut items = Vec::new();
                let mut entries = Vec::new();
                while self.peek()? != b'e' {
                    if dict {
                        let key = self.bytes()?;
                        entries.push((key, self.value()?));
                    } else {
                        items.push(self.value()?);
                    }
                }
                self.pos += 1;
                self.depth -= 1;
                if dict { Kind::Dict(entries) } else { Kind::List(items) }
            }
            b'0'..=b'9' => Kind::Bytes(self.bytes()?),
            other => return Err(format!("unexpected byte {other:#04x} at {start}")),
        };
        Ok(Value { kind, span: (start, self.pos) })
    }

    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let len = usize::try_from(self.int_until(b':')?).map_err(|_| "negative string length")?;
        let end = self.pos.checked_add(len).filter(|end| *end <= self.bytes.len()).ok_or("string runs past the end")?;
        let out = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn int_until(&mut self, end: u8) -> Result<i64, String> {
        let rest = &self.bytes[self.pos..];
        let len = rest.iter().position(|b| *b == end).ok_or("unterminated number")?;
        let text = std::str::from_utf8(&rest[..len]).map_err(|_| "number is not ASCII")?;
        let n = text.parse().map_err(|_| format!("bad number {text:?}"))?;
        self.pos += len + 1;
        Ok(n)
    }

    fn peek(&self) -> Result<u8, String> {
        self.bytes.get(self.pos).copied().ok_or_else(|| "unexpected end of torrent".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_the_info_dictionary_bytes_exactly() {
        let info = b"d6:lengthi1024e4:name5:a.iso12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae";
        let torrent = [b"d8:announce39:udp://tracker.example.org:1337/announce4:info".as_slice(), info, b"e"].concat();
        let meta = parse(&torrent).unwrap();
        let expected: [u8; 20] = Sha1::digest(info).into();
        assert_eq!(meta.hash, InfoHash::V1(expected));
        assert_eq!(meta.size, Some(1024));
        assert_eq!(meta.trackers, vec!["udp://tracker.example.org:1337/announce".to_string()]);
    }

    #[test]
    fn rejects_truncated_and_hostile_input() {
        assert_eq!(parse(b"d4:infod6:lengthi1").unwrap_err(), "unterminated number");
        assert_eq!(parse(b"d4:info99999999999:x").unwrap_err(), "string runs past the end");
        assert_eq!(parse(&[vec![b'l'; 100], vec![b'e'; 100]].concat()).unwrap_err(), "nested too deeply");
        assert_eq!(parse(b"d4:infoi3ee").unwrap_err(), "info is not a dictionary");
    }
}
