use super::*;

#[test]
fn crc32_is_the_gzip_checksum() {
    assert_eq!(crc32(b""), 0);
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(b"The quick brown fox jumps over the lazy dog"), 0x414F_A339);
}

/// The signed empty frame AWS prints in its event-stream docs (Amazon Transcribe, "Setting up a
/// streaming transcription", WebSocket end frame): a timestamp header and a bytes header, no
/// payload. It proves the checksums and header types against bytes AWS produced.
#[test]
fn an_aws_documented_frame_decodes() {
    let b64 = "AAAAUwAAAEP1RHpYBTpkYXRlCAAAAWiXUkMLEDpjaHVuay1zaWduYXR1cmUGACCt6Zy+uymwEK2SrLp/zVBI5eGn83jdBwCaRUBJA+eaDafqjqI=";
    let bytes = decode_b64(b64);
    let messages = Decoder::new().push(&bytes).unwrap();
    assert_eq!(messages.len(), 1);
    let m = &messages[0];
    assert!(m.payload.is_empty());
    assert_eq!(m.headers[0].0, ":date");
    assert!(matches!(m.headers[0].1, HeaderValue::Timestamp(t) if t == 0x0000_0168_9752_430B));
    assert_eq!(m.headers[1].0, ":chunk-signature");
    assert!(matches!(&m.headers[1].1, HeaderValue::Bytes(b) if b.len() == 32));
}

fn decode_b64(s: &str) -> Vec<u8> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits = 0u32;
    let mut n = 0;
    let mut out = Vec::new();
    for c in s.bytes().filter(|c| *c != b'=') {
        bits = (bits << 6) | T.iter().position(|t| *t == c).unwrap() as u32;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
        }
    }
    out
}

fn event(kind: &str, payload: &str) -> Vec<u8> {
    encode(&[(":event-type", kind), (":content-type", "application/json"), (":message-type", "event")], payload.as_bytes())
}

