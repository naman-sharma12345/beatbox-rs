//! Sample loading (wav/mp3/flac/ogg via symphonia) and fetching from the
//! internet: Freesound search + download, or any direct audio URL.

use crate::dsp::SR;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A sample registered in the project (the audio itself lives on disk).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SampleInfo {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub duration: f32,
}

/// Decoded mono sample data, keyed by sample name.
#[derive(Default, Clone)]
pub struct SampleBank {
    data: HashMap<String, Arc<Vec<f32>>>,
    sources: HashMap<String, PathBuf>,
}

impl SampleBank {
    /// Sample names are case-insensitive ("Vox_A" and "vox_a" are one
    /// sample), matching edit_sample's lowercased names.
    pub fn get(&self, name: &str) -> Option<&[f32]> {
        self.data.get(&name.to_lowercase()).map(|v| v.as_slice())
    }
    pub fn insert(&mut self, name: &str, data: Vec<f32>) {
        self.sources.remove(&name.to_lowercase());
        self.data.insert(name.to_lowercase(), Arc::new(data));
    }
    /// Keep an intentional decoded/edited buffer tied to its project source.
    pub fn insert_for_source(&mut self, name: &str, path: &Path, data: Vec<f32>) {
        self.insert(name, data);
        self.sources.insert(name.to_lowercase(), path.to_path_buf());
    }
    pub fn contains(&self, name: &str) -> bool {
        self.data.contains_key(&name.to_lowercase())
    }
    /// Make sure every sample in `infos` is decoded.
    pub fn sync(&mut self, infos: &[SampleInfo]) -> Vec<String> {
        let mut errors = Vec::new();
        for info in infos {
            let name = info.name.to_lowercase();
            let path = Path::new(&info.path);
            let source_changed = self.sources.get(&name).is_some_and(|p| p != path);
            if !self.contains(&info.name) || source_changed {
                match decode_file(path) {
                    Ok(d) => self.insert_for_source(&info.name, path, d),
                    Err(e) => {
                        // Never play the previous project's same-named audio on failure.
                        self.data.remove(&name);
                        self.sources.remove(&name);
                        errors.push(format!("{}: {e:#}", info.name));
                    }
                }
            } else {
                // Raw insert() is also used for intentional in-memory fixtures.
                self.sources
                    .entry(name)
                    .or_insert_with(|| path.to_path_buf());
            }
        }
        errors
    }
}

/// A copy of a RIFF/WAVE file whose PCM / float `fmt ` chunk is longer than
/// the 16 bytes those formats need, with that chunk cut to 16 bytes (the
/// extra bytes carry nothing for PCM). None when there is nothing to fix.
pub fn wav_fmt_fixed(b: &[u8]) -> Option<Vec<u8>> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let mut i = 12usize;
    while i + 8 <= b.len() {
        let size = u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize;
        if &b[i..i + 4] == b"fmt " {
            if i + 8 + 16 > b.len() {
                return None;
            }
            let tag = u16::from_le_bytes([b[i + 8], b[i + 9]]);
            if !(tag == 1 || tag == 3) || size <= 16 {
                return None;
            }
            let end = (i + 8 + size + (size & 1)).min(b.len());
            let mut out = Vec::with_capacity(b.len());
            out.extend_from_slice(&b[..i + 4]);
            out.extend_from_slice(&16u32.to_le_bytes());
            out.extend_from_slice(&b[i + 8..i + 24]);
            out.extend_from_slice(&b[end..]);
            let riff = (out.len() - 8) as u32;
            out[4..8].copy_from_slice(&riff.to_le_bytes());
            return Some(out);
        }
        i += 8 + size + (size & 1);
    }
    None
}

/// Decode any supported audio file to mono f32 at the engine sample rate.
pub fn decode_file(path: &Path) -> Result<Vec<f32>> {
    let (l, r) = decode_stereo(path)?;
    Ok(l.iter().zip(r.iter()).map(|(a, b)| 0.5 * (a + b)).collect())
}

