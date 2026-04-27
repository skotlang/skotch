//! JSON-RPC 2.0 transport with LSP/BSP Content-Length framing.
//!
//! Messages are framed as:
//! ```text
//! Content-Length: <length>\r\n
//! \r\n
//! <json-body>
//! ```

use anyhow::{Context, Result};
use std::io::{BufRead, Write};

/// Read a single JSON-RPC message from the reader.
/// Returns `Ok(None)` on EOF.
pub fn read_message<R: BufRead>(reader: &mut R) -> Result<Option<serde_json::Value>> {
    // Read headers until blank line.
    let mut content_length: Option<usize> = None;
    loop {
        let mut header = String::new();
        let n = reader.read_line(&mut header)?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let header = header.trim();
        if header.is_empty() {
            break; // End of headers
        }
        if let Some(len_str) = header
            .strip_prefix("Content-Length:")
            .or_else(|| header.strip_prefix("content-length:"))
        {
            content_length = len_str.trim().parse().ok();
        }
    }

    let length = content_length.context("missing Content-Length header")?;

    // Read the body.
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let msg: serde_json::Value = serde_json::from_slice(&body)?;
    Ok(Some(msg))
}

/// Write a JSON-RPC message with Content-Length framing.
pub fn write_message<W: Write>(writer: &mut W, msg: &serde_json::Value) -> Result<()> {
    let body = serde_json::to_string(msg)?;
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn roundtrip_message() {
        let msg = serde_json::json!({"jsonrpc": "2.0", "method": "test", "id": 1});
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();

        let mut reader = Cursor::new(buf);
        let read_back = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(read_back["method"], "test");
        assert_eq!(read_back["id"], 1);
    }

    #[test]
    fn eof_returns_none() {
        let mut reader = Cursor::new(Vec::<u8>::new());
        assert!(read_message(&mut reader).unwrap().is_none());
    }
}
