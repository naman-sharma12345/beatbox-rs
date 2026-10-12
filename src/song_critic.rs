//! The song's own critic: measure a delivered song against its genre's
//! target profile (data: genre_profiles.json), name what is off in plain
//! words, and (song_improve) patch the spec with the knob that owns each
//! weakness, rebuild, re-measure and keep the best take. Producing a good
//! song needs no human ear in the loop; every judgment is a number with the
//! range it was held to.

use crate::engine::Engine;
use crate::mix_rules::{self, Targets};
use crate::song::{self, SongSpec};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const PROFILES: &str = include_str!("genre_profiles.json");
/// The onset picker's sensitivity for groove density (0..1; the profiles'
/// onsets_per_s ranges are calibrated at this value).
pub const ONSET_SENSITIVITY: f32 = 0.15;

/// What a finished song in a genre measures like on the delivered file.
/// Ranges are [least, most].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// 30-80 Hz (sub / 808) minus integrated LUFS (dB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_rel_lufs_db: Option<[f32; 2]>,
    /// Hooks over verses, RMS dB on the delivered file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_lift_db: Option<[f32; 2]>,
    /// Loudness range (LU), this engine's estimate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lra_lu: Option<[f32; 2]>,
    /// Side over mid energy (dB): stereo width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width_db: Option<[f32; 2]>,
    /// Onsets per second over the whole song (groove density).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onsets_per_s: Option<[f32; 2]>,
    /// Hooks' onset rate over verses' at least (energy build into the hook).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_vs_verse_onsets: Option<f32>,
    /// The genre in a few words (what the profile is for).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<String>,
}

impl Profile {
    fn over(mut self, d: &Profile) -> Profile {
        self.sub_rel_lufs_db = self.sub_rel_lufs_db.or(d.sub_rel_lufs_db);
        self.hook_lift_db = self.hook_lift_db.or(d.hook_lift_db);
        self.lra_lu = self.lra_lu.or(d.lra_lu);
        self.width_db = self.width_db.or(d.width_db);
        self.onsets_per_s = self.onsets_per_s.or(d.onsets_per_s);
        self.hook_vs_verse_onsets = self.hook_vs_verse_onsets.or(d.hook_vs_verse_onsets);
        self.words = self.words.or(d.words.clone());
        self
    }
}

/// Every profile in the data file (genre -> profile, "default" included).
pub fn profiles() -> Result<BTreeMap<String, Profile>> {
    let v: Value = serde_json::from_str(PROFILES)?;
    let mut out = BTreeMap::new();
    for (k, p) in v.as_object().ok_or_else(|| anyhow!("genre_profiles.json is not an object"))? {
        if k.starts_with('_') {
            continue;
        }
        let p: Profile = serde_json::from_value(p.clone()).map_err(|e| anyhow!("genre_profiles.json '{k}': {e}"))?;
        out.insert(k.clone(), p);
    }
    Ok(out)
}

/// The profile for a genre word (aliases resolve through the playbooks:
/// "honey singh" -> punjabi_party), filled from "default".
pub fn profile_for(genre: Option<&str>) -> Result<(String, Profile)> {
    let all = profiles()?;
    let d = all.get("default").cloned().ok_or_else(|| anyhow!("genre_profiles.json has no default"))?;
    let name = genre.and_then(|g| crate::producer::playbook(g).ok()).map(|pb| pb.name.to_string());
    match name.as_deref().and_then(|n| all.get(n).map(|p| (n, p))) {
        Some((n, p)) => Ok((n.to_string(), p.clone().over(&d))),
        None => Ok(("default".into(), d)),
    }
}

/// What the critic measured on one delivered file.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Metrics {
    pub lufs: f32,
    pub lra_lu: f32,
    pub true_peak_db: f32,
    pub sub_rel_lufs_db: f32,
    /// 40-120 Hz (kick + 808 body) minus integrated LUFS (dB).
    #[serde(default)]
    pub low_rel_lufs_db: f32,
    pub presence_rel_lufs_db: f32,
    pub air_rel_lufs_db: f32,
    pub width_db: f32,
    pub onsets_per_s: f32,
    pub hook_lift_db: Option<f32>,
    pub hook_vs_verse_onsets: Option<f32>,
    /// name, kind, RMS dB, onsets per second
    pub sections: Vec<(String, String, f32, f32)>,
    pub seconds: f32,
}

fn r1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}
fn r2(x: f32) -> f32 {
    (x * 100.0).round() / 100.0
}
/// A knob value as the spec should read it (an f32 widened to f64 carries
/// float noise: -25.200000762939453).
fn clean(v: Value) -> Value {
    match v.as_f64() {
        Some(x) if v.is_f64() => json!((x * 100.0).round() / 100.0),
        _ => v,
    }
}
fn db(x: f64) -> f32 {
    (10.0 * x.max(1e-12).log10()) as f32
}

