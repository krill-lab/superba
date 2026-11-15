//! Per-message deflate (RFC 7692) on the router's upstream WebSocket leg.
//!
//! The relay ([`crate::ws_relay`]) reads and rewrites every frame, and
//! tokio-tungstenite 0.30 has no extension support (it fails a frame with a
//! reserved bit set). So the extension lives below it, in [`DeflateIo`]: the
//! member's frames are inflated before tungstenite reads them, and the
//! router's frames are deflated after tungstenite wrote them. The relay above
//! sees plain frames, exactly as on a socket without the extension.
//!
//! - **Negotiation** is the member's choice: the handshake offers
//!   [`OFFER`] (no `client_max_window_bits`, so a member may not shrink the
//!   router's window, which miniz cannot honour) and [`negotiated`] reads the
//!   101. A member that does not answer with the extension gets plain frames,
//!   so offering it is always safe.
//! - **Member → router**: a data message whose first frame has RSV1 set is
//!   inflated (fragments too; the window is kept across messages unless the
//!   member announced `server_no_context_takeover`). Control frames and
//!   uncompressed messages pass as they are. A message that inflates beyond
//!   the relay's message limit is turned into a frame header announcing more
//!   than the limit, so tungstenite fails it with "message too long" and the
//!   relay closes both legs with 1009, as for any message over the limit.
//! - **Router → member**: an unfragmented text or binary message of at least
//!   [`MIN_DEFLATE_BYTES`] is deflated (level [`DEFLATE_LEVEL`], context
//!   kept unless `client_no_context_takeover` was agreed) when it shrinks;
//!   otherwise it goes as it is and the compressor starts afresh (a fresh
//!   compressor only refers to data the member has seen). The frame keeps the
//!   mask key tungstenite chose for it.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, ready};

use bytes::{Buf, BufMut, BytesMut};
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
use http::HeaderMap;
use http::header::SEC_WEBSOCKET_EXTENSIONS;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// The extension offered on the router's upstream handshakes.
pub(crate) const OFFER: &str = "permessage-deflate";

/// Smaller messages go uncompressed (deflate gains nothing worth the CPU).
pub(crate) const MIN_DEFLATE_BYTES: usize = 512;

/// Deflate level of the router's messages (fast; Responses JSON shrinks
/// about as much as with zstd level 1).
pub(crate) const DEFLATE_LEVEL: u32 = 3;

/// The empty stored block that ends each compressed message (RFC 7692
/// 7.2.1: removed by the sender, appended again by the receiver).
const TAIL: [u8; 4] = [0x00, 0x00, 0xff, 0xff];

/// Above this many bytes waiting for the member, writes wait.
const HIGH_WATER: usize = 1 << 20;

/// What the member agreed to in its 101.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Params {
    /// The router compresses its messages (off when the member limited the
    /// router's window or answered parameters the router does not know; its
    /// own messages are still inflated).
    pub compress: bool,
    /// `client_no_context_takeover`: the router's compressor starts afresh
    /// for each message.
    pub client_reset: bool,
    /// `server_no_context_takeover`: the member's messages never refer to
    /// earlier ones (the inflater is reset after each).
    pub server_reset: bool,
}

/// The extension the member's 101 agreed to: `None` when it answered without
/// `permessage-deflate` (frames are plain). Other extensions are ignored (the
/// router offered none; a frame using one fails in tungstenite as before).
pub(crate) fn negotiated(headers: &HeaderMap) -> Option<Params> {
    let mut found = None;
    for value in headers.get_all(SEC_WEBSOCKET_EXTENSIONS) {
        let Ok(value) = value.to_str() else { continue };
        for ext in value.split(',') {
            let mut parts = ext.split(';').map(str::trim);
            if !parts
                .next()
                .is_some_and(|n| n.eq_ignore_ascii_case("permessage-deflate"))
            {
                continue;
            }
            let mut p = Params {
                compress: true,
                client_reset: false,
                server_reset: false,
            };
            for param in parts.filter(|s| !s.is_empty()) {
                let (name, val) = match param.split_once('=') {
                    Some((n, v)) => (n.trim(), Some(v.trim().trim_matches('"'))),
                    None => (param, None),
                };
                match name.to_ascii_lowercase().as_str() {
                    "server_no_context_takeover" => p.server_reset = true,
                    "client_no_context_takeover" => p.client_reset = true,
                    // Any smaller window of the member's is read by a full one.
                    "server_max_window_bits" => {}
                    // Not offered; only the full window can be honoured.
                    "client_max_window_bits" if val.is_none_or(|v| v == "15") => {}
                    _ => p.compress = false,
                }
            }
            // One agreement only; a second would be a protocol error.
            found.get_or_insert(p);
        }
    }
    found
}

