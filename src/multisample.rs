//! Multisampled instruments: key zones x velocity layers x round robins
//! (SFZ-style), an SFZ loader, filename-based zone mapping, and a catalog of
//! CC0 instrument packs from VSCO 2 Community Edition (real piano, string
//! sections, woodwinds, brass) that `install_instrument_pack` downloads.

use crate::dsp::SR;
use crate::samples::SampleBank;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

fn one() -> f32 {
    1.0
}
fn max_key() -> u8 {
    127
}

// Internally tagged instruments deserialize through serde Content, whose map
// keys stay strings. JSON's native u8 map-key adapter is not used there.
fn deserialize_key_settings<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<std::collections::BTreeMap<u8, f32>, D::Error> {
    let raw = std::collections::BTreeMap::<String, f32>::deserialize(d)?;
    raw.into_iter()
        .map(|(k, v)| {
            let key = k.parse::<u8>().map_err(serde::de::Error::custom)?;
            if key > 127 {
                return Err(serde::de::Error::custom("MIDI key must be 0..127"));
            }
            Ok((key, v))
        })
        .collect()
}

/// Indirect per-key response storage keeps the Instrument enum from growing.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct KeyVelocityResponse(
    #[serde(deserialize_with = "deserialize_key_settings")] pub std::collections::BTreeMap<u8, f32>,
);
impl std::ops::Deref for KeyVelocityResponse {
    type Target = std::collections::BTreeMap<u8, f32>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for KeyVelocityResponse {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
fn response_empty(v: &KeyVelocityResponse) -> bool {
    v.is_empty()
}

/// One sample mapped to a key range and velocity range.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Zone {
    /// Sample name registered in the project.
    pub sample: String,
    /// MIDI note the sample was recorded at.
    pub root: u8,
    #[serde(default)]
    pub lo_note: u8,
    #[serde(default = "max_key")]
    pub hi_note: u8,
    /// Velocity range 0..1.
    #[serde(default)]
    pub vel_lo: f32,
    #[serde(default = "one")]
    pub vel_hi: f32,
    #[serde(default)]
    pub tune_cents: f32,
    #[serde(default)]
    pub gain_db: f32,
    /// Round-robin slot (zones with equal ranges alternate).
    #[serde(default)]
    pub rr: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MultisampleParams {
    pub zones: Vec<Zone>,
    pub attack: f32,
    /// Release after note-off, seconds.
    pub release: f32,
    /// Play the whole sample regardless of note length (drums, plucks).
    pub one_shot: bool,
    pub round_robin: bool,
    /// Opt-in GM open-hat46 choke by closed/pedal42/44 on this channel.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gm_hat_choke: bool,
    /// Blend neighbouring velocity layers near their boundary.
    pub vel_crossfade: bool,
    /// How much velocity changes level (0 = layers only, 1 = full).
    pub vel_to_gain: f32,
    /// Response exponent before layer selection and level. 1 preserves input.
    #[serde(default = "one")]
    pub velocity_curve: f32,
    /// Instrument-wide sample-rate fine tuning; mapping and MIDI unchanged.
    #[serde(default)]
    pub tune_cents: f32,
    pub gain: f32,
    /// Absolute per-played-key trim, independent of recorded zone gains.
    #[serde(
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty",
        deserialize_with = "deserialize_key_settings"
    )]
    pub key_trim_db: std::collections::BTreeMap<u8, f32>,
    /// Absolute per-played-key tuning in cents; mapping and zone tuning unchanged.
    #[serde(
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty",
        deserialize_with = "deserialize_key_settings"
    )]
    pub key_tune_cents: std::collections::BTreeMap<u8, f32>,
    /// Optional per-key maximum playback in milliseconds, with 5ms end fade.
    #[serde(
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty",
        deserialize_with = "deserialize_key_settings"
    )]
    pub key_length_ms: std::collections::BTreeMap<u8, f32>,
    /// Per-key balance before channel effects/pan: -1 left, +1 right, 0 unchanged.
    #[serde(
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty",
        deserialize_with = "deserialize_key_settings"
    )]
    pub key_pan: std::collections::BTreeMap<u8, f32>,
    /// Per-key attack override milliseconds. Missing/zero uses instrument attack.
    #[serde(
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty",
        deserialize_with = "deserialize_key_settings"
    )]
    pub key_attack_ms: std::collections::BTreeMap<u8, f32>,
    /// Per-key exponential decay half-life milliseconds; missing/zero is off.
    #[serde(
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty",
        deserialize_with = "deserialize_key_settings"
    )]
    pub key_decay_ms: std::collections::BTreeMap<u8, f32>,
    /// Two cascaded one-pole low-pass stages per played key. Zero/missing bypasses.
    #[serde(
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty",
        deserialize_with = "deserialize_key_settings"
    )]
    pub key_lowpass_hz: std::collections::BTreeMap<u8, f32>,
    /// Per-key cascaded DC-blocking high-pass stages; missing/zero bypasses.
    #[serde(
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty",
        deserialize_with = "deserialize_key_settings"
    )]
    pub key_highpass_hz: std::collections::BTreeMap<u8, f32>,
    /// Per-played-key velocity curve override; missing/zero uses kit curve.
    #[serde(default, skip_serializing_if = "response_empty")]
    pub key_velocity_curve: Box<KeyVelocityResponse>,
    /// Keys whose source recordings play backwards, before resampling/envelope.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub reverse_keys: std::collections::BTreeSet<u8>,
    /// Where the zones came from (pack / sfz) and its licence.
    pub source: String,
}

impl Default for MultisampleParams {
    fn default() -> Self {
        MultisampleParams {
            zones: Vec::new(),
            attack: 0.002,
            release: 0.35,
            one_shot: false,
            round_robin: true,
            gm_hat_choke: false,
            vel_crossfade: true,
            vel_to_gain: 0.6,
            velocity_curve: 1.0,
            tune_cents: 0.0,
            gain: 0.9,
            key_trim_db: Default::default(),
            key_tune_cents: Default::default(),
            key_length_ms: Default::default(),
            key_pan: Default::default(),
            key_attack_ms: Default::default(),
            key_decay_ms: Default::default(),
            key_lowpass_hz: Default::default(),
            key_highpass_hz: Default::default(),
            key_velocity_curve: Default::default(),
            reverse_keys: Default::default(),
            source: String::new(),
        }
    }
}

fn key_dist(z: &Zone, key: i32) -> i32 {
    if key < z.lo_note as i32 {
        z.lo_note as i32 - key
    } else if key > z.hi_note as i32 {
        key - z.hi_note as i32
    } else {
        0
    }
}

/// Non-destructive touch response, shared by layer choice and note gain.
pub fn response_velocity(p: &MultisampleParams, velocity: f32) -> f32 {
    let curve = if p.velocity_curve.is_finite() {
        p.velocity_curve.clamp(0.25, 4.0)
    } else {
        1.0
    };
    if curve == 1.0 {
        velocity
    } else {
        velocity.clamp(0.0, 1.0).powf(curve)
    }
}

/// Per-key override affects layer selection and level equally, not MIDI.
pub fn response_velocity_for_key(p: &MultisampleParams, key: i32, velocity: f32) -> f32 {
    match p
        .key_velocity_curve
        .get(&(key.clamp(0, 127) as u8))
        .copied()
        .filter(|v| v.is_finite() && *v > 0.)
    {
        Some(curve) => velocity.clamp(0., 1.).powf(curve.clamp(0.25, 4.)),
        None => response_velocity(p, velocity),
    }
}

