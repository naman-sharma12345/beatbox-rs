//! Sprint 9: the creative layer of the producer.
//!
//! Decisions are hierarchical, not a flat random draw:
//!
//! 1. **Direction** (mood, energy, emotional intent, identity, hero element)
//!    comes first and is recorded with its reasons.
//! 2. **Core material** is chosen *by* the direction so the parts complement
//!    each other: generation method (template / procedural / reference-guided
//!    / AI-authored), tempo, mode, key, harmony (a pool of progressions with
//!    modal/borrowed chords), the main motif (contour + rhythm cell), the
//!    drum groove (genre-conditioned probability grids, Euclidean and
//!    polyrhythmic percussion, hat rates and accents, snare placement),
//!    the 808 behaviour, the palette and the arrangement form.
//! 3. **Controlled variation** happens inside those relationships: per-section
//!    groove variation and generated fills, motif development (inversion,
//!    displacement, register, fragmentation, call and response), counter
//!    lines, and 1-3 purposeful wildcards (each with a stated role).
//!
//! Randomness only ever picks *within* the space the level above allowed.
//! Templates from playbooks.json are kept: they are one starting point
//! (the `template` method) and the prior the probability grids are learned from.

use crate::dsp::Rng;
use crate::producer::{MotifNote, PlanSection, Playbook, SectionTemplate};
use crate::project::Note;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Bumped whenever the generator's choices change for the same seed.
pub const GENERATOR_VERSION: &str = concat!(
    "beatbox-producer/",
    env!("CARGO_PKG_VERSION"),
    "+s9-creative.1"
);

// ---------------------------------------------------------------- seeds

/// A fresh seed from OS entropy (std's RandomState keys come from the OS)
/// mixed with the clock and the process id. Kept below 2^53 so it survives
/// a round trip through JSON numbers exactly.
pub fn fresh_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    h.write_u128(t);
    h.write_u32(std::process::id());
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let mut b = [0u8; 8];
        if f.read_exact(&mut b).is_ok() {
            h.write(&b);
        }
    }
    splitmix(h.finish()) & ((1u64 << 53) - 1)
}

pub fn splitmix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

// ---------------------------------------------------------------- records

/// One decision and why it was made (recorded in the plan, in order).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Decision {
    /// Level: direction, core, variation, wildcard.
    pub level: String,
    pub what: String,
    pub choice: String,
    pub why: String,
}

pub fn decide(
    v: &mut Vec<Decision>,
    level: &str,
    what: &str,
    choice: impl Into<String>,
    why: impl Into<String>,
) {
    v.push(Decision {
        level: level.into(),
        what: what.into(),
        choice: choice.into(),
        why: why.into(),
    });
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Direction {
    pub mood: String,
    /// 0..1 overall energy.
    pub energy: f32,
    /// The emotional intent in a few words ("menace", "bittersweet memory").
    pub intent: String,
    /// What carries the beat: motif, groove, bass or texture.
    pub hero: String,
    /// sparse, balanced or dense.
    pub density: String,
    /// One line that ties it together.
    pub identity: String,
}

/// A purposeful off-playbook choice.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Wildcard {
    pub name: String,
    /// tension, release, contrast, groove or emotion.
    pub role: String,
    /// Where it lands (section name, track).
    pub target: String,
    pub why: String,
}

// ---------------------------------------------------------------- genre distributions

/// A genre as a distribution (playbooks.json "generative"; every field is
/// optional and falls back to defaults derived from the playbook itself).
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct GenSpec {
    #[serde(default)]
    pub template_weight: Option<f32>,
    #[serde(default)]
    pub swing: Option<[f32; 2]>,
    #[serde(default)]
    pub kick_per_bar: Option<[f32; 2]>,
    #[serde(default)]
    pub snare_modes: BTreeMap<String, f32>,
    #[serde(default)]
    pub snare_variations: BTreeMap<String, f32>,
    #[serde(default)]
    pub hat_rates: BTreeMap<String, f32>,
    #[serde(default)]
    pub hat_rolls: Option<[f32; 2]>,
    #[serde(default)]
    pub ghost: Option<[f32; 2]>,
    #[serde(default)]
    pub perc_modes: BTreeMap<String, f32>,
    #[serde(default)]
    pub modes: Vec<String>,
    #[serde(default)]
    pub progressions: Vec<String>,
    #[serde(default)]
    pub bars_per_chord: Vec<f32>,
    #[serde(default)]
    pub bass_modes: BTreeMap<String, f32>,
    #[serde(default)]
    pub glide: Option<[f32; 2]>,
    #[serde(default)]
    pub harmony_styles: BTreeMap<String, f32>,
    #[serde(default)]
    pub kits: BTreeMap<String, f32>,
    #[serde(default)]
    pub drum_sounds: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub forms: BTreeMap<String, f32>,
    /// Per-genre multipliers on the random wildcard draw (0 = never drawn at
    /// random; a structured-intent contrast can still force it).
    #[serde(default)]
    pub wildcard_weights: BTreeMap<String, f32>,
}

pub fn wmap_pub(m: &BTreeMap<String, f32>, rng: &mut Rng) -> Option<String> {
    wmap(m, rng)
}

fn wmap(m: &BTreeMap<String, f32>, rng: &mut Rng) -> Option<String> {
    if m.is_empty() {
        return None;
    }
    let keys: Vec<&String> = m.keys().collect();
    let w: Vec<f32> = m.values().map(|x| x.max(0.0)).collect();
    Some(keys[rng.weighted(&w)].clone())
}

fn wlist(items: &[(&str, f32)], rng: &mut Rng) -> String {
    let w: Vec<f32> = items.iter().map(|x| x.1.max(0.0)).collect();
    items[rng.weighted(&w)].0.to_string()
}

fn m(items: &[(&str, f32)]) -> BTreeMap<String, f32> {
    items.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

/// The genre's distribution with defaults filled in.
pub fn spec(pb: &Playbook) -> GenSpec {
    let mut s = pb.generative.clone().unwrap_or_default();
    let d = &pb.drums;
    if s.swing.is_none() {
        s.swing = Some([(pb.swing - 0.06).max(0.0), pb.swing + 0.08]);
    }
    if s.kick_per_bar.is_none() {
        s.kick_per_bar = Some([2.0, 4.5]);
    }
    if s.snare_modes.is_empty() {
        s.snare_modes = if d.half_time {
            m(&[("anchor", 2.0), ("half_time", 1.0)])
        } else {
            m(&[("anchor", 2.0), ("backbeat", 1.0)])
        };
    }
    if s.snare_variations.is_empty() {
        s.snare_variations = m(&[
            ("none", 1.2),
            ("displaced", 0.8),
            ("pickup", 0.8),
            ("double", 0.4),
        ]);
    }
    if s.hat_rates.is_empty() {
        s.hat_rates = m(&[("8", 1.5), ("16", 1.0), ("16_gallop", 0.5), ("8t", 0.4)]);
    }
    if s.hat_rolls.is_none() {
        s.hat_rolls = Some([(d.hat_rolls - 0.15).max(0.0), (d.hat_rolls + 0.2).min(0.9)]);
    }
    if s.ghost.is_none() {
        s.ghost = Some([(d.ghost_snare - 0.1).max(0.0), d.ghost_snare + 0.15]);
    }
    if s.perc_modes.is_empty() {
        s.perc_modes = m(&[
            ("euclid", 1.0),
            ("poly", 0.6),
            ("sparse", 0.6),
            ("template", 0.4),
        ]);
    }
    if s.bars_per_chord.is_empty() {
        s.bars_per_chord = vec![pb.bars_per_chord];
    }
    if s.bass_modes.is_empty() {
        s.bass_modes = if pb.bass.style == "808_glide" {
            m(&[("lock", 1.5), ("independent", 1.0), ("sustain", 0.4)])
        } else {
            m(&[("lock", 1.0)])
        };
    }
    if s.glide.is_none() {
        s.glide = Some([
            (pb.bass.glide - 0.1).max(0.0),
            (pb.bass.glide + 0.2).min(0.9),
        ]);
    }
    if s.harmony_styles.is_empty() {
        s.harmony_styles = m(&[(pb.harmony.style.as_deref().unwrap_or("block"), 1.0)]);
    }
    if s.kits.is_empty() {
        if let Some(k) = &d.sample_kit {
            s.kits = m(&[(k.as_str(), 1.0)]);
        }
    }
    if s.forms.is_empty() {
        s.forms = m(&[
            ("template", 1.0),
            ("standard", 1.0),
            ("hook_first", 1.0),
            ("with_bridge", 0.8),
            ("build", 0.6),
        ]);
    }
    if s.template_weight.is_none() {
        s.template_weight = Some(0.3);
    }
    s
}

// ---------------------------------------------------------------- direction

const INTENTS: &[(&str, &[&str])] = &[
    (
        "dark",
        &[
            "menace",
            "paranoia",
            "cold confidence",
            "night-drive dread",
            "villain entrance",
        ],
    ),
    (
        "sad",
        &[
            "longing",
            "numb heartbreak",
            "bittersweet memory",
            "rainy-window loneliness",
        ],
    ),
    (
        "hype",
        &["victory lap", "adrenaline", "swagger", "crowd chant"],
    ),
    ("chill", &["late-night calm", "daydream", "warm nostalgia"]),
    (
        "jazzy",
        &["smoky nostalgia", "head-nod cool", "basement cypher"],
    ),
    (
        "hopeful",
        &[
            "rising up",
            "sunrise after a long night",
            "triumph after struggle",
        ],
    ),
    ("devotional", &["stillness", "devotion", "temple dawn"]),
    ("smooth", &["intimacy", "slow burn", "velvet late night"]),
    (
        "default",
        &["swagger", "tension", "focus", "restless energy"],
    ),
];

fn base_energy(mood: &str) -> f32 {
    match mood {
        "dark" => 0.7,
        "hype" => 0.9,
        "sad" => 0.45,
        "chill" => 0.3,
        "jazzy" => 0.5,
        "hopeful" => 0.65,
        "devotional" => 0.4,
        "smooth" => 0.4,
        _ => 0.6,
    }
}

pub fn direct(
    pb: &Playbook,
    brief: &str,
    mood: (String, String),
    rng: &mut Rng,
    dec: &mut Vec<Decision>,
) -> Direction {
    let b = brief.to_lowercase();
    let (mood, mood_why) = mood;
    decide(dec, "direction", "mood", &mood, mood_why);
    let mut energy = base_energy(&mood) + rng.range(-0.15, 0.15);
    for (w, d) in [
        ("hard", 0.1),
        ("aggressive", 0.12),
        ("bounce", 0.08),
        ("calm", -0.12),
        ("soft", -0.1),
        ("slow", -0.08),
    ] {
        if b.contains(w) {
            energy += d;
        }
    }
    let energy = energy.clamp(0.15, 1.0);
    decide(dec, "direction", "energy", format!("{energy:.2}"), format!("'{mood}' sits around {:.2}; jittered so two {mood} beats don't share a level, nudged by words in the brief", base_energy(&mood)));
    let pool = INTENTS
        .iter()
        .find(|(k, _)| *k == mood)
        .or_else(|| INTENTS.iter().find(|(k, _)| *k == "default"))
        .map(|x| x.1)
        .unwrap_or(&["focus"]);
    let intent = pool[rng.below(pool.len())].to_string();
    decide(
        dec,
        "direction",
        "intent",
        &intent,
        format!(
            "one emotional target from the '{mood}' family; every later choice should serve it"
        ),
    );
    let mut hw = vec![
        ("motif", 0.35f32),
        ("groove", 0.25 + 0.2 * energy),
        (
            "bass",
            if pb.bass.style == "808_glide" {
                0.25
            } else {
                0.1
            },
        ),
        ("texture", 0.1 + 0.25 * (1.0 - energy)),
    ];
    let mut forced = None;
    for (w, h) in [
        ("808", "bass"),
        ("melod", "motif"),
        ("hook", "motif"),
        ("bounce", "groove"),
        ("groove", "groove"),
        ("drums", "groove"),
        ("ambient", "texture"),
        ("atmos", "texture"),
    ] {
        if b.contains(w) {
            forced = Some(h);
            break;
        }
    }
    if let Some(h) = forced {
        for x in hw.iter_mut() {
            if x.0 == h {
                x.1 += 0.6;
            }
        }
    }
    let hero = wlist(&hw, rng);
    let density = if energy < 0.4 {
        wlist(&[("sparse", 0.6), ("balanced", 0.4)], rng)
    } else if energy > 0.75 {
        wlist(&[("dense", 0.55), ("balanced", 0.45)], rng)
    } else {
        wlist(&[("balanced", 0.6), ("sparse", 0.2), ("dense", 0.2)], rng)
    };
    let hero_desc = match hero.as_str() {
        "motif" => "a hook motif you can hum; drums and 808 support it",
        "groove" => "the drum pocket is the hook; the melody stays out of its way",
        "bass" => "the 808 carries the tune; chords are a bed",
        _ => "atmosphere first: pads and space, drums leave room",
    };
    decide(
        dec,
        "direction",
        "hero",
        &hero,
        format!(
            "{hero_desc}{}",
            if forced.is_some() {
                " (the brief points at it)"
            } else {
                ""
            }
        ),
    );
    decide(
        dec,
        "direction",
        "density",
        &density,
        format!("energy {energy:.2} -> {density} arrangement and drums"),
    );
    let identity = format!("{intent}: {hero}-led, {density} - {hero_desc}");
    Direction {
        mood,
        energy,
        intent,
        hero,
        density,
        identity,
    }
}

// ---------------------------------------------------------------- harmony

pub fn mood_modes(mood: &str) -> &'static [&'static str] {
    match mood {
        "dark" => &["harmonic_minor", "phrygian", "minor"],
        "sad" => &["minor", "dorian", "melodic_minor"],
        "hype" => &["minor", "phrygian"],
        "hopeful" => &["dorian", "major", "mixolydian"],
        "chill" => &["dorian", "major", "lydian"],
        "jazzy" => &["dorian", "minor"],
        "devotional" => &["bhairav"],
        "smooth" => &["dorian", "major"],
        _ => &[],
    }
}

