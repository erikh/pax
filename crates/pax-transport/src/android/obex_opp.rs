//! A minimal, dependency-free **OBEX Object Push** client.
//!
//! Android exposes Classic Bluetooth as an RFCOMM `BluetoothSocket` whose input and
//! output streams are byte pipes. OBEX Object Push (the "send a file" profile) is a
//! small request/response protocol layered on that pipe. Rather than reach for a
//! heavy OBEX crate (and to keep this auditable and *testable without a phone*),
//! the protocol is implemented here over any [`std::io::Read`] + [`std::io::Write`]
//! transport. The Android backend adapts a JNI socket's streams to that transport;
//! the unit tests drive it against an in-memory fake server.
//!
//! Only the subset Object Push needs is implemented: `CONNECT`, a chunked `PUT`
//! (Name + Length + Body / End-of-Body), and `DISCONNECT`.

use std::io::{self, Read, Write};

// OBEX request opcodes.
const OP_CONNECT: u8 = 0x80;
const OP_DISCONNECT: u8 = 0x81;
const OP_PUT: u8 = 0x02;
/// The "final" bit, OR'd into an opcode to mark the last packet of a request.
const FINAL: u8 = 0x80;

// OBEX response codes (low 7 bits; the final bit is also set on responses).
const RESP_CONTINUE: u8 = 0x90;
const RESP_SUCCESS: u8 = 0xA0;

// Header identifiers.
const HI_NAME: u8 = 0x01;
const HI_BODY: u8 = 0x48;
const HI_END_OF_BODY: u8 = 0x49;
const HI_LENGTH: u8 = 0xC3;

/// The OBEX protocol version this client speaks (1.0).
const OBEX_VERSION: u8 = 0x10;
/// The maximum OBEX packet size we advertise (and cap chunking at).
const OUR_MAX_PACKET: u16 = 0x2000;

/// An error from the OBEX exchange.
#[derive(Debug)]
pub(crate) enum ObexError {
    /// Underlying transport I/O failed.
    Io(io::Error),
    /// The peer returned a non-success response code at `stage`.
    Rejected {
        /// Which step failed (`"connect"`, `"put"`, `"disconnect"`).
        stage: &'static str,
        /// The OBEX response code the peer sent.
        code: u8,
    },
    /// A response was malformed (too short, bad length).
    Malformed(&'static str),
}

impl std::fmt::Display for ObexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObexError::Io(e) => write!(f, "obex i/o: {e}"),
            ObexError::Rejected { stage, code } => {
                write!(f, "obex {stage} rejected with code 0x{code:02X}")
            }
            ObexError::Malformed(w) => write!(f, "malformed obex response: {w}"),
        }
    }
}

impl From<io::Error> for ObexError {
    fn from(e: io::Error) -> Self {
        ObexError::Io(e)
    }
}

type Result<T> = std::result::Result<T, ObexError>;

/// Encode a string as the OBEX Name header payload: big-endian UTF-16, NUL
/// terminated.
fn name_to_utf16(name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() * 2 + 2);
    for unit in name.encode_utf16() {
        out.extend_from_slice(&unit.to_be_bytes());
    }
    out.extend_from_slice(&[0, 0]); // NUL terminator
    out
}

/// An OBEX Object Push client over a byte transport.
pub(crate) struct ObexOppClient<T> {
    io: T,
    peer_max_packet: u16,
}