/// Zones to play with their weights for a key / velocity / round-robin counter.
pub fn pick_zones(p: &MultisampleParams, key: i32, vel: f32, rr_counter: u64) -> Vec<(usize, f32)> {
    if p.zones.is_empty() {
        return vec![];
    }
    let vel = response_velocity_for_key(p, key, vel);
    // nearest key range first (exact match = 0)
    let best = p.zones.iter().map(|z| key_dist(z, key)).min().unwrap_or(0);
    let near: Vec<usize> = (0..p.zones.len())
        .filter(|&i| key_dist(&p.zones[i], key) == best)
        .collect();
    // velocity layer: containing zone, or the closest one
    let vdist = |z: &Zone| {
        if vel < z.vel_lo {
            z.vel_lo - vel
        } else if vel > z.vel_hi {
            vel - z.vel_hi
        } else {
            0.0
        }
    };
    let vbest = near
        .iter()
        .map(|&i| vdist(&p.zones[i]))
        .fold(f32::MAX, f32::min);
    let layer: Vec<usize> = near
        .iter()
        .copied()
        .filter(|&i| (vdist(&p.zones[i]) - vbest).abs() < 1e-6)
        .collect();
    let choose_rr = |cands: &[usize]| -> usize {
        if !p.round_robin || cands.len() == 1 {
            return cands[0];
        }
        let mut c = cands.to_vec();
        c.sort_by_key(|&i| p.zones[i].rr);
        c[(rr_counter % c.len() as u64) as usize]
    };
    let main = choose_rr(&layer);
    let mut out = vec![(main, 1.0f32)];
    if p.vel_crossfade {
        let z = &p.zones[main];
        let width = (z.vel_hi - z.vel_lo).max(0.01);
        let edge = 0.25 * width;
        // blend toward the neighbouring layer near the boundaries
        let (other_side, t) = if vel - z.vel_lo < edge && z.vel_lo > 0.0 {
            (-1, 0.5 * (1.0 - (vel - z.vel_lo) / edge))
        } else if z.vel_hi - vel < edge && z.vel_hi < 1.0 {
            (1, 0.5 * (1.0 - (z.vel_hi - vel) / edge))
        } else {
            (0, 0.0)
        };
        // A nearest layer can be selected just outside its velocity range
        // (SFZ/MIDI ranges often leave small gaps). Do not extrapolate the
        // boundary blend: the neighbour must never outweigh the main layer
        // or produce a negative main weight for narrow ranges/wider gaps.
        let t = t.clamp(0.0, 0.5);
        if other_side != 0 && t > 0.01 {
            let neigh: Vec<usize> = near
                .iter()
                .copied()
                .filter(|&i| {
                    let q = &p.zones[i];
                    if other_side < 0 {
                        (q.vel_hi - z.vel_lo).abs() < 0.02
                    } else {
                        (q.vel_lo - z.vel_hi).abs() < 0.02
                    }
                })
                .collect();
            if !neigh.is_empty() {
                let n = choose_rr(&neigh);
                out[0].1 = 1.0 - t;
                out.push((n, t));
            }
        }
    }
    out
}

pub fn render_multisample(
    p: &MultisampleParams,
    pitch: f32,
    vel: f32,
    gate: f32,
    bank: &SampleBank,
    seed: u64,
) -> Vec<f32> {
    let key = pitch.round() as i32;
    let picks = pick_zones(p, key, vel, seed);
    let mut out: Vec<f32> = Vec::new();
    let vel = response_velocity_for_key(p, key, vel);
    let vel_gain = 1.0 - p.vel_to_gain.clamp(0.0, 1.0) * (1.0 - vel);
    for (zi, w) in picks {
        let z = &p.zones[zi];
        let Some(data) = bank.get(&z.sample) else {
            continue;
        };
        if data.is_empty() {
            continue;
        }
        let tune = p
            .key_tune_cents
            .get(&(key.clamp(0, 127) as u8))
            .copied()
            .filter(|x| x.is_finite())
            .unwrap_or(0.0)
            .clamp(-1200.0, 1200.0);
        let global_tune = if p.tune_cents.is_finite() {
            p.tune_cents.clamp(-100.0, 100.0)
        } else {
            0.0
        };
        let rate =
            2f32.powf((pitch - z.root as f32 + (z.tune_cents + tune + global_tune) / 100.0) / 12.0);
        let avail = data.len() as f32 / rate / SR;
        let len = if p.one_shot {
            avail
        } else {
            (gate + p.release.max(0.005)).min(avail)
        };
        let cap = p
            .key_length_ms
            .get(&(key.clamp(0, 127) as u8))
            .copied()
            .filter(|x| x.is_finite() && *x > 0.0)
            .map(|x| x.clamp(10.0, 5000.0) / 1000.0);
        let capped = cap.is_some_and(|c| c < len);
        let n = (cap.map_or(len, |c| len.min(c)) * SR) as usize;
        if out.len() < n {
            out.resize(n, 0.0);
        }
        let trim = p
            .key_trim_db
            .get(&(key.clamp(0, 127) as u8))
            .copied()
            .filter(|x| x.is_finite())
            .unwrap_or(0.0)
            .clamp(-24.0, 6.0);
        let g = w * vel_gain * p.gain * crate::dsp::db_to_gain(z.gain_db + trim);
        let attack = p
            .key_attack_ms
            .get(&(key.clamp(0, 127) as u8))
            .copied()
            .filter(|v| v.is_finite() && *v > 0.)
            .map(|v| v.clamp(0.5, 200.) / 1000.)
            .unwrap_or(p.attack);
        let att = (attack.max(0.0005) * SR).max(1.0);
        let rel_n = (p.release.max(0.005) * SR).max(1.0);
        let gate_n = (gate * SR) as usize;
        // band-limited pitching (windowed sinc) instead of linear interpolation
        let reversed: Vec<f32>;
        let source = if p.reverse_keys.contains(&(key.clamp(0, 127) as u8)) {
            reversed = data.iter().rev().copied().collect();
            &reversed[..]
        } else {
            data
        };
        let src = crate::resample::pitch_read(source, rate as f64, n);
        let decay = p
            .key_decay_ms
            .get(&(key.clamp(0, 127) as u8))
            .copied()
            .filter(|v| v.is_finite() && *v > 0.)
            .map(|v| v.clamp(10., 2000.) * 0.001 * SR);
        let lp = p
            .key_lowpass_hz
            .get(&(key.clamp(0, 127) as u8))
            .copied()
            .filter(|v| v.is_finite() && *v > 0.)
            .map(|v| 1. - (-2. * std::f32::consts::PI * v.clamp(100., 18000.) / SR).exp());
        let (mut low1, mut low2) = (0f32, 0f32);
        let hp = p
            .key_highpass_hz
            .get(&(key.clamp(0, 127) as u8))
            .copied()
            .filter(|v| v.is_finite() && *v > 0.)
            .map(|v| (-2. * std::f32::consts::PI * v.clamp(20., 4000.) / SR).exp());
        let (mut hp_x1, mut hp_y1, mut hp_x2, mut hp_y2) = (0f32, 0f32, 0f32, 0f32);
        for (i, o) in out.iter_mut().enumerate().take(n) {
            let a = (i as f32 / att).min(1.0);
            let r = if p.one_shot || i < gate_n {
                1.0
            } else {
                let x = (i - gate_n) as f32 / rel_n;
                (1.0 - x).max(0.0).powi(2)
            };
            let fade_n = if capped { (0.005 * SR).max(64.0) } else { 64.0 };
            let tail = ((n - i) as f32 / fade_n).min(1.0);
            let d = decay.map_or(1., |half_samples| 2f32.powf(-(i as f32) / half_samples));
            let sample = if let Some(alpha) = lp {
                low1 += alpha * (src[i] - low1);
                low2 += alpha * (low1 - low2);
                low2
            } else {
                src[i]
            };
            let sample = if let Some(pole) = hp {
                let gain = (1. + pole) * 0.5;
                let first = gain * (sample - hp_x1) + pole * hp_y1;
                hp_x1 = sample;
                hp_y1 = first;
                let second = gain * (first - hp_x2) + pole * hp_y2;
                hp_x2 = first;
                hp_y2 = second;
                second
            } else {
                sample
            };
            *o += sample * a * r * tail * g * d;
        }
    }
    out
}

