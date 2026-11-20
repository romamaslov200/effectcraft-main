//! The control-channel client: talks a source Craft app over its loopback JSON-lines control
//! channel (`--control PORT`). One persistent connection; one JSON object per line in each
//! direction.
//!
//! Methods the client uses (registered by every app that exposes Dynamic Link):
//! - `dynamicLink.items` → `{items: [{id, name, width, height, frameRateNum, frameRateDen, durationSeconds}]}`
//! - `dynamicLink.frame` → `{png: "<base64>", width, height}` for a given item at a time

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::{Value, json};

use crate::{Error, LinkTarget, Result};

/// Slightly longer than the app's own per-request timeout so its error reply wins.
const READ_TIMEOUT: Duration = Duration::from_secs(65);

/// A session with a source app's control channel.
pub struct SourceClient {
    addr: String,
    conn: Option<(BufReader<TcpStream>, TcpStream)>,
    next_id: u64,
}

impl SourceClient {
    /// Connect to `host:port` (host must be loopback).
    pub fn connect(target: &LinkTarget) -> Result<Self> {
        let addr = format!("127.0.0.1:{}", target.port);
        let sa = addr
            .to_socket_addrs()
            .map_err(|e| Error::Bridge(format!("{addr}: {e}")))?
            .next()
            .ok_or_else(|| Error::BadAddr(format!("cannot resolve {addr}")))?;
        let s = TcpStream::connect_timeout(&sa, Duration::from_secs(5))
            .map_err(|e| Error::Unreachable(target.port))?;
        s.set_read_timeout(Some(READ_TIMEOUT)).ok();
        s.set_nodelay(true).ok();
        let r = s.try_clone().map_err(|e| Error::Bridge(e.to_string()))?;
        Ok(Self {
            addr,
            conn: Some((BufReader::new(r), s)),
            next_id: 1,
        })
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let mut last = Error::Bridge("not attempted".into());
        for attempt in 0..2 {
            let mut conn = self.conn.take();
            let Some((r, w)) = conn.as_mut() else { continue };
            let id = self.next_id;
            self.next_id = self.next_id.checked_add(1).ok_or_else(|| Error::Bridge("control request id space exhausted".into()))?;
            let line = format!("{}\n", json!({"id": id, "method": method, "params": params}));
            let res = (|| -> Result<std::result::Result<Value, Error>> {
                if let Err(e) = w.write_all(line.as_bytes()) {
                    return Ok(Err(Error::Bridge(e.to_string())));
                }
                let mut buf = String::new();
                match r.read_line(&mut buf) {
                    Ok(0) => return Ok(Err(Error::Bridge("connection closed by the source app".into()))),
                    Ok(_) => {}
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                        return Err(Error::Bridge(format!("no reply to `{method}` within {}s", READ_TIMEOUT.as_secs())));
                    }
                    Err(e) => return Ok(Err(Error::Bridge(e.to_string()))),
                }
                serde_json::from_str(&buf).map(Ok).map_err(|e| Error::Bridge(format!("bad reply: {e}")))
            })();
            self.conn = conn;
            match res {
                Ok(Ok(v)) if v.get("ok").and_then(Value::as_bool) == Some(true) => return Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                Ok(Ok(v)) => return Err(Error::App(v.get("error").and_then(Value::as_str).unwrap_or("error").to_string())),
                Ok(Err(e)) => {
                    last = e;
                    if attempt == 1 {
                        break;
                    }
                    // reconnect once for the retry
                }
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    /// List the linkable items (comps / sequences) of the source project.
    pub fn items(&mut self) -> Result<Vec<super::model::LinkItem>> {
        let v = self.call("dynamicLink.items", json!({}))?;
        let arr = v.get("items").and_then(Value::as_array).ok_or_else(|| Error::Other("source did not return an `items` list".into()))?;
        arr.iter()
            .map(|item| {
                Ok(super::model::LinkItem {
                    id: item.get("id").and_then(Value::as_u64).ok_or_else(|| Error::Other("item missing `id`".into()))?,
                    name: item.get("name").and_then(Value::as_str).ok_or_else(|| Error::Other("item missing `name`".into()))?.to_string(),
                    width: item.get("width").and_then(Value::as_u64).ok_or_else(|| Error::Other("item missing `width`".into()))? as u32,
                    height: item.get("height").and_then(Value::as_u64).ok_or_else(|| Error::Other("item missing `height`".into()))? as u32,
                    frame_rate_num: item.get("frameRateNum").and_then(Value::as_u64).unwrap_or(0) as u32,
                    frame_rate_den: item.get("frameRateDen").and_then(Value::as_u64).unwrap_or(1) as u32,
                    duration_seconds: item.get("durationSeconds").and_then(Value::as_f64).unwrap_or(0.0),
                })
            })
            .collect()
    }

    /// Render one frame of `item` at `time_seconds` into PNG bytes (base64 in the reply).
    pub fn frame(&mut self, item: u64, time_seconds: f64, max_side: u32) -> Result<(Vec<u8>, u32, u32)> {
        let v = self.call(
            "dynamicLink.frame",
            json!({"item": item, "time": time_seconds, "maxSide": max_side, "base64": true}),
        )?;
        let png_b64 = v.get("png").and_then(Value::as_str).ok_or_else(|| Error::Other("source did not return a `png` frame".into()))?;
        let width = v.get("width").and_then(Value::as_u64).ok_or_else(|| Error::Other("frame missing `width`".into()))? as u32;
        let height = v.get("height").and_then(Value::as_u64).ok_or_else(|| Error::Other("frame missing `height`".into()))? as u32;
        let png = base64_decode(png_b64).ok_or_else(|| Error::Other("source frame was not valid base64".into()))?;
        Ok((png, width, height))
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }
}

/// Decode standard (RFC 4648 padded) base64 without pulling a whole crate in.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn v(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    let bytes: Vec<u8> = input.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    let body = bytes.strip_suffix(b"==").or_else(|| bytes.strip_suffix(b"=")).unwrap_or(&bytes);
    let mut out = Vec::with_capacity(body.len() / 4 * 3 + 2);
    for c in body.chunks(4) {
        let mut n = 0u32;
        for (i, b) in c.iter().enumerate() {
            n |= v(*b)? << (18 - 6 * i);
        }
        for i in 0..c.len().saturating_sub(1) {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}
