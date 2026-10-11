//! SoundFont (SF2) instruments: real recorded multisampled instruments played
//! through rustysynth (MIT), plus the on-demand library they come from.
//!
//! A `soundfont` track names a font (a library id such as `generaluser_gs`,
//! or a file path), a bank and a program (General MIDI numbering), or a drum
//! kit (`drums: true`, GM percussion, bank 128). `fixed_key` makes a one-drum
//! track (kick = 36, snare = 38, closed hat = 42 ...) play that drum whatever
//! the note. Fonts are loaded once per process and shared.

use crate::dsp::SR;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

fn one() -> f32 {
    1.0
}
fn two() -> f32 {
    2.5
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SoundFontParams {
    /// Library id (see list_instruments / fetch_instrument) or a .sf2 path.
    pub font: String,
    pub bank: u16,
    pub program: u8,
    /// GM percussion (channel 10, bank 128); `program` picks the kit.
    pub drums: bool,
    /// Play this key for every note (one-drum tracks).
    pub fixed_key: Option<u8>,
    #[serde(default = "one")]
    pub gain: f32,
    /// Longest release tail rendered after note-off, seconds.
    #[serde(default = "two")]
    pub release_max: f32,
    /// Semitones added to every note.
    pub transpose: i32,
    /// Preset name, for display.
    pub preset_name: String,
}

impl Default for SoundFontParams {
    fn default() -> Self {
        SoundFontParams { font: "generaluser_gs".into(), bank: 0, program: 0, drums: false, fixed_key: None, gain: 1.0, release_max: 2.5, transpose: 0, preset_name: String::new() }
    }
}

// ------------------------------------------------------------ library

pub struct LibEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    pub license: &'static str,
    pub source: &'static str,
}

/// Fonts fetch_instrument can download (pinned URL + sha256).
pub const FONTS: &[LibEntry] = &[LibEntry {
    id: "generaluser_gs",
    name: "GeneralUser GS 2.0.3 (S. Christian Collins)",
    kind: "sf2",
    file: "GeneralUser-GS.sf2",
    url: "https://raw.githubusercontent.com/mrbumpy409/GeneralUser-GS/684543d5e5efaef08d02be50dcda8d552478fa60/GeneralUser-GS.sf2",
    sha256: "9575028c7a1f589f5770fccc8cff2734566af40cd26ed836944e9a5152688cfe",
    bytes: 32_319_396,
    license: "GeneralUser GS License v2.0: free use without restriction for music, private or commercial; may be used in software and modified",
    source: "https://github.com/mrbumpy409/GeneralUser-GS",
}];

pub fn font_entry(id: &str) -> Option<&'static LibEntry> {
    FONTS.iter().find(|f| f.id.eq_ignore_ascii_case(id))
}

/// ~/.beatbox/library (BEATBOX_LIBRARY overrides).
pub fn library_dir() -> PathBuf {
    if let Ok(d) = std::env::var("BEATBOX_LIBRARY") {
        return PathBuf::from(d);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".beatbox").join("library")
}

pub fn installed_path(e: &LibEntry) -> PathBuf {
    library_dir().join(e.id).join(e.file)
}

/// Download (once) and sha256-check a library font. Returns its path.
pub fn fetch_font(id: &str) -> Result<(PathBuf, bool)> {
    let e = font_entry(id).ok_or_else(|| anyhow!("unknown library font '{id}'. Known: {}", FONTS.iter().map(|f| f.id).collect::<Vec<_>>().join(", ")))?;
    let p = installed_path(e);
    if p.is_file() && std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0) == e.bytes {
        return Ok((p, false));
    }
    let resp = ureq::get(e.url).timeout(std::time::Duration::from_secs(600)).call().map_err(|er| anyhow!("download {}: {er}", e.id))?;
    let mut buf = Vec::with_capacity(e.bytes as usize);
    std::io::Read::read_to_end(&mut resp.into_reader(), &mut buf)?;
    let got = crate::voice_ai::sha256_hex(&buf);
    if got != e.sha256 {
        bail!("{}: checksum mismatch ({got}), not installed", e.id);
    }
    std::fs::create_dir_all(p.parent().unwrap())?;
    let tmp = p.with_extension("part");
    std::fs::write(&tmp, &buf)?;
    std::fs::rename(&tmp, &p)?;
    Ok((p, true))
}

