//! Mix and master rules: the ONE place where beatbox decides how a finished
//! song should sit, and the one place that checks it.
//!
//! Everything a song render needs to get right about levels lives here as
//! named numbers ([`Targets`]) plus the code that applies and measures them:
//!
//! * the vocal over the beat (>= +6 dB where a vocal sings, never more than
//!   +12), corrected with the vocal TRACK fader (post-insert, so a 3:1
//!   compressor cannot eat it the way it eats clip gain) as per-section
//!   automation;
//! * hooks/drops louder than verses (+2..3 dB on the delivered file), with
//!   the master's section automation;
//! * loudness and peaks at delivery (-10 LUFS, true peak <= -1 dBTP), done by
//!   the delivery stage (soft clip + limiter with the section contrast kept),
//!   not by a limiter inside the project that would flatten the drops;
//! * the vocal chain for a raw take (HPF, light de-ess, 3:1 compression,
//!   small reverb/delay sends; no noise reduction, gate, tuning or extra EQ);
//! * the 1-2 kHz pocket in the melodic beds, applied whenever a voice is in
//!   the project (also when the vocal arrives after the beat was composed).
//!
//! [`check`] measures a render against the targets and every song render
//! returns its result, so a level problem is a failed check in the response,
//! not something the listener finds.

use crate::engine::Engine;
use crate::project::Project;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const SR: f32 = crate::dsp::SR;

/// The measurable targets every song render is checked against.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Targets {
    /// Least the vocal sits over the beat, full band, where it sings (dB).
    pub vocal_over_beat_db: f32,
    /// Most the vocal sits over the beat before the beat feels gone (dB).
    pub vocal_over_beat_max_db: f32,
    /// Hooks/drops over verses on the delivered file, RMS dB: [least, most].
    pub drop_lift_db: [f32; 2],
    /// Integrated loudness of the delivered file (LUFS).
    pub lufs: f32,
    /// How far under/over `lufs` still passes (LU).
    pub lufs_tolerance: f32,
    /// Highest true peak of the delivered (decoded) file (dBTP).
    pub true_peak_max_db: f32,
    /// Most of the delivered mix's energy in 2-5 kHz (dB of the total):
    /// over this a mix reads harsh/bright on phones.
    pub presence_max_share_db: f32,
    /// Most of the delivered mix's energy in 5-10 kHz (dB of the total).
    pub air_max_share_db: f32,
    /// Least in 2-5 kHz: under this the mix reads muffled.
    pub presence_min_share_db: f32,
    /// Tilt: 2-5 kHz energy against 250 Hz-2 kHz (the voice's body) on the
    /// delivered file, at most (dB). Independent of how much sub there is.
    /// Calibrated on the hand remix the person approved (-16.5 / -18.3).
    pub presence_tilt_max_db: f32,
    /// Tilt: 5-10 kHz against 250 Hz-2 kHz, at most (dB).
    pub air_tilt_max_db: f32,
    /// Where a vocal sings, every beat part sits at least this far under the
    /// vocal in 2-5 kHz (the words' band belongs to the voice).
    pub beat_under_vocal_presence_db: f32,
}

impl Default for Targets {
    fn default() -> Self {
        Targets {
            vocal_over_beat_db: 6.0,
            vocal_over_beat_max_db: 12.0,
            drop_lift_db: [2.0, 3.0],
            lufs: -10.0,
            lufs_tolerance: 1.0,
            true_peak_max_db: -1.0,
            presence_max_share_db: -19.0,
            air_max_share_db: -20.5,
            presence_min_share_db: -24.0,
            presence_tilt_max_db: -16.0,
            air_tilt_max_db: -17.5,
            beat_under_vocal_presence_db: 6.0,
        }
    }
}

impl Targets {
    /// Where the vocal correction aims: a little over the floor so the
    /// ducking and the master glue cannot pull a section back under it.
    pub fn vocal_aim_db(&self) -> f32 {
        (self.vocal_over_beat_db + 1.5).min(self.vocal_over_beat_max_db - 1.0)
    }
}

/// Delivery: the limiter ceiling sits a little under the true-peak target so
/// a lossy encode (mp3 overshoot) still passes after decoding.
pub const DELIVERY_CEILING_MARGIN_DB: f32 = 0.2;
/// Delivery: soft clip up to this much before the limiter (keeps drops up).
pub const DELIVERY_SOFT_CLIP_DB: f32 = 3.0;
/// Delivery: how much of the section contrast survives limiting.
pub const DELIVERY_PRESERVE_DYNAMICS: f32 = 0.8;
/// A song with a loud vocal starts here: measured on the freestyle song,
/// 0.8 stopped 1.2 dB short of -10 LUFS with hooks +3.1 over verses; 0.6
/// reached -10.7 with +2.4 (the delivery loop moves it further if needed).
pub const SONG_PRESERVE_DYNAMICS: f32 = 0.6;

/// The vocal pocket: the melodic beds step back around 1.4 kHz.
pub const POCKET_HZ: f32 = 1400.0;
pub const POCKET_DB: f32 = -3.0;
pub const POCKET_Q: f32 = 0.9;
/// The beds the producer writes (track names) that get the pocket.
pub const POCKET_TRACKS: &[&str] = &["chords", "counter", "texture"];
/// The melodic parts that duck a little under the vocal (sidechain).
pub const DUCK_TRACKS: &[&str] = &["lead", "counter", "chords", "texture"];
pub const DUCK_AMOUNT: f32 = 0.3;

/// The vocal chain for a raw spoken/sung take: what the person approved for
/// their own voice (HPF 80-90, light de-ess, 2:1-3:1 compression, sends).
/// Nothing else: no noise reduction, gate, tuning, exciter or extra EQ.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct VocalChain {
    pub hpf_hz: f32,
    pub deess_freq_hz: f32,
    /// Most the de-esser takes off (dB, negative).
    pub deess_range_db: f32,
    pub comp_ratio: f32,
    pub comp_threshold_db: f32,
    pub comp_makeup_db: f32,
    /// Reverb send level (dB); None = no reverb send.
    pub reverb_send_db: Option<f32>,
    /// Delay send level (dB); None = no delay send.
    pub delay_send_db: Option<f32>,
}