// ---------------- mapping helpers ----------------

/// Parse a note token like "C4", "F#2", "Bb1", "A-1".
pub fn note_token(t: &str) -> Option<u8> {
    let b = t.as_bytes();
    if b.is_empty() || !(b'A'..=b'G').contains(&b[0].to_ascii_uppercase()) {
        return None;
    }
    let rest = &t[1..];
    let (acc, num) = if let Some(r) = rest.strip_prefix('#') {
        (1, r)
    } else if let Some(r) = rest.strip_prefix("b").filter(|r| !r.is_empty()) {
        (-1, r)
    } else {
        (0, rest)
    };
    let oct: i32 = num.parse().ok()?;
    if !(-1..=9).contains(&oct) {
        return None;
    }
    let base = match b[0].to_ascii_uppercase() {
        b'C' => 0,
        b'D' => 2,
        b'E' => 4,
        b'F' => 5,
        b'G' => 7,
        b'A' => 9,
        _ => 11,
    };
    let m = (oct + 1) * 12 + base + acc;
    (0..=127).contains(&m).then_some(m as u8)
}

fn dyn_rank(t: &str) -> Option<i32> {
    let l = t.to_lowercase();
    match l.as_str() {
        "ppp" => return Some(0),
        "pp" => return Some(1),
        "p" => return Some(2),
        "mp" => return Some(3),
        "mf" => return Some(4),
        "f" => return Some(5),
        "ff" => return Some(6),
        "fff" => return Some(7),
        _ => {}
    }
    for pre in ["dyn", "vl", "v"] {
        if let Some(n) = l.strip_prefix(pre) {
            if let Ok(x) = n.parse::<i32>() {
                return Some(x);
            }
        }
    }
    None
}

/// A sample file name parsed into (root, dynamic rank, round robin).
pub fn parse_sample_name(file: &str) -> Option<(u8, i32, u8)> {
    let stem = file.rsplit('/').next().unwrap_or(file);
    let stem = stem.rsplit_once('.').map(|x| x.0).unwrap_or(stem);
    let toks: Vec<&str> = stem
        .split(['_', '-', ' '])
        .filter(|t| !t.is_empty())
        .collect();
    let mut root = None;
    let mut dynr = 0;
    let mut rr = 0u8;
    for (i, t) in toks.iter().enumerate() {
        if root.is_none() {
            if let Some(n) = note_token(t) {
                root = Some(n);
                continue;
            }
        }
        if let Some(d) = dyn_rank(t) {
            if root.is_some() {
                dynr = d;
                continue;
            }
        }
        let l = t.to_lowercase();
        if let Some(n) = l.strip_prefix("rr").and_then(|n| n.parse::<u8>().ok()) {
            rr = n.saturating_sub(1);
        } else if i == toks.len() - 1 && root.is_some() {
            if let Ok(n) = l.parse::<u8>() {
                rr = n.saturating_sub(1);
            }
        }
    }
    root.map(|r| (r, dynr, rr))
}

/// Build key / velocity zones from (sample name, root, dyn rank, rr).
pub fn zones_from(entries: &[(String, u8, i32, u8)]) -> Vec<Zone> {
    let mut roots: Vec<u8> = entries.iter().map(|e| e.1).collect();
    roots.sort_unstable();
    roots.dedup();
    let mut dyns: Vec<i32> = entries.iter().map(|e| e.2).collect();
    dyns.sort_unstable();
    dyns.dedup();
    let nd = dyns.len().max(1) as f32;
    entries
        .iter()
        .map(|(name, root, d, rr)| {
            let i = roots.iter().position(|x| x == root).unwrap_or(0);
            let lo = if i == 0 {
                0
            } else {
                ((roots[i - 1] as u16 + *root as u16) / 2 + 1) as u8
            };
            let hi = if i + 1 == roots.len() {
                127
            } else {
                ((*root as u16 + roots[i + 1] as u16) / 2) as u8
            };
            let di = dyns.iter().position(|x| x == d).unwrap_or(0) as f32;
            Zone {
                sample: name.clone(),
                root: *root,
                lo_note: lo,
                hi_note: hi,
                vel_lo: di / nd,
                vel_hi: (di + 1.0) / nd,
                tune_cents: 0.0,
                gain_db: 0.0,
                rr: *rr,
            }
        })
        .collect()
}

/// One `<region>` of an SFZ file with its opcodes resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct SfzRegion {
    pub sample: String,
    pub root: u8,
    pub lo: u8,
    pub hi: u8,
    pub vel_lo: u8,
    pub vel_hi: u8,
    pub tune: f32,
    pub volume: f32,
    pub seq: u8,
    /// trigger opcode (attack, release, first, legato); "" = attack.
    pub trigger: String,
    /// Keyswitch this region belongs to (sw_last), if any.
    pub sw_last: Option<u8>,
    /// Random round-robin window start (lorand), 0 when unused.
    pub lorand: f32,
}

/// Regions that play on a plain note-on: attack triggers only, and when the
/// instrument uses keyswitches, only the default articulation (sw_default,
/// else the lowest keyswitch). lorand windows become round-robin slots.
pub fn playable_regions(regions: Vec<SfzRegion>) -> Vec<SfzRegion> {
    // `*sine`, `*silence` ... are built-in generators, not files
    let mut v: Vec<SfzRegion> = regions
        .into_iter()
        .filter(|r| !r.sample.rsplit('/').next().unwrap_or("").starts_with('*'))
        .filter(|r| r.trigger.is_empty() || r.trigger == "attack" || r.trigger == "first")
        .collect();
    let sws: Vec<u8> = v.iter().filter_map(|r| r.sw_last).collect();
    if let Some(&first) = sws.iter().min() {
        v.retain(|r| r.sw_last.is_none_or(|s| s == first));
    }
    let mut rands: Vec<f32> = v.iter().map(|r| r.lorand).filter(|x| *x > 0.0).collect();
    rands.sort_by(|a, b| a.partial_cmp(b).unwrap());
    rands.dedup();
    if !rands.is_empty() {
        for r in v.iter_mut() {
            if r.seq <= 1 {
                r.seq = 1 + rands
                    .iter()
                    .position(|x| (*x - r.lorand).abs() < 1e-6)
                    .map(|p| p + 1)
                    .unwrap_or(0) as u8;
            }
        }
    }
    v
}

fn sfz_key(v: &str) -> Option<u8> {
    v.parse::<u8>().ok().or_else(|| {
        let mut s = v.to_string();
        if let Some(c) = s.get_mut(0..1) {
            c.make_ascii_uppercase();
        }
        note_token(&s)
    })
}