/// Measure a delivered file; with section spans (beats) and the bpm, also
/// per-section level and groove, the hook lift and the hook's build.
pub fn measure(path: &std::path::Path, spans: &[mix_rules::Span], bpm: f32) -> Result<Metrics> {
    let (l, r, sr) = crate::samples::decode_stereo_native(path).map_err(|e| anyhow!("could not read {}: {e}", path.display()))?;
    if l.len() < sr as usize {
        bail!("{} is under a second long: nothing to judge", path.display());
    }
    let ld = crate::analysis::loudness_at(&l, &r, sr as f32);
    let rel = |lo: f32, hi: f32| mix_rules::band_rel_lufs(&l, &r, ld.integrated_lufs, lo, hi);
    let m: Vec<f32> = l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect();
    let (mut em, mut es) = (0f64, 0f64);
    for (a, b) in l.iter().zip(&r) {
        let (mi, si) = (0.5 * (a + b) as f64, 0.5 * (a - b) as f64);
        em += mi * mi;
        es += si * si;
    }
    let width = db(es / em.max(1e-12));
    // a strict threshold: hits, not every flicker of a pad or hiss
    let on = crate::audio_edit::onsets(&m, ONSET_SENSITIVITY, 60.0);
    let secs = m.len() as f32 / sr as f32;
    let mut sections = Vec::new();
    let bs = 60.0 / bpm.max(1.0);
    for s in spans {
        let a = ((s.start_beat * bs * sr as f32) as usize).min(m.len());
        let b = ((s.end_beat * bs * sr as f32) as usize).min(m.len());
        if b <= a + sr as usize {
            continue;
        }
        let e: f64 = m[a..b].iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>() / (b - a) as f64;
        let n = on.iter().filter(|&&o| o >= a && o < b).count();
        sections.push((s.name.clone(), s.kind.clone(), r1(db(e)), r2(n as f32 / ((b - a) as f32 / sr as f32))));
    }
    // level: 0 = RMS dB, 1 = onsets per second
    let avg = |kinds: &[&str], what: usize| -> Option<f32> {
        let v: Vec<f32> = sections.iter().filter(|s| kinds.contains(&s.1.as_str())).map(|s| if what == 0 { s.2 } else { s.3 }).collect();
        (!v.is_empty()).then(|| v.iter().sum::<f32>() / v.len() as f32)
    };
    let hooks = ["hook", "chorus", "drop"];
    let verses = ["verse"];
    let lift = match (avg(&hooks, 0), avg(&verses, 0)) {
        (Some(h), Some(v)) => Some(r1(h - v)),
        _ => None,
    };
    let build = match (avg(&hooks, 1), avg(&verses, 1)) {
        (Some(h), Some(v)) if v > 0.1 => Some(r2(h / v)),
        _ => None,
    };
    Ok(Metrics {
        lufs: r1(ld.integrated_lufs),
        lra_lu: r1(ld.loudness_range_lu),
        true_peak_db: r1(ld.true_peak_dbtp),
        sub_rel_lufs_db: r1(rel(30.0, 80.0)),
        low_rel_lufs_db: r1(rel(40.0, 120.0)),
        presence_rel_lufs_db: r1(rel(2000.0, 5000.0)),
        air_rel_lufs_db: r1(rel(5000.0, 10000.0)),
        width_db: r1(width),
        onsets_per_s: r2(on.len() as f32 / secs),
        hook_lift_db: lift,
        hook_vs_verse_onsets: build,
        sections,
        seconds: r1(secs),
    })
}

/// One thing the critic judged off, in plain words, with the knob that
/// owns it (spec paths -> new values; empty when no automatic fix exists).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Weakness {
    pub id: String,
    pub says: String,
    pub measured: Value,
    pub target: Value,
    /// How far outside its range, in range-widths (0..3): the order fixes go in.
    pub severity: f32,
    pub fix: BTreeMap<String, Value>,
}

fn sev(x: f32, [lo, hi]: [f32; 2]) -> f32 {
    let w = (hi - lo).abs().max(1.0);
    let d = if x < lo { lo - x } else if x > hi { x - hi } else { 0.0 };
    (d / w).min(3.0)
}

fn density_step(cur: Option<&str>, up: bool) -> Option<&'static str> {
    let order = ["sparse", "balanced", "dense"];
    let i = order.iter().position(|d| Some(*d) == cur).unwrap_or(0);
    if up {
        order.get(i + 1).copied()
    } else if i > 0 {
        Some(order[i - 1])
    } else {
        None
    }
}