impl Default for VocalChain {
    fn default() -> Self {
        VocalChain {
            hpf_hz: 85.0,
            deess_freq_hz: 6500.0,
            deess_range_db: -4.0,
            comp_ratio: 3.0,
            comp_threshold_db: -30.0,
            comp_makeup_db: 6.0,
            reverb_send_db: Some(-20.0),
            delay_send_db: Some(-28.0),
        }
    }
}

impl VocalChain {
    /// Problems with the chain, each with the fix.
    pub fn problems(&self, path: &str) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        if !(40.0..=200.0).contains(&self.hpf_hz) {
            out.push((format!("{path}/hpf_hz"), format!("{} Hz is outside 40..200", self.hpf_hz), "use 80-90 Hz for a voice".into()));
        }
        if !(1.5..=4.0).contains(&self.comp_ratio) {
            out.push((format!("{path}/comp_ratio"), format!("{}:1 is outside the 1.5..4 the voice rules allow", self.comp_ratio), "use 2 or 3".into()));
        }
        if !(-12.0..=0.0).contains(&self.deess_range_db) {
            out.push((format!("{path}/deess_range_db"), format!("{} dB is not a light de-ess", self.deess_range_db), "use -3 to -6".into()));
        }
        out
    }
}

/// A voice track by its name.
pub fn is_voice_name(name: &str) -> bool {
    let n = name.to_lowercase();
    ["vocal", "vox", "voice", "rap", "sing", "acapella", "a_cappella"].iter().any(|w| n.contains(w))
}

/// Whether the project holds a voice (a voice-named track, or any audio clip:
/// recordings placed on the timeline are almost always the voice).
pub fn has_voice(p: &Project) -> bool {
    !p.audio_clips.is_empty() || p.tracks.iter().any(|t| is_voice_name(&t.name))
}

fn has_pocket(p: &Project, track: &str) -> bool {
    let Ok(i) = p.track_index(track) else { return false };
    let fx = serde_json::to_value(&p.tracks[i].effects).unwrap_or(Value::Null);
    fx.as_array().into_iter().flatten().any(|e| {
        e["type"] == "parametric_eq"
            && e["bands"].as_array().into_iter().flatten().any(|b| {
                let f = b["freq"].as_f64().unwrap_or(0.0) as f32;
                let g = b["gain_db"].as_f64().unwrap_or(0.0) as f32;
                (f - POCKET_HZ).abs() < 250.0 && g <= -1.5
            })
    })
}

/// The vocal pocket in the melodic beds, once, when a voice is present.
/// Idempotent; returns the tracks it changed.
pub fn ensure_vocal_pocket(e: &mut Engine, source: &str) -> Result<Vec<String>> {
    let mut done = Vec::new();
    if !has_voice(&e.project) {
        return Ok(done);
    }
    for t in POCKET_TRACKS {
        if e.project.track_index(t).is_ok() && !has_pocket(&e.project, t) {
            e.call_nested(
                "add_effect",
                &json!({"track": t, "type": "parametric_eq", "params": {"bands": [{"kind": "bell", "freq": POCKET_HZ, "gain_db": POCKET_DB, "q": POCKET_Q}]}}),
                source,
            )?;
            done.push(t.to_string());
        }
    }
    Ok(done)
}

/// Put the vocal chain on `track` (replacing an earlier one this module
/// wrote) and the melodic beds' ducking under it.
pub fn apply_vocal_chain(e: &mut Engine, track: &str, c: &VocalChain, source: &str) -> Result<Value> {
    let ti = e.project.track_index(track)?;
    e.project.tracks[ti].effects.clear();
    e.project.tracks[ti].sends.clear();
    let mut steps = Vec::new();
    e.call_nested("add_effect", &json!({"track": track, "type": "parametric_eq", "params": {"bands": [{"kind": "low_cut", "freq": c.hpf_hz, "gain_db": 0.0, "q": 0.707, "stages": 2}]}}), source)?;
    steps.push(format!("high-pass {} Hz", c.hpf_hz));
    e.call_nested("add_effect", &json!({"track": track, "type": "deesser", "params": {"freq": c.deess_freq_hz, "threshold_db": -30.0, "range_db": c.deess_range_db, "release_ms": 60.0}}), source)?;
    steps.push(format!("de-ess {} kHz, at most {} dB", c.deess_freq_hz / 1000.0, c.deess_range_db));
    // two stages, both inside the 2:1-3:1 rule: a fast one 12 dB up that
    // only catches plosives and shouts (measured on the freestyle take:
    // peaks ~20 dB over the phrase level made the delivery limiter duck the
    // whole song 7+ dB and lift the beat in every gap), then the body
    e.call_nested("add_effect", &json!({"track": track, "type": "compressor", "params": {"threshold_db": c.comp_threshold_db + 12.0, "ratio": c.comp_ratio, "attack_ms": 0.3, "release_ms": 40.0, "makeup_db": 0.0, "knee_db": 3.0, "lookahead_ms": 2.0}}), source)?;
    e.call_nested("add_effect", &json!({"track": track, "type": "compressor", "params": {"threshold_db": c.comp_threshold_db, "ratio": c.comp_ratio, "attack_ms": 1.0, "release_ms": 100.0, "makeup_db": c.comp_makeup_db, "knee_db": 6.0, "lookahead_ms": 2.0}}), source)?;
    steps.push(format!("compressor {}:1 in two stages (peak catch, then body)", c.comp_ratio));
    for (bus, db) in [("verb", c.reverb_send_db), ("echo", c.delay_send_db)] {
        if let Some(db) = db {
            if e.project.buses.iter().any(|b| b.name == bus) {
                e.call_nested("set_send", &json!({"track": track, "bus": bus, "db": db}), source)?;
                steps.push(format!("{bus} send {db} dB"));
            }
        }
    }
    for t in DUCK_TRACKS {
        if let Ok(i) = e.project.track_index(t) {
            let already = serde_json::to_value(&e.project.tracks[i].effects)
                .ok()
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .any(|x| x["type"] == "sidechain" && x["source"] == track);
            if !already {
                e.call_nested("add_effect", &json!({"track": t, "type": "sidechain", "params": {"source": track, "amount": DUCK_AMOUNT, "release_ms": 180.0}}), source)?;
                steps.push(format!("{t} ducks under the vocal"));
            }
        }
    }
    let pocket = ensure_vocal_pocket(e, source)?;
    if !pocket.is_empty() {
        steps.push(format!("1.4 kHz pocket in {}", pocket.join(", ")));
    }
    Ok(json!(steps))
}