/// Modal and borrowed progressions per harmonic reference (roman numerals are
/// relative to the parallel natural minor / major).
pub fn borrowed_pool(href: &str) -> &'static [&'static str] {
    if href == "minor" {
        &[
            "i IV VI v",
            "i bII VI V",
            "i VI iv V7",
            "i v iv VI",
            "iv i VI V",
            "i III VII IV",
            "i VII VI V",
            "i iv bII i",
            "VI iv i V",
            "i VI III V",
        ]
    } else {
        &[
            "I bVII IV I",
            "I iv I bVI",
            "Imaj7 bVImaj7 IVmaj7 iv",
            "vi IV I V",
            "I V vi iii IV",
            "IVmaj7 V7 iii7 vi7",
            "I bIII IV I",
            "ii7 V7 Imaj7 bVII7",
        ]
    }
}

pub struct HarmonyChoice {
    pub verse: String,
    pub hook: String,
    pub bridge: String,
    pub color: String,
    pub bars_per_chord: f32,
}

#[allow(clippy::too_many_arguments)]
pub fn choose_harmony(
    pb: &Playbook,
    sp: &GenSpec,
    dir: &Direction,
    scale: &str,
    template: bool,
    rng: &mut Rng,
    dec: &mut Vec<Decision>,
) -> HarmonyChoice {
    let href = crate::producer::harmony_ref(scale);
    // pool: the mood's progressions weigh most, then the rest of the genre,
    // then modal/borrowed colour (procedural only)
    let mut pool: Vec<(String, f32, &str)> = Vec::new();
    let mood_list = pb
        .progressions
        .get(&dir.mood)
        .or_else(|| pb.progressions.get("default"));
    if let Some(l) = mood_list {
        for p in l {
            pool.push((p.clone(), 3.0, "the mood's own progressions"));
        }
    }
    if !template {
        for (k, l) in &pb.progressions {
            if *k == dir.mood {
                continue;
            }
            for p in l {
                if !pool.iter().any(|x| x.0 == *p) {
                    pool.push((p.clone(), 1.0, "the genre's other progressions"));
                }
            }
        }
        // borrowed colour only where the genre's harmony is functional
        if pb.harmony.style.as_deref() != Some("drone") {
            for p in sp
                .progressions
                .iter()
                .map(String::as_str)
                .chain(borrowed_pool(href).iter().copied())
            {
                if crate::theory::parse_progression(p, 0, href).is_ok()
                    && !pool.iter().any(|x| x.0 == p)
                {
                    pool.push((p.to_string(), 1.2, "modal/borrowed colour"));
                }
            }
        }
    }
    let w: Vec<f32> = pool.iter().map(|x| x.1).collect();
    let vi = rng.weighted(&w);
    let verse = pool[vi].0.clone();
    decide(
        dec,
        "core",
        "verse progression",
        &verse,
        format!("from {} (pool of {})", pool[vi].2, pool.len()),
    );
    let hook = if pool.len() > 1 && rng.chance(0.65) {
        let mut w2 = w.clone();
        w2[vi] = 0.0;
        let hi = rng.weighted(&w2);
        decide(
            dec,
            "core",
            "hook progression",
            &pool[hi].0,
            "a different progression for the hook so it lifts away from the verse",
        );
        pool[hi].0.clone()
    } else {
        decide(
            dec,
            "core",
            "hook progression",
            &verse,
            "same as the verse: the hook lifts through arrangement and melody instead",
        );
        verse.clone()
    };
    let bridge = if !template && pb.harmony.style.as_deref() != Some("drone") && rng.chance(0.6) {
        let b = borrowed_pool(href);
        let p = b[rng.below(b.len())].to_string();
        decide(
            dec,
            "core",
            "bridge progression",
            &p,
            "borrowed colour for the bridge/breakdown: contrast before the last hook",
        );
        p
    } else {
        String::new()
    };
    let color = if template || pb.harmony.style.as_deref() == Some("drone") {
        "as_written".to_string()
    } else {
        let mut cw = vec![
            ("as_written", 2.0f32),
            ("sevenths", 0.6),
            ("add9", 0.4),
            ("sus", 0.3),
            ("power", 0.2),
        ];
        match dir.mood.as_str() {
            "sad" | "smooth" | "chill" | "jazzy" => {
                cw[1].1 += 1.2;
                cw[2].1 += 0.6
            }
            "hopeful" => cw[3].1 += 1.0,
            "dark" | "hype" => cw[4].1 += 0.6,
            _ => {}
        }
        wlist(&cw, rng)
    };
    if color != "as_written" {
        decide(
            dec,
            "core",
            "chord colour",
            &color,
            format!("triads coloured for '{}'", dir.intent),
        );
    }
    let bpc_pool: Vec<f32> = sp.bars_per_chord.clone();
    let mut bpc = bpc_pool[rng
        .below(bpc_pool.len().max(1))
        .min(bpc_pool.len().saturating_sub(1))];
    if !template {
        let alt = match dir.density.as_str() {
            "dense" if bpc >= 2.0 => Some(bpc / 2.0),
            "sparse" if bpc <= 1.0 => Some(bpc * 2.0),
            _ => None,
        };
        if let Some(a) = alt {
            if rng.chance(0.5) {
                bpc = a;
            }
        }
    }
    decide(
        dec,
        "core",
        "harmonic rhythm",
        format!("{bpc} bar(s) per chord"),
        format!("{} density", dir.density),
    );
    HarmonyChoice {
        verse,
        hook,
        bridge,
        color,
        bars_per_chord: bpc,
    }
}