/// Minimal SFZ parser: <control> default_path, <global>/<group>/<region>
/// inheritance; sample, key, lokey, hikey, pitch_keycenter, lovel, hivel,
/// tune, volume, seq_position. Returns regions and the release time.
pub fn parse_sfz(text: &str) -> Result<(Vec<SfzRegion>, Option<f32>)> {
    let mut default_path = String::new();
    let mut global: Vec<(String, String)> = Vec::new();
    let mut group: Vec<(String, String)> = Vec::new();
    // <master> sits above <group>: a <group> clears only its own opcodes (Big
    // Rusty Drums sets key= on <master> and opens <group>s under it; clearing
    // both left every zone on keys 0-127, so each drum note played any drum)
    let mut master: Vec<(String, String)> = Vec::new();
    let mut regions = Vec::new();
    let mut release = None;
    let mut cur: Option<Vec<(String, String)>> = None;
    let mut section = String::new();
    let flush = |cur: &mut Option<Vec<(String, String)>>,
                 out: &mut Vec<SfzRegion>,
                 global: &[(String, String)],
                 group: &[(String, String)],
                 default_path: &str,
                 release: &mut Option<f32>| {
        if let Some(r) = cur.take() {
            let all: Vec<&(String, String)> =
                global.iter().chain(group.iter()).chain(r.iter()).collect();
            let get = |k: &str| {
                all.iter()
                    .rev()
                    .find(|(a, _)| a == k)
                    .map(|(_, v)| v.clone())
            };
            let Some(sample) = get("sample") else { return };
            let key = get("key").and_then(|k| sfz_key(&k));
            let root = get("pitch_keycenter")
                .and_then(|k| sfz_key(&k))
                .or(key)
                .unwrap_or(60);
            let lo = get("lokey").and_then(|k| sfz_key(&k)).or(key).unwrap_or(0);
            let hi = get("hikey")
                .and_then(|k| sfz_key(&k))
                .or(key)
                .unwrap_or(127);
            if let Some(r) = get("ampeg_release").and_then(|v| v.parse::<f32>().ok()) {
                *release = Some(r);
            }
            out.push(SfzRegion {
                sample: format!("{default_path}{}", sample.replace('\\', "/")),
                root,
                lo,
                hi,
                vel_lo: get("lovel").and_then(|v| v.parse().ok()).unwrap_or(0),
                vel_hi: get("hivel").and_then(|v| v.parse().ok()).unwrap_or(127),
                tune: get("tune").and_then(|v| v.parse().ok()).unwrap_or(0.0),
                volume: get("volume").and_then(|v| v.parse().ok()).unwrap_or(0.0),
                seq: get("seq_position")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1),
                trigger: get("trigger").unwrap_or_default().to_lowercase(),
                sw_last: get("sw_last").and_then(|k| sfz_key(&k)),
                lorand: get("lorand").and_then(|v| v.parse().ok()).unwrap_or(0.0),
            });
        }
    };
    // strip comments
    let clean: String = text
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    let mut rest = clean.as_str();
    while !rest.is_empty() {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        if let Some(r) = rest.strip_prefix('<') {
            let end = r.find('>').unwrap_or(r.len());
            let header = r[..end].trim().to_lowercase();
            rest = &r[(end + 1).min(r.len())..];
            flush(
                &mut cur,
                &mut regions,
                &global,
                &[master.clone(), group.clone()].concat(),
                &default_path,
                &mut release,
            );
            section = header.clone();
            match header.as_str() {
                "region" => cur = Some(Vec::new()),
                "group" => group.clear(),
                "master" => {
                    master.clear();
                    group.clear();
                }
                "global" => global.clear(),
                _ => {}
            }
            continue;
        }
        // opcode=value (value may contain spaces for `sample`, up to next opcode/header)
        let eq = match rest.find('=') {
            Some(i) => i,
            None => break,
        };
        let key = rest[..eq].trim().to_string();
        let after = &rest[eq + 1..];
        // value ends at the next "<" or the next " word=" token
        let mut end = after.len();
        if let Some(i) = after.find('<') {
            end = end.min(i);
        }
        let bytes = after.as_bytes();
        let mut i = 0;
        while i < end {
            if bytes[i].is_ascii_whitespace() {
                let tail = &after[i..end];
                let t = tail.trim_start();
                let word_end = t
                    .find(|c: char| c.is_whitespace() || c == '=')
                    .unwrap_or(t.len());
                if t[word_end..].starts_with('=') && word_end > 0 {
                    end = i;
                    break;
                }
            }
            i += 1;
        }
        let val = after[..end].trim().to_string();
        rest = &after[end..];
        let kv = (key.to_lowercase(), val);
        match section.as_str() {
            "control" => {
                if kv.0 == "default_path" {
                    default_path = kv.1.replace('\\', "/");
                }
            }
            "global" => global.push(kv),
            "group" => group.push(kv),
            "master" => master.push(kv),
            "region" => {
                if let Some(c) = cur.as_mut() {
                    c.push(kv)
                }
            }
            _ => {}
        }
    }
    flush(
        &mut cur,
        &mut regions,
        &global,
        &[master.clone(), group.clone()].concat(),
        &default_path,
        &mut release,
    );
    if regions.is_empty() {
        bail!("no <region> with a sample= found in the SFZ");
    }
    Ok((regions, release))
}

/// A CC0 instrument from VSCO 2 Community Edition (github.com/sgossner/VSCO-2-CE).
pub struct Pack {
    pub name: &'static str,
    pub folder: &'static str,
    pub description: &'static str,
    pub release: f32,
    pub one_shot: bool,
}

pub const PACK_REPO: &str = "sgossner/VSCO-2-CE";
pub const PACK_LICENSE: &str = "CC0 1.0 (VSCO 2 Community Edition, Versilian Studios)";

