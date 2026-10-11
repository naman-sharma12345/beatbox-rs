//! The song model: tracks, patterns of notes, an arrangement, a master bus.
//! Everything is plain serde data so a project is one JSON file.

use crate::automation::AutomationLane;
use crate::fx::{CompressorFx, Effect, LimiterFx};
use crate::instruments::Instrument;
use crate::samples::SampleInfo;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const STEPS_PER_BAR: u32 = 16;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Note {
    /// Start position in 16th-note steps from the pattern start (fractional allowed).
    pub start: f32,
    /// Length in steps.
    pub len: f32,
    /// MIDI pitch (C4 = 60). Drums use 60 as their natural pitch.
    pub pitch: u8,
    /// Velocity 0..1.
    pub vel: f32,
    /// Chance this note plays on each pass, 0..1 (1 = always). Lets a loop
    /// breathe: ghost hats at 0.6, a fill hit at 0.3.
    #[serde(default = "one_f32", skip_serializing_if = "is_one")]
    pub prob: f32,
    /// Microtiming nudge in steps (-0.5..0.5) applied after swing:
    /// negative = push (early), positive = lay back (late).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: f32,
    /// Glide into this pitch by the end of the note (808 / synth slides).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slide_to: Option<u8>,
}

impl Default for Note {
    fn default() -> Self {
        Note {
            start: 0.0,
            len: 1.0,
            pitch: 60,
            vel: 0.8,
            prob: 1.0,
            offset: 0.0,
            slide_to: None,
        }
    }
}

impl Note {
    pub fn new(start: f32, len: f32, pitch: u8, vel: f32) -> Self {
        Note {
            start,
            len,
            pitch,
            vel,
            ..Default::default()
        }
    }
    pub fn end(&self) -> f32 {
        self.start + self.len
    }
}

fn one_f32() -> f32 {
    1.0
}
fn is_one(x: &f32) -> bool {
    (*x - 1.0).abs() < 1e-9
}
fn is_zero(x: &f32) -> bool {
    *x == 0.0
}

fn zero() -> f32 {
    0.0
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub name: String,
    pub instrument: Instrument,
    #[serde(default = "zero")]
    pub volume_db: f32,
    /// -1 (left) .. 1 (right)
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    #[serde(default)]
    pub effects: Vec<Effect>,
    /// Bus this track's post-fader signal goes to (None = master).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Aux sends to buses (e.g. a shared reverb return).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sends: Vec<Send>,
}

impl Track {
    pub fn new(name: &str, instrument: Instrument) -> Self {
        Track {
            name: name.to_string(),
            instrument,
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            effects: Vec::new(),
            output: None,
            sends: Vec::new(),
        }
    }
}

/// An aux send from a track to a bus.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Send {
    pub bus: String,
    /// Send level in dB (0 = unity).
    pub db: f32,
    /// Tap before the track fader (true) or after it (false, default).
    #[serde(default)]
    pub pre_fader: bool,
}

/// A mix bus: group (drum bus) or return (reverb/delay). Buses sum into the
/// master after their own effect chain, fader and pan.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Bus {
    pub name: String,
    #[serde(default)]
    pub effects: Vec<Effect>,
    #[serde(default = "zero")]
    pub volume_db: f32,
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub mute: bool,
    /// Where this bus goes: another bus (mixer insert routing, e.g. a
    /// "drums" insert into a "beat" group) or the master (None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

impl Bus {
    pub fn new(name: &str) -> Self {
        Bus {
            name: name.to_string(),
            effects: Vec::new(),
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            output: None,
        }
    }
}

/// A pattern placed on the playlist (FL-style pattern clip): `pattern`
/// plays from `start_bar` for `bars` bars (looping the pattern when the
/// clip is longer), optionally only some of its tracks.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PlaylistClip {
    pub pattern: String,
    pub start_bar: u32,
    pub bars: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<String>,
    /// Playlist lane, for display only (clips on any lane all play).
    #[serde(default)]
    pub lane: u32,
}

/// A reusable automation clip: a shape for one parameter, in beats from
/// the clip start, that can be placed anywhere in the song.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AutomationClip {
    pub name: String,
    pub target: String,
    pub param: String,
    pub length_beats: f32,
    pub points: Vec<crate::automation::AutoPoint>,
    /// Song beats where the clip is placed.
    #[serde(default)]
    pub placements: Vec<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Pattern {
    pub name: String,
    pub bars: u32,
    /// Notes per track, keyed by lowercase track name.
    #[serde(default)]
    pub clips: BTreeMap<String, Vec<Note>>,
    /// Time signature [numerator, denominator] (default 4/4). A bar holds
    /// numerator * 16 / denominator steps: 3/4 and 6/8 = 12, 7/8 = 14, 5/4 = 20.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meter: Option<(u32, u32)>,
}

