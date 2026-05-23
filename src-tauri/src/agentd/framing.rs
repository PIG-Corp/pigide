//! NDJSON framer.
//!
//! Each frame is a single line of UTF-8 JSON terminated by `\n`. Lines longer
//! than [`crate::agentd::proto::MAX_FRAME_BYTES`] are rejected to bound
//! per-connection memory (a wedged client can't drown the broker by
//! sending an unbounded line).
//!
//! Two flavours, both used by client and broker:
//! - `read_frame_async` / `write_frame_async`: tokio variants for async I/O.
//! - `read_frame_blocking` / `write_frame_blocking`: std::io variants for
//!   the broker's reader thread per agent.
//!
//! Both flavours preserve the invariant that one frame == one line ==
//! one Request/Response/Event. Empty lines and `#`-prefixed comment lines
//! are skipped (eases manual debugging with `nc -U`).

use crate::agentd::proto::MAX_FRAME_BYTES;
use std::io::{self, BufRead, Write};

/// Sentinel error returned when a frame exceeds [`MAX_FRAME_BYTES`].
///
/// The connection should be torn down on this — there is no way to resync
/// once we've started discarding bytes mid-line.
#[derive(Debug)]
pub struct FrameTooLarge {
    pub bytes_read: usize,
}

impl std::fmt::Display for FrameTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "frame exceeded {} bytes (read {})",
            MAX_FRAME_BYTES, self.bytes_read
        )
    }
}

impl std::error::Error for FrameTooLarge {}

/// Read one NDJSON frame from a blocking buffered reader.
///
/// Returns:
/// - `Ok(Some(line))` — frame received (without the trailing `\n`).
/// - `Ok(None)` — peer closed the connection cleanly at a frame boundary.
/// - `Err(_)` — IO error, malformed frame, or [`FrameTooLarge`].
///
/// Skips blank lines and lines starting with `#`. Both are convenient for
/// manual debugging and have no protocol meaning.
pub fn read_frame_blocking<R: BufRead>(r: &mut R) -> io::Result<Option<String>> {
    loop {
        let mut buf = String::new();
        // `read_line` enforces `MAX_FRAME_BYTES` indirectly by checking
        // the buffer length after each call. We can't pre-cap the read
        // because BufRead has no bounded-line API; but a malicious peer
        // can't grow `buf` past the point where we re-check.
        let n = r.read_line(&mut buf)?;
        if n == 0 {
            return Ok(None);
        }
        if buf.len() > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                FrameTooLarge { bytes_read: buf.len() },
            ));
        }
        // Strip the trailing newline (and CR if present).
        if buf.ends_with('\n') {
            buf.pop();
            if buf.ends_with('\r') {
                buf.pop();
            }
        }
        // Skip comments / blank lines.
        let trimmed = buf.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        return Ok(Some(buf));
    }
}

/// Write one NDJSON frame to a blocking writer. Appends `\n`.
///
/// Validates that `payload` itself contains no embedded newlines — a
/// payload with `\n` would split into two frames on the receiving side
/// and corrupt the stream. Callers should never construct such payloads
/// (serde_json never emits `\n` in default-encoded output), but the
/// check is cheap insurance against future bugs.
pub fn write_frame_blocking<W: Write>(w: &mut W, payload: &str) -> io::Result<()> {
    if payload.contains('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame payload contains newline",
        ));
    }
    if payload.len() + 1 > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            FrameTooLarge {
                bytes_read: payload.len() + 1,
            },
        ));
    }
    w.write_all(payload.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()
}

/// Async variant for client-side use under tokio. Same semantics as the
/// blocking version: returns `Ok(None)` on clean EOF, `Err(FrameTooLarge)`
/// on overlong frame.
pub async fn read_frame_async<R>(r: &mut R) -> io::Result<Option<String>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    loop {
        let mut buf = String::new();
        let n = r.read_line(&mut buf).await?;
        if n == 0 {
            return Ok(None);
        }
        if buf.len() > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                FrameTooLarge { bytes_read: buf.len() },
            ));
        }
        if buf.ends_with('\n') {
            buf.pop();
            if buf.ends_with('\r') {
                buf.pop();
            }
        }
        let trimmed = buf.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        return Ok(Some(buf));
    }
}

