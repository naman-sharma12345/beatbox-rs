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
    /// Calibrated on the hand remix the person approved (-16.5 / -18.3),
    /// set a little darker now that only the beat is darkened (the vocal
    /// keeps its own top): -17 / -18.5.
    pub presence_tilt_max_db: f32,
    /// Tilt: 5-10 kHz against 250 Hz-2 kHz, at most (dB).
    pub air_tilt_max_db: f32,
    /// Where a vocal sings, every beat part sits at least this far under the
    /// vocal in 2-5 kHz (the words' band belongs to the voice).
    pub beat_under_vocal_presence_db: f32,
    /// The delivered file's 2-5 kHz level minus its integrated loudness
    /// (4th-order high-pass + 4th-order low-pass, mean power, dB minus LUFS),
    /// [darkest, brightest]: measured the way a person checks a master with
    /// ffmpeg. Calibrated on the hand remix the person approved (song2:
    /// -22.4); under -23.0 the beat starts to read dull.
    pub presence_rel_lufs_db: [f32; 2],
    /// The same for 5-10 kHz, [darkest, brightest] (song2: -24.2). Under
    /// the floor the top reads dull: hats and cymbals lost their sparkle
    /// (B21's song came out -27.9 with the hats 10.7 dB down).
    pub air_rel_lufs_db: [f32; 2],
    /// Quiet sections (intro, outro, breakdown) sit at most this far under
    /// the verses (mix RMS, pre-delivery); further down they read as a gap.
    pub quiet_section_floor_db: f32,
    /// Where the vocal sings, its 1-4 kHz (the words) sits at least this far
    /// over the beat's 1-4 kHz in every section.
    pub vocal_presence_over_beat_db: f32,
}

impl Default for Targets {
    fn default() -> Self {
        Targets {
            // vocal first (Naman, 2026-10-12, on a mix measured 7.6-8.3 dB
            // over the beat: "the instruments are so loud the voice is
            // getting overpowered"): a user's vocal sits 11+ dB over the beat
            vocal_over_beat_db: 11.0,
            vocal_over_beat_max_db: 17.0,
            drop_lift_db: [2.0, 3.0],
            lufs: -10.0,
            lufs_tolerance: 1.0,
            true_peak_max_db: -1.0,
            presence_max_share_db: -19.0,
            air_max_share_db: -20.5,
            presence_min_share_db: -24.0,
            presence_tilt_max_db: -17.0,
            air_tilt_max_db: -18.5,
            beat_under_vocal_presence_db: 6.0,
            presence_rel_lufs_db: [-23.0, -22.4],
            air_rel_lufs_db: [-26.0, -24.3],
            quiet_section_floor_db: 9.0,
            vocal_presence_over_beat_db: 6.0,
        }
    }
}