/// Re-colour triads (sevenths, add9, sus, power).
pub fn color_chords(chords: &mut [crate::theory::Chord], color: &str, key_pc: u8) {
    for c in chords.iter_mut() {
        if c.intervals.len() != 3 {
            continue;
        }
        let minor = c.intervals == [0, 3, 7];
        let major = c.intervals == [0, 4, 7];
        if !minor && !major {
            continue;
        }
        match color {
            "sevenths" => {
                let rel = (c.root_pc + 12 - key_pc) % 12;
                c.intervals
                    .push(if major && (rel == 0 || rel == 5 || rel == 8 || rel == 3) {
                        11
                    } else {
                        10
                    });
            }
            "add9" => c.intervals.push(14),
            "sus" if c.root_pc == key_pc || major => c.intervals = vec![0, 2, 7],
            "power" => c.intervals = vec![0, 7, 12],
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- motif

/// A motif with an intended contour and rhythm cell (one bar).
pub fn shaped_motif(
    rng: &mut Rng,
    density: f32,
    scale_len: usize,
    contour: &str,
    cell: &str,
) -> Vec<MotifNote> {
    let n = (3.0 + density * 5.0 + rng.range(-0.6, 0.6))
        .round()
        .clamp(3.0, 8.0) as usize;
    let grid: Vec<(f32, f32)> = match cell {
        "syncopated" => [0.0, 3.0, 6.0, 7.0, 10.0, 11.0, 13.0, 14.0, 2.0, 8.0]
            .iter()
            .enumerate()
            .map(|(i, t)| (*t, 1.0 - i as f32 * 0.06))
            .collect(),
        "triplet" => [0.0, 2.667, 5.333, 8.0, 10.667, 13.333, 4.0, 12.0]
            .iter()
            .enumerate()
            .map(|(i, t)| (*t, 1.0 - i as f32 * 0.08))
            .collect(),
        "long_short" => [0.0, 3.0, 4.0, 7.0, 8.0, 11.0, 12.0, 15.0]
            .iter()
            .map(|t| (*t, 1.0))
            .collect(),
        _ => [0.0, 4.0, 8.0, 12.0, 2.0, 6.0, 10.0, 14.0, 3.0, 11.0]
            .iter()
            .enumerate()
            .map(|(i, t)| (*t, 1.0 - i as f32 * 0.07))
            .collect(),
    };
    let mut onsets = vec![0.0f32];
    let mut w: Vec<f32> = grid
        .iter()
        .map(|x| if x.0 == 0.0 { 0.0 } else { x.1 })
        .collect();
    while onsets.len() < n && w.iter().any(|x| *x > 0.0) {
        let i = rng.weighted(&w);
        w[i] = 0.0;
        onsets.push(grid[i].0);
    }
    onsets.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = onsets.len();
    let sl = scale_len as i32;
    let peak = 4 + rng.below(3) as i32;
    let degs: Vec<i32> = (0..n)
        .map(|i| {
            let x = if n > 1 {
                i as f32 / (n - 1) as f32
            } else {
                0.0
            };
            let j = [-1, 0, 0, 1][rng.below(4)];
            match contour {
                "arch" => {
                    ((1.0 - (2.0 * x - 1.0).abs()) * peak as f32).round() as i32
                        + if i > 0 && i + 1 < n { j } else { 0 }
                }
                "descending" => {
                    ((1.0 - x) * peak as f32).round() as i32
                        + if i > 0 && i + 1 < n { j } else { 0 }
                }
                "ascending" => {
                    (x * peak as f32).round() as i32 + if i > 0 && i + 1 < n { j } else { 0 }
                }
                "leap_fall" => {
                    if i == 0 {
                        0
                    } else {
                        (peak + 1 - (i as i32 - 1) * 2).max(0)
                    }
                }
                "static" => {
                    if i % 2 == 1 {
                        [1, -1, 2][rng.below(3)]
                    } else {
                        0
                    }
                }
                _ => {
                    // wave
                    ((x * std::f32::consts::TAU * 1.5).sin() * 2.5).round() as i32 + 2
                }
            }
        })
        .collect();
    let mut out = Vec::new();
    for (i, &t) in onsets.iter().enumerate() {
        let mut deg = degs[i];
        if i + 1 == n {
            // land on a stable degree
            let stable = [0, 2, 4];
            let s = stable
                .iter()
                .min_by_key(|s| (deg.rem_euclid(sl) - **s).abs())
                .copied()
                .unwrap_or(0);
            deg = deg - deg.rem_euclid(sl) + s;
        }
        let next = onsets.get(i + 1).copied().unwrap_or(16.0);
        let len = ((next - t) * if rng.chance(0.3) { 0.5 } else { 0.9 }).max(0.5);
        out.push(MotifNote {
            t,
            deg: deg.clamp(-3, sl + 4),
            len,
        });
    }
    out
}

/// Develop a motif: the same material, transformed. Returns the new motif
/// and an octave shift for the section.
pub fn develop(motif: &[MotifNote], ops: &[String], rng: &mut Rng) -> (Vec<MotifNote>, i32) {
    let mut v = motif.to_vec();
    let mut oct = 0;
    if v.is_empty() {
        return (v, 0);
    }
    for op in ops {
        match op.as_str() {
            "inversion" => {
                let f = v[0].deg;
                for x in v.iter_mut() {
                    x.deg = f - (x.deg - f);
                }
            }
            "retrograde" => {
                let d: Vec<i32> = v.iter().rev().map(|x| x.deg).collect();
                for (x, d) in v.iter_mut().zip(d) {
                    x.deg = d;
                }
            }
            "displace" => {
                let s = [2.0f32, 3.0, 4.0][rng.below(3)];
                for x in v.iter_mut() {
                    x.t += s;
                    if x.t >= 16.0 {
                        x.t -= 16.0;
                    }
                }
                v.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap());
                v.dedup_by(|a, b| (a.t - b.t).abs() < 0.25);
                let starts: Vec<f32> = v.iter().map(|x| x.t).collect();
                for (i, x) in v.iter_mut().enumerate() {
                    let next = starts.get(i + 1).copied().unwrap_or(16.0);
                    x.len = x.len.min(next - x.t).max(0.5);
                }
            }
            "fragment" if v.len() >= 2 => {
                let k = v.len().div_ceil(2).clamp(2, v.len());
                v.truncate(k);
                let span = v.last().map(|x| x.t + x.len).unwrap_or(16.0);
                if span <= 8.0 {
                    let rep: Vec<MotifNote> = v
                        .iter()
                        .map(|x| MotifNote {
                            t: x.t + 8.0,
                            deg: x.deg,
                            len: x.len,
                        })
                        .collect();
                    v.extend(rep);
                }
            }
            "sequence" => {
                for x in v.iter_mut() {
                    x.deg += 2;
                }
            }
            "register_up" => oct += 1,
            "register_down" => oct -= 1,
            _ => {}
        }
    }
    (v, oct)
}

// ---------------------------------------------------------------- groove

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Hit {
    /// Step within the 2-bar core (0..32).
    pub t: f32,
    pub v: f32,
}

/// The beat's drum identity: a 2-bar core per voice plus how it moves.
/// Sections vary it (see `section_drums`); the critic never touches it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Groove {
    pub source: String,
    pub kick: Vec<Hit>,
    /// Plausible extra kick positions (hooks add them, phrase ends move to them).
    pub kick_spare: Vec<f32>,
    pub snare: Vec<Hit>,
    pub snare_mode: String,
    pub hat: Vec<Hit>,
    pub hat_rate: String,
    pub hat_accent: String,
    pub hat_rolls: f32,
    pub ghosts: f32,
    pub open_hat: Vec<Hit>,
    pub perc: Vec<Hit>,
    pub perc_mode: String,
    #[serde(default)]
    pub tabla: Vec<Hit>,
    #[serde(default)]
    pub bayan: Vec<Hit>,
    pub summary: String,
}

fn hits_of(s: &str) -> Vec<Hit> {
    if s.is_empty() {
        return Vec::new();
    }
    let len = s.chars().count();
    let reps = if len <= 16 { 2 } else { 1 };
    let mut out = Vec::new();
    for r in 0..reps {
        for (i, c) in s.chars().enumerate() {
            let v = match c {
                'X' => 1.0,
                'x' => 0.8,
                'o' => 0.45,
                _ => continue,
            };
            let t = (r * len + i) as f32;
            if t < 32.0 {
                out.push(Hit { t, v });
            }
        }
    }
    out
}

pub fn grid_string(h: &[Hit]) -> String {
    (0..32)
        .map(|i| match h.iter().find(|x| (x.t - i as f32).abs() < 0.01) {
            Some(x) if x.v >= 0.95 => 'X',
            Some(x) if x.v >= 0.6 => 'x',
            Some(_) => 'o',
            None => {
                if h.iter().any(|x| x.t > i as f32 && x.t < i as f32 + 1.0) {
                    '3'
                } else {
                    '.'
                }
            }
        })
        .collect()
}

/// Template frequency per 32-step position (the convention prior).
pub fn template_freq(list: &[String]) -> [f32; 32] {
    let mut f = [0.0f32; 32];
    let n = list.iter().filter(|s| !s.is_empty()).count();
    if n == 0 {
        return f;
    }
    for s in list.iter().filter(|s| !s.is_empty()) {
        for h in hits_of(s) {
            f[h.t as usize] += if h.v < 0.5 { 0.5 } else { 1.0 };
        }
    }
    for x in f.iter_mut() {
        *x /= n as f32;
    }
    f
}

fn euclid(k: usize, n: usize, rot: usize) -> Vec<usize> {
    (0..n).filter(|i| (((i + rot) % n) * k) % n < k).collect()
}

/// Features read from a reference recording (reference-guided method).
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct RefFeatures {
    pub lufs: f32,
    pub low_onsets_per_s: f32,
    pub high_onsets_per_s: f32,
}

pub fn reference_features(path: &str) -> anyhow::Result<RefFeatures> {
    let (l, r) = crate::samples::decode_stereo(std::path::Path::new(path))?;
    let lo = crate::analysis::loudness(&l, &r);
    let mono: Vec<f32> = l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect();
    // band envelopes: one-pole low-pass for the low band, first difference for the highs
    let sr = crate::dsp::SR;
    let hop = (sr * 0.01) as usize;
    let a = (-std::f32::consts::TAU * 150.0 / sr).exp();
    let mut lp = 0.0f32;
    let mut prev = 0.0f32;
    let (mut el, mut eh) = (Vec::new(), Vec::new());
    let (mut al, mut ah) = (0.0f32, 0.0f32);
    for (i, x) in mono.iter().enumerate() {
        lp = a * lp + (1.0 - a) * x;
        let hp = x - prev;
        prev = *x;
        al += lp * lp;
        ah += hp * hp;
        if (i + 1) % hop == 0 {
            el.push(al.sqrt());
            eh.push(ah.sqrt());
            al = 0.0;
            ah = 0.0;
        }
    }
    let onsets = |e: &[f32]| -> f32 {
        if e.len() < 3 {
            return 0.0;
        }
        let mean = e.iter().sum::<f32>() / e.len() as f32;
        let mut c = 0;
        let mut last = 0usize;
        for i in 1..e.len() {
            if e[i] > e[i - 1] * 1.6 && e[i] > mean * 0.8 && i > last + 6 {
                c += 1;
                last = i;
            }
        }
        c as f32 / (e.len() as f32 * 0.01)
    };
    Ok(RefFeatures {
        lufs: lo.integrated_lufs,
        low_onsets_per_s: onsets(&el),
        high_onsets_per_s: onsets(&eh),
    })
}

#[allow(clippy::too_many_arguments)]
pub fn build_groove(
    pb: &Playbook,
    sp: &GenSpec,
    dir: &Direction,
    template: bool,
    bpm: f32,
    refx: Option<&RefFeatures>,
    rng: &mut Rng,
    dec: &mut Vec<Decision>,
) -> Groove {
    let d = &pb.drums;
    let mut g = Groove {
        source: if template {
            "template".into()
        } else {
            "procedural".into()
        },
        ..Default::default()
    };
    // ---- snare first: it sets where the kick must leave room
    let snare_mode = if template {
        "anchor".to_string()
    } else {
        wmap(&sp.snare_modes, rng).unwrap_or_else(|| "anchor".into())
    };
    let tmpl_snare = if d.snare.is_empty() {
        String::new()
    } else {
        d.snare[rng.below(d.snare.len())].clone()
    };
    let mut snare: Vec<Hit> = match snare_mode.as_str() {
        "half_time" => vec![Hit { t: 8.0, v: 1.0 }, Hit { t: 24.0, v: 1.0 }],
        "backbeat" => [4.0, 12.0, 20.0, 28.0]
            .iter()
            .map(|t| Hit { t: *t, v: 1.0 })
            .collect(),
        _ => hits_of(&tmpl_snare)
            .into_iter()
            .filter(|h| h.v > 0.5)
            .collect(),
    };
    if snare.is_empty() {
        snare = vec![Hit { t: 8.0, v: 1.0 }, Hit { t: 24.0, v: 1.0 }];
    }
    let variation = wmap(&sp.snare_variations, rng).unwrap_or_else(|| "none".into());
    match variation.as_str() {
        "displaced" => {
            // the bar-2 snare lands late (or early): the drill/bounce lurch
            if let Some(h) = snare.iter_mut().rev().find(|h| h.t >= 16.0) {
                let s = [1.0, 2.0, -1.0][rng.below(3)];
                h.t = (h.t + s).clamp(16.0, 31.0);
            }
        }
        "pickup" => snare.push(Hit {
            t: 30.0 + rng.below(2) as f32,
            v: 0.6,
        }),
        "double" => {
            if let Some(h) = snare.iter().rev().find(|h| h.t >= 16.0).copied() {
                let t = h.t + 3.0;
                if t < 32.0 {
                    snare.push(Hit { t, v: 0.75 });
                }
            }
        }
        _ => {}
    }
    snare.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap());
    let snare_pos: Vec<f32> = snare.iter().map(|h| h.t).collect();
    g.snare_mode = format!("{snare_mode}+{variation}");
    // ---- kick
    let freq = template_freq(&d.kick);
    let prior16 = [
        1.0, 0.05, 0.2, 0.25, 0.3, 0.05, 0.35, 0.3, 0.25, 0.05, 0.45, 0.2, 0.3, 0.1, 0.3, 0.15,
    ];
    let mut kick: Vec<Hit>;
    if template && !d.kick.is_empty() {
        // two (possibly different) template bars, then one controlled mutation
        let a = &d.kick[rng.below(d.kick.len())];
        let b = &d.kick[rng.below(d.kick.len())];
        kick = hits_of(&a.chars().take(16).collect::<String>())
            .into_iter()
            .filter(|h| h.t < 16.0)
            .collect();
        kick.extend(
            hits_of(&b.chars().take(16).collect::<String>())
                .into_iter()
                .filter(|h| h.t < 16.0)
                .map(|h| Hit {
                    t: h.t + 16.0,
                    v: h.v,
                }),
        );
        if a.len() == 32 {
            kick = hits_of(a);
        }
        if rng.chance(0.6) {
            let movable: Vec<usize> = (0..kick.len())
                .filter(|i| kick[*i].t != 0.0 && kick[*i].t != 16.0)
                .collect();
            if !movable.is_empty() {
                let i = movable[rng.below(movable.len())];
                let s = if rng.chance(0.5) { 1.0 } else { -1.0 };
                let nt = (kick[i].t + s).clamp(1.0, 31.0);
                if !snare_pos.contains(&nt) && !kick.iter().any(|h| h.t == nt) {
                    kick[i].t = nt;
                }
            }
        }
    } else {
        let [lo, hi] = sp.kick_per_bar.unwrap_or([2.0, 4.5]);
        let mut target = lo + (hi - lo) * (0.3 * dir.energy + 0.7 * rng.f32());
        if dir.hero == "groove" || dir.hero == "bass" {
            target += 0.5;
        }
        if dir.density == "sparse" {
            target -= 0.5;
        }
        if let Some(r) = refx {
            let per_bar = r.low_onsets_per_s * 240.0 / bpm;
            if per_bar > 0.5 {
                target = 0.5 * target + 0.5 * per_bar.clamp(1.5, 6.0);
            }
        }
        let n = (target * 2.0).round().clamp(2.0, 12.0) as usize;
        let mut w: Vec<f32> = (0..32)
            .map(|i| {
                let p = 0.65 * freq[i] + 0.35 * prior16[i % 16];
                let near_snare = snare_pos.iter().any(|s| (s - i as f32).abs() < 0.5);
                if i == 0 {
                    0.0
                } else if near_snare {
                    p * 0.12
                } else {
                    p.max(0.02)
                }
            })
            .collect();
        kick = vec![Hit { t: 0.0, v: 1.0 }];
        if rng.chance(0.75) {
            // the second bar usually restates the downbeat
            kick.push(Hit { t: 16.0, v: 1.0 });
            w[16] = 0.0;
        }
        while kick.len() < n {
            let i = rng.weighted(&w);
            if w[i] <= 0.0 {
                break;
            }
            w[i] = 0.0;
            kick.push(Hit {
                t: i as f32,
                v: if i % 4 == 0 { 0.92 } else { 0.82 },
            });
        }
        // spares: the best of what was left
        let mut rest: Vec<(usize, f32)> = w
            .iter()
            .copied()
            .enumerate()
            .filter(|x| x.1 > 0.1)
            .collect();
        rest.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        g.kick_spare = rest.iter().take(4).map(|x| x.0 as f32).collect();
    }
    kick.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap());
    kick.dedup_by(|a, b| a.t == b.t);
    if g.kick_spare.is_empty() {
        g.kick_spare = [10.0, 14.0, 26.0, 30.0]
            .iter()
            .copied()
            .filter(|t| !kick.iter().any(|h| h.t == *t) && !snare_pos.contains(t))
            .collect();
    }
    g.kick = kick;
    g.snare = snare;
    // ---- hats
    if template && !d.hat.is_empty() {
        g.hat = hits_of(&d.hat[rng.below(d.hat.len())]);
        g.hat_rate = "template".into();
        g.hat_accent = "template".into();
    } else {
        let mut rates = sp.hat_rates.clone();
        match dir.density.as_str() {
            "sparse" => {
                for (k, v) in rates.iter_mut() {
                    if k == "8" || k == "4" {
                        *v *= 1.8;
                    }
                }
            }
            "dense" => {
                for (k, v) in rates.iter_mut() {
                    if k.starts_with("16") {
                        *v *= 1.8;
                    }
                }
            }
            _ => {}
        }
        if let Some(r) = refx {
            let per_beat = r.high_onsets_per_s * 60.0 / bpm;
            let want = if per_beat > 3.2 {
                "16"
            } else if per_beat > 2.4 {
                "8t"
            } else if per_beat > 1.4 {
                "8"
            } else {
                "4"
            };
            *rates.entry(want.into()).or_insert(0.0) += 3.0;
        }
        let rate = wmap(&rates, rng).unwrap_or_else(|| "8".into());
        let accents = [
            ("4", 1.0f32),
            ("3+3+2", 0.8),
            ("2", 0.6),
            ("3", 0.5),
            ("3+3+3+3+4", 0.4),
        ];
        let acc = wlist(&accents, rng);
        let groups: Vec<usize> = acc.split('+').filter_map(|x| x.parse().ok()).collect();
        let times: Vec<f32> = match rate.as_str() {
            "4" => (0..8).map(|i| i as f32 * 4.0).collect(),
            "16" => (0..32).map(|i| i as f32).collect(),
            "8t" => (0..24).map(|i| i as f32 * 4.0 / 3.0).collect(),
            "16_gallop" => (0..32).filter(|i| i % 4 != 1).map(|i| i as f32).collect(),
            _ => (0..16).map(|i| i as f32 * 2.0).collect(),
        };
        let mut gi = 0usize;
        let mut next_acc = 0usize;
        for (k, t) in times.iter().enumerate() {
            let accent = k == next_acc;
            if accent {
                next_acc += groups[gi % groups.len()];
                gi += 1;
            }
            if !accent && rng.chance(0.06) {
                continue; // a breath
            }
            g.hat.push(Hit {
                t: *t,
                v: if accent { 0.95 } else { 0.62 + 0.1 * rng.f32() },
            });
        }
        g.hat_rate = rate;
        g.hat_accent = acc;
    }
    let [rl, rh] = sp.hat_rolls.unwrap_or([0.2, 0.5]);
    g.hat_rolls = if template {
        d.hat_rolls
    } else {
        rl + (rh - rl) * (0.4 * dir.energy + 0.6 * rng.f32())
    };
    let [gl, gh] = sp.ghost.unwrap_or([0.1, 0.3]);
    g.ghosts = if template {
        d.ghost_snare
    } else {
        gl + (gh - gl) * rng.f32()
    };
    // ---- open hat: a couple of off-beat breaths
    if template {
        if !d.open_hat.is_empty() {
            g.open_hat = hits_of(&d.open_hat[rng.below(d.open_hat.len())]);
        }
    } else {
        let cands = [
            (14.0f32, 1.5f32),
            (30.0, 1.5),
            (6.0, 0.8),
            (22.0, 0.8),
            (10.0, 0.5),
            (26.0, 0.5),
            (3.0, 0.3),
            (19.0, 0.3),
            (11.0, 0.3),
        ];
        let k = rng.below(3);
        let mut w: Vec<f32> = cands.iter().map(|x| x.1).collect();
        for _ in 0..k {
            let i = rng.weighted(&w);
            if w[i] <= 0.0 {
                break;
            }
            w[i] = 0.0;
            g.open_hat.push(Hit {
                t: cands[i].0,
                v: 0.75,
            });
        }
    }
    for o in &g.open_hat {
        g.hat.retain(|h| (h.t - o.t).abs() >= 0.5);
    }
    // ---- perc: Euclidean, polyrhythmic or sparse
    let pm = if template {
        "template".to_string()
    } else {
        wmap(&sp.perc_modes, rng).unwrap_or_else(|| "euclid".into())
    };
    match pm.as_str() {
        "euclid" => {
            let k = 3 + rng.below(5);
            let rot = rng.below(16);
            for bar in 0..2 {
                for s in euclid(k, 16, rot) {
                    g.perc.push(Hit {
                        t: (bar * 16 + s) as f32,
                        v: if s == euclid(k, 16, rot)[0] {
                            0.8
                        } else {
                            0.55
                        },
                    });
                }
            }
            g.perc_mode = format!("euclid E({k},16)>>{rot}");
        }
        "poly" => {
            let l = [3usize, 3, 5, 6, 7][rng.below(5)];
            let off = rng.below(l);
            let mut t = off;
            while t < 32 {
                g.perc.push(Hit {
                    t: t as f32,
                    v: 0.6,
                });
                t += l;
            }
            g.perc_mode = format!("poly every {l} steps (+{off}) against the 4/4");
        }
        "sparse" => {
            for _ in 0..(2 + rng.below(3)) {
                let t = [3.0, 7.0, 11.0, 13.0, 19.0, 23.0, 27.0, 29.0][rng.below(8)];
                if !g.perc.iter().any(|h| h.t == t) {
                    g.perc.push(Hit { t, v: 0.6 });
                }
            }
            g.perc_mode = "sparse accents".into();
        }
        _ => {
            if !d.perc.is_empty() {
                g.perc = hits_of(&d.perc[rng.below(d.perc.len())]);
            }
            g.perc_mode = "template".into();
        }
    }
    g.perc.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap());
    // ---- tabla/bayan: the theka stays a theka, bar 2 answers bar 1
    for (src, dst) in [(&d.tabla, 0), (&d.bayan, 1)] {
        if src.is_empty() {
            continue;
        }
        let a = &src[rng.below(src.len())];
        let b = &src[rng.below(src.len())];
        let mut h: Vec<Hit> = hits_of(a).into_iter().filter(|x| x.t < 16.0).collect();
        h.extend(hits_of(b).into_iter().filter(|x| x.t < 16.0).map(|x| Hit {
            t: x.t + 16.0,
            v: x.v,
        }));
        if !template {
            h.retain(|x| x.t == 0.0 || x.t == 16.0 || !rng.chance(0.12));
        }
        if dst == 0 {
            g.tabla = h;
        } else {
            g.bayan = h;
        }
    }
    g.summary = format!(
        "{}: kick {} ({} hits/2 bars) | snare {} {} | hats {} rate, accents {}, rolls {:.2} | perc {} | ghosts {:.2}",
        g.source,
        grid_string(&g.kick),
        g.kick.len(),
        g.snare_mode,
        grid_string(&g.snare),
        g.hat_rate,
        g.hat_accent,
        g.hat_rolls,
        g.perc_mode,
        g.ghosts
    );
    decide(dec, "core", "drum groove", &g.summary, format!("{} groove for a {} beat ({}-led): kick density from energy {:.2}, snare placement '{}', hat rate '{}'", g.source, dir.density, dir.hero, dir.energy, g.snare_mode, g.hat_rate));
    g
}