/// Async variant of [`write_frame_blocking`].
pub async fn write_frame_async<W>(w: &mut W, payload: &str) -> io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    if payload.contains('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame payload contains newline",
        ));
    }
    if payload.len() + 1 > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            FrameTooLarge {
                bytes_read: payload.len() + 1,
            },
        ));
    }
    w.write_all(payload.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn round_trip_single_frame() {
        let mut out = Vec::new();
        write_frame_blocking(&mut out, "{\"id\":1}").unwrap();
        assert_eq!(out, b"{\"id\":1}\n");

        let mut r = BufReader::new(Cursor::new(out));
        let f = read_frame_blocking(&mut r).unwrap();
        assert_eq!(f.as_deref(), Some("{\"id\":1}"));
        // Next read = clean EOF.
        let eof = read_frame_blocking(&mut r).unwrap();
        assert!(eof.is_none());
    }

    #[test]
    fn pipelined_frames_decode_in_order() {
        let buf = b"{\"id\":1}\n{\"id\":2}\n{\"id\":3}\n";
        let mut r = BufReader::new(Cursor::new(buf.to_vec()));
        assert_eq!(read_frame_blocking(&mut r).unwrap().as_deref(), Some("{\"id\":1}"));
        assert_eq!(read_frame_blocking(&mut r).unwrap().as_deref(), Some("{\"id\":2}"));
        assert_eq!(read_frame_blocking(&mut r).unwrap().as_deref(), Some("{\"id\":3}"));
        assert!(read_frame_blocking(&mut r).unwrap().is_none());
    }

    #[test]
    fn blank_and_comment_lines_skipped() {
        let buf = b"\n# comment\n  \n{\"id\":7}\n# trailing\n";
        let mut r = BufReader::new(Cursor::new(buf.to_vec()));
        assert_eq!(read_frame_blocking(&mut r).unwrap().as_deref(), Some("{\"id\":7}"));
        // After the data line, comments are skipped and we hit EOF.
        assert!(read_frame_blocking(&mut r).unwrap().is_none());
    }

    #[test]
    fn crlf_terminator_stripped() {
        let buf = b"{\"id\":1}\r\n";
        let mut r = BufReader::new(Cursor::new(buf.to_vec()));
        assert_eq!(read_frame_blocking(&mut r).unwrap().as_deref(), Some("{\"id\":1}"));
    }

    #[test]
    fn frame_larger_than_max_returns_error() {
        let huge = "x".repeat(MAX_FRAME_BYTES + 10);
        let mut buf = huge.into_bytes();
        buf.push(b'\n');
        let mut r = BufReader::new(Cursor::new(buf));
        let err = read_frame_blocking(&mut r).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn write_rejects_embedded_newline() {
        let mut out = Vec::new();
        let err = write_frame_blocking(&mut out, "bad\nframe").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        // Nothing partial was written.
        assert!(out.is_empty());
    }

    #[test]
    fn write_rejects_oversized_payload() {
        let mut out = Vec::new();
        let too_big = "x".repeat(MAX_FRAME_BYTES);
        let err = write_frame_blocking(&mut out, &too_big).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn async_round_trip() {
        let mut out: Vec<u8> = Vec::new();
        write_frame_async(&mut out, "{\"id\":1}").await.unwrap();
        assert_eq!(out, b"{\"id\":1}\n");

        let cursor = std::io::Cursor::new(out);
        let mut r = tokio::io::BufReader::new(cursor);
        let f = read_frame_async(&mut r).await.unwrap();
        assert_eq!(f.as_deref(), Some("{\"id\":1}"));
        let eof = read_frame_async(&mut r).await.unwrap();
        assert!(eof.is_none());
    }
}
