//! Multisampled instruments: key zones x velocity layers x round robins
//! (SFZ-style), an SFZ loader, filename-based zone mapping, and a catalog of
//! CC0 instrument packs from VSCO 2 Community Edition (real piano, string
//! sections, woodwinds, brass) that `install_instrument_pack` downloads.

use crate::dsp::{lerp_read, SR};
use crate::samples::SampleBank;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

fn one() -> f32 {
    1.0
}
fn max_key() -> u8 {
    127
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
    /// Blend neighbouring velocity layers near their boundary.
    pub vel_crossfade: bool,
    /// How much velocity changes level (0 = layers only, 1 = full).
    pub vel_to_gain: f32,
    pub gain: f32,
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
            vel_crossfade: true,
            vel_to_gain: 0.6,
            gain: 0.9,
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

/// Zones to play with their weights for a key / velocity / round-robin counter.
pub fn pick_zones(p: &MultisampleParams, key: i32, vel: f32, rr_counter: u64) -> Vec<(usize, f32)> {
    if p.zones.is_empty() {
        return vec![];
    }
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
    let vel_gain = 1.0 - p.vel_to_gain.clamp(0.0, 1.0) * (1.0 - vel);
    for (zi, w) in picks {
        let z = &p.zones[zi];
        let Some(data) = bank.get(&z.sample) else {
            continue;
        };
        if data.is_empty() {
            continue;
        }
        let rate = 2f32.powf((pitch - z.root as f32 + z.tune_cents / 100.0) / 12.0);
        let avail = data.len() as f32 / rate / SR;
        let len = if p.one_shot {
            avail
        } else {
            (gate + p.release.max(0.005)).min(avail)
        };
        let n = (len * SR) as usize;
        if out.len() < n {
            out.resize(n, 0.0);
        }
        let g = w * vel_gain * p.gain * crate::dsp::db_to_gain(z.gain_db);
        let att = (p.attack.max(0.0005) * SR).max(1.0);
        let rel_n = (p.release.max(0.005) * SR).max(1.0);
        let gate_n = (gate * SR) as usize;
        for (i, o) in out.iter_mut().enumerate().take(n) {
            let a = (i as f32 / att).min(1.0);
            let r = if p.one_shot || i < gate_n {
                1.0
            } else {
                let x = (i - gate_n) as f32 / rel_n;
                (1.0 - x).max(0.0).powi(2)
            };
            let tail = ((n - i) as f32 / 64.0).min(1.0);
            *o += lerp_read(data, i as f32 * rate) * a * r * tail * g;
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
                &group,
                &default_path,
                &mut release,
            );
            section = header.clone();
            match header.as_str() {
                "region" => cur = Some(Vec::new()),
                "group" | "master" => group.clear(),
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
            "group" | "master" => group.push(kv),
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
        &group,
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
    use super::*;

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