/// One arrangement section on the song timeline.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Span {
    pub name: String,
    pub kind: String,
    pub start_beat: f32,
    pub end_beat: f32,
}

/// The section kind from a section/pattern name ("hook2" -> "hook").
pub fn kind_of(name: &str) -> String {
    let k = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '_' || c == '-').to_lowercase();
    match k.as_str() {
        "chorus" | "drop" => "hook".into(),
        _ => k,
    }
}

pub fn is_lift_kind(kind: &str) -> bool {
    matches!(kind, "hook" | "chorus" | "drop")
}

/// The song's sections in order (arrangement x pattern lengths, 4/4).
pub fn section_spans(p: &Project) -> Vec<Span> {
    let mut out = Vec::new();
    let mut beat = 0.0f32;
    for s in &p.arrangement {
        let bars = p.patterns.iter().find(|x| x.name == s.pattern).map(|x| x.bars).unwrap_or(4) as f32;
        let len = bars * 4.0 * s.repeats.max(1) as f32;
        out.push(Span { name: s.pattern.clone(), kind: kind_of(&s.pattern), start_beat: beat, end_beat: beat + len });
        beat += len;
    }
    out
}

fn mono(l: &[f32], r: &[f32]) -> Vec<f32> {
    l.iter().zip(r.iter()).map(|(x, y)| 0.5 * (x + y)).collect()
}

fn rms_db(x: &[f32]) -> f32 {
    let e: f64 = x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len().max(1) as f64;
    (10.0 * e.max(1e-12).log10()) as f32
}

fn r1(x: f32) -> f64 {
    ((x * 10.0).round() / 10.0) as f64
}

/// The vocal against the beat, per section, on a render: where the vocal
/// sounds (voice-band level over its own threshold), full-band RMS of the
/// vocal (with its reverb/delay returns) minus the beat (the mix minus the
/// vocal, so ducking counts). Also every section's mix RMS.
#[derive(Clone, Debug, Serialize)]
pub struct SectionLevel {
    pub name: String,
    pub kind: String,
    pub start_s: f32,
    pub mix_rms_db: f32,
    /// None where the vocal does not sing (< 0.5 s voiced).
    pub vocal_over_beat_db: Option<f32>,
    pub presence_over_beat_db: Option<f32>,
}

pub fn measure_sections(e: &mut Engine, voice_track: Option<&str>) -> Result<Vec<SectionLevel>> {
    let spans = section_spans(&e.project);
    // measured BEFORE the master chain: a master compressor applies one gain
    // to the sum (the vocal/beat ratio survives it), but the same compressor
    // on the vocal alone acts differently, so "mix minus solo vocal" after
    // the master would leave vocal in the "beat" and the measured gap would
    // stall near 0 dB however loud the vocal gets. Before the master the mix
    // is exactly beat + vocal.
    let mut pre = e.project.clone();
    pre.master_effects.clear();
    let full = e.render_version(&pre)?;
    let bs = 60.0 / e.project.bpm;
    let m = mono(&full.left, &full.right);
    let voc = match voice_track {
        Some(t) => {
            let vi = e.project.track_index(t)?;
            let mut solo = pre.clone();
            for (i, tr) in solo.tracks.iter_mut().enumerate() {
                tr.mute = i != vi;
                tr.solo = false;
            }
            let v = e.render_version(&solo)?;
            Some(mono(&v.left, &v.right))
        }
        None => None,
    };
    // one voice threshold over the whole take, so a quiet section is not
    // measured against its own floor
    let (vdb, thr) = match &voc {
        Some(v) => {
            let d = crate::vocal_flex::band_db(v);
            let t = crate::vocal_flex::voice_threshold(&d, None);
            (d, t)
        }
        None => (Vec::new(), 0.0),
    };
    let hop = (crate::vocal_flex::HOP * SR) as usize;
    let mut out = Vec::new();
    for s in &spans {
        let a = ((s.start_beat * bs * SR) as usize).min(m.len());
        let b = ((s.end_beat * bs * SR) as usize).min(m.len());
        if b <= a + hop {
            continue;
        }
        let mut lvl = SectionLevel { name: s.name.clone(), kind: s.kind.clone(), start_s: s.start_beat * bs, mix_rms_db: rms_db(&m[a..b]), vocal_over_beat_db: None, presence_over_beat_db: None };
        if let Some(v) = &voc {
            let (mut vv, mut bb) = (Vec::new(), Vec::new());
            for f in (a / hop)..(b / hop).min(vdb.len()) {
                if vdb[f] >= thr {
                    let x0 = f * hop;
                    let x1 = (x0 + hop).min(v.len()).min(m.len());
                    for k in x0..x1 {
                        vv.push(v[k]);
                        bb.push(m[k] - v[k]);
                    }
                }
            }
            if vv.len() as f32 >= 0.5 * SR && rms_db(&bb) > -90.0 {
                lvl.vocal_over_beat_db = Some(rms_db(&vv) - rms_db(&bb));
                lvl.presence_over_beat_db = Some(crate::vocal_flex::band_rms_db(&vv, 1000.0, 4000.0) - crate::vocal_flex::band_rms_db(&bb, 1000.0, 4000.0));
            }
        }
        out.push(lvl);
    }
    Ok(out)
}

/// Write the vocal's per-section level as fader automation (absolute dB,
/// stepped one beat before each section so pickups ride with it).
fn write_vocal_lane(e: &mut Engine, track: &str, base_db: f32, offs: &[(f32, f32)]) {
    use crate::automation::{AutoPoint, AutomationLane};
    e.project.automation.retain(|l| !(l.target == track && l.param == "volume"));
    if offs.iter().all(|(_, o)| o.abs() < 0.05) {
        return;
    }
    let mut lane = AutomationLane::new(track, "volume");
    for (i, (beat, off)) in offs.iter().enumerate() {
        let at = if i == 0 { 0.0 } else { (beat - 1.0).max(0.0) };
        lane.points.push(AutoPoint { beat: at, value: (base_db + off).clamp(-60.0, 24.0), curve: crate::automation::Curve::Step });
    }
    e.project.automation.push(lane);
}