/// Decode to stereo (mono files are duplicated) at the engine sample rate.
pub fn decode_stereo(path: &Path) -> Result<(Vec<f32>, Vec<f32>)> {
    let (l, r, rate) = decode_stereo_native(path)?;
    let src_rate = rate as f32;
    Ok((resample(&l, src_rate, SR), resample(&r, src_rate, SR)))
}

/// Decode to stereo at the file's own sample rate (no conversion), returning
/// `(left, right, sample_rate)`. Delivery QC measures exactly these frames.
pub fn decode_stereo_native(path: &Path) -> Result<(Vec<f32>, Vec<f32>, u32)> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error as SErr;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probe = |src: Box<dyn symphonia::core::io::MediaSource>| {
        symphonia::default::get_probe().format(
            &hint,
            MediaSourceStream::new(src, Default::default()),
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
    };
    let probed = match probe(Box::new(file)) {
        Ok(p) => p,
        // some sample libraries (e.g. VCSL) write PCM WAVs whose fmt chunk
        // carries extra bytes; the decoder only takes 16 or 18
        Err(e) => match std::fs::read(path).ok().and_then(|b| wav_fmt_fixed(&b)) {
            Some(fixed) => probe(Box::new(std::io::Cursor::new(fixed)))
                .context("unsupported or corrupt audio file")?,
            None => return Err(anyhow::Error::new(e).context("unsupported or corrupt audio file")),
        },
    };
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| anyhow!("no audio track"))?;
    let track_id = track.id;
    let src_rate = track.codec_params.sample_rate.unwrap_or(44_100);
    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let mut left = Vec::new();
    let mut right = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SErr::IoError(_)) | Err(SErr::ResetRequired) => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = *buf.spec();
                let ch = spec.channels.count().max(1);
                let mut sb = SampleBuffer::<f32>::new(buf.capacity() as u64, spec);
                sb.copy_interleaved_ref(buf);
                for frame in sb.samples().chunks(ch) {
                    if ch == 1 {
                        left.push(frame[0]);
                        right.push(frame[0]);
                    } else {
                        left.push(frame[0]);
                        right.push(frame[1]);
                    }
                }
            }
            Err(SErr::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    if left.is_empty() {
        bail!("file decoded to zero samples");
    }
    Ok((left, right, src_rate))
}

/// Sample-rate conversion (Kaiser windowed sinc, see `resample.rs`).
pub fn resample(input: &[f32], from: f32, to: f32) -> Vec<f32> {
    if (from - to).abs() < 0.5 {
        return input.to_vec();
    }
    crate::resample::resample(input, from.round() as u32, to.round() as u32)
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches('_').to_string();
    if s.is_empty() {
        "sample".into()
    } else {
        s.chars().take(48).collect()
    }
}

pub fn sample_name(raw: &str) -> String {
    sanitize(raw)
}

fn freesound_key() -> Result<String> {
    std::env::var("FREESOUND_API_KEY").map_err(|_| {
        anyhow!(
            "FREESOUND_API_KEY is not set. Get a free key at https://freesound.org/apiv2/apply \
             and export FREESOUND_API_KEY=... (or use download_sample with a direct audio URL)"
        )
    })
}

/// Search Freesound. Returns compact results an AI can choose from.
pub fn freesound_search(
    query: &str,
    max: usize,
    cc0_only: bool,
    max_duration: f32,
) -> Result<Value> {
    let key = freesound_key()?;
    let mut filter = format!("duration:[0 TO {}]", max_duration.max(0.1));
    if cc0_only {
        filter.push_str(" license:\"Creative Commons 0\"");
    }
    let resp: Value = ureq::get("https://freesound.org/apiv2/search/text/")
        .query("query", query)
        .query("filter", &filter)
        .query(
            "fields",
            "id,name,duration,license,username,tags,avg_rating,num_downloads",
        )
        .query("page_size", &max.clamp(1, 50).to_string())
        .query("sort", "score")
        .query("token", &key)
        .call()
        .context("freesound search failed")?
        .into_json()?;
    let results: Vec<Value> = resp["results"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|r| {
            json!({
                "id": r["id"],
                "name": r["name"],
                "duration": r["duration"],
                "license": r["license"],
                "author": r["username"],
                "rating": r["avg_rating"],
                "downloads": r["num_downloads"],
                "tags": r["tags"].as_array().map(|t| t.iter().take(8).cloned().collect::<Vec<_>>()),
            })
        })
        .collect();
    Ok(json!({ "query": query, "count": resp["count"], "results": results }))
}