/// A WebSocket frame header.
#[derive(Clone, Copy, Debug)]
struct Header {
    fin: bool,
    rsv1: bool,
    /// RSV2 or RSV3.
    rsv_other: bool,
    opcode: u8,
    mask: Option<[u8; 4]>,
    /// Bytes of the header itself.
    len: usize,
    payload: u64,
}

impl Header {
    /// The header at the start of `buf`; `None` until it is complete.
    fn parse(buf: &[u8]) -> Option<Self> {
        let (&b0, rest) = buf.split_first()?;
        let (&b1, rest) = rest.split_first()?;
        let (payload, mut len) = match b1 & 0x7f {
            126 => (
                u64::from(u16::from_be_bytes(rest.get(..2)?.try_into().ok()?)),
                4,
            ),
            127 => (u64::from_be_bytes(rest.get(..8)?.try_into().ok()?), 10),
            n => (u64::from(n), 2),
        };
        let mask = if b1 & 0x80 != 0 {
            let key: [u8; 4] = buf.get(len..len + 4)?.try_into().ok()?;
            len += 4;
            Some(key)
        } else {
            None
        };
        Some(Self {
            fin: b0 & 0x80 != 0,
            rsv1: b0 & 0x40 != 0,
            rsv_other: b0 & 0x30 != 0,
            opcode: b0 & 0x0f,
            mask,
            len,
            payload,
        })
    }

    fn is_control(&self) -> bool {
        self.opcode & 0x08 != 0
    }
}

fn put_header(
    out: &mut BytesMut,
    fin: bool,
    rsv1: bool,
    opcode: u8,
    mask: Option<[u8; 4]>,
    len: u64,
) {
    out.put_u8(u8::from(fin) << 7 | u8::from(rsv1) << 6 | opcode);
    let m = if mask.is_some() { 0x80 } else { 0 };
    match len {
        0..=125 => out.put_u8(m | len as u8),
        126..=0xffff => {
            out.put_u8(m | 126);
            out.put_u16(len as u16);
        }
        _ => {
            out.put_u8(m | 127);
            out.put_u64(len);
        }
    }
    if let Some(key) = mask {
        out.put_slice(&key);
    }
}

fn apply_mask(data: &mut [u8], key: [u8; 4]) {
    for (i, b) in data.iter_mut().enumerate() {
        *b ^= key[i & 3];
    }
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("permessage-deflate: {what}"),
    )
}

/// Inflate `input` onto `out`, at most `limit` bytes in all; `Err(None)` when
/// the output would exceed it.
fn inflate(
    d: &mut Decompress,
    mut input: &[u8],
    out: &mut Vec<u8>,
    limit: usize,
) -> Result<(), Option<io::Error>> {
    loop {
        if out.capacity() - out.len() < 8 * 1024 {
            out.reserve((input.len() * 4).clamp(16 * 1024, 1 << 20));
        }
        let (in0, out0) = (d.total_in(), d.total_out());
        let status = d
            .decompress_vec(input, out, FlushDecompress::Sync)
            .map_err(|e| Some(invalid(&e.to_string())))?;
        let used = usize::try_from(d.total_in() - in0).unwrap_or(usize::MAX);
        input = &input[used.min(input.len())..];
        if out.len() > limit {
            return Err(None);
        }
        let stalled = used == 0 && d.total_out() == out0;
        // A final block ends the stream: nothing after it belongs to the message.
        if status == Status::StreamEnd || (input.is_empty() && out.len() < out.capacity()) {
            return Ok(());
        }
        if stalled {
            return Err(Some(invalid("inflate made no progress")));
        }
    }
}

