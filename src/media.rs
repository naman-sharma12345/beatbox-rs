//! Media helpers for MCP content blocks: PNG encoding, in-memory WAV and
//! base64, plus the `_content` convention tools use to return images and
//! audio. A tool puts extra MCP content blocks under the `_content` key of
//! its JSON result; the MCP layer moves them into the response `content`
//! array and strips them from `structuredContent`.

use anyhow::Result;
use base64::Engine as _;
use serde_json::{json, Value};
use std::path::Path;

/// Key under which tools return extra MCP content blocks.
pub const CONTENT_KEY: &str = "_content";

pub fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Encode an 8-bit RGB image as PNG.
pub fn png_rgb(width: u32, height: u32, rgb: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut w = enc.write_header()?;
        w.write_image_data(rgb)?;
    }
    Ok(out)
}

/// Decode a PNG file to (width, height, RGB8) (used by tests and screenshots).
pub fn png_dims(bytes: &[u8]) -> Result<(u32, u32)> {
    let dec = png::Decoder::new(std::io::Cursor::new(bytes));
    let r = dec.read_info()?;
    let i = r.info();
    Ok((i.width, i.height))
}

/// 16-bit stereo PCM WAV in memory at `sr` Hz.
pub fn wav_bytes(l: &[f32], r: &[f32], sr: u32) -> Vec<u8> {
    let n = l.len().min(r.len());
    let data_len = (n * 4) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&sr.to_le_bytes());
    out.extend_from_slice(&(sr * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..n {
        for s in [l[i], r[i]] {
            let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

pub fn image_block(png: &[u8]) -> Value {
    json!({"type": "image", "data": b64(png), "mimeType": "image/png"})
}

pub fn audio_block(bytes: &[u8], mime: &str) -> Value {
    json!({"type": "audio", "data": b64(bytes), "mimeType": mime})
}

/// A link to a file on disk the client can fetch (MCP resource_link).
pub fn link_block(path: &Path, mime: &str) -> Value {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    json!({
        "type": "resource_link",
        "uri": format!("file://{}", abs.display()),
        "name": path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
        "mimeType": mime,
    })
}

/// Attach content blocks to a tool result object.
pub fn attach(v: &mut Value, blocks: Vec<Value>) {
    if let Value::Object(m) = v {
        let e = m
            .entry(CONTENT_KEY.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(a) = e {
            a.extend(blocks);
        }
    }
}

/// Split a tool result into (visible JSON, extra content blocks).
pub fn split_content(mut v: Value) -> (Value, Vec<Value>) {
    let blocks = match &mut v {
        Value::Object(m) => match m.remove(CONTENT_KEY) {
            Some(Value::Array(a)) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    (v, blocks)
}

/// Replace bulky content blocks with a short summary (CLI / activity log).
pub fn summarize_content(v: Value) -> Value {
    let (mut v, blocks) = split_content(v);
    if !blocks.is_empty() {
        if let Value::Object(m) = &mut v {
            m.insert(
                "content_blocks".into(),
                Value::Array(
                    blocks
                        .iter()
                        .map(|b| {
                            json!({"type": b["type"], "mimeType": b["mimeType"], "bytes_b64": b["data"].as_str().map(|s| s.len())})
                        })
                        .collect(),
                ),
            );
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_roundtrip_and_split() {
        let px = vec![200u8; 4 * 3 * 3];
        let png = png_rgb(4, 3, &px).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        assert_eq!(png_dims(&png).unwrap(), (4, 3));
        let mut v = json!({"a": 1});
        attach(&mut v, vec![image_block(&png)]);
        let (vis, blocks) = split_content(v);
        assert_eq!(vis, json!({"a": 1}));
        assert_eq!(blocks[0]["type"], "image");
        assert_eq!(wav_bytes(&[0.0; 10], &[0.0; 10], 44100).len(), 84);
    }
}