fn download_to(url: &str, dest: &Path) -> Result<()> {
    let resp = ureq::get(url)
        .call()
        .with_context(|| format!("download {url}"))?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(50 * 1024 * 1024)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() {
        bail!("downloaded file is empty");
    }
    std::fs::write(dest, bytes)?;
    Ok(())
}

/// Download a Freesound sound by id (HQ preview) into `dir`.
pub fn freesound_download(id: u64, name: Option<&str>, dir: &Path) -> Result<SampleInfo> {
    let key = freesound_key()?;
    let info: Value = ureq::get(&format!("https://freesound.org/apiv2/sounds/{id}/"))
        .query("fields", "id,name,duration,license,username,previews")
        .query("token", &key)
        .call()
        .context("freesound lookup failed")?
        .into_json()?;
    let url = info["previews"]["preview-hq-mp3"]
        .as_str()
        .ok_or_else(|| anyhow!("sound {id} has no preview"))?;
    let nm = sanitize(name.unwrap_or_else(|| info["name"].as_str().unwrap_or("sample")));
    std::fs::create_dir_all(dir)?;
    let dest = dir.join(format!("fs_{id}_{nm}.mp3"));
    if !dest.exists() {
        download_to(url, &dest)?;
    }
    Ok(SampleInfo {
        name: nm,
        path: dest.to_string_lossy().into(),
        source: format!("https://freesound.org/s/{id}/"),
        license: info["license"].as_str().unwrap_or("").into(),
        author: info["username"].as_str().unwrap_or("").into(),
        duration: info["duration"].as_f64().unwrap_or(0.0) as f32,
    })
}