/// Deflate one whole message with a sync flush, without [`TAIL`].
fn deflate(c: &mut Compress, input: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 3 + 64);
    let mut rest = input;
    loop {
        if out.capacity() - out.len() < 1024 {
            out.reserve(out.capacity().max(4096));
        }
        let before = c.total_in();
        c.compress_vec(rest, &mut out, FlushCompress::Sync)
            .map_err(|e| invalid(&e.to_string()))?;
        let used = usize::try_from(c.total_in() - before).unwrap_or(usize::MAX);
        rest = &rest[used.min(rest.len())..];
        // Done when all input is in and the flush fitted (spare room left).
        if rest.is_empty() && out.len() < out.capacity() {
            break;
        }
    }
    if out.ends_with(&TAIL) {
        out.truncate(out.len() - TAIL.len());
    }
    Ok(out)
}

/// Read-side state.
struct Reader {
    /// One read from the member's socket.
    scratch: Box<[u8]>,
    raw: BytesMut,
    plain: BytesMut,
    inflater: Decompress,
    /// Inside a compressed (fragmented) message.
    inflating: bool,
    /// Bytes inflated for the current message.
    inflated: usize,
    /// The member's stream ended.
    eof: bool,
    /// Everything from here on passes as it is (a frame tungstenite will
    /// refuse, or the stream after an oversize message).
    raw_rest: bool,
}

/// Write-side state.
struct Writer {
    frames: BytesMut,
    wire: BytesMut,
    deflater: Compress,
}

struct Codec {
    params: Params,
    /// The relay's message limit (also bounds a single frame here).
    limit: usize,
    max_frame: usize,
    r: Reader,
    w: Writer,
    /// Payload bytes of the router's data frames as sent (after deflate).
    wire_bytes: Arc<AtomicU64>,
}

/// The member's upgraded socket, with per-message deflate when it was
/// agreed (`codec`), else passed straight through.
pub(crate) struct DeflateIo<S> {
    inner: S,
    codec: Option<Box<Codec>>,
}

impl<S> DeflateIo<S> {
    /// Plain frames.
    pub(crate) fn plain(inner: S) -> Self {
        Self { inner, codec: None }
    }

    /// Deflate as agreed in `params`; messages up to `limit` bytes, frames up
    /// to `max_frame`. The payload bytes of the router's data frames as sent
    /// are added to `wire_bytes`.
    pub(crate) fn deflate(
        inner: S,
        params: Params,
        limit: usize,
        max_frame: usize,
        wire_bytes: Arc<AtomicU64>,
    ) -> Self {
        Self {
            inner,
            codec: Some(Box::new(Codec {
                params,
                limit,
                max_frame,
                r: Reader {
                    scratch: vec![0; 16 * 1024].into_boxed_slice(),
                    raw: BytesMut::new(),
                    plain: BytesMut::new(),
                    inflater: Decompress::new(false),
                    inflating: false,
                    inflated: 0,
                    eof: false,
                    raw_rest: false,
                },
                w: Writer {
                    frames: BytesMut::new(),
                    wire: BytesMut::new(),
                    deflater: Compress::new(Compression::new(DEFLATE_LEVEL), false),
                },
                wire_bytes,
            })),
        }
    }

    /// Per-message deflate is in use.
    pub(crate) fn is_deflate(&self) -> bool {
        self.codec.is_some()
    }
}