/// The closed loop that puts the vocal where the targets say, section by
/// section, with the vocal track's fader (post-compressor: 1 dB here is
/// 1 dB heard). Re-measures after every move; at most `passes` renders.
pub fn balance_vocal(e: &mut Engine, track: &str, t: &Targets, passes: usize) -> Result<Value> {
    let ti = e.project.track_index(track)?;
    let base = e.project.tracks[ti].volume_db;
    let spans = section_spans(&e.project);
    // start from the lane an earlier balance wrote (re-renders converge fast)
    let lane = e.project.automation.iter().find(|l| l.target == track && l.param == "volume").cloned();
    let mut offs: Vec<(f32, f32)> = spans
        .iter()
        .map(|s| {
            let v = lane.as_ref().and_then(|l| l.points.iter().filter(|p| p.beat <= s.start_beat + 0.5).last().map(|p| p.value - base)).unwrap_or(0.0);
            (s.start_beat, v)
        })
        .collect();
    let aim = t.vocal_aim_db();
    let mut log = Vec::new();
    let mut last = Vec::new();
    for pass in 0..passes.max(1) {
        write_vocal_lane(e, track, base, &offs);
        let m = measure_sections(e, Some(track))?;
        let mut moved = false;
        let mut prev_off = 0.0f32;
        for (i, s) in spans.iter().enumerate() {
            let lv = m.iter().find(|x| x.name == s.name && (x.start_s - s.start_beat * 60.0 / e.project.bpm).abs() < 0.01);
            match lv.and_then(|x| x.vocal_over_beat_db) {
                Some(v) => {
                    if v < t.vocal_over_beat_db || v > t.vocal_over_beat_max_db - 0.5 {
                        offs[i].1 = (offs[i].1 + (aim - v)).clamp(-12.0, 24.0);
                        moved = true;
                    }
                    prev_off = offs[i].1;
                }
                // no vocal here: hold the last level (its reverb tail rides it)
                None => offs[i].1 = prev_off,
            }
        }
        // gain staging: a quiet take would need the fader near its +24 dB
        // stop; the common part of the move goes into a gain stage after the
        // compressor (still linear, still post-insert), the fader keeps the
        // per-section differences
        let voiced: Vec<f32> = spans.iter().enumerate().filter(|(i, s)| m.iter().any(|x| x.name == s.name && x.vocal_over_beat_db.is_some()) && offs[*i].1.abs() > 0.0).map(|(i, _)| offs[i].1).collect();
        if !voiced.is_empty() {
            let mean = voiced.iter().sum::<f32>() / voiced.len() as f32;
            if mean.abs() > 6.0 {
                stage_gain(e, track, mean)?;
                for o in offs.iter_mut() {
                    o.1 -= mean;
                }
            }
        }
        log.push(json!({"pass": pass + 1, "vocal_over_beat_db": m.iter().filter_map(|x| x.vocal_over_beat_db.map(|v| json!([x.name, r1(v)]))).collect::<Vec<_>>()}));
        last = m;
        if !moved {
            break;
        }
        if pass + 1 == passes.max(1) {
            // the last move is written and measured once more
            write_vocal_lane(e, track, base, &offs);
            last = measure_sections(e, Some(track))?;
            log.push(json!({"pass": "final", "vocal_over_beat_db": last.iter().filter_map(|x| x.vocal_over_beat_db.map(|v| json!([x.name, r1(v)]))).collect::<Vec<_>>()}));
        }
    }
    Ok(json!({
        "track": track,
        "aim_db": aim,
        "fader_offsets_db": spans.iter().zip(offs.iter()).map(|(s, o)| json!([s.name, r1(o.1)])).collect::<Vec<_>>(),
        "passes": log,
        "sections": last,
        "how": "vocal track fader automation (after its compressor), never clip gain",
    }))
}

/// Add `db` to the vocal's gain stage (a gain effect right after the
/// chain this module writes: HPF, de-esser, compressor).
fn stage_gain(e: &mut Engine, track: &str, db: f32) -> Result<()> {
    let ti = e.project.track_index(track)?;
    // the stage sits right after the last compressor of the chain
    let at = serde_json::to_value(&e.project.tracks[ti].effects)?
        .as_array()
        .map(|a| a.iter().rposition(|f| f["type"] == "compressor").map(|i| i + 1).unwrap_or(a.len()))
        .unwrap_or(0);
    let fx = &mut e.project.tracks[ti].effects;
    if let Some(f) = fx.get_mut(at) {
        let mut v = serde_json::to_value(&*f)?;
        if v["type"] == "gain" {
            let cur = v["db"].as_f64().unwrap_or(0.0) as f32;
            v["db"] = json!((cur + db).clamp(-24.0, 36.0));
            *f = serde_json::from_value(v)?;
            return Ok(());
        }
    }
    e.call_nested("add_effect", &json!({"track": track, "type": "gain", "params": {"db": db.clamp(-24.0, 36.0)}}), "mix_rules")?;
    // keep it right after the compressor (before nothing else this module adds)
    let ti = e.project.track_index(track)?;
    let fx = &mut e.project.tracks[ti].effects;
    if fx.len() > at + 1 {
        let g = fx.pop().expect("just added");
        fx.insert(at, g);
    }
    Ok(())
}

/// Glue, not loudness: the master compressor only touches the loudest
/// peaks (threshold 5 dB over the loudest section's RMS, 1.5:1), so it
/// cannot flatten the hooks; loudness is the delivery stage's job.
pub fn set_glue(e: &mut Engine) -> Result<Value> {
    let m = measure_sections(e, None)?;
    let loud = m.iter().map(|x| x.mix_rms_db).fold(-120.0f32, f32::max);
    let thr = (loud + 5.0).min(-1.0);
    let mut n = 0;
    for f in e.project.master_effects.iter_mut() {
        let mut v = serde_json::to_value(&*f)?;
        if v["type"] == "compressor" {
            v["threshold_db"] = json!(thr);
            v["ratio"] = json!(1.5);
            v["attack_ms"] = json!(30.0);
            v["release_ms"] = json!(150.0);
            v["makeup_db"] = json!(0.0);
            *f = serde_json::from_value(v)?;
            n += 1;
        }
    }
    Ok(json!({"compressors": n, "threshold_db": r1(thr), "ratio": 1.5}))
}

