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
}

impl SampleBank {
    pub fn get(&self, name: &str) -> Option<&[f32]> {
        self.data.get(name).map(|v| v.as_slice())
    }
    pub fn insert(&mut self, name: &str, data: Vec<f32>) {
        self.data.insert(name.to_string(), Arc::new(data));
    }
    pub fn contains(&self, name: &str) -> bool {
        self.data.contains_key(name)
    }
    /// Make sure every sample in `infos` is decoded.
    pub fn sync(&mut self, infos: &[SampleInfo]) -> Vec<String> {
        let mut errors = Vec::new();
        for info in infos {
            if !self.contains(&info.name) {
                match decode_file(Path::new(&info.path)) {
                    Ok(d) => self.insert(&info.name, d),
                    Err(e) => errors.push(format!("{}: {e:#}", info.name)),
                }
            }
        }
        errors
    }
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
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .context("unsupported or corrupt audio file")?;
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