// ---------------------------------------------------------------- section drums

fn notes_from(h: &[Hit], bar: u32, core_bar: u32, vel: f32) -> Vec<Note> {
    let lo = core_bar as f32 * 16.0;
    h.iter()
        .filter(|x| x.t >= lo && x.t < lo + 16.0)
        .map(|x| {
            Note::new(
                bar as f32 * 16.0 + x.t - lo,
                1.0,
                60,
                (x.v * vel).clamp(0.05, 1.0),
            )
        })
        .collect()
}

/// Drums for one section from the groove: role treatment (verse thins,
/// hook adds, bridge strips or halves), per-phrase mutation, rolls and rate
/// changes, ghosts, tags (half_time, dropout, sparse_to_dense) and the
/// generated fill into the next section.
pub fn section_drums(
    g: &Groove,
    sec: &PlanSection,
    seed: u64,
    idx: usize,
) -> BTreeMap<String, Vec<Note>> {
    let mut rng = Rng::new(seed ^ 0xD5D5_0000 ^ (idx as u64).wrapping_mul(0x9E37_79B9));
    let bars = sec.bars;
    let end = (bars * 16) as f32;
    let kind = sec.kind.as_str();
    let tag = |t: &str| sec.tags.iter().any(|x| x == t);
    let half = tag("half_time");
    let mut out: BTreeMap<String, Vec<Note>> = BTreeMap::new();
    let (thin, add, roll_scale) = match kind {
        "hook" => (0.0, 0.18, 1.0),
        "verse" => (0.22, 0.0, 0.6),
        "intro" | "outro" => (0.5, 0.0, 0.3),
        _ => (0.4, 0.0, 0.2),
    };
    // bridge/breakdown treatment drawn once per section
    let strip = if matches!(kind, "bridge" | "breakdown") {
        ["half", "no_kick", "perc_only"][rng.below(3)]
    } else {
        ""
    };
    let half = half || strip == "half";
    let hat_up =
        kind == "hook" && g.hat_rate != "16" && g.hat_rate != "template" && rng.chance(0.4);
    let mut kick = Vec::new();
    let mut snare = Vec::new();
    let mut hat = Vec::new();
    let mut oh = Vec::new();
    let mut perc = Vec::new();
    let mut tabla = Vec::new();
    let mut bayan = Vec::new();
    for bar in 0..bars {
        let cb = bar % 2;
        let base = bar as f32 * 16.0;
        // sparse_to_dense: hats, then kick, then everything
        let stage = if tag("sparse_to_dense") {
            let q = (bar as f32 / bars.max(1) as f32 * 3.0) as u32;
            q.min(2)
        } else {
            2
        };
        let dropped = tag("dropout") && bars >= 6 && bar >= bars / 2 - 1 && bar < bars / 2 + 1;
        if dropped {
            continue;
        }
        // kick
        if stage >= 1 && strip != "no_kick" && strip != "perc_only" {
            let mut k = notes_from(&g.kick, bar, cb, 1.0);
            if thin > 0.0 {
                k.retain(|n| (n.start - base) % 16.0 == 0.0 || !rng.chance(thin));
            }
            if half {
                k.retain(|n| (n.start - base) % 8.0 == 0.0 || (n.start - base) < 4.0);
            }
            if add > 0.0 && rng.chance(add) && !g.kick_spare.is_empty() {
                let s = g.kick_spare[rng.below(g.kick_spare.len())] % 16.0;
                k.push(Note::new(base + s, 1.0, 60, 0.8));
            }
            // phrase-end mutation: displace a kick or add a pickup
            if bar % 4 == 3 && !k.is_empty() {
                if rng.chance(0.5) && k.len() > 1 {
                    let i = 1 + rng.below(k.len() - 1);
                    k[i].start = (k[i].start + if rng.chance(0.5) { 1.0 } else { -1.0 })
                        .clamp(base + 1.0, base + 15.0);
                } else if rng.chance(0.4) {
                    k.push(Note::new(base + 14.0 + rng.below(2) as f32, 1.0, 60, 0.75));
                }
            }
            kick.extend(k);
        }
        // snare / clap
        if stage >= 2 && strip != "perc_only" {
            let mut s = notes_from(&g.snare, bar, cb, 1.0);
            if half {
                // one hit per bar on beat 3 (per two bars when the core is already half-time)
                let already_half = g.snare.len() <= 3;
                s.clear();
                if !already_half || bar % 2 == 0 {
                    s.push(Note::new(base + 8.0, 1.0, 60, 1.0));
                }
            }
            // ghosts around the backbeat
            if g.ghosts > 0.0 && !half {
                let taken: Vec<f32> = s.iter().map(|n| n.start).collect();
                for st in [3.0f32, 6.0, 7.0, 10.0, 14.0, 15.0] {
                    let t = base + st;
                    if !taken.iter().any(|x| (x - t).abs() <= 1.0) && rng.chance(g.ghosts * 0.5) {
                        s.push(Note::new(t, 1.0, 60, 0.2 + rng.f32() * 0.12));
                    }
                }
            }
            snare.extend(s);
        }
        // hats (with rate changes and rolls)
        let mut h = notes_from(&g.hat, bar, cb, 1.0);
        if half || stage == 0 {
            let mut keep = Vec::new();
            for (i, n) in h.iter().enumerate() {
                if i % 2 == 0 {
                    keep.push(n.clone());
                }
            }
            h = keep;
        }
        if hat_up && !half {
            // fill the gaps with quiet 16ths: the hook moves faster
            let have: Vec<f32> = h.iter().map(|n| n.start).collect();
            for st in 0..16 {
                let t = base + st as f32;
                if !have.iter().any(|x| (x - t).abs() < 0.4) {
                    h.push(Note::new(t, 1.0, 60, 0.42));
                }
            }
        }
        if kind == "hook" && rng.chance(0.15) && !half {
            // rate change: beats 3-4 in triplets
            h.retain(|n| n.start < base + 8.0);
            for k in 0..6 {
                h.push(Note::new(
                    base + 8.0 + k as f32 * 4.0 / 3.0,
                    1.0,
                    60,
                    if k % 3 == 0 { 0.85 } else { 0.6 },
                ));
            }
        }
        if stage >= 1 && rng.chance(g.hat_rolls * roll_scale * (0.5 + 0.5 * sec.energy)) {
            let beat = if rng.chance(0.55) {
                3.0
            } else {
                [1.0, 2.0, 3.0][rng.below(3)]
            };
            let kind_r = ["32", "16t", "32t", "64"][rng.weighted(&[1.0, 0.9, 0.5, 0.35])];
            let (step, count, span) = match kind_r {
                "16t" => (4.0 / 6.0, 6, 4.0),
                "32t" => (1.0 / 3.0, 6, 2.0),
                "64" => (0.25, 8, 2.0),
                _ => (0.5, 8, 4.0),
            };
            let t0 = base + beat * 4.0 + if span < 4.0 { 2.0 } else { 0.0 };
            h.retain(|n| n.start < t0 || n.start >= t0 + span);
            let dirn = rng.below(3);
            for k in 0..count {
                let p = match dirn {
                    0 => 60 + (k / 2) as u8,
                    1 => 64u8.saturating_sub((k / 2) as u8),
                    _ => 60,
                };
                h.push(Note::new(
                    t0 + k as f32 * step,
                    step,
                    p,
                    0.42 + 0.45 * k as f32 / count as f32,
                ));
            }
        }
        hat.extend(h);
        if stage >= 2 && !half {
            oh.extend(notes_from(&g.open_hat, bar, cb, 0.85));
        }
        if stage >= 2 || strip == "perc_only" {
            perc.extend(notes_from(&g.perc, bar, cb, 0.85));
        }
        tabla.extend(notes_from(&g.tabla, bar, cb, 1.0));
        bayan.extend(notes_from(&g.bayan, bar, cb, 1.0));
    }
    if kind == "hook" && !half {
        oh.push(Note::new(0.0, 2.0, 60, 0.9));
    }
    // ---- the fill into the next section
    let cut = |v: &mut Vec<Note>, from: f32| v.retain(|n| n.start < from);
    match sec.transition.as_str() {
        "drop_and_roll" => {
            cut(&mut kick, end - 8.0);
            cut(&mut hat, end - 4.0);
            for k in 0..8 {
                hat.push(Note::new(
                    end - 4.0 + k as f32 * 0.5,
                    0.5,
                    60 + k / 2,
                    0.4 + 0.07 * k as f32,
                ));
            }
        }
        "snare_fill" => {
            cut(&mut snare, end - 4.0);
            for k in 0..4 {
                snare.push(Note::new(
                    end - 4.0 + k as f32,
                    1.0,
                    60,
                    0.55 + 0.12 * k as f32,
                ));
            }
            kick.retain(|n| n.start < end - 4.0 || (n.start - (end - 4.0)).abs() < 0.01);
        }
        "triplet_fill" => {
            cut(&mut snare, end - 8.0);
            cut(&mut hat, end - 8.0);
            kick.retain(|n| n.start < end - 8.0 || (n.start - (end - 8.0)).abs() < 0.01);
            for k in 0..6 {
                snare.push(Note::new(
                    end - 8.0 + k as f32 * 4.0 / 3.0,
                    1.0,
                    60,
                    0.45 + 0.09 * k as f32,
                ));
            }
        }
        "stutter" => {
            cut(&mut snare, end - 4.0);
            cut(&mut hat, end - 4.0);
            for k in 0..8 {
                snare.push(Note::new(
                    end - 4.0 + k as f32 * 0.5,
                    0.5,
                    60,
                    0.3 + 0.07 * k as f32,
                ));
            }
        }
        "kick_drop" => cut(&mut kick, end - 16.0),
        "silence" => {
            for v in [
                &mut kick, &mut snare, &mut hat, &mut oh, &mut perc, &mut tabla, &mut bayan,
            ] {
                cut(v, end - 4.0);
            }
        }
        "turnaround" => {
            if !snare.iter().any(|n| (n.start - (end - 1.0)).abs() < 0.5) && rng.chance(0.6) {
                snare.push(Note::new(end - 1.0, 1.0, 60, 0.55));
            }
        }
        "fade_down" => {
            cut(&mut snare, end - 8.0);
            for n in hat.iter_mut().filter(|n| n.start >= end - 16.0) {
                n.vel *= 0.7;
            }
        }
        _ => {}
    }
    // tihai for the tabla at section ends
    if !tabla.is_empty() && sec.transition != "end" && bars >= 4 {
        let p = tabla.first().map(|n| n.pitch).unwrap_or(60);
        cut(&mut tabla, end - 9.0);
        for rep in 0..3 {
            let t = end - 9.0 + rep as f32 * 3.0;
            tabla.push(Note::new(t, 1.0, p, 0.95));
            tabla.push(Note::new(t + 1.0, 1.0, p, 0.4));
            tabla.push(Note::new(t + 2.0, 1.0, p, 0.7));
        }
    }
    for (k, mut v) in [
        ("kick", kick),
        ("snare", snare),
        ("hat", hat),
        ("open_hat", oh),
        ("perc", perc),
        ("tabla", tabla),
        ("bayan", bayan),
    ] {
        v.retain(|n| n.start >= 0.0 && n.start < end);
        v.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
        out.insert(k.to_string(), v);
    }
    out
}