/// Step the verses down by `db` more on the master's section automation.
pub fn deepen_verses(e: &mut Engine, db: f32) -> Result<()> {
    let spans = section_spans(&e.project);
    let mut names: Vec<String> = spans.iter().filter(|s| s.kind == "verse").map(|s| s.name.clone()).collect();
    names.dedup();
    for n in &names {
        let cur = crate::tools_studio::static_value(&e.project, "master", "volume").unwrap_or(0.0);
        let at = section_master_db(&e.project, &spans, n).unwrap_or(cur);
        e.call_nested("set_section_mix", &json!({"section": n, "track": "master", "volume_db": at - db, "all_occurrences": false}), "mix_rules")?;
    }
    Ok(())
}

/// Band energy (dB, mean square) with 4th-order edges, so a loud sub does
/// not leak into a 2-5 kHz reading.
fn band_energy_db(x: &[f32], lo: f32, hi: f32) -> f32 {
    use crate::dsp::{Biquad, BiquadKind};
    let mut f = [
        Biquad::new(BiquadKind::LowCut, lo, 0.707, 0.0),
        Biquad::new(BiquadKind::LowCut, lo, 0.707, 0.0),
        Biquad::new(BiquadKind::HighCut, hi, 0.707, 0.0),
        Biquad::new(BiquadKind::HighCut, hi, 0.707, 0.0),
    ];
    let e: f64 = x
        .iter()
        .map(|v| {
            let mut y = *v;
            for b in f.iter_mut() {
                y = b.process(y);
            }
            (y as f64) * (y as f64)
        })
        .sum::<f64>()
        / x.len().max(1) as f64;
    (10.0 * e.max(1e-12).log10()) as f32
}

/// Most any top-end rule cuts one part in one render (dB).
pub const MAX_TOP_CUT_DB: f32 = 4.0;
/// Most a group bus (drums) peaks over its own loudness after the peak rule.
pub const BUS_MAX_CREST_DB: f32 = 13.0;

/// Peaks, measured on the group buses (the drum bus): a bus whose peaks
/// stand more than BUS_MAX_CREST_DB over its own loudness gets a fast
/// limiter at that crest, so the delivery limiter does not have to take
/// those dB out of the whole song (which flattens the hooks). The level
/// moves this module makes later are post-bus, so this limiter never eats
/// them.
pub fn tame_peaks(e: &mut Engine) -> Result<Value> {
    let mut pre = e.project.clone();
    pre.master_effects.clear();
    e.bank.sync(&pre.samples);
    let mix = crate::render::render(&pre, &e.bank, &crate::render::RenderOptions { keep_stems: true, ..Default::default() })?;
    let mut done = Vec::new();
    for st in &mix.bus_stems {
        let is_group = e.project.tracks.iter().any(|t| t.output.as_deref() == Some(st.name.as_str()));
        if !is_group {
            continue;
        }
        let x = mono(&st.left, &st.right);
        let pk = 20.0 * x.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-9).log10();
        let blk = (0.4 * SR) as usize;
        let ms: Vec<f32> = x.chunks(blk).map(|c| c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32).collect();
        let loud = ms.iter().cloned().fold(0.0f32, f32::max);
        let act: Vec<f32> = ms.iter().cloned().filter(|m| *m > loud * 1e-3).collect();
        if act.is_empty() {
            continue;
        }
        let rms = 10.0 * (act.iter().sum::<f32>() / act.len() as f32).max(1e-12).log10();
        let crest = pk - rms;
        if crest > BUS_MAX_CREST_DB + 0.5 {
            let ceiling = rms + BUS_MAX_CREST_DB;
            let bi = e.project.buses.iter().position(|b| b.name == st.name).ok_or_else(|| anyhow!("bus {}", st.name))?;
            let has = serde_json::to_value(&e.project.buses[bi].effects).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default().iter().any(|f| f["type"] == "limiter");
            if !has {
                e.call_nested("add_effect", &json!({"track": st.name, "type": "limiter", "params": {"ceiling_db": ceiling, "release_ms": 40.0, "true_peak": false}}), "mix_rules")?;
                done.push(json!({"bus": st.name, "crest_db": r1(crest), "limiter_ceiling_db": r1(ceiling)}));
            }
        }
    }
    Ok(json!(done))
}

fn eq_cut(e: &mut Engine, owner: &str, kind: &str, freq: f32, db: f32) -> Result<()> {
    if db < 0.3 {
        return Ok(());
    }
    e.call_nested(
        "add_effect",
        &json!({"track": owner, "type": "parametric_eq", "params": {"bands": [{"kind": kind, "freq": freq, "gain_db": -db.min(9.0), "q": 0.7}]}}),
        "mix_rules",
    )?;
    Ok(())
}