/// The weaknesses of a measured song against its profile and the song
/// targets, biggest first. `spec` is the song's spec (for the knobs' current
/// values); `checks` the song's own delivered-file checks.
pub fn judge(m: &Metrics, p: &Profile, t: &Targets, spec: &SongSpec, checks: &Value) -> Vec<Weakness> {
    let mut out = Vec::new();
    let level = |role: &str| spec.mix.levels.get(role).copied().unwrap_or(0.0);
    // the verses: where a flat hook gets its lift (thinner arrangement)
    let verses: Vec<(String, f32)> = spec
        .structure
        .iter()
        .enumerate()
        .filter(|(_, s)| s.kind.to_lowercase().starts_with("verse"))
        .map(|(i, s)| (format!("/structure/{i}/energy"), s.energy.unwrap_or(0.0)))
        .collect();
    let mut w = |id: &str, says: String, measured: Value, target: Value, severity: f32, fix: Vec<(&str, Value)>| {
        out.push(Weakness { id: id.into(), says, measured, target, severity: r2(severity), fix: fix.into_iter().map(|(k, v)| (k.to_string(), clean(v))).collect() });
    };
    if let Some(r) = p.sub_rel_lufs_db {
        let s = sev(m.sub_rel_lufs_db, r);
        if m.sub_rel_lufs_db < r[0] {
            let up = (r[0] + 1.5 - m.sub_rel_lufs_db).clamp(1.0, 4.0);
            w("low_end_light", format!("the 808/sub is light for the genre ({} dB under the loudness, wants at least {})", m.sub_rel_lufs_db, r[0]), json!(m.sub_rel_lufs_db), json!(r), s, vec![("/mix/levels/bass", json!(r1((level("bass") + up).min(12.0))))]);
        } else if m.sub_rel_lufs_db > r[1] {
            let down = (m.sub_rel_lufs_db - (r[1] - 1.5)).clamp(1.0, 4.0);
            w("low_end_heavy", format!("the 808/sub swamps the mix ({} dB, wants at most {}): it eats the loudness the hook needs", m.sub_rel_lufs_db, r[1]), json!(m.sub_rel_lufs_db), json!(r), s, vec![("/mix/levels/bass", json!(r1((level("bass") - down).max(-12.0))))]);
        }
    }
    if let (Some(r), Some(l)) = (p.hook_lift_db, m.hook_lift_db) {
        if l < r[0] {
            // a little over the floor: the song's own lift check holds the delivered
            // file to this value less its tolerance, and the limiter takes some back
            let want = (r[0] + 0.3).max(spec.mix.drop_lift_db + 0.3).min(5.0);
            // the arrangement first: thinner verses lift the hook without
            // master automation, so a loud master keeps the lift; the
            // master's verse step only once the verses are as thin as allowed
            let step = (r[0] + 0.5 - l).clamp(1.0, 3.0);
            let thin: Vec<(&str, Value)> = verses.iter().filter(|(_, e)| *e > -6.0 + 0.05).map(|(p, e)| (p.as_str(), json!(r1((e - step).max(-6.0))))).collect();
            let fix = if thin.is_empty() { vec![("/mix/drop_lift_db", json!(r1(want)))] } else { thin };
            w("hook_flat", format!("the hook barely lifts over the verse ({l} dB, wants {}..{}): the verses thin out", r[0], r[1]), json!(l), json!(r), sev(l, r), fix);
        } else if l > r[1] {
            w("hook_jumps", format!("the hook jumps {l} dB over the verse (wants at most {}): the verses may read thin", r[1]), json!(l), json!(r), sev(l, r), vec![]);
        }
    }
    let loud_low = m.lufs < t.lufs - t.lufs_tolerance;
    if let Some(r) = p.lra_lu {
        if m.lra_lu > r[1] || (loud_low && m.lra_lu > r[0] + 1.0) {
            let pd = (spec.master.preserve_dynamics - 0.15).max(0.3);
            let says = if loud_low { format!("quieter than a release ({} LUFS, wants {}) with room to push (range {} LU)", m.lufs, t.lufs, m.lra_lu) } else { format!("the level swings too much for the genre (range {} LU, wants at most {})", m.lra_lu, r[1]) };
            let fix = if pd < spec.master.preserve_dynamics - 0.01 { vec![("/master/preserve_dynamics", json!(r2(pd)))] } else { vec![] };
            w(if loud_low { "too_quiet" } else { "too_dynamic" }, says, json!({"lufs": m.lufs, "lra_lu": m.lra_lu}), json!({"lufs": t.lufs, "lra_lu": r}), sev(m.lra_lu, r).max(if loud_low { (t.lufs - m.lufs - t.lufs_tolerance) / 2.0 } else { 0.0 }), fix);
        } else if m.lra_lu < r[0] {
            let pd = (spec.master.preserve_dynamics + 0.1).min(0.9);
            w("squashed", format!("squashed flat (range {} LU, wants at least {}): the drops lose their punch", m.lra_lu, r[0]), json!(m.lra_lu), json!(r), sev(m.lra_lu, r), vec![("/master/preserve_dynamics", json!(r2(pd)))]);
        }
    }
    if let Some(r) = p.onsets_per_s {
        if m.onsets_per_s < r[0] {
            let fix = density_step(spec.density.as_deref().or(spec.vocal.as_ref().map(|_| "sparse")), true).map(|d| vec![("/density", json!(d))]).unwrap_or_default();
            w("groove_sparse", format!("the groove is sparse for the genre ({} hits a second, wants {}..{})", m.onsets_per_s, r[0], r[1]), json!(m.onsets_per_s), json!(r), sev(m.onsets_per_s, r), fix);
        } else if m.onsets_per_s > r[1] {
            let fix = density_step(spec.density.as_deref().or(Some("balanced")), false).map(|d| vec![("/density", json!(d))]).unwrap_or_default();
            w("groove_busy", format!("the groove is crowded ({} hits a second, wants at most {}): the words have no gaps", m.onsets_per_s, r[1]), json!(m.onsets_per_s), json!(r), sev(m.onsets_per_s, r), fix);
        }
    }
    if let (Some(min), Some(b)) = (p.hook_vs_verse_onsets, m.hook_vs_verse_onsets) {
        if b < min {
            // the hook needs more motion than the verse: the hats and the
            // percussion carry it (a level step on the busiest top parts)
            w("hook_no_build", format!("the hook is no busier than the verse ({b}x the verse's hits, wants at least {min}x): no energy build into it"), json!(b), json!(min), ((min - b) / 0.2).min(3.0), vec![("/mix/levels/hat", json!(r1((level("hat") + 1.5).min(12.0))))]);
        }
    }
    if let Some(r) = p.width_db {
        if m.width_db < r[0] {
            w("narrow", format!("narrow, close to mono ({} dB side over mid, wants at least {})", m.width_db, r[0]), json!(m.width_db), json!(r), sev(m.width_db, r), vec![]);
        } else if m.width_db > r[1] {
            w("too_wide", format!("very wide ({} dB side over mid, wants at most {}): it folds to mono badly on a phone", m.width_db, r[1]), json!(m.width_db), json!(r), sev(m.width_db, r), vec![]);
        }
    }
    let [alo, ahi] = t.air_rel_lufs_db;
    if m.air_rel_lufs_db < alo - 0.15 {
        // the hats sit nearer the snare by about 1.5x the shortfall (2..6 dB a
        // round); the source level rule keeps the offset on every rebuild
        let up = (1.5 * (alo - m.air_rel_lufs_db)).clamp(2.0, 6.0);
        let mut fix = vec![("/mix/levels/hat", json!(r1((level("hat") + up).min(12.0))))];
        // a floor above the song's own (a reference's) goes into the spec so
        // every delivery holds it
        if alo > spec.targets().air_rel_lufs_db[0] + 0.05 {
            fix.push(("/mix/air_floor_db", json!(r1(alo))));
        }
        w("top_dull", format!("the top end is dull ({} dB in 5-10 kHz under the loudness, wants {}..{}): the hats have lost their sparkle", m.air_rel_lufs_db, alo, ahi), json!(m.air_rel_lufs_db), json!([alo, ahi]), sev(m.air_rel_lufs_db, [alo, ahi]), fix);
    }
    let [plo, phi] = t.presence_rel_lufs_db;
    // strict: a reference's 2-5 kHz is a ceiling, not a neighbourhood
    if m.presence_rel_lufs_db > phi {
        // the delivery's beat-bus tone aims under the ceiling by the overshoot
        let cur = spec.mix.presence_ceiling_db.unwrap_or(phi).min(phi);
        let aim = (cur - (m.presence_rel_lufs_db - phi).max(0.0) - 0.3).max(-30.0);
        w("harsh", format!("harsh in 2-5 kHz ({} dB, wants at most {}): the beat bus aims darker there", m.presence_rel_lufs_db, phi), json!(m.presence_rel_lufs_db), json!([plo, phi]), sev(m.presence_rel_lufs_db, [plo, phi]), vec![("/mix/presence_ceiling_db", json!(r1(aim)))]);
    }
    if let Some(c) = checks["checks"].as_array().and_then(|c| c.iter().find(|x| x["check"] == "vocal_over_beat")) {
        if c["pass"] == false {
            let lo = c["measured"]["min_db"].as_f64().unwrap_or(99.0) as f32;
            let hi = c["measured"]["max_db"].as_f64().unwrap_or(-99.0) as f32;
            if lo < t.vocal_over_beat_db {
                let want = (spec.mix.vocal_over_beat_db + 1.0).min(spec.mix.vocal_over_beat_max_db - 2.0);
                w("vocal_buried", format!("the vocal is buried in places ({lo} dB over the beat at its lowest, wants at least {})", t.vocal_over_beat_db), json!(lo), json!(t.vocal_over_beat_db), ((t.vocal_over_beat_db - lo) / 2.0).min(3.0), vec![("/mix/vocal_over_beat_db", json!(r1(want)))]);
            } else if hi > t.vocal_over_beat_max_db {
                w("vocal_alone", format!("the beat disappears under the vocal in places ({hi} dB, wants at most {})", t.vocal_over_beat_max_db), json!(hi), json!(t.vocal_over_beat_max_db), ((hi - t.vocal_over_beat_max_db) / 2.0).min(3.0), vec![]);
            }
        }
    }
    out.sort_by(|a, b| b.severity.partial_cmp(&a.severity).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// A reference file's measures turned into targets: the song should land
/// near it where it matters most on a phone (top end within ~1 dB, low end
/// no heavier, loudness within ~0.6 LU). Returns the extra weaknesses (the
/// top end is judged by `judge` against the raised floor this sets on `t`).
pub fn judge_reference(m: &Metrics, r: &Metrics, t: &mut Targets, spec: &SongSpec) -> Vec<Weakness> {
    let mut out = Vec::new();
    let level = |role: &str| spec.mix.levels.get(role).copied().unwrap_or(0.0);
    // the top end: within REF_AIR_BELOW_DB under the reference (never over
    // the song's own ceiling)
    let floor = (r.air_rel_lufs_db - REF_AIR_BELOW_DB).min(t.air_rel_lufs_db[1] - 0.4);
    if floor > t.air_rel_lufs_db[0] {
        t.air_rel_lufs_db[0] = r1(floor);
    }
    let over = m.low_rel_lufs_db - (r.low_rel_lufs_db + REF_LOW_OVER_DB);
    if over > 0.0 {
        let down = (over + 0.5).clamp(1.0, 4.0);
        out.push(Weakness {
            id: "low_heavier_than_reference".into(),
            says: format!("the kick and 808 sit heavier than the reference ({} dB in 40-120 Hz under the loudness, the reference {}): it eats the loudness", m.low_rel_lufs_db, r.low_rel_lufs_db),
            measured: json!(m.low_rel_lufs_db),
            target: json!(r1(r.low_rel_lufs_db + REF_LOW_OVER_DB)),
            severity: r2((over / 2.0).min(3.0)),
            fix: [("/mix/levels/bass".to_string(), clean(json!(r1((level("bass") - down).max(-12.0))))), ("/mix/levels/kick".to_string(), clean(json!(r1((level("kick") - down * 0.5).max(-12.0)))))].into_iter().collect(),
        });
    }
    let quiet = r.lufs - m.lufs - REF_LUFS_TOL;
    if quiet > 0.0 {
        let mut fix: BTreeMap<String, Value> = BTreeMap::new();
        fix.insert("/master/lufs".into(), clean(json!(r1(r.lufs))));
        let pd = (spec.master.preserve_dynamics - 0.15).max(0.3);
        if pd < spec.master.preserve_dynamics - 0.01 {
            fix.insert("/master/preserve_dynamics".into(), clean(json!(r2(pd))));
        }
        out.push(Weakness {
            id: "quieter_than_reference".into(),
            says: format!("quieter than the reference ({} LUFS, the reference {})", m.lufs, r.lufs),
            measured: json!(m.lufs),
            target: json!(r.lufs),
            severity: r2((quiet / 1.0).min(3.0)),
            fix,
        });
    }
    out
}

/// The top end may sit at most this far under a reference's (dB).
pub const REF_AIR_BELOW_DB: f32 = 1.0;
/// The low end (40-120 Hz) may sit at most this far over a reference's (dB).
pub const REF_LOW_OVER_DB: f32 = 0.5;
/// Loudness may sit this far under a reference's before it counts (LU).
pub const REF_LUFS_TOL: f32 = 0.6;

/// How far the song is from its profile: the sum of the severities (0 =
/// meets every range).
pub fn score(ws: &[Weakness]) -> f32 {
    r2(ws.iter().map(|w| w.severity).sum())
}

fn song_parts(e: &Engine) -> Result<(song::SongState, SongSpec)> {
    let st = e.project.song.clone().ok_or_else(|| anyhow!("this project has no song yet: song_create builds one, then song_critique / song_improve judge it (or give song_critique a file and a genre)"))?;
    let mut sv = st.spec.clone();
    sv["seed"] = json!(st.seed);
    let (spec, probs) = song::validate(&sv);
    let spec = spec.ok_or_else(|| anyhow!("the stored song spec no longer validates: {}", serde_json::to_string(&probs).unwrap_or_default()))?;
    Ok((st, spec))
}

fn critique_song(e: &Engine, genre: Option<&str>, reference: Option<&Metrics>) -> Result<(Metrics, Vec<Weakness>, String, Profile)> {
    let (st, spec) = song_parts(e)?;
    let audio = st.audio.clone().ok_or_else(|| anyhow!("the song has no delivered file yet: song_render makes one"))?;
    let (pname, prof) = profile_for(genre.or(spec.genre.as_deref()))?;
    let m = measure(std::path::Path::new(&audio), &st.sections, e.project.bpm)?;
    let mut t = spec.targets();
    let mut extra = match reference {
        Some(r) => judge_reference(&m, r, &mut t, &spec),
        None => Vec::new(),
    };
    let mut ws = judge(&m, &prof, &t, &spec, &st.checks);
    ws.append(&mut extra);
    ws.sort_by(|a, b| b.severity.partial_cmp(&a.severity).unwrap_or(std::cmp::Ordering::Equal));
    Ok((m, ws, pname, prof))
}

pub fn reference_metrics(e: &Engine, a: &Value) -> Result<Option<(String, Metrics)>> {
    match a.get("reference").and_then(|v| v.as_str()) {
        Some(f) => {
            let p = e.resolve(f);
            let m = measure(&p, &[], e.project.bpm).map_err(|err| anyhow!("reference: {err}"))?;
            Ok(Some((p.to_string_lossy().to_string(), m)))
        }
        None => Ok(None),
    }
}

fn verdict(ws: &[Weakness]) -> String {
    if ws.is_empty() {
        "meets its genre profile and every song check".into()
    } else {
        ws.iter().map(|w| w.says.clone()).collect::<Vec<_>>().join("; ")
    }
}

/// song_critique: judge the current song (or any audio file with a genre).
pub fn critique_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let genre = a.get("genre").and_then(|v| v.as_str());
    if let Some(f) = a.get("file").and_then(|v| v.as_str()) {
        let path = e.resolve(f);
        let (pname, prof) = profile_for(genre)?;
        let m = measure(&path, &[], e.project.bpm)?;
        let t = Targets::default();
        let ws: Vec<Weakness> = judge(&m, &prof, &t, &SongSpec::default(), &Value::Null).into_iter().map(|mut w| {
            w.fix.clear();
            w
        }).collect();
        return Ok(json!({"file": path.to_string_lossy(), "profile": pname, "profile_ranges": prof, "metrics": m, "weaknesses": ws, "score": score(&ws), "verdict": verdict(&ws), "note": "a file has no spec: weaknesses are named without fixes, and section measures (hook lift, build) need a song"}));
    }
    let rf = reference_metrics(e, a)?;
    let (m, ws, pname, prof) = critique_song(e, genre, rf.as_ref().map(|x| &x.1))?;
    let fixable = ws.iter().filter(|w| !w.fix.is_empty()).count();
    Ok(json!({"profile": pname, "profile_ranges": prof, "reference": rf.map(|(p, m)| json!({"file": p, "metrics": m})), "metrics": m, "weaknesses": ws, "score": score(&ws), "verdict": verdict(&ws),
        "next": if fixable > 0 { json!(format!("song_improve applies the {fixable} automatic fix(es) and keeps the better take")) } else { json!("nothing here has an automatic fix: song_patch by hand, or leave it") }}))
}

fn round_path(final_path: &std::path::Path, n: usize) -> String {
    let stem = final_path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "song".into());
    let ext = final_path.extension().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "mp3".into());
    final_path.with_file_name(format!("{stem}_try{n}.{ext}")).to_string_lossy().to_string()
}