// ---------------------------------------------------------------- arrangement

fn form_kinds(name: &str) -> &'static [&'static str] {
    match name {
        "hook_first" => &["intro", "hook", "verse", "hook", "verse", "hook", "outro"],
        "with_bridge" => &[
            "intro", "verse", "hook", "verse", "hook", "bridge", "hook", "outro",
        ],
        "build" => &["intro", "verse", "hook", "breakdown", "hook", "outro"],
        "short" => &["intro", "hook", "verse", "hook", "outro"],
        _ => &["intro", "verse", "hook", "verse", "hook", "outro"],
    }
}

#[allow(clippy::too_many_arguments)]
pub fn generate_arrangement(
    pb: &Playbook,
    sp: &GenSpec,
    dir: &Direction,
    palette: &BTreeMap<String, String>,
    bpm: f32,
    template: bool,
    rng: &mut Rng,
    dec: &mut Vec<Decision>,
) -> Vec<SectionTemplate> {
    let mut forms = sp.forms.clone();
    if template {
        forms = m(&[("template", 1.0)]);
    } else {
        match dir.mood.as_str() {
            "hype" => *forms.entry("hook_first".into()).or_insert(0.0) += 1.0,
            "sad" | "chill" | "devotional" => *forms.entry("build".into()).or_insert(0.0) += 0.8,
            _ => {}
        }
        if dir.hero == "texture" {
            *forms.entry("build".into()).or_insert(0.0) += 0.6;
        }
    }
    let form = wmap(&forms, rng).unwrap_or_else(|| "template".into());
    let mut out: Vec<SectionTemplate> = if form == "template" {
        pb.arrangement.clone()
    } else {
        let has = |r: &str| palette.contains_key(r);
        let drums: Vec<&str> = ["kick", "snare", "hat", "open_hat", "perc", "tabla", "bayan"]
            .into_iter()
            .filter(|r| has(r))
            .collect();
        form_kinds(&form)
            .iter()
            .map(|k| {
                let (bars, energy) = match *k {
                    "intro" => ([4u32, 4, 2, 8][rng.below(4)], rng.range(0.22, 0.45)),
                    "verse" => ([8u32, 8, 16, 12][rng.below(4)], rng.range(0.55, 0.75)),
                    "hook" => ([8u32, 8, 8, 16][rng.below(4)], rng.range(0.85, 1.0)),
                    "bridge" | "breakdown" => ([4u32, 8][rng.below(2)], rng.range(0.35, 0.55)),
                    _ => ([4u32, 4, 2, 8][rng.below(4)], rng.range(0.25, 0.4)),
                };
                let mut layers: Vec<String> = Vec::new();
                let mut add = |l: &str| {
                    if has(l) && !layers.iter().any(|x| x == l) {
                        layers.push(l.to_string());
                    }
                };
                match *k {
                    "hook" => {
                        for d in drums.iter().copied() {
                            add(d);
                        }
                        for l in ["bass", "harmony", "lead", "counter", "texture"] {
                            add(l);
                        }
                    }
                    "verse" => {
                        for d in ["kick", "snare", "hat", "tabla", "bayan"] {
                            add(d);
                        }
                        add("bass");
                        add("harmony");
                        if rng.chance(0.6) {
                            add("perc");
                        }
                        if dir.hero == "motif" || rng.chance(0.3) {
                            add("lead");
                        }
                        if rng.chance(0.4) {
                            add("texture");
                        }
                    }
                    "intro" => {
                        add("harmony");
                        match rng.below(3) {
                            0 => add("lead"),
                            1 => add("hat"),
                            _ => {
                                add("texture");
                                add("perc");
                            }
                        }
                        if dir.hero == "motif" {
                            add("lead");
                        }
                    }
                    "bridge" | "breakdown" => {
                        add("harmony");
                        add("bass");
                        if rng.chance(0.7) {
                            add("lead");
                        }
                        if rng.chance(0.5) {
                            add(["hat", "perc", "tabla"][rng.below(3)]);
                        }
                        add("texture");
                    }
                    _ => {
                        add("harmony");
                        if rng.chance(0.6) {
                            add("lead");
                        }
                        if rng.chance(0.4) {
                            add("hat");
                        }
                    }
                }
                if layers.is_empty() {
                    layers.push("harmony".into());
                }
                SectionTemplate {
                    kind: k.to_string(),
                    bars,
                    energy,
                    layers,
                }
            })
            .collect()
    };
    // keep the song under ~150 s so a beat renders in time
    let bar_s = 240.0 / bpm;
    while out.iter().map(|s| s.bars as f32 * bar_s).sum::<f32>() > 150.0 {
        if let Some(s) = out.iter_mut().rev().find(|s| s.bars >= 12) {
            s.bars = 8;
            continue;
        }
        let hooks = out.iter().filter(|s| s.kind == "hook").count();
        let Some(i) = (1..out.len().saturating_sub(1))
            .rev()
            .find(|i| out[*i].kind != "hook" || hooks > 2)
        else {
            break;
        };
        if out.len() <= 4 {
            break;
        }
        out.remove(i);
    }
    decide(
        dec,
        "core",
        "arrangement form",
        format!(
            "{form}: {}",
            out.iter()
                .map(|s| format!("{}({})", s.kind, s.bars))
                .collect::<Vec<_>>()
                .join(" > ")
        ),
        if form == "template" {
            "the genre's own arrangement, a known-good starting point".to_string()
        } else {
            format!(
                "form drawn for '{}' ({} energy {:.2}); bar counts and layers drawn per section",
                dir.mood, dir.density, dir.energy
            )
        },
    );
    out
}