/// The top end, measured on the parts and fixed on the parts: where a
/// vocal exists no beat part may sit within `beat_under_vocal_presence_db`
/// of it at 2-5 kHz (a bell cut at 3 kHz, then the fader for what is left);
/// the whole beat may not put more than `presence_max_share_db` /
/// `air_max_share_db` of the mix in 2-5 / 5-10 kHz (a 3 kHz bell / a 6 kHz
/// shelf on the parts that carry it, buses included).
pub fn tame_top(e: &mut Engine, voice: Option<&str>, t: &Targets, shift_db: f32) -> Result<Value> {
    let mut pre = e.project.clone();
    pre.master_effects.clear();
    e.bank.sync(&pre.samples);
    let mix = crate::render::render(&pre, &e.bank, &crate::render::RenderOptions { keep_stems: true, ..Default::default() })?;
    let m = mono(&mix.left, &mix.right);
    let total = band_energy_db(&m, 20.0, 20000.0);
    let pres_share = band_energy_db(&m, 2000.0, 5000.0) - total;
    let air_share = band_energy_db(&m, 5000.0, 10000.0) - total;
    let mut parts: Vec<(String, f32, f32, f32)> = Vec::new(); // name, 2-5k dB, 5-10k dB, full dB
    let mut vocal_p = None;
    for st in mix.stems.iter() {
        let x = mono(&st.left, &st.right);
        let p = band_energy_db(&x, 2000.0, 5000.0);
        let a = band_energy_db(&x, 5000.0, 10000.0);
        let f = band_energy_db(&x, 20.0, 20000.0);
        if voice.map(|v| v.eq_ignore_ascii_case(&st.name)).unwrap_or(false) {
            vocal_p = Some(p);
        } else if f > -90.0 {
            parts.push((st.name.clone(), p, a, f));
        }
    }
    let grouped: Vec<String> = e.project.tracks.iter().filter(|tr| tr.output.is_some()).map(|tr| tr.name.clone()).collect();
    let mut done = Vec::new();
    // 1. the words' band belongs to the vocal (first pass only)
    if let (Some(vp), true) = (vocal_p, shift_db == 0.0) {
        let ceiling = vp - t.beat_under_vocal_presence_db;
        for (name, p, _, _) in &parts {
            let over = p - ceiling;
            if over > 0.5 {
                // a bell, not the fader: the part keeps its body and level
                let cut = over.min(MAX_TOP_CUT_DB);
                eq_cut(e, name, "bell", 3000.0, cut)?;
                done.push(json!({"part": name, "over_vocal_presence_db": r1(over - t.beat_under_vocal_presence_db), "cut_3k_db": r1(cut)}));
            }
        }
    }
    // the shares are judged on the mix as it is after step 1
    let (pres_share, air_share, parts) = if done.is_empty() {
        (pres_share, air_share, parts)
    } else {
        let mut pre = e.project.clone();
        pre.master_effects.clear();
        let mix = crate::render::render(&pre, &e.bank, &crate::render::RenderOptions { keep_stems: true, ..Default::default() })?;
        let m = mono(&mix.left, &mix.right);
        let total = band_energy_db(&m, 20.0, 20000.0);
        let parts: Vec<(String, f32, f32, f32)> = mix
            .stems
            .iter()
            .filter(|st| !voice.map(|v| v.eq_ignore_ascii_case(&st.name)).unwrap_or(false))
            .map(|st| {
                let x = mono(&st.left, &st.right);
                (st.name.clone(), band_energy_db(&x, 2000.0, 5000.0), band_energy_db(&x, 5000.0, 10000.0), band_energy_db(&x, 20.0, 20000.0))
            })
            .filter(|p| p.3 > -90.0)
            .collect();
        (band_energy_db(&m, 2000.0, 5000.0) - total, band_energy_db(&m, 5000.0, 10000.0) - total, parts)
    };
    // 2. the beat's share of the top end (biggest carriers first)
    // (only when asked with a shift: the master tone stage owns the overall
    // tilt; this per-part spread is for a caller that wants it)
    for (band, share, max, kind, freq) in [("2-5k", pres_share, t.presence_max_share_db - shift_db, "bell", 3000.0f32), ("5-10k", air_share, t.air_max_share_db - shift_db, "high_shelf", 6000.0f32)] {
        if shift_db == 0.0 {
            break;
        }
        let over = share - max;
        if over <= 0.3 {
            continue;
        }
        let idx = if band == "2-5k" { 1 } else { 2 };
        let mut ranked: Vec<&(String, f32, f32, f32)> = parts.iter().collect();
        ranked.sort_by(|a, b| { let (x, y) = if idx == 1 { (a.1, b.1) } else { (a.2, b.2) }; y.partial_cmp(&x).unwrap() });
        let top = if idx == 1 { ranked.first().map(|r| r.1) } else { ranked.first().map(|r| r.2) }.unwrap_or(-120.0);
        for r in ranked.iter().take(4) {
            let lvl = if idx == 1 { r.1 } else { r.2 };
            // the carriers within 6 dB of the biggest share the cut
            if lvl < top - 6.0 {
                continue;
            }
            eq_cut(e, &r.0, kind, freq, (over + 0.5).min(MAX_TOP_CUT_DB))?;
            done.push(json!({"part": r.0, "band": band, "cut_db": r1((over + 0.5).min(MAX_TOP_CUT_DB)), "mix_share_db": r1(share), "target_db": max}));
        }
        // a drum bus that adds grit adds air: it shares the 5-10k cut
        if idx == 2 && e.project.buses.iter().any(|b| b.name == "drums") && grouped.iter().any(|g| ranked.iter().take(4).any(|r| &r.0 == g)) {
            eq_cut(e, "drums", kind, freq, (over * 0.5).min(3.0))?;
            done.push(json!({"part": "drums (bus)", "band": band, "cut_db": r1((over * 0.5).min(3.0))}));
        }
    }
    Ok(json!({"mix_2_5k_share_db": r1(pres_share), "mix_5_10k_share_db": r1(air_share), "vocal_2_5k_db": vocal_p.map(r1), "changes": done}))
}

/// (2-5 kHz, 5-10 kHz) against 250 Hz-2 kHz, dB.
pub fn tilt_db(m: &[f32]) -> (f32, f32) {
    let body = band_energy_db(m, 250.0, 2000.0);
    (band_energy_db(m, 2000.0, 5000.0) - body, band_energy_db(m, 5000.0, 10000.0) - body)
}

/// The master tone stage's marker frequencies (one EQ this module owns).
const TONE_PRES_HZ: f32 = 3150.0;
const TONE_AIR_HZ: f32 = 7500.0;

fn tone_gains(p: &Project) -> Option<(usize, f32, f32)> {
    p.master_effects.iter().enumerate().find_map(|(i, f)| {
        let v = serde_json::to_value(f).ok()?;
        let b = v["bands"].as_array()?;
        if v["type"] == "parametric_eq" && b.len() == 2 && (b[0]["freq"].as_f64()? as f32 - TONE_PRES_HZ).abs() < 1.0 {
            Some((i, b[0]["gain_db"].as_f64()? as f32, b[1]["gain_db"].as_f64()? as f32))
        } else {
            None
        }
    })
}