/// Steps (16ths) in one bar of a meter.
pub fn meter_steps(meter: (u32, u32)) -> u32 {
    let (n, d) = (meter.0.clamp(1, 32), meter.1.clamp(1, 16));
    (n * 16 / d).max(1)
}

/// Parse "7/8" into (7, 8): numerator 1-32, denominator 2, 4, 8 or 16.
pub fn parse_meter(s: &str) -> Option<(u32, u32)> {
    let (n, d) = s.trim().split_once('/')?;
    let n: u32 = n.trim().parse().ok()?;
    let d: u32 = d.trim().parse().ok()?;
    ((1..=32).contains(&n) && matches!(d, 2 | 4 | 8 | 16)).then_some((n, d))
}

/// Click positions inside one bar, in steps from the bar start, and whether
/// each is the downbeat: quarter-note meters click every beat, compound
/// eighth meters (6/8, 9/8, 12/8) on each dotted quarter, other x/8 and x/16
/// meters on every eighth / sixteenth.
pub fn meter_clicks(meter: (u32, u32)) -> Vec<(u32, bool)> {
    let (n, d) = (meter.0.clamp(1, 32), meter.1.clamp(1, 16));
    let unit = (16 / d).max(1);
    let step = if d == 8 && n % 3 == 0 && n > 3 { unit * 3 } else { unit };
    let bar = meter_steps(meter);
    (0..bar).step_by(step as usize).map(|s| (s, s == 0)).collect()
}

impl Pattern {
    pub fn new(name: &str, bars: u32) -> Self {
        Pattern {
            name: name.to_string(),
            bars: bars.clamp(1, 64),
            clips: BTreeMap::new(),
            meter: None,
        }
    }
    /// Steps in one bar of this pattern (16 in 4/4).
    pub fn steps_per_bar(&self) -> u32 {
        self.meter.map(meter_steps).unwrap_or(STEPS_PER_BAR)
    }
    pub fn meter(&self) -> (u32, u32) {
        self.meter.unwrap_or((4, 4))
    }
    pub fn steps(&self) -> u32 {
        self.bars * self.steps_per_bar()
    }
    pub fn notes_mut(&mut self, track: &str) -> &mut Vec<Note> {
        self.clips.entry(track.to_lowercase()).or_default()
    }
    pub fn notes(&self, track: &str) -> &[Note] {
        self.clips
            .get(&track.to_lowercase())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Section {
    pub pattern: String,
    #[serde(default = "one_u32")]
    pub repeats: u32,
}

fn one_u32() -> u32 {
    1
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub name: String,
    pub bpm: f32,
    /// 0 = straight, 1 = heavy shuffle on off-beat 16ths.
    #[serde(default)]
    pub swing: f32,
    pub key_root: String,
    pub scale: String,
    pub tracks: Vec<Track>,
    pub patterns: Vec<Pattern>,
    /// Song order. Empty = loop the first pattern once.
    #[serde(default)]
    pub arrangement: Vec<Section>,
    #[serde(default)]
    pub master_effects: Vec<Effect>,
    #[serde(default)]
    pub master_volume_db: f32,
    #[serde(default)]
    pub samples: Vec<SampleInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buses: Vec<Bus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub automation: Vec<AutomationLane>,
    /// Macro knobs: one 0..1 value driving several parameters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub macros: Vec<Macro>,
    /// FL-style playlist of pattern clips. When non-empty it is compiled
    /// into the arrangement (generated "pl:" patterns) by the playlist tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub playlist: Vec<PlaylistClip>,
    /// Reusable automation clips (compiled into automation lanes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub automation_clips: Vec<AutomationClip>,
    /// Audio clips on the song timeline (a recorded vocal, a long sample),
    /// each played on a track's channel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_clips: Vec<AudioClip>,
    /// What vocal_to_song heard, on the song grid (lyrics, melody, chords,
    /// sections) for the studio's vocal view and for AIs editing around it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vocal_map: Option<VocalMap>,
    /// Saved effect chains (FL Patcher-style: a whole chain as one preset),
    /// by name; save_fx_chain / apply_fx_chain.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub fx_chains: std::collections::BTreeMap<String, Vec<Effect>>,
    /// Playlist markers (FL): named song positions, optionally carrying a
    /// time-signature label; add_marker / list_markers / remove_marker.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
    /// Tempo automation (FL tempo clip): bpm points on the song timeline;
    /// `bpm` above is the tempo at beat 0. add_tempo_point / tempo_ramp.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tempo_points: Vec<TempoPoint>,
}

/// One tempo change: from `beat` on the song plays at `bpm`. With `ramp`
/// the tempo glides linearly from the previous point to this one.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TempoPoint {
    pub beat: f32,
    pub bpm: f32,
    #[serde(default)]
    pub ramp: bool,
}

/// A named position on the song timeline.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Marker {
    pub name: String,
    /// Song beat (quarter notes from the start).
    pub beat: f32,
    /// Time-signature label from here on ("3/4"); metadata for now (the
    /// renderer counts 4/4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_signature: Option<String>,
}

