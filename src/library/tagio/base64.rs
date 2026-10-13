// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! RFC 4648 base64: the standard alphabet with padding (Vorbis
//! `METADATA_BLOCK_PICTURE`) and the URL-safe alphabet without padding
//! (AcoustID's compressed fingerprints).

const STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn encode_with(data: &[u8], alphabet: &[u8; 64], pad: bool) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let chars = chunk.len() + 1;
        for i in 0..4 {
            if i < chars {
                out.push(char::from(alphabet[(n >> (18 - 6 * i)) as usize & 63]));
            } else if pad {
                out.push('=');
            }
        }
    }
    out
}

/// Standard alphabet, padded.
#[must_use]
pub fn encode(data: &[u8]) -> String {
    encode_with(data, STD, true)
}

/// URL-safe alphabet, unpadded.
#[must_use]
pub fn encode_url_nopad(data: &[u8]) -> String {
    encode_with(data, URL, false)
}

/// Decodes either alphabet, with or without padding; ASCII whitespace is
/// ignored. `None` on any other invalid character or a dangling sextet.
#[must_use]
pub fn decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    (bits < 6).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_vectors() {
        for (raw, enc) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(raw.as_bytes()), enc);
            assert_eq!(decode(enc).unwrap(), raw.as_bytes());
        }
    }

    #[test]
    fn url_safe_alphabet_without_padding() {
        let data = [0xfb, 0xff, 0xbf];
        assert_eq!(encode(&data), "+/+/");
        assert_eq!(encode_url_nopad(&data), "-_-_");
        assert_eq!(encode_url_nopad(b"f"), "Zg");
        assert_eq!(decode("-_-_").unwrap(), data);
        assert_eq!(decode("Zg").unwrap(), b"f");
    }

    #[test]
    fn rejects_invalid_characters() {
        assert_eq!(decode("Zm9v*"), None);
    }
}