/// Transition into the next section, drawn by the energy step.
pub fn draw_transition(cur: f32, next: Option<f32>, rolls: f32, rng: &mut Rng) -> String {
    match next {
        None => "end".into(),
        Some(n) if n > cur + 0.15 => wlist(
            &[
                ("drop_and_roll", if rolls > 0.3 { 2.0 } else { 0.6 }),
                ("snare_fill", 1.0),
                ("stutter", 0.6),
                ("triplet_fill", 0.6),
                ("kick_drop", 0.7),
            ],
            rng,
        ),
        Some(n) if n + 0.3 < cur => wlist(
            &[("fade_down", 2.0), ("turnaround", 0.5), ("kick_drop", 0.5)],
            rng,
        ),
        Some(_) => wlist(
            &[("turnaround", 2.0), ("stutter", 0.4), ("snare_fill", 0.4)],
            rng,
        ),
    }
}

/// Motif development per section: the same idea, transformed by role.
pub fn development_for(
    kind: &str,
    nth: usize,
    last_hook: bool,
    rng: &mut Rng,
) -> (Vec<String>, &'static str) {
    let pick = |opts: &[(&str, &'static str)], rng: &mut Rng| -> (Vec<String>, &'static str) {
        let o = opts[rng.below(opts.len())];
        (o.0.split('+').map(String::from).collect(), o.1)
    };
    match kind {
        "hook" if nth == 1 => pick(&[("statement+call_response", "first hook states the motif plainly, answered in the next bar")], rng),
        "hook" if last_hook => pick(
            &[
                ("call_response", "last hook: the motif an octave up, answered"),
                ("call_response+displace", "last hook: lifted and pushed off the beat for urgency"),
            ],
            rng,
        ),
        "hook" => pick(
            &[
                ("call_response+displace", "the motif displaced against the beat: familiar but restless"),
                ("call_response+sequence", "the motif sequenced up a third: same shape, new height"),
                ("call_response+register_down", "the motif an octave down: a darker restatement"),
            ],
            rng,
        ),
        "verse" => pick(
            &[
                ("fragment", "verse quotes a fragment of the motif and leaves room for the vocal"),
                ("fragment+displace", "a displaced fragment: the hook's DNA, hidden"),
                ("register_down+fragment", "a low fragment under the vocal"),
            ],
            rng,
        ),
        "bridge" | "breakdown" => pick(
            &[
                ("inversion", "bridge turns the motif upside down (augmented): contrast from the same material"),
                ("retrograde", "bridge plays the motif backwards: contrast that still belongs"),
                ("inversion+fragment", "an inverted fragment: tension before the last hook"),
            ],
            rng,
        ),
        "intro" => pick(
            &[
                ("fragment", "intro teases the motif's opening"),
                ("statement", "intro states the motif bare so the hook feels earned"),
            ],
            rng,
        ),
        _ => pick(
            &[
                ("fragment+register_down", "outro: the motif dissolving downwards"),
                ("statement", "outro: a callback to the motif"),
            ],
            rng,
        ),
    }
}

// ---------------------------------------------------------------- wildcards

pub const WILDCARDS: &[(&str, &str)] = &[
    ("half_time_hook", "contrast"),
    ("silence_before_drop", "tension"),
    ("beat_switch", "contrast"),
    ("odd_phrase", "tension"),
    ("drum_dropout", "release"),
    ("unusual_instrument", "emotion"),
    ("key_change", "emotion"),
    ("bass_kick_call_response", "groove"),
    ("sparse_to_dense", "tension"),
    ("quiet_section", "release"),
];

pub const UNUSUAL: &[&str] = &[
    "koto",
    "marimba",
    "santoor",
    "sitar",
    "brass_stab",
    "choir_aah",
    "wt_growl",
    "granular_shimmer",
    "cello_section",
    "organ",
    "guitar_pluck",
    "fm_bell",
    "bansuri",
    "staccato_strings",
    "chip_lead",
];

/// Everything a wildcard may change.
pub struct WildTargets<'a> {
    pub sections: &'a mut Vec<PlanSection>,
    pub palette: &'a mut BTreeMap<String, String>,
    pub transpose: &'a mut BTreeMap<String, i32>,
    pub switch_groove: &'a mut bool,
    pub genre_presets: Vec<String>,
    pub weights: BTreeMap<String, f32>,
}