/// Master tone: one bell at 3.15 kHz and one shelf at 7.5 kHz on the master,
/// set so the mix's tilt meets the targets with `margin_db` to spare (the
/// delivery limiter adds ~1-2 dB of top). Cuts only, at most 8 dB each.
pub fn tone_stage(e: &mut Engine, t: &Targets, margin_db: f32) -> Result<Value> {
    let full = e.mix()?;
    let m = mono(&full.left, &full.right);
    let (pt, at) = tilt_db(&m);
    let (idx, gp, ga) = tone_gains(&e.project).map(|(i, a, b)| (Some(i), a, b)).unwrap_or((None, 0.0, 0.0));
    let np = (gp - (pt - (t.presence_tilt_max_db - margin_db)).max(0.0)).clamp(-8.0, 0.0);
    let na = (ga - (at - (t.air_tilt_max_db - margin_db)).max(0.0)).clamp(-8.0, 0.0);
    if (np - gp).abs() < 0.1 && (na - ga).abs() < 0.1 {
        return Ok(json!({"tilt_2_5k_db": r1(pt), "tilt_5_10k_db": r1(at), "changed": false}));
    }
    let eq: crate::fx::Effect = serde_json::from_value(json!({"type": "parametric_eq", "bands": [
        {"kind": "bell", "freq": TONE_PRES_HZ, "gain_db": np, "q": 0.8},
        {"kind": "high_shelf", "freq": TONE_AIR_HZ, "gain_db": na, "q": 0.7}
    ]}))?;
    match idx {
        Some(i) => e.project.master_effects[i] = eq,
        None => e.project.master_effects.insert(0, eq),
    }
    e.project.ensure_fx_ids();
    Ok(json!({"tilt_2_5k_db": r1(pt), "tilt_5_10k_db": r1(at), "bell_3k_db": r1(np), "shelf_7k5_db": r1(na), "changed": true}))
}

/// Hooks over verses on a render (mix RMS, mean of each kind).
pub fn lift_db(levels: &[SectionLevel]) -> Option<f32> {
    let mean = |f: &dyn Fn(&str) -> bool| -> Option<f32> {
        let v: Vec<f32> = levels.iter().filter(|x| f(&x.kind)).map(|x| x.mix_rms_db).collect();
        (!v.is_empty()).then(|| v.iter().sum::<f32>() / v.len() as f32)
    };
    Some(mean(&|k| is_lift_kind(k))? - mean(&|k| k == "verse")?)
}

/// Make hooks lift over verses by at least the target on the pre-delivery
/// mix (delivery keeps ~80% of it): verses step down on the master's
/// section automation. Returns what it did.
pub fn ensure_drop_lift(e: &mut Engine, t: &Targets) -> Result<Value> {
    let m = measure_sections(e, None)?;
    let Some(l) = lift_db(&m) else {
        return Ok(json!({"skipped": "no hook/drop or no verse in the structure"}));
    };
    // aim a bit over the floor: delivery limiting takes some of it back
    let want = t.drop_lift_db[0] + 1.0;
    if l >= want {
        return Ok(json!({"lift_db": r1(l), "changed": false}));
    }
    let need = (want - l).min(4.0);
    let spans = section_spans(&e.project);
    let mut names: Vec<String> = spans.iter().filter(|s| s.kind == "verse").map(|s| s.name.clone()).collect();
    names.dedup();
    for n in &names {
        let cur = crate::tools_studio::static_value(&e.project, "master", "volume").unwrap_or(0.0);
        let at = section_master_db(&e.project, &spans, n).unwrap_or(cur);
        e.call_nested("set_section_mix", &json!({"section": n, "track": "master", "volume_db": at - need, "all_occurrences": false}), "mix_rules")?;
    }
    let m2 = measure_sections(e, None)?;
    Ok(json!({"lift_db_before": r1(l), "verses_down_db": r1(need), "lift_db": lift_db(&m2).map(r1), "changed": true}))
}

fn section_master_db(p: &Project, spans: &[Span], name: &str) -> Option<f32> {
    let s = spans.iter().find(|s| s.name == name)?;
    let lane = p.automation.iter().find(|l| l.target == "master" && l.param == "volume")?;
    let mid = 0.5 * (s.start_beat + s.end_beat);
    lane.points.iter().filter(|pt| pt.beat <= mid).last().map(|pt| pt.value)
}

/// One target, measured.
fn chk(name: &str, target: String, measured: Value, pass: bool, fix: &str) -> Value {
    let mut v = json!({"check": name, "target": target, "measured": measured, "pass": pass});
    if !pass {
        v["fix"] = json!(fix);
    }
    v
}

