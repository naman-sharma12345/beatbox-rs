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
}

impl Pattern {
    pub fn new(name: &str, bars: u32) -> Self {
        Pattern {
            name: name.to_string(),
            bars: bars.clamp(1, 64),
            clips: BTreeMap::new(),
        }
    }
    pub fn steps(&self) -> u32 {
        self.bars * STEPS_PER_BAR
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