impl<T: Read + Write> ObexOppClient<T> {
    /// Perform the OBEX `CONNECT` handshake and return a ready client.
    pub(crate) fn connect(mut io: T) -> Result<Self> {
        // CONNECT body: version, flags, max-packet (BE). No Target header for OPP.
        let mut packet = vec![OP_CONNECT, 0, 0, OBEX_VERSION, 0x00];
        packet.extend_from_slice(&OUR_MAX_PACKET.to_be_bytes());
        put_len(&mut packet);
        io.write_all(&packet)?;
        io.flush()?;

        let resp = read_response(&mut io)?;
        if resp.code != RESP_SUCCESS {
            return Err(ObexError::Rejected {
                stage: "connect",
                code: resp.code,
            });
        }
        // CONNECT response body: version, flags, max-packet (BE).
        let peer_max_packet = if resp.body.len() >= 4 {
            u16::from_be_bytes([resp.body[2], resp.body[3]])
        } else {
            255 // conservative floor
        };
        Ok(ObexOppClient {
            io,
            peer_max_packet: peer_max_packet.max(255),
        })
    }

    /// The negotiated maximum OBEX packet size (min of ours and the peer's).
    fn max_packet(&self) -> usize {
        OUR_MAX_PACKET.min(self.peer_max_packet) as usize
    }

    /// Push one object. `on_progress` is called with the cumulative byte count as
    /// each Body chunk is acknowledged.
    pub(crate) fn put_file(
        &mut self,
        name: &str,
        data: &[u8],
        mut on_progress: impl FnMut(u64),
    ) -> Result<()> {
        let name_payload = name_to_utf16(name);

        // The first PUT packet carries the Name and Length headers; remaining
        // budget in that packet (and every later one) carries Body bytes.
        let max = self.max_packet();
        // Per-packet fixed overhead: 3-byte packet header + 3-byte Body header.
        let body_overhead = 3 + 3;
        // First packet additionally carries Name (3 + payload) and Length (5).
        let first_extra = (3 + name_payload.len()) + 5;

        let mut sent: u64 = 0;
        let total = data.len();
        let mut offset = 0usize;
        let mut first = true;

        loop {
            let header_budget = if first {
                body_overhead + first_extra
            } else {
                body_overhead
            };
            let chunk_cap = max.saturating_sub(header_budget).max(1);
            let remaining = total - offset;
            let take = remaining.min(chunk_cap);
            let is_last = offset + take >= total;

            let mut packet = vec![if is_last { OP_PUT | FINAL } else { OP_PUT }, 0, 0];
            if first {
                // Name header.
                packet.push(HI_NAME);
                let nlen = (3 + name_payload.len()) as u16;
                packet.extend_from_slice(&nlen.to_be_bytes());
                packet.extend_from_slice(&name_payload);
                // Length header (4-byte total object size).
                packet.push(HI_LENGTH);
                packet.extend_from_slice(&(total as u32).to_be_bytes());
            }
            // Body / End-of-Body header.
            let hi = if is_last { HI_END_OF_BODY } else { HI_BODY };
            packet.push(hi);
            let blen = (3 + take) as u16;
            packet.extend_from_slice(&blen.to_be_bytes());
            packet.extend_from_slice(&data[offset..offset + take]);
            put_len(&mut packet);

            self.io.write_all(&packet)?;
            self.io.flush()?;

            let resp = read_response(&mut self.io)?;
            let expected = if is_last { RESP_SUCCESS } else { RESP_CONTINUE };
            if resp.code != expected {
                return Err(ObexError::Rejected {
                    stage: "put",
                    code: resp.code,
                });
            }

            offset += take;
            sent += take as u64;
            on_progress(sent);
            first = false;
            if is_last {
                break;
            }
        }
        Ok(())
    }

    /// Send `DISCONNECT` and consume the client.
    pub(crate) fn disconnect(mut self) -> Result<()> {
        let packet = vec![OP_DISCONNECT, 0x00, 0x03];
        self.io.write_all(&packet)?;
        self.io.flush()?;
        let resp = read_response(&mut self.io)?;
        if resp.code != RESP_SUCCESS {
            return Err(ObexError::Rejected {
                stage: "disconnect",
                code: resp.code,
            });
        }
        Ok(())
    }
}

/// Back-patch the 2-byte big-endian length field (bytes 1..3) of a packet.
fn put_len(packet: &mut [u8]) {
    let len = packet.len() as u16;
    packet[1..3].copy_from_slice(&len.to_be_bytes());
}