pub const PACKS: &[Pack] = &[
    Pack {
        name: "upright_piano",
        folder: "Keys/Upright Nr1",
        description: "Upright piano, 3 velocity layers x 2 round robins, C and G of every octave",
        release: 0.5,
        one_shot: false,
    },
    Pack {
        name: "violins_sus",
        folder: "Strings/Violin Section/susVib",
        description: "Violin section sustain with vibrato, 2 dynamics",
        release: 0.45,
        one_shot: false,
    },
    Pack {
        name: "violins_spic",
        folder: "Strings/Violin Section/Spic",
        description: "Violin section spiccato (short)",
        release: 0.15,
        one_shot: true,
    },
    Pack {
        name: "violins_pizz",
        folder: "Strings/Violin Section/Pizz",
        description: "Violin section pizzicato",
        release: 0.2,
        one_shot: true,
    },
    Pack {
        name: "violins_trem",
        folder: "Strings/Violin Section/Trem",
        description: "Violin section tremolo (tension)",
        release: 0.4,
        one_shot: false,
    },
    Pack {
        name: "violas_sus",
        folder: "Strings/Viola Section/susvib",
        description: "Viola section sustain",
        release: 0.45,
        one_shot: false,
    },
    Pack {
        name: "celli_sus",
        folder: "Strings/Cello Section/susvib",
        description: "Cello section sustain with vibrato",
        release: 0.5,
        one_shot: false,
    },
    Pack {
        name: "celli_spic",
        folder: "Strings/Cello Section/spic",
        description: "Cello section spiccato",
        release: 0.15,
        one_shot: true,
    },
    Pack {
        name: "celli_pizz",
        folder: "Strings/Cello Section/pizzT",
        description: "Cello section pizzicato",
        release: 0.2,
        one_shot: true,
    },
    Pack {
        name: "contrabass_sus",
        folder: "Strings/Solo Contrabass/SusVib",
        description: "Solo contrabass sustain",
        release: 0.5,
        one_shot: false,
    },
    Pack {
        name: "harp",
        folder: "Strings/Harp",
        description: "Concert harp",
        release: 1.0,
        one_shot: true,
    },
    Pack {
        name: "flute_sus",
        folder: "Woodwinds/Flute/susvib",
        description: "Flute sustain with vibrato",
        release: 0.3,
        one_shot: false,
    },
    Pack {
        name: "clarinet_sus",
        folder: "Woodwinds/Clarinet/susLong",
        description: "Clarinet long sustain",
        release: 0.3,
        one_shot: false,
    },
    Pack {
        name: "oboe_sus",
        folder: "Woodwinds/Oboe/Sus",
        description: "Oboe sustain",
        release: 0.3,
        one_shot: false,
    },
    Pack {
        name: "bassoon_sus",
        folder: "Woodwinds/Bassoon/sus",
        description: "Bassoon sustain",
        release: 0.3,
        one_shot: false,
    },
    Pack {
        name: "horn_sus",
        folder: "Brass/F Horn/sus",
        description: "French horn sustain (cinematic)",
        release: 0.4,
        one_shot: false,
    },
    Pack {
        name: "trumpet_sus",
        folder: "Brass/Trumpet/sus",
        description: "Trumpet sustain",
        release: 0.3,
        one_shot: false,
    },
    Pack {
        name: "trombone_sus",
        folder: "Brass/Tenor Trombone/sus",
        description: "Tenor trombone sustain",
        release: 0.35,
        one_shot: false,
    },
    Pack {
        name: "tuba_sus",
        folder: "Brass/Tuba/sus",
        description: "Tuba sustain",
        release: 0.35,
        one_shot: false,
    },
    Pack {
        name: "glockenspiel",
        folder: "Percussion/Glock",
        description: "Glockenspiel (real, C of each octave)",
        release: 1.2,
        one_shot: true,
    },
    Pack {
        name: "marimba",
        folder: "Percussion/Marimba",
        description: "Marimba, hard mallets (real)",
        release: 0.8,
        one_shot: true,
    },
    Pack {
        name: "xylophone",
        folder: "Percussion/Xylo",
        description: "Xylophone (real)",
        release: 0.5,
        one_shot: true,
    },
    Pack {
        name: "trumpet_stac",
        folder: "Brass/Trumpet/stac",
        description: "Trumpet staccato, 2 dynamics x round robins (brass stabs)",
        release: 0.15,
        one_shot: true,
    },
    Pack {
        name: "trumpet_mute_sus",
        folder: "Brass/Trumpet/straightM-sus",
        description: "Trumpet with straight mute, sustain",
        release: 0.35,
        one_shot: false,
    },
    Pack {
        name: "horn_stac",
        folder: "Brass/F Horn/stac",
        description: "French horn staccato (cinematic hits)",
        release: 0.15,
        one_shot: true,
    },
    Pack {
        name: "trombone_stac",
        folder: "Brass/Tenor Trombone/stac",
        description: "Tenor trombone staccato (brass stabs)",
        release: 0.15,
        one_shot: true,
    },
    Pack {
        name: "tuba_stac",
        folder: "Brass/Tuba/stac",
        description: "Tuba staccato (low brass punches)",
        release: 0.15,
        one_shot: true,
    },
    Pack {
        name: "flute_stac",
        folder: "Woodwinds/Flute/stac",
        description: "Flute staccato",
        release: 0.12,
        one_shot: true,
    },
    Pack {
        name: "clarinet_stac",
        folder: "Woodwinds/Clarinet/stac",
        description: "Clarinet staccato",
        release: 0.12,
        one_shot: true,
    },
    Pack {
        name: "oboe_stac",
        folder: "Woodwinds/Oboe/Stacc",
        description: "Oboe staccato",
        release: 0.12,
        one_shot: true,
    },
    Pack {
        name: "bassoon_stac",
        folder: "Woodwinds/Bassoon/stac",
        description: "Bassoon staccato",
        release: 0.12,
        one_shot: true,
    },
    Pack {
        name: "piccolo_sus",
        folder: "Woodwinds/Piccolo/Sus",
        description: "Piccolo sustain (bright top line)",
        release: 0.35,
        one_shot: false,
    },
    Pack {
        name: "violas_pizz",
        folder: "Strings/Viola Section/pizz",
        description: "Viola section pizzicato",
        release: 0.2,
        one_shot: true,
    },
    Pack {
        name: "contrabass_pizz",
        folder: "Strings/Solo Contrabass/Pizz",
        description: "Solo double bass pizzicato (real upright-style bass)",
        release: 0.25,
        one_shot: true,
    },
];

pub fn pack(name: &str) -> Option<&'static Pack> {
    let n = name.trim().to_lowercase().replace([' ', '-'], "_");
    let alias = match n.as_str() {
        "piano" | "grand_piano" | "felt_piano" => "upright_piano",
        "violins" | "strings" | "string_section" => "violins_sus",
        "staccato_strings" => "violins_spic",
        "cellos" | "celli" | "cello" => "celli_sus",
        "violas" => "violas_sus",
        "flute" => "flute_sus",
        "horn" | "french_horn" => "horn_sus",
        other => other,
    };
    PACKS.iter().find(|p| p.name == alias)
}