/// Download any direct audio URL into `dir`.
pub fn url_download(url: &str, name: Option<&str>, dir: &Path) -> Result<SampleInfo> {
    let file_part = url
        .split('?')
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .next()
        .unwrap_or("sample");
    let (stem, ext) = match file_part.rsplit_once('.') {
        Some((s, e)) if e.len() <= 4 => (s, e.to_lowercase()),
        _ => (file_part, "wav".to_string()),
    };
    let nm = sanitize(name.unwrap_or(stem));
    std::fs::create_dir_all(dir)?;
    let dest: PathBuf = dir.join(format!("url_{nm}.{ext}"));
    download_to(url, &dest)?;
    Ok(SampleInfo {
        name: nm,
        path: dest.to_string_lossy().into(),
        source: url.to_string(),
        license: "unknown (check the source)".into(),
        author: String::new(),
        duration: 0.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_names() {
        assert_eq!(sanitize("Big Vinyl Crackle!.wav"), "big_vinyl_crackle__wav");
        assert_eq!(sanitize("***"), "sample");
    }

    #[test]
    fn decodes_pcm_wav_with_a_long_fmt_chunk() {
        // a VCSL-style header: PCM fmt chunk of 20 bytes (cbSize + 2 padding bytes)
        let dir = std::env::temp_dir().join("beatbox_test_decode_fmt20");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.wav");
        let l: Vec<f32> = (0..4410).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        crate::render::write_wav(&p, &l, &l).unwrap();
        let b = std::fs::read(&p).unwrap();
        let i = b.windows(4).position(|w| w == b"fmt ").unwrap();
        let size = u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize;
        assert_eq!(size, 16);
        let fmt_tag = u16::from_le_bytes([b[i + 8], b[i + 9]]);
        if fmt_tag != 1 {
            return; // writer uses another format; nothing to emulate
        }
        let mut v = b[..i + 4].to_vec();
        v.extend_from_slice(&20u32.to_le_bytes());
        v.extend_from_slice(&b[i + 8..i + 24]);
        v.extend_from_slice(&[0, 0, 0, 0]);
        v.extend_from_slice(&b[i + 24..]);
        let riff = (v.len() - 8) as u32;
        v[4..8].copy_from_slice(&riff.to_le_bytes());
        let q = dir.join("fmt20.wav");
        std::fs::write(&q, &v).unwrap();
        let d = decode_file(&q).unwrap();
        assert!((d.len() as i64 - 4410).abs() < 10, "{}", d.len());
        assert!(wav_fmt_fixed(&b).is_none());
    }

    #[test]
    fn decode_roundtrip_wav() {
        let dir = std::env::temp_dir().join("beatbox_test_decode");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.wav");
        let l: Vec<f32> = (0..4410).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        crate::render::write_wav(&p, &l, &l).unwrap();
        let d = decode_file(&p).unwrap();
        assert_eq!(d.len(), 4410);
        assert!((d[100] - l[100]).abs() < 1e-3);
    }

    #[test]
    fn resample_halves() {
        let v = vec![0.0; 88200];
        assert_eq!(resample(&v, 88200.0, 44100.0).len(), 44100);
    }
}

#[cfg(test)]
mod bank_source_tests {
    use super::*;
    #[test]
    fn same_name_source_switch_and_return_loads_correct_audio() {
        let dir = std::env::temp_dir().join(format!("beatbox-bank-source-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        crate::render::write_wav(&a, &vec![0.1; 1000], &vec![0.1; 1000]).unwrap();
        crate::render::write_wav(&b, &vec![0.8; 2000], &vec![0.8; 2000]).unwrap();
        let info = |p: &Path| SampleInfo {
            name: "Same".into(),
            path: p.to_string_lossy().into(),
            ..Default::default()
        };
        let mut bank = SampleBank::default();
        for (p, len) in [(&a, 1000), (&b, 2000), (&a, 1000)] {
            assert!(bank.sync(&[info(p)]).is_empty());
            assert_eq!(bank.get("SAME").unwrap().len(), len);
        }
        let missing = dir.join("missing.wav");
        assert_eq!(bank.sync(&[info(&missing)]).len(), 1);
        assert!(!bank.contains("same"));
        assert!(bank.sync(&[info(&a)]).is_empty());
        assert_eq!(bank.get("same").unwrap().len(), 1000);
        // A trimmed/edited registration must survive sync without raw re-decoding.
        bank.insert_for_source("same", &a, vec![0.3; 50]);
        assert!(bank.sync(&[info(&a)]).is_empty());
        assert_eq!(bank.get("same").unwrap(), &[0.3; 50]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod engine_source_history_tests {
    #[test]
    fn project_source_switch_history_and_save_load() {
        let dir =
            std::env::temp_dir().join(format!("beatbox-source-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        crate::render::write_wav(&a, &vec![0.1; 1000], &vec![0.1; 1000]).unwrap();
        crate::render::write_wav(&b, &vec![0.8; 2000], &vec![0.8; 2000]).unwrap();
        let mut e = crate::Engine::new(dir.clone());
        let info = |p: &std::path::Path| crate::samples::SampleInfo {
            name: "same".into(),
            path: p.to_string_lossy().into(),
            ..Default::default()
        };
        e.project.samples = vec![info(&a)];
        assert!(e.bank.sync(&e.project.samples).is_empty());
        let before = serde_json::to_value(&e.project).unwrap();
        let mut next = e.project.clone();
        next.samples = vec![info(&b)];
        e.replace_project(next);
        assert_eq!(e.bank.get("same").unwrap().len(), 2000);
        let after = serde_json::to_value(&e.project).unwrap();
        assert!(e.undo());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        assert!(e.bank.sync(&e.project.samples).is_empty());
        assert_eq!(e.bank.get("same").unwrap().len(), 1000);
        assert!(e.redo());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        assert!(e.bank.sync(&e.project.samples).is_empty());
        assert_eq!(e.bank.get("same").unwrap().len(), 2000);
        e.call("save_project", &serde_json::json!({"path":"history.json"}))
            .unwrap();
        e.call("load_project", &serde_json::json!({"path":"history.json"}))
            .unwrap();
        assert_eq!(e.bank.get("same").unwrap().len(), 2000);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod rendered_source_history_tests {
    use serde_json::json;
    #[test]
    fn rendered_stem_follows_source_after_project_switch_undo_and_redo() {
        let d = std::env::temp_dir().join(format!("beatbox-render-source-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let a = d.join("a.wav");
        let b = d.join("b.wav");
        for (p, f) in [(&a, 220.), (&b, 880.)] {
            let x: Vec<_> = (0..4410)
                .map(|i| (std::f32::consts::TAU * f * i as f32 / 44100.).sin() * 0.3)
                .collect();
            crate::render::write_wav(p, &x, &x).unwrap();
        }
        let mut e = crate::Engine::new(d.clone());
        e.project.samples = vec![crate::samples::SampleInfo {
            name: "same".into(),
            path: a.to_string_lossy().into(),
            ..Default::default()
        }];
        e.call("add_sample_track", &json!({"name":"test","sample":"same"}))
            .unwrap();
        e.call(
            "add_notes",
            &json!({"track":"test","notes":[{"start":0,"pitch":60,"len":1,"vel":0.8}]}),
        )
        .unwrap();
        let before = e.project.clone();
        let first = e.render_stems_all(&before).unwrap();
        let first = first
            .stems
            .iter()
            .find(|s| s.name == "test")
            .unwrap()
            .left
            .clone();
        let mut after = before.clone();
        after.samples[0].path = b.to_string_lossy().into();
        e.replace_project(after.clone());
        let second = e.render_stems_all(&after).unwrap();
        let second = second
            .stems
            .iter()
            .find(|s| s.name == "test")
            .unwrap()
            .left
            .clone();
        assert_ne!(first, second);
        assert!(first.iter().any(|v| v.abs() > 0.01));
        assert!(second.iter().any(|v| v.abs() > 0.01));
        assert!(e.undo());
        let p = e.project.clone();
        let restored = e.render_stems_all(&p).unwrap();
        assert_eq!(
            restored
                .stems
                .iter()
                .find(|s| s.name == "test")
                .unwrap()
                .left,
            first
        );
        assert!(e.redo());
        let p = e.project.clone();
        let restored = e.render_stems_all(&p).unwrap();
        assert_eq!(
            restored
                .stems
                .iter()
                .find(|s| s.name == "test")
                .unwrap()
                .left,
            second
        );
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[cfg(test)]
mod stereo_rate_tests {
    use super::*;
    #[test]
    fn native_stereo_resample_and_mono_downmix_keep_channel_identity() {
        let dir = std::env::temp_dir().join(format!("beatbox-stereo-rate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stereo48.wav");
        let left = vec![0.5; 4800];
        let right = vec![-0.25; 4800];
        std::fs::write(&path, crate::media::wav_bytes(&left, &right, 48000)).unwrap();
        let (l, r, rate) = decode_stereo_native(&path).unwrap();
        assert_eq!(rate, 48000);
        assert_eq!((l.len(), r.len()), (4800, 4800));
        assert!((l[2400] - 0.5).abs() < 0.001);
        assert!((r[2400] + 0.25).abs() < 0.001);
        let (l, r) = decode_stereo(&path).unwrap();
        let frames = (0.1 * SR) as usize;
        assert_eq!((l.len(), r.len()), (frames, frames));
        assert!((l[frames / 2] - 0.5).abs() < 0.001);
        assert!((r[frames / 2] + 0.25).abs() < 0.001);
        let mono = decode_file(&path).unwrap();
        assert_eq!(mono.len(), frames);
        for i in 0..frames {
            assert!((mono[i] - (l[i] + r[i]) * 0.5).abs() < 0.000001);
        }
        assert!((mono[frames / 2] - 0.125).abs() < 0.001);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