/// A parsed OBEX response: its code and the bytes after the 3-byte header.
struct Response {
    code: u8,
    body: Vec<u8>,
}

/// Read exactly one OBEX response packet.
fn read_response(io: &mut impl Read) -> Result<Response> {
    let mut head = [0u8; 3];
    io.read_exact(&mut head)?;
    let code = head[0];
    let len = u16::from_be_bytes([head[1], head[2]]) as usize;
    if len < 3 {
        return Err(ObexError::Malformed("length < 3"));
    }
    let mut body = vec![0u8; len - 3];
    io.read_exact(&mut body)?;
    Ok(Response { code, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A fake OBEX server: serves a fixed sequence of response bytes for reads, and
    /// captures everything the client writes.
    struct Fake {
        responses: Cursor<Vec<u8>>,
        sent: Vec<u8>,
    }
    impl Read for Fake {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.responses.read(buf)
        }
    }
    impl Write for Fake {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.sent.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn connect_resp() -> Vec<u8> {
        // code, len(2), version, flags, max-packet(2) == 0x0400 (1024)
        vec![RESP_SUCCESS, 0x00, 0x07, OBEX_VERSION, 0x00, 0x04, 0x00]
    }
    fn ok() -> Vec<u8> {
        vec![RESP_SUCCESS, 0x00, 0x03]
    }
    fn cont() -> Vec<u8> {
        vec![RESP_CONTINUE, 0x00, 0x03]
    }

    #[test]
    fn small_object_is_a_single_final_put() {
        let mut responses = connect_resp();
        responses.extend(ok()); // single final PUT
        responses.extend(ok()); // disconnect
        let fake = Fake {
            responses: Cursor::new(responses),
            sent: Vec::new(),
        };

        let mut client = ObexOppClient::connect(fake).expect("connect");
        let mut progress = Vec::new();
        client
            .put_file("note.txt", b"hello", |n| progress.push(n))
            .expect("put");
        client.disconnect().expect("disconnect");

        // The whole payload was acknowledged in one shot.
        assert_eq!(progress, vec![5]);
    }

    #[test]
    fn large_object_is_chunked_with_continue() {
        // Peer max-packet is 1024 (from connect_resp), so a 4 KiB payload spans
        // several packets. We serve plenty of CONTINUE and no SUCCESS: the final
        // packet then errors, which is fine — we only assert that the payload was
        // split into multiple Body chunks (one progress tick each) beforehand.
        let mut responses = connect_resp();
        for _ in 0..32 {
            responses.extend(cont());
        }
        let fake = Fake {
            responses: Cursor::new(responses),
            sent: Vec::new(),
        };
        let mut client = ObexOppClient::connect(fake).expect("connect");
        let data = vec![0xABu8; 4096];
        let mut ticks = 0u32;
        let _ = client.put_file("big.bin", &data, |_| ticks += 1);
        assert!(ticks >= 2, "expected multiple body chunks, got {ticks}");
    }

    #[test]
    fn rejection_surfaces() {
        let mut responses = vec![0xC0, 0x00, 0x03]; // 0xC0 == Bad Request
        responses.extend(ok());
        let fake = Fake {
            responses: Cursor::new(responses),
            sent: Vec::new(),
        };
        match ObexOppClient::connect(fake) {
            Err(ObexError::Rejected { stage, code }) => {
                assert_eq!(stage, "connect");
                assert_eq!(code, 0xC0);
            }
            Err(other) => panic!("unexpected error: {other}"),
            Ok(_) => panic!("expected rejection"),
        }
    }

    #[test]
    fn utf16_name_is_nul_terminated_big_endian() {
        let p = name_to_utf16("AB");
        assert_eq!(p, vec![0x00, 0x41, 0x00, 0x42, 0x00, 0x00]);
    }
}