/// A sung vocal mapped onto the song: every time is in song beats.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct VocalMap {
    pub track: String,
    pub sample: String,
    /// (start beat, end beat, word)
    #[serde(default)]
    pub words: Vec<(f32, f32, String)>,
    /// (start beat, length in beats, MIDI pitch)
    #[serde(default)]
    pub notes: Vec<(f32, f32, u8)>,
    /// (start beat, length in beats, roman numeral)
    #[serde(default)]
    pub chords: Vec<(f32, f32, String)>,
    /// (section name, kind, first bar (0-based), bars)
    #[serde(default)]
    pub sections: Vec<(String, String, u32, u32)>,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub warped: bool,
    #[serde(default)]
    pub phrases_pinned: usize,
}

/// A sample placed on the song timeline at a beat, played through a track's
/// channel (its FX, fader, sends and routing), independent of patterns.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AudioClip {
    pub track: String,
    pub sample: String,
    /// Song beat where the clip's audio (after `offset_s`) starts; may be
    /// negative to start a take mid-way.
    pub start_beat: f32,
    /// Seconds skipped at the start of the sample.
    #[serde(default)]
    pub offset_s: f32,
    /// Seconds played (None = to the end of the sample).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_s: Option<f32>,
    #[serde(default)]
    pub gain_db: f32,
    /// Equal-power fade at the clip's start / end in ms (None = a 4 ms ramp).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_in_ms: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_out_ms: Option<f32>,
}

/// One parameter a macro drives, mapped from the macro's 0..1 value.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MacroTarget {
    /// Track, bus or "master".
    pub track: String,
    /// volume, pan, instrument.<path> or fx.<i>.<path>
    pub param: String,
    pub min: f32,
    pub max: f32,
    /// 1 = linear, >1 = slow start (exponential feel), <1 = fast start.
    #[serde(default = "one_f32")]
    pub curve: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Macro {
    pub name: String,
    #[serde(default)]
    pub value: f32,
    pub targets: Vec<MacroTarget>,
}

impl Default for Project {
    fn default() -> Self {
        Project::new("untitled", 120.0)
    }
}

impl Project {
    pub fn new(name: &str, bpm: f32) -> Self {
        Project {
            name: name.to_string(),
            bpm,
            swing: 0.0,
            key_root: "C".into(),
            scale: "minor".into(),
            tracks: Vec::new(),
            patterns: vec![Pattern::new("A", 4)],
            arrangement: Vec::new(),
            master_effects: vec![
                Effect::Compressor(CompressorFx {
                    threshold_db: -14.0,
                    ratio: 2.0,
                    attack_ms: 25.0,
                    release_ms: 150.0,
                    makeup_db: 2.0,
                    ..Default::default()
                }),
                Effect::Limiter(LimiterFx {
                    ceiling_db: -1.0,
                    release_ms: 80.0,
                    ..Default::default()
                }),
            ],
            master_volume_db: 0.0,
            samples: Vec::new(),
            buses: Vec::new(),
            automation: Vec::new(),
            macros: Vec::new(),
            playlist: Vec::new(),
            automation_clips: Vec::new(),
            audio_clips: Vec::new(),
            vocal_map: None,
            fx_chains: Default::default(),
            markers: Vec::new(),
            tempo_points: Vec::new(),
        }
    }

    pub fn step_secs(&self) -> f32 {
        60.0 / self.bpm.clamp(20.0, 400.0) / 4.0
    }