/// How much worse (score) a round may come out and still be built on by the
/// next one (the best take is kept regardless).
pub const EXPLORE_MARGIN: f32 = 0.35;
/// The same weakness's fix is applied at most this many times in one run.
pub const MAX_TRIES_PER_FIX: usize = 3;

/// song_improve: critique -> patch the knobs that own the biggest
/// weaknesses -> rebuild -> critique again, keeping the best take, for up to
/// `rounds` rebuilds. The delivered file at the song's own path is the best
/// take at the end; the spec holds the knobs that made it.
pub fn improve_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let rounds = a.get("rounds").and_then(|v| v.as_u64()).unwrap_or(2).clamp(1, 4) as usize;
    let max_fixes = a.get("max_fixes").and_then(|v| v.as_u64()).unwrap_or(3).clamp(1, 6) as usize;
    let genre = a.get("genre").and_then(|v| v.as_str()).map(String::from);
    let rf = reference_metrics(e, a)?;
    improve(e, rounds, max_fixes, genre.as_deref(), rf)
}

pub fn improve(e: &mut Engine, rounds: usize, max_fixes: usize, genre: Option<&str>, reference: Option<(String, Metrics)>) -> Result<Value> {
    let rm = reference.as_ref().map(|x| &x.1);
    let t0 = std::time::Instant::now();
    let (st0, _) = song_parts(e)?;
    let final_path = std::path::PathBuf::from(st0.audio.clone().ok_or_else(|| anyhow!("the song has no delivered file yet: song_render makes one"))?);
    let orig_export = st0.spec.pointer("/export/path").cloned();
    let (m0, w0, pname, _) = critique_song(e, genre, rm)?;
    let s0 = score(&w0);
    // the take we keep: the project, its spec and its file
    let keep0 = round_path(&final_path, 0);
    std::fs::copy(&final_path, &keep0).map_err(|err| anyhow!("could not keep the first take: {err}"))?;
    let mut best = (s0, e.project.clone(), keep0.clone(), 0usize);
    let mut failed: Vec<String> = Vec::new();
    let mut tries: BTreeMap<String, usize> = BTreeMap::new();
    let mut history = vec![json!({"round": 0, "score": s0, "verdict": verdict(&w0), "weaknesses": w0.iter().map(|w| &w.id).collect::<Vec<_>>()})];
    let mut ws = w0.clone();
    let mut best_ws = w0.clone();
    let mut stop = None;
    for n in 1..=rounds {
        let mut set: Map<String, Value> = Map::new();
        let mut using = Vec::new();
        // a fix that helped may be applied again (its values are worked out
        // from the new measures); one from a round that went worse is not
        for w in ws.iter().filter(|w| !w.fix.is_empty() && !failed.contains(&w.id) && tries.get(&w.id).copied().unwrap_or(0) < MAX_TRIES_PER_FIX) {
            if using.len() >= max_fixes {
                break;
            }
            if w.fix.keys().any(|k| set.contains_key(k)) {
                continue;
            }
            for (k, v) in &w.fix {
                set.insert(k.clone(), v.clone());
            }
            using.push(w.id.clone());
        }
        if set.is_empty() {
            stop = Some(if ws.is_empty() { "the song meets its profile".to_string() } else { "what is left has no automatic fix (or its fix made the song worse, or was applied 3 times)".to_string() });
            break;
        }
        set.insert("/export/path".into(), json!(round_path(&final_path, n)));
        for id in &using {
            *tries.entry(id.clone()).or_insert(0) += 1;
        }
        let built = song::patch_song(e, &set, &[]);
        if let Err(err) = built {
            history.push(json!({"round": n, "tried": using, "set": set, "error": err.to_string()}));
            e.project = best.1.clone();
            ws = best_ws.clone();
            failed.extend(using.iter().cloned());
            continue;
        }
        let (_m, w, _, _) = critique_song(e, genre, rm)?;
        let s = score(&w);
        let better = s < best.0 - 0.05;
        // a take close to the best is built on (its new weaknesses may have
        // fixes of their own: a hook lift that cost a little air gets the air
        // back next round); a take that went clearly worse is dropped
        let explore = !better && s <= best.0 + EXPLORE_MARGIN;
        history.push(json!({"round": n, "tried": using, "set": set, "score": s, "kept": better, "built_on": better || explore, "verdict": verdict(&w)}));
        if better {
            best = (s, e.project.clone(), round_path(&final_path, n), n);
            ws = w;
        } else if explore {
            ws = w;
        } else {
            // back to the best take; its weaknesses stand, minus what failed
            e.project = best.1.clone();
            ws = best_ws.clone();
            failed.extend(using.iter().cloned());
        }
        if better {
            best_ws = ws.clone();
        }
    }
    // the best take becomes the song: its file at the song's own path, its
    // spec with the asked export path
    e.project = best.1.clone();
    if best.2 != final_path.to_string_lossy() {
        std::fs::copy(&best.2, &final_path).map_err(|err| anyhow!("could not put the best take at {}: {err}", final_path.display()))?;
    }
    if let Some(s) = e.project.song.as_mut() {
        match &orig_export {
            Some(p) => {
                let _ = song::set_pointer(&mut s.spec, "/export/path", p.clone());
            }
            None => {
                if let Some(x) = s.spec.get_mut("export").and_then(|x| x.as_object_mut()) {
                    x.remove("path");
                }
            }
        }
        s.audio = Some(final_path.to_string_lossy().to_string());
    }
    for n in 0..=rounds {
        let _ = std::fs::remove_file(round_path(&final_path, n));
    }
    let (m, w, _, _) = critique_song(e, genre, rm)?;
    Ok(json!({
        "audio": final_path.to_string_lossy(),
        "profile": pname,
        "reference": reference.as_ref().map(|(p, m)| json!({"file": p, "metrics": m})),
        "kept_round": best.3,
        "score": {"before": s0, "after": score(&w)},
        "before": {"metrics": m0, "verdict": verdict(&w0)},
        "after": {"metrics": m, "weaknesses": w, "verdict": verdict(&w)},
        "rounds": history,
        "stopped": stop,
        "spec_levels": e.project.song.as_ref().and_then(|s| s.spec.pointer("/mix/levels").cloned()),
        "wall_seconds": r1(t0.elapsed().as_secs_f32()),
        "next": "song_get shows the knobs the kept take used; undo restores the song before this call",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_profile_parses_and_names_a_known_genre() {
        let all = profiles().unwrap();
        assert!(all.contains_key("default"));
        let d = &all["default"];
        assert!(d.sub_rel_lufs_db.is_some() && d.hook_lift_db.is_some() && d.lra_lu.is_some() && d.width_db.is_some() && d.onsets_per_s.is_some() && d.hook_vs_verse_onsets.is_some());
        for (k, p) in &all {
            if k != "default" {
                assert_eq!(crate::producer::playbook(k).map(|pb| pb.name.to_string()).ok().as_deref(), Some(k.as_str()), "profile '{k}' is not a playbook name");
            }
            for r in [p.sub_rel_lufs_db, p.hook_lift_db, p.lra_lu, p.width_db, p.onsets_per_s].into_iter().flatten() {
                assert!(r[0] < r[1], "{k}: {r:?}");
            }
        }
        // aliases resolve through the playbooks; unknown words fall back
        assert_eq!(profile_for(Some("honey singh")).unwrap().0, "punjabi_party");
        assert_eq!(profile_for(Some("polka")).unwrap().0, "default");
        let (_, p) = profile_for(Some("punjabi_party")).unwrap();
        assert!(p.width_db.is_some(), "a genre is filled from the default");
    }

    fn metrics_ok() -> Metrics {
        Metrics { lufs: -10.0, lra_lu: 5.0, true_peak_db: -1.5, sub_rel_lufs_db: -12.0, low_rel_lufs_db: -12.0, presence_rel_lufs_db: -22.7, air_rel_lufs_db: -25.0, width_db: -14.0, onsets_per_s: 6.0, hook_lift_db: Some(3.0), hook_vs_verse_onsets: Some(1.1), sections: vec![], seconds: 120.0 }
    }

    #[test]
    fn the_approved_mix_meets_its_own_genre_profile() {
        // naman_freestyle_song2.mp3 as `song_critique {file}` measures it
        let m = Metrics { lufs: -9.7, lra_lu: 2.2, true_peak_db: -1.8, sub_rel_lufs_db: -19.4, low_rel_lufs_db: -15.4, presence_rel_lufs_db: -22.4, air_rel_lufs_db: -24.2, width_db: -18.2, onsets_per_s: 8.08, hook_lift_db: None, hook_vs_verse_onsets: None, sections: vec![], seconds: 135.9 };
        let (_, p) = profile_for(Some("desi_hiphop")).unwrap();
        let ws = judge(&m, &p, &Targets::default(), &SongSpec::default(), &Value::Null);
        assert!(ws.is_empty(), "the mix the person approved must pass: {ws:?}");
    }

    #[test]
    fn judge_names_each_weakness_with_the_knob_that_owns_it() {
        let (_, p) = profile_for(Some("punjabi_party")).unwrap();
        let t = Targets::default();
        let spec = SongSpec::default();
        assert!(judge(&metrics_ok(), &p, &t, &spec, &Value::Null).is_empty(), "a song inside every range has nothing to fix");
        let mut m = metrics_ok();
        m.sub_rel_lufs_db = -19.0;
        m.hook_lift_db = Some(1.0);
        m.air_rel_lufs_db = -28.0;
        m.onsets_per_s = 3.0;
        let ws = judge(&m, &p, &t, &spec, &Value::Null);
        let ids: Vec<&str> = ws.iter().map(|w| w.id.as_str()).collect();
        for id in ["low_end_light", "hook_flat", "top_dull", "groove_sparse"] {
            assert!(ids.contains(&id), "{ids:?}");
        }
        let get = |id: &str| ws.iter().find(|w| w.id == id).unwrap();
        assert!(get("low_end_light").fix["/mix/levels/bass"].as_f64().unwrap() >= 1.0);
        let hf = &get("hook_flat").fix;
        assert!(hf.iter().any(|(k, v)| k.ends_with("/energy") && v.as_f64().unwrap() < 0.0) || hf["/mix/drop_lift_db"].as_f64().unwrap() > spec.mix.drop_lift_db as f64);
        assert_eq!(get("groove_sparse").fix["/density"], "balanced");
        assert!(ws.windows(2).all(|x| x[0].severity >= x[1].severity), "biggest first");
        assert!(score(&ws) > 0.0);
        // every fix is a spec path the song accepts
        let mut sv = serde_json::to_value(&spec).unwrap();
        for w in &ws {
            for (k, v) in &w.fix {
                song::set_pointer(&mut sv, k, v.clone()).unwrap();
            }
        }
        let (ok, probs) = song::validate(&sv);
        assert!(ok.is_some(), "{probs:?}");
    }

    #[test]
    fn a_reference_raises_the_floor_and_names_low_end_and_loudness() {
        let (_, p) = profile_for(Some("punjabi_party")).unwrap();
        let spec = SongSpec::default();
        let mut song2 = metrics_ok();
        song2.air_rel_lufs_db = -24.2;
        song2.low_rel_lufs_db = -15.4;
        song2.lufs = -9.7;
        let mut m = metrics_ok();
        m.air_rel_lufs_db = -25.9;
        m.low_rel_lufs_db = -13.9;
        m.lufs = -10.9;
        let mut t = Targets::default();
        let extra = judge_reference(&m, &song2, &mut t, &spec);
        assert!((t.air_rel_lufs_db[0] - -25.2).abs() < 0.05, "{:?}", t.air_rel_lufs_db);
        let ids: Vec<&str> = extra.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, ["low_heavier_than_reference", "quieter_than_reference"]);
        let ws = judge(&m, &p, &t, &spec, &Value::Null);
        let dull = ws.iter().find(|w| w.id == "top_dull").expect("the top end is judged against the raised floor");
        assert_eq!(dull.fix["/mix/air_floor_db"], json!(-25.2));
        // every fix validates as a spec change
        let mut sv = serde_json::to_value(&spec).unwrap();
        for w in extra.iter().chain(ws.iter()) {
            for (k, v) in &w.fix {
                song::set_pointer(&mut sv, k, v.clone()).unwrap();
            }
        }
        let (ok, probs) = song::validate(&sv);
        assert!(ok.is_some(), "{probs:?}");
        // a reference never lifts the floor over the song's own ceiling
        let mut bright = song2.clone();
        bright.air_rel_lufs_db = -20.0;
        let mut t2 = Targets::default();
        judge_reference(&m, &bright, &mut t2, &spec);
        assert!(t2.air_rel_lufs_db[0] <= t2.air_rel_lufs_db[1] - 0.4 + 1e-3);
    }

    #[test]
    fn measure_reads_a_file_and_judges_it_without_a_song() {
        let d = std::env::temp_dir().join("bb_song_critic_file");
        std::fs::create_dir_all(&d).unwrap();
        let sr = crate::dsp::SR;
        let n = (sr * 6.0) as usize;
        // a kick-like thump every half second over a quiet hiss
        let mut l = vec![0f32; n];
        for (i, x) in l.iter_mut().enumerate() {
            let tt = (i % (sr as usize / 2)) as f32 / sr;
            *x = 0.6 * (std::f32::consts::TAU * 55.0 * tt).sin() * (-tt * 12.0).exp() + 0.01 * ((((i as u64).wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407) >> 33) % 2000) as f32 / 1000.0 - 1.0);
        }
        let path = d.join("thump.wav");
        crate::render::write_wav(&path, &l, &l).unwrap();
        let m = measure(&path, &[], 120.0).unwrap();
        assert!((m.seconds - 6.0).abs() < 0.2, "{m:?}");
        assert!(m.width_db < -40.0, "mono reads narrow: {}", m.width_db);
        assert!(m.sub_rel_lufs_db > -12.0, "a 55 Hz thump is all sub: {}", m.sub_rel_lufs_db);
        assert!(m.onsets_per_s > 1.0 && m.onsets_per_s < 4.0, "two hits a second: {}", m.onsets_per_s);
        let mut e = Engine::new(d.clone());
        let r = critique_tool(&mut e, &json!({"file": path.to_string_lossy(), "genre": "badshah"})).unwrap();
        assert_eq!(r["profile"], "punjabi_party");
        assert!(r["weaknesses"].as_array().unwrap().iter().any(|w| w["id"] == "narrow"), "{r}");
        assert!(critique_tool(&mut Engine::new(d), &json!({})).is_err(), "no song, no file: a clear error");
    }
}
