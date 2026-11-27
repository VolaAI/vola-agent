//! A minimal blocking WebSocket server (RFC 6455) for the GUI protocol.
//!
//! Only what the protocol needs: the opening handshake, text frames, fragmentation, ping/pong
//! and the close handshake. Reads happen on the connection thread; writes go through a
//! [`WsWriter`] that can be cloned into a turn's worker thread.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use serde::Serialize;
use sha1::{Digest, Sha1};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::protocol::ServerMessage;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const MAX_MESSAGE: usize = 16 * 1024 * 1024;
const MAX_HANDSHAKE: usize = 16 * 1024;

const OP_CONTINUATION: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

/// A accepted connection: read messages on one thread, write them from any thread.
pub struct WsConnection {
    frames: FrameReader<TcpStream>,
    writer: WsWriter,
    fragment_opcode: Option<u8>,
    fragments: Vec<u8>,
}

/// The writing half, cloned into turn workers.
#[derive(Clone)]
pub struct WsWriter {
    stream: Arc<Mutex<TcpStream>>,
}

impl WsConnection {
    /// Performs the HTTP upgrade on `stream` and returns the connection.
    pub fn accept(mut stream: TcpStream) -> Result<WsConnection> {
        stream.set_nodelay(true).ok();
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        handshake(&mut stream)?;
        stream.set_read_timeout(None)?;
        let write_stream = stream
            .try_clone()
            .context("failed to clone the WebSocket socket")?;
        Ok(WsConnection {
            frames: FrameReader::new(stream),
            writer: WsWriter {
                stream: Arc::new(Mutex::new(write_stream)),
            },
            fragment_opcode: None,
            fragments: Vec::new(),
        })
    }

    /// A handle for writing from another thread.
    pub fn writer(&self) -> WsWriter {
        self.writer.clone()
    }

    /// Reads the next message. `Ok(None)` means the peer closed the connection.
    pub fn read_text(&mut self) -> Result<Option<String>> {
        loop {
            let Some(frame) = self.frames.read_frame()? else {
                return Ok(None);
            };
            match frame.opcode {
                OP_CLOSE => {
                    self.writer.close(1000);
                    return Ok(None);
                }
                OP_PING => {
                    let _ = self.writer.send_raw(OP_PONG, &frame.payload);
                }
                OP_PONG => {}
                OP_CONTINUATION => {
                    if self.fragment_opcode.is_none() {
                        bail!("client sent a continuation frame without a message");
                    }
                    self.fragments.extend_from_slice(&frame.payload);
                    if self.fragments.len() > MAX_MESSAGE {
                        let _ = self.writer.close(1009);
                        bail!("client message exceeds {} bytes", MAX_MESSAGE);
                    }
                    if frame.fin {
                        let opcode = self.fragment_opcode.take().unwrap_or(OP_TEXT);
                        let payload = std::mem::take(&mut self.fragments);
                        return decode(opcode, payload).map(Some);
                    }
                }
                OP_TEXT | OP_BINARY => {
                    if self.fragment_opcode.is_some() {
                        bail!("client started a new message before finishing the last one");
                    }
                    if frame.fin {
                        return decode(frame.opcode, frame.payload).map(Some);
                    }
                    self.fragment_opcode = Some(frame.opcode);
                    self.fragments = frame.payload;
                }
                other => {
                    let _ = self.writer.close(1002);
                    bail!("unsupported WebSocket opcode 0x{:x}", other);
                }
            }
        }
    }
}

fn decode(_opcode: u8, payload: Vec<u8>) -> Result<String> {
    String::from_utf8(payload).context("client sent invalid UTF-8")
}

impl WsWriter {
    /// Sends one protocol message.
    pub fn send(&self, message: &ServerMessage) -> Result<()> {
        self.send_text(&message.to_json())
    }

    pub fn send_text(&self, text: &str) -> Result<()> {
        self.send_raw(OP_TEXT, text.as_bytes())
    }

    /// Sends any serializable value as JSON.
    pub fn send_json<T: Serialize>(&self, value: &T) -> Result<()> {
        self.send_text(&serde_json::to_string(value)?)
    }

    pub fn close(&self, code: u16) {
        let _ = self.send_raw(OP_CLOSE, &code.to_be_bytes());
    }