#[cfg(test)]
mod tests {
    #[test]
    fn master_key_survives_groups_under_it() {
        let t = "<master> key=38\n<group> amp_velcurve_1=1\n<region> sample=a.wav\n<master> key=36\n<group>\n<region> sample=b.wav\n";
        let (r, _) = super::parse_sfz(t).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].lo, r[0].hi, r[0].root), (38, 38, 38));
        assert_eq!((r[1].lo, r[1].hi), (36, 36));
    }

    use super::*;

    #[test]
    fn new_vsco_packs_have_pitched_file_names() {
        let cases: &[(&str, &[&str])] = &[
            (
                "glockenspiel",
                &[
                    "glock_medium_C5.wav",
                    "glock_medium_C6.wav",
                    "glock_medium_G6.wav",
                ],
            ),
            (
                "marimba",
                &[
                    "Marimba_hit_Outrigger_B2_loud_01.wav",
                    "Marimba_hit_Outrigger_B4_loud_01.wav",
                    "Marimba_hit_Outrigger_G4_loud_01.wav",
                ],
            ),
            (
                "xylophone",
                &[
                    "Xylo_Medium_C4_ff_01_far.wav",
                    "Xylo_Medium_C5_ff_01_far.wav",
                    "Xylo_Medium_G6_ff_01_far.wav",
                ],
            ),
            (
                "trumpet_stac",
                &[
                    "Sum_SHTrumpet_stac_A#3_v1_rr1.wav",
                    "Sum_SHTrumpet_stac_A#3_v1_rr2.wav",
                    "Sum_SHTrumpet_stac_G3_v3_rr2.wav",
                ],
            ),
            (
                "trumpet_mute_sus",
                &[
                    "Sum_SHTrumpet_straightM-sus_A#2_v1_rr1.wav",
                    "Sum_SHTrumpet_straightM-sus_A#2_v3_rr1.wav",
                    "Sum_SHTrumpet_straightM-sus_G3_v3_rr1.wav",
                ],
            ),
            (
                "horn_stac",
                &[
                    "MOHorn_stac_A#1_v1_rr1.wav",
                    "MOHorn_stac_A#1_v2_rr1.wav",
                    "MOHorn_stac_G1_v2_rr2.wav",
                ],
            ),
            (
                "trombone_stac",
                &[
                    "tenortbn_stac_A#0_v1_rr1.wav",
                    "tenortbn_stac_A#0_v1_rr2.wav",
                    "tenortbn_stac_F3_v4_rr2.wav",
                ],
            ),
            (
                "tuba_stac",
                &[
                    "Tuba3_stac_A#0_v1_rr1_Sum.wav",
                    "Tuba3_stac_A#0_v1_rr2_Sum.wav",
                    "Tuba3_stac_F2_v2_rr4_Sum.wav",
                ],
            ),
            (
                "flute_stac",
                &[
                    "LDFlute_stac_A3_v1_rr1.wav",
                    "LDFlute_stac_A3_v1_rr2.wav",
                    "LDFlute_stac_E5_v3_rr2.wav",
                ],
            ),
            (
                "clarinet_stac",
                &[
                    "DCClar_stac_A#2_v1_rr1_sum.wav",
                    "DCClar_stac_A#2_v1_rr2_sum.wav",
                    "DCClar_stac_F5_v3_rr2_sum.wav",
                ],
            ),
            (
                "oboe_stac",
                &[
                    "Oboe_Stacc_A#2_v1_rr1_Main.wav",
                    "Oboe_Stacc_A#2_v1_rr2_Main.wav",
                    "Oboe_Stacc_F5_v3_rr2_Main.wav",
                ],
            ),
            (
                "bassoon_stac",
                &[
                    "PSBassoon_A#0_v2_rr1.wav",
                    "PSBassoon_A#0_v2_rr2.wav",
                    "PSBassoon_G3_v2_rr2.wav",
                ],
            ),
            (
                "piccolo_sus",
                &[
                    "piccolo_C5_sustain1.wav",
                    "piccolo_C6_sustain1.wav",
                    "piccolo_G6_sustain1.wav",
                ],
            ),
            (
                "violas_pizz",
                &[
                    "ViolaEns_pizz_A3_v1_rr1.wav",
                    "ViolaEns_pizz_A3_v1_rr2.wav",
                    "ViolaEns_pizz_G4_v2_rr2.wav",
                ],
            ),
            (
                "contrabass_pizz",
                &[
                    "BKCtbss_Pizz_A#0_v1_rr1.wav",
                    "BKCtbss_Pizz_A#0_v1_rr2.wav",
                    "BKCtbss_Pizz_G0_v3_rr2.wav",
                ],
            ),
        ];
        for (pack_name, files) in cases {
            assert!(pack(pack_name).is_some(), "{pack_name}");
            for f in *files {
                let (key, _, _) =
                    parse_sample_name(f).unwrap_or_else(|| panic!("{pack_name}: {f}"));
                assert!((12..=108).contains(&key), "{f} -> {key}");
            }
        }
    }

    #[test]
    fn filename_parsing() {
        assert_eq!(parse_sample_name("UR1_C4_mf_RR2.wav"), Some((60, 4, 1)));
        assert_eq!(
            parse_sample_name("VlnEns_susVib_A2_v1.wav"),
            Some((45, 1, 0))
        );
        assert_eq!(parse_sample_name("susvib_B1_v3_1.wav"), Some((35, 3, 0)));
        assert_eq!(
            parse_sample_name("MOHorn_sus_A#1_v2_1.wav"),
            Some((34, 2, 0))
        );
        assert_eq!(parse_sample_name("KSHarp_A4_mf.wav"), Some((69, 4, 0)));
        assert_eq!(parse_sample_name("Player_dyn1_rr1_000.wav"), None);
    }

    #[test]
    fn zones_and_picking() {
        let e = vec![
            ("c4p".to_string(), 60, 1, 0),
            ("c4f".to_string(), 60, 5, 0),
            ("c4f2".to_string(), 60, 5, 1),
            ("g4p".to_string(), 67, 1, 0),
            ("g4f".to_string(), 67, 5, 0),
        ];
        let z = zones_from(&e);
        assert_eq!((z[0].lo_note, z[0].hi_note), (0, 63));
        assert_eq!((z[3].lo_note, z[3].hi_note), (64, 127));
        let p = MultisampleParams {
            zones: z,
            vel_crossfade: false,
            ..Default::default()
        };
        let a = pick_zones(&p, 62, 0.9, 0);
        assert_eq!(p.zones[a[0].0].sample, "c4f");
        let b = pick_zones(&p, 62, 0.9, 1);
        assert_eq!(p.zones[b[0].0].sample, "c4f2");
        let c = pick_zones(&p, 70, 0.1, 0);
        assert_eq!(p.zones[c[0].0].sample, "g4p");
        let pc = MultisampleParams {
            vel_crossfade: true,
            ..p.clone()
        };
        let d = pick_zones(&pc, 60, 0.52, 0);
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn sfz_parse() {
        let s = "<control> default_path=samples/\n<group> ampeg_release=0.6 lovel=0 hivel=64\n<region> sample=piano C4 soft.wav lokey=58 hikey=62 pitch_keycenter=c4\n<region> sample=d4.wav key=62 // comment\n<group> lovel=65 hivel=127\n<region> sample=loud.wav lokey=0 hikey=127 pitch_keycenter=60 tune=-5";
        let (r, rel) = parse_sfz(s).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].sample, "samples/piano C4 soft.wav");
        assert_eq!((r[0].lo, r[0].hi, r[0].root, r[0].vel_hi), (58, 62, 60, 64));
        assert_eq!((r[1].lo, r[1].hi, r[1].root), (62, 62, 62));
        assert_eq!((r[2].vel_lo, r[2].tune), (65, -5.0));
        assert_eq!(rel, Some(0.6));
    }
}

