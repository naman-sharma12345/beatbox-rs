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
    /// The genre as a distribution (ranges and weights) for the procedural
    /// generator; missing fields are derived from the fields above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generative: Option<crate::creative::GenSpec>,
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
    /// How the main motif is developed here (statement, call_response,
    /// inversion, retrograde, displace, fragment, sequence, register_up/down).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub development: Vec<String>,
    /// Creative tags from wildcards: half_time, switch, dropout,
    /// sparse_to_dense, bass_call_response.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
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
    /// Reverb send offset in dB (the beat's sense of space).
    #[serde(default)]
    pub space_db: f32,
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
    /// The sound palette (palette.rs) picked for the genre, if any.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sound_palette: String,
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
    /// Critic iterations: render id, scores, the ears' diff verdict against
    /// the best render so far, and whether the revision was kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Value>,
    /// The kept render's full-ears critique (the loop chooses with the fast
    /// ears; the delivered score always comes from the full report).
    #[serde(default)]
    pub final_review: Value,
    // ---- sprint 9: the creative layer (all optional so older plans load)
    /// template, procedural, reference_guided or ai_authored.
    #[serde(default)]
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<crate::creative::Direction>,
    /// Every decision with its level and reason, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<crate::creative::Decision>,
    /// The drum identity (2-bar core per voice); sections vary it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groove: Option<crate::creative::Groove>,
    /// A second groove for sections tagged `switch` (beat switch wildcard).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groove_b: Option<crate::creative::Groove>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wildcards: Vec<crate::creative::Wildcard>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub progression_bridge: String,
    /// Chords for sections tagged `switch` (the beat switch is a real flip:
    /// new groove, new harmonic motion).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub progression_switch: String,
    /// as_written, sevenths, add9, sus, power.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub harmony_color: String,
    /// Chord rhythm style (overrides the playbook's when set).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub harmony_style: String,
    /// lock (808 on the kick), independent, sustain.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bass_mode: String,
    /// pad_lines or motif_echo.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub counter_mode: String,
    /// Semitones per section (key change wildcard).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub section_transpose: BTreeMap<String, i32>,
    /// AI-authored MIDI: section name or kind (or "*") -> role -> notes.
    /// Played verbatim in place of the generated part.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub authored: BTreeMap<String, BTreeMap<String, Vec<Note>>>,
    /// Every requested constraint (args and structured intent) and whether
    /// it was applied, adjusted or ignored, with why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<crate::creative::Constraint>,
    /// Seed, seed source, generator version, config and asset manifest.
    #[serde(default)]
    pub provenance: Value,
    /// The novelty check: distance to recent outputs, regenerations.
    #[serde(default)]
    pub novelty: Value,
}

#[derive(Clone, Debug, Default)]
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
    /// "explicit" (reproduce exactly) or "entropy" (fresh per produce).
    pub seed_source: String,
    /// Force a generation method (template, procedural, reference_guided).
    pub method: Option<String>,
    /// Reference recording for the reference-guided method.
    pub reference: Option<String>,
    /// AI-authored notes (see Plan::authored).
    pub authored: BTreeMap<String, BTreeMap<String, Vec<Note>>>,
    /// Structured intent filled in by the connected model (see creative::parse_intent).
    pub intent: Value,
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
    let stable: [i32; 3] = [0, 2, 4];
    let mut deg: i32 = stable[rng.below(3)];
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

/// Map a sound palette onto the plan's role -> preset table. Drum roles,
/// the bass and a keys/pad harmony take the palette's voice; roles that the
/// variation step already changed from the playbook stock keep their swap,
/// and featured voices (choir, strings, tanpura, Indian leads) stay.
pub fn apply_sound_palette(
    palette: &mut BTreeMap<String, String>,
    stock: &BTreeMap<String, String>,
    sp: &crate::palette::Palette,
) -> Vec<String> {
    let voice = |r: &str| {
        sp.voices
            .iter()
            .find(|(x, _)| *x == r)
            .map(|(_, v)| v.to_string())
    };
    let mut out = Vec::new();
    let roles: Vec<String> = palette.keys().cloned().collect();
    for role in roles {
        let cur = palette[&role].clone();
        if stock.get(&role) != Some(&cur) {
            continue; // a deliberate swap (kit-sound variation)
        }
        let pal_role = match role.as_str() {
            "kick" | "hat" | "open_hat" | "bass" => Some(role.clone()),
            "snare" if cur.contains("clap") => Some("clap".to_string()),
            "snare" => Some("snare".to_string()),
            "perc" if matches!(cur.as_str(), "rim" | "rimshot" | "cowbell" | "perc") => {
                Some("perc".to_string())
            }
            "harmony" | "counter" | "texture" => match cur.as_str() {
                "epiano" | "dark_keys" | "upright_keys" | "layered_keys" => {
                    Some("keys".to_string())
                }
                "dark_pad" | "warm_pad" | "wt_pad" | "pad" => Some("pad".to_string()),
                _ => None,
            },
            _ => None,
        };
        let Some(pr) = pal_role else { continue };
        let Some(v) = voice(&pr) else { continue };
        // never put an Indian drum or drone on a kit role
        if matches!(v.as_str(), "tabla" | "bayan" | "tanpura") && role != "harmony" {
            continue;
        }
        if v != cur && crate::instruments::preset(&v).is_some() {
            out.push(format!("{role}={v}"));
            palette.insert(role, v);
        }
    }
    out
}