/// Pick 1-3 wildcards (weighted toward roles that serve the direction),
/// apply them and say what each is for.
pub fn apply_wildcards(
    t: WildTargets,
    dir: &Direction,
    forced: &[String],
    skipped: &mut Vec<String>,
    rng: &mut Rng,
    dec: &mut Vec<Decision>,
) -> Vec<Wildcard> {
    let mut n = 1 + rng.weighted(&[0.35, 0.45, 0.2]);
    // contrasts the intent asked for come first and replace the random draw
    let forced_idx: Vec<usize> = forced
        .iter()
        .filter_map(|f| WILDCARDS.iter().position(|w| w.0 == f))
        .collect();
    if !forced_idx.is_empty() {
        n = forced_idx.len();
    }
    let mut fi = 0;
    let mut w: Vec<f32> = WILDCARDS
        .iter()
        .map(|(name, role)| {
            // base weights: the more drastic or the more generic a move, the rarer
            let mut x = match *name {
                "sparse_to_dense" => 0.45,
                "drum_dropout" => 0.6,
                "quiet_section" => 0.5,
                "silence_before_drop" => 0.7,
                "odd_phrase" => 0.8,
                _ => 1.0f32,
            };
            match (*role, dir.mood.as_str()) {
                ("tension", "dark" | "hype") => x *= 1.3,
                ("emotion", "sad" | "hopeful" | "smooth") => x *= 1.6,
                ("release", "chill" | "devotional") => x *= 1.5,
                _ => {}
            }
            if *role == "groove" && dir.hero == "groove" {
                x *= 2.0;
            }
            if *role == "groove" && dir.hero == "bass" {
                x *= 1.6;
            }
            if let Some(m) = t.weights.get(*name) {
                x *= m.max(0.0);
            }
            x
        })
        .collect();
    let mut out = Vec::new();
    let mut tries = 0;
    while out.len() < n && tries < 20 + forced_idx.len() {
        tries += 1;
        let (i, is_forced) = if fi < forced_idx.len() {
            fi += 1;
            (forced_idx[fi - 1], true)
        } else if !forced_idx.is_empty() {
            break;
        } else {
            let i = rng.weighted(&w);
            if w[i] <= 0.0 {
                break;
            }
            (i, false)
        };
        w[i] = 0.0;
        let (name, role) = WILDCARDS[i];
        let hooks: Vec<usize> = (0..t.sections.len())
            .filter(|j| t.sections[*j].kind == "hook")
            .collect();
        let verses: Vec<usize> = (0..t.sections.len())
            .filter(|j| t.sections[*j].kind == "verse")
            .collect();
        let applied: Option<(String, String)> = match name {
            "half_time_hook" if hooks.len() >= 2 => {
                let j = hooks[1 + rng.below(hooks.len() - 1)];
                t.sections[j].tags.push("half_time".into());
                Some((
                    t.sections[j].name.clone(),
                    "the groove halves under a familiar hook: same melody, new weight".into(),
                ))
            }
            "silence_before_drop" if !hooks.is_empty() && hooks[hooks.len() - 1] > 0 => {
                let j = hooks[if rng.chance(0.5) { 0 } else { hooks.len() - 1 }];
                if j == 0 {
                    None
                } else {
                    t.sections[j - 1].transition = "silence".into();
                    Some((
                        t.sections[j - 1].name.clone(),
                        format!(
                            "a beat of silence before {} so the drop lands harder",
                            t.sections[j].name
                        ),
                    ))
                }
            }
            "beat_switch" if t.sections.len() >= 5 => {
                *t.switch_groove = true;
                // after a quiet section if there is one (the switch slams in out of the calm)
                let from = t
                    .sections
                    .iter()
                    .position(|s| s.tags.iter().any(|x| x == "quiet"))
                    .map(|q| q + 1)
                    .unwrap_or(t.sections.len() / 2);
                if from > 0 && from < t.sections.len() && t.sections[from - 1].transition != "silence" {
                    t.sections[from - 1].transition = "silence".into();
                }
                let mut names = Vec::new();
                for s in t.sections.iter_mut().skip(from) {
                    if s.kind != "outro" && !s.tags.iter().any(|x| x == "quiet") {
                        s.tags.push("switch".into());
                        names.push(s.name.clone());
                    }
                }
                Some((names.join(","), "the drums switch to a second groove halfway: the back half feels like a new chapter".into()))
            }
            "odd_phrase" => {
                let cands: Vec<usize> = (0..t.sections.len())
                    .filter(|j| {
                        matches!(
                            t.sections[*j].kind.as_str(),
                            "verse" | "bridge" | "breakdown"
                        ) && t.sections[*j].bars >= 4
                    })
                    .collect();
                if cands.is_empty() {
                    None
                } else {
                    let j = cands[rng.below(cands.len())];
                    let nb = if t.sections[j].bars >= 8 {
                        t.sections[j].bars - 1
                    } else {
                        t.sections[j].bars + 1
                    };
                    t.sections[j].bars = nb;
                    Some((t.sections[j].name.clone(), format!("{nb}-bar phrase: the next section arrives a bar early/late, keeping the listener off balance")))
                }
            }
            "drum_dropout" => {
                let c: Vec<usize> = verses
                    .iter()
                    .copied()
                    .filter(|j| t.sections[*j].bars >= 6)
                    .collect();
                if c.is_empty() {
                    None
                } else {
                    let j = c[rng.below(c.len())];
                    t.sections[j].tags.push("dropout".into());
                    Some((t.sections[j].name.clone(), "drums drop out for two bars mid-verse: space for the vocal, then relief when they return".into()))
                }
            }
            "quiet_section" if t.sections.len() >= 3 => {
                // a real quiet section: its own 4 bars, drums and 808 out, the
                // harmony and a sparse lead under a filter; then everything returns
                let at = t
                    .sections
                    .iter()
                    .position(|s| s.tags.iter().any(|x| x == "switch"))
                    .unwrap_or(t.sections.len() / 2)
                    .max(1)
                    .min(t.sections.len() - 1);
                let mut layers: Vec<String> = ["harmony", "lead", "texture"]
                    .iter()
                    .filter(|r| t.palette.contains_key(**r))
                    .map(|r| r.to_string())
                    .collect();
                if layers.is_empty() {
                    layers.push("harmony".into());
                }
                // room to breathe at the start too: a 4-bar intro, a shorter outro
                if t.sections[0].kind == "intro" && t.sections[0].bars < 4 {
                    t.sections[0].bars = 4;
                    if let Some(o) = t.sections.iter_mut().rev().find(|s| s.kind == "outro") {
                        if o.bars >= 4 {
                            o.bars -= 2;
                        }
                    }
                }
                t.sections.insert(
                    at,
                    crate::producer::PlanSection {
                        name: "quiet1".into(),
                        kind: "breakdown".into(),
                        bars: 4,
                        energy: 0.22,
                        layers,
                        lead: "sparse".into(),
                        transition: "silence".into(),
                        development: vec!["fragment".into()],
                        tags: vec!["quiet".into()],
                    },
                );
                Some((
                    "quiet1".into(),
                    "a 4-bar quiet section: drums and 808 out, only the chords and a sparse lead; a beat of silence, then the beat comes back".into(),
                ))
            }
            "unusual_instrument" => {
                let pool: Vec<&str> = UNUSUAL
                    .iter()
                    .copied()
                    .filter(|p| {
                        !t.genre_presets.iter().any(|g| g == p)
                            && Some(&p.to_string()) != t.palette.get("lead")
                    })
                    .collect();
                if pool.is_empty() {
                    None
                } else {
                    let p = pool[rng.below(pool.len())];
                    let role = if t.palette.contains_key("counter") {
                        "counter"
                    } else {
                        "texture"
                    };
                    t.palette.insert(role.into(), p.into());
                    for s in t.sections.iter_mut().filter(|s| s.kind == "hook") {
                        if !s.layers.iter().any(|l| l == role) {
                            s.layers.push(role.into());
                        }
                    }
                    Some((format!("{role}={p}"), format!("{p} is outside the genre's usual palette: a colour that gives '{}' its own identity", dir.intent)))
                }
            }
            "key_change" if hooks.len() >= 2 => {
                let j = hooks[hooks.len() - 1];
                let st = if rng.chance(0.6) { 1 } else { 2 };
                t.transpose.insert(t.sections[j].name.clone(), st);
                Some((t.sections[j].name.clone(), format!("the last hook steps up {st} semitone(s): an emotional lift for the final statement")))
            }
            "bass_kick_call_response" if !verses.is_empty() => {
                let j = verses[rng.below(verses.len())];
                t.sections[j].tags.push("bass_call_response".into());
                Some((t.sections[j].name.clone(), "the 808 answers in the kick's gaps instead of doubling it: a conversation in the low end".into()))
            }
            "sparse_to_dense" if !verses.is_empty() => {
                let j = verses[0];
                if t.sections[j].bars < 6 {
                    None
                } else {
                    t.sections[j].tags.push("sparse_to_dense".into());
                    Some((
                        t.sections[j].name.clone(),
                        "the verse builds: hats alone, then the kick, then the full kit".into(),
                    ))
                }
            }
            _ => None,
        };
        if let Some((target, why)) = applied {
            decide(dec, "wildcard", name, &target, format!("[{role}] {why}"));
            out.push(Wildcard {
                name: name.into(),
                role: role.into(),
                target,
                why,
            });
        } else if is_forced {
            skipped.push(name.to_string());
        }
    }
    out
}

// ---------------------------------------------------------------- structured intent

/// What was asked for and what happened to it (applied / adjusted / ignored).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Constraint {
    pub field: String,
    pub requested: serde_json::Value,
    pub status: String,
    pub note: String,
}

pub fn constraint(
    v: &mut Vec<Constraint>,
    field: &str,
    requested: serde_json::Value,
    status: &str,
    note: impl Into<String>,
) {
    v.push(Constraint {
        field: field.into(),
        requested,
        status: status.into(),
        note: note.into(),
    });
}