impl Codec {
    /// Turn the complete frames of `r.raw` into plain ones on `r.plain`.
    fn decode(&mut self) -> io::Result<()> {
        let r = &mut self.r;
        while !r.raw.is_empty() {
            if r.raw_rest {
                let all = r.raw.split();
                r.plain.extend_from_slice(&all);
                return Ok(());
            }
            let Some(h) = Header::parse(&r.raw) else {
                return Ok(());
            };
            // tungstenite refuses these itself (too big, masked, unknown
            // bits): pass them on and step aside.
            let compressed_part =
                (h.rsv1 && matches!(h.opcode, 1 | 2)) || (r.inflating && h.opcode == 0 && !h.rsv1);
            if h.payload > self.max_frame as u64
                || h.mask.is_some()
                || h.rsv_other
                || (h.rsv1 && !compressed_part)
            {
                r.raw_rest = true;
                continue;
            }
            let total = h.len + h.payload as usize;
            if r.raw.len() < total {
                r.raw.reserve(total - r.raw.len());
                return Ok(());
            }
            let frame = r.raw.split_to(total);
            if !compressed_part {
                if !h.is_control() {
                    // A plain message (or its fragment) between compressed ones.
                    r.inflating = false;
                }
                r.plain.extend_from_slice(&frame);
                continue;
            }
            if h.opcode != 0 {
                r.inflating = true;
                r.inflated = 0;
            }
            let mut out = Vec::new();
            let budget = self.limit - r.inflated.min(self.limit);
            let mut res = inflate(&mut r.inflater, &frame[h.len..], &mut out, budget);
            if res.is_ok() && h.fin {
                res = inflate(&mut r.inflater, &TAIL, &mut out, budget);
            }
            match res {
                Ok(()) => {}
                Err(None) => {
                    // Over the limit: a header tungstenite fails with
                    // "message too long", then nothing more.
                    put_header(
                        &mut r.plain,
                        true,
                        false,
                        h.opcode.max(1),
                        None,
                        self.limit as u64 + 1,
                    );
                    r.raw_rest = true;
                    r.raw.clear();
                    return Ok(());
                }
                Err(Some(e)) => return Err(e),
            }
            r.inflated += out.len();
            put_header(&mut r.plain, h.fin, false, h.opcode, None, out.len() as u64);
            r.plain.extend_from_slice(&out);
            if h.fin {
                r.inflating = false;
                if self.params.server_reset {
                    r.inflater.reset(false);
                }
            }
        }
        Ok(())
    }

    /// Turn the complete frames tungstenite wrote into what goes on the wire.
    fn encode(&mut self) -> io::Result<()> {
        let w = &mut self.w;
        while let Some(h) = Header::parse(&w.frames) {
            let total = h.len + usize::try_from(h.payload).unwrap_or(usize::MAX);
            if w.frames.len() < total {
                return Ok(());
            }
            let frame = w.frames.split_to(total);
            let data = !h.is_control();
            let eligible = self.params.compress
                && data
                && h.fin
                && matches!(h.opcode, 1 | 2)
                && !h.rsv1
                && !h.rsv_other
                && h.payload as usize >= MIN_DEFLATE_BYTES;
            if !eligible {
                if data {
                    self.wire_bytes.fetch_add(h.payload, Ordering::Relaxed);
                }
                w.wire.extend_from_slice(&frame);
                continue;
            }
            let mut payload = frame[h.len..].to_vec();
            if let Some(key) = h.mask {
                apply_mask(&mut payload, key);
            }
            let mut packed = deflate(&mut w.deflater, &payload)?;
            if packed.len() >= payload.len() {
                // Not worth it: sent as it is; the next message starts afresh.
                w.deflater.reset();
                self.wire_bytes.fetch_add(h.payload, Ordering::Relaxed);
                w.wire.extend_from_slice(&frame);
                continue;
            }
            if self.params.client_reset {
                w.deflater.reset();
            }
            if let Some(key) = h.mask {
                apply_mask(&mut packed, key);
            }
            self.wire_bytes
                .fetch_add(packed.len() as u64, Ordering::Relaxed);
            put_header(
                &mut w.wire,
                true,
                true,
                h.opcode,
                h.mask,
                packed.len() as u64,
            );
            w.wire.extend_from_slice(&packed);
        }
        Ok(())
    }
}

