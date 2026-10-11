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
    e.call_nested("add_effect", &json!({"track": track, "type": "compressor", "params": {"threshold_db": c.comp_threshold_db, "ratio": c.comp_ratio, "attack_ms": 2.0, "release_ms": 100.0, "makeup_db": c.comp_makeup_db, "knee_db": 6.0}}), source)?;
    steps.push(format!("compressor {}:1", c.comp_ratio));
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
    let full = e.mix()?;
    let bs = 60.0 / e.project.bpm;
    let m = mono(&full.left, &full.right);
    let voc = match voice_track {
        Some(t) => {
            let vi = e.project.track_index(t)?;
            let mut solo = e.project.clone();
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
    let mut offs: Vec<(f32, f32)> = spans.iter().map(|s| (s.start_beat, 0.0)).collect();
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
        "measured_on": "loudness, peak and lift on the delivered file decoded back; vocal over beat on the final mix before delivery limiting (delivery applies one gain to both)",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

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