    /// Find a track by (case-insensitive) name or numeric index.
    pub fn track_index(&self, key: &str) -> Result<usize> {
        let k = key.trim().to_lowercase();
        if let Some(i) = self.tracks.iter().position(|t| t.name.to_lowercase() == k) {
            return Ok(i);
        }
        if let Ok(i) = k.parse::<usize>() {
            if i < self.tracks.len() {
                return Ok(i);
            }
        }
        Err(anyhow!(
            "no track '{key}'. Tracks: [{}]",
            self.tracks
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    pub fn pattern_index(&self, key: &str) -> Result<usize> {
        let k = key.trim().to_lowercase();
        if let Some(i) = self
            .patterns
            .iter()
            .position(|p| p.name.to_lowercase() == k)
        {
            return Ok(i);
        }
        Err(anyhow!(
            "no pattern '{key}'. Patterns: [{}]",
            self.patterns
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    /// Sections to play, resolving the empty-arrangement default.
    pub fn song_sections(&self) -> Vec<Section> {
        if self.arrangement.is_empty() {
            self.patterns
                .first()
                .map(|p| {
                    vec![Section {
                        pattern: p.name.clone(),
                        repeats: 1,
                    }]
                })
                .unwrap_or_default()
        } else {
            self.arrangement.clone()
        }
    }

    pub fn song_steps(&self) -> u32 {
        self.song_sections()
            .iter()
            .filter_map(|s| {
                self.pattern_index(&s.pattern)
                    .ok()
                    .map(|i| self.patterns[i].steps() * s.repeats)
            })
            .sum()
    }

    /// 1-based bar number (fractional) at a song beat, counting each
    /// pattern's own bar length (3/4 bars are 3 beats long).
    pub fn bar_at_beat(&self, beat: f32) -> f32 {
        let mut b0 = 0.0f32;
        let mut bar = 1.0f32;
        for s in self.song_sections() {
            let Ok(i) = self.pattern_index(&s.pattern) else { continue };
            let pat = &self.patterns[i];
            let bb = pat.steps_per_bar() as f32 / 4.0;
            let span = bb * (pat.bars * s.repeats.max(1)) as f32;
            if beat < b0 + span {
                return bar + (beat - b0) / bb;
            }
            b0 += span;
            bar += (pat.bars * s.repeats.max(1)) as f32;
        }
        bar + (beat - b0) / 4.0
    }

    /// Song beat at a 1-based bar (the inverse of bar_at_beat).
    pub fn beat_at_bar(&self, bar: f32) -> f32 {
        let mut b0 = 0.0f32;
        let mut first = 1.0f32;
        for s in self.song_sections() {
            let Ok(i) = self.pattern_index(&s.pattern) else { continue };
            let pat = &self.patterns[i];
            let n = (pat.bars * s.repeats.max(1)) as f32;
            let bb = pat.steps_per_bar() as f32 / 4.0;
            if bar < first + n {
                return b0 + (bar - first).max(0.0) * bb;
            }
            b0 += n * bb;
            first += n;
        }
        b0 + (bar - first).max(0.0) * 4.0
    }

    pub fn song_beats(&self) -> f32 {
        self.song_steps() as f32 / 4.0
    }

    pub fn bus_index(&self, key: &str) -> Result<usize> {
        let k = key.trim().to_lowercase();
        self.buses
            .iter()
            .position(|b| b.name.to_lowercase() == k)
            .ok_or_else(|| {
                anyhow!(
                    "no bus '{key}'. Buses: [{}]",
                    self.buses
                        .iter()
                        .map(|b| b.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }

    /// Song-beat range of the arrangement section at `index` (0-based),
    /// or of the first section that plays pattern `name`.
    pub fn section_beats(&self, key: &str) -> Result<(f32, f32)> {
        let secs = self.song_sections();
        let mut start = 0.0f32;
        let by_index = key.trim().parse::<usize>().ok();
        for (i, s) in secs.iter().enumerate() {
            let len = self
                .pattern_index(&s.pattern)
                .map(|pi| (self.patterns[pi].steps() * s.repeats.max(1)) as f32 / 4.0)
                .unwrap_or(0.0);
            if by_index == Some(i) || (by_index.is_none() && s.pattern.eq_ignore_ascii_case(key)) {
                return Ok((start, start + len));
            }
            start += len;
        }
        Err(anyhow!(
            "no section '{key}'. Arrangement: [{}] (use an index or a pattern name)",
            secs.iter()
                .map(|s| s.pattern.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    /// Assign stable effect ids everywhere (new effects, legacy projects).
    pub fn ensure_fx_ids(&mut self) -> bool {
        let mut ch = crate::fx::ensure_ids(&mut self.master_effects);
        for t in self.tracks.iter_mut() {
            ch |= crate::fx::ensure_ids(&mut t.effects);
        }
        for b in self.buses.iter_mut() {
            ch |= crate::fx::ensure_ids(&mut b.effects);
        }
        ch
    }

    /// The effect chain of a track, bus or "master".
    pub fn chain_of(&self, owner: &str) -> Option<&Vec<crate::fx::Effect>> {
        if owner.eq_ignore_ascii_case("master") {
            Some(&self.master_effects)
        } else if let Ok(i) = self.track_index(owner) {
            Some(&self.tracks[i].effects)
        } else if let Ok(i) = self.bus_index(owner) {
            Some(&self.buses[i].effects)
        } else {
            None
        }
    }

    pub fn song_seconds(&self) -> f32 {
        self.song_steps() as f32 * self.step_secs()
    }
}
