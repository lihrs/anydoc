//! Minimal base64 encoding: standard alphabet with `=` padding.

/// Base64-encode `bytes` using the standard alphabet with `=` padding. Used
/// for data URIs when embedding image assets in Markdown output.
pub fn encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Base64-decode `text` using the standard alphabet. Returns `None` on any
/// invalid character, a malformed length, or a non-zero tail-group padding.
/// Used to recover image bytes from an embedded `data:` URI.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    fn decode_val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let bytes = text.as_bytes();
    // Strip a trailing `=` pad, at most two.
    let mut pad = 0;
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == b'=' {
        pad += 1;
        end -= 1;
    }
    if pad > 2 {
        return None;
    }
    // A padded group must be a full-length root (len%4 must be 0 after pad).
    if (end + pad) % 4 != 0 {
        return None;
    }

    let mut out = Vec::with_capacity(end / 4 * 3);
    let mut acc: u32 = 0;
    let mut nbits: u32 = 0;
    for &b in &bytes[..end] {
        acc = (acc << 6) | decode_val(b)? as u32;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((acc >> nbits) as u8);
            acc &= (1 << nbits) - 1;
        }
    }
    // Tail-group zero bits check: any leftover bits must be zero for canonical form.
    if acc != 0 {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_empty() {
        assert_eq!(encode(b""), "");
    }

    #[test]
    fn decode_roundtrips_encode() {
        for data in ["f", "fo", "foo", "foob", "fooba", "foobar"] {
            let e = encode(data.as_bytes());
            assert_eq!(decode(&e).as_deref(), Some(data.as_bytes()), "roundtrip {data}");
        }
    }

    #[test]
    fn decode_rejects_invalid() {
        assert_eq!(decode("!!!"), None);
        assert_eq!(decode("====="), None);
        assert_eq!(decode("Zg"), None); // missing pad
        assert_eq!(decode("A"), None); // impossible length
    }

    #[test]
    fn rfc4648_vectors() {
        // RFC 4648 §10 test vectors.
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }
}