#[test]
fn messages_split_anywhere_reassemble_in_order() {
    let mut stream = event("messageStart", r#"{"role":"assistant"}"#);
    stream.extend(event("contentBlockDelta", r#"{"contentBlockIndex":0,"delta":{"text":"Hi"}}"#));
    stream.extend(event("messageStop", r#"{"stopReason":"end_turn"}"#));
    for piece in [1, 2, 3, 7, 11, 12, 13, 64, stream.len()] {
        let mut d = Decoder::new();
        let mut got = Vec::new();
        for chunk in stream.chunks(piece) {
            got.extend(d.push(chunk).unwrap());
        }
        d.finish().unwrap();
        let kinds: Vec<_> = got.iter().map(|m| m.header(":event-type").unwrap().to_owned()).collect();
        assert_eq!(kinds, ["messageStart", "contentBlockDelta", "messageStop"], "pieces of {piece}");
        assert_eq!(got[1].payload, br#"{"contentBlockIndex":0,"delta":{"text":"Hi"}}"#);
    }
}

#[test]
fn every_header_type_is_read() {
    let mut h = Vec::new();
    let mut put = |name: &str, kind: u8, value: &[u8]| {
        h.push(name.len() as u8);
        h.extend_from_slice(name.as_bytes());
        h.push(kind);
        h.extend_from_slice(value);
    };
    put("t", 0, &[]);
    put("f", 1, &[]);
    put("b", 2, &[0xFF]);
    put("s", 3, &[0x01, 0x02]);
    put("i", 4, &[0, 0, 0, 5]);
    put("l", 5, &[0, 0, 0, 0, 0, 0, 0, 6]);
    put("y", 6, &[0, 2, 0xAB, 0xCD]);
    put("u", 9, &[7; 16]);
    let frame = raw_frame(&h, b"{}");
    let m = Decoder::new().push(&frame).unwrap().remove(0);
    let values: Vec<_> = m.headers.into_iter().map(|(_, v)| v).collect();
    assert_eq!(
        values,
        [
            HeaderValue::Bool(true),
            HeaderValue::Bool(false),
            HeaderValue::Byte(-1),
            HeaderValue::Short(0x0102),
            HeaderValue::Int(5),
            HeaderValue::Long(6),
            HeaderValue::Bytes(vec![0xAB, 0xCD]),
            HeaderValue::Uuid([7; 16]),
        ]
    );
}

/// A frame with raw header bytes and both checksums right.
fn raw_frame(headers: &[u8], payload: &[u8]) -> Vec<u8> {
    let total = (16 + headers.len() + payload.len()) as u32;
    let mut out = Vec::new();
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    let c = crc32(&out);
    out.extend_from_slice(&c.to_be_bytes());
    out.extend_from_slice(headers);
    out.extend_from_slice(payload);
    let c = crc32(&out);
    out.extend_from_slice(&c.to_be_bytes());
    out
}

/// A prelude claiming `total` and `headers` bytes, with a correct prelude checksum.
fn prelude(total: u32, headers: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&headers.to_be_bytes());
    let c = crc32(&out);
    out.extend_from_slice(&c.to_be_bytes());
    out
}

#[test]
fn corrupt_frames_are_named_errors() {
    let good = event("messageStop", r#"{"stopReason":"end_turn"}"#);
    // A flipped payload byte fails the message checksum.
    let mut bad = good.clone();
    let mid = bad.len() - 6;
    bad[mid] ^= 0x20;
    assert_eq!(Decoder::new().push(&bad).unwrap_err(), FrameError::MessageChecksum);
    // A flipped length byte fails the prelude checksum before the length is believed.
    let mut bad = good.clone();
    bad[3] ^= 0x01;
    assert_eq!(Decoder::new().push(&bad).unwrap_err(), FrameError::PreludeChecksum);
    // A wrong final checksum.
    let mut bad = good.clone();
    let last = bad.len() - 1;
    bad[last] ^= 0xFF;
    assert_eq!(Decoder::new().push(&bad).unwrap_err(), FrameError::MessageChecksum);
    // Garbage.
    assert_eq!(Decoder::new().push(b"data: {\"not\":\"an event stream\"}\n\n").unwrap_err(), FrameError::PreludeChecksum);
    // Errors are named and turn into the right kinds.
    assert_eq!(ProviderError::from(FrameError::MessageChecksum).kind, ErrorKind::Malformed);
    assert_eq!(ProviderError::from(FrameError::Truncated).kind, ErrorKind::Network);
}

#[test]
fn hostile_lengths_are_refused_before_anything_is_sized_by_them() {
    // 4 GiB claimed: refused from the 12-byte prelude alone, nothing more is waited for.
    assert_eq!(Decoder::new().push(&prelude(u32::MAX, 0)).unwrap_err(), FrameError::TooLarge);
    assert_eq!(Decoder::new().push(&prelude(MAX_MESSAGE_BYTES as u32 + 1, 0)).unwrap_err(), FrameError::TooLarge);
    assert_eq!(Decoder::new().push(&prelude(1024, MAX_HEADER_BYTES as u32 + 1)).unwrap_err(), FrameError::TooLarge);
    // Shorter than its own overhead, or headers longer than the message.
    assert_eq!(Decoder::new().push(&prelude(15, 0)).unwrap_err(), FrameError::BadLength);
    assert_eq!(Decoder::new().push(&prelude(0, 0)).unwrap_err(), FrameError::BadLength);
    assert_eq!(Decoder::new().push(&prelude(32, 17)).unwrap_err(), FrameError::BadLength);
    // Once broken, the stream stays broken.
    let mut d = Decoder::new();
    assert!(d.push(&prelude(u32::MAX, 0)).is_err());
    assert_eq!(d.push(&event("messageStart", "{}")).unwrap_err(), FrameError::TooLarge);
    assert_eq!(d.finish().unwrap_err(), FrameError::TooLarge);
}

#[test]
fn a_stream_cut_short_is_truncated() {
    let good = event("contentBlockDelta", r#"{"contentBlockIndex":0,"delta":{"text":"Hi"}}"#);
    for cut in [1, 5, 11, 12, 20, good.len() - 1] {
        let mut d = Decoder::new();
        assert!(d.push(&good[..cut]).unwrap().is_empty(), "nothing whole at {cut}");
        assert_eq!(d.finish().unwrap_err(), FrameError::Truncated, "cut at {cut}");
    }
    let mut d = Decoder::new();
    assert_eq!(d.push(&good).unwrap().len(), 1);
    d.finish().unwrap();
}

#[test]
fn malformed_headers_are_refused() {
    let cases: [&[u8]; 6] = [
        &[0],                      // empty name
        &[5, b'a'],                // name cut off
        &[1, b'a', 42],            // unknown type
        &[1, b'a', 7, 0, 9, b'x'], // string cut off
        &[1, b'a', 7, 0, 1, 0xFF], // string not UTF-8
        &[1, b'a', 0, 1, b'a', 1], // repeated name
    ];
    for h in cases {
        assert_eq!(Decoder::new().push(&raw_frame(h, b"")).unwrap_err(), FrameError::BadHeader, "{h:?}");
    }
}

/// Every prefix and every single-byte corruption of a real stream ends in a message, an error or
/// a wait, never a panic.
#[test]
fn corruptions_never_panic() {
    let mut stream = event("messageStart", r#"{"role":"assistant"}"#);
    stream.extend(event("messageStop", r#"{"stopReason":"end_turn"}"#));
    for i in 0..stream.len() {
        for flip in [0x01u8, 0x80, 0xFF] {
            let mut bad = stream.clone();
            bad[i] ^= flip;
            let mut d = Decoder::new();
            let _ = d.push(&bad);
            let _ = d.finish();
        }
        let mut d = Decoder::new();
        let _ = d.push(&stream[..i]);
        let _ = d.finish();
    }
}