impl Targets {
    /// Where the delivered 2-5 kHz level (minus LUFS) aims: the middle of
    /// its window.
    pub fn presence_rel_aim_db(&self) -> f32 {
        0.5 * (self.presence_rel_lufs_db[0] + self.presence_rel_lufs_db[1])
    }

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
/// Vocal first: the beds duck clearly while the words sound (0.3 left the
/// vocal buried under piano/organ on the 2026-10-12 brutal test).
pub const DUCK_AMOUNT: f32 = 0.55;
/// Vocal first: the melodic beds' low end is trimmed under a vocal (the
/// low-cut frequency, Hz), so their body does not mask the voice's.
pub const BED_LOW_TRIM_HZ: f32 = 150.0;

/// Dark-source fix aims (vocal_clarity::tone, dB against the 500 Hz-2 kHz
/// core): a clear vocal's 2-5 kHz sits about here, and its 5-8 kHz here.
/// A take more than 3 dB under an aim gets a corrective boost (at most
/// 6 dB) on its own track, before the de-esser.
pub const VOCAL_PRESENCE_AIM_DB: f32 = -10.0;
pub const VOCAL_BRILLIANCE_AIM_DB: f32 = -17.0;

/// A dull recording (a phone voice note): the presence bell and air shelf
/// (dB) that bring it toward a clear vocal, with the measured tone. None
/// when the take is not dark.
pub fn dark_source_fix(x: &[f32]) -> Option<(f32, f32, Value)> {
    if x.len() < (SR * 1.0) as usize {
        return None;
    }
    let t = crate::vocal_clarity::tone(x);
    let g = |k: &str| t[k].as_f64().unwrap_or(0.0) as f32;
    let (pres, bril) = (g("presence_2k_5k"), g("brilliance_5k_8k"));
    let need = |aim: f32, v: f32| if v < aim - 3.0 { (aim - v).min(6.0) } else { 0.0 };
    let (pg, ag) = (need(VOCAL_PRESENCE_AIM_DB, pres), need(VOCAL_BRILLIANCE_AIM_DB, bril));
    if pg <= 0.0 && ag <= 0.0 {
        return None;
    }
    Some(((pg * 10.0).round() / 10.0, (ag * 10.0).round() / 10.0, t))
}

/// Up to `max_s` seconds of the audio the clips on `track` play (their own
/// samples, no FX): what the vocal chain measures the take on.
pub fn track_clip_audio(e: &mut Engine, track: &str, max_s: f32) -> Option<Vec<f32>> {
    e.bank.sync(&e.project.samples);
    let cap = (max_s * SR) as usize;
    let mut out = Vec::new();
    for c in e.project.audio_clips.iter().filter(|c| c.track.eq_ignore_ascii_case(track)) {
        let Some(d) = e.bank.get(&c.sample) else { continue };
        let off = ((c.offset_s.max(0.0) * SR) as usize).min(d.len());
        let len = c.length_s.map(|l| ((l.max(0.0) * SR) as usize).min(d.len() - off)).unwrap_or(d.len() - off);
        let take = len.min(cap - out.len());
        out.extend_from_slice(&d[off..off + take]);
        if out.len() >= cap {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The beat's top end starts at the source. Measured on the desi hip-hop
/// demo (B20): before any EQ the beat's 5-10 kHz was ~5.5 dB and its 2-5 kHz
/// ~3.5 dB over what the approved reference master carries, and the hats,
/// the snare's noise and the shaker were almost all of it (the hats sat only
/// ~7 dB under the snare). So in a vocal song the bright parts are voiced
/// softer and sit lower before anything is measured, and the beat-bus tone
/// is left with a small cut (a deep bus cut reads boxy).
///
/// Per role (the track name, or the drum kind): the brightest a synth drum's
/// `tone` may be (-1 dark .. 1 bright; sampled parts keep their sample), and
/// a 7.5 kHz high shelf on the part (dB; 0 = none) as `source_tone`, set,
/// never stacked.
pub const SOURCE_TONE: &[(&str, Option<f32>, f32)] = &[
    ("hat", Some(-0.3), 0.0),
    ("open_hat", Some(-0.4), 0.0),
    ("cymbal", Some(-0.4), 0.0),
    ("crash", Some(-0.4), 0.0),
    ("ride", Some(-0.4), 0.0),
    ("perc", None, 0.0),
    ("shaker", None, 0.0),
    ("snare", Some(0.0), -6.0),
    ("clap", Some(0.0), -4.0),
];
/// Levels in a vocal song: each of these roles sits at least this far under
/// the snare (full band, whole song). Only ever turns a part down.
pub const UNDER_SNARE_DB: &[(&str, f32)] = &[
    ("hat", 13.0),
    ("open_hat", 13.0),
    ("cymbal", 13.0),
    ("crash", 12.0),
    ("ride", 13.0),
    ("perc", 12.0),
    ("shaker", 12.0),
];
/// An instrumental (no vocal) keeps its bright parts this much nearer the
/// snare than a vocal song does: no words need the room, but the same
/// voicing keeps the beat bus from a deep 3 kHz cut (beat-only songs took
/// -12 dB at 3k on the bus before the source rules ran for them).
pub const INSTRUMENTAL_UNDER_SNARE_RELIEF_DB: f32 = 2.0;
/// The drum bus's grit (distortion mix) at most, in a vocal song: drive on
/// the drum bus adds ~1 dB of 5-10 kHz the parts never had.
pub const DRUM_GRIT_MAX_MIX: f32 = 0.15;
/// The effect id of a part's source shelf.
pub const SOURCE_TONE_ID: &str = "source_tone";

fn source_role(t: &crate::project::Track) -> Option<&'static str> {
    use crate::instruments::{DrumKind, Instrument};
    let n = t.name.to_lowercase();
    let base = n.trim_end_matches(|c: char| c.is_ascii_digit() || c == '_');
    if let Some(r) = SOURCE_TONE.iter().find(|r| r.0 == base) {
        return Some(r.0);
    }
    match &t.instrument {
        Instrument::Drum(d) => match d.kind {
            DrumKind::ClosedHat => Some("hat"),
            DrumKind::OpenHat => Some("open_hat"),
            DrumKind::Crash => Some("crash"),
            DrumKind::Shaker => Some("shaker"),
            DrumKind::Snare => Some("snare"),
            DrumKind::Clap => Some("clap"),
            _ => None,
        },
        _ => None,
    }
}

/// Voice the beat's bright parts softer at the source and set the hat /
/// cymbal / shaker levels under the snare (SOURCE_TONE, UNDER_SNARE_DB,
/// DRUM_GRIT_MAX_MIX). With a vocal or without one (an instrumental keeps
/// its bright parts INSTRUMENTAL_UNDER_SNARE_RELIEF_DB nearer the snare);
/// idempotent: a second pass changes nothing. Costs one render (it fills the
/// track cache the next steps reuse). `levels[].down_db` is what the level
/// rule took, so the delivery loop can give some back if the delivered top
/// end reads dull (Targets::air_rel_lufs_db[0]).
pub fn soften_beat_source(e: &mut Engine, voice: Option<&str>) -> Result<Value> {
    soften_beat_source_with(e, voice, &std::collections::BTreeMap::new())
}

/// `soften_beat_source` with the song's own level offsets (`/mix/levels`,
/// role -> dB): a bright role's offset moves where the rule puts it (+2 on
/// "hat" = the hats sit 2 dB nearer the snare than the rule's distance), so
/// the level rule and the song's levels have one owner: a dull top end
/// raises `/mix/levels/hat` and the rule keeps it on every rebuild instead
/// of pulling it back under the snare.
pub fn soften_beat_source_with(e: &mut Engine, voice: Option<&str>, offsets: &std::collections::BTreeMap<String, f32>) -> Result<Value> {
    use crate::instruments::Instrument;
    let relief = if voice.is_some() { 0.0 } else { INSTRUMENTAL_UNDER_SNARE_RELIEF_DB };
    let is_v = |n: &str| voice.map(|v| n.eq_ignore_ascii_case(v)).unwrap_or(false) || is_voice_name(n);
    let mut done = Vec::new();
    // 1. voicing: tone caps and source shelves (before the render, so the
    //    cache holds the softer parts)
    for ti in 0..e.project.tracks.len() {
        if is_v(&e.project.tracks[ti].name) {
            continue;
        }
        let Some(role) = source_role(&e.project.tracks[ti]) else { continue };
        let &(_, tone_max, shelf) = SOURCE_TONE.iter().find(|r| r.0 == role).expect("role from the table");
        let t = &mut e.project.tracks[ti];
        let mut what = json!({"part": t.name, "role": role});
        // a bright part the song turned up (a dull top end's fix) may also
        // be brighter: 0.05 tone per dB of offset, at most +0.3 (sparkle
        // lives over 5 kHz, so the top lifts more than the 2-5k presence)
        let bright = UNDER_SNARE_DB.iter().any(|r| r.0 == role);
        let lift = if bright { (0.05 * offsets.get(role).copied().unwrap_or(0.0).max(0.0)).min(0.3) } else { 0.0 };
        let tone_max = tone_max.map(|m| (m + lift).min(1.0));
        if let (Instrument::Drum(d), Some(mx)) = (&mut t.instrument, tone_max) {
            if d.tone > mx + 1e-3 {
                what["tone"] = json!([r1(d.tone), mx]);
                d.tone = mx;
            }
        }
        if shelf < -0.05 {
            let have = t.effects.iter().position(|f| f.id() == SOURCE_TONE_ID);
            let cur = have.and_then(|i| serde_json::to_value(&t.effects[i]).ok()).and_then(|x| x["bands"][0]["gain_db"].as_f64()).map(|x| x as f32);
            if cur.map(|c| (c - shelf).abs() > 0.05).unwrap_or(true) {
                let mut fx: crate::fx::Effect = serde_json::from_value(json!({"type": "parametric_eq", "bands": [{"kind": "high_shelf", "freq": TONE_AIR_HZ, "gain_db": shelf, "q": 0.7}]}))?;
                fx.set_id(SOURCE_TONE_ID);
                match have {
                    Some(i) => t.effects[i] = fx,
                    None => t.effects.push(fx),
                }
                what["shelf_7k5_db"] = json!(shelf);
            }
        }
        if what.as_object().map(|o| o.len() > 2).unwrap_or(false) {
            done.push(what);
        }
    }
    let mut grit = Vec::new();
    for b in e.project.buses.iter_mut() {
        if !b.name.to_lowercase().contains("drum") {
            continue;
        }
        for f in b.effects.iter_mut() {
            let mut x = serde_json::to_value(&*f)?;
            if x["type"] == "distortion" && x["mix"].as_f64().map(|m| m as f32 > DRUM_GRIT_MAX_MIX + 1e-3).unwrap_or(false) {
                grit.push(json!({"bus": b.name, "mix": [r1(x["mix"].as_f64().unwrap_or(0.0) as f32), DRUM_GRIT_MAX_MIX]}));
                x["mix"] = json!(DRUM_GRIT_MAX_MIX);
                *f = serde_json::from_value(x)?;
            }
        }
    }
    e.project.ensure_fx_ids();
    // 2. levels: the bright parts under the snare (faders: not part of the
    //    cached sound, so this re-sums)
    let mut levels = Vec::new();
    let has_snare = e.project.tracks.iter().any(|t| source_role(t) == Some("snare") && !is_v(&t.name));
    if has_snare {
        let mut pre = e.project.clone();
        pre.master_effects.clear();
        e.bank.sync(&pre.samples);
        let mix = e.render_stems(&pre)?;
        let lvl = |name: &str| mix.stems.iter().find(|s| s.name == name).map(|s| s.band_db(20.0, 20000.0));
        let snare = e.project.tracks.iter().filter(|t| source_role(t) == Some("snare") && !is_v(&t.name)).filter_map(|t| lvl(&t.name)).fold(-200.0f32, f32::max);
        for ti in 0..e.project.tracks.len() {
            let t = &e.project.tracks[ti];
            if is_v(&t.name) {
                continue;
            }
            let Some(role) = source_role(t) else { continue };
            let Some(&(_, under)) = UNDER_SNARE_DB.iter().find(|r| r.0 == role) else { continue };
            let Some(l) = lvl(&t.name) else { continue };
            let offset = offsets.get(role).copied().unwrap_or(0.0);
            let over = l - (snare - (under - relief - offset));
            if snare > -90.0 && l > -90.0 && over > 0.3 {
                let name = t.name.clone();
                e.project.tracks[ti].volume_db -= over;
                levels.push(json!({"part": name, "under_snare_db": r1(snare - l), "down_db": r1(over), "level_offset_db": r1(offset)}));
            }
        }
    }
    let changed = !done.is_empty() || !grit.is_empty() || !levels.is_empty();
    let mode = if voice.is_some() { "vocal song" } else { "instrumental" };
    Ok(json!({"changed": changed, "mode": mode, "voicing": done, "grit": grit, "levels": levels}))
}

/// Most the delivery loop gives back to a part the level rule turned down,
/// per round (dB); never more than the rule took.
pub const AIR_GIVE_BACK_STEP_MAX_DB: f32 = 4.0;

/// The top end read dull on the delivered file (5-10 kHz under
/// `Targets::air_rel_lufs_db[0]`): give the bright parts back part of what
/// `soften_beat_source` took from them (`taken`: part -> dB still owed),
/// all by the same amount (1.5x the shortfall: the parts carry most of
/// 5-10k, not all of it); aims at the middle of the air window. Returns
/// what moved (empty when nothing is owed).
pub fn give_back_air(e: &mut Engine, taken: &mut Vec<(String, f32)>, air_rel: f32, t: &Targets) -> Result<Vec<Value>> {
    let aim = 0.5 * (t.air_rel_lufs_db[0] + t.air_rel_lufs_db[1]);
    let need = (aim - air_rel).max(0.0);
    let mut moved = Vec::new();
    if need < 0.2 {
        return Ok(moved);
    }
    for (name, owed) in taken.iter_mut() {
        // the part carries most but not all of the band: a little more than the need
        let up = (need * 1.5).min(*owed).min(AIR_GIVE_BACK_STEP_MAX_DB);
        if up < 0.1 {
            continue;
        }
        let Ok(i) = e.project.track_index(name) else { continue };
        e.project.tracks[i].volume_db += up;
        *owed -= up;
        moved.push(json!({"part": name, "up_db": r1(up), "still_owed_db": r1(*owed)}));
    }
    Ok(moved)
}

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
    /// Dark-source fix: when the take measures dull (2-5 kHz / 5-8 kHz far
    /// under a clear vocal, e.g. a phone voice note), a corrective presence
    /// bell (3.5 kHz) and air shelf (7 kHz) go on the vocal before the
    /// de-esser. false keeps the take's own tone.
    pub brighten: bool,
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
            brighten: true,
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
    if c.brighten {
        if let Some((pg, ag, t)) = track_clip_audio(e, track, 90.0).as_deref().and_then(dark_source_fix) {
            let mut bands = Vec::new();
            if pg > 0.0 {
                bands.push(json!({"kind": "bell", "freq": 3500.0, "gain_db": pg, "q": 0.8}));
            }
            if ag > 0.0 {
                bands.push(json!({"kind": "high_shelf", "freq": 7000.0, "gain_db": ag, "q": 0.707}));
            }
            e.call_nested("add_effect", &json!({"track": track, "type": "parametric_eq", "params": {"bands": bands}}), source)?;
            steps.push(format!(
                "dull recording (2-5 kHz {} dB, 5-8 kHz {} dB against the voice's core; a clear vocal is about {VOCAL_PRESENCE_AIM_DB} / {VOCAL_BRILLIANCE_AIM_DB}): presence +{pg} dB at 3.5 kHz, air +{ag} dB shelf at 7 kHz",
                t["presence_2k_5k"], t["brilliance_5k_8k"]
            ));
        }
    }
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
                // and its low end steps out of the voice's body
                e.call_nested("add_effect", &json!({"track": t, "type": "parametric_eq", "params": {"bands": [{"kind": "low_cut", "freq": BED_LOW_TRIM_HZ, "gain_db": 0.0, "q": 0.707, "stages": 1}]}}), source)?;
                steps.push(format!("{t} low end trimmed under {BED_LOW_TRIM_HZ} Hz"));
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
    // multiband glue under the broadband one: the sub band is held a little
    // tighter (the 808 and kick stop pumping the whole mix), the mids barely
    // touched, the top kept from spitting; gentle ratios, no makeup
    let has_mb = e.project.master_effects.iter().any(|f| serde_json::to_value(f).map(|v| v["type"] == "multiband").unwrap_or(false));
    let mb = json!({"type": "multiband", "low_freq": 150.0, "high_freq": 4500.0,
        "low_threshold_db": loud - 4.0, "mid_threshold_db": loud - 2.0, "high_threshold_db": loud - 8.0,
        "low_ratio": 1.8, "mid_ratio": 1.3, "high_ratio": 1.6, "attack_ms": 25.0, "release_ms": 160.0,
        "low_gain_db": 0.0, "mid_gain_db": 0.0, "high_gain_db": 0.0, "mix": 1.0});
    if !has_mb && loud > -100.0 {
        e.project.master_effects.push(serde_json::from_value(mb)?);
    }
    Ok(json!({"compressors": n, "threshold_db": r1(thr), "ratio": 1.5, "multiband": !has_mb && loud > -100.0}))
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
pub(crate) fn band_energy_db(x: &[f32], lo: f32, hi: f32) -> f32 {
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

/// One band of a delivered file minus its integrated loudness (dB - LUFS):
/// 4th-order high-pass at `lo`, 4th-order low-pass at `hi`, mean power over
/// both channels.
pub fn band_rel_lufs(l: &[f32], r: &[f32], lufs: f32, lo: f32, hi: f32) -> f32 {
    let p = |x: &[f32]| 10f64.powf(band_energy_db(x, lo, hi) as f64 / 10.0);
    (10.0 * (0.5 * (p(l) + p(r))).max(1e-12).log10()) as f32 - lufs
}

/// In an instrumental the hook carrier owns 2-5 kHz the way a vocal does:
/// where it plays, every other part sits at least this far under it there
/// (a 3 kHz bell per part, at most MAX_TOP_CUT_DB). Gentler than the vocal's
/// 6 dB: a lead needs to read, not to be understood.
pub const INSTRUMENTAL_UNDER_LEAD_PRESENCE_DB: f32 = 3.0;
/// The tracks that carry an instrumental's hook (the producer's names).
pub const INSTRUMENTAL_FOCAL_TRACKS: &[&str] = &["lead", "hook", "melody"];
/// Most any top-end rule cuts one part in one render (dB).
pub const MAX_TOP_CUT_DB: f32 = 7.0;
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
    let mix = e.render_stems(&pre)?;
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
    let mix = if voice.is_none() { e.render_stems_all(&pre)? } else { e.render_stems(&pre)? };
    let m = mono(&mix.left, &mix.right);
    let total = band_energy_db(&m, 20.0, 20000.0);
    let pres_share = band_energy_db(&m, 2000.0, 5000.0) - total;
    let air_share = band_energy_db(&m, 5000.0, 10000.0) - total;
    let mut parts: Vec<(String, f32, f32, f32)> = Vec::new(); // name, 2-5k dB, 5-10k dB, full dB
    let mut vocal_p = None;
    for st in mix.stems.iter() {
        let p = st.band_db(2000.0, 5000.0);
        let a = st.band_db(5000.0, 10000.0);
        let f = st.band_db(20.0, 20000.0);
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
    // 1b. an instrumental: where the hook carrier (the lead) plays, the
    // other parts sit under it at 2-5 kHz (per-part 3 kHz bells, so the
    // beat bus is not left to make one deep boxy cut; first pass only)
    let mut focal = Value::Null;
    if voice.is_none() && shift_db == 0.0 {
        focal = json!({"skipped": "no lead/hook/melody track"});
        if let Some(lead) = mix.stems.iter().find(|st| INSTRUMENTAL_FOCAL_TRACKS.iter().any(|f| st.name.eq_ignore_ascii_case(f))) {
            let lm = mono(&lead.left, &lead.right);
            let blk = 2048usize;
            let en: Vec<f32> = lm.chunks(blk).map(|c| c.iter().map(|v| v * v).sum::<f32>() / c.len().max(1) as f32).collect();
            let top = en.iter().cloned().fold(0.0f32, f32::max);
            let active: Vec<usize> = en.iter().enumerate().filter(|(_, x)| top > 0.0 && **x > top * 1e-3).map(|(i, _)| i).collect();
            // stems can differ in length (tails): a block past a stem's end
            // is silence for it
            let gather = |x: &[f32]| -> Vec<f32> {
                active.iter().flat_map(|&i| (i * blk..(i + 1) * blk).map(move |k| x.get(k).copied().unwrap_or(0.0))).collect()
            };
            focal = json!({"lead": lead.name, "plays_s": r1(active.len() as f32 * blk as f32 / SR), "skipped": "the lead plays under 4 s"});
            if active.len() * blk >= (SR * 4.0) as usize {
                let lp = band_energy_db(&gather(&lm), 2000.0, 5000.0);
                focal = json!({"lead": lead.name, "plays_s": r1(active.len() as f32 * blk as f32 / SR), "lead_2_5k_db": r1(lp), "others_under_db": INSTRUMENTAL_UNDER_LEAD_PRESENCE_DB});
                let ceiling = lp - INSTRUMENTAL_UNDER_LEAD_PRESENCE_DB;
                let lead_name = lead.name.clone();
                let mut cuts = Vec::new();
                let mut seen = Vec::new();
                for st in mix.stems.iter() {
                    if st.name == lead_name {
                        continue;
                    }
                    let x = mono(&st.left, &st.right);
                    let p = band_energy_db(&gather(&x), 2000.0, 5000.0);
                    let over = p - ceiling;
                    seen.push(json!([st.name, r1(p - lp)]));
                    if p > -90.0 && over > 0.5 {
                        cuts.push((st.name.clone(), over));
                    }
                }
                focal["others_vs_lead_2_5k_db"] = json!(seen);
                for (name, over) in cuts {
                    let cut = over.min(MAX_TOP_CUT_DB);
                    eq_cut(e, &name, "bell", 3000.0, cut)?;
                    done.push(json!({"part": name, "over_lead_presence_db": r1(over - INSTRUMENTAL_UNDER_LEAD_PRESENCE_DB), "cut_3k_db": r1(cut), "lead": lead_name}));
                }
            }
        }
    }
    drop(mix);
    // the shares are judged on the mix as it is after step 1
    let (pres_share, air_share, parts) = if done.is_empty() {
        (pres_share, air_share, parts)
    } else {
        let mut pre = e.project.clone();
        pre.master_effects.clear();
        let mix = e.render_stems(&pre)?;
        let m = mono(&mix.left, &mix.right);
        let total = band_energy_db(&m, 20.0, 20000.0);
        let parts: Vec<(String, f32, f32, f32)> = mix
            .stems
            .iter()
            .filter(|st| !voice.map(|v| v.eq_ignore_ascii_case(&st.name)).unwrap_or(false))
            .map(|st| (st.name.clone(), st.band_db(2000.0, 5000.0), st.band_db(5000.0, 10000.0), st.band_db(20.0, 20000.0)))
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
    Ok(json!({"mix_2_5k_share_db": r1(pres_share), "mix_5_10k_share_db": r1(air_share), "vocal_2_5k_db": vocal_p.map(r1), "instrumental_lead": focal, "changes": done}))
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

/// The bus the whole beat sums into (every part but the vocal and the send
/// returns); the beat's own tone lives on it, so the vocal keeps its top.
pub const BEAT_BUS: &str = "beat";
/// Deepest cut the beat tone stage makes at 3.15 kHz / with the 7.5 kHz shelf.
pub const BEAT_TONE_MAX_CUT_DB: f32 = 12.0;
/// Deepest cut the master tone stage may make (it EQs the vocal too).
pub const MASTER_TONE_MAX_CUT_DB: f32 = 2.0;

/// Route the beat into BEAT_BUS: every master-bound bus that is not a send
/// return and carries no vocal (the drum bus), and every master-bound part
/// that is not a vocal. Returns what was moved. Idempotent.
pub fn route_beat_bus(e: &mut Engine, voice: Option<&str>) -> Vec<String> {
    let p = &mut e.project;
    let is_voice = |n: &str| voice.map(|v| v.eq_ignore_ascii_case(n)).unwrap_or(false) || is_voice_name(n);
    if !p.tracks.iter().any(|t| !is_voice(&t.name)) {
        return Vec::new();
    }
    let returns: Vec<String> = p.tracks.iter().flat_map(|t| t.sends.iter().map(|s| s.bus.to_lowercase())).collect();
    let vocal_buses: Vec<String> = p.tracks.iter().filter(|t| is_voice(&t.name)).filter_map(|t| t.output.as_ref().map(|o| o.to_lowercase())).collect();
    if p.bus_index(BEAT_BUS).is_err() {
        p.buses.push(crate::project::Bus::new(BEAT_BUS));
    }
    let mut moved = Vec::new();
    for b in p.buses.iter_mut() {
        let n = b.name.to_lowercase();
        if n != BEAT_BUS && b.output.is_none() && !returns.contains(&n) && !vocal_buses.contains(&n) {
            b.output = Some(BEAT_BUS.to_string());
            moved.push(format!("bus {}", b.name));
        }
    }
    for t in p.tracks.iter_mut() {
        if t.output.is_none() && !is_voice(&t.name) {
            t.output = Some(BEAT_BUS.to_string());
            moved.push(t.name.clone());
        }
    }
    if !moved.is_empty() {
        p.ensure_fx_ids();
    }
    moved
}

/// The beat bus's 7.5 kHz shelf as it stands (dB; 0 when there is none).
pub fn beat_shelf_db(p: &Project) -> f32 {
    beat_tone_gains(p).map(|g| g.2).unwrap_or(0.0)
}

fn beat_tone_gains(p: &Project) -> Option<(usize, f32, f32)> {
    let b = &p.buses[p.bus_index(BEAT_BUS).ok()?];
    b.effects.iter().enumerate().find_map(|(i, f)| {
        let v = serde_json::to_value(f).ok()?;
        let bands = v["bands"].as_array()?;
        if v["type"] == "parametric_eq" && bands.len() == 2 && (bands[0]["freq"].as_f64()? as f32 - TONE_PRES_HZ).abs() < 1.0 {
            Some((i, bands[0]["gain_db"].as_f64()? as f32, bands[1]["gain_db"].as_f64()? as f32))
        } else {
            None
        }
    })
}

/// Up to `n` evenly spaced windows of `win` samples (the whole signal when
/// it is shorter): the tone search reads ~40 s instead of the whole song.
fn excerpt(x: &[f32], n: usize, win: usize) -> Vec<f32> {
    if x.len() <= n * win * 2 {
        return x.to_vec();
    }
    let step = (x.len() - win) / n;
    (0..n).flat_map(|k| x[k * step..k * step + win].iter().copied()).collect()
}

fn tone_filter(x: &[f32], pres_db: f32, air_db: f32) -> Vec<f32> {
    use crate::dsp::{Biquad, BiquadKind};
    let mut a = Biquad::new(BiquadKind::Bell, TONE_PRES_HZ, 0.8, pres_db);
    let mut b = Biquad::new(BiquadKind::HighShelf, TONE_AIR_HZ, 0.7, air_db);
    x.iter().map(|v| b.process(a.process(*v))).collect()
}

/// The beat's tone: one bell at 3.15 kHz and one shelf at 7.5 kHz on
/// BEAT_BUS, the least cut that brings the WHOLE mix's tilt (vocal
/// included, as it sits) to the targets with `margin_db` to spare. The vocal
/// is never EQ'd here: only the beat darkens. Solved on the stems (the beat
/// bus filtered in memory and re-summed with the rest), so it costs one
/// render, which the track cache makes a re-sum. Cuts only, at most
/// BEAT_TONE_MAX_CUT_DB each; what is left over is for `tone_stage`.
pub fn beat_tone(e: &mut Engine, voice: Option<&str>, t: &Targets, margin_db: f32) -> Result<Value> {
    beat_tone_with(e, voice, t, margin_db, margin_db)
}

/// The beat bus takes only what the source rules (SOURCE_TONE, the
/// hat/perc levels, the per-part 3 kHz rule) left; it aims to stay within
/// this (a deep bus bell reads boxy). Reported, never forced.
pub const BEAT_BUS_AIM_MAX_CUT_DB: f32 = 5.0;

/// `beat_tone` with its own margin per band (2-5 kHz, 5-10 kHz).
pub fn beat_tone_with(e: &mut Engine, voice: Option<&str>, t: &Targets, margin_p: f32, margin_a: f32) -> Result<Value> {
    let moved = route_beat_bus(e, voice);
    let mut pre = e.project.clone();
    pre.master_effects.clear();
    pre.master_volume_db = 0.0;
    pre.automation.retain(|l| !l.target.eq_ignore_ascii_case("master"));
    let mix = e.render_stems(&pre)?;
    let Some(bst) = mix.bus_stems.iter().find(|s| s.name.eq_ignore_ascii_case(BEAT_BUS)) else {
        return Ok(json!({"changed": false, "why": "no beat to shape"}));
    };
    let (n, win) = (28usize, (1.5 * SR) as usize);
    let m = excerpt(&mono(&mix.left, &mix.right), n, win);
    let b = excerpt(&mono(&bst.left, &bst.right), n, win);
    let rest: Vec<f32> = m.iter().zip(b.iter()).map(|(x, y)| x - y).collect();
    let vocal_top = voice.and_then(|v| mix.stems.iter().find(|s| s.name.eq_ignore_ascii_case(v))).map(|s| {
        let x = excerpt(&mono(&s.left, &s.right), n, win);
        tilt_db(&x)
    });
    let (idx, gp, ga) = beat_tone_gains(&e.project).map(|(i, a, b)| (Some(i), a, b)).unwrap_or((None, 0.0, 0.0));
    let tilt_with = |np: f32, na: f32| -> (f32, f32) {
        let fb = tone_filter(&b, np - gp, na - ga);
        let s: Vec<f32> = rest.iter().zip(fb.iter()).map(|(x, y)| x + y).collect();
        tilt_db(&s)
    };
    let before = tilt_db(&m);
    let (tp, ta) = (t.presence_tilt_max_db - margin_p, t.air_tilt_max_db - margin_a);
    // the least cut per band that meets its target (bisection; the other
    // band held), twice round since the shelf also moves 2-5 kHz a little
    let (mut np, mut na) = (0.0f32, 0.0f32);
    for _ in 0..2 {
        for band in 0..2 {
            let ok = |g: f32| {
                let r = if band == 0 { tilt_with(g, na) } else { tilt_with(np, g) };
                if band == 0 { r.0 <= tp } else { r.1 <= ta }
            };
            let g = if ok(0.0) {
                0.0
            } else if !ok(-BEAT_TONE_MAX_CUT_DB) {
                -BEAT_TONE_MAX_CUT_DB
            } else {
                let (mut lo, mut hi) = (-BEAT_TONE_MAX_CUT_DB, 0.0f32);
                for _ in 0..9 {
                    let mid = 0.5 * (lo + hi);
                    if ok(mid) { lo = mid } else { hi = mid }
                }
                lo
            };
            if band == 0 { np = g } else { na = g }
        }
    }
    let after = tilt_with(np, na);
    let changed = (np - gp).abs() >= 0.1 || (na - ga).abs() >= 0.1 || !moved.is_empty();
    if (np - gp).abs() >= 0.1 || (na - ga).abs() >= 0.1 {
        let eq: crate::fx::Effect = serde_json::from_value(json!({"type": "parametric_eq", "bands": [
            {"kind": "bell", "freq": TONE_PRES_HZ, "gain_db": np, "q": 0.8},
            {"kind": "high_shelf", "freq": TONE_AIR_HZ, "gain_db": na, "q": 0.7}
        ]}))?;
        let bi = e.project.bus_index(BEAT_BUS)?;
        match idx {
            Some(i) => e.project.buses[bi].effects[i] = eq,
            None => e.project.buses[bi].effects.push(eq),
        }
        e.project.ensure_fx_ids();
    }
    Ok(json!({
        "bus": BEAT_BUS,
        "routed": moved,
        "mix_tilt_before": {"2_5k_db": r1(before.0), "5_10k_db": r1(before.1)},
        "mix_tilt_after": {"2_5k_db": r1(after.0), "5_10k_db": r1(after.1)},
        "targets": {"2_5k_db": r1(tp), "5_10k_db": r1(ta)},
        "beat_bell_3k_db": r1(np), "beat_shelf_7k5_db": r1(na),
        "within_aim": np >= -BEAT_BUS_AIM_MAX_CUT_DB - 0.05 && na >= -BEAT_BUS_AIM_MAX_CUT_DB - 0.05,
        "at_limit": np <= -BEAT_TONE_MAX_CUT_DB + 0.05 || na <= -BEAT_TONE_MAX_CUT_DB + 0.05,
        "vocal_own_tilt": vocal_top.map(|v| json!({"2_5k_db": r1(v.0), "5_10k_db": r1(v.1)})),
        "changed": changed,
    }))
}

/// The whole top-end rule, in order: the beat darkens on its own bus
/// (`beat_tone`), the vocal is re-balanced over the darker beat, and the full
/// mix (master chain included) is measured; what the stem model missed is
/// fed back per band (up to 3 rounds). The master tone (`tone_stage`, capped
/// at MASTER_TONE_MAX_CUT_DB) only engages when the beat tone is at its
/// limit; otherwise it is taken off, so the vocal is never EQ'd for the beat.
pub fn shape_top(e: &mut Engine, voice: Option<&str>, t: &Targets, margin_db: f32) -> Result<Value> {
    shape_top_with(e, voice, t, margin_db, margin_db)
}

/// `shape_top` with its own margin per band (2-5 kHz, 5-10 kHz): the
/// delivery loop aims each band at its own window.
pub fn shape_top_with(e: &mut Engine, voice: Option<&str>, t: &Targets, margin_p: f32, margin_a: f32) -> Result<Value> {
    let (mut xp, mut xa) = (0.0f32, 0.0f32);
    let mut rounds = Vec::new();
    let mut at_limit = false;
    for _ in 0..3 {
        let r = beat_tone_with(e, voice, t, margin_p + xp, margin_a + xa)?;
        at_limit = r["at_limit"] == true;
        if let Some(v) = voice {
            balance_vocal(e, v, t, 1)?;
        }
        let full = e.mix()?;
        let (pt, at) = tilt_db(&mono(&full.left, &full.right));
        let (op, oa) = (pt - (t.presence_tilt_max_db - margin_p), at - (t.air_tilt_max_db - margin_a));
        rounds.push(json!({"beat_bell_3k_db": r["beat_bell_3k_db"], "beat_shelf_7k5_db": r["beat_shelf_7k5_db"], "mix_tilt_2_5k_db": r1(pt), "mix_tilt_5_10k_db": r1(at), "routed": r["routed"]}));
        // the model predicts the render within a few tenths both ways: a
        // round that lands darker than aimed is pulled back too, so the
        // beat is never darker than the target asks
        if (op.abs() <= 0.25 && oa <= 0.25) || at_limit {
            break;
        }
        xp += 0.8 * op;
        xa += (0.8 * oa).max(-xa);
    }
    let master = if at_limit {
        tone_stage(e, t, margin_p.min(margin_a))?
    } else {
        let had = tone_gains(&e.project);
        if let Some((i, _, _)) = had {
            e.project.master_effects.remove(i);
        }
        json!({"changed": had.is_some(), "why": "the beat tone met the targets; the master (and the vocal) are not EQ'd"})
    };
    Ok(json!({"rounds": rounds, "beat_at_limit": at_limit, "master_tone": master}))
}

/// Master tone, the last resort after `beat_tone`: one bell at 3.15 kHz and
/// one shelf at 7.5 kHz on the master, set so the mix's tilt meets the
/// targets with `margin_db` to spare. It touches the vocal too, so it may cut
/// at most MASTER_TONE_MAX_CUT_DB each; the beat's own tone does the rest.
pub fn tone_stage(e: &mut Engine, t: &Targets, margin_db: f32) -> Result<Value> {
    let full = e.mix()?;
    let m = mono(&full.left, &full.right);
    let (pt, at) = tilt_db(&m);
    let (idx, gp, ga) = tone_gains(&e.project).map(|(i, a, b)| (Some(i), a, b)).unwrap_or((None, 0.0, 0.0));
    let cap = -MASTER_TONE_MAX_CUT_DB;
    let np = (gp - (pt - (t.presence_tilt_max_db - margin_db)).max(0.0)).clamp(cap, 0.0);
    let na = (ga - (at - (t.air_tilt_max_db - margin_db)).max(0.0)).clamp(cap, 0.0);
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

/// Quiet sections (intro, outro, breakdown: not a verse, not a hook/drop)
/// sit at most `quiet_section_floor_db` under the verses (mix RMS,
/// pre-delivery); further down they step up on the master's section
/// automation (the vocal and the beat together, so the vocal over the beat
/// is unchanged), at most 6 dB.
pub fn lift_quiet_sections(e: &mut Engine, t: &Targets) -> Result<Value> {
    let m = measure_sections(e, None)?;
    let verses: Vec<f32> = m.iter().filter(|x| x.kind == "verse").map(|x| x.mix_rms_db).collect();
    if verses.is_empty() {
        return Ok(json!({"changed": false, "why": "no verse to measure against"}));
    }
    let vmean = verses.iter().sum::<f32>() / verses.len() as f32;
    let spans = section_spans(&e.project);
    let mut done = Vec::new();
    for s in m.iter().filter(|x| x.kind != "verse" && !is_lift_kind(&x.kind)) {
        let gap = vmean - s.mix_rms_db;
        if gap <= t.quiet_section_floor_db + 0.3 || s.mix_rms_db < -80.0 {
            continue;
        }
        let up = (gap - t.quiet_section_floor_db).min(6.0);
        let cur = crate::tools_studio::static_value(&e.project, "master", "volume").unwrap_or(0.0);
        let at = section_master_db(&e.project, &spans, &s.name).unwrap_or(cur);
        e.call_nested("set_section_mix", &json!({"section": s.name, "track": "master", "volume_db": at + up, "all_occurrences": false}), "mix_rules")?;
        done.push(json!({"section": s.name, "under_verses_db": r1(gap), "up_db": r1(up)}));
    }
    Ok(json!({"changed": !done.is_empty(), "floor_db": t.quiet_section_floor_db, "sections": done}))
}

/// The top-end aim on the pre-delivery mix sits this much under the
/// delivered-file targets: limiting and the encode add about 2.5 dB of
/// 2-5 kHz tilt (measured, B19: soft clip ruled out); the delivery loop
/// corrects what is left after checking the file.
pub const TOP_DELIVERY_MARGIN_DB: f32 = 1.9;

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
    let pob: Vec<(String, f32)> = sections_pre.iter().filter_map(|x| x.presence_over_beat_db.map(|v| (x.name.clone(), v))).collect();
    if !pob.is_empty() {
        let lo = pob.iter().map(|x| x.1).fold(f32::MAX, f32::min);
        checks.push(chk(
            "vocal_presence",
            format!(">= {} dB of the vocal's 1-4 kHz over the beat's in every section with vocal (the words)", t.vocal_presence_over_beat_db),
            json!({"min_db": r1(lo), "by_section": pob.iter().map(|(n, v)| json!([n, r1(*v)])).collect::<Vec<_>>()}),
            lo >= t.vocal_presence_over_beat_db - 0.25,
            "the beat masks the words: song_patch /mix/vocal_over_beat_db higher, or a darker/sparser part in /instruments",
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
    // measured the way a person checks a master: each band through a
    // 4th-order high-pass and low-pass, mean power over both channels,
    // minus the integrated loudness (ffmpeg highpass x2 + lowpass x2 +
    // volumedetect gives the same numbers)
    let rel = |lo: f32, hi: f32| band_rel_lufs(&l, &r, ld.integrated_lufs, lo, hi);
    let (body, pres, air) = (rel(250.0, 2000.0), rel(2000.0, 5000.0), rel(5000.0, 10000.0));
    let [p_lo, p_hi] = t.presence_rel_lufs_db;
    let [a_lo, a_hi] = t.air_rel_lufs_db;
    checks.push(chk(
        "not_harsh",
        format!("2-5 kHz between {p_lo} and {p_hi} dB and 5-10 kHz between {a_lo} and {a_hi} dB, each band minus integrated LUFS (bright enough to read, never harsher than the reference master, never dull)"),
        json!({"presence_rel_lufs_db": r1(pres), "air_rel_lufs_db": r1(air), "body_rel_lufs_db": r1(body), "tilt_2_5k_db": r1(pt), "tilt_5_10k_db": r1(at)}),
        pres <= p_hi + 0.15 && pres >= p_lo - 0.15 && air <= a_hi + 0.15 && air >= a_lo - 0.15,
        "every render shapes the top on its own: the bright beat parts darken first, then the beat bus; a darker preset in /instruments helps more",
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
    fn beat_tone_darkens_the_beat_bus_and_leaves_the_vocal_and_master_alone() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_beat_tone_tests"));
        e.call("generate_beat", &serde_json::json!({"style": "boom_bap", "bars": 2, "seed": 7})).unwrap();
        // a stand-in "vocal": a copy of a melodic part under a voice name
        let src = e.project.tracks.last().unwrap().clone();
        let mut v = src.clone();
        v.name = "vocal".into();
        v.id = String::new();
        v.output = None;
        for pt in e.project.patterns.iter_mut() {
            if let Some(n) = pt.clips.get(&src.name).cloned() {
                pt.clips.insert("vocal".into(), n);
            }
        }
        e.project.tracks.push(v);
        e.project.ensure_fx_ids();
        let vocal_fx = serde_json::to_string(&e.project.tracks.last().unwrap().effects).unwrap();
        let master_fx = serde_json::to_string(&e.project.master_effects).unwrap();
        e.track_cache = Some(Default::default());
        let m0 = e.mix().unwrap();
        let (p0, a0) = tilt_db(&mono(&m0.left, &m0.right));
        let mut t = Targets::default();
        t.presence_tilt_max_db = p0 - 2.0;
        t.air_tilt_max_db = a0 - 2.0;
        let r = beat_tone(&mut e, Some("vocal"), &t, 0.0).unwrap();
        assert_eq!(r["changed"], true, "{r}");
        // the beat is routed into the beat bus, the vocal is not
        let vi = e.project.track_index("vocal").unwrap();
        assert_eq!(e.project.tracks[vi].output, None);
        assert!(e.project.tracks.iter().filter(|t| t.name != "vocal").all(|t| t.output.is_some()));
        assert!(beat_tone_gains(&e.project).map(|g| g.1 < -0.5).unwrap_or(false), "{r}");
        assert_eq!(serde_json::to_string(&e.project.tracks[vi].effects).unwrap(), vocal_fx);
        assert_eq!(serde_json::to_string(&e.project.master_effects).unwrap(), master_fx);
        let m1 = e.mix().unwrap();
        let (p1, _) = tilt_db(&mono(&m1.left, &m1.right));
        assert!(p1 < p0 - 1.0 || r["at_limit"] == true, "beat tone did not darken the mix: {p0} -> {p1} ({r})");
        // the solve on stems predicts the render
        let predicted = r["mix_tilt_after"]["2_5k_db"].as_f64().unwrap() as f32;
        assert!((predicted - p1).abs() < 1.0, "predicted {predicted}, rendered {p1}");
        // a second pass at the same targets changes nothing
        let r2 = beat_tone(&mut e, Some("vocal"), &t, 0.0).unwrap();
        assert!(r2["routed"].as_array().map(|a| a.is_empty()).unwrap_or(false));
        // the master tone never cuts more than its cap
        t.presence_tilt_max_db = p0 - 20.0;
        t.air_tilt_max_db = a0 - 20.0;
        tone_stage(&mut e, &t, 0.0).unwrap();
        let (_, gp, ga) = tone_gains(&e.project).unwrap();
        assert!(gp >= -MASTER_TONE_MAX_CUT_DB - 1e-3 && ga >= -MASTER_TONE_MAX_CUT_DB - 1e-3, "{gp} {ga}");
    }

    #[test]
    fn beat_source_level_rule_keeps_the_songs_hat_offset() {
        // one owner for the hat level: an offset in /mix/levels moves where
        // the under-snare rule puts the hat instead of being pulled back
        let mut e = Engine::new(std::env::temp_dir().join("bb_mixrules_hat_offset"));
        e.call("generate_beat", &json!({"style": "boom_bap", "bars": 2, "seed": 3})).unwrap();
        let hi = (0..e.project.tracks.len()).find(|&i| source_role(&e.project.tracks[i]) == Some("hat")).expect("a hat");
        e.project.tracks[hi].volume_db += 12.0; // far over the snare: the rule must take
        let mut a = Engine::new(std::env::temp_dir().join("bb_mixrules_hat_offset_a"));
        a.project = e.project.clone();
        let ra = soften_beat_source(&mut a, Some("vocal")).unwrap();
        let mut off = std::collections::BTreeMap::new();
        off.insert("hat".to_string(), 3.0f32);
        let r = soften_beat_source_with(&mut e, Some("vocal"), &off).unwrap();
        let d = e.project.tracks[hi].volume_db - a.project.tracks[hi].volume_db;
        assert!((d - 3.0).abs() < 0.2, "hat offset kept: {d} {r} {ra}");
    }

    #[test]
    fn beat_source_tone_cap_rises_with_a_positive_hat_offset() {
        // a dull top end's fix turns the hat up; the tone cap rises with it
        // (0.05 per dB, at most +0.3) so the top brightens past 5 kHz
        let mut e = Engine::new(std::env::temp_dir().join("bb_mixrules_hat_tonecap"));
        e.call("add_track", &json!({"name": "hat", "preset": "hat_crisp"})).unwrap();
        e.call("add_track", &json!({"name": "snare", "preset": "snare_tight"})).unwrap();
        let hi = e.project.track_index("hat").unwrap();
        if let crate::instruments::Instrument::Drum(d) = &mut e.project.tracks[hi].instrument { d.tone = 1.0; }
        let tone = |e: &Engine| match &e.project.tracks[hi].instrument {
            crate::instruments::Instrument::Drum(d) => d.tone,
            other => panic!("hat is not a drum: {other:?}"),
        };
        let mut plain = Engine::new(std::env::temp_dir().join("bb_mixrules_hat_tonecap_plain"));
        plain.project = e.project.clone();
        soften_beat_source(&mut plain, Some("vocal")).unwrap();
        assert!((tone(&plain) - -0.3).abs() < 1e-3, "no offset: base cap {}", tone(&plain));
        let mut small = Engine::new(std::env::temp_dir().join("bb_mixrules_hat_tonecap_small"));
        small.project = e.project.clone();
        let mut off = std::collections::BTreeMap::new();
        off.insert("hat".to_string(), 2.0f32);
        soften_beat_source_with(&mut small, Some("vocal"), &off).unwrap();
        assert!((tone(&small) - -0.2).abs() < 1e-3, "+2 dB: cap -0.2, got {}", tone(&small));
        off.insert("hat".to_string(), 12.0f32);
        soften_beat_source_with(&mut e, Some("vocal"), &off).unwrap();
        assert!((tone(&e) - 0.0).abs() < 1e-3, "+12 dB: cap clamps at base+0.3, got {}", tone(&e));
        // a cut never darkens the cap below the base
        let mut cut = Engine::new(std::env::temp_dir().join("bb_mixrules_hat_tonecap_cut"));
        cut.project = plain.project.clone();
        if let crate::instruments::Instrument::Drum(d) = &mut cut.project.tracks[hi].instrument { d.tone = 1.0; }
        off.insert("hat".to_string(), -4.0f32);
        soften_beat_source_with(&mut cut, Some("vocal"), &off).unwrap();
        assert!((tone(&cut) - -0.3).abs() < 1e-3, "negative offset keeps base cap, got {}", tone(&cut));
    }

    #[test]
    fn beat_source_softens_bright_parts_once_with_or_without_a_vocal() {
        let mut e = Engine::new(std::env::temp_dir().join("bb_mixrules_source_tone"));
        e.call("add_track", &json!({"name": "hat", "preset": "hat_crisp"})).unwrap();
        e.call("add_track", &json!({"name": "snare", "preset": "snare_tight"})).unwrap();
        e.call("add_track", &json!({"name": "kick", "preset": "kick"})).unwrap();
        let mut inst = Engine::new(std::env::temp_dir().join("bb_mixrules_source_tone_inst"));
        inst.project = e.project.clone();
        let r = soften_beat_source(&mut inst, None).unwrap();
        assert_eq!(r["changed"], true, "an instrumental is voiced too: {r}");
        assert_eq!(r["mode"], "instrumental");
        let r = soften_beat_source(&mut e, Some("vocal")).unwrap();
        assert_eq!(r["changed"], true, "{r}");
        let hi = e.project.track_index("hat").unwrap();
        match &e.project.tracks[hi].instrument {
            crate::instruments::Instrument::Drum(d) => assert!(d.tone <= -0.3 + 1e-3, "{}", d.tone),
            other => panic!("hat is not a drum: {other:?}"),
        }
        let si = e.project.track_index("snare").unwrap();
        let shelves = |e: &Engine| e.project.tracks[si].effects.iter().filter(|f| f.id() == SOURCE_TONE_ID).count();
        assert_eq!(shelves(&e), 1, "the snare gets its source shelf");
        let ki = e.project.track_index("kick").unwrap();
        assert!(e.project.tracks[ki].effects.iter().all(|f| f.id() != SOURCE_TONE_ID), "the kick keeps its sound");
        // a second pass (a re-render, a patch) changes nothing and stacks nothing
        let r2 = soften_beat_source(&mut e, Some("vocal")).unwrap();
        assert_eq!(r2["changed"], false, "{r2}");
        assert_eq!(shelves(&e), 1);
    }

    #[test]
    fn band_minus_lufs_reads_a_tone_where_it_is() {
        let n = (SR * 4.0) as usize;
        let tone = |f: f32| -> Vec<f32> { (0..n).map(|i| 0.25 * (std::f32::consts::TAU * f * i as f32 / SR).sin()).collect() };
        let x = tone(3000.0);
        let ld = crate::analysis::loudness_at(&x, &x, SR);
        let pres = band_rel_lufs(&x, &x, ld.integrated_lufs, 2000.0, 5000.0);
        let air = band_rel_lufs(&x, &x, ld.integrated_lufs, 5000.0, 10000.0);
        assert!(pres > air + 10.0, "a 3 kHz tone reads in 2-5k: {pres} vs {air}");
        // the whole tone is in the band: it sits near the loudness (K-weighting
        // and the two-channel sum move it by a few dB, never by 10)
        assert!((-12.0..3.0).contains(&pres), "{pres}");
    }

    #[test]
    fn quiet_intro_steps_up_to_the_floor() {
        let mut e = Engine::new(std::env::temp_dir().join("bb_mixrules_quiet_intro"));
        e.call("generate_beat", &json!({"style": "boom_bap", "bars": 2, "seed": 3})).unwrap();
        let spans = section_spans(&e.project);
        if spans.iter().all(|s| s.kind != "verse") || spans.iter().all(|s| s.kind != "intro") {
            // the generator wrote no intro/verse here: the rule has nothing to do
            assert_eq!(lift_quiet_sections(&mut e, &Targets::default()).unwrap()["changed"], false);
            return;
        }
        let intro = spans.iter().find(|s| s.kind == "intro").unwrap().name.clone();
        e.call_nested("set_section_mix", &json!({"section": intro, "track": "master", "volume_db": -20.0, "all_occurrences": false}), "test").unwrap();
        let t = Targets::default();
        let r = lift_quiet_sections(&mut e, &t).unwrap();
        assert_eq!(r["changed"], true, "{r}");
        let m = measure_sections(&mut e, None).unwrap();
        let v: Vec<f32> = m.iter().filter(|x| x.kind == "verse").map(|x| x.mix_rms_db).collect();
        let vm = v.iter().sum::<f32>() / v.len() as f32;
        let i = m.iter().find(|x| x.name == intro).unwrap().mix_rms_db;
        assert!(vm - i < t.quiet_section_floor_db + 6.5, "intro still {} dB under", vm - i);
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
        assert_eq!(t.vocal_over_beat_db, 11.0);
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