    fn send_raw(&self, opcode: u8, payload: &[u8]) -> Result<()> {
        let mut frame = Vec::with_capacity(payload.len() + 10);
        frame.push(0x80 | opcode);
        match payload.len() {
            n if n < 126 => frame.push(n as u8),
            n if n <= u16::MAX as usize => {
                frame.push(126);
                frame.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                frame.push(127);
                frame.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        frame.extend_from_slice(payload);
        let mut stream = self
            .stream
            .lock()
            .map_err(|_| anyhow!("the WebSocket writer is poisoned"))?;
        stream
            .write_all(&frame)
            .context("failed to write a WebSocket frame")?;
        stream
            .flush()
            .context("failed to flush a WebSocket frame")?;
        Ok(())
    }
}

// ---------- handshake ----------

fn handshake(stream: &mut TcpStream) -> Result<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => bail!("the client closed the connection during the handshake"),
            Ok(_) => {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
                if head.len() > MAX_HANDSHAKE {
                    bail!("the WebSocket handshake is too large");
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e).context("failed to read the WebSocket handshake"),
        }
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let request = lines.next().unwrap_or_default();
    if !request.starts_with("GET ") {
        bail!("expected `GET <path> HTTP/1.1` in the WebSocket handshake");
    }

    let mut key = None;
    let mut upgrade = false;
    let mut connection = false;
    let mut version = String::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "sec-websocket-key" => key = Some(value.to_string()),
            "upgrade" => upgrade = value.eq_ignore_ascii_case("websocket"),
            "connection" => {
                connection = value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            }
            "sec-websocket-version" => version = value.to_string(),
            _ => {}
        }
    }

    if !upgrade || !connection {
        let _ = stream.write_all(
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        bail!("the client did not ask to upgrade to WebSocket");
    }
    if version != "13" {
        let _ = stream.write_all(
            b"HTTP/1.1 426 Upgrade Required\r\nSec-WebSocket-Version: 13\r\nContent-Length: 0\r\n\r\n",
        );
        bail!("unsupported WebSocket version `{}`", version);
    }
    let key = key.context("the WebSocket handshake has no Sec-WebSocket-Key")?;
    let accept = accept_key(&key);
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept
    );
    stream
        .write_all(response.as_bytes())
        .and_then(|_| stream.flush())
        .context("failed to answer the WebSocket handshake")?;
    Ok(key)
}

fn accept_key(key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

// ---------- frames ----------

struct Frame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

struct FrameReader<R: Read> {
    inner: R,
}

impl<R: Read> FrameReader<R> {
    fn new(inner: R) -> FrameReader<R> {
        FrameReader { inner }
    }

    fn read_frame(&mut self) -> Result<Option<Frame>> {
        let mut head = [0u8; 2];
        if !self.read_exact_or_eof(&mut head)? {
            return Ok(None);
        }
        let fin = head[0] & 0x80 != 0;
        let opcode = head[0] & 0x0f;
        let masked = head[1] & 0x80 != 0;
        let len = match head[1] & 0x7f {
            126 => {
                let mut buf = [0u8; 2];
                self.inner
                    .read_exact(&mut buf)
                    .context("truncated WebSocket frame length")?;
                u16::from_be_bytes(buf) as usize
            }
            127 => {
                let mut buf = [0u8; 8];
                self.inner
                    .read_exact(&mut buf)
                    .context("truncated WebSocket frame length")?;
                let n = u64::from_be_bytes(buf);
                if n > MAX_MESSAGE as u64 {
                    bail!("WebSocket frame exceeds {} bytes", MAX_MESSAGE);
                }
                n as usize
            }
            n => n as usize,
        };
        if len > MAX_MESSAGE {
            bail!("WebSocket frame exceeds {} bytes", MAX_MESSAGE);
        }
        let mut mask = [0u8; 4];
        if masked {
            self.inner
                .read_exact(&mut mask)
                .context("truncated WebSocket mask")?;
        }
        let mut payload = vec![0u8; len];
        self.inner
            .read_exact(&mut payload)
            .context("truncated WebSocket payload")?;
        if masked {
            for (i, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[i % 4];
            }
        }
        Ok(Some(Frame {
            fin,
            opcode,
            payload,
        }))
    }

    /// Reads exactly `buf.len()` bytes; `Ok(false)` if the stream ended before the first byte.
    fn read_exact_or_eof(&mut self, buf: &mut [u8]) -> Result<bool> {
        let mut filled = 0;
        while filled < buf.len() {
            match self.inner.read(&mut buf[filled..]) {
                Ok(0) if filled == 0 => return Ok(false),
                Ok(0) => bail!("the WebSocket stream ended inside a frame"),
                Ok(n) => filled += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e).context("failed to read from the WebSocket"),
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// RFC 6455 section 1.3's example key and accept value.
    #[test]
    fn computes_the_rfc_accept_key() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn reads_masked_and_unmasked_frames() {
        // "hello" masked with 0x37 0xfa 0x21 0x3d.
        let masked = [
            0x81,
            0x85,
            0x37,
            0xfa,
            0x21,
            0x3d,
            b'h' ^ 0x37,
            b'e' ^ 0xfa,
            b'l' ^ 0x21,
            b'l' ^ 0x3d,
            b'o' ^ 0x37,
        ];
        let mut reader = FrameReader::new(Cursor::new(masked.to_vec()));
        let frame = reader.read_frame().unwrap().unwrap();
        assert!(frame.fin);
        assert_eq!(frame.opcode, OP_TEXT);
        assert_eq!(frame.payload, b"hello");

        let unmasked = [0x81, 0x02, b'h', b'i'];
        let mut reader = FrameReader::new(Cursor::new(unmasked.to_vec()));
        let frame = reader.read_frame().unwrap().unwrap();
        assert_eq!(frame.payload, b"hi");

        let mut reader = FrameReader::new(Cursor::new(Vec::new()));
        assert!(reader.read_frame().unwrap().is_none());
    }

    #[test]
    fn reads_extended_lengths_and_fragments() {
        let mut bytes = vec![0x01, 126, 0x00, 0x03, b'a', b'b', b'c'];
        bytes.extend_from_slice(&[0x80, 0x02, b'd', b'e']);
        let mut reader = FrameReader::new(Cursor::new(bytes));
        let first = reader.read_frame().unwrap().unwrap();
        assert!(!first.fin);
        assert_eq!(first.payload, b"abc");
        let second = reader.read_frame().unwrap().unwrap();
        assert!(second.fin);
        assert_eq!(second.payload, b"de");
    }
}