/// Check a delivered file (decoded back) and the pre-delivery section
/// levels against the targets. Every song render returns this.
pub fn check(file: &std::path::Path, sections_pre: &[SectionLevel], spans: &[Span], bpm: f32, t: &Targets) -> Result<Value> {
    let (l, r, sr) = crate::samples::decode_stereo_native(file).map_err(|e| anyhow!("could not read back {}: {e}", file.display()))?;
    let ld = crate::analysis::loudness_at(&l, &r, sr as f32);
    let tp = crate::analysis::true_peak(&l).max(crate::analysis::true_peak(&r));
    let tp_db = 20.0 * tp.max(1e-9).log10();
    let m = mono(&l, &r);
    let bs = 60.0 / bpm;
    let mut sec = Vec::new();
    for s in spans {
        let a = ((s.start_beat * bs * sr as f32) as usize).min(m.len());
        let b = ((s.end_beat * bs * sr as f32) as usize).min(m.len());
        if b > a + 1000 {
            sec.push(SectionLevel { name: s.name.clone(), kind: s.kind.clone(), start_s: s.start_beat * bs, mix_rms_db: rms_db(&m[a..b]), vocal_over_beat_db: None, presence_over_beat_db: None });
        }
    }
    let lift = lift_db(&sec);
    let mut checks = Vec::new();
    let vob: Vec<(String, f32)> = sections_pre.iter().filter_map(|x| x.vocal_over_beat_db.map(|v| (x.name.clone(), v))).collect();
    if !vob.is_empty() {
        let lo = vob.iter().map(|x| x.1).fold(f32::MAX, f32::min);
        let hi = vob.iter().map(|x| x.1).fold(f32::MIN, f32::max);
        checks.push(chk(
            "vocal_over_beat",
            format!(">= {} dB and <= {} dB in every section with vocal", t.vocal_over_beat_db, t.vocal_over_beat_max_db),
            json!({"min_db": r1(lo), "max_db": r1(hi), "by_section": vob.iter().map(|(n, v)| json!([n, r1(*v)])).collect::<Vec<_>>()}),
            lo >= t.vocal_over_beat_db - 0.25 && hi <= t.vocal_over_beat_max_db + 0.25,
            "song_patch /mix/vocal_over_beat_db, or re-render: the vocal balance loop runs on every render",
        ));
    }
    if let Some(lv) = lift {
        checks.push(chk(
            "drop_lift",
            format!("hooks {}..{} dB over verses (RMS, delivered file)", t.drop_lift_db[0], t.drop_lift_db[1]),
            json!(r1(lv)),
            lv >= t.drop_lift_db[0] - 0.25,
            "song_patch /mix/drop_lift_db higher, or give the hook more instruments in /instruments",
        ));
    }
    let (pt, at) = tilt_db(&m);
    let tot = band_energy_db(&m, 20.0, 20000.0);
    let pres = band_energy_db(&m, 2000.0, 5000.0) - tot;
    checks.push(chk(
        "not_harsh",
        format!("2-5 kHz <= {} dB and 5-10 kHz <= {} dB against 250 Hz-2 kHz; 2-5 kHz >= {} dB of the mix (not muffled)", t.presence_tilt_max_db, t.air_tilt_max_db, t.presence_min_share_db),
        json!({"tilt_2_5k_db": r1(pt), "tilt_5_10k_db": r1(at), "share_2_5k_db": r1(pres)}),
        pt <= t.presence_tilt_max_db + 0.3 && at <= t.air_tilt_max_db + 0.3 && pres >= t.presence_min_share_db,
        "the master tone stage takes the top down on every render; a darker preset in /instruments helps more",
    ));
    checks.push(chk(
        "loudness",
        format!("{} LUFS +/- {}", t.lufs, t.lufs_tolerance),
        json!(r1(ld.integrated_lufs)),
        (ld.integrated_lufs - t.lufs).abs() <= t.lufs_tolerance,
        "song_patch /master/lufs; a quieter target keeps more punch",
    ));
    checks.push(chk(
        "true_peak",
        format!("<= {} dBTP (decoded file)", t.true_peak_max_db),
        json!(r1(tp_db)),
        tp_db <= t.true_peak_max_db + 0.05,
        "song_patch /master/true_peak lower",
    ));
    let all = checks.iter().all(|c| c["pass"] == true);
    Ok(json!({
        "all_pass": all,
        "checks": checks,
        "delivered_sections": sec.iter().map(|x| json!({"name": x.name, "rms_db": r1(x.mix_rms_db)})).collect::<Vec<_>>(),
        "loudness_range_lu": r1(ld.loudness_range_lu),
        "measured_on": "loudness, peak and lift on the delivered file decoded back; vocal over beat on the final mix before the master chain and delivery (both apply one gain to the whole mix, so the gap carries through)",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_inside_one_call_renders_fresh_and_tone_stage_darkens() {
        // a loop inside ONE tool call keeps one revision; the mix must still
        // follow the project (demos 5/6 re-delivered stale audio when it didn't)
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_tone_stage_tests"));
        e.call("generate_beat", &serde_json::json!({"style": "boom_bap", "bars": 2, "seed": 5})).unwrap();
        let rev = e.revision;
        let m0 = e.mix().unwrap();
        let rms = |m: &crate::render::Mix| (m.left.iter().map(|x| x * x).sum::<f32>() / m.left.len().max(1) as f32).sqrt();
        let r0 = rms(&m0);
        let (p0, _) = tilt_db(&mono(&m0.left, &m0.right));
        e.project.master_volume_db -= 6.0;
        let r1 = rms(&e.mix().unwrap());
        assert_eq!(e.revision, rev);
        assert!(r1 < r0 * 0.9, "mix ignored a project change made inside one call: {r0} -> {r1}");
        e.project.master_volume_db += 6.0;
        let mut t = Targets::default();
        t.presence_tilt_max_db = p0 - 3.0;
        let r = tone_stage(&mut e, &t, 0.0).unwrap();
        assert_eq!(r["changed"], true);
        let m2 = e.mix().unwrap();
        let (p2, _) = tilt_db(&mono(&m2.left, &m2.right));
        assert!(p2 < p0 - 1.0, "tone stage did not darken the next render: {p0} -> {p2}");
    }

    #[test]
    fn kinds_from_names() {
        assert_eq!(kind_of("hook2"), "hook");
        assert_eq!(kind_of("verse_1"), "verse");
        assert_eq!(kind_of("chorus"), "hook");
        assert_eq!(kind_of("outro1"), "outro");
    }

    #[test]
    fn targets_defaults_are_the_documented_ones() {
        let t = Targets::default();
        assert_eq!(t.vocal_over_beat_db, 6.0);
        assert_eq!(t.lufs, -10.0);
        assert_eq!(t.true_peak_max_db, -1.0);
        assert!(t.vocal_aim_db() > t.vocal_over_beat_db && t.vocal_aim_db() < t.vocal_over_beat_max_db);
    }

    #[test]
    fn pocket_follows_a_vocal_added_after_the_beat() {
        let mut e = Engine::new(std::env::temp_dir().join("bb_mixrules_pocket"));
        e.call("add_track", &json!({"name": "chords", "preset": "pad"})).unwrap();
        assert!(ensure_vocal_pocket(&mut e, "test").unwrap().is_empty(), "no voice yet: no pocket");
        e.call("add_track", &json!({"name": "vocal", "instrument": {"type": "sampler", "sample": "", "one_shot": true}})).unwrap();
        assert!(has_pocket(&e.project, "chords"), "adding a vocal track brings the pocket");
        assert!(ensure_vocal_pocket(&mut e, "test").unwrap().is_empty(), "idempotent");
    }
}
