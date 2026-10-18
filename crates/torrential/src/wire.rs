use std::io::{self, Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use torrential_core::protocol::{MAX_FRAME, encode_frame};

fn frame_len(header: [u8; 4]) -> io::Result<usize> {
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a {len}-byte frame is over the {MAX_FRAME}-byte limit"),
        ));
    }
    Ok(len)
}

fn decode<T: DeserializeOwned>(body: &[u8]) -> io::Result<T> {
    serde_json::from_slice(body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn encode(message: &impl Serialize) -> io::Result<Vec<u8>> {
    encode_frame(message).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write(w: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    w.write_all(&encode(message)?)?;
    w.flush()
}

/// `None` at a clean end of stream.
pub fn read<T: DeserializeOwned>(r: &mut impl Read) -> io::Result<Option<T>> {
    let mut header = [0; 4];
    match r.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let mut body = vec![0; frame_len(header)?];
    r.read_exact(&mut body)?;
    decode(&body).map(Some)
}

pub async fn write_async(w: &mut (impl AsyncWrite + Unpin), message: &impl Serialize) -> io::Result<()> {
    w.write_all(&encode(message)?).await?;
    w.flush().await
}

pub async fn read_async<T: DeserializeOwned>(r: &mut (impl AsyncRead + Unpin)) -> io::Result<Option<T>> {
    let mut header = [0; 4];
    match r.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let mut body = vec![0; frame_len(header)?];
    r.read_exact(&mut body).await?;
    decode(&body).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use torrential_core::protocol::Request;

    #[test]
    fn frames_round_trip_and_end_cleanly() {
        let mut buf = Vec::new();
        write(&mut buf, &Request::Shutdown).unwrap();
        let mut cursor = io::Cursor::new(buf);
        assert_eq!(read::<Request>(&mut cursor).unwrap(), Some(Request::Shutdown));
        assert_eq!(read::<Request>(&mut cursor).unwrap(), None);
    }

    #[test]
    fn oversized_frames_are_refused_before_allocating() {
        let mut cursor = io::Cursor::new(u32::MAX.to_be_bytes().to_vec());
        let err = read::<Request>(&mut cursor).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
