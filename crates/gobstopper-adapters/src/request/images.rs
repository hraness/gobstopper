//! Token estimates for image blocks.
//!
//! The rest of a request is estimated at four characters per token, which
//! does not hold for base64: a 1536x1024 screenshot is ~2.7M characters but
//! costs about 2k tokens. Images are priced from their dimensions with
//! Anthropic's 28x28-pixel patch formula, capped at the high-resolution
//! ceiling. Erring high is deliberate: overcounting compacts a little early;
//! undercounting sends a request the provider refuses for length.

use serde_json::Value;

const PATCH_PX: u64 = 28;
const MAX_IMAGE_TOKENS: u64 = 4_784;
/// Hosted URLs and unreadable headers: roughly a 1MP screenshot.
const DEFAULT_IMAGE_TOKENS: u64 = 1_568;
/// Enough base64 to reach a JPEG frame header past an EXIF block.
const HEADER_B64_CHARS: usize = 8_192;

fn be16(raw: &[u8], at: usize) -> Option<u64> {
    Some(u64::from(u16::from_be_bytes([
        *raw.get(at)?,
        *raw.get(at + 1)?,
    ])))
}

fn le16(raw: &[u8], at: usize) -> Option<u64> {
    Some(u64::from(u16::from_le_bytes([
        *raw.get(at)?,
        *raw.get(at + 1)?,
    ])))
}

fn le24(raw: &[u8], at: usize) -> Option<u64> {
    Some(
        u64::from(*raw.get(at)?)
            | u64::from(*raw.get(at + 1)?) << 8
            | u64::from(*raw.get(at + 2)?) << 16,
    )
}