#[cfg(test)]
mod kit_piece_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn piece_level_absolute_history_and_audio() {
        let mut e = beatbox::Engine::new("/tmp/piece-test-work".into());
        e.call(
            "load_instrument",
            &json!({"track":"kit","catalog_id":"vcsl_acoustic_drum_kit","max_mb":100}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let render = |e: &beatbox::Engine, key: f32| {
            let beatbox::instruments::Instrument::Multisample(p) = &e.project.tracks[0].instrument
            else {
                panic!()
            };
            beatbox::multisample::render_multisample(p, key, 0.8, 0.2, &e.bank, 0)
        };
        let kick = render(&e, 36.);
        let snare = render(&e, 38.);
        let b = serde_json::to_value(&e.project).unwrap();
        let a = json!({"track":"kit","key":38,"gain_db":-6});
        e.call(
            "instrument",
            &json!({"action":"set_kit_piece_level","params":a}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut ex = b.clone();
        ex["tracks"][0]["instrument"]["key_trim_db"] = json!({"38":-6.0});
        assert_eq!(ex, after);
        assert_eq!(render(&e, 36.), kick);
        let quiet = render(&e, 38.);
        let g = 10f32.powf(-6. / 20.);
        assert!(quiet
            .iter()
            .zip(&snare)
            .all(|(a, b)| (a - b * g).abs() < 1e-6));
        let h = e.history().0.len();
        e.call("set_kit_piece_level", &a).unwrap();
        assert_eq!(e.history().0.len(), h);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        for bad in [
            json!({"track":"kit","key":38,"gain_db":7}),
            json!({"track":"kit","key":38.5,"gain_db":0}),
        ] {
            assert!(e.call("set_kit_piece_level", &bad).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        }
        e.call(
            "set_kit_piece_level",
            &json!({"track":"kit","key":38,"gain_db":0}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod kit_tuning_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn tuning_absolute_mapping_audio_history() {
        let mut e = beatbox::Engine::new("/tmp/tune-tests-work".into());
        e.call(
            "load_instrument",
            &json!({"track":"kit","catalog_id":"vcsl_acoustic_drum_kit","max_mb":100}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let render = |p: &beatbox::multisample::MultisampleParams, key: f32| {
            beatbox::multisample::render_multisample(p, key, 0.8, 0.2, &e.bank, 0)
        };
        let beatbox::instruments::Instrument::Multisample(basep) =
            e.project.tracks[0].instrument.clone()
        else {
            panic!()
        };
        let kick = render(&basep, 36.);
        let snare = render(&basep, 38.);
        let b = serde_json::to_value(&e.project).unwrap();
        let a = json!({"track":"kit","key":38,"cents":-400});
        e.call(
            "instrument",
            &json!({"action":"set_kit_piece_tuning","params":a}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut ex = b.clone();
        ex["tracks"][0]["instrument"]["key_tune_cents"] = json!({"38":-400.0});
        assert_eq!(ex, after);
        let beatbox::instruments::Instrument::Multisample(p) =
            e.project.tracks[0].instrument.clone()
        else {
            panic!()
        };
        let tuned = beatbox::multisample::render_multisample(&p, 38., 0.8, 0.2, &e.bank, 0);
        let mut oracle = basep.clone();
        for z in &mut oracle.zones {
            if z.root == 38 {
                z.tune_cents -= 400.;
            }
        }
        assert_eq!(
            tuned,
            beatbox::multisample::render_multisample(&oracle, 38., 0.8, 0.2, &e.bank, 0)
        );
        assert_ne!(tuned, snare);
        assert!(tuned.len() > snare.len());
        assert_eq!(
            kick,
            beatbox::multisample::render_multisample(&p, 36., 0.8, 0.2, &e.bank, 0)
        );
        let h = e.history().0.len();
        e.call("set_kit_piece_tuning", &a).unwrap();
        assert_eq!(h, e.history().0.len());
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        for bad in [
            json!({"track":"kit","key":38,"cents":1201}),
            json!({"track":"kit","key":38.5,"cents":0}),
            json!({"track":"kit","key":37,"cents":0}),
        ] {
            assert!(e.call("set_kit_piece_tuning", &bad).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        }
        e.call(
            "set_kit_piece_tuning",
            &json!({"track":"kit","key":38,"cents":0}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod kit_length_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn piece_length_audio_history_stream() {
        let mut e = beatbox::Engine::new("/tmp/length-test-work".into());
        e.project.tracks.clear();
        e.call(
            "load_instrument",
            &json!({"track":"kit","catalog_id":"vcsl_acoustic_drum_kit","max_mb":100}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let r = |e: &beatbox::Engine, k| {
            let beatbox::instruments::Instrument::Multisample(p) = &e.project.tracks[0].instrument
            else {
                panic!()
            };
            beatbox::multisample::render_multisample(p, k, 0.8, 0.2, &e.bank, 0)
        };
        let kick = r(&e, 36.);
        let snare = r(&e, 38.);
        let b = serde_json::to_value(&e.project).unwrap();
        let a = json!({"track":"kit","key":38,"length_ms":120});
        e.call(
            "instrument",
            &json!({"action":"set_kit_piece_length","params":a}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut ex = b.clone();
        ex["tracks"][0]["instrument"]["key_length_ms"] = json!({"38":120.0});
        assert_eq!(ex, after);
        let short = r(&e, 38.);
        assert_eq!(short.len(), 5292);
        assert!(short.len() < snare.len());
        assert_eq!(&short[..5000], &snare[..5000]);
        for i in 5072..short.len() {
            let fade = ((short.len() - i) as f32 / (0.005 * 44100.0)).min(1.0);
            assert!((short[i] - snare[i] * fade).abs() < 1e-6);
        }
        assert_eq!(kick, r(&e, 36.));
        let h = e.history().0.len();
        e.call("set_kit_piece_length", &a).unwrap();
        assert_eq!(h, e.history().0.len());
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        for val in [-1, 1, 9, 5001] {
            assert!(e
                .call(
                    "set_kit_piece_length",
                    &json!({"track":"kit","key":38,"length_ms":val})
                )
                .is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        }
        e.call(
            "set_kit_piece_length",
            &json!({"track":"kit","key":38,"length_ms":0}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("set_kit_piece_length", &a).unwrap();
        e.call("add_notes",&json!({"track":"kit","notes":[{"start":0,"pitch":38,"vel":0.8,"len":1},{"start":4,"pitch":36,"vel":0.8,"len":1},{"start":8,"pitch":38,"vel":0.8,"len":1}]})).unwrap();
        let opts = beatbox::render::RenderOptions {
            keep_stems: true,
            ..Default::default()
        };
        let oracle = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        for block in [63, 512] {
            let mut stream = beatbox::rt::VoiceStreamer::new(&e.project, &e.bank, &opts, block, 0);
            let (l, r) = stream.render_all();
            assert_eq!(l, oracle.stems[0].left);
            assert_eq!(r, oracle.stems[0].right);
        }
    }
}

#[cfg(test)]
mod kit_persistence_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn combined_kit_controls_persistence_stream() {
        let mut e = beatbox::Engine::new("/tmp/combined-kit-work".into());
        e.project.bpm = 120.;
        e.project.swing = 0.;
        e.project.tracks.clear();
        e.project.master_effects.clear();
        e.project.patterns[0].bars = 4;
        e.call("load_instrument",&json!({"track":"kit","catalog_id":"vcsl_acoustic_drum_kit","max_mb":100,"volume_db":0})).unwrap();
        e.call("add_notes",&json!({"track":"kit","notes":[{"start":0,"pitch":38,"vel":0.8,"len":1},{"start":1,"pitch":36,"vel":0.7,"len":1},{"start":2,"pitch":46,"vel":0.7,"len":1},{"start":3,"pitch":42,"vel":0.7,"len":1},{"start":24,"pitch":38,"vel":0.8,"len":1}]})).unwrap();
        let original = serde_json::to_value(&e.project).unwrap();
        for (name, args) in [
            (
                "set_kit_piece_level",
                json!({"track":"kit","key":38,"gain_db":-4}),
            ),
            (
                "set_kit_piece_tuning",
                json!({"track":"kit","key":38,"cents":-300}),
            ),
            (
                "set_kit_piece_length",
                json!({"track":"kit","key":38,"length_ms":120}),
            ),
            (
                "set_sample_performance",
                json!({"track":"kit","gm_hat_choke":true}),
            ),
        ] {
            e.call("instrument", &json!({"action":name,"params":args}))
                .unwrap();
        }
        let configured = serde_json::to_value(&e.project).unwrap();
        let mut expected = original.clone();
        expected["tracks"][0]["instrument"]["key_trim_db"] = json!({"38":-4.0});
        expected["tracks"][0]["instrument"]["key_tune_cents"] = json!({"38":-300.0});
        expected["tracks"][0]["instrument"]["key_length_ms"] = json!({"38":120.0});
        expected["tracks"][0]["instrument"]["gm_hat_choke"] = json!(true);
        assert_eq!(configured, expected);
        e.bank.sync(&e.project.samples);
        let opts = beatbox::render::RenderOptions {
            keep_stems: true,
            ..Default::default()
        };
        let full = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        let stem = &full.stems[0];
        for block in [1, 63, 512, 4096] {
            let mut s = beatbox::rt::VoiceStreamer::new(&e.project, &e.bank, &opts, block, 0);
            let (l, r) = s.render_all();
            assert_eq!(l, stem.left);
            assert_eq!(r, stem.right);
        }

        e.call("save_project", &json!({"path":"combined.json"}))
            .unwrap();
        let mut reload = beatbox::Engine::new("/tmp/combined-kit-work".into());
        reload
            .call("load_project", &json!({"path":"combined.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&reload.project).unwrap(), configured);
        reload.bank.sync(&reload.project.samples);
        let roundtrip = beatbox::render::render(&reload.project, &reload.bank, &opts).unwrap();
        assert_eq!(roundtrip.left, full.left);
        assert_eq!(roundtrip.right, full.right);
        for _ in 0..4 {
            e.call("undo", &json!({})).unwrap();
        }
        assert_eq!(serde_json::to_value(&e.project).unwrap(), original);
        for _ in 0..4 {
            e.call("redo", &json!({})).unwrap();
        }
        assert_eq!(serde_json::to_value(&e.project).unwrap(), configured);
        e.call(
            "load_instrument",
            &json!({"track":"piano","catalog_id":"vcsl_upright_piano_yamaha_lite","max_mb":100}),
        )
        .unwrap();
        let melodic = serde_json::to_value(&e.project).unwrap();
        for (name, param) in [
            ("set_kit_piece_level", "gain_db"),
            ("set_kit_piece_tuning", "cents"),
            ("set_kit_piece_length", "length_ms"),
        ] {
            assert!(e
                .call(name, &json!({"track":"piano","key":38,param:100}))
                .is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), melodic);
        }
    }
}

#[cfg(test)]
mod velocity_gap_tests {
    use super::*;
    fn fixture() -> MultisampleParams {
        MultisampleParams {
            zones: vec![
                Zone {
                    sample: "low".into(),
                    root: 60,
                    lo_note: 60,
                    hi_note: 60,
                    vel_lo: 0.0,
                    vel_hi: 0.4,
                    tune_cents: 0.0,
                    gain_db: 0.0,
                    rr: 0,
                },
                Zone {
                    sample: "high".into(),
                    root: 60,
                    lo_note: 60,
                    hi_note: 60,
                    vel_lo: 0.419,
                    vel_hi: 1.0,
                    tune_cents: 0.0,
                    gain_db: 0.0,
                    rr: 0,
                },
            ],
            ..Default::default()
        }
    }
    #[test]
    fn gap_blends_are_convex_and_nearest_layer_keeps_half() {
        for p in [fixture(), {
            let mut p = fixture();
            p.zones[0].vel_lo = 0.39;
            p
        }] {
            for i in 0..=10000 {
                let picks = pick_zones(&p, 60, i as f32 / 10000.0, 0);
                assert!(picks[0].1 >= 0.5, "{i}: {picks:?}");
                assert!(picks
                    .iter()
                    .all(|(_, w)| w.is_finite() && *w >= 0.0 && *w <= 1.0));
                assert!((picks.iter().map(|x| x.1).sum::<f32>() - 1.0).abs() < 1e-6);
            }
        }
    }
    #[test]
    fn interior_disabled_and_round_robin_are_preserved() {
        let mut p = fixture();
        assert_eq!(pick_zones(&p, 60, 0.2, 0), vec![(0, 1.0)]);
        let v = 0.38;
        let expected = 0.5 * (1.0 - (0.4 - v) / 0.1);
        let picks = pick_zones(&p, 60, v, 0);
        assert!((picks[1].1 - expected).abs() < 1e-6);
        p.vel_crossfade = false;
        assert_eq!(pick_zones(&p, 60, 0.405, 0), vec![(0, 1.0)]);
        p.vel_crossfade = true;
        let mut rr = p.zones[0].clone();
        rr.rr = 1;
        rr.sample = "low_rr2".into();
        p.zones.push(rr);
        assert_eq!(pick_zones(&p, 60, 0.405, 1)[0].0, 2);
        let left = pick_zones(&p, 60, 0.40949, 0);
        let right = pick_zones(&p, 60, 0.40951, 0);
        assert_eq!(left, vec![(0, 0.5), (1, 0.5)]);
        assert_eq!(right, vec![(1, 0.5), (0, 0.5)]);
    }
}

#[cfg(test)]
mod recorded_velocity_gap_tests {
    use serde_json::json;
    #[test]
    fn actual_kit_gap_preview_and_streaming() {
        let mut e = crate::Engine::new("/tmp/recorded-gap-test".into());
        e.call(
            "load_project",
            &json!({"path":"/tmp/recorded-beat-project/recorded-groove.json"}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let p = match &e.project.tracks[0].instrument {
            crate::instruments::Instrument::Multisample(p) => p,
            _ => panic!(),
        };
        for key in [36, 38, 42, 46, 49] {
            for i in 0..=10000 {
                let picks = super::pick_zones(p, key, i as f32 / 10000.0, 1);
                assert!(picks[0].1 >= 0.5, "key{key} v{i}: {picks:?}");
                assert!(picks.iter().all(|(_, w)| *w >= 0.0 && *w <= 1.0));
            }
        }
        let expected = super::render_multisample(p, 38.0, 0.2032, 0.25, &e.bank, 0);
        let saved = serde_json::to_value(&e.project).unwrap();
        e.call(
            "preview_kit_piece",
            &json!({"track":"drums","key":38,"velocity":0.2032,"play":true}),
        )
        .unwrap();
        let heard = &e
            .transport
            .pending_audition
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .0;
        assert_eq!(expected.len(), heard.len());
        assert!(expected
            .iter()
            .zip(heard)
            .all(|(a, b)| (a * 10f32.powf(-6.0 / 20.0) - b).abs() < 1e-7));
        assert_eq!(serde_json::to_value(&e.project).unwrap(), saved);
        // Put the gap velocity into the song, so both playback paths really
        // exercise the corrected weights, not just ordinary interior notes.
        let mut v = saved;
        fn gap_notes(v: &mut serde_json::Value) {
            match v {
                serde_json::Value::Object(m) => {
                    if m.contains_key("vel") {
                        m.insert("vel".into(), json!(0.2032));
                    }
                    for x in m.values_mut() {
                        gap_notes(x);
                    }
                }
                serde_json::Value::Array(a) => {
                    for x in a {
                        gap_notes(x);
                    }
                }
                _ => (),
            }
        }
        gap_notes(&mut v);
        e.project = serde_json::from_value(v).unwrap();
        e.call(
            "apply_fx_chain",
            &json!({"track":"drums","chain":"short_drum_room"}),
        )
        .unwrap();
        for range in [None, Some((16, 32))] {
            let o = crate::render::RenderOptions {
                step_range: range,
                tail: 0.5,
                ..Default::default()
            };
            let expected = crate::render::render(&e.project, &e.bank, &o).unwrap();
            for block in [63, 512, 4096] {
                let mut m = crate::rt::MixNode::new(&e.project, &e.bank, &o, block);
                let (l, r) = m.render();
                assert_eq!(l, expected.left);
                assert_eq!(r, expected.right);
            }
        }
    }
}