/// The hierarchical planner: direction first, then core material chosen by
/// the direction, then controlled variation (see creative.rs). Every choice
/// is recorded in `decisions` with its reason; the same seed (and args)
/// always gives the same plan.
pub fn plan_track(a: &PlanArgs) -> Result<Plan> {
    use crate::creative as cr;
    let mut rng = Rng::new(a.seed ^ 0xB00B5);
    let mut dec: Vec<cr::Decision> = Vec::new();
    let mut thinking = Vec::new();
    let pb = match &a.genre {
        Some(g) => {
            playbook(g).or_else(|_| guess_genre(g).ok_or_else(|| anyhow!("unknown genre '{g}'")))?
        }
        None => guess_genre(&a.brief).unwrap_or_else(|| playbook("trap").unwrap()),
    };
    // what was asked for, and what happens to each request
    let mut cons: Vec<cr::Constraint> = Vec::new();
    match &a.genre {
        Some(g) if g.as_str() == pb.name || pb.aliases.contains(g) => cr::constraint(
            &mut cons,
            "genre",
            json!(g),
            "applied",
            format!("playbook {}", pb.name),
        ),
        Some(g) => cr::constraint(
            &mut cons,
            "genre",
            json!(g),
            "adjusted",
            format!("matched to playbook {}", pb.name),
        ),
        None => cr::constraint(
            &mut cons,
            "genre",
            Value::Null,
            "adjusted",
            format!(
                "not given; {} guessed from the brief (keyword match)",
                pb.name
            ),
        ),
    }
    for (k, v) in [
        ("bpm", json!(a.bpm)),
        ("key", json!(a.key)),
        ("scale", json!(a.scale)),
        ("duration_s", json!(a.duration_s)),
    ] {
        if !v.is_null() {
            cr::constraint(&mut cons, k, v, "applied", "used as given");
        }
    }
    let intent = cr::parse_intent(&a.intent, &mut cons);
    let mut sp = cr::spec(pb);
    cr::apply_feel(&mut sp, &intent.rhythmic_feel);
    thinking.push(format!("Genre: {} - {}", pb.name, pb.description));
    // ---- 0. how this beat is generated
    let tw = sp.template_weight.unwrap_or(0.3);
    let (mut method, why) = if !a.authored.is_empty() {
        (
            "ai_authored".to_string(),
            "notes were authored by an agent through MCP; generated parts fill the rest"
                .to_string(),
        )
    } else if let Some(m) = &a.method {
        if !["template", "procedural", "reference_guided"].contains(&m.as_str()) {
            bail!("unknown method '{m}' (template, procedural, reference_guided; ai_authored comes from passing 'authored' notes)");
        }
        (m.clone(), "asked for".to_string())
    } else if a.reference.is_some() {
        (
            "reference_guided".to_string(),
            "a reference recording was given: its density and loudness steer the groove and energy"
                .to_string(),
        )
    } else if rng.chance(tw) {
        ("template".to_string(), format!("drawn: the genre's templates are one starting point ({:.0}% of beats), varied by the same development and wildcards", tw * 100.0))
    } else {
        (
            "procedural".to_string(),
            format!(
                "drawn: generated from the genre's distributions ({:.0}% of beats)",
                (1.0 - tw) * 100.0
            ),
        )
    };
    let mut refx = None;
    if method == "reference_guided" {
        match a.reference.as_deref().map(cr::reference_features) {
            Some(Ok(f)) => refx = Some(f),
            Some(Err(err)) => {
                cr::decide(
                    &mut dec,
                    "method",
                    "reference",
                    "unreadable",
                    format!("{err}; falling back to procedural"),
                );
                method = "procedural".into();
            }
            None => {
                cr::decide(
                    &mut dec,
                    "method",
                    "reference",
                    "missing",
                    "method reference_guided needs reference_path; falling back to procedural",
                );
                method = "procedural".into();
            }
        }
    }
    let mut why = why;
    if method == "template" && intent.shapes_material() {
        if a.method.is_some() {
            for c in cons.iter_mut().filter(|c| {
                c.field.starts_with("intent.rhythmic_feel")
                    || c.field.starts_with("intent.motif.contour")
                    || c.field.starts_with("intent.motif.rhythm")
            }) {
                c.status = "ignored".into();
                c.note =
                    "method=template keeps the playbook's drum patterns and motif generator".into();
            }
        } else {
            method = "procedural".into();
            why = "the intent constrains rhythm/motif, which the template method cannot honour"
                .into();
        }
    }
    cr::decide(&mut dec, "method", "generation method", &method, why);
    let template = method == "template";
    // ---- 1. creative direction
    if let (Some(m), Some(im)) = (&a.mood, &intent.mood) {
        if m != im {
            for c in cons.iter_mut().filter(|c| c.field == "intent.mood") {
                c.status = "ignored".into();
                c.note = format!("the mood argument '{m}' wins over intent.mood");
            }
        }
    }
    if let Some(m) = &a.mood {
        cr::constraint(&mut cons, "mood", json!(m), "applied", "used as given");
    }
    let mood = if let Some(m) = &a.mood {
        (m.clone(), "asked for".to_string())
    } else if let Some(m) = &intent.mood {
        (m.clone(), "asked for in the structured intent".to_string())
    } else if let Some(m) = detect_mood(&a.brief) {
        (m.to_string(), "read from the brief".to_string())
    } else {
        let moods: Vec<&String> = pb.progressions.keys().filter(|k| *k != "default").collect();
        if moods.is_empty() {
            (
                "default".to_string(),
                "the genre has a single harmonic mood".to_string(),
            )
        } else {
            (
                moods[rng.below(moods.len())].clone(),
                format!(
                    "the brief names no mood; drawn from the moods {} supports",
                    pb.name
                ),
            )
        }
    };
    let mut dir = cr::direct(pb, &a.brief, mood, &mut rng, &mut dec);
    // the structured intent overrides what the keyword reading guessed
    let mut overridden = Vec::new();
    if let Some(e) = intent.energy {
        dir.energy = e;
        overridden.push(format!("energy {e:.2}"));
    }
    if let Some(h) = &intent.hero {
        dir.hero = h.clone();
        overridden.push(format!("hero {h}"));
    }
    if let Some(d) = &intent.density {
        dir.density = d.clone();
        overridden.push(format!("density {d}"));
    }
    if let Some(em) = &intent.emotion {
        dir.intent = em.clone();
        overridden.push(format!("emotion '{em}'"));
    }
    if !overridden.is_empty() {
        dir.identity = format!(
            "{}: {}-led, {} (from the structured intent)",
            dir.intent, dir.hero, dir.density
        );
        cr::decide(
            &mut dec,
            "direction",
            "intent overrides",
            overridden.join(", "),
            "asked for in the structured intent",
        );
    }
    if let Some(r) = &refx {
        dir.energy = (0.5 * dir.energy + 0.5 * ((r.lufs + 22.0) / 14.0)).clamp(0.15, 1.0);
        cr::decide(
            &mut dec,
            "direction",
            "energy (reference)",
            format!("{:.2}", dir.energy),
            format!("blended with the reference's loudness ({:.1} LUFS)", r.lufs),
        );
    }
    let mood = dir.mood.clone();
    thinking.push(format!(
        "Direction: {} (mood {mood}, energy {:.2}).",
        dir.identity, dir.energy
    ));
    // ---- 2. core material, chosen by the direction
    let bpm = match a.bpm {
        Some(b) => {
            cr::decide(&mut dec, "core", "tempo", format!("{b}"), "asked for");
            b
        }
        None => {
            let x = (0.15 + 0.7 * dir.energy + rng.range(-0.2, 0.2)).clamp(0.0, 1.0);
            let b = (pb.bpm[0] + x * (pb.bpm[1] - pb.bpm[0])).round();
            cr::decide(
                &mut dec,
                "core",
                "tempo",
                format!("{b}"),
                format!(
                    "{}-{} for {}; energy {:.2} places it at {:.0}% of the range",
                    pb.bpm[0],
                    pb.bpm[1],
                    pb.name,
                    dir.energy,
                    x * 100.0
                ),
            );
            b
        }
    };
    if bpm < pb.bpm[0] - 10.0 || bpm > pb.bpm[1] + 10.0 {
        thinking.push(format!(
            "Note: {bpm} BPM is outside the usual {}-{} for {}; keeping it as asked.",
            pb.bpm[0], pb.bpm[1], pb.name
        ));
    }
    let scale = match &a.scale {
        Some(s) => s.clone(),
        None => {
            let mut allowed: Vec<String> = pb.scales.clone();
            if !template {
                for m in &sp.modes {
                    if !allowed.contains(m) {
                        allowed.push(m.clone());
                    }
                }
            }
            let prefs = cr::mood_modes(&mood);
            let w: Vec<f32> = allowed
                .iter()
                .map(|s| {
                    if prefs.contains(&s.as_str()) {
                        3.0
                    } else {
                        1.0
                    }
                })
                .collect();
            let s = allowed
                .get(rng.weighted(&w))
                .cloned()
                .unwrap_or_else(|| "minor".into());
            cr::decide(
                &mut dec,
                "core",
                "mode",
                &s,
                format!("from {:?}; modes that suit '{mood}' weigh 3x", allowed),
            );
            s
        }
    };
    theory::scale_intervals(&scale)?;
    let key = match &a.key {
        Some(k) => k.clone(),
        None => {
            // any tonic, slightly favouring keys whose 808 sits in the sweet spot
            let names = [
                "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
            ];
            let w = [1.0, 1.0, 1.0, 1.0, 1.4, 1.4, 1.4, 1.4, 1.2, 1.2, 1.0, 1.0];
            let k = names[rng.weighted(&w)].to_string();
            cr::decide(
                &mut dec,
                "core",
                "key",
                &k,
                "any tonic; E-A weigh a little more (808 fundamentals around 41-55 Hz)",
            );
            k
        }
    };
    theory::pitch_class(&key)?;
    let hc = cr::choose_harmony(pb, &sp, &dir, &scale, template, &mut rng, &mut dec);
    let pv = hc.verse.clone();
    let ph = hc.hook.clone();
    thinking.push(format!("Mood '{mood}': {key} {scale} at {bpm} BPM; verse progression '{pv}', hook '{ph}' (numerals relative to parallel {}).", harmony_ref(&scale)));
    let iv = theory::scale_intervals(&scale)?;
    // the main motif: its density and shape serve the hero element and mood
    let base_d = pb.lead.density.unwrap_or(0.5);
    let lead_density = if let Some(d) = intent.motif_density {
        d
    } else if template {
        base_d
    } else {
        let d = match dir.hero.as_str() {
            "motif" => rng.range(0.55, 0.8),
            "groove" => rng.range(0.3, 0.5),
            "bass" => rng.range(0.35, 0.55),
            _ => rng.range(0.25, 0.45),
        };
        (0.5 * d + 0.5 * base_d + 0.1 * (dir.energy - 0.5)).clamp(0.2, 0.95)
    };
    let motif = if template {
        let m = make_motif(&mut rng, lead_density, iv.len());
        cr::decide(
            &mut dec,
            "core",
            "motif",
            format!("{:?}", m.iter().map(|x| x.deg).collect::<Vec<_>>()),
            "template method: the classic motif generator",
        );
        m
    } else {
        let cw: Vec<(&str, f32)> = match mood.as_str() {
            "dark" => vec![
                ("descending", 2.0),
                ("wave", 1.5),
                ("static", 1.5),
                ("leap_fall", 1.0),
                ("arch", 0.5),
            ],
            "sad" => vec![
                ("descending", 2.0),
                ("arch", 1.5),
                ("leap_fall", 1.5),
                ("wave", 0.7),
            ],
            "hype" => vec![
                ("static", 2.0),
                ("wave", 1.5),
                ("ascending", 1.0),
                ("leap_fall", 0.7),
            ],
            "hopeful" => vec![("ascending", 2.0), ("arch", 2.0), ("wave", 0.7)],
            "devotional" => vec![("arch", 2.0), ("wave", 1.5), ("descending", 0.8)],
            _ => vec![
                ("arch", 1.2),
                ("wave", 1.5),
                ("descending", 1.0),
                ("ascending", 0.8),
                ("static", 0.6),
                ("leap_fall", 0.6),
            ],
        };
        let drawn = cw[rng.weighted(&cw.iter().map(|x| x.1).collect::<Vec<_>>())].0;
        let contour = intent.motif_contour.as_deref().unwrap_or(drawn);
        let mut rw = [
            ("on_grid", 1.0f32),
            ("syncopated", 1.0),
            ("long_short", 0.6),
            ("triplet", 0.2),
        ];
        if dir.hero == "groove" {
            rw[1].1 += 1.0;
        }
        if pb.drums.half_time {
            rw[3].1 += 0.5;
        }
        if pb.swing > 0.2 {
            rw[0].1 += 0.6;
        }
        let drawn = rw[rng.weighted(&rw.iter().map(|x| x.1).collect::<Vec<_>>())].0;
        let cell = intent.motif_rhythm.as_deref().unwrap_or(drawn);
        let m = cr::shaped_motif(&mut rng, lead_density, iv.len(), contour, cell);
        cr::decide(
            &mut dec,
            "core",
            "motif",
            format!(
                "{contour} contour, {cell} rhythm, {} notes, degrees {:?}",
                m.len(),
                m.iter().map(|x| x.deg).collect::<Vec<_>>()
            ),
            format!(
                "a {contour} line suits '{}'; density {lead_density:.2} because the beat is {}-led",
                dir.intent, dir.hero
            ),
        );
        m
    };
    thinking.push(format!(
        "Motif ({} notes, one bar): degrees {:?} - developed per section (statement, call and response, fragments, inversion, displacement, register).",
        motif.len(),
        motif.iter().map(|m| m.deg).collect::<Vec<_>>()
    ));
    let mut palette = layer_part_presets(pb, &mut rng, &a.brief);
    let stock = palette.clone();
    if !template {
        for (voice, alts) in &sp.drum_sounds {
            if !alts.is_empty() && palette.contains_key(voice) && rng.chance(0.4) {
                let p = alts[rng.below(alts.len())].clone();
                if crate::instruments::preset(&p).is_some() {
                    cr::decide(
                        &mut dec,
                        "core",
                        "kit sound",
                        format!("{voice}={p}"),
                        "a different drum voice from the genre's alternatives",
                    );
                    palette.insert(voice.clone(), p);
                }
            }
        }
    }
    // the genre's sound palette (palette.rs) supplies the drum, bass and
    // keys/pad voices; a kit sound the variation step already swapped stays
    let sound_palette = crate::palette::for_genre(&pb.name)
        .map(|sp| {
            let swapped = apply_sound_palette(&mut palette, &stock, sp);
            cr::decide(
                &mut dec,
                "core",
                "sound palette",
                sp.name,
                format!(
                    "the default palette for {}: {} ({})",
                    pb.name,
                    if swapped.is_empty() {
                        "no role swapped".to_string()
                    } else {
                        swapped.join(", ")
                    },
                    sp.character.join(", ")
                ),
            );
            sp.name.to_string()
        })
        .unwrap_or_default();
    for (r, p) in &intent.palette {
        cr::decide(
            &mut dec,
            "core",
            "sound (intent)",
            format!("{r}={p}"),
            "asked for in the structured intent",
        );
        palette.insert(r.clone(), p.clone());
    }
    let sample_kit = if a.use_samples {
        let k = if template {
            pb.drums.sample_kit.clone()
        } else {
            cr::wmap_pub(&sp.kits, &mut rng).or_else(|| pb.drums.sample_kit.clone())
        };
        if let Some(k) = &k {
            cr::decide(
                &mut dec,
                "core",
                "drum kit",
                k,
                "real public-domain kit drawn from the genre's kits",
            );
        }
        k
    } else {
        None
    };
    // groove: the drum identity
    let groove = cr::build_groove(
        pb,
        &sp,
        &dir,
        template,
        bpm,
        refx.as_ref(),
        &mut rng,
        &mut dec,
    );
    let swing = if template {
        pb.swing
    } else {
        let [lo, hi] = sp.swing.unwrap_or([pb.swing, pb.swing]);
        let mut x = rng.f32();
        if matches!(mood.as_str(), "chill" | "jazzy" | "smooth") {
            x = x.max(rng.f32());
        }
        let s = ((lo + (hi - lo) * x) * 100.0).round() / 100.0;
        cr::decide(
            &mut dec,
            "core",
            "swing",
            format!("{s:.2}"),
            format!("drawn from {lo:.2}-{hi:.2}; laid-back moods lean late"),
        );
        s
    };
    // the 808 / bass
    let mut bm = sp.bass_modes.clone();
    if dir.hero == "bass" {
        if let Some(v) = bm.get_mut("independent") {
            *v *= 2.0;
        }
    }
    if dir.hero == "texture" {
        if let Some(v) = bm.get_mut("sustain") {
            *v *= 2.0;
        }
    }
    let bass_mode = if template {
        "lock".to_string()
    } else {
        cr::wmap_pub(&bm, &mut rng).unwrap_or_else(|| "lock".into())
    };
    let [gl, gh] = sp.glide.unwrap_or([pb.bass.glide, pb.bass.glide]);
    let glide = if template {
        pb.bass.glide
    } else {
        gl + (gh - gl) * rng.f32()
    };
    cr::decide(
        &mut dec,
        "core",
        "808/bass",
        format!("{bass_mode}, glide {glide:.2}"),
        match bass_mode.as_str() {
            "independent" => "the 808 keeps the kick's anchors but adds its own pushes",
            "sustain" => "long held 808 notes: weight under an atmospheric beat",
            _ => "the 808 locks to the kick",
        },
    );
    let harmony_style = if template {
        String::new()
    } else {
        let mut hs = sp.harmony_styles.clone();
        if dir.hero == "groove" {
            for k in ["stabs", "pulse", "offbeat"] {
                if let Some(v) = hs.get_mut(k) {
                    *v *= 2.0;
                }
            }
        }
        if dir.hero == "texture" {
            if let Some(v) = hs.get_mut("block") {
                *v *= 2.0;
            }
        }
        let s = cr::wmap_pub(&hs, &mut rng).unwrap_or_default();
        cr::decide(
            &mut dec,
            "core",
            "chord rhythm",
            &s,
            format!("{}-led beat", dir.hero),
        );
        s
    };
    let counter_mode = if !template && (dir.hero == "motif" && rng.chance(0.6) || rng.chance(0.2)) {
        cr::decide(&mut dec, "variation", "counter-melody", "motif_echo", "the counter line answers the hook with an inverted fragment of the motif (call and response)");
        "motif_echo".to_string()
    } else {
        "pad_lines".to_string()
    };
    // arrangement
    let mut tmpl =
        cr::generate_arrangement(pb, &sp, &dir, &palette, bpm, template, &mut rng, &mut dec);
    let bar_s = 240.0 / bpm;
    let total_bars: u32 = tmpl.iter().map(|s| s.bars).sum();
    if let Some(d) = a.duration_s {
        let want = (d / bar_s).round().max(8.0) as u32;
        if want < total_bars {
            // drop sections from the back until it fits (keep intro, outro, one hook)
            while tmpl.iter().map(|s| s.bars).sum::<u32>() > want && tmpl.len() > 3 {
                let hooks = tmpl.iter().filter(|s| s.kind == "hook").count();
                let Some(idx) = (1..tmpl.len() - 1)
                    .rev()
                    .find(|i| tmpl[*i].kind != "hook" || hooks > 1)
                else {
                    break;
                };
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
    // one hook reads as a sketch, not a song (critique_mix and the critic,
    // beat 9): when a verse follows the only hook, the hook comes back after it
    if a.duration_s.is_none() && tmpl.iter().filter(|s| s.kind == "hook").count() == 1 {
        let h = tmpl.iter().position(|s| s.kind == "hook").unwrap_or(0);
        if let Some(lv) = tmpl.iter().rposition(|s| s.kind == "verse").filter(|lv| *lv > h) {
            let again = tmpl[h].clone();
            tmpl.insert(lv + 1, again);
            thinking.push("The hook comes back after the last verse, so it lands twice.".to_string());
        }
    }
    // a form the words dictate (make_beat with lyrics: one section per stanza,
    // sized to its lines) replaces the playbook's arrangement
    if let Some(form) = a.intent.get("form").and_then(|v| v.as_array()) {
        let mut out: Vec<SectionTemplate> = Vec::new();
        for f in form {
            let kind = f["kind"].as_str().unwrap_or("verse").to_string();
            let bars = f["bars"].as_u64().unwrap_or(8).clamp(1, 64) as u32;
            let like = match kind.as_str() {
                "pre" | "prechorus" | "bridge" | "breakdown" => "verse",
                k => k,
            };
            let base = tmpl
                .iter()
                .find(|s| s.kind == kind)
                .or_else(|| tmpl.iter().find(|s| s.kind == like))
                .or_else(|| tmpl.iter().find(|s| s.kind == "verse"))
                .cloned();
            if let Some(mut s) = base {
                s.kind = kind;
                s.bars = bars;
                out.push(s);
            }
        }
        if out.len() >= 2 {
            thinking.push(format!(
                "Form from the words: {}.",
                out.iter().map(|s| format!("{}({})", s.kind, s.bars)).collect::<Vec<_>>().join(" > ")
            ));
            tmpl = out;
        }
    }
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    let hooks = tmpl.iter().filter(|s| s.kind == "hook").count();
    let mut hook_no = 0;
    let mut sections = Vec::new();
    for (i, s) in tmpl.iter().enumerate() {
        let c = counts.entry(s.kind.clone()).or_insert(0);
        *c += 1;
        let nth = *c as usize;
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
            "bridge" | "breakdown" => "augment",
            _ => "call",
        };
        let next = tmpl.get(i + 1);
        let transition = if template {
            match next {
                Some(n) if n.energy > s.energy + 0.15 => {
                    if pb.drums.hat_rolls > 0.3 {
                        "drop_and_roll".to_string()
                    } else {
                        "snare_fill".to_string()
                    }
                }
                Some(n) if n.energy + 0.3 < s.energy => "fade_down".to_string(),
                Some(_) => "turnaround".to_string(),
                None => "end".to_string(),
            }
        } else {
            cr::draw_transition(s.energy, next.map(|n| n.energy), groove.hat_rolls, &mut rng)
        };
        let last_hook = s.kind == "hook" && hook_no == hooks && hooks > 1;
        let (development, why) = cr::development_for(&s.kind, nth, last_hook, &mut rng);
        cr::decide(
            &mut dec,
            "variation",
            &format!("motif in {name}"),
            development.join("+"),
            why,
        );
        sections.push(PlanSection {
            name,
            kind: s.kind.clone(),
            bars: s.bars,
            energy: s.energy,
            layers: s.layers.clone(),
            lead: lead.into(),
            transition,
            development,
            tags: Vec::new(),
        });
    }
    // ---- 3. purposeful wildcards
    let mut genre_presets: Vec<String> = Vec::new();
    for r in [&pb.harmony, &pb.lead, &pb.counter]
        .into_iter()
        .chain(pb.texture.iter())
    {
        genre_presets.extend(r.presets.iter().cloned());
        genre_presets.extend(r.preset.iter().cloned());
    }
    let mut switch = false;
    let mut skipped = Vec::new();
    let mut section_transpose = BTreeMap::new();
    let wildcards = cr::apply_wildcards(
        cr::WildTargets {
            sections: &mut sections,
            palette: &mut palette,
            transpose: &mut section_transpose,
            switch_groove: &mut switch,
            genre_presets,
            weights: sp.wildcard_weights.clone(),
        },
        &dir,
        &intent.contrasts,
        &mut skipped,
        &mut rng,
        &mut dec,
    );
    for c in &intent.contrasts {
        if wildcards.iter().any(|w| &w.name == c) {
            cr::constraint(
                &mut cons,
                "intent.contrasts",
                json!(c),
                "applied",
                "placed as a wildcard (see wildcards)",
            );
        } else {
            cr::constraint(
                &mut cons,
                "intent.contrasts",
                json!(c),
                "ignored",
                "not possible in this arrangement (e.g. needs two hooks or a long verse)",
            );
        }
    }
    let groove_b = if switch {
        let mut r2 = Rng::new(a.seed ^ 0x0057_17C4);
        let mut d2 = dir.clone();
        d2.density = if dir.density == "dense" {
            "balanced".into()
        } else {
            "dense".into()
        };
        Some(cr::build_groove(
            pb, &sp, &d2, false, bpm, None, &mut r2, &mut dec,
        ))
    } else {
        None
    };
    // a real flip: the switched half also moves to new harmonic motion (the hook's
    // chords rotated to start elsewhere, so the colour stays and the motion changes)
    let progression_switch = if switch {
        let toks: Vec<&str> = ph.split_whitespace().collect();
        let mut best = String::new();
        for r in [2usize, 1, 3] {
            if toks.len() < 2 {
                break;
            }
            let rot: Vec<&str> = toks.iter().cycle().skip(r % toks.len()).take(toks.len()).copied().collect();
            let s = rot.join(" ");
            if s != ph && s != pv {
                best = s;
                break;
            }
        }
        if !best.is_empty() {
            cr::decide(
                &mut dec,
                "wildcard",
                "beat switch harmony",
                &best,
                "the switched half gets its own chord motion, so it is a new chapter, not only new drums",
            );
            thinking.push(format!("Beat switch: drums flip to a second groove, chords move to {best}."));
        }
        best
    } else {
        String::new()
    };
    for w in &wildcards {
        thinking.push(format!(
            "Wildcard [{}] {} on {}: {}",
            w.role, w.name, w.target, w.why
        ));
    }
    let secs: f32 = sections.iter().map(|s| s.bars as f32 * bar_s).sum();
    thinking.push(format!(
        "Structure ({:.0} s): {} - energy {:?}.",
        secs,
        sections
            .iter()
            .map(|s| format!("{}({})", s.name, s.bars))
            .collect::<Vec<_>>()
            .join(" > "),
        sections
            .iter()
            .map(|s| (s.energy * 10.0).round() / 10.0)
            .collect::<Vec<_>>()
    ));
    thinking.push(format!("Drums: {}", groove.summary));
    // mix character: the hero element sits a little forward, the space varies
    let mut offsets = BTreeMap::new();
    let space_db = if template {
        0.0
    } else {
        (rng.range(-3.0, 3.0) * 10.0).round() / 10.0
    };
    if !template {
        let (t, db) = match dir.hero.as_str() {
            "motif" => ("lead", 1.5),
            "groove" => ("hat", 1.0),
            "bass" => ("bass", 1.0),
            _ => ("chords", 1.5),
        };
        offsets.insert(t.to_string(), db);
        cr::decide(
            &mut dec,
            "core",
            "mix character",
            format!("{t} {db:+.1} dB, reverb sends {space_db:+.1} dB"),
            format!(
                "the hero ({}) sits forward; the space is part of this beat's identity",
                dir.hero
            ),
        );
    }
    let knobs = Knobs {
        lead_density,
        hat_rolls: groove.hat_rolls,
        ghost_snare: groove.ghosts,
        glide,
        verse_velocity: 0.9,
        sidechain: pb.mix.sidechain_bass,
        variation_seed: 0,
        offsets,
        // a delivery loudness asked for in the intent (make_beat: -14 for streaming)
        target_lufs: a
            .intent
            .get("target_lufs")
            .and_then(|v| v.as_f64())
            .map(|v| (v as f32).clamp(-24.0, -6.0))
            .unwrap_or(pb.mix.target_lufs),
        tonic_anchor: false,
        humanize: 0.04,
        space_db,
    };
    let title = {
        let words: Vec<&str> = a.brief.split_whitespace().take(4).collect();
        if words.is_empty() {
            format!("{} {}", pb.name, a.seed)
        } else {
            words.join(" ")
        }
    };
    let mut drum_patterns = BTreeMap::new();
    for (k, h) in [
        ("kick", &groove.kick),
        ("snare", &groove.snare),
        ("hat", &groove.hat),
        ("open_hat", &groove.open_hat),
        ("perc", &groove.perc),
        ("tabla", &groove.tabla),
        ("bayan", &groove.bayan),
    ] {
        if !h.is_empty() {
            let b = groove_b
                .as_ref()
                .map(|g| match k {
                    "kick" => cr::grid_string(&g.kick),
                    "snare" => cr::grid_string(&g.snare),
                    "hat" => cr::grid_string(&g.hat),
                    "open_hat" => cr::grid_string(&g.open_hat),
                    "perc" => cr::grid_string(&g.perc),
                    _ => cr::grid_string(h),
                })
                .unwrap_or_else(|| cr::grid_string(h));
            drum_patterns.insert(k.to_string(), [cr::grid_string(h), b]);
        }
    }
    let provenance = json!({
        "seed": a.seed,
        "seed_source": if a.seed_source.is_empty() { "explicit" } else { a.seed_source.as_str() },
        "generator_version": cr::GENERATOR_VERSION,
        "build": option_env!("GITHUB_SHA"),
        "config": {
            "brief": a.brief, "genre": a.genre, "bpm": a.bpm, "key": a.key, "scale": a.scale, "mood": a.mood,
            "duration_s": a.duration_s, "use_samples": a.use_samples, "method": a.method, "reference": a.reference,
            "authored_parts": a.authored.iter().map(|(k, v)| format!("{k}:{}", v.keys().cloned().collect::<Vec<_>>().join("+"))).collect::<Vec<_>>(),
        },
        "assets": {"presets": palette, "sample_kit": sample_kit, "sound_palette": sound_palette},
        "reproduce": format!("produce_track with the same config and seed {} (generator {})", a.seed, cr::GENERATOR_VERSION),
    });
    Ok(Plan {
        title,
        brief: a.brief.clone(),
        genre: pb.name.clone(),
        mood,
        bpm,
        key,
        scale,
        swing,
        progression_verse: pv,
        progression_hook: ph,
        bars_per_chord: hc.bars_per_chord,
        motif,
        palette,
        sound_palette,
        drum_patterns,
        sections,
        knobs,
        sample_kit,
        flip_sample: a.flip_sample.clone(),
        seed: a.seed,
        thinking,
        history: Vec::new(),
        final_review: Value::Null,
        method,
        direction: Some(dir),
        decisions: dec,
        groove: Some(groove),
        groove_b,
        wildcards,
        progression_bridge: hc.bridge,
        progression_switch,
        harmony_color: hc.color,
        harmony_style,
        bass_mode,
        counter_mode,
        section_transpose,
        authored: a.authored.clone(),
        provenance,
        novelty: Value::Null,
        constraints: cons,
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
    bridge_chords: Vec<theory::Chord>,
    switch_chords: Vec<theory::Chord>,
}

impl Ctx<'_> {
    fn chords_for(&self, sec: &PlanSection) -> &[theory::Chord] {
        if !self.switch_chords.is_empty()
            && matches!(sec.kind.as_str(), "verse" | "hook")
            && sec.tags.iter().any(|t| t == "switch")
        {
            return &self.switch_chords;
        }
        self.chords(&sec.kind)
    }
    fn chords(&self, kind: &str) -> &[theory::Chord] {
        match kind {
            "hook" => &self.hook_chords,
            "bridge" | "breakdown" if !self.bridge_chords.is_empty() => &self.bridge_chords,
            _ => &self.verse_chords,
        }
    }
}

/// Highest MIDI pitch a lo-fi lead/counter may reach (E5, ~660 Hz).
const LOFI_LEAD_CEILING: u8 = 76;

/// Keep a line under `ceiling`: when the line sits mostly above it the whole
/// phrase moves down an octave (contour intact), and the few notes still over
/// it fold down an octave each.
fn cap_register(notes: &mut [Note], ceiling: u8) {
    if notes.is_empty() {
        return;
    }
    let mean = notes.iter().map(|n| n.pitch as f32).sum::<f32>() / notes.len() as f32;
    let max = notes.iter().map(|n| n.pitch).max().unwrap_or(0);
    if max > ceiling && mean > ceiling as f32 - 6.0 {
        for n in notes.iter_mut() {
            n.pitch = n.pitch.saturating_sub(12);
            n.slide_to = n.slide_to.map(|x| x.saturating_sub(12));
        }
    }
    for n in notes.iter_mut() {
        while n.pitch > ceiling && n.pitch >= 12 {
            n.pitch -= 12;
        }
        if let Some(t) = n.slide_to.as_mut() {
            while *t > ceiling && *t >= 12 {
                *t -= 12;
            }
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
    let chords = cx.chords_for(sec);
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
    // the main motif, developed for this section (same idea, transformed)
    let (motif, dev_oct) = crate::creative::develop(&cx.plan.motif, &sec.development, rng);
    let oct = oct + dev_oct;
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
            let mut notes: Vec<MotifNote> = motif.clone();
            match pos {
                1 => {
                    // answer: same rhythm start, contour inverted at the end
                    let k = notes.len() / 2;
                    for (i, m) in notes.iter_mut().enumerate() {
                        if i >= k {
                            m.deg = notes_first(&motif) - (m.deg - notes_first(&motif));
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

/// Counter-melody that answers the lead: an inverted fragment of the main
/// motif in the second half of every other bar.
fn motif_echo(cx: &Ctx, sec: &PlanSection, octave: i32) -> Vec<Note> {
    let m = &cx.plan.motif;
    let k = m.len().div_ceil(2).max(1);
    let f = notes_first(m);
    let mut out = Vec::new();
    for bar in (1..sec.bars).step_by(2) {
        for x in m.iter().take(k) {
            let t = bar as f32 * 16.0 + 8.0 + x.t * 0.5;
            let p = scale_pitch(cx.root_pc, cx.iv, octave, f - (x.deg - f));
            out.push(Note::new(t, (x.len * 0.5).max(0.5), p, 0.55));
        }
    }
    let end = (sec.bars * STEPS_PER_BAR) as f32;
    out.retain(|n| n.start < end);
    out
}

fn notes_first(m: &[MotifNote]) -> i32 {
    m.first().map(|x| x.deg).unwrap_or(0)
}

/// Counter-melody: long chord tones moving by step where the lead rests.
fn counter_notes(cx: &Ctx, sec: &PlanSection, octave: i32, lead: &[Note]) -> Vec<Note> {
    let chords = cx.chords_for(sec);
    let bpc = cx.plan.bars_per_chord;
    let mut out = Vec::new();
    let mut prev: Option<u8> = None;
    for half in 0..(sec.bars * 2) {
        let t = half as f32 * 8.0;
        let ch = chord_at(chords, bpc, t);
        // pick the chord tone (3rd/5th/7th preferred) nearest to the previous note
        let base = (octave + 1) * 12;
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
    let chords = cx.chords_for(sec);
    let bpc = cx.plan.bars_per_chord;
    let end = (sec.bars * STEPS_PER_BAR) as f32;
    let root_of = |t: f32| -> u8 {
        let c = chord_at(chords, bpc, t);
        ((octave + 1) * 12 + c.root_pc as i32).clamp(0, 127) as u8
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
        "sustain" => {
            // long 808s, one per chord, gliding into the next root
            let span = bpc * STEPS_PER_BAR as f32;
            let mut t = 0.0;
            while t < end {
                let p = root_of(t);
                let mut n = Note::new(t, span.min(end - t), p, 0.9);
                let np = root_of(t + span);
                if np != p && t + span < end && rng.chance(glide + 0.4) {
                    n.slide_to = Some(np);
                }
                out.push(n);
                t += span;
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
    let mut chord_sets = [
        if plan.progression_switch.is_empty() {
            Vec::new()
        } else {
            theory::parse_progression(&plan.progression_switch, key_pc, href)?
        },
        theory::parse_progression(&plan.progression_verse, key_pc, href)?,
        theory::parse_progression(&plan.progression_hook, key_pc, href)?,
        if plan.progression_bridge.is_empty() {
            Vec::new()
        } else {
            theory::parse_progression(&plan.progression_bridge, key_pc, href)?
        },
    ];
    if !plan.harmony_color.is_empty() {
        for c in chord_sets.iter_mut() {
            crate::creative::color_chords(c, &plan.harmony_color, key_pc);
        }
    }
    let [switch_chords, verse_chords, hook_chords, bridge_chords] = chord_sets;
    let cx = Ctx {
        plan,
        root_pc: key_pc,
        iv: &iv,
        verse_chords,
        hook_chords,
        bridge_chords,
        switch_chords,
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
    // AI-authored parts need their tracks even where no layer asks for them
    for parts in plan.authored.values() {
        for r in parts.keys() {
            if !roles.contains(r) && plan.palette.contains_key(r) {
                roles.push(r.clone());
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
    // R&B with a bright bell/pluck lead: an octave down (critic, beat 8 round 2:
    // the fm_bell lead was over half of all the 2-16 kHz energy in the mix)
    let bright_lead = plan.palette.get("lead").map(|s| ["bell", "pluck", "glock", "celesta", "music_box"].iter().any(|k| s.contains(k))).unwrap_or(false);
    let lead_oct = if pb.mix.balance_genre == "rnb" && bright_lead { pb.lead.octave - 1 } else { pb.lead.octave };
    let ornament = pb.lead.ornament.clone().unwrap_or_default();
    let harmony_style = if plan.harmony_style.is_empty() {
        pb.harmony.style.clone().unwrap_or_else(|| "block".into())
    } else {
        plan.harmony_style.clone()
    };
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
        // melodic parts follow the energy curve harder than drums
        let mel = if is_hook {
            1.0
        } else {
            plan.knobs.verse_velocity
        } * (0.55 + 0.45 * sec.energy);
        let has = |l: &str| sec.layers.iter().any(|x| x == l) && roles.iter().any(|r| r == l);
        let pat_of = |part: &str| {
            plan.drum_patterns
                .get(part)
                .map(|v| v[vi].clone())
                .unwrap_or_default()
        };
        let mut kick = Vec::new();
        if let Some(g0) = &plan.groove {
            // generated drums: the groove varied for this section, with its fill
            let g = if sec.tags.iter().any(|t| t == "switch") {
                plan.groove_b.as_ref().unwrap_or(g0)
            } else {
                g0
            };
            for (voice, mut notes) in crate::creative::section_drums(g, sec, plan.seed, si) {
                if !has(voice.as_str()) || notes.is_empty() {
                    continue;
                }
                let scale = match voice.as_str() {
                    "hat" => 0.75 + 0.25 * sec.energy,
                    "open_hat" | "perc" => vel * 0.85,
                    _ => vel,
                };
                for n in notes.iter_mut() {
                    n.vel = (n.vel * scale).clamp(0.05, 1.0);
                    if voice == "tabla" {
                        n.pitch = tabla_pitch;
                        if (n.start as u32) % 2 == 1 {
                            n.vel = n.vel.min(0.42);
                        }
                    } else if voice == "bayan" {
                        n.pitch = bayan_pitch;
                    }
                }
                if voice == "kick" {
                    kick = notes;
                } else {
                    pat.clips.insert(track_name(&voice), notes);
                }
            }
        } else if has("kick") {
            kick = grid_notes(&pat_of("kick"), sec.bars, 60, vel);
        }
        let legacy = plan.groove.is_none();
        if legacy && has("snare") {
            let mut sn = grid_notes(&pat_of("snare"), sec.bars, 60, vel);
            add_ghosts(&mut sn, sec.bars, plan.knobs.ghost_snare, &mut rng);
            pat.clips.insert(track_name("snare"), sn);
        }
        if legacy && has("hat") {
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
        if legacy && has("open_hat") {
            let mut oh = grid_notes(&pat_of("open_hat"), sec.bars, 60, vel * 0.8);
            if is_hook {
                // crash-like open hat on the downbeat of the hook
                oh.push(Note::new(0.0, 2.0, 60, 0.9));
            }
            pat.clips.insert(track_name("open_hat"), oh);
        }
        if legacy && has("perc") {
            pat.clips.insert(
                track_name("perc"),
                grid_notes(&pat_of("perc"), sec.bars, 60, vel * 0.85),
            );
        }
        if legacy && has("tabla") {
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
        if legacy && has("bayan") {
            pat.clips.insert(
                track_name("bayan"),
                grid_notes(&pat_of("bayan"), sec.bars, bayan_pitch, vel),
            );
        }
        // transitions (generated drums carry their own fills)
        let end = (sec.bars * 16) as f32;
        match if legacy { sec.transition.as_str() } else { "" } {
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
            let mode = plan.bass_mode.as_str();
            let style = if mode == "sustain" {
                "sustain"
            } else {
                pb.bass.style.as_str()
            };
            let mut hits = kick.clone();
            if sec.tags.iter().any(|t| t == "bass_call_response") {
                // the 808 answers in the kick's gaps instead of doubling it
                let mut ks: Vec<f32> = kick.iter().map(|n| n.start).collect();
                ks.sort_by(|a, b| a.partial_cmp(b).unwrap());
                ks.dedup();
                hits.clear();
                for (i, t) in ks.iter().enumerate() {
                    if t % 16.0 == 0.0 {
                        hits.push(Note::new(*t, 1.0, 60, 1.0));
                    }
                    let next = ks.get(i + 1).copied().unwrap_or(end);
                    if next - t >= 4.0 {
                        hits.push(Note::new(
                            (t + ((next - t) / 2.0).floor()).min(end - 1.0),
                            1.0,
                            60,
                            0.9,
                        ));
                    }
                }
            } else if mode == "independent" {
                if let Some(g) = &plan.groove {
                    for bar in 0..sec.bars {
                        for s in &g.kick_spare {
                            let lo = (bar % 2) as f32 * 16.0;
                            if *s >= lo && *s < lo + 16.0 && rng.chance(0.5) {
                                hits.push(Note::new(bar as f32 * 16.0 + s - lo, 1.0, 60, 0.85));
                            }
                        }
                    }
                }
            }
            let bass = bass_notes(
                &cx,
                sec,
                style,
                pb.bass.octave,
                &hits,
                plan.knobs.glide,
                &mut rng,
            );
            pat.clips.insert(track_name("bass"), bass);
        }
        let chords = cx.chords_for(sec);
        if has("harmony") {
            let notes = if harmony_style == "drone" {
                // tanpura: root + fifth (+ upper root), re-struck every 2 bars;
                // an octave up when a bass/808 owns the low end
                let mut v = Vec::new();
                let lift = if roles.iter().any(|r| r == "bass") {
                    1
                } else {
                    0
                };
                let base = (pb.harmony.octave + 1 + lift) * 12 + key_pc as i32;
                let mut t = 0.0;
                while t < end {
                    for (k, d) in [0, 7, 12].iter().enumerate() {
                        v.push(Note::new(
                            t + k as f32 * 0.5,
                            32.0f32.min(end - t) - k as f32 * 0.5,
                            (base + d) as u8,
                            0.6 * mel,
                        ));
                    }
                    t += 32.0;
                }
                v
            } else {
                // with a bass/808 in the beat the chords voice open, above ~247 Hz
                let voiced = if roles.iter().any(|r| r == "bass") {
                    theory::voice_chords_open(chords, pb.harmony.octave, 59)
                } else {
                    theory::voice_chords(chords, pb.harmony.octave, true)
                };
                let style = if matches!(
                    sec.kind.as_str(),
                    "intro" | "outro" | "bridge" | "breakdown"
                ) {
                    "block"
                } else {
                    harmony_style.as_str()
                };
                theory::chord_notes(
                    &voiced,
                    plan.bars_per_chord,
                    sec.bars * STEPS_PER_BAR,
                    style,
                    0.7 * mel,
                )?
            };
            let mut notes = notes;
            if is_hook && harmony_style != "drone" {
                // hook layering: double each chord's top voice an octave up
                let mut tops: BTreeMap<i64, Note> = BTreeMap::new();
                for n in &notes {
                    let k = (n.start * 100.0) as i64;
                    if tops.get(&k).map(|t| n.pitch > t.pitch).unwrap_or(true) {
                        tops.insert(k, n.clone());
                    }
                }
                for (_, mut t) in tops {
                    t.pitch = (t.pitch + 12).min(108);
                    t.vel *= 0.45;
                    notes.push(t);
                }
            }
            pat.clips.insert(track_name("harmony"), notes);
        }
        let mut lead = Vec::new();
        if has("lead") {
            lead = lead_notes(&cx, sec, lead_oct, &ornament, &mut rng);
            if pb.mix.balance_genre == "lofi" {
                // critic v16/v17: the last hook's register lift put the lead
                // 1.7-2.3 dB hotter in 2-5 kHz than hook 1. A lo-fi lead is a
                // muffled keys line: it develops by rhythm, not by climbing.
                cap_register(&mut lead, LOFI_LEAD_CEILING);
            }
            for n in lead.iter_mut() {
                n.vel = (n.vel * mel).clamp(0.05, 1.0);
            }
            let mut layered = lead.clone();
            if is_hook {
                // hooks: an octave double underneath the lead for weight
                for n in &lead {
                    let mut d = n.clone();
                    d.pitch = d.pitch.saturating_sub(12);
                    d.vel *= 0.5;
                    d.slide_to = d.slide_to.map(|x| x.saturating_sub(12));
                    layered.push(d);
                }
            }
            pat.clips.insert(track_name("lead"), layered);
        }
        if has("counter") {
            let mut c = if plan.counter_mode == "motif_echo" && is_hook {
                motif_echo(&cx, sec, pb.counter.octave)
            } else {
                counter_notes(&cx, sec, pb.counter.octave, &lead)
            };
            for n in c.iter_mut() {
                n.vel *= mel;
            }
            if pb.mix.balance_genre == "lofi" {
                cap_register(&mut c, LOFI_LEAD_CEILING + 3);
            }
            pat.clips.insert(track_name("counter"), c);
        }
        if has("texture") {
            let tex_oct = pb.texture.as_ref().map(|t| t.octave).unwrap_or(4);
            let mut v = Vec::new();
            let span = (plan.bars_per_chord * 16.0).max(16.0);
            let mut t = 0.0;
            while t < end {
                let c = chord_at(chords, plan.bars_per_chord, t);
                let r = ((tex_oct + 1) * 12) as i32 + c.root_pc as i32;
                v.push(Note::new(t, span.min(end - t), r as u8, 0.45 * mel));
                v.push(Note::new(t, span.min(end - t), (r + 7) as u8, 0.4 * mel));
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
        // key change: everything pitched moves; drums stay
        if let Some(st) = plan.section_transpose.get(&sec.name) {
            for t in ["bass", "chords", "lead", "counter", "texture"] {
                if let Some(v) = pat.clips.get_mut(t) {
                    for n in v.iter_mut() {
                        n.pitch = (n.pitch as i32 + st).clamp(0, 127) as u8;
                        n.slide_to = n.slide_to.map(|x| (x as i32 + st).clamp(0, 127) as u8);
                    }
                }
            }
        }
        // a quiet section is played softer, not only thinner
        if sec.tags.iter().any(|t| t == "quiet") {
            for v in pat.clips.values_mut() {
                for n in v.iter_mut() {
                    n.vel *= 0.6;
                }
            }
        }
        // a beat of silence before the drop: everything stops
        if sec.transition == "silence" {
            for v in pat.clips.values_mut() {
                v.retain(|n| n.start < end - 4.0);
                for n in v.iter_mut() {
                    n.len = n.len.min(end - 4.0 - n.start).max(0.1);
                }
            }
        }
        // AI-authored MIDI replaces the generated part verbatim
        for (k, parts) in &plan.authored {
            if k != &sec.name && k != &sec.kind && k != "*" {
                continue;
            }
            for (role, notes) in parts {
                if roles.iter().any(|r| r == role) {
                    let v: Vec<Note> = notes
                        .iter()
                        .filter(|n| n.start >= 0.0 && n.start < end)
                        .cloned()
                        .collect();
                    pat.clips.insert(track_name(role), v);
                }
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

    // tracks that never play (an empty grammar variation) only add noise
    let silent: Vec<String> = e
        .project
        .tracks
        .iter()
        .filter(|t| {
            e.project
                .patterns
                .iter()
                .all(|p| p.notes(&t.name).is_empty())
        })
        .map(|t| t.name.clone())
        .collect();
    e.project.tracks.retain(|t| !silent.contains(&t.name));
    // ---- transition FX that mark the big moments: a downlifter into a quiet
    // section, a reverse cymbal swelling into the beat switch and an impact on its one
    if let Some(q) = plan.sections.iter().position(|s| s.tags.iter().any(|t| t == "quiet")) {
        if q >= 1 {
            let _ = e.call_from("add_transition", &json!({"into": plan.sections[q].name, "type": "downlifter"}), "producer");
        }
    }
    if let Some(w) = plan.sections.iter().position(|s| s.tags.iter().any(|t| t == "switch")) {
        if w >= 1 {
            let name = plan.sections[w].name.clone();
            let _ = e.call_from("add_transition", &json!({"into": name, "type": "reverse_cymbal", "bars": 1}), "producer");
            let _ = e.call_from("add_transition", &json!({"into": name, "type": "impact"}), "producer");
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
    let rs = pb.mix.reverb_send + plan.knobs.space_db;
    for (t, db) in [
        ("lead", rs),
        ("counter", rs - 2.0),
        // the harmony role's track is "chords" (track_name)
        ("chords", rs + 1.0),
        ("texture", rs),
        ("snare", rs - 6.0),
        ("tabla", rs - 4.0),
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
    for t in ["chords", "lead", "counter", "texture"] {
        if have(e, t) {
            e.call_from(
                "add_effect",
                &json!({"track": t, "type": "eq", "params": {"low_db": -9.0, "low_freq": 180.0}}),
                "producer",
            )?;
        }
    }
    if have(e, "bass") && have(e, "kick") {
        // the 808/bass starts on the kick in most of these grooves: duck it
        // for the kick's body so the two never stack in the sub (a floor
        // even when the genre plays it dry; the critic can deepen it)
        let follows = matches!(pb.bass.style.as_str(), "808_glide" | "follow_kick");
        let amount = plan.knobs.sidechain.max(if follows { 0.55 } else { 0.4 });
        let release = if follows { 140.0 } else { 110.0 };
        e.call_from("add_effect", &json!({"track": "bass", "type": "sidechain", "params": {"source": "kick", "amount": amount, "release_ms": release}}), "producer")?;
    }
    // hook lift: melodic beds play filtered in verses and open up in hooks,
    // with a one-bar sweep into each hook (the classic build)
    let spans: Vec<(f32, f32, &PlanSection)> = {
        let mut b = 0.0f32;
        plan.sections
            .iter()
            .map(|s| {
                let x = (b, b + s.bars as f32 * 4.0, s);
                b += s.bars as f32 * 4.0;
                x
            })
            .collect()
    };
    if plan.sections.iter().any(|s| s.kind == "hook") {
        for t in ["chords", "counter", "texture"] {
            if !have(e, t) {
                continue;
            }
            let r = e.call_from(
                "add_effect",
                &json!({"track": t, "type": "filter", "params": {"mode": "lowpass", "cutoff": 18000.0, "resonance": 0.15}}),
                "producer",
            )?;
            let idx = r["index"].as_u64().unwrap_or(0);
            let mut pts: Vec<Value> = Vec::new();
            for (k, (b0, b1, s)) in spans.iter().enumerate() {
                // a quiet section closes right down (~1 kHz): it has to read as quiet
                let quiet = s.tags.iter().any(|t| t == "quiet");
                let closed = if quiet { 1000.0 } else { 1400.0 + 4200.0 * s.energy.clamp(0.0, 1.0) };
                let v = if s.kind == "hook" { 18000.0 } else { closed };
                pts.push(json!({"beat": b0, "value": v, "curve": "step"}));
                let next_hook = spans
                    .get(k + 1)
                    .map(|x| x.2.kind == "hook")
                    .unwrap_or(false);
                if next_hook && s.kind != "hook" && b1 - b0 >= 8.0 {
                    pts.push(json!({"beat": b1 - 4.0, "value": v, "curve": "smooth"}));
                    pts.push(json!({"beat": b1 - 0.01, "value": 16000.0, "curve": "step"}));
                }
            }
            e.call_from(
                "add_automation",
                &json!({"track": t, "param": format!("fx.{idx}.cutoff"), "points": pts}),
                "producer",
            )?;
        }
    }
    if have(e, "chords")
        && plan
            .palette
            .get("harmony")
            .map(|h| h != "tanpura")
            .unwrap_or(true)
    {
        e.call_from(
            "add_effect",
            &json!({"track": "chords", "type": "width", "params": {"amount": 1.3, "low_mono_hz": 150.0}}),
            "producer",
        )?;
    }
    // plucked strings (sitar, santoor, guitar, koto) render mono; the critic
    // heard them as a narrow point in the middle (pluck A/B, beats 6b, 8).
    // A pseudo-stereo shaper widens them above 250 Hz-ish while the mid stays
    // put, so they sit beside the vocal instead of on top of it.
    let plucks: Vec<String> = e.project.tracks.iter().filter(|t| matches!(t.instrument, crate::instruments::Instrument::Pluck(_))).map(|t| t.name.clone()).collect();
    for t in plucks {
        e.call_from("add_effect", &json!({"track": t, "type": "stereo_shaper", "params": {"preset": "pseudo_stereo", "delay_ms": 9.0, "mix": 0.8}}), "producer")?;
    }
    let hiphop = matches!(pb.mix.balance_genre.as_str(), "boom_bap" | "hiphop" | "hip_hop");
    // hip-hop: hats and noise sit narrow (wide hiss reads as cheap), and the
    // low end of anything stereo stays mono
    let rnb = matches!(pb.mix.balance_genre.as_str(), "rnb");
    let lofi = matches!(pb.mix.balance_genre.as_str(), "lofi");
    if hiphop || rnb || lofi {
        for t in ["hat", "open_hat"] {
            if have(e, t) {
                e.call_from("add_effect", &json!({"track": t, "type": "width", "params": {"amount": 0.55}}), "producer")?;
                // dark top: the hats' fizz above 10 kHz read as hiss (critic, beats 6b and 8);
                // a flat Butterworth cut, not a resonant filter (critic, beat 8 r3:
                // the resonant low-pass rang at 8.3 kHz on every hat, 29 glassy ticks)
                e.call_from("add_effect", &json!({"track": t, "type": "parametric_eq", "params": {"bands": [{"kind": "high_cut", "freq": 10000.0, "gain_db": 0.0, "q": 0.707}]}}), "producer")?;
                if lofi {
                    // lo-fi hats are dusty, not crisp: closed above ~5 kHz
                    // (critic, beat 9: 5-16 kHz was all hat streaks; v16: the
                    // 7 kHz cut still left 5-7 kHz, so a 12 dB/oct cut at 5k)
                    e.call_from("add_effect", &json!({"track": t, "type": "parametric_eq", "params": {"bands": [
                        {"kind": "high_cut", "freq": 5000.0, "gain_db": 0.0, "q": 0.707},
                        {"kind": "high_shelf", "freq": 3500.0, "gain_db": -2.0, "q": 0.7}]}}), "producer")?;
                }
            }
        }
    }
    // lo-fi (critic, beat 9): perc and cymbals as dusty as the hats, and the
    // harmony bed thinned where it stacked (260-800 Hz, low-mid +5.4 dB)
    if lofi {
        for t in ["perc", "cymbal", "crash", "shaker"] {
            if have(e, t) {
                e.call_from("add_effect", &json!({"track": t, "type": "parametric_eq", "params": {"bands": [{"kind": "high_cut", "freq": 6000.0, "gain_db": 0.0, "q": 0.707}]}}), "producer")?;
            }
        }
        // a dusty snare (critique_mix owners, beat 9 r2: once the hats were
        // dark the snare made 60% of 2-5 kHz and 42% of the air)
        for t in ["snare", "clap", "rim"] {
            if have(e, t) {
                e.call_from("add_effect", &json!({"track": t, "type": "parametric_eq", "params": {"bands": [
                    {"kind": "high_cut", "freq": 4500.0, "gain_db": 0.0, "q": 0.707},
                    // critic v17: once 3-5k was tamed, 2-3 kHz became the
                    // hottest top band (mix share -16.2), so the dip sits at 2.5k
                    {"kind": "bell", "freq": 2500.0, "gain_db": -4.0, "q": 0.9}]}}), "producer")?;
            }
        }
        // the lead made 38% of 2-5 kHz and climbed in hook 2 (critic v16):
        // a lo-fi lead is a muffled keys line, closed above 3.5 kHz
        for t in ["lead", "counter"] {
            if have(e, t) {
                e.call_from("add_effect", &json!({"track": t, "type": "parametric_eq", "params": {"bands": [
                    {"kind": "high_cut", "freq": 3500.0, "gain_db": 0.0, "q": 0.707},
                    // critic v17: lead/counter still co-own 2-3 kHz with the snare
                    {"kind": "bell", "freq": 2500.0, "gain_db": -3.0, "q": 1.0}]}}), "producer")?;
            }
        }
        // the chords made 53% of 250-800 Hz: rootless up top, no mud under
        for t in ["chords", "keys", "pad", "texture", "counter"] {
            if have(e, t) {
                e.call_from("add_effect", &json!({"track": t, "type": "parametric_eq", "params": {"bands": [
                    {"kind": "low_cut", "freq": 180.0, "gain_db": 0.0, "q": 0.707},
                    {"kind": "bell", "freq": 350.0, "gain_db": -4.5, "q": 1.0},
                    {"kind": "bell", "freq": 600.0, "gain_db": -2.5, "q": 0.9}]}}), "producer")?;
            }
        }
    }
    // R&B (critic, beat 8 round 2: 2-5 kHz +8.9 dB over a reference and tonal,
    // 60-250 Hz 7.9 dB under): the tonal lead/counter lose 4 dB above 2.5 kHz,
    // perc and texture lose their fizz above 10 kHz, and the 808/kick get body
    if rnb {
        for t in ["lead", "counter"] {
            if have(e, t) {
                e.call_from("add_effect", &json!({"track": t, "type": "parametric_eq", "params": {"bands": [{"kind": "high_shelf", "freq": 2500.0, "gain_db": -4.0, "q": 0.7}]}}), "producer")?;
            }
        }
        // a glassy FM bell (ratio 3.5, index 4: inharmonic partials up to the
        // top) becomes a softer tine (harmonic 2:1, low index), Rhodes-like
        for t in ["lead", "counter"] {
            if let Ok(i) = e.project.track_index(t) {
                let mut v = serde_json::to_value(&e.project.tracks[i].instrument)?;
                if v["type"] == "fm" && v["index"].as_f64().unwrap_or(0.0) > 2.0 {
                    v["index"] = json!(1.6);
                    v["ratio"] = json!(2.0);
                    e.project.tracks[i].instrument = serde_json::from_value(v)?;
                }
            }
        }
        for (t, hz) in [("perc", 10000.0), ("texture", 9000.0), ("lead", 8000.0)] {
            if have(e, t) {
                e.call_from("add_effect", &json!({"track": t, "type": "parametric_eq", "params": {"bands": [{"kind": "high_cut", "freq": hz, "gain_db": 0.0, "q": 0.707}]}}), "producer")?;
            }
        }
        // hats: a gentle -6 dB shelf from 7 kHz instead of another cut
        for t in ["hat", "open_hat"] {
            if have(e, t) {
                e.call_from("add_effect", &json!({"track": t, "type": "parametric_eq", "params": {"bands": [{"kind": "high_shelf", "freq": 7000.0, "gain_db": -6.0, "q": 0.7}]}}), "producer")?;
            }
        }
        // the clap filled 800 Hz-2 kHz (+6.4 dB over the reference)
        if have(e, "perc") {
            e.call_from("add_effect", &json!({"track": "perc", "type": "parametric_eq", "params": {"bands": [{"kind": "bell", "freq": 1200.0, "gain_db": -3.0, "q": 1.0}]}}), "producer")?;
        }
        // the harmony bed was a dense stack at 300-800 Hz (low-mid +4.8 dB, the
        // band a voice lives in): a gentle static dip here, and produce_song
        // adds the vocal-keyed 300-900 Hz dip once there is a voice
        for t in ["chords", "texture"] {
            if have(e, t) {
                e.call_from("add_effect", &json!({"track": t, "type": "parametric_eq", "params": {"bands": [{"kind": "bell", "freq": 520.0, "gain_db": -2.5, "q": 0.8}, {"kind": "low_shelf", "freq": 150.0, "gain_db": 2.0, "q": 0.7}]}}), "producer")?;
            }
        }
        if have(e, "bass") {
            // the 808 was almost all sub (58% under 60 Hz): tape harmonics put it
            // into the 100-250 Hz range small speakers play
            e.call_from("add_effect", &json!({"track": "bass", "type": "saturator", "params": {"mode": "tape", "drive_db": 8.0, "mix": 0.4}}), "producer")?;
            e.call_from("add_effect", &json!({"track": "bass", "type": "parametric_eq", "params": {"bands": [{"kind": "bell", "freq": 110.0, "gain_db": 6.0, "q": 0.9}]}}), "producer")?;
        }
        if have(e, "kick") {
            e.call_from("add_effect", &json!({"track": "kick", "type": "parametric_eq", "params": {"bands": [{"kind": "bell", "freq": 75.0, "gain_db": 3.0, "q": 1.0}]}}), "producer")?;
        }
    }
    // a pocket for the voice: the melodic beds step back 3 dB around 1-2 kHz
    // (critic, 6b round 2: 800-2k ran ~8 dB over a hip-hop reference)
    for t in ["chords", "counter", "texture"] {
        if have(e, t) {
            e.call_from("add_effect", &json!({"track": t, "type": "parametric_eq", "params": {"bands": [{"kind": "bell", "freq": 1400.0, "gain_db": -3.0, "q": 0.9}]}}), "producer")?;
        }
    }
    // R&B and sad/melodic beds: the sustained harmony builds a 300 Hz band
    // under the vocal range (critic, beat 8)
    let soft = rnb || ["sad", "melancholic", "heartbreak", "romantic", "dreamy"].iter().any(|m| plan.mood.contains(m));
    if soft && have(e, "chords") {
        e.call_from("add_effect", &json!({"track": "chords", "type": "parametric_eq", "params": {"bands": [{"kind": "bell", "freq": 320.0, "gain_db": -5.0, "q": 1.0}]}}), "producer")?;
    }
    // ...and a held pad partial can ring through the whole song: find and notch it
    {
        let beds: Vec<&str> = ["chords", "texture"].into_iter().filter(|t| have(e, t)).collect();
        if !beds.is_empty() {
            let _ = e.call_from("notch_drones", &json!({"tracks": beds}), "producer");
        }
    }
    for t in ["texture", "counter"] {
        if have(e, t) {
            e.call_from("add_effect", &json!({"track": t, "type": "width", "params": {"amount": 1.0, "low_mono_hz": 150.0}}), "producer")?;
        }
    }
    // transition noise (risers, downlifters, reverse cymbals) above 8 kHz is hiss
    for t in ["downlifter", "cymbal_rev", "riser", "uplifter"] {
        if have(e, t) {
            e.call_from("add_effect", &json!({"track": t, "type": "filter", "params": {"mode": "lowpass", "cutoff": 8000.0, "resonance": 0.1}}), "producer")?;
        }
    }
    // section energy at the master: hooks are the loudest thing in the song.
    // Measured on our own renders: thinner parts alone left hooks only ~1 dB
    // over verses and a quiet section ~2 dB under; the targets are hooks
    // +3 dB over verses, intro/outro ~5 dB and quiet sections ~7 dB under.
    for s in &plan.sections {
        let quiet = s.tags.iter().any(|t| t == "quiet");
        // the beat switch is an event: it comes back at full level
        let switch = s.tags.iter().any(|t| t == "switch");
        let db = match s.kind.as_str() {
            _ if quiet => -7.0,
            _ if switch => 0.0,
            "intro" | "outro" => -5.0,
            "verse" | "bridge" | "pre" | "prechorus" => -3.0,
            "breakdown" => -5.0,
            _ => 0.0,
        };
        if db != 0.0 {
            let _ = e.call_from("set_section_mix", &json!({"section": s.name, "track": "master", "volume_db": db, "all_occurrences": true}), "producer");
        }
    }
    if pb.mix.crush {
        e.call_from("add_effect", &json!({"track": "master", "type": "bitcrush", "params": {"bits": 12.0, "downsample": 2, "mix": 0.25}}), "producer")?;
        // the crusher's sample-and-hold folds hash into 11-22 kHz; a dusty
        // record has no top there anyway (beat 9: air +11.6 dB over the reference)
        e.call_from("add_effect", &json!({"track": "master", "type": "parametric_eq", "params": {"bands": [
            {"kind": "high_cut", "freq": if lofi { 8000.0 } else { 10000.0 }, "gain_db": 0.0, "q": 0.707},
            {"kind": "high_shelf", "freq": 7000.0, "gain_db": -3.0, "q": 0.7}
        ]}}), "producer")?;
    }
    // arrangement-level EQ carve + the sound palette's own mix moves
    let mix_moves = if plan.sound_palette.is_empty() {
        crate::carve::carve(e, &[])
    } else {
        crate::palette::mix_moves(e, &plan.sound_palette)
    };
    Ok(json!({
        "sound_palette": plan.sound_palette,
        "mix_moves": mix_moves,
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

// ---------------------------------------------------------------- step timing

fn prof_store() -> &'static std::sync::Mutex<Vec<(String, f64)>> {
    static S: std::sync::OnceLock<std::sync::Mutex<Vec<(String, f64)>>> =
        std::sync::OnceLock::new();
    S.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Record how long a producer step took (ms since `t0`); drained per
/// iteration into the plan history so slow steps are visible.
static PROFILING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn prof(label: &str, t0: std::time::Instant) {
    if !PROFILING.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    if std::env::var_os("BEATBOX_PROFILE").is_some() {
        eprintln!("[prof] {label}: {ms:.0} ms");
    }
    if let Ok(mut v) = prof_store().lock() {
        v.push((label.to_string(), ms));
    }
}

fn prof_drain() -> Value {
    let mut m = serde_json::Map::new();
    if let Ok(mut v) = prof_store().lock() {
        for (k, ms) in v.drain(..) {
            let e = m.entry(k).or_insert(json!(0.0));
            *e = json!((e.as_f64().unwrap_or(0.0) + ms).round());
        }
    }
    Value::Object(m)
}

/// Gain staging + mastering for the plan (balance_mix then master_assistant).
pub fn mix_and_master(e: &mut Engine, plan: &Plan) -> Result<Value> {
    let pb = playbook(&plan.genre)?;
    let mut offsets: serde_json::Map<String, Value> = plan
        .knobs
        .offsets
        .iter()
        .filter(|(t, _)| e.project.track_index(t).is_ok())
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    // sustained beds read as "keys" by track name but fill the low mids like
    // pads: sit a pad-like harmony 4 dB under the keys target; the tabla
    // dayan rings at Sa (~260-350 Hz) and plays densely, so 4 dB under perc
    let padlike = |p: &str| {
        ["pad", "choir", "string", "tanpura", "drone", "organ"]
            .iter()
            .any(|w| p.contains(w))
    };
    let have = |t: &str| e.project.track_index(t).is_ok();
    if have("chords") && plan.palette.get("harmony").is_some_and(|h| padlike(h)) {
        offsets.entry("chords").or_insert(json!(-4.0));
    }
    if have("tabla") {
        offsets.entry("tabla").or_insert(json!(-4.0));
    }
    let t = std::time::Instant::now();
    let bal = e.call_from(
        "balance_mix",
        &json!({"genre": pb.mix.balance_genre, "iterations": 2, "offsets": offsets}),
        "producer",
    )?;
    prof("balance_mix", t);
    let t = std::time::Instant::now();
    let master = e.call_from("master_assistant", &json!({"target_lufs": plan.knobs.target_lufs, "style": pb.mix.master_style, "max_iterations": 3, "true_peak_ceiling": -1.6}), "producer")?;
    prof("master_assistant", t);
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
    let t = std::time::Instant::now();
    let c = compose(e, plan)?;
    prof("compose", t);
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
    /// ears_report render id of the critiqued render (feeds diff_renders).
    #[serde(default)]
    pub render_id: String,
    /// ears_report's own technical / musical scores (kept separate).
    #[serde(default)]
    pub ears: Value,
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
    let tonic_third = |k: &str| -> Option<(u8, bool)> {
        let mut it = k.split_whitespace();
        let root = theory::pitch_class(it.next()?).ok()?;
        let iv = theory::scale_intervals(it.next().unwrap_or("major")).ok()?;
        Some((root, iv.contains(&4)))
    };
    if let (Some(x), Some(y)) = (tonic_third(a), tonic_third(b)) {
        if x == y {
            return true;
        }
    }
    match (set(a), set(b)) {
        (Some(x), Some(y)) => {
            let common = x.iter().filter(|p| y.contains(p)).count();
            common + 1 >= x.len().min(y.len())
        }
        _ => false,
    }
}

pub fn critique(e: &mut Engine, plan: &Plan, reference: Option<&str>) -> Result<Critique> {
    critique_mode(e, plan, reference, false)
}

/// `critique` with the fast ears (`fast: true`, identical scores, no
/// display-only payload) for choosing between candidates in the produce
/// loop; see `listen::ears_report_mode`.
pub fn critique_mode(
    e: &mut Engine,
    plan: &Plan,
    reference: Option<&str>,
    fast: bool,
) -> Result<Critique> {
    let mut findings = Vec::new();
    let mut tech = 100.0f32;
    let mut mus = 100.0f32;
    // --- delivery QC
    let t = std::time::Instant::now();
    let cm = e.call_from(
        "check_master",
        &json!({"target_lufs": plan.knobs.target_lufs, "lufs_tolerance": 1.5}),
        "producer",
    )?;
    prof("check_master", t);
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
    let t = std::time::Instant::now();
    let art = e.call_from("detect_artifacts", &json!({}), "producer")?;
    prof("detect_artifacts", t);
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
    let t = std::time::Instant::now();
    let am = e.call_from("analyze_mix", &json!({}), "producer")?;
    prof("analyze_mix", t);
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
            json!({"action": "low_end", "sidechain": (plan.knobs.sidechain.max(0.55) + 0.15).min(0.85)})
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
    let t = std::time::Instant::now();
    let sec = e.call_from("analyze_sections", &json!({"top_tracks": 3}), "producer")?;
    prof("analyze_sections", t);
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
        // advisory only: wildcards (dropouts, half-time hooks) bend the curve on purpose
        findings.push(Finding { id: "energy_curve".into(), severity: 0.3, message: format!("measured loudness follows the planned energy curve weakly (rank corr {rho:.2}) - advisory"), fix: json!({"action": "none"}) });
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
        // guardrail: a dead hook (no lift at all); fixed with dynamics only,
        // never by changing which instruments play where
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
    // guardrail: a dead hook melody (under one note per bar); how busy a
    // verse is relative to the hook is taste, so it is only reported
    if dh < 1.0
        && plan
            .sections
            .iter()
            .any(|s| s.kind == "hook" && s.layers.iter().any(|l| l == "lead"))
    {
        mus -= 8.0;
        findings.push(Finding { id: "thin_hook".into(), severity: 0.5, message: format!("hook melody is thin ({dh:.1} notes/bar)"), fix: json!({"action": "density", "lead_density": (plan.knobs.lead_density + 0.15).min(1.0)}) });
    } else if dv > dh && dh > 0.0 {
        mus -= 5.0;
        findings.push(Finding {
            id: "busy_verse".into(),
            severity: 0.2,
            message: format!(
                "verse lead ({dv:.1}/bar) busier than the hook ({dh:.1}/bar) - advisory"
            ),
            fix: json!({"action": "none"}),
        });
    }
    // --- key: rendered notes should read as the planned key family
    let t = std::time::Instant::now();
    let dk = e.call_from("detect_key", &json!({}), "producer")?;
    prof("detect_key", t);
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
            message: format!("notes read as {found}, planned {want} - advisory (modal/borrowed harmony and key changes are intended)"),
            fix: json!({"action": "none"}),
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
    // --- the ears: ranked findings + masking, bound to a render id
    let t = std::time::Instant::now();
    let ears = crate::listen::ears_report_mode(e, None, fast)?;
    prof("ears_report", t);
    let render_id = ears["render_id"].as_str().unwrap_or("").to_string();
    let et = ears["scores"]["technical"].as_f64().unwrap_or(tech as f64) as f32;
    let em = ears["scores"]["musical"].as_f64().unwrap_or(mus as f64) as f32;
    for pm in ears["details"]["masking"]["pairs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|p| p["masking_db"].as_f64().unwrap_or(0.0) >= 4.0)
        .take(2)
    {
        let masker = pm["masker"].as_str().unwrap_or("").to_string();
        let maskee = pm["maskee"].as_str().unwrap_or("").to_string();
        let db = pm["masking_db"].as_f64().unwrap_or(0.0) as f32;
        let fix = match masker.as_str() {
            "kick" | "bass" if maskee == "kick" || maskee == "bass" => {
                json!({"action": "low_end", "sidechain": (plan.knobs.sidechain.max(0.55) + 0.15).min(0.85)})
            }
            "kick" | "snare" => json!({"action": "none"}),
            m => json!({"action": "offset", "track": m, "db": -1.5}),
        };
        findings.push(Finding {
            id: "masking".into(),
            severity: (db / 14.0).min(0.7),
            message: format!("'{masker}' masks '{maskee}' by {db:.1} dB"),
            fix,
        });
    }
    // blend: the producer's checks lead, the ears refine (both stay visible)
    let tech = (0.7 * tech + 0.3 * et).clamp(0.0, 100.0);
    let mus = (0.8 * mus + 0.2 * em).clamp(0.0, 100.0);
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
        render_id,
        ears: json!({"technical": et, "musical": em, "top_findings": ears["top_findings"].as_array().map(|v| v.iter().take(4).map(|f| f["message"].clone()).collect::<Vec<_>>())}),
    })
}

/// Apply the critic's fixes to the plan. Returns what changed; an empty
/// list means nothing actionable was left.
pub fn revise(plan: &mut Plan, c: &Critique, max_fixes: usize) -> Vec<String> {
    revise_skipping(plan, c, max_fixes, &Default::default())
        .into_iter()
        .map(|x| x.1)
        .collect()
}

/// Key of a fix: action + track (what a rejected revision is remembered by).
pub fn fix_key(f: &Finding) -> String {
    format!(
        "{}:{}",
        f.fix["action"].as_str().unwrap_or("none"),
        f.fix["track"].as_str().unwrap_or("")
    )
}

/// `revise`, skipping fixes whose key is in `skip` (tried and rejected by
/// the ears). Returns (fix key, description) per applied change.
pub fn revise_skipping(
    plan: &mut Plan,
    c: &Critique,
    max_fixes: usize,
    skip: &std::collections::HashSet<String>,
) -> Vec<(String, String)> {
    let mut done: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for f in &c.findings {
        if done.len() >= max_fixes {
            break;
        }
        let action = f.fix["action"].as_str().unwrap_or("none");
        let key = fix_key(f);
        if action == "none" || skip.contains(&key) || !seen.insert(key.clone()) {
            continue;
        }
        let mut done_s: Vec<String> = Vec::new();
        let k = &mut plan.knobs;
        match action {
            "remaster" => {
                if let Some(t) = f.fix["target_lufs"].as_f64() {
                    k.target_lufs = t as f32;
                }
                done_s.push(format!("{}: re-master at {:.1} LUFS", f.id, k.target_lufs));
            }
            "low_end" => {
                let s = f.fix["sidechain"].as_f64().unwrap_or(0.5) as f32;
                if s > k.sidechain + 0.01 {
                    k.sidechain = s;
                    done_s.push(format!(
                        "{}: sidechain 808/bass to the kick at {s:.2}",
                        f.id
                    ));
                } else {
                    let o = k.offsets.entry("bass".into()).or_insert(0.0);
                    *o -= 1.5;
                    done_s.push(format!("{}: bass -1.5 dB", f.id));
                }
            }
            "offset" => {
                let t = f.fix["track"].as_str().unwrap_or("").to_string();
                let db = f.fix["db"].as_f64().unwrap_or(0.0) as f32;
                // capped: a guardrail against masking/burying, not a mix template
                let o = k.offsets.entry(t.clone()).or_insert(0.0);
                *o = (*o + db).clamp(-3.0, 3.0);
                done_s.push(format!("{}: {t} {db:+.1} dB vs genre target", f.id));
            }
            "contrast" => {
                let v = f.fix["verse_velocity"].as_f64().unwrap_or(0.85) as f32;
                if v < k.verse_velocity - 0.01 {
                    // dynamics only: the critic never rewrites which parts play
                    // where (that pulled every beat toward one arrangement)
                    k.verse_velocity = v;
                    done_s.push(format!("{}: verse velocity {v:.2} (dynamics only)", f.id));
                }
            }
            "vary" => {
                k.variation_seed += 1;
                done_s.push(format!(
                    "{}: develop the motif differently (variation {})",
                    f.id, k.variation_seed
                ));
            }
            "density" => {
                let d = f.fix["lead_density"].as_f64().unwrap_or(0.5) as f32;
                // only ever rescues a dead hook, and only up to a modest density
                k.lead_density = d.max(k.lead_density).min(0.7);
                done_s.push(format!("{}: lead density {d:.2}", f.id));
            }
            "tonic" => {
                if !k.tonic_anchor {
                    k.tonic_anchor = true;
                    done_s.push(format!(
                        "{}: anchor the tonic (bass resolves home in the last section)",
                        f.id
                    ));
                }
            }
            "declick" if k.humanize > 0.0 => {
                k.humanize = 0.0;
                done_s.push(format!(
                    "{}: remove timing humanize (overlapping retriggers)",
                    f.id
                ));
            }
            _ => {}
        }
        for m in done_s {
            done.push((key.clone(), m));
        }
    }
    done
}

// ---------------------------------------------------------------- produce

/// Hard cap on critic iterations per produce_track call.
pub const MAX_ITERATIONS: usize = 4;
/// A kept revision must raise the best score by more than this to count as
/// progress; two iterations in a row without progress end the loop.
pub const PLATEAU_GAIN: f32 = 0.2;

pub struct ProduceOpts {
    pub max_iterations: usize,
    pub reference: Option<String>,
    pub out_dir: Option<std::path::PathBuf>,
    pub mp3: bool,
    /// Novelty check against recent outputs (None = off).
    pub novelty: Option<crate::novelty::NoveltyOpts>,
}

/// Fetch the plan's sample kit (falls back to the synth kit offline) and
/// record the files in the plan's asset manifest.
fn prepare_kit(e: &mut Engine, plan: &mut Plan) {
    if let Some(k) = plan.sample_kit.clone() {
        match crate::sample_lib::install_kit(e, &k) {
            Ok(files) => {
                plan.thinking.push(format!(
                    "Drums: real {k} samples (public domain/CC0, credits kept in the project)."
                ));
                let lic = crate::sample_lib::kit(&k).map(|x| x.license).unwrap_or("");
                if plan.provenance.is_object() {
                    plan.provenance["assets"]["sample_files"] = json!(files
                        .iter()
                        .map(|(r, p)| format!(
                            "{r}={}",
                            p.file_name()
                                .map(|x| x.to_string_lossy().to_string())
                                .unwrap_or_default()
                        ))
                        .collect::<Vec<_>>());
                    plan.provenance["assets"]["sample_license"] = json!(lic);
                }
            }
            Err(err) => {
                plan.thinking.push(format!(
                    "Drums: synth kit ({k} samples unavailable: {err})."
                ));
                plan.sample_kit = None;
            }
        }
    }
}

pub fn produce(e: &mut Engine, args: &PlanArgs, o: &ProduceOpts) -> Result<Value> {
    let t0 = std::time::Instant::now();
    // gain staging + mastering re-render with only faders/master moved:
    // synthesise each track once per composition (dropped when we return)
    let prev_cache = e.track_cache.replace(Default::default());
    let prev_mask = e.mask_cache.replace(Default::default());
    PROFILING.store(true, std::sync::atomic::Ordering::Relaxed);
    let r = produce_inner(e, args, o, t0);
    PROFILING.store(false, std::sync::atomic::Ordering::Relaxed);
    prof_drain();
    e.track_cache = prev_cache;
    e.mask_cache = prev_mask;
    r
}

fn produce_inner(
    e: &mut Engine,
    args: &PlanArgs,
    o: &ProduceOpts,
    t0: std::time::Instant,
) -> Result<Value> {
    let mut plan = plan_track(args)?;
    prepare_kit(e, &mut plan);
    // novelty: a candidate too close to a recent beat is regenerated with a
    // new seed (fresh seeds only: an explicit seed must reproduce exactly)
    let mut nov_hist = Vec::new();
    let mut nov_path = None;
    if let Some(no) = o.novelty.as_ref().filter(|n| n.enabled) {
        let hp = crate::novelty::history_path(e, no);
        nov_hist = crate::novelty::load_history(&hp, no.window);
        nov_path = Some(hp);
        let regenerate = args.seed_source == "entropy";
        let check = |e: &mut Engine, plan: &Plan| -> Result<(f32, Value)> {
            compose(e, plan)?;
            let mut f = crate::novelty::fingerprint(&e.project);
            f.genre = plan.genre.clone();
            Ok(match crate::novelty::nearest(&f, &nov_hist) {
                Some((h, d)) => (
                    d.total,
                    json!({"label": h.label, "seed": h.seed, "genre": h.genre, "distance": crate::novelty::round_d(&d)}),
                ),
                None => (1.0, Value::Null),
            })
        };
        let (mut best_d, mut best_near) = check(e, &plan)?;
        let mut rejected = Vec::new();
        let mut attempt = 0;
        while regenerate
            && best_d < no.threshold
            && attempt < no.max_attempts
            && !nov_hist.is_empty()
        {
            attempt += 1;
            let mut a2 = args.clone();
            a2.seed = crate::creative::splitmix(args.seed.wrapping_add(attempt as u64))
                & ((1u64 << 53) - 1);
            let mut p2 = plan_track(&a2)?;
            prepare_kit(e, &mut p2);
            let (d2, n2) = check(e, &p2)?;
            if d2 > best_d {
                rejected.push(json!({"seed": plan.seed, "distance": best_d, "nearest": best_near}));
                plan = p2;
                best_d = d2;
                best_near = n2;
            } else {
                rejected.push(json!({"seed": p2.seed, "distance": d2, "nearest": n2}));
            }
        }
        plan.novelty = json!({
            "threshold": no.threshold, "history_entries": nov_hist.len(),
            "nearest_distance": if nov_hist.is_empty() { Value::Null } else { json!((best_d * 1000.0).round() / 1000.0) },
            "nearest": best_near, "regenerated": rejected,
            "accepted": nov_hist.is_empty() || best_d >= no.threshold,
            "mode": if regenerate { "regenerate near-repeats" } else { "report only (explicit seed)" },
        });
        if !rejected.is_empty() {
            plan.thinking.push(format!("Novelty: {} candidate(s) were too close to recent beats and were regenerated; kept seed {} at distance {best_d:.3}.", rejected.len(), plan.seed));
        }
    }
    // critic loop: render -> critique (+ears) -> diff_renders against the
    // best render so far -> keep the revision only if the ears agree it is
    // better, otherwise roll back and try the next fix
    let mut log: Vec<Value> = Vec::new();
    let mut best: Option<(f32, Project, Plan, Critique)> = None;
    let mut best_mix = None;
    let mut rejected: std::collections::HashSet<String> = Default::default();
    let mut pending: Vec<String> = Vec::new();
    let mut misses = 0;
    // the critic loop is capped; it also stops early on a plateau (two
    // iterations in a row that did not raise the best score)
    let max_it = o.max_iterations.clamp(1, MAX_ITERATIONS);
    for it in 0..max_it {
        let ti = std::time::Instant::now();
        prof_drain();
        apply_plan(e, &plan)?;
        let c = critique_mode(e, &plan, o.reference.as_deref(), true)?;
        let tc = std::time::Instant::now();
        let mut entry = json!({
            "iteration": it + 1, "render_id": c.render_id, "score": c.score, "technical": c.technical, "musical": c.musical,
            "ears": {"technical": c.ears["technical"], "musical": c.ears["musical"]}, "reference": c.reference,
            "top_findings": c.findings.iter().take(5).map(|f| f.message.clone()).collect::<Vec<_>>(),
        });
        let (accepted, verdict) = match &best {
            None => (true, "baseline".to_string()),
            Some(b) => {
                let d = match (
                    crate::listen::recall(&b.3.render_id),
                    crate::listen::recall(&c.render_id),
                ) {
                    (Some(x), Some(y)) => crate::listen::diff(&x, &y),
                    _ => json!({"verdict": "unknown"}),
                };
                let v = d["verdict"].as_str().unwrap_or("unknown").to_string();
                entry["diff"] = json!({"vs": b.3.render_id, "verdict": v, "improvements": d["improvements"], "regressions": d["regressions"]});
                let ok = match v.as_str() {
                    "better" => c.score >= b.0 - 0.5,
                    "worse" => false,
                    _ => c.score > b.0 + 0.3,
                };
                (ok, v)
            }
        };
        entry["accepted"] = json!(accepted);
        entry["verdict"] = json!(verdict);
        prof("diff", tc);
        let gained = best.as_ref().is_none_or(|b| c.score > b.0 + PLATEAU_GAIN);
        if accepted {
            misses = if gained { 0 } else { misses + 1 };
            best = Some((c.score, e.project.clone(), plan.clone(), c.clone()));
            best_mix = e.mix().ok();
        } else {
            misses += 1;
            // roll back and never retry the fixes that made it worse
            for k in pending.drain(..) {
                rejected.insert(k);
            }
            if let Some(b) = &best {
                plan = b.2.clone();
            }
        }
        if it + 1 < max_it && misses < 2 {
            let base = best.as_ref().map(|b| b.3.clone()).unwrap_or(c);
            let changes = revise_skipping(&mut plan, &base, 2, &rejected);
            pending = changes.iter().map(|x| x.0.clone()).collect();
            entry["revisions"] = json!(changes.iter().map(|x| x.1.clone()).collect::<Vec<_>>());
            entry["seconds"] = json!((ti.elapsed().as_secs_f32() * 10.0).round() / 10.0);
            entry["ms"] = json!(ti.elapsed().as_millis() as u64);
            entry["ms_by_step"] = prof_drain();
            log.push(entry);
            if changes.is_empty() {
                break;
            }
        } else {
            entry["seconds"] = json!((ti.elapsed().as_secs_f32() * 10.0).round() / 10.0);
            entry["ms"] = json!(ti.elapsed().as_millis() as u64);
            entry["ms_by_step"] = prof_drain();
            log.push(entry);
            break;
        }
    }
    let (fast_score, proj, mut plan, _) = best.ok_or_else(|| anyhow!("no iteration ran"))?;
    if let Some(m) = best_mix {
        e.prime_render(&proj, m);
    }
    e.replace_project(proj);
    e.revision += 1;
    // the kept render always gets the full ears (same render, from the cache)
    let tf = std::time::Instant::now();
    let crit = critique_mode(e, &plan, o.reference.as_deref(), false)?;
    let score = crit.score;
    let final_review = json!({
        "render_id": crit.render_id, "ears_mode": "full",
        "score": crit.score, "technical": crit.technical, "musical": crit.musical,
        "fast_score": fast_score, "fast_vs_full_delta": ((crit.score - fast_score) * 10.0).round() / 10.0,
        "ms": tf.elapsed().as_millis() as u64, "ms_by_step": prof_drain(),
    });
    plan.history = log.clone();
    plan.final_review = final_review.clone();
    // the delivered beat joins the novelty history (with its audio summary)
    let mut novelty_out = plan.novelty.clone();
    if let (Some(no), Some(hp)) = (o.novelty.as_ref().filter(|n| n.enabled), &nov_path) {
        let mut f = crate::novelty::fingerprint(&e.project);
        f.genre = plan.genre.clone();
        f.seed = Some(plan.seed);
        f.label = format!("{}_{}", plan.genre, plan.seed);
        f.created_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Ok(m) = e.mix() {
            let (c, s) = crate::novelty::audio_summary(&m.left, &m.right, f.key_pc);
            f.chroma = c;
            f.spectrum = s;
        }
        if let Some((h, d)) = crate::novelty::nearest(&f, &nov_hist) {
            novelty_out["final_nearest"] =
                json!({"label": h.label, "seed": h.seed, "distance": crate::novelty::round_d(&d)});
        }
        if no.record {
            crate::novelty::append_history(hp, &f)?;
            novelty_out["history"] = json!(hp.to_string_lossy());
        }
        novelty_out["fingerprint"] = crate::novelty::summary(&f);
    }
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
        let ex = e.call_from("export_audio", &json!({"path": audio.to_string_lossy(), "format": fmt, "target_lufs": plan.knobs.target_lufs, "true_peak_ceiling": -1.2}), "producer")?;
        files["audio"] = json!(audio.to_string_lossy());
        files["export"] = compact(&ex, 700);
    }
    Ok(json!({
        "title": plan.title,
        "genre": plan.genre, "key": format!("{} {}", plan.key, plan.scale), "bpm": plan.bpm,
        "score": score, "score_kind": "diagnostic heuristic, not musical quality (see quality)", "technical": crit.technical, "musical": crit.musical, "reference_match": crit.reference,
        "iterations": log, "final_review": final_review, "thinking": plan.thinking, "plan": plan,
        "remaining_findings": crit.findings.iter().take(6).map(|f| f.message.clone()).collect::<Vec<_>>(),
        "measurements": crit.measurements,
        "seed": plan.seed,
        "constraints": {
            "applied": plan.constraints.iter().filter(|c| c.status != "ignored").collect::<Vec<_>>(),
            "ignored": plan.constraints.iter().filter(|c| c.status == "ignored").collect::<Vec<_>>(),
        },
        "diagnostics": {
            "note": "Technical and musical numbers are heuristic diagnostics (delivery gates and pointers), not a measure of musical quality. Quality is judged by blind A/B listening (blind_ab_create / blind_ab_rate), tracked separately.",
            "score": score, "technical": crit.technical, "musical": crit.musical,
        },
        "quality": {"measure": "blind A/B listener preference", "status": "unrated", "how": "blind_ab_create with this render and another, then blind_ab_rate; blind_ab_preferences shows the running tally"},
        "method": plan.method,
        "direction": plan.direction,
        "wildcards": plan.wildcards,
        "groove": plan.groove.as_ref().map(|g| g.summary.clone()),
        "progressions": {"verse": plan.progression_verse, "hook": plan.progression_hook, "bridge": plan.progression_bridge, "color": plan.harmony_color, "bars_per_chord": plan.bars_per_chord},
        "sections": plan.sections.iter().map(|s| json!({"name": s.name, "bars": s.bars, "development": s.development, "tags": s.tags, "transition": s.transition})).collect::<Vec<_>>(),
        "novelty": novelty_out,
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
            seed_source: "explicit".into(),
            ..Default::default()
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
    fn genre_guessing_and_flat_two_reference() {
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
    fn producer_picks_the_genre_sound_palette_and_records_it() {
        for (genre, pal) in [
            ("trap", "dark_trap"),
            ("drill", "drill_slide"),
            ("desi_hiphop", "dhh_grit"),
            ("boom_bap", "boom_bap_dusty"),
        ] {
            let plan = plan_track(&args("a beat", Some(genre), 5)).unwrap();
            assert_eq!(plan.sound_palette, pal, "{genre}");
            assert_eq!(
                plan.provenance["assets"]["sound_palette"].as_str(),
                Some(pal),
                "{genre}: the manifest names the palette"
            );
            assert!(
                plan.decisions.iter().any(|d| d.what == "sound palette"),
                "{genre}: the choice is a recorded decision"
            );
            // the bass always takes the palette voice (the variation step never swaps it)
            let p = crate::palette::get(pal).unwrap();
            let bass = p.voices.iter().find(|(r, _)| *r == "bass").unwrap().1;
            assert_eq!(
                plan.palette.get("bass").map(String::as_str),
                Some(bass),
                "{genre}"
            );
            // the plan round-trips with the palette name
            let v = serde_json::to_value(&plan).unwrap();
            assert_eq!(v["sound_palette"], pal);
            assert_eq!(plan_from_value(&v).unwrap().sound_palette, pal);
        }
        // compose applies it: the palette's mix moves run (EQ carve on the beds)
        let mut e = engine();
        let plan = plan_track(&args("dark drill", Some("drill"), 9)).unwrap();
        let out = compose(&mut e, &plan).unwrap();
        assert_eq!(out["sound_palette"], "drill_slide");
        let i = e.project.track_index("hat").unwrap();
        assert!(e.project.tracks[i]
            .effects
            .iter()
            .any(|x| x.id() == crate::carve::CARVE_ID));
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
            render_id: String::new(),
            ears: json!({}),
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
                novelty: None,
            },
        )
        .unwrap();
        assert!(r["score"].as_f64().unwrap() > 0.0);
        assert!(!r["iterations"].as_array().unwrap().is_empty());
        assert!(e.project.tracks.len() >= 5);
        assert!(r["measurements"]["integrated_lufs"].as_f64().unwrap() > -30.0);
        // the critic loop is recorded in the plan: render ids, verdicts, kept or not
        let h = r["plan"]["history"].as_array().unwrap();
        assert_eq!(h.len(), r["iterations"].as_array().unwrap().len());
        assert_eq!(h[0]["verdict"], "baseline");
        assert!(h[0]["render_id"].as_str().unwrap().starts_with('r'));
        assert!(h.iter().all(|x| x["accepted"].is_boolean()));
        if h.len() > 1 {
            assert!(h[1]["diff"]["verdict"].is_string());
        }
        // every iteration records its time; the kept render got the full ears
        assert!(h.iter().all(|x| x["ms"].is_u64()));
        let fr = &r["final_review"];
        assert_eq!(fr["ears_mode"], "full");
        assert_eq!(fr["score"], r["score"]);
        assert!(
            fr["fast_vs_full_delta"].as_f64().unwrap().abs() <= 1.0,
            "{fr}"
        );
        assert_eq!(r["plan"]["final_review"]["ears_mode"], "full");
    }

    #[test]
    fn rejected_fixes_are_not_retried() {
        let mut plan = plan_track(&args("trap", Some("trap"), 1)).unwrap();
        let f = Finding {
            id: "mix".into(),
            severity: 0.3,
            message: "z".into(),
            fix: json!({"action": "offset", "track": "hat", "db": 2.0}),
        };
        let c = Critique {
            score: 60.0,
            technical: 70.0,
            musical: 50.0,
            reference: None,
            measurements: json!({}),
            render_id: String::new(),
            ears: json!({}),
            findings: vec![f.clone()],
        };
        let skip: std::collections::HashSet<String> = [fix_key(&f)].into_iter().collect();
        assert!(revise_skipping(&mut plan, &c, 3, &skip).is_empty());
        let done = revise_skipping(&mut plan, &c, 3, &Default::default());
        assert_eq!(done[0].0, "offset:hat");
    }

    fn fp_of(seed: u64, genre: &str) -> (Plan, crate::novelty::Fingerprint) {
        let mut e = engine();
        let mut a = args(&format!("{genre} beat"), Some(genre), seed);
        a.duration_s = None;
        let plan = plan_track(&a).unwrap();
        compose(&mut e, &plan).unwrap();
        let mut f = crate::novelty::fingerprint(&e.project);
        f.genre = plan.genre.clone();
        (plan, f)
    }

    #[test]
    fn same_seed_gives_identical_output() {
        let mut a = args("dark trap", Some("trap"), 4242);
        a.duration_s = None;
        let p1 = plan_track(&a).unwrap();
        let p2 = plan_track(&a).unwrap();
        assert_eq!(p1, p2);
        let (mut e1, mut e2) = (engine(), engine());
        compose(&mut e1, &p1).unwrap();
        compose(&mut e2, &p2).unwrap();
        assert_eq!(
            serde_json::to_string(&e1.project.patterns).unwrap(),
            serde_json::to_string(&e2.project.patterns).unwrap()
        );
        assert_eq!(p1.provenance["seed"], 4242);
        assert_eq!(
            p1.provenance["generator_version"],
            crate::creative::GENERATOR_VERSION
        );
    }

    #[test]
    fn different_seeds_give_clearly_different_beats() {
        // procedural beats of one genre: drum grids, harmony and section maps must differ
        let seeds = [101u64, 202, 303, 404, 505];
        let fps: Vec<(Plan, crate::novelty::Fingerprint)> =
            seeds.iter().map(|s| fp_of(*s, "trap")).collect();
        let (mut r, mut h, mut ar, mut n) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for i in 0..fps.len() {
            for j in (i + 1)..fps.len() {
                let d = crate::novelty::distance(&fps[i].1, &fps[j].1);
                assert!(
                    d.rhythm.unwrap() > 0.1,
                    "drum grids too close: {} vs {}: {:?}",
                    seeds[i],
                    seeds[j],
                    d
                );
                assert!(
                    d.total > 0.15,
                    "beats too close: {} vs {}: {:?}",
                    seeds[i],
                    seeds[j],
                    d
                );
                assert_ne!(fps[i].1.rhythm.get("kick"), fps[j].1.rhythm.get("kick"));
                r += d.rhythm.unwrap();
                h += d.harmony.unwrap_or(0.0);
                ar += d.arrangement.unwrap_or(0.0);
                n += 1.0;
            }
        }
        assert!(r / n > 0.25, "mean drum distance {}", r / n);
        assert!(h / n > 0.15, "mean harmony distance {}", h / n);
        assert!(ar / n > 0.15, "mean section-map distance {}", ar / n);
        // and the plans say why: direction, decisions, wildcards with roles
        for (p, _) in &fps {
            assert!(p.direction.is_some());
            assert!(p.decisions.len() > 8);
            assert!(!p.wildcards.is_empty() && p.wildcards.len() <= 3);
            assert!(p
                .wildcards
                .iter()
                .all(|w| ["tension", "release", "contrast", "groove", "emotion"]
                    .contains(&w.role.as_str())));
            assert!(p.sections.iter().all(|s| !s.development.is_empty()));
        }
        let progs: std::collections::BTreeSet<String> = fps
            .iter()
            .map(|(p, _)| format!("{}|{}", p.progression_verse, p.progression_hook))
            .collect();
        assert!(progs.len() >= 3, "progressions barely vary: {progs:?}");
        let maps: std::collections::BTreeSet<String> = fps
            .iter()
            .map(|(_, f)| format!("{:?}", f.sections))
            .collect();
        assert!(maps.len() >= 4, "section maps barely vary: {maps:?}");
    }

    #[test]
    fn templates_remain_a_starting_point() {
        let mut a = args("trap", Some("trap"), 9);
        a.method = Some("template".into());
        let p = plan_track(&a).unwrap();
        assert_eq!(p.method, "template");
        assert_eq!(p.groove.as_ref().unwrap().source, "template");
        assert_eq!(
            p.sections.len(),
            playbook("trap")
                .unwrap()
                .arrangement
                .len()
                .min(p.sections.len())
        );
        a.method = Some("bogus".into());
        assert!(plan_track(&a).is_err());
    }

    #[test]
    fn motif_development_keeps_the_idea() {
        let m = vec![
            MotifNote {
                t: 0.0,
                deg: 0,
                len: 2.0,
            },
            MotifNote {
                t: 4.0,
                deg: 2,
                len: 2.0,
            },
            MotifNote {
                t: 8.0,
                deg: 4,
                len: 2.0,
            },
            MotifNote {
                t: 12.0,
                deg: 2,
                len: 2.0,
            },
        ];
        let mut rng = Rng::new(1);
        let (inv, _) = crate::creative::develop(&m, &["inversion".to_string()], &mut rng);
        assert_eq!(
            inv.iter().map(|x| x.deg).collect::<Vec<_>>(),
            vec![0, -2, -4, -2]
        );
        let (frag, _) = crate::creative::develop(&m, &["fragment".to_string()], &mut rng);
        // the first half, repeated in the second half of the bar
        assert_eq!(
            frag.iter().map(|x| x.deg).collect::<Vec<_>>(),
            vec![0, 2, 0, 2]
        );
        assert_eq!(frag[2].t, 8.0);
        let (_, oct) = crate::creative::develop(&m, &["register_up".to_string()], &mut rng);
        assert_eq!(oct, 1);
        let (disp, _) = crate::creative::develop(&m, &["displace".to_string()], &mut rng);
        assert_eq!(disp.len(), 4);
        assert_ne!(
            disp.iter().map(|x| x.t).collect::<Vec<_>>(),
            m.iter().map(|x| x.t).collect::<Vec<_>>()
        );
    }

    #[test]
    fn authored_midi_is_played_verbatim() {
        let mut e = engine();
        let mut a = args("trap", Some("trap"), 77);
        a.duration_s = None;
        let notes = vec![Note::new(0.0, 4.0, 72, 0.9), Note::new(8.0, 4.0, 75, 0.9)];
        a.authored
            .entry("hook".into())
            .or_default()
            .insert("lead".into(), notes.clone());
        let plan = plan_track(&a).unwrap();
        assert_eq!(plan.method, "ai_authored");
        compose(&mut e, &plan).unwrap();
        let h = &e.project.patterns[e.project.pattern_index("hook1").unwrap()];
        let lead = h.notes("lead");
        assert_eq!(lead.len(), 2, "{lead:?}");
        assert_eq!(lead[1].pitch, 75);
    }

    #[test]
    fn critic_never_rewrites_musical_content() {
        let mut plan = plan_track(&args("trap", Some("trap"), 1)).unwrap();
        let before: Vec<Vec<String>> = plan.sections.iter().map(|s| s.layers.clone()).collect();
        let c = Critique {
            score: 60.0,
            technical: 70.0,
            musical: 50.0,
            reference: None,
            measurements: json!({}),
            render_id: String::new(),
            ears: json!({}),
            findings: vec![
                Finding {
                    id: "hook_lift".into(),
                    severity: 0.8,
                    message: "x".into(),
                    fix: json!({"action": "contrast", "verse_velocity": 0.7}),
                },
                Finding {
                    id: "mix".into(),
                    severity: 0.3,
                    message: "z".into(),
                    fix: json!({"action": "offset", "track": "hat", "db": 9.0}),
                },
            ],
        };
        revise(&mut plan, &c, 3);
        let after: Vec<Vec<String>> = plan.sections.iter().map(|s| s.layers.clone()).collect();
        assert_eq!(before, after, "layers changed by the critic");
        assert!(plan.knobs.offsets["hat"] <= 3.0);
    }

    #[test]
    fn fresh_seeds_differ() {
        let a = crate::creative::fresh_seed();
        let b = crate::creative::fresh_seed();
        assert_ne!(a, b);
        assert!(a < (1u64 << 53));
    }

    #[test]
    fn omitted_seed_is_fresh_and_returned_explicit_is_deterministic() {
        let mut e = engine();
        let a = e
            .call("plan_track", &json!({"genre": "trap", "brief": "x"}))
            .unwrap();
        let b = e
            .call("plan_track", &json!({"genre": "trap", "brief": "x"}))
            .unwrap();
        assert_ne!(a["seed"], b["seed"]);
        assert_eq!(a["seed_source"], "fresh");
        assert_eq!(a["plan"]["provenance"]["seed_source"], "entropy");
        let c = e
            .call(
                "plan_track",
                &json!({"genre": "trap", "brief": "x", "seed": 5}),
            )
            .unwrap();
        let d = e
            .call(
                "plan_track",
                &json!({"genre": "trap", "brief": "x", "seed": 5}),
            )
            .unwrap();
        assert_eq!(c["plan"], d["plan"]);
        assert_eq!(c["seed"], 5);
        assert_eq!(c["plan"]["provenance"]["seed_source"], "explicit");
        assert!(c.get("seed_source").is_none());
    }

    #[test]
    fn structured_intent_is_applied_and_reported() {
        let mut a = args("beat", Some("trap"), 31);
        a.duration_s = None;
        a.intent = json!({
            "mood": "sad", "energy": 0.4, "hero": "bass", "density": "sparse",
            "rhythmic_feel": ["triplet", "straight"],
            "motif": {"contour": "ascending", "rhythm": "syncopated"},
            "palette": {"lead": "koto", "snare": "rim", "lead2": "koto"},
            "contrasts": ["key_change", "nonsense"],
            "vibe": "x"
        });
        let p = plan_track(&a).unwrap();
        assert_eq!(p.mood, "sad");
        let d = p.direction.as_ref().unwrap();
        assert!((d.energy - 0.4).abs() < 1e-6);
        assert_eq!(d.hero, "bass");
        assert_eq!(d.density, "sparse");
        assert_eq!(p.method, "procedural");
        assert_eq!(p.groove.as_ref().unwrap().hat_rate, "8t");
        assert_eq!(p.swing, 0.0);
        assert_eq!(p.palette["lead"], "koto");
        assert_eq!(p.palette["snare"], "rim");
        assert_eq!(p.wildcards.len(), 1);
        assert_eq!(p.wildcards[0].name, "key_change");
        assert!(p
            .decisions
            .iter()
            .any(|x| x.what == "motif" && x.choice.contains("ascending contour, syncopated")));
        let ignored: Vec<&str> = p
            .constraints
            .iter()
            .filter(|c| c.status == "ignored")
            .map(|c| c.field.as_str())
            .collect();
        assert!(ignored.contains(&"intent.vibe"), "{ignored:?}");
        assert!(ignored.contains(&"intent.contrasts"), "{ignored:?}");
        assert!(ignored.contains(&"intent.palette.lead2"), "{ignored:?}");
        assert!(p
            .constraints
            .iter()
            .any(|c| c.field == "intent.rhythmic_feel" && c.status == "applied"));
    }
    #[test]
    fn quiet_section_then_a_real_beat_switch() {
        let a = PlanArgs {
            brief: "bohemia type beat".into(),
            genre: Some("desi_hiphop".into()),
            seed: 7,
            duration_s: Some(90.0),
            intent: json!({"contrasts": ["quiet_section", "beat_switch"], "target_lufs": -14.0}),
            ..Default::default()
        };
        let p = plan_track(&a).unwrap();
        let q = p.sections.iter().position(|s| s.tags.iter().any(|t| t == "quiet")).expect("quiet section");
        let quiet = &p.sections[q];
        assert!(quiet.layers.iter().all(|l| !matches!(l.as_str(), "kick" | "snare" | "hat" | "bass")));
        assert_eq!(quiet.transition, "silence");
        // the switch comes straight out of the quiet section, with its own chords
        assert!(p.sections[q + 1].tags.iter().any(|t| t == "switch"));
        assert!(!p.progression_switch.is_empty() && p.progression_switch != p.progression_hook);
        assert!(p.groove_b.is_some());
        assert_eq!(p.knobs.target_lufs, -14.0);
        assert!(p.sections[0].bars >= 4);
    }

}