fn png(raw: &[u8]) -> Option<(u64, u64)> {
    if !raw.starts_with(b"\x89PNG\r\n\x1a\n") || raw.len() < 24 {
        return None;
    }
    let width = u32::from_be_bytes(raw[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(raw[20..24].try_into().ok()?);
    Some((u64::from(width), u64::from(height)))
}

fn gif(raw: &[u8]) -> Option<(u64, u64)> {
    if !(raw.starts_with(b"GIF87a") || raw.starts_with(b"GIF89a")) {
        return None;
    }
    Some((le16(raw, 6)?, le16(raw, 8)?))
}

fn jpeg(raw: &[u8]) -> Option<(u64, u64)> {
    if !raw.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2;
    while i + 9 < raw.len() {
        if raw[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = raw[i + 1];
        if marker == 0x01 || (0xD0..=0xD9).contains(&marker) {
            i += 2;
            continue;
        }
        let segment = be16(raw, i + 2)? as usize;
        if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
            return Some((be16(raw, i + 7)?, be16(raw, i + 5)?));
        }
        if segment < 2 {
            return None;
        }
        i += 2 + segment;
    }
    None
}

fn webp(raw: &[u8]) -> Option<(u64, u64)> {
    if raw.len() < 30 || !raw.starts_with(b"RIFF") || &raw[8..12] != b"WEBP" {
        return None;
    }
    match &raw[12..16] {
        b"VP8X" => Some((le24(raw, 24)? + 1, le24(raw, 27)? + 1)),
        b"VP8 " => Some((le16(raw, 26)? & 0x3FFF, le16(raw, 28)? & 0x3FFF)),
        b"VP8L" => {
            let bits = u32::from_le_bytes(raw[21..25].try_into().ok()?);
            Some((
                u64::from(bits & 0x3FFF) + 1,
                u64::from((bits >> 14) & 0x3FFF) + 1,
            ))
        }
        _ => None,
    }
}

fn dimensions(raw: &[u8]) -> Option<(u64, u64)> {
    [png, jpeg, gif, webp]
        .iter()
        .find_map(|reader| reader(raw).filter(|&(w, h)| w > 0 && h > 0))
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

/// Decode the leading base64 characters of a payload (standard or URL-safe
/// alphabet), enough to read an image header.
fn header_bytes(payload: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_B64_CHARS / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in payload.bytes().take(HEADER_B64_CHARS) {
        let Some(value) = base64_value(byte) else {
            if byte == b'=' {
                break;
            }
            continue;
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    out
}

/// Estimated token cost of one image payload: a `data:` URL, raw base64
/// (Anthropic `source.data`), or a hosted URL.
pub fn tokens_for_payload(payload: &str) -> u64 {
    let encoded = match payload.strip_prefix("data:") {
        Some(rest) => match rest.split_once(',') {
            Some((_, data)) => data,
            None => return DEFAULT_IMAGE_TOKENS,
        },
        None if payload.contains("://") => return DEFAULT_IMAGE_TOKENS,
        None => payload,
    };
    match dimensions(&header_bytes(encoded)) {
        Some((width, height)) => {
            let patches = width.div_ceil(PATCH_PX) * height.div_ceil(PATCH_PX);
            patches.clamp(1, MAX_IMAGE_TOKENS)
        }
        None => DEFAULT_IMAGE_TOKENS,
    }
}

/// Every image payload string in a request body, across dialects:
/// Anthropic `{"type":"image","source":{"data":..}}`, Responses
/// `{"type":"input_image","image_url":"data:.."}`, and Chat
/// `{"type":"image_url","image_url":{"url":"data:.."}}`. Documents keep the
/// character estimate: their cost tracks their text, not pixels.
pub fn payloads(value: &Value) -> Vec<&str> {
    let mut found = Vec::new();
    collect(value, &mut found);
    found
}

fn collect<'a>(value: &'a Value, found: &mut Vec<&'a str>) {
    match value {
        Value::Object(map) => {
            let payload = match map.get("type").and_then(Value::as_str) {
                Some("image") => map
                    .get("source")
                    .and_then(|source| source.get("data"))
                    .and_then(Value::as_str),
                Some("input_image" | "image_url") => match map.get("image_url") {
                    Some(Value::String(url)) => Some(url.as_str()),
                    Some(Value::Object(inner)) => inner.get("url").and_then(Value::as_str),
                    _ => None,
                },
                _ => None,
            };
            match payload {
                Some(payload) => found.push(payload),
                None => map.values().for_each(|inner| collect(inner, found)),
            }
        }
        Value::Array(items) => items.iter().for_each(|inner| collect(inner, found)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn encode(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
            for i in 0..=chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            }
            for _ in chunk.len()..3 {
                out.push('=');
            }
        }
        out
    }

    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut raw = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        raw.extend_from_slice(&width.to_be_bytes());
        raw.extend_from_slice(&height.to_be_bytes());
        raw.extend_from_slice(&[8, 6, 0, 0, 0]);
        raw
    }

    #[test]
    fn prices_images_by_dimensions_not_base64_length() {
        let data = encode(&png_header(1092, 1092));
        assert_eq!(
            tokens_for_payload(&format!("data:image/png;base64,{data}")),
            1521
        );
        assert_eq!(tokens_for_payload(&data), 1521);
        let huge = encode(&png_header(8000, 8000));
        assert_eq!(tokens_for_payload(&huge), MAX_IMAGE_TOKENS);
        assert_eq!(
            tokens_for_payload("https://example.com/x.png"),
            DEFAULT_IMAGE_TOKENS
        );
        assert_eq!(
            tokens_for_payload("data:image/png;base64,%%%%"),
            DEFAULT_IMAGE_TOKENS
        );
    }

    #[test]
    fn reads_gif_and_jpeg_headers() {
        assert_eq!(dimensions(b"GIF89a\x40\x01\xf0\x00rest"), Some((320, 240)));
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x04, 0x00, 0x00];
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0xE0, 0x02, 0x80, 0x03]);
        assert_eq!(dimensions(&jpeg), Some((640, 480)));
        assert_eq!(dimensions(b"not an image at all"), None);
    }

    #[test]
    fn finds_payloads_in_every_dialect_shape() {
        let body = json!({"messages": [
            {"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "data": "AAA"}},
                {"type": "input_image", "image_url": "data:image/png;base64,BBB"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,CCC"}},
                {"type": "document", "source": {"type": "base64", "data": "DDD"}}
            ]}
        ]});
        assert_eq!(
            payloads(&body),
            vec![
                "AAA",
                "data:image/png;base64,BBB",
                "data:image/png;base64,CCC"
            ]
        );
    }
}
