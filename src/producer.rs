//! The autonomous producer: genre playbooks (data), a planner that writes a
//! structured song plan, a composer that turns the plan into a project, a
//! critic that listens to the render with Beatbox's own ears, and a reviser
//! that turns findings into plan/mix edits and loops until the score
//! plateaus. Every step is also a separate MCP tool (tools_producer.rs) so
//! an outside AI can steer it step by step.

use crate::dsp::Rng;
use crate::engine::Engine;
use crate::project::{Note, Pattern, Project, Section, STEPS_PER_BAR};
use crate::theory;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;

// ---------------------------------------------------------------- playbooks

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DrumGrammar {
    pub kit: BTreeMap<String, String>,
    #[serde(default)]
    pub sample_kit: Option<String>,
    pub kick: Vec<String>,
    pub snare: Vec<String>,
    pub hat: Vec<String>,
    #[serde(default)]
    pub open_hat: Vec<String>,
    #[serde(default)]
    pub perc: Vec<String>,
    #[serde(default)]
    pub tabla: Vec<String>,
    #[serde(default)]
    pub bayan: Vec<String>,
    pub ghost_snare: f32,
    pub hat_rolls: f32,
    pub hat_vel_accent: f32,
    pub half_time: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BassRule {
    pub preset: String,
    pub style: String,
    pub octave: i32,
    pub glide: f32,
    pub follow_kick: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PartRule {
    #[serde(default)]
    pub presets: Vec<String>,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub style: Option<String>,
    pub octave: i32,
    #[serde(default)]
    pub density: Option<f32>,
    #[serde(default)]
    pub ornament: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SectionTemplate {
    pub kind: String,
    pub bars: u32,
    pub energy: f32,
    pub layers: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MixRule {
    pub balance_genre: String,
    pub target_lufs: f32,
    pub reverb_send: f32,
    pub delay_send: f32,
    pub drum_bus: bool,
    pub sidechain_bass: f32,
    pub master_style: String,
    pub crush: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Playbook {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub description: String,
    pub bpm: [f32; 2],
    pub swing: f32,
    pub scales: Vec<String>,
    pub progressions: BTreeMap<String, Vec<String>>,
    pub bars_per_chord: f32,
    pub drums: DrumGrammar,
    pub bass: BassRule,
    pub harmony: PartRule,
    pub lead: PartRule,
    pub counter: PartRule,
    #[serde(default)]
    pub texture: Option<PartRule>,
    pub arrangement: Vec<SectionTemplate>,
    pub mix: MixRule,
}

pub fn playbooks() -> &'static [Playbook] {
    static P: OnceLock<Vec<Playbook>> = OnceLock::new();
    P.get_or_init(|| {
        serde_json::from_str(include_str!("playbooks.json")).expect("playbooks.json is valid")
    })
}

fn norm(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .replace([' ', '-', '/'], "_")
        .replace('&', "_and_")
}

/// Find a playbook by name or alias; with `fuzzy`, also by any word of a
/// free-text brief ("dark desi rap with sitar" -> desi_hiphop).
pub fn playbook(name: &str) -> Result<&'static Playbook> {
    let n = norm(name);
    let n = n.trim_matches('_');
    playbooks()
        .iter()
        .find(|p| p.name == n || p.aliases.iter().any(|a| a == n))
        .ok_or_else(|| {
            anyhow!(
                "unknown genre '{name}'. Genres: {}",
                playbooks()
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

pub fn guess_genre(brief: &str) -> Option<&'static Playbook> {
    let b = norm(brief);
    // longest match wins ("melodic trap" -> melodic_rap, not trap)
    let mut best: Option<(&Playbook, usize)> = None;
    for p in playbooks() {
        for k in std::iter::once(&p.name).chain(p.aliases.iter()) {
            let words: Vec<&str> = k.split('_').collect();
            let hit =
                b.contains(k.as_str()) || (words.len() > 1 && words.iter().all(|w| b.contains(w)));
            if hit && best.map(|(_, l)| k.len() > l).unwrap_or(true) {
                best = Some((p, k.len()));
            }
        }
    }
    if best.is_none() {
        for (kw, g) in [
            ("sitar", "desi_hiphop"),
            ("tabla", "desi_hiphop"),
            ("bansuri", "desi_hiphop"),
            ("808", "trap"),
            ("sad", "melodic_rap"),
            ("chill", "lofi"),
            ("jazz", "boom_bap"),
            ("soul", "rnb"),
        ] {
            if b.contains(kw) {
                return playbook(g).ok();
            }
        }
    }
    best.map(|(p, _)| p)
}

pub fn describe_playbook(p: &Playbook) -> Value {
    serde_json::to_value(p).unwrap_or(Value::Null)
}

// ---------------------------------------------------------------- the plan

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MotifNote {
    /// Start in steps within the motif bar.
    pub t: f32,
    /// Scale degree relative to the tonic (0 = tonic, 7 = octave up).
    pub deg: i32,
    pub len: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PlanSection {
    pub name: String,
    pub kind: String,
    pub bars: u32,
    pub energy: f32,
    pub layers: Vec<String>,
    /// How the lead treats the motif here: full, call, sparse, high, augment, none.
    pub lead: String,
    /// Fill/transition at the end of the section into the next one.
    pub transition: String,
}

/// Knobs the critic turns between iterations.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Knobs {
    pub lead_density: f32,
    pub hat_rolls: f32,
    pub ghost_snare: f32,
    pub glide: f32,
    /// Velocity scale for non-hook sections (contrast with the hook).
    pub verse_velocity: f32,
    pub sidechain: f32,
    pub variation_seed: u64,
    /// Extra dB per track on top of the genre balance targets.
    pub offsets: BTreeMap<String, f32>,
    pub target_lufs: f32,
    pub tonic_anchor: bool,
    pub humanize: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub title: String,
    pub brief: String,
    pub genre: String,
    pub mood: String,
    pub bpm: f32,
    pub key: String,
    pub scale: String,
    pub swing: f32,
    pub progression_verse: String,
    pub progression_hook: String,
    pub bars_per_chord: f32,
    pub motif: Vec<MotifNote>,
    /// role -> preset
    pub palette: BTreeMap<String, String>,
    /// drum part -> [verse pattern, hook pattern]
    pub drum_patterns: BTreeMap<String, [String; 2]>,
    pub sections: Vec<PlanSection>,
    pub knobs: Knobs,
    /// Use the playbook's real sample kit for drums when installed.
    pub sample_kit: Option<String>,
    /// A sample to flip (chop + rearrange) into intro/hook, if any.
    pub flip_sample: Option<String>,
    pub seed: u64,
    /// The producer's reasoning, in order.
    pub thinking: Vec<String>,
}

pub struct PlanArgs {
    pub brief: String,
    pub genre: Option<String>,
    pub bpm: Option<f32>,
    pub key: Option<String>,
    pub scale: Option<String>,
    pub mood: Option<String>,
    pub duration_s: Option<f32>,
    pub seed: u64,
    pub use_samples: bool,
    pub flip_sample: Option<String>,
}

const MOODS: &[(&str, &[&str])] = &[
    (
        "sad",
        &[
            "sad",
            "emotional",
            "heartbreak",
            "pain",
            "cry",
            "lonely",
            "melancholy",
            "dard",
            "rain",
        ],
    ),
    (
        "dark",
        &[
            "dark",
            "evil",
            "menacing",
            "hard",
            "aggressive",
            "street",
            "gritty",
            "night",
            "villain",
        ],
    ),
    (
        "hype",
        &[
            "hype",
            "energetic",
            "turn up",
            "bounce",
            "club",
            "party",
            "banger",
        ],
    ),
    (
        "chill",
        &[
            "chill",
            "relax",
            "study",
            "calm",
            "smooth",
            "late night",
            "mellow",
        ],
    ),
    ("jazzy", &["jazz", "jazzy", "soul", "sample"]),
    (
        "hopeful",
        &["hope", "uplifting", "happy", "victory", "motivat", "inspir"],
    ),
    (
        "devotional",
        &[
            "devotional",
            "spiritual",
            "temple",
            "bhajan",
            "morning raag",
            "bhairav",
        ],
    ),
    ("smooth", &["smooth", "silky", "sensual", "love"]),
];

fn detect_mood(brief: &str) -> Option<&'static str> {
    let b = brief.to_lowercase();
    MOODS
        .iter()
        .find(|(_, ws)| ws.iter().any(|w| b.contains(w)))
        .map(|(m, _)| *m)
}

fn pick<'a, T>(v: &'a [T], rng: &mut Rng) -> Option<&'a T> {
    if v.is_empty() {
        None
    } else {
        Some(&v[rng.below(v.len())])
    }
}

/// Reference scale for roman numerals: accidentals in progressions are
/// relative to the parallel natural major/minor, so "bII" always means a
/// half step above the tonic whatever the melodic scale is.
pub fn harmony_ref(scale: &str) -> &'static str {
    match theory::scale_intervals(scale) {
        Ok(iv) if iv.contains(&3) && !iv.contains(&4) => "minor",
        _ => "major",
    }
}

/// Build a motif: 4-7 notes in one bar with a clear contour, starting and
/// ending on stable degrees, mostly stepwise with one leap.
pub fn make_motif(rng: &mut Rng, density: f32, scale_len: usize) -> Vec<MotifNote> {
    let n = (3.0 + density * 5.0 + rng.range(-0.6, 0.6))
        .round()
        .clamp(3.0, 8.0) as usize;
    // rhythm: pick n onsets from a weighted grid (downbeat always)
    let weights = [
        1.0, 0.2, 0.5, 0.35, 0.8, 0.25, 0.6, 0.45, 0.9, 0.2, 0.55, 0.4, 0.7, 0.3, 0.5, 0.25,
    ];
    let mut onsets = vec![0usize];
    let mut tries = 0;
    while onsets.len() < n && tries < 200 {
        tries += 1;
        let s = rng.below(14) + 1;
        if !onsets.contains(&s) && rng.f32() < weights[s] {
            onsets.push(s);
        }
    }
    onsets.sort();
    let stable = [0, 2, 4];
    let mut deg = stable[rng.below(3)];
    let leap_at = 1 + rng.below(n.max(2) - 1);
    let mut out = Vec::new();
    for (i, &t) in onsets.iter().enumerate() {
        if i > 0 {
            if i == leap_at {
                deg += if rng.chance(0.5) { 3 } else { -3 } + if rng.chance(0.5) { 1 } else { 0 };
            } else {
                deg += [-1, 1, -1, 1, 2, -2, 0][rng.below(7)];
            }
        }
        if i + 1 == onsets.len() {
            // resolve to a stable degree
            let s = stable
                .iter()
                .min_by_key(|s| (deg.rem_euclid(scale_len as i32) - **s).abs())
                .copied()
                .unwrap_or(0);
            deg = deg - deg.rem_euclid(scale_len as i32) + s;
        }
        deg = deg.clamp(-3, scale_len as i32 + 4);
        let next = onsets.get(i + 1).copied().unwrap_or(16);
        let len = ((next - t) as f32 * if rng.chance(0.3) { 0.5 } else { 0.9 }).max(0.5);
        out.push(MotifNote {
            t: t as f32,
            deg,
            len,
        });
    }
    out
}

fn layer_part_presets(pb: &Playbook, rng: &mut Rng, brief: &str) -> BTreeMap<String, String> {
    let b = brief.to_lowercase();
    let mut pal = BTreeMap::new();
    for (k, v) in &pb.drums.kit {
        pal.insert(k.clone(), v.clone());
    }
    pal.insert("bass".into(), pb.bass.preset.clone());
    let choose = |r: &PartRule, rng: &mut Rng| -> Option<String> {
        // honour instruments named in the brief
        for p in &r.presets {
            if b.contains(p.replace('_', " ").as_str()) || b.contains(p.as_str()) {
                return Some(p.clone());
            }
        }
        pick(&r.presets, rng).cloned().or_else(|| r.preset.clone())
    };
    if let Some(p) = choose(&pb.harmony, rng) {
        pal.insert("harmony".into(), p);
    }
    if let Some(p) = choose(&pb.lead, rng) {
        pal.insert("lead".into(), p);
    }
    if let Some(mut p) = choose(&pb.counter, rng) {
        if Some(&p) == pal.get("lead") && pb.counter.presets.len() > 1 {
            p = pb
                .counter
                .presets
                .iter()
                .find(|x| Some(*x) != pal.get("lead"))
                .cloned()
                .unwrap_or(p);
        }
        pal.insert("counter".into(), p);
    }
    if let Some(t) = &pb.texture {
        if let Some(p) = choose(t, rng) {
            pal.insert("texture".into(), p);
        }
    }
    // explicit instrument requests in the brief override the lead
    for (word, preset) in [
        ("sitar", "sitar"),
        ("bansuri", "bansuri"),
        ("flute", "bansuri"),
        ("piano", "felt_piano"),
        ("bell", "fm_bell"),
        ("guitar", "guitar_pluck"),
        ("santoor", "santoor"),
    ] {
        if b.contains(word) && pal.get("lead").map(|l| l != preset).unwrap_or(true) {
            pal.insert("lead".into(), preset.into());
            break;
        }
    }
    pal
}

pub fn plan_track(a: &PlanArgs) -> Result<Plan> {
    let mut rng = Rng::new(a.seed ^ 0xB00B5);
    let mut thinking = Vec::new();
    let pb = match &a.genre {
        Some(g) => {
            playbook(g).or_else(|_| guess_genre(g).ok_or_else(|| anyhow!("unknown genre '{g}'")))?
        }
        None => guess_genre(&a.brief).unwrap_or_else(|| playbook("trap").unwrap()),
    };
    thinking.push(format!("Genre: {} - {}", pb.name, pb.description));
    let mood = a
        .mood
        .clone()
        .or_else(|| detect_mood(&a.brief).map(String::from))
        .unwrap_or_else(|| "default".into());
    let bpm = a
        .bpm
        .unwrap_or_else(|| (pb.bpm[0] + rng.f32() * (pb.bpm[1] - pb.bpm[0])).round());
    if bpm < pb.bpm[0] - 10.0 || bpm > pb.bpm[1] + 10.0 {
        thinking.push(format!(
            "Note: {bpm} BPM is outside the usual {}-{} for {}; keeping it as asked.",
            pb.bpm[0], pb.bpm[1], pb.name
        ));
    }
    let scale = a.scale.clone().unwrap_or_else(|| {
        pick(&pb.scales, &mut rng)
            .cloned()
            .unwrap_or_else(|| "minor".into())
    });
    theory::scale_intervals(&scale)?;
    let key = a.key.clone().unwrap_or_else(|| {
        [
            "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
        ][rng.below(12)]
        .to_string()
    });
    theory::pitch_class(&key)?;
    let progs = pb
        .progressions
        .get(&mood)
        .or_else(|| pb.progressions.get("default"))
        .ok_or_else(|| anyhow!("playbook {} has no progressions", pb.name))?;
    let pv = pick(progs, &mut rng).unwrap().clone();
    // hook: a different progression when there is one (contrast), else the same
    let ph = if progs.len() > 1 && rng.chance(0.6) {
        progs
            .iter()
            .find(|p| **p != pv)
            .cloned()
            .unwrap_or(pv.clone())
    } else {
        pv.clone()
    };
    thinking.push(format!("Mood '{mood}': {key} {scale} at {bpm} BPM; verse progression '{pv}', hook '{ph}' (numerals relative to parallel {}).", harmony_ref(&scale)));
    let iv = theory::scale_intervals(&scale)?;
    let lead_density = pb.lead.density.unwrap_or(0.5);
    let motif = make_motif(&mut rng, lead_density, iv.len());
    thinking.push(format!(
        "Motif ({} notes, one bar): degrees {:?} - hooks state it in full, verses only call it and leave space for vocals, the last hook lifts it an octave, the bridge augments it.",
        motif.len(),
        motif.iter().map(|m| m.deg).collect::<Vec<_>>()
    ));
    let palette = layer_part_presets(pb, &mut rng, &a.brief);
    thinking.push(format!(
        "Palette: {}",
        palette
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    // drum grammar: different verse/hook variations
    let mut drum_patterns = BTreeMap::new();
    for (part, list) in [
        ("kick", &pb.drums.kick),
        ("snare", &pb.drums.snare),
        ("hat", &pb.drums.hat),
        ("open_hat", &pb.drums.open_hat),
        ("perc", &pb.drums.perc),
        ("tabla", &pb.drums.tabla),
        ("bayan", &pb.drums.bayan),
    ] {
        if list.is_empty() {
            continue;
        }
        let v = rng.below(list.len());
        let mut h = rng.below(list.len());
        if list.len() > 1 && h == v && part != "snare" {
            h = (v + 1) % list.len();
        }
        drum_patterns.insert(part.to_string(), [list[v].clone(), list[h].clone()]);
    }
    // arrangement: playbook template scaled to the requested duration
    let mut tmpl = pb.arrangement.clone();
    let bar_s = 240.0 / bpm;
    let total_bars: u32 = tmpl.iter().map(|s| s.bars).sum();
    if let Some(d) = a.duration_s {
        let want = (d / bar_s).round().max(8.0) as u32;
        if want < total_bars {
            // drop middle sections until it fits (keep intro, first hook, outro)
            while tmpl.iter().map(|s| s.bars).sum::<u32>() > want && tmpl.len() > 3 {
                let idx = tmpl.len() - 2;
                tmpl.remove(idx);
            }
        } else if want > total_bars + 8 {
            // extend with another verse + hook
            let extra: Vec<SectionTemplate> = tmpl
                .iter()
                .filter(|s| s.kind == "verse" || s.kind == "hook")
                .take(2)
                .cloned()
                .collect();
            let mut i = tmpl.len() - 1;
            while tmpl.iter().map(|s| s.bars).sum::<u32>() + 8 <= want && !extra.is_empty() {
                for s in &extra {
                    tmpl.insert(i, s.clone());
                    i += 1;
                }
            }
        }
    }
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    let hooks = tmpl.iter().filter(|s| s.kind == "hook").count();
    let mut hook_no = 0;
    let mut sections = Vec::new();
    for (i, s) in tmpl.iter().enumerate() {
        let c = counts.entry(s.kind.clone()).or_insert(0);
        *c += 1;
        let name = format!("{}{}", s.kind, c);
        let lead = match s.kind.as_str() {
            "hook" => {
                hook_no += 1;
                if hook_no == hooks && hooks > 1 {
                    "high"
                } else {
                    "full"
                }
            }
            "verse" => {
                if s.layers.iter().any(|l| l == "lead") {
                    "sparse"
                } else {
                    "none"
                }
            }
            "bridge" => "augment",
            "intro" | "outro" => "call",
            _ => "call",
        };
        let next = tmpl.get(i + 1);
        let transition = match next {
            Some(n) if n.energy > s.energy + 0.15 => {
                if pb.drums.hat_rolls > 0.3 {
                    "drop_and_roll"
                } else {
                    "snare_fill"
                }
            }
            Some(n) if n.energy + 0.3 < s.energy => "fade_down",
            Some(_) => "turnaround",
            None => "end",
        };
        sections.push(PlanSection {
            name,
            kind: s.kind.clone(),
            bars: s.bars,
            energy: s.energy,
            layers: s.layers.clone(),
            lead: lead.into(),
            transition: transition.into(),
        });
    }
    let secs: f32 = sections.iter().map(|s| s.bars as f32 * bar_s).sum();
    thinking.push(format!(
        "Structure ({:.0} s): {} - energy {:?}; transitions into higher-energy sections drop the kick and roll/fill, hooks add counter-melody and open hats.",
        secs,
        sections.iter().map(|s| format!("{}({})", s.name, s.bars)).collect::<Vec<_>>().join(" > "),
        sections.iter().map(|s| (s.energy * 10.0).round() / 10.0).collect::<Vec<_>>()
    ));
    let sample_kit = if a.use_samples {
        pb.drums.sample_kit.clone()
    } else {
        None
    };
    let knobs = Knobs {
        lead_density,
        hat_rolls: pb.drums.hat_rolls,
        ghost_snare: pb.drums.ghost_snare,
        glide: pb.bass.glide,
        verse_velocity: 0.9,
        sidechain: pb.mix.sidechain_bass,
        variation_seed: 0,
        offsets: BTreeMap::new(),
        target_lufs: pb.mix.target_lufs,
        tonic_anchor: false,
        humanize: 0.04,
    };
    let title = {
        let words: Vec<&str> = a.brief.split_whitespace().take(4).collect();
        if words.is_empty() {
            format!("{} {}", pb.name, a.seed)
        } else {
            words.join(" ")
        }
    };
    Ok(Plan {
        title,
        brief: a.brief.clone(),
        genre: pb.name.clone(),
        mood,
        bpm,
        key,
        scale,
        swing: pb.swing,
        progression_verse: pv,
        progression_hook: ph,
        bars_per_chord: pb.bars_per_chord,
        motif,
        palette,
        drum_patterns,
        sections,
        knobs,
        sample_kit,
        flip_sample: a.flip_sample.clone(),
        seed: a.seed,
        thinking,
    })
}

// ---------------------------------------------------------------- composing

fn steps_of(s: &str) -> Vec<(usize, f32)> {
    s.chars()
        .enumerate()
        .filter_map(|(i, c)| match c {
            'X' => Some((i, 1.0)),
            'x' => Some((i, 0.8)),
            'o' => Some((i, 0.45)),
            _ => None,
        })
        .collect()
}

/// Step string -> notes over `bars`, repeating (16 or 32 step strings).
fn grid_notes(s: &str, bars: u32, pitch: u8, vel_scale: f32) -> Vec<Note> {
    if s.is_empty() {
        return Vec::new();
    }
    let len = s.chars().count().max(1);
    let total = (bars * STEPS_PER_BAR) as usize;
    let hits = steps_of(s);
    let mut out = Vec::new();
    let mut base = 0;
    while base < total {
        for (i, v) in &hits {
            let t = base + i;
            if t < total {
                out.push(Note::new(
                    t as f32,
                    1.0,
                    pitch,
                    (v * vel_scale).clamp(0.05, 1.0),
                ));
            }
        }
        base += len;
    }
    out
}

fn scale_pitch(root_pc: u8, iv: &[u8], octave: i32, deg: i32) -> u8 {
    let n = iv.len() as i32;
    let o = deg.div_euclid(n);
    let d = deg.rem_euclid(n) as usize;
    ((octave + 1 + o) * 12 + root_pc as i32 + iv[d] as i32).clamp(0, 127) as u8
}

fn chord_at(chords: &[theory::Chord], bpc: f32, step: f32) -> &theory::Chord {
    let idx = (step / (bpc * STEPS_PER_BAR as f32)).floor() as usize % chords.len();
    &chords[idx]
}

fn snap_to_chord(p: u8, c: &theory::Chord) -> u8 {
    let pcs: Vec<i32> = c
        .intervals
        .iter()
        .map(|i| (c.root_pc as i32 + *i as i32) % 12)
        .collect();
    let mut best = p as i32;
    let mut bd = 99;
    for d in -3i32..=3 {
        let q = p as i32 + d;
        if pcs.contains(&q.rem_euclid(12)) && d.abs() < bd {
            bd = d.abs();
            best = q;
        }
    }
    best.clamp(0, 127) as u8
}

fn snap_to_scale(p: u8, root_pc: u8, iv: &[u8]) -> u8 {
    for d in [0i32, -1, 1, -2, 2] {
        let q = p as i32 + d;
        let rel = (q - root_pc as i32).rem_euclid(12) as u8;
        if iv.contains(&rel) {
            return q.clamp(0, 127) as u8;
        }
    }
    p
}

struct Ctx<'a> {
    plan: &'a Plan,
    root_pc: u8,
    iv: &'a [u8],
    verse_chords: Vec<theory::Chord>,
    hook_chords: Vec<theory::Chord>,
}

impl Ctx<'_> {
    fn chords(&self, kind: &str) -> &[theory::Chord] {
        if kind == "hook" {
            &self.hook_chords
        } else {
            &self.verse_chords
        }
    }
}

/// Lead line for one section, developed from the motif.
fn lead_notes(
    cx: &Ctx,
    sec: &PlanSection,
    octave: i32,
    ornament: &str,
    rng: &mut Rng,
) -> Vec<Note> {
    let chords = cx.chords(&sec.kind);
    let bpc = cx.plan.bars_per_chord;
    let mut out = Vec::new();
    let n = cx.iv.len() as i32;
    let (oct, aug, which): (i32, f32, &str) = match sec.lead.as_str() {
        "high" => (octave + 1, 1.0, "full"),
        "augment" => (octave, 2.0, "full"),
        other => (octave, 1.0, other),
    };
    if which == "none" {
        return out;
    }
    let dens = cx.plan.knobs.lead_density;
    let phrase_bars = 4u32;
    let bar_len = STEPS_PER_BAR as f32;
    let mut bar = 0u32;
    while bar < sec.bars {
        let pos = bar % phrase_bars;
        // phrase role: 0 call, 1 answer, 2 sequence, 3 cadence
        let play = match which {
            "full" => true,
            "call" => pos == 0 || pos == 2,
            "sparse" => (pos == 1 || pos == 3) && rng.chance(0.4 + dens * 0.6),
            _ => true,
        };
        let start = bar as f32 * bar_len;
        if play {
            let ch = chord_at(chords, bpc, start);
            // transpose the motif so its first note sits near the chord root
            let root_deg = (0..n)
                .find(|d| (cx.iv[*d as usize] as i32 + cx.root_pc as i32) % 12 == ch.root_pc as i32)
                .unwrap_or(0);
            let shift = match pos {
                2 => root_deg,
                _ => 0,
            };
            let mut notes: Vec<MotifNote> = cx.plan.motif.clone();
            match pos {
                1 => {
                    // answer: same rhythm start, contour inverted at the end
                    let k = notes.len() / 2;
                    for (i, m) in notes.iter_mut().enumerate() {
                        if i >= k {
                            m.deg =
                                notes_first(&cx.plan.motif) - (m.deg - notes_first(&cx.plan.motif));
                        }
                    }
                }
                3 => {
                    // cadence: first half of the motif then a long stable tone
                    let k = (notes.len() / 2).max(1);
                    notes.truncate(k);
                    let t = notes
                        .last()
                        .map(|m| m.t + m.len.max(1.0))
                        .unwrap_or(0.0)
                        .min(12.0);
                    notes.push(MotifNote {
                        t,
                        deg: if rng.chance(0.5) { 0 } else { 4 },
                        len: (16.0 - t).max(2.0),
                    });
                }
                _ => {}
            }
            // variation seed: occasionally displace one note
            if cx.plan.knobs.variation_seed > 0 && !notes.is_empty() {
                let i = rng.below(notes.len());
                notes[i].deg += if rng.chance(0.5) { 1 } else { -1 };
            }
            for m in notes {
                let t = start + m.t * aug;
                let mut p = scale_pitch(cx.root_pc, cx.iv, oct, m.deg + shift);
                let strong = (m.t as u32) % 4 == 0;
                if strong {
                    p = snap_to_scale(
                        snap_to_chord(p, chord_at(chords, bpc, t)),
                        cx.root_pc,
                        cx.iv,
                    );
                }
                let len = (m.len * aug).max(0.5);
                let vel = if strong { 0.9 } else { 0.72 } * (0.85 + 0.15 * sec.energy);
                // ornaments: krintan grace notes and meend glides
                if ornament == "meend" && len >= 2.0 && rng.chance(0.45) && t >= 0.5 {
                    let grace = scale_pitch(cx.root_pc, cx.iv, oct, m.deg + shift + 1);
                    out.push(Note::new(t - 0.5, 0.5, grace, vel * 0.7));
                }
                let mut note = Note::new(t, len, p, vel);
                if (ornament == "meend" || ornament == "glide") && len >= 3.0 && rng.chance(0.35) {
                    let target = scale_pitch(
                        cx.root_pc,
                        cx.iv,
                        oct,
                        m.deg + shift + if rng.chance(0.5) { 1 } else { -1 },
                    );
                    note.slide_to = Some(target);
                }
                out.push(note);
            }
        }
        bar += if aug > 1.0 { 2 } else { 1 };
    }
    let end = (sec.bars * STEPS_PER_BAR) as f32;
    out.retain(|n| n.start < end);
    for n in out.iter_mut() {
        n.len = n.len.min(end - n.start);
    }
    out
}

fn notes_first(m: &[MotifNote]) -> i32 {
    m.first().map(|x| x.deg).unwrap_or(0)
}

/// Counter-melody: long chord tones moving by step where the lead rests.
fn counter_notes(cx: &Ctx, sec: &PlanSection, octave: i32, lead: &[Note]) -> Vec<Note> {
    let chords = cx.chords(&sec.kind);
    let bpc = cx.plan.bars_per_chord;
    let mut out = Vec::new();
    let mut prev: Option<u8> = None;
    for half in 0..(sec.bars * 2) {
        let t = half as f32 * 8.0;
        let ch = chord_at(chords, bpc, t);
        // pick the chord tone (3rd/5th/7th preferred) nearest to the previous note
        let base = ((octave + 1) * 12) as i32;
        let cands: Vec<u8> = ch
            .intervals
            .iter()
            .skip(1)
            .flat_map(|i| {
                let pc = (ch.root_pc as i32 + *i as i32) % 12;
                [base + pc, base + pc + 12]
            })
            .map(|x| x.clamp(0, 127) as u8)
            .collect();
        let target = prev.unwrap_or(cands[0]);
        let p = *cands
            .iter()
            .min_by_key(|c| (**c as i32 - target as i32).abs())
            .unwrap_or(&cands[0]);
        // call & response: shorter when the lead is busy in this half bar
        let busy = lead
            .iter()
            .filter(|n| n.start >= t && n.start < t + 8.0)
            .count();
        let len = if busy > 3 { 4.0 } else { 7.5 };
        let start = if busy > 3 { t + 4.0 } else { t };
        out.push(Note::new(start, len, p, 0.62));
        prev = Some(p);
    }
    out
}

fn bass_notes(
    cx: &Ctx,
    sec: &PlanSection,
    style: &str,
    octave: i32,
    kick: &[Note],
    glide: f32,
    rng: &mut Rng,
) -> Vec<Note> {
    let chords = cx.chords(&sec.kind);
    let bpc = cx.plan.bars_per_chord;
    let end = (sec.bars * STEPS_PER_BAR) as f32;
    let root_of = |t: f32| -> u8 {
        let c = chord_at(chords, bpc, t);
        (((octave + 1) * 12) as i32 + c.root_pc as i32).clamp(0, 127) as u8
    };
    let mut out = Vec::new();
    match style {
        "808_glide" | "follow_kick" => {
            let mut hits: Vec<f32> = kick.iter().map(|n| n.start).collect();
            hits.sort_by(|a, b| a.partial_cmp(b).unwrap());
            hits.dedup();
            if hits.is_empty() {
                // no kick in this section: one long note per chord
                let span = bpc * STEPS_PER_BAR as f32;
                let mut t = 0.0;
                while t < end {
                    out.push(Note::new(t, span.min(end - t), root_of(t), 0.85));
                    t += span;
                }
                return out;
            }
            for (i, &t) in hits.iter().enumerate() {
                let next = hits.get(i + 1).copied().unwrap_or(end);
                let mut len = (next - t).max(1.0);
                if style == "follow_kick" {
                    len = len.min(3.0);
                }
                let p = root_of(t);
                let mut n = Note::new(t, len, p, if (t as u32) % 16 == 0 { 1.0 } else { 0.88 });
                if style == "808_glide" && i + 1 < hits.len() {
                    let np = root_of(next);
                    if np != p && rng.chance(glide + 0.3) {
                        n.slide_to = Some(np);
                    } else if rng.chance(glide * 0.6) && len >= 2.0 {
                        // octave flick, the trap/drill signature
                        n.slide_to =
                            Some((p as i32 + if rng.chance(0.5) { 12 } else { 7 }).min(127) as u8);
                        n.len = len.min(3.0);
                    }
                }
                out.push(n);
            }
        }
        _ => {
            // root: half notes with a passing fifth
            let mut t = 0.0;
            while t < end {
                let p = root_of(t);
                out.push(Note::new(t, 7.0, p, 0.9));
                out.push(Note::new(
                    t + 8.0,
                    5.0,
                    if rng.chance(0.3) { (p + 7).min(127) } else { p },
                    0.8,
                ));
                t += 16.0;
            }
        }
    }
    out
}

fn hat_notes(
    pattern: &str,
    bars: u32,
    rolls: f32,
    accent: f32,
    energy: f32,
    rng: &mut Rng,
) -> Vec<Note> {
    let mut out = grid_notes(pattern, bars, 60, 0.75 + 0.25 * energy);
    // velocity accents on the quarter notes
    for n in out.iter_mut() {
        if (n.start as u32) % 4 != 0 {
            n.vel *= 1.0 - accent * 0.5;
        }
    }
    if rolls <= 0.0 {
        return out;
    }
    for bar in 0..bars {
        if !rng.chance(rolls * (0.5 + 0.5 * energy)) {
            continue;
        }
        // roll over one beat: 32nds or triplet 16ths, pitched up for drama
        let beat = if rng.chance(0.6) {
            3
        } else {
            1 + rng.below(3) as u32
        };
        let t0 = (bar * 16 + beat * 4) as f32;
        out.retain(|n| n.start < t0 || n.start >= t0 + 4.0);
        let triplet = rng.chance(0.45);
        let step = if triplet { 4.0 / 6.0 } else { 0.5 };
        let count = if triplet { 6 } else { 8 };
        let rise = rng.chance(0.4);
        for k in 0..count {
            let mut n = Note::new(
                t0 + k as f32 * step,
                step,
                if rise { 60 + (k as u8 / 2) } else { 60 },
                0.45 + 0.4 * k as f32 / count as f32,
            );
            n.len = step;
            out.push(n);
        }
    }
    out
}

fn add_ghosts(snare: &mut Vec<Note>, bars: u32, prob: f32, rng: &mut Rng) {
    if prob <= 0.0 {
        return;
    }
    let taken: Vec<u32> = snare.iter().map(|n| n.start as u32).collect();
    for bar in 0..bars {
        for s in [3u32, 6, 7, 10, 14, 15] {
            let t = bar * 16 + s;
            if !taken.iter().any(|x| x.abs_diff(t) <= 1) && rng.chance(prob * 0.5) {
                snare.push(Note::new(t as f32, 1.0, 60, 0.22 + rng.f32() * 0.12));
            }
        }
    }
}

/// Tihai-style ending for tabla: a 3x repeated phrase landing on the next sam.
fn tabla_tihai(bars: u32, pitch: u8) -> Vec<Note> {
    let end = (bars * 16) as f32;
    let start = end - 9.0;
    let mut out = Vec::new();
    for rep in 0..3 {
        let t = start + rep as f32 * 3.0;
        out.push(Note::new(t, 1.0, pitch, 0.95));
        out.push(Note::new(t + 1.0, 1.0, pitch, 0.4));
        out.push(Note::new(t + 2.0, 1.0, pitch, 0.7));
    }
    out
}

/// Turn the plan into a fresh project inside `e` (tracks, patterns,
/// arrangement, buses, sends). Mixing (balance/master) is separate.
pub fn compose(e: &mut Engine, plan: &Plan) -> Result<Value> {
    let key_pc = theory::pitch_class(&plan.key)?;
    let iv: Vec<u8> = theory::scale_intervals(&plan.scale)?.to_vec();
    let href = harmony_ref(&plan.scale);
    let cx = Ctx {
        plan,
        root_pc: key_pc,
        iv: &iv,
        verse_chords: theory::parse_progression(&plan.progression_verse, key_pc, href)?,
        hook_chords: theory::parse_progression(&plan.progression_hook, key_pc, href)?,
    };
    let pb = playbook(&plan.genre)?;
    let mut p = Project::new(&plan.title, plan.bpm);
    p.key_root = plan.key.clone();
    p.scale = plan.scale.clone();
    p.swing = plan.swing;
    p.patterns.clear();
    e.replace_project(p);

    // which roles are used anywhere
    let mut roles: Vec<String> = Vec::new();
    for s in &plan.sections {
        for l in &s.layers {
            if !roles.contains(l) && (plan.palette.contains_key(l) || l == "harmony") {
                roles.push(l.clone());
            }
        }
    }
    // tracks: real kit samples when available, else the synth kit
    let kit_samples = match &plan.sample_kit {
        Some(k) => crate::sample_lib::kit_map(e, k, &plan.palette).unwrap_or_default(),
        None => BTreeMap::new(),
    };
    let mut used_samples = Vec::new();
    for r in &roles {
        let Some(preset) = plan.palette.get(r) else {
            continue;
        };
        let track = track_name(r);
        if let Some(sample) = kit_samples.get(r.as_str()) {
            e.call_from(
                "add_sample_track",
                &json!({"name": track, "sample": sample, "one_shot": true}),
                "producer",
            )?;
            // calibrate the fader like a preset
            used_samples.push(format!("{track}={sample}"));
            continue;
        }
        e.call_from(
            "add_track",
            &json!({"name": track, "preset": preset}),
            "producer",
        )?;
    }
    if !used_samples.is_empty() {
        e.call_from("level_hints", &json!({"apply": true}), "producer")?;
    }

    // patterns per section
    let mut rng = Rng::new(plan.seed ^ 0x5EC7 ^ plan.knobs.variation_seed.wrapping_mul(0x9E37));
    let lead_oct = pb.lead.octave;
    let ornament = pb.lead.ornament.clone().unwrap_or_default();
    let harmony_style = pb.harmony.style.clone().unwrap_or_else(|| "block".into());
    let tabla_pitch = (60 + key_pc as i32 - if key_pc > 6 { 12 } else { 0 }) as u8;
    let bayan_pitch =
        (60 + key_pc as i32 - 4 - if key_pc > 8 { 12 } else { 0 }).clamp(40, 80) as u8;
    for (si, sec) in plan.sections.iter().enumerate() {
        let mut pat = Pattern::new(&sec.name, sec.bars);
        let is_hook = sec.kind == "hook";
        let vi = if is_hook { 1 } else { 0 };
        let vel = if is_hook {
            1.0
        } else {
            plan.knobs.verse_velocity
        } * (0.8 + 0.2 * sec.energy);
        let has = |l: &str| sec.layers.iter().any(|x| x == l) && roles.iter().any(|r| r == l);
        let pat_of = |part: &str| {
            plan.drum_patterns
                .get(part)
                .map(|v| v[vi].clone())
                .unwrap_or_default()
        };
        let mut kick = Vec::new();
        if has("kick") {
            kick = grid_notes(&pat_of("kick"), sec.bars, 60, vel);
        }
        if has("snare") {
            let mut sn = grid_notes(&pat_of("snare"), sec.bars, 60, vel);
            add_ghosts(&mut sn, sec.bars, plan.knobs.ghost_snare, &mut rng);
            pat.clips.insert(track_name("snare"), sn);
        }
        if has("hat") {
            pat.clips.insert(
                track_name("hat"),
                hat_notes(
                    &pat_of("hat"),
                    sec.bars,
                    plan.knobs.hat_rolls,
                    pb.drums.hat_vel_accent,
                    sec.energy,
                    &mut rng,
                ),
            );
        }
        if has("open_hat") {
            let mut oh = grid_notes(&pat_of("open_hat"), sec.bars, 60, vel * 0.8);
            if is_hook {
                // crash-like open hat on the downbeat of the hook
                oh.push(Note::new(0.0, 2.0, 60, 0.9));
            }
            pat.clips.insert(track_name("open_hat"), oh);
        }
        if has("perc") {
            pat.clips.insert(
                track_name("perc"),
                grid_notes(&pat_of("perc"), sec.bars, 60, vel * 0.85),
            );
        }
        if has("tabla") {
            let mut t = grid_notes(&pat_of("tabla"), sec.bars, tabla_pitch, vel);
            for n in t.iter_mut() {
                // muted bols on the weak 16ths
                if (n.start as u32) % 2 == 1 {
                    n.vel = n.vel.min(0.42);
                }
            }
            if sec.transition != "end" && sec.bars >= 4 {
                let end = (sec.bars * 16) as f32 - 9.0;
                t.retain(|n| n.start < end);
                t.extend(tabla_tihai(sec.bars, tabla_pitch));
            }
            pat.clips.insert(track_name("tabla"), t);
        }
        if has("bayan") {
            pat.clips.insert(
                track_name("bayan"),
                grid_notes(&pat_of("bayan"), sec.bars, bayan_pitch, vel),
            );
        }
        // transitions
        let end = (sec.bars * 16) as f32;
        match sec.transition.as_str() {
            "drop_and_roll" => {
                // pull the kick out of the last half bar so the next downbeat hits harder
                kick.retain(|n| n.start < end - 8.0);
                if let Some(h) = pat.clips.get_mut(&track_name("hat")) {
                    h.retain(|n| n.start < end - 4.0);
                    for k in 0..8 {
                        h.push(Note::new(
                            end - 4.0 + k as f32 * 0.5,
                            0.5,
                            60 + k / 2,
                            0.4 + 0.07 * k as f32,
                        ));
                    }
                }
            }
            "snare_fill" => {
                if let Some(sn) = pat.clips.get_mut(&track_name("snare")) {
                    sn.retain(|n| n.start < end - 4.0);
                    for k in 0..4 {
                        sn.push(Note::new(
                            end - 4.0 + k as f32,
                            1.0,
                            60,
                            0.55 + 0.12 * k as f32,
                        ));
                    }
                }
                kick.retain(|n| n.start < end - 4.0 || (n.start - (end - 4.0)).abs() < 0.01);
            }
            _ => {}
        }
        if !kick.is_empty() {
            pat.clips.insert(track_name("kick"), kick.clone());
        }
        if has("bass") {
            let bass = bass_notes(
                &cx,
                sec,
                &pb.bass.style,
                pb.bass.octave,
                &kick,
                plan.knobs.glide,
                &mut rng,
            );
            pat.clips.insert(track_name("bass"), bass);
        }
        let chords = cx.chords(&sec.kind);
        if has("harmony") {
            let notes = if harmony_style == "drone" {
                // tanpura: root + fifth (+ upper root), re-struck every 2 bars
                let mut v = Vec::new();
                let base = (pb.harmony.octave + 1) * 12 + key_pc as i32;
                let mut t = 0.0;
                while t < end {
                    for (k, d) in [0, 7, 12].iter().enumerate() {
                        v.push(Note::new(
                            t + k as f32 * 0.5,
                            32.0f32.min(end - t) - k as f32 * 0.5,
                            (base + d) as u8,
                            0.6,
                        ));
                    }
                    t += 32.0;
                }
                v
            } else {
                let voiced = theory::voice_chords(chords, pb.harmony.octave, true);
                let style = if sec.kind == "intro" || sec.kind == "outro" || sec.kind == "bridge" {
                    "block"
                } else {
                    harmony_style.as_str()
                };
                theory::chord_notes(
                    &voiced,
                    plan.bars_per_chord,
                    sec.bars * STEPS_PER_BAR,
                    style,
                    0.7 * vel,
                )?
            };
            pat.clips.insert(track_name("harmony"), notes);
        }
        let mut lead = Vec::new();
        if has("lead") {
            lead = lead_notes(&cx, sec, lead_oct, &ornament, &mut rng);
            pat.clips.insert(track_name("lead"), lead.clone());
        }
        if has("counter") {
            pat.clips.insert(
                track_name("counter"),
                counter_notes(&cx, sec, pb.counter.octave, &lead),
            );
        }
        if has("texture") {
            let tex_oct = pb.texture.as_ref().map(|t| t.octave).unwrap_or(4);
            let mut v = Vec::new();
            let span = (plan.bars_per_chord * 16.0).max(16.0);
            let mut t = 0.0;
            while t < end {
                let c = chord_at(chords, plan.bars_per_chord, t);
                let r = ((tex_oct + 1) * 12) as i32 + c.root_pc as i32;
                v.push(Note::new(t, span.min(end - t), r as u8, 0.45));
                v.push(Note::new(t, span.min(end - t), (r + 7) as u8, 0.4));
                t += span;
            }
            pat.clips.insert(track_name("texture"), v);
        }
        if plan.knobs.tonic_anchor && sec.kind != "intro" && si + 1 == plan.sections.len() {
            // end on the tonic so the key is unambiguous
            let t = end - 16.0;
            if let Some(b) = pat.clips.get_mut(&track_name("bass")) {
                b.retain(|n| n.start < t);
                b.push(Note::new(
                    t,
                    16.0,
                    (((pb.bass.octave + 1) * 12) as i32 + key_pc as i32) as u8,
                    0.9,
                ));
            }
        }
        pat.clips.retain(|_, v| !v.is_empty());
        e.project.patterns.push(pat);
    }
    e.project.arrangement = plan
        .sections
        .iter()
        .map(|s| Section {
            pattern: s.name.clone(),
            repeats: 1,
        })
        .collect();
    e.revision += 1;
    if plan.knobs.humanize > 0.0 {
        for s in &plan.sections {
            e.call_from("humanize", &json!({"pattern": s.name, "timing": plan.knobs.humanize, "velocity": 0.06, "seed": plan.seed}), "producer")?;
        }
    }

    // ---- mix bus architecture
    let have = |e: &Engine, t: &str| e.project.track_index(t).is_ok();
    e.call_from(
        "add_bus",
        &json!({"name": "verb", "preset": "reverb"}),
        "producer",
    )?;
    e.call_from(
        "add_bus",
        &json!({"name": "echo", "preset": "delay"}),
        "producer",
    )?;
    for (t, db) in [
        ("lead", pb.mix.reverb_send),
        ("counter", pb.mix.reverb_send - 2.0),
        ("harmony", pb.mix.reverb_send + 1.0),
        ("texture", pb.mix.reverb_send),
        ("snare", pb.mix.reverb_send - 6.0),
        ("tabla", pb.mix.reverb_send - 4.0),
    ] {
        if have(e, t) {
            e.call_from(
                "set_send",
                &json!({"track": t, "bus": "verb", "db": db}),
                "producer",
            )?;
        }
    }
    if have(e, "lead") {
        e.call_from(
            "set_send",
            &json!({"track": "lead", "bus": "echo", "db": pb.mix.delay_send}),
            "producer",
        )?;
    }
    let drums: Vec<String> = ["kick", "snare", "hat", "open_hat", "perc", "tabla", "bayan"]
        .iter()
        .filter(|t| have(e, t))
        .map(|t| t.to_string())
        .collect();
    if pb.mix.drum_bus && !drums.is_empty() {
        e.call_from(
            "add_bus",
            &json!({"name": "drums", "preset": "drum"}),
            "producer",
        )?;
        e.call_from(
            "route_track",
            &json!({"track": drums, "bus": "drums"}),
            "producer",
        )?;
    }
    // carve: low cut on everything melodic so the low end belongs to kick + bass
    for t in ["harmony", "lead", "counter", "texture"] {
        if have(e, t) {
            e.call_from(
                "add_effect",
                &json!({"track": t, "type": "eq", "params": {"low_db": -9.0, "low_freq": 180.0}}),
                "producer",
            )?;
        }
    }
    if plan.knobs.sidechain > 0.0 && have(e, "bass") && have(e, "kick") {
        e.call_from("add_effect", &json!({"track": "bass", "type": "sidechain", "params": {"source": "kick", "amount": plan.knobs.sidechain, "release_ms": 120.0}}), "producer")?;
    }
    if have(e, "harmony")
        && plan
            .palette
            .get("harmony")
            .map(|h| h != "tanpura")
            .unwrap_or(true)
    {
        e.call_from(
            "add_effect",
            &json!({"track": "harmony", "type": "width", "params": {"amount": 1.3}}),
            "producer",
        )?;
    }
    if pb.mix.crush {
        e.call_from("add_effect", &json!({"track": "master", "type": "bitcrush", "params": {"bits": 12.0, "downsample": 2, "mix": 0.25}}), "producer")?;
    }
    Ok(json!({
        "tracks": e.project.tracks.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
        "sections": plan.sections.iter().map(|s| format!("{} ({} bars)", s.name, s.bars)).collect::<Vec<_>>(),
        "samples_used": used_samples,
        "seconds": e.project.song_seconds(),
    }))
}

/// Track names are the role names, except harmony -> chords.
pub fn track_name(role: &str) -> String {
    match role {
        "harmony" => "chords".into(),
        r => r.into(),
    }
}

/// Gain staging + mastering for the plan (balance_mix then master_assistant).
pub fn mix_and_master(e: &mut Engine, plan: &Plan) -> Result<Value> {
    let pb = playbook(&plan.genre)?;
    let offsets: serde_json::Map<String, Value> = plan
        .knobs
        .offsets
        .iter()
        .filter(|(t, _)| e.project.track_index(t).is_ok())
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    let bal = e.call_from(
        "balance_mix",
        &json!({"genre": pb.mix.balance_genre, "iterations": 2, "offsets": offsets}),
        "producer",
    )?;
    let master = e.call_from("master_assistant", &json!({"target_lufs": plan.knobs.target_lufs, "style": pb.mix.master_style, "max_iterations": 3}), "producer")?;
    Ok(json!({"balance": compact(&bal, 600), "master": compact(&master, 600)}))
}

fn compact(v: &Value, max: usize) -> Value {
    let s = v.to_string();
    if s.len() <= max {
        v.clone()
    } else {
        Value::String(format!(
            "{}...",
            &s[..s
                .char_indices()
                .take_while(|(i, _)| *i < max)
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0)]
        ))
    }
}

/// compose + mix in one go.
pub fn apply_plan(e: &mut Engine, plan: &Plan) -> Result<Value> {
    let c = compose(e, plan)?;
    let m = mix_and_master(e, plan)?;
    Ok(json!({"compose": c, "mix": m}))
}

// ---------------------------------------------------------------- the critic

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub severity: f32,
    pub message: String,
    /// The plan/mix edit that addresses it (see revise).
    pub fix: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Critique {
    pub score: f32,
    pub technical: f32,
    pub musical: f32,
    pub reference: Option<f32>,
    pub findings: Vec<Finding>,
    pub measurements: Value,
}

fn spearman(a: &[f32], b: &[f32]) -> f32 {
    let rank = |v: &[f32]| -> Vec<f32> {
        let mut idx: Vec<usize> = (0..v.len()).collect();
        idx.sort_by(|i, j| {
            v[*i]
                .partial_cmp(&v[*j])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut r = vec![0.0; v.len()];
        for (k, i) in idx.iter().enumerate() {
            r[*i] = k as f32;
        }
        r
    };
    let n = a.len();
    if n < 3 {
        return 1.0;
    }
    let (ra, rb) = (rank(a), rank(b));
    let d2: f32 = ra.iter().zip(&rb).map(|(x, y)| (x - y) * (x - y)).sum();
    1.0 - 6.0 * d2 / (n as f32 * (n * n - 1) as f32)
}

/// Same pitch-class set (relative major/minor) counts as the same key.
fn same_key_family(a: &str, b: &str) -> bool {
    let set = |k: &str| -> Option<Vec<u8>> {
        let mut it = k.split_whitespace();
        let root = theory::pitch_class(it.next()?).ok()?;
        let sc = it.next().unwrap_or("major");
        let iv = theory::scale_intervals(sc).ok()?;
        let mut v: Vec<u8> = iv.iter().map(|i| (root + i) % 12).collect();
        v.sort();
        Some(v)
    };
    match (set(a), set(b)) {
        (Some(x), Some(y)) => {
            let common = x.iter().filter(|p| y.contains(p)).count();
            common + 1 >= x.len().min(y.len())
        }
        _ => false,
    }
}

pub fn critique(e: &mut Engine, plan: &Plan, reference: Option<&str>) -> Result<Critique> {
    let mut findings = Vec::new();
    let mut tech = 100.0f32;
    let mut mus = 100.0f32;
    // --- delivery QC
    let cm = e.call_from(
        "check_master",
        &json!({"target_lufs": plan.knobs.target_lufs, "lufs_tolerance": 1.5}),
        "producer",
    )?;
    let lufs = cm["loudness"]["integrated_lufs"].as_f64().unwrap_or(-99.0) as f32;
    let tp = cm["loudness"]["true_peak_dbtp"]
        .as_f64()
        .or_else(|| cm["loudness"]["true_peak"].as_f64())
        .unwrap_or(0.0) as f32;
    for it in cm["items"].as_array().cloned().unwrap_or_default() {
        let st = it["status"].as_str().unwrap_or("pass");
        if st == "pass" {
            continue;
        }
        let check = it["check"].as_str().unwrap_or("");
        tech -= if st == "fail" { 12.0 } else { 4.0 };
        let fix = match check {
            "loudness" => json!({"action": "remaster", "target_lufs": plan.knobs.target_lufs}),
            "true_peak" | "clipping" => {
                json!({"action": "remaster", "target_lufs": plan.knobs.target_lufs - 0.5})
            }
            _ => json!({"action": "none"}),
        };
        findings.push(Finding {
            id: format!("master_{check}"),
            severity: if st == "fail" { 0.9 } else { 0.4 },
            message: it["message"].as_str().unwrap_or("").to_string(),
            fix,
        });
    }
    // --- artifacts (clicks are what listeners notice; DC steps under 808s are usually benign)
    let art = e.call_from("detect_artifacts", &json!({}), "producer")?;
    let clicks = art["events"]
        .as_array()
        .map(|v| v.iter().filter(|x| x["kind"] == "click").count())
        .unwrap_or(0);
    if clicks > 0 {
        tech -= (clicks as f32 * 3.0).min(15.0);
        findings.push(Finding {
            id: "clicks".into(),
            severity: 0.6,
            message: format!("{clicks} click(s)/pop(s) detected"),
            fix: json!({"action": "declick"}),
        });
    }
    if art["noise_bed"]["detected"].as_bool().unwrap_or(false) {
        tech -= 5.0;
    }
    // --- mix balance from the analyzer
    let am = e.call_from("analyze_mix", &json!({}), "producer")?;
    let mix_score = am["score"].as_f64().unwrap_or(70.0) as f32;
    tech = tech * 0.6 + mix_score * 0.4;
    for s in am["suggestions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .take(4)
    {
        let msg = s.as_str().unwrap_or("").to_string();
        let low = msg.to_lowercase();
        let fix = if low.contains("sidechain") || low.contains("overlap") {
            json!({"action": "low_end", "sidechain": (plan.knobs.sidechain + 0.2).min(0.85)})
        } else if let Some(t) = ["hat", "snare", "lead", "chords", "counter", "perc", "tabla"]
            .iter()
            .find(|t| {
                low.contains(&format!("'{t}'"))
                    && (low.contains("buried")
                        || low.contains("inaudible")
                        || low.contains("too quiet")
                        || low.contains("raise"))
            })
        {
            json!({"action": "offset", "track": t, "db": 2.0})
        } else if let Some(t) = [
            "hat", "snare", "lead", "chords", "counter", "perc", "kick", "bass", "tabla",
        ]
        .iter()
        .find(|t| {
            low.contains(&format!("'{t}'"))
                && (low.contains("too loud") || low.contains("dominat") || low.contains("lower"))
        }) {
            json!({"action": "offset", "track": t, "db": -2.0})
        } else {
            json!({"action": "none"})
        };
        findings.push(Finding {
            id: "mix".into(),
            severity: 0.35,
            message: msg,
            fix,
        });
    }
    // --- sections: contrast and energy flow
    let sec = e.call_from("analyze_sections", &json!({"top_tracks": 3}), "producer")?;
    let measured: Vec<f32> = sec["sections"]
        .as_array()
        .map(|v| {
            v.iter()
                .map(|s| {
                    s["metrics"]["short_term_max_lufs"]
                        .as_f64()
                        .unwrap_or(-40.0) as f32
                })
                .collect()
        })
        .unwrap_or_default();
    let planned: Vec<f32> = plan.sections.iter().map(|s| s.energy).collect();
    let rho = if measured.len() == planned.len() {
        spearman(&planned, &measured)
    } else {
        0.5
    };
    if rho < 0.6 {
        mus -= (0.6 - rho) * 30.0;
        findings.push(Finding { id: "energy_curve".into(), severity: 0.6, message: format!("measured loudness follows the planned energy curve weakly (rank corr {rho:.2})"), fix: json!({"action": "contrast", "verse_velocity": (plan.knobs.verse_velocity - 0.08).max(0.7)}) });
    }
    let hook_l: Vec<f32> = plan
        .sections
        .iter()
        .zip(&measured)
        .filter(|(s, _)| s.kind == "hook")
        .map(|(_, m)| *m)
        .collect();
    let verse_l: Vec<f32> = plan
        .sections
        .iter()
        .zip(&measured)
        .filter(|(s, _)| s.kind == "verse")
        .map(|(_, m)| *m)
        .collect();
    if !hook_l.is_empty() && !verse_l.is_empty() {
        let h = hook_l.iter().sum::<f32>() / hook_l.len() as f32;
        let v = verse_l.iter().sum::<f32>() / verse_l.len() as f32;
        if h < v + 0.8 {
            mus -= 12.0;
            findings.push(Finding { id: "hook_lift".into(), severity: 0.8, message: format!("hook ({h:.1} LUFS short-term) does not lift over the verse ({v:.1})"), fix: json!({"action": "contrast", "verse_velocity": (plan.knobs.verse_velocity - 0.1).max(0.65)}) });
        }
    }
    for f in sec["flags"].as_array().cloned().unwrap_or_default() {
        let msg = f
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| f["message"].as_str().unwrap_or(&f.to_string()).to_string());
        mus -= 3.0;
        let fix = if msg.contains("low end") {
            json!({"action": "low_end", "sidechain": plan.knobs.sidechain})
        } else {
            json!({"action": "none"})
        };
        findings.push(Finding {
            id: "section_flag".into(),
            severity: 0.4,
            message: msg,
            fix,
        });
    }
    // --- repetition: every section's lead should not be a copy of another kind's lead
    let mut lead_sigs: Vec<(String, String)> = Vec::new();
    for s in &plan.sections {
        if let Ok(i) = e.project.pattern_index(&s.name) {
            let sig: String = e.project.patterns[i]
                .notes("lead")
                .iter()
                .map(|n| format!("{}:{}", n.start as u32, n.pitch))
                .collect::<Vec<_>>()
                .join(",");
            if !sig.is_empty() {
                lead_sigs.push((s.kind.clone(), sig));
            }
        }
    }
    let mut dup = 0;
    for i in 0..lead_sigs.len() {
        for j in (i + 1)..lead_sigs.len() {
            if lead_sigs[i].0 != lead_sigs[j].0 && lead_sigs[i].1 == lead_sigs[j].1 {
                dup += 1;
            }
        }
    }
    if dup > 0 {
        mus -= 8.0;
        findings.push(Finding {
            id: "repetition".into(),
            severity: 0.5,
            message: format!("{dup} section pair(s) of different kinds share an identical lead"),
            fix: json!({"action": "vary", "variation_seed": plan.knobs.variation_seed + 1}),
        });
    }
    // lead density sanity: hooks should be busier than verses
    let density = |kind: &str| -> f32 {
        let (mut n, mut bars) = (0usize, 0u32);
        for s in plan.sections.iter().filter(|s| s.kind == kind) {
            if let Ok(i) = e.project.pattern_index(&s.name) {
                n += e.project.patterns[i].notes("lead").len();
                bars += s.bars;
            }
        }
        if bars == 0 {
            0.0
        } else {
            n as f32 / bars as f32
        }
    };
    let (dh, dv) = (density("hook"), density("verse"));
    if dh < 2.0
        && plan
            .sections
            .iter()
            .any(|s| s.kind == "hook" && s.layers.iter().any(|l| l == "lead"))
    {
        mus -= 8.0;
        findings.push(Finding { id: "thin_hook".into(), severity: 0.5, message: format!("hook melody is thin ({dh:.1} notes/bar)"), fix: json!({"action": "density", "lead_density": (plan.knobs.lead_density + 0.15).min(1.0)}) });
    } else if dv > dh && dh > 0.0 {
        mus -= 5.0;
        findings.push(Finding { id: "busy_verse".into(), severity: 0.4, message: format!("verse lead ({dv:.1}/bar) busier than the hook ({dh:.1}/bar): crowds the vocal"), fix: json!({"action": "density", "lead_density": (plan.knobs.lead_density - 0.1).max(0.25)}) });
    }
    // --- key: rendered notes should read as the planned key family
    let dk = e.call_from("detect_key", &json!({}), "producer")?;
    let found = dk["key"].as_str().unwrap_or("").to_string();
    let want = format!("{} {}", plan.key, plan.scale);
    let key_ok = same_key_family(&found, &want)
        || dk["candidates"]
            .as_array()
            .map(|c| {
                c.iter()
                    .take(2)
                    .any(|k| same_key_family(k["key"].as_str().unwrap_or(""), &want))
            })
            .unwrap_or(false);
    if !key_ok && !found.is_empty() {
        mus -= 6.0;
        findings.push(Finding {
            id: "key".into(),
            severity: 0.3,
            message: format!("notes read as {found}, planned {want}"),
            fix: json!({"action": "tonic"}),
        });
    }
    // --- duration
    let secs = e.project.song_seconds();
    if !(60.0..=260.0).contains(&secs) {
        mus -= 5.0;
    }
    // --- reference
    let mut refscore = None;
    if let Some(r) = reference {
        if let Ok(c) = e.call_from("compare_to_reference", &json!({"reference": r}), "producer") {
            let s = c["match_score"]
                .as_f64()
                .or_else(|| c["score"].as_f64())
                .unwrap_or(50.0) as f32;
            refscore = Some(s);
            if s < 75.0 {
                findings.push(Finding {
                    id: "reference".into(),
                    severity: 0.5,
                    message: format!("tonal/dynamic match to the reference is {s:.0}/100"),
                    fix: json!({"action": "match_reference", "reference": r}),
                });
            }
        }
    }
    let tech = tech.clamp(0.0, 100.0);
    let mus = mus.clamp(0.0, 100.0);
    let score = match refscore {
        Some(r) => 0.4 * tech + 0.4 * mus + 0.2 * r,
        None => 0.5 * tech + 0.5 * mus,
    };
    findings.sort_by(|a, b| {
        b.severity
            .partial_cmp(&a.severity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(Critique {
        score: (score * 10.0).round() / 10.0,
        technical: (tech * 10.0).round() / 10.0,
        musical: (mus * 10.0).round() / 10.0,
        reference: refscore,
        findings,
        measurements: json!({
            "integrated_lufs": lufs, "true_peak_dbtp": tp, "mix_score": mix_score,
            "section_short_term_max_lufs": measured, "energy_rank_corr": (rho * 100.0).round() / 100.0,
            "detected_key": found, "lead_notes_per_bar": {"hook": dh, "verse": dv}, "seconds": secs,
        }),
    })
}

/// Apply the critic's fixes to the plan. Returns what changed; an empty
/// list means nothing actionable was left.
pub fn revise(plan: &mut Plan, c: &Critique, max_fixes: usize) -> Vec<String> {
    let mut done = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for f in &c.findings {
        if done.len() >= max_fixes {
            break;
        }
        let action = f.fix["action"].as_str().unwrap_or("none");
        if action == "none"
            || !seen.insert(action.to_string() + f.fix["track"].as_str().unwrap_or(""))
        {
            continue;
        }
        let k = &mut plan.knobs;
        match action {
            "remaster" => {
                if let Some(t) = f.fix["target_lufs"].as_f64() {
                    k.target_lufs = t as f32;
                }
                done.push(format!("{}: re-master at {:.1} LUFS", f.id, k.target_lufs));
            }
            "low_end" => {
                let s = f.fix["sidechain"].as_f64().unwrap_or(0.5) as f32;
                if s > k.sidechain + 0.01 {
                    k.sidechain = s;
                    done.push(format!(
                        "{}: sidechain 808/bass to the kick at {s:.2}",
                        f.id
                    ));
                } else {
                    let o = k.offsets.entry("bass".into()).or_insert(0.0);
                    *o -= 1.5;
                    done.push(format!("{}: bass -1.5 dB", f.id));
                }
            }
            "offset" => {
                let t = f.fix["track"].as_str().unwrap_or("").to_string();
                let db = f.fix["db"].as_f64().unwrap_or(0.0) as f32;
                let o = k.offsets.entry(t.clone()).or_insert(0.0);
                *o = (*o + db).clamp(-8.0, 8.0);
                done.push(format!("{}: {t} {db:+.1} dB vs genre target", f.id));
            }
            "contrast" => {
                let v = f.fix["verse_velocity"].as_f64().unwrap_or(0.85) as f32;
                if v < k.verse_velocity - 0.01 {
                    k.verse_velocity = v;
                    // a hook needs a lift: give it the counter line and open hats
                    for s in plan.sections.iter_mut().filter(|s| s.kind == "hook") {
                        for l in ["counter", "open_hat"] {
                            if !s.layers.iter().any(|x| x == l) && plan.palette.contains_key(l) {
                                s.layers.push(l.into());
                            }
                        }
                    }
                    // and verses breathe: drop the open hat / counter there
                    for s in plan.sections.iter_mut().filter(|s| s.kind == "verse") {
                        s.layers.retain(|l| l != "open_hat" && l != "counter");
                    }
                    let o = k.offsets.entry("counter".into()).or_insert(0.0);
                    *o = (*o + 1.0).min(4.0);
                    done.push(format!(
                        "{}: verse velocity {v:.2}, hooks get counter-melody + open hats",
                        f.id
                    ));
                }
            }
            "vary" => {
                k.variation_seed += 1;
                done.push(format!(
                    "{}: develop the motif differently (variation {})",
                    f.id, k.variation_seed
                ));
            }
            "density" => {
                let d = f.fix["lead_density"].as_f64().unwrap_or(0.5) as f32;
                k.lead_density = d;
                done.push(format!("{}: lead density {d:.2}", f.id));
            }
            "tonic" => {
                if !k.tonic_anchor {
                    k.tonic_anchor = true;
                    done.push(format!(
                        "{}: anchor the tonic (bass resolves home in the last section)",
                        f.id
                    ));
                }
            }
            "declick" => {
                if k.humanize > 0.0 {
                    k.humanize = 0.0;
                    done.push(format!(
                        "{}: remove timing humanize (overlapping retriggers)",
                        f.id
                    ));
                }
            }
            _ => {}
        }
    }
    done
}

// ---------------------------------------------------------------- produce

pub struct ProduceOpts {
    pub max_iterations: usize,
    pub reference: Option<String>,
    pub out_dir: Option<std::path::PathBuf>,
    pub mp3: bool,
}

pub fn produce(e: &mut Engine, args: &PlanArgs, o: &ProduceOpts) -> Result<Value> {
    let t0 = std::time::Instant::now();
    let mut plan = plan_track(args)?;
    if let Some(k) = &plan.sample_kit {
        // fetch the CC0/PD kit once; fall back to the synth kit offline
        match crate::sample_lib::install_kit(e, k) {
            Ok(_) => plan.thinking.push(format!(
                "Drums: real {k} samples (public domain/CC0, credits kept in the project)."
            )),
            Err(err) => {
                plan.thinking.push(format!(
                    "Drums: synth kit ({k} samples unavailable: {err})."
                ));
                plan.sample_kit = None;
            }
        }
    }
    let mut log = Vec::new();
    let mut best: Option<(f32, Project, Plan, Critique)> = None;
    let mut last_score = -1.0f32;
    let mut stale = 0;
    for it in 0..o.max_iterations.max(1) {
        let ti = std::time::Instant::now();
        apply_plan(e, &plan)?;
        let c = critique(e, &plan, o.reference.as_deref())?;
        let entry = json!({
            "iteration": it + 1, "score": c.score, "technical": c.technical, "musical": c.musical, "reference": c.reference,
            "top_findings": c.findings.iter().take(5).map(|f| f.message.clone()).collect::<Vec<_>>(),
            "seconds": (ti.elapsed().as_secs_f32() * 10.0).round() / 10.0,
        });
        let improved = c.score > last_score + 0.5;
        if best.as_ref().map(|b| c.score > b.0).unwrap_or(true) {
            best = Some((c.score, e.project.clone(), plan.clone(), c.clone()));
        }
        stale = if improved { 0 } else { stale + 1 };
        last_score = last_score.max(c.score);
        let mut entry = entry;
        if it + 1 < o.max_iterations && stale < 2 {
            let changes = revise(&mut plan, &c, 3);
            entry["revisions"] = json!(changes);
            log.push(entry);
            if changes.is_empty() {
                break;
            }
        } else {
            log.push(entry);
            break;
        }
    }
    let (score, proj, plan, crit) = best.ok_or_else(|| anyhow!("no iteration ran"))?;
    e.replace_project(proj);
    e.revision += 1;
    // deliver
    let mut files = json!({});
    if let Some(dir) = &o.out_dir {
        std::fs::create_dir_all(dir)?;
        let stem = crate::samples::sample_name(&format!("{}_{}", plan.genre, plan.seed));
        let pj = dir.join(format!("{stem}.json"));
        std::fs::write(&pj, serde_json::to_string_pretty(&e.project)?)?;
        let plan_path = dir.join(format!("{stem}.plan.json"));
        std::fs::write(&plan_path, serde_json::to_string_pretty(&plan)?)?;
        files["project"] = json!(pj.to_string_lossy());
        files["plan"] = json!(plan_path.to_string_lossy());
        let fmt = if o.mp3 && crate::tools_delivery::ffmpeg_available() {
            "mp3"
        } else {
            "wav"
        };
        let audio = dir.join(format!("{stem}.{fmt}"));
        let ex = e.call_from("export_audio", &json!({"path": audio.to_string_lossy(), "format": fmt, "target_lufs": plan.knobs.target_lufs, "true_peak_ceiling": -1.0}), "producer")?;
        files["audio"] = json!(audio.to_string_lossy());
        files["export"] = compact(&ex, 700);
    }
    Ok(json!({
        "title": plan.title,
        "genre": plan.genre, "key": format!("{} {}", plan.key, plan.scale), "bpm": plan.bpm,
        "score": score, "technical": crit.technical, "musical": crit.musical, "reference_match": crit.reference,
        "iterations": log, "thinking": plan.thinking, "plan": plan,
        "remaining_findings": crit.findings.iter().take(6).map(|f| f.message.clone()).collect::<Vec<_>>(),
        "measurements": crit.measurements,
        "files": files,
        "wall_seconds": (t0.elapsed().as_secs_f32() * 10.0).round() / 10.0,
    }))
}

pub fn plan_from_value(v: &Value) -> Result<Plan> {
    if v.is_null() {
        bail!("missing 'plan' (call plan_track first and pass its plan)");
    }
    Ok(serde_json::from_value(v.clone())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(brief: &str, genre: Option<&str>, seed: u64) -> PlanArgs {
        PlanArgs {
            brief: brief.into(),
            genre: genre.map(String::from),
            bpm: None,
            key: None,
            scale: None,
            mood: None,
            duration_s: Some(60.0),
            seed,
            use_samples: false,
            flip_sample: None,
        }
    }

    fn engine() -> Engine {
        let d = std::env::temp_dir().join("beatbox_producer_tests");
        std::fs::create_dir_all(&d).unwrap();
        Engine::new(d)
    }

    #[test]
    fn playbooks_are_valid_data() {
        assert!(playbooks().len() >= 7);
        for p in playbooks() {
            for r in [&p.harmony, &p.lead, &p.counter]
                .into_iter()
                .chain(p.texture.iter())
            {
                for pr in r.presets.iter().chain(r.preset.iter()) {
                    assert!(
                        crate::instruments::preset(pr).is_some(),
                        "{}: unknown preset {pr}",
                        p.name
                    );
                }
            }
            assert!(
                crate::instruments::preset(&p.bass.preset).is_some(),
                "{}: bass {}",
                p.name,
                p.bass.preset
            );
            for v in p.drums.kit.values() {
                assert!(
                    crate::instruments::preset(v).is_some(),
                    "{}: kit {v}",
                    p.name
                );
            }
            for sc in &p.scales {
                let iv = theory::scale_intervals(sc).unwrap();
                assert!(iv.len() >= 5);
                for progs in p.progressions.values() {
                    for pr in progs {
                        theory::parse_progression(pr, 0, harmony_ref(sc))
                            .unwrap_or_else(|e| panic!("{} {pr}: {e}", p.name));
                    }
                }
            }
            for list in [
                &p.drums.kick,
                &p.drums.snare,
                &p.drums.hat,
                &p.drums.open_hat,
                &p.drums.perc,
                &p.drums.tabla,
                &p.drums.bayan,
            ] {
                for s in list {
                    assert!(
                        s.chars().all(|c| "Xxo.".contains(c)),
                        "{}: bad step string {s}",
                        p.name
                    );
                    assert!(
                        s.is_empty() || s.len() == 16 || s.len() == 32,
                        "{}: {s}",
                        p.name
                    );
                }
            }
            assert!(p.bpm[0] <= p.bpm[1]);
        }
    }

    #[test]
    fn genre_guessing_and_bII_reference() {
        assert_eq!(
            guess_genre("dark desi hip-hop with sitar and tabla")
                .unwrap()
                .name,
            "desi_hiphop"
        );
        assert_eq!(
            guess_genre("melodic trap for a sad song").unwrap().name,
            "melodic_rap"
        );
        assert_eq!(guess_genre("90s boom bap").unwrap().name, "boom_bap");
        // bII is a half step above the tonic whatever the melodic scale
        for sc in ["bhairav", "phrygian", "minor", "phrygian_dominant"] {
            let c = theory::parse_progression("bII", 0, harmony_ref(sc)).unwrap();
            assert_eq!(c[0].root_pc, 1, "{sc}");
        }
    }

    #[test]
    fn plan_is_deterministic_and_structured() {
        let a = plan_track(&args("hard trap", Some("trap"), 7)).unwrap();
        let b = plan_track(&args("hard trap", Some("trap"), 7)).unwrap();
        assert_eq!(a, b);
        let c = plan_track(&args("hard trap", Some("trap"), 8)).unwrap();
        assert_ne!(a.motif, c.motif);
        assert!(a
            .sections
            .iter()
            .any(|s| s.kind == "hook" && s.lead == "full"));
        assert!(!a.thinking.is_empty());
        assert!(a.bpm >= 130.0 && a.bpm <= 150.0);
    }

    #[test]
    fn compose_develops_the_motif_and_glides_808s() {
        let mut e = engine();
        let mut a = args("dark trap", Some("trap"), 3);
        a.duration_s = None;
        let plan = plan_track(&a).unwrap();
        compose(&mut e, &plan).unwrap();
        let p = &e.project;
        assert_eq!(p.arrangement.len(), plan.sections.len());
        let lead = |n: &str| {
            p.patterns[p.pattern_index(n).unwrap()]
                .notes("lead")
                .to_vec()
        };
        assert!(!lead("hook1").is_empty());
        assert_ne!(lead("hook1"), lead("verse1"));
        let glides = p
            .patterns
            .iter()
            .flat_map(|pt| pt.notes("bass"))
            .filter(|n| n.slide_to.is_some())
            .count();
        assert!(glides > 0, "trap 808 should glide somewhere");
        assert!(p.buses.iter().any(|b| b.name == "verb"));
        // last hook lifts an octave
        let hooks: Vec<&PlanSection> = plan.sections.iter().filter(|s| s.kind == "hook").collect();
        let first = lead(&hooks[0].name);
        let last = lead(&hooks[hooks.len() - 1].name);
        let avg =
            |v: &[Note]| v.iter().map(|n| n.pitch as f32).sum::<f32>() / v.len().max(1) as f32;
        assert!(avg(&last) > avg(&first) + 6.0);
    }

    #[test]
    fn desi_plan_uses_indian_palette_and_tabla() {
        let mut e = engine();
        let plan = plan_track(&args("desi hip hop with sitar", None, 11)).unwrap();
        assert_eq!(plan.genre, "desi_hiphop");
        assert_eq!(plan.palette.get("lead").map(String::as_str), Some("sitar"));
        compose(&mut e, &plan).unwrap();
        let tabla: usize = e
            .project
            .patterns
            .iter()
            .map(|p| p.notes("tabla").len())
            .sum();
        assert!(tabla > 20);
    }

    #[test]
    fn revise_turns_findings_into_knob_changes() {
        let mut plan = plan_track(&args("trap", Some("trap"), 1)).unwrap();
        let c = Critique {
            score: 60.0,
            technical: 70.0,
            musical: 50.0,
            reference: None,
            measurements: json!({}),
            findings: vec![
                Finding {
                    id: "hook_lift".into(),
                    severity: 0.8,
                    message: "x".into(),
                    fix: json!({"action": "contrast", "verse_velocity": 0.8}),
                },
                Finding {
                    id: "repetition".into(),
                    severity: 0.5,
                    message: "y".into(),
                    fix: json!({"action": "vary", "variation_seed": 1}),
                },
                Finding {
                    id: "mix".into(),
                    severity: 0.3,
                    message: "z".into(),
                    fix: json!({"action": "offset", "track": "hat", "db": 2.0}),
                },
            ],
        };
        let ch = revise(&mut plan, &c, 3);
        assert_eq!(ch.len(), 3);
        assert!((plan.knobs.verse_velocity - 0.8).abs() < 1e-6);
        assert_eq!(plan.knobs.variation_seed, 1);
        assert_eq!(plan.knobs.offsets.get("hat"), Some(&2.0));
    }

    #[test]
    fn produce_track_end_to_end() {
        let mut e = engine();
        let mut a = args("chill lofi", Some("lofi"), 5);
        a.duration_s = Some(40.0);
        let r = produce(
            &mut e,
            &a,
            &ProduceOpts {
                max_iterations: 2,
                reference: None,
                out_dir: None,
                mp3: false,
            },
        )
        .unwrap();
        assert!(r["score"].as_f64().unwrap() > 0.0);
        assert!(!r["iterations"].as_array().unwrap().is_empty());
        assert!(e.project.tracks.len() >= 5);
        assert!(r["measurements"]["integrated_lufs"].as_f64().unwrap() > -30.0);
    }
}