impl<S: AsyncWrite + Unpin> DeflateIo<S> {
    /// Write what is waiting for the member; `Ready` once nothing is.
    fn drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let Some(codec) = self.codec.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        while !codec.w.wire.is_empty() {
            let n = ready!(Pin::new(&mut self.inner).poll_write(cx, &codec.w.wire))?;
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            codec.w.wire.advance(n);
        }
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for DeflateIo<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let Some(codec) = this.codec.as_mut() else {
            return Pin::new(&mut this.inner).poll_read(cx, buf);
        };
        loop {
            if !codec.r.plain.is_empty() {
                let n = codec.r.plain.len().min(buf.remaining());
                buf.put_slice(&codec.r.plain[..n]);
                codec.r.plain.advance(n);
                return Poll::Ready(Ok(()));
            }
            if codec.r.eof {
                return Poll::Ready(Ok(()));
            }
            let r = &mut codec.r;
            let mut rb = ReadBuf::new(&mut r.scratch);
            ready!(Pin::new(&mut this.inner).poll_read(cx, &mut rb))?;
            let n = rb.filled().len();
            if n == 0 {
                // What is left is an incomplete frame: tungstenite reports it.
                r.eof = true;
                let rest = r.raw.split();
                r.plain.extend_from_slice(&rest);
                continue;
            }
            r.raw.extend_from_slice(&r.scratch[..n]);
            codec.decode()?;
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for DeflateIo<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.codec.is_none() {
            return Pin::new(&mut this.inner).poll_write(cx, buf);
        }
        // Waiting bytes go first; beyond the high-water mark the writer waits.
        if let Poll::Ready(r) = this.drain(cx) {
            r?;
        }
        let codec = this.codec.as_mut().expect("deflate codec");
        if codec.w.wire.len() >= HIGH_WATER {
            return Poll::Pending;
        }
        codec.w.frames.extend_from_slice(buf);
        codec.encode()?;
        if let Poll::Ready(Err(e)) = this.drain(cx) {
            return Poll::Ready(Err(e));
        }
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.drain(cx))?;
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.drain(cx))?;
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use http::HeaderValue;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};

    fn ext(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(SEC_WEBSOCKET_EXTENSIONS, HeaderValue::from_str(v).unwrap());
        h
    }

    #[test]
    fn negotiation_reads_the_members_answer() {
        assert_eq!(negotiated(&HeaderMap::new()), None);
        assert_eq!(negotiated(&ext("x-webkit-deflate-frame")), None);
        let plain = Params {
            compress: true,
            client_reset: false,
            server_reset: false,
        };
        assert_eq!(negotiated(&ext("permessage-deflate")), Some(plain));
        assert_eq!(
            negotiated(&ext(
                "permessage-deflate; server_no_context_takeover; client_no_context_takeover; server_max_window_bits=10"
            )),
            Some(Params {
                compress: true,
                client_reset: true,
                server_reset: true,
            })
        );
        // A window the router's compressor cannot keep to: inflate only.
        let small = negotiated(&ext("permessage-deflate; client_max_window_bits=9")).unwrap();
        assert!(!small.compress);
        assert!(
            negotiated(&ext("permessage-deflate; client_max_window_bits=\"15\""))
                .unwrap()
                .compress
        );
        assert!(
            !negotiated(&ext("permessage-deflate; mystery"))
                .unwrap()
                .compress
        );
    }

    /// Raw deflate of `data` the way a member compresses (sync flush, tail
    /// removed), with its own state.
    fn member_deflate(c: &mut Compress, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 64);
        c.compress_vec(data, &mut out, FlushCompress::Sync).unwrap();
        assert!(out.ends_with(&TAIL));
        out.truncate(out.len() - 4);
        out
    }

    fn frame(fin: bool, rsv1: bool, opcode: u8, mask: Option<[u8; 4]>, payload: &[u8]) -> Vec<u8> {
        let mut out = BytesMut::new();
        put_header(&mut out, fin, rsv1, opcode, mask, payload.len() as u64);
        let mut p = payload.to_vec();
        if let Some(k) = mask {
            apply_mask(&mut p, k);
        }
        out.extend_from_slice(&p);
        out.to_vec()
    }

    fn body(n: usize) -> String {
        let mut s = String::from("{\"type\":\"response.create\",\"input\":[");
        let mut i = 0;
        while s.len() < n {
            s.push_str(&format!("{{\"type\":\"message\",\"role\":\"user\",\"content\":\"line {i} of the conversation\"}},"));
            i += 1;
        }
        s.push_str("{}]}");
        s
    }

    fn config() -> WebSocketConfig {
        WebSocketConfig::default()
            .max_message_size(Some(1 << 20))
            .max_frame_size(Some(1 << 20))
    }

    async fn client(
        params: Params,
    ) -> (
        WebSocketStream<DeflateIo<tokio::io::DuplexStream>>,
        tokio::io::DuplexStream,
        Arc<AtomicU64>,
    ) {
        let (a, b) = tokio::io::duplex(1 << 22);
        let wire = Arc::new(AtomicU64::new(0));
        let io = DeflateIo::deflate(a, params, 1 << 20, 1 << 20, wire.clone());
        let ws = WebSocketStream::from_raw_socket(io, Role::Client, Some(config())).await;
        (ws, b, wire)
    }

    const TAKEOVER: Params = Params {
        compress: true,
        client_reset: false,
        server_reset: false,
    };

    /// Read one whole frame the router sent (masked) from the raw side.
    async fn read_frame(raw: &mut tokio::io::DuplexStream) -> (Header, Vec<u8>) {
        let mut buf = Vec::new();
        loop {
            if let Some(h) = Header::parse(&buf)
                && buf.len() >= h.len + h.payload as usize
            {
                let mut p = buf[h.len..h.len + h.payload as usize].to_vec();
                if let Some(k) = h.mask {
                    apply_mask(&mut p, k);
                }
                return (h, p);
            }
            let mut chunk = [0u8; 4096];
            let n = raw.read(&mut chunk).await.unwrap();
            assert!(n > 0, "frame cut short");
            buf.extend_from_slice(&chunk[..n]);
        }
    }

    #[tokio::test]
    async fn the_routers_messages_are_deflated_and_inflate_back() {
        let (mut ws, mut raw, wire) = client(TAKEOVER).await;
        let mut member = Decompress::new(false);
        let mut sent = 0u64;
        for n in [4096, 20_000, 4096] {
            let text = body(n);
            ws.send(Message::text(text.clone())).await.unwrap();
            let (h, payload) = read_frame(&mut raw).await;
            assert!(h.rsv1 && h.fin && h.opcode == 1 && h.mask.is_some());
            assert!(
                payload.len() < text.len() / 4,
                "{} -> {}",
                text.len(),
                payload.len()
            );
            sent += payload.len() as u64;
            let mut input = payload.clone();
            input.extend_from_slice(&TAIL);
            let mut out = Vec::new();
            inflate(&mut member, &input, &mut out, usize::MAX).unwrap();
            assert_eq!(String::from_utf8(out).unwrap(), text);
        }
        assert_eq!(wire.load(Ordering::Relaxed), sent);
        // Small messages and control frames pass as they are.
        ws.send(Message::text("{\"type\":\"ping\"}")).await.unwrap();
        let (h, payload) = read_frame(&mut raw).await;
        assert!(!h.rsv1);
        assert_eq!(payload, b"{\"type\":\"ping\"}");
        ws.send(Message::Ping(vec![1, 2, 3].into())).await.unwrap();
        let (h, payload) = read_frame(&mut raw).await;
        assert!(!h.rsv1 && h.opcode == 9);
        assert_eq!(payload, [1, 2, 3]);
    }

    #[tokio::test]
    async fn incompressible_messages_go_as_they_are_and_the_context_restarts() {
        let (mut ws, mut raw, _) = client(TAKEOVER).await;
        // Bytes that do not shrink (a xorshift stream).
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let noise: Vec<u8> = (0..4096)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        ws.send(Message::binary(noise.clone())).await.unwrap();
        let (h, payload) = read_frame(&mut raw).await;
        assert!(!h.rsv1);
        assert_eq!(payload, noise);
        // The next compressed message must not refer to the skipped one.
        let text = body(4096);
        ws.send(Message::text(text.clone())).await.unwrap();
        let (h, payload) = read_frame(&mut raw).await;
        assert!(h.rsv1);
        let mut d = Decompress::new(false);
        let mut out = Vec::new();
        let mut input = payload;
        input.extend_from_slice(&TAIL);
        inflate(&mut d, &input, &mut out, usize::MAX).unwrap();
        assert_eq!(out, text.as_bytes());
    }

    #[tokio::test]
    async fn the_members_messages_are_inflated_with_context_and_fragments() {
        let (mut ws, mut raw, _) = client(TAKEOVER).await;
        let mut c = Compress::new(Compression::default(), false);
        // Two messages sharing context, then one in three fragments with a
        // ping between them, then a plain one.
        let one = body(3000);
        let two = body(3500);
        raw.write_all(&frame(
            true,
            true,
            1,
            None,
            &member_deflate(&mut c, one.as_bytes()),
        ))
        .await
        .unwrap();
        raw.write_all(&frame(
            true,
            true,
            1,
            None,
            &member_deflate(&mut c, two.as_bytes()),
        ))
        .await
        .unwrap();
        let three = body(9000);
        let packed = member_deflate(&mut c, three.as_bytes());
        let (a, rest) = packed.split_at(packed.len() / 3);
        let (b, cc) = rest.split_at(rest.len() / 2);
        raw.write_all(&frame(false, true, 1, None, a))
            .await
            .unwrap();
        raw.write_all(&frame(true, false, 9, None, b"hi"))
            .await
            .unwrap();
        raw.write_all(&frame(false, false, 0, None, b))
            .await
            .unwrap();
        raw.write_all(&frame(true, false, 0, None, cc))
            .await
            .unwrap();
        raw.write_all(&frame(true, false, 1, None, b"plain"))
            .await
            .unwrap();
        let mut got = Vec::new();
        while got.len() < 5 {
            match ws.next().await.unwrap().unwrap() {
                Message::Text(t) => got.push(t.as_str().to_owned()),
                Message::Ping(p) => got.push(format!("ping:{}", String::from_utf8_lossy(&p))),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(got[0], one);
        assert_eq!(got[1], two);
        assert_eq!(got[2], "ping:hi");
        assert_eq!(got[3], three);
        assert_eq!(got[4], "plain");
    }

    #[tokio::test]
    async fn an_inflated_message_over_the_limit_fails_as_too_long() {
        let (a, mut raw) = tokio::io::duplex(1 << 20);
        let io = DeflateIo::deflate(a, TAKEOVER, 64 * 1024, 64 * 1024, Arc::default());
        let mut ws = WebSocketStream::from_raw_socket(
            io,
            Role::Client,
            Some(
                WebSocketConfig::default()
                    .max_message_size(Some(64 * 1024))
                    .max_frame_size(Some(64 * 1024)),
            ),
        )
        .await;
        // 1 MiB of spaces deflates to about a kilobyte.
        let mut c = Compress::new(Compression::default(), false);
        let bomb = member_deflate(&mut c, &vec![b' '; 1 << 20]);
        assert!(bomb.len() < 8 * 1024);
        raw.write_all(&frame(true, true, 1, None, &bomb))
            .await
            .unwrap();
        let err = ws.next().await.unwrap().unwrap_err();
        assert!(
            matches!(
                err,
                tokio_tungstenite::tungstenite::Error::Capacity(
                    tokio_tungstenite::tungstenite::error::CapacityError::MessageTooLong { .. }
                )
            ),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn without_the_extension_bytes_pass_straight_through() {
        let (a, mut raw) = tokio::io::duplex(1 << 16);
        let mut io = DeflateIo::plain(a);
        assert!(!io.is_deflate());
        io.write_all(b"abc").await.unwrap();
        let mut buf = [0u8; 3];
        raw.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"abc");
        raw.write_all(&frame(true, true, 1, None, b"xyz"))
            .await
            .unwrap();
        let mut got = [0u8; 5];
        io.read_exact(&mut got).await.unwrap();
        assert_eq!(got, frame(true, true, 1, None, b"xyz")[..]);
    }
}