/// A structured creative intent, filled in by the connected model instead of
/// relying on keyword matching of the brief.
#[derive(Clone, Debug, Default)]
pub struct Intent {
    pub mood: Option<String>,
    pub energy: Option<f32>,
    pub emotion: Option<String>,
    pub hero: Option<String>,
    pub density: Option<String>,
    pub rhythmic_feel: Vec<String>,
    pub motif_contour: Option<String>,
    pub motif_rhythm: Option<String>,
    pub motif_density: Option<f32>,
    pub palette: BTreeMap<String, String>,
    pub contrasts: Vec<String>,
}

impl Intent {
    /// Constrains the musical material (rhythm or motif), which the
    /// template method cannot honour.
    pub fn shapes_material(&self) -> bool {
        !self.rhythmic_feel.is_empty()
            || self.motif_contour.is_some()
            || self.motif_rhythm.is_some()
    }
}

pub const FEELS: &[&str] = &[
    "half_time",
    "backbeat",
    "triplet",
    "swing",
    "straight",
    "bounce",
    "driving",
    "rolling",
    "sparse_hats",
];
pub const CONTOURS: &[&str] = &[
    "arch",
    "descending",
    "ascending",
    "wave",
    "static",
    "leap_fall",
];
pub const CELLS: &[&str] = &["on_grid", "syncopated", "long_short", "triplet"];
pub const ROLES: &[&str] = &[
    "lead", "harmony", "counter", "texture", "bass", "kick", "snare", "hat", "open_hat", "perc",
    "tabla", "bayan",
];

pub fn contrast_alias(s: &str) -> Option<&'static str> {
    let n = s.trim().to_lowercase().replace([' ', '-'], "_");
    Some(match n.as_str() {
        "half_time_hook" | "half_time" | "halftime" | "half_time_breakdown" => "half_time_hook",
        "silence_before_drop" | "drop_silence" | "silence" | "pause_before_drop" => {
            "silence_before_drop"
        }
        "beat_switch" | "switch" | "groove_switch" => "beat_switch",
        "odd_phrase" | "odd_length" | "odd_bars" => "odd_phrase",
        "drum_dropout" | "dropout" | "drop_out" => "drum_dropout",
        "quiet_section" | "quiet" | "quiet_part" | "breakdown" | "calm_section" | "stripped_section" => {
            "quiet_section"
        }
        "unusual_instrument" | "unexpected_instrument" => "unusual_instrument",
        "key_change" | "modulation" | "key_lift" => "key_change",
        "bass_kick_call_response"
        | "call_response"
        | "808_call_response"
        | "kick_808_call_response" => "bass_kick_call_response",
        "sparse_to_dense" | "build" | "build_up" | "sparse_verse_dense_hook" => "sparse_to_dense",
        _ => return None,
    })
}

fn strs(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::String(s) => s
            .split([',', '+'])
            .map(|x| x.trim().to_lowercase().replace([' ', '-'], "_"))
            .filter(|x| !x.is_empty())
            .collect(),
        serde_json::Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str())
            .map(|x| x.trim().to_lowercase().replace([' ', '-'], "_"))
            .collect(),
        _ => Vec::new(),
    }
}

/// Parse and validate an intent object; every field lands in `cons` as
/// applied/adjusted/ignored (contrasts are confirmed once applied).
pub fn parse_intent(v: &serde_json::Value, cons: &mut Vec<Constraint>) -> Intent {
    use serde_json::json;
    let mut it = Intent::default();
    let Some(m) = v.as_object() else {
        if !v.is_null() {
            constraint(
                cons,
                "intent",
                v.clone(),
                "ignored",
                "intent must be an object",
            );
        }
        return it;
    };
    for (k, x) in m {
        match k.as_str() {
            "mood" => match x.as_str() {
                Some(s) => {
                    let s = s.trim().to_lowercase();
                    let known = INTENTS.iter().any(|(m, _)| *m == s);
                    constraint(cons, "intent.mood", x.clone(), "applied", if known { "drives harmony pool, modes, energy and intent".to_string() } else { format!("'{s}' has no harmony pool of its own; the genre's default progressions are used") });
                    it.mood = Some(s);
                }
                None => constraint(cons, "intent.mood", x.clone(), "ignored", "not a string"),
            },
            "energy" => match x.as_f64() {
                Some(e) => {
                    let c = (e as f32).clamp(0.15, 1.0);
                    let st = if (c - e as f32).abs() > 1e-4 { "adjusted" } else { "applied" };
                    constraint(cons, "intent.energy", x.clone(), st, format!("energy {c:.2} (0..1, floor 0.15)"));
                    it.energy = Some(c);
                }
                None => constraint(cons, "intent.energy", x.clone(), "ignored", "energy is a number 0..1"),
            },
            "emotion" | "emotional_intent" => match x.as_str() {
                Some(s) => {
                    constraint(cons, "intent.emotion", x.clone(), "applied", "recorded as the direction's intent");
                    it.emotion = Some(s.to_string());
                }
                None => constraint(cons, "intent.emotion", x.clone(), "ignored", "not a string"),
            },
            "hero" => match x.as_str().map(|s| s.to_lowercase()) {
                Some(s) if ["motif", "groove", "bass", "texture"].contains(&s.as_str()) => {
                    constraint(cons, "intent.hero", x.clone(), "applied", "the hero element shapes motif density, kick density, 808 mode, chord rhythm and mix");
                    it.hero = Some(s);
                }
                _ => constraint(cons, "intent.hero", x.clone(), "ignored", "hero is motif, groove, bass or texture"),
            },
            "density" => match x.as_str().map(|s| s.to_lowercase()) {
                Some(s) if ["sparse", "balanced", "dense"].contains(&s.as_str()) => {
                    constraint(cons, "intent.density", x.clone(), "applied", "drives kick count, hat rate and harmonic rhythm");
                    it.density = Some(s);
                }
                _ => constraint(cons, "intent.density", x.clone(), "ignored", "density is sparse, balanced or dense"),
            },
            "rhythmic_feel" | "feel" => {
                for f in strs(x) {
                    let f = if f == "halftime" { "half_time".to_string() } else { f };
                    if FEELS.contains(&f.as_str()) {
                        constraint(cons, "intent.rhythmic_feel", json!(f), "applied", "restricts the groove distribution");
                        it.rhythmic_feel.push(f);
                    } else {
                        constraint(cons, "intent.rhythmic_feel", json!(f), "ignored", format!("unknown feel (known: {})", FEELS.join(", ")));
                    }
                }
            }
            "motif" => {
                let Some(mo) = x.as_object() else {
                    constraint(cons, "intent.motif", x.clone(), "ignored", "motif is {contour, rhythm, density}");
                    continue;
                };
                for (mk, mv) in mo {
                    match (mk.as_str(), mv) {
                        ("contour", serde_json::Value::String(s)) if CONTOURS.contains(&s.as_str()) => {
                            constraint(cons, "intent.motif.contour", mv.clone(), "applied", "main motif contour");
                            it.motif_contour = Some(s.clone());
                        }
                        ("rhythm", serde_json::Value::String(s)) if CELLS.contains(&s.as_str()) => {
                            constraint(cons, "intent.motif.rhythm", mv.clone(), "applied", "main motif rhythm cell");
                            it.motif_rhythm = Some(s.clone());
                        }
                        ("density", serde_json::Value::Number(n)) => {
                            let d = (n.as_f64().unwrap_or(0.5) as f32).clamp(0.2, 0.95);
                            constraint(cons, "intent.motif.density", mv.clone(), "applied", format!("lead density {d:.2}"));
                            it.motif_density = Some(d);
                        }
                        _ => constraint(cons, &format!("intent.motif.{mk}"), mv.clone(), "ignored", format!("contour: {}; rhythm: {}; density: 0..1", CONTOURS.join("/"), CELLS.join("/"))),
                    }
                }
            }
            "palette" | "sound_palette" => {
                let Some(pm) = x.as_object() else {
                    constraint(cons, "intent.palette", x.clone(), "ignored", "palette is {role: preset}");
                    continue;
                };
                for (role, pv) in pm {
                    let r = role.to_lowercase();
                    match pv.as_str() {
                        Some(p) if ROLES.contains(&r.as_str()) && crate::instruments::preset(p).is_some() => {
                            constraint(cons, &format!("intent.palette.{r}"), pv.clone(), "applied", "sound for that role");
                            it.palette.insert(r, p.to_string());
                        }
                        Some(p) if !ROLES.contains(&r.as_str()) => constraint(cons, &format!("intent.palette.{r}"), pv.clone(), "ignored", format!("unknown role (roles: {}); '{p}' not used", ROLES.join(", "))),
                        _ => constraint(cons, &format!("intent.palette.{r}"), pv.clone(), "ignored", "unknown preset (see get_guide for presets)"),
                    }
                }
            }
            "contrasts" => {
                for c in strs(x) {
                    match contrast_alias(&c) {
                        Some(w) => it.contrasts.push(w.to_string()),
                        None => constraint(cons, "intent.contrasts", json!(c), "ignored", format!("unknown contrast (known: {})", WILDCARDS.iter().map(|w| w.0).collect::<Vec<_>>().join(", "))),
                    }
                }
            }
            "target_lufs" => match x.as_f64() {
                Some(v) => constraint(cons, "intent.target_lufs", x.clone(), "applied", format!("master to {v:.1} LUFS")),
                None => constraint(cons, "intent.target_lufs", x.clone(), "ignored", "target_lufs is a number like -14"),
            },
            other => constraint(cons, &format!("intent.{other}"), x.clone(), "ignored", "unknown intent field (mood, energy, emotion, hero, density, rhythmic_feel, motif, palette, contrasts)"),
        }
    }
    it
}

/// Narrow the genre distribution to the requested rhythmic feel.
pub fn apply_feel(sp: &mut GenSpec, feels: &[String]) {
    for f in feels {
        match f.as_str() {
            "half_time" => sp.snare_modes = m(&[("half_time", 1.0)]),
            "backbeat" => sp.snare_modes = m(&[("backbeat", 1.0)]),
            "triplet" => sp.hat_rates = m(&[("8t", 1.0)]),
            "driving" => sp.hat_rates = m(&[("16", 1.0)]),
            "sparse_hats" => sp.hat_rates = m(&[("8", 1.0), ("4", 0.6)]),
            "rolling" => sp.hat_rolls = Some([0.6, 0.9]),
            "swing" => {
                let [lo, hi] = sp.swing.unwrap_or([0.0, 0.2]);
                sp.swing = Some([lo.max(0.15), (hi + 0.12).min(0.55)]);
            }
            "straight" => sp.swing = Some([0.0, 0.0]),
            "bounce" => {
                sp.kick_per_bar = Some([3.5, 5.5]);
                sp.snare_variations = m(&[("pickup", 1.0), ("displaced", 1.0)]);
            }
            _ => {}
        }
    }
}