/// Resolve a font name to a file: a library id (fetched when missing and
/// BEATBOX_NO_FETCH is unset) or a path.
pub fn resolve_font(font: &str) -> Result<PathBuf> {
    if let Some(e) = font_entry(font) {
        let p = installed_path(e);
        if p.is_file() {
            return Ok(p);
        }
        if std::env::var("BEATBOX_NO_FETCH").is_ok() {
            bail!("library font '{font}' is not installed (fetch_instrument {font})");
        }
        return fetch_font(font).map(|r| r.0);
    }
    let p = PathBuf::from(font);
    if p.is_file() {
        return Ok(p);
    }
    bail!("no soundfont '{font}': give a library id ({}) or a .sf2 path", FONTS.iter().map(|f| f.id).collect::<Vec<_>>().join(", "))
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Arc<rustysynth::SoundFont>>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, Arc<rustysynth::SoundFont>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn load_font(font: &str) -> Result<Arc<rustysynth::SoundFont>> {
    let path = resolve_font(font)?;
    if let Some(f) = cache().lock().unwrap().get(&path) {
        return Ok(f.clone());
    }
    let mut r = std::io::BufReader::new(std::fs::File::open(&path)?);
    let sf = Arc::new(rustysynth::SoundFont::new(&mut r).map_err(|e| anyhow!("reading {}: {e:?}", path.display()))?);
    let mut c = cache().lock().unwrap();
    // keep at most 3 fonts resident
    if c.len() >= 3 {
        let k = c.keys().next().cloned();
        if let Some(k) = k {
            c.remove(&k);
        }
    }
    c.insert(path, sf.clone());
    Ok(sf)
}

/// (bank, program, name) of every preset in a font, sorted.
pub fn presets(font: &str) -> Result<Vec<(u16, u8, String)>> {
    let sf = load_font(font)?;
    let mut v: Vec<(u16, u8, String)> = sf.get_presets().iter().map(|p| (p.get_bank_number() as u16, p.get_patch_number() as u8, p.get_name().trim().to_string())).collect();
    v.sort();
    Ok(v)
}

// ------------------------------------------------------------ playing

/// Render one note (mono mix of the stereo render).
pub fn render_note(p: &SoundFontParams, pitch: f32, vel: f32, gate: f32) -> Vec<f32> {
    let (l, r) = render_note_stereo(p, pitch, vel, gate);
    l.iter().zip(r.iter()).map(|(a, b)| 0.5 * (a + b)).collect()
}

/// Render one note in stereo (the font's own panning and stereo samples),
/// through a fresh synthesizer on the shared font.
pub fn render_note_stereo(p: &SoundFontParams, pitch: f32, vel: f32, gate: f32) -> (Vec<f32>, Vec<f32>) {
    let Ok(sf) = load_font(&p.font) else { return (Vec::new(), Vec::new()) };
    let mut st = rustysynth::SynthesizerSettings::new(SR as i32);
    st.enable_reverb_and_chorus = false;
    st.maximum_polyphony = 16;
    let Ok(mut syn) = rustysynth::Synthesizer::new(&sf, &st) else { return (Vec::new(), Vec::new()) };
    let ch = if p.drums { 9 } else { 0 };
    if !p.drums {
        syn.process_midi_message(ch, 0xB0, 0, (p.bank & 0x7f) as i32);
    }
    syn.process_midi_message(ch, 0xC0, p.program as i32, 0);
    let target = p.fixed_key.map(|k| k as f32).unwrap_or(pitch + p.transpose as f32);
    let key = target.round().clamp(0.0, 127.0) as i32;
    let frac = target - key as f32;
    if frac.abs() > 0.01 && !p.drums {
        // default bend range is +/-2 semitones
        let v = (8192.0 + frac / 2.0 * 8192.0).clamp(0.0, 16383.0) as i32;
        syn.process_midi_message(ch, 0xE0, v & 0x7f, v >> 7);
    }
    let velocity = (vel.clamp(0.0, 1.0) * 127.0).round().clamp(1.0, 127.0) as i32;
    syn.note_on(ch, key, velocity);
    let block = 512;
    let on = ((gate.max(0.01)) * SR) as usize;
    let max_tail = (p.release_max.clamp(0.05, 10.0) * SR) as usize;
    let mut out = Vec::with_capacity(on + max_tail / 2);
    let mut out_r = Vec::with_capacity(on + max_tail / 2);
    let (mut l, mut r) = (vec![0.0f32; block], vec![0.0f32; block]);
    let mut n = 0;
    let mut off = false;
    let mut quiet = 0;
    while n < on + max_tail {
        if !off && n >= on {
            syn.note_off(ch, key);
            off = true;
        }
        let len = if !off { block.min(on - n).max(1) } else { block };
        syn.render(&mut l[..len], &mut r[..len]);
        let mut pk = 0.0f32;
        for i in 0..len {
            let (a, b) = (l[i] * p.gain, r[i] * p.gain);
            pk = pk.max(a.abs()).max(b.abs());
            out.push(a);
            out_r.push(b);
        }
        n += len;
        if off {
            quiet = if pk < 1e-4 { quiet + 1 } else { 0 };
            if quiet >= 4 {
                break;
            }
        }
    }
    // short fade at the very end so a cut tail cannot click
    let f = (0.005 * SR) as usize;
    let m = out.len();
    for i in 0..f.min(m) {
        out[m - 1 - i] *= i as f32 / f as f32;
        out_r[m - 1 - i] *= i as f32 / f as f32;
    }
    (out, out_r)
}

/// Curated names for the instruments songs ask for most (GM numbers).
pub const ALIASES: &[(&str, &str, u16, u8, bool, Option<u8>)] = &[
    ("grand_piano", "Acoustic grand piano", 0, 0, false, None),
    ("bright_piano", "Bright acoustic piano", 0, 1, false, None),
    ("electric_piano", "Electric piano (Rhodes-style)", 0, 4, false, None),
    ("organ", "Drawbar organ", 0, 16, false, None),
    ("nylon_guitar", "Nylon-string acoustic guitar", 0, 24, false, None),
    ("acoustic_guitar", "Steel-string acoustic guitar", 0, 25, false, None),
    ("jazz_guitar", "Jazz electric guitar", 0, 26, false, None),
    ("clean_electric_guitar", "Clean electric guitar", 0, 27, false, None),
    ("muted_guitar", "Palm-muted electric guitar", 0, 28, false, None),
    ("overdrive_guitar", "Overdriven guitar", 0, 29, false, None),
    ("distortion_guitar", "Distortion guitar", 0, 30, false, None),
    ("acoustic_bass", "Acoustic (upright) bass", 0, 32, false, None),
    ("electric_bass", "Electric bass, finger", 0, 33, false, None),
    ("picked_bass", "Electric bass, pick", 0, 34, false, None),
    ("fretless_bass", "Fretless bass", 0, 35, false, None),
    ("synth_bass", "Synth bass", 0, 38, false, None),
    ("violin", "Violin", 0, 40, false, None),
    ("cello", "Cello", 0, 42, false, None),
    ("strings", "String ensemble", 0, 48, false, None),
    ("slow_strings", "Slow string ensemble", 0, 49, false, None),
    ("pizzicato", "Pizzicato strings", 0, 45, false, None),
    ("harp", "Harp", 0, 46, false, None),
    ("choir", "Choir aahs", 0, 52, false, None),
    ("trumpet", "Trumpet", 0, 56, false, None),
    ("brass", "Brass section", 0, 61, false, None),
    ("sax", "Tenor sax", 0, 66, false, None),
    ("flute", "Flute", 0, 73, false, None),
    ("warm_pad", "Warm pad", 0, 89, false, None),
    ("drum_kit", "Acoustic drum kit (GM standard; notes pick the drum)", 0, 0, true, None),
    ("room_kit", "Room drum kit", 0, 8, true, None),
    ("brush_kit", "Brush drum kit", 0, 40, true, None),
    ("kick", "Acoustic kick (GM 36)", 0, 0, true, Some(36)),
    ("snare", "Acoustic snare (GM 38)", 0, 0, true, Some(38)),
    ("rimshot", "Side stick (GM 37)", 0, 0, true, Some(37)),
    ("clap", "Hand clap (GM 39)", 0, 0, true, Some(39)),
    ("closed_hat", "Closed hi-hat (GM 42)", 0, 0, true, Some(42)),
    ("pedal_hat", "Pedal hi-hat (GM 44)", 0, 0, true, Some(44)),
    ("open_hat", "Open hi-hat (GM 46)", 0, 0, true, Some(46)),
    ("floor_tom", "Low floor tom (GM 41)", 0, 0, true, Some(41)),
    ("low_tom", "Low tom (GM 45)", 0, 0, true, Some(45)),
    ("mid_tom", "Mid tom (GM 47)", 0, 0, true, Some(47)),
    ("high_tom", "High tom (GM 50)", 0, 0, true, Some(50)),
    ("crash", "Crash cymbal (GM 49)", 0, 0, true, Some(49)),
    ("ride", "Ride cymbal (GM 51)", 0, 0, true, Some(51)),
    ("tambourine", "Tambourine (GM 54)", 0, 0, true, Some(54)),
    ("shaker", "Shaker / maracas (GM 70)", 0, 0, true, Some(70)),
];

pub fn alias(name: &str) -> Option<SoundFontParams> {
    let n = name.trim().to_lowercase().replace([' ', '-'], "_");
    ALIASES.iter().find(|a| a.0 == n).map(|a| SoundFontParams { bank: a.2, program: a.3, drums: a.4, fixed_key: a.5, preset_name: a.1.to_string(), ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays a real note when the GM font is installed (skipped otherwise, so
    /// CI without network stays green).
    #[test]
    fn soundfont_notes_render_when_the_font_is_installed() {
        let e = font_entry("generaluser_gs").unwrap();
        if !installed_path(e).is_file() {
            eprintln!("generaluser_gs not installed; skipping");
            return;
        }
        for name in ["clean_electric_guitar", "electric_bass", "kick", "snare", "closed_hat", "strings", "grand_piano"] {
            let p = alias(name).unwrap();
            let y = render_note(&p, 52.0, 0.8, 0.4);
            let pk = y.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(y.len() > (0.1 * SR) as usize, "{name}: {} samples", y.len());
            assert!(pk > 0.01 && pk < 2.0, "{name}: peak {pk}");
        }
        let ps = presets("generaluser_gs").unwrap();
        assert!(ps.len() > 128, "{} presets", ps.len());
    }
}
