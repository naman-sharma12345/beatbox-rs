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

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    /// Stable id ("n12"), unique in the project and kept across edits,
    /// undo and save/load. Empty on a fresh note until the engine assigns one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
}

/// Notes compare by their musical content; the id is identity, not content
/// (a pasted copy equals its source).
impl PartialEq for Note {
    fn eq(&self, o: &Self) -> bool {
        self.start == o.start
            && self.len == o.len
            && self.pitch == o.pitch
            && self.vel == o.vel
            && self.prob == o.prob
            && self.offset == o.offset
            && self.slide_to == o.slide_to
    }
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
            id: String::new(),
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
fn is_false(x: &bool) -> bool {
    !*x
}

fn is_zero(x: &f32) -> bool {
    *x == 0.0
}

fn zero() -> f32 {
    0.0
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Track {
    /// Stable id, unique in the project, kept across renames, undo and
    /// save/load (assigned by the engine; tools accept it wherever they take
    /// a name or index).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
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
            id: String::new(),
        }
    }
}

/// An aux send from a track to a bus.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Send {
    /// Stable id, unique in the project, kept across renames, undo and
    /// save/load (assigned by the engine; tools accept it wherever they take
    /// a name or index).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
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
    /// Stable id, unique in the project, kept across renames, undo and
    /// save/load (assigned by the engine; tools accept it wherever they take
    /// a name or index).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
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
            id: String::new(),
        }
    }
}

/// A pattern placed on the playlist (FL-style pattern clip): `pattern`
/// plays from `start_bar` for `bars` bars (looping the pattern when the
/// clip is longer), optionally only some of its tracks.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PlaylistClip {
    /// Stable id, unique in the project, kept across renames, undo and
    /// save/load (assigned by the engine; tools accept it wherever they take
    /// a name or index).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub pattern: String,
    pub start_bar: u32,
    pub bars: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<String>,
    /// Playlist lane, for display only (clips on any lane all play).
    #[serde(default)]
    pub lane: u32,
    /// The clip starts this many bars into its pattern (looping), so a split
    /// or a trimmed start plays on where the original left off.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub offset_bars: u32,
}

fn is_zero_u32(x: &u32) -> bool {
    *x == 0
}

/// A reusable automation clip: a shape for one parameter, in beats from
/// the clip start, that can be placed anywhere in the song.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AutomationClip {
    /// Stable id, unique in the project, kept across renames, undo and
    /// save/load (assigned by the engine; tools accept it wherever they take
    /// a name or index).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
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
    /// Stable id, unique in the project, kept across renames, undo and
    /// save/load (assigned by the engine; tools accept it wherever they take
    /// a name or index).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
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
    let step = if d == 8 && n % 3 == 0 && n > 3 {
        unit * 3
    } else {
        unit
    };
    let bar = meter_steps(meter);
    (0..bar)
        .step_by(step as usize)
        .map(|s| (s, s == 0))
        .collect()
}

impl Pattern {
    pub fn new(name: &str, bars: u32) -> Self {
        Pattern {
            name: name.to_string(),
            bars: bars.clamp(1, 64),
            clips: BTreeMap::new(),
            meter: None,
            id: String::new(),
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
    /// Project file schema. 0/absent = written before stable ids; the engine
    /// migrates on load (every object gets an id) and writes the current one.
    #[serde(default)]
    pub schema_version: u32,
    /// Next stable-id number (ids are never reused, even after a delete).
    #[serde(default)]
    pub id_seq: u64,
    /// MIDI controller setup (links, the keys track, record grid), written
    /// by save_project and restored by load_project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi: Option<MidiSetup>,
    /// The song this project was built from (song_create): its spec, seed,
    /// sections and last checks, so song_patch can change it by path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub song: Option<crate::song::SongState>,
    /// Every seed a generating tool ran with (given or fresh), oldest first,
    /// so any arrangement can be rebuilt on demand: rerun `tool` with `args`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seed_log: Vec<SeedEntry>,
    /// Concert-pitch offset in cents for every pitched instrument (A4 =
    /// 440 * 2^(cents/1200) Hz), e.g. to sit the backing on a singer's own
    /// tuning (cover_song match_tuning). Drum kits and audio clips are not
    /// moved. 0 (the default, not written) is strict A440.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub tuning_cents: f32,
}

fn is_zero_f32(v: &f32) -> bool {
    *v == 0.0
}

/// One generation: the tool, the seed it ran with, where the seed came
/// from ("given" or "fresh"), the call's arguments (seed included) and
/// when (unix seconds).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SeedEntry {
    pub tool: String,
    pub seed: u64,
    pub source: String,
    #[serde(default)]
    pub args: serde_json::Value,
    #[serde(default)]
    pub at: u64,
}

/// The seed log keeps the newest entries up to this many.
pub const SEED_LOG_MAX: usize = 256;

/// A saved MIDI controller setup: which controls drive what, which track the
/// keyboard plays, the record grid and live note length.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MidiSetup {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<MidiLinkSave>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub track: String,
    #[serde(default = "one_f32")]
    pub quantize: f32,
    #[serde(default = "default_gate")]
    pub gate_s: f32,
}

fn default_gate() -> f32 {
    0.6
}

/// One saved link: a control label ('cc 7 ch 2', 'note 36') and its target.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MidiLinkSave {
    pub control: String,
    pub target: String,
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
    /// Stable id, unique in the project, kept across renames, undo and
    /// save/load (assigned by the engine; tools accept it wherever they take
    /// a name or index).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
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
    /// Semitones the clip's audio is transposed (-24..24; length and grid
    /// place kept, so an acapella or loop sits in the song's key). 0 (the
    /// default) renders the sample untouched, bit for bit.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pitch_st: f32,
    /// Voice mode for `pitch_st`: a TD-PSOLA (vocal::psola, formants kept)
    /// instead of the default WSOLA+resample shifter - a transposed singer
    /// keeps their tone. Best within +-7 semitones.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pitch_voice: bool,
    /// In-place time-stretch: 1 (default) plays the clip untouched; 2 plays
    /// it twice as long at the same pitch, 0.5 half as long. The start stays
    /// on its beat and the end follows the stretch. Render-time, not a
    /// bounce - 1 renders the sample bit for bit.
    #[serde(default = "one_f32", skip_serializing_if = "is_one")]
    pub stretch: f32,
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
            tuning_cents: 0.0,
            schema_version: crate::ids::SCHEMA_VERSION,
            id_seq: 0,
            midi: None,
            song: None,
            seed_log: Vec::new(),
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
        if let Some(i) = self
            .tracks
            .iter()
            .position(|t| !t.id.is_empty() && t.id == key.trim())
        {
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
            .position(|p| p.name.to_lowercase() == k || (!p.id.is_empty() && p.id == key.trim()))
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
            let Ok(i) = self.pattern_index(&s.pattern) else {
                continue;
            };
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
            let Ok(i) = self.pattern_index(&s.pattern) else {
                continue;
            };
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
            .position(|b| b.name.to_lowercase() == k || (!b.id.is_empty() && b.id == key.trim()))
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
    /// Run `Effect::validate` over every effect in the project (tracks,
    /// buses, master). Cheap: reads plain parameter fields, allocates
    /// nothing sized by them. The message names where the bad effect is.
    /// Bring global timing fields into the ranges every use site already
    /// clamps to (`step_secs` and `TempoCurve` clamp bpm to 20..=400, the
    /// renderer clamps swing to 0..=1). A hostile or corrupt project file
    /// with bpm 0 / negative / 1e300 or an extreme swing otherwise reaches
    /// raw-`bpm` math in some voice paths and hangs the render instead of
    /// making bad music. Clamping (not rejecting) keeps degenerate projects
    /// loadable and renderable, matching validate_effects' callers' policy.
    pub fn sanitize_timing(&mut self) {
        self.bpm = self.bpm.clamp(20.0, 400.0);
        if !self.bpm.is_finite() {
            self.bpm = 120.0;
        }
        self.swing = self.swing.clamp(0.0, 1.0);
        if !self.swing.is_finite() {
            self.swing = 0.0;
        }
    }

    pub fn validate_effects(&self) -> Result<(), String> {
        for t in &self.tracks {
            for (i, fx) in t.effects.iter().enumerate() {
                fx.validate().map_err(|m| {
                    format!("track '{}' effect {i} ({}): {m}", t.name, fx.type_name())
                })?;
            }
        }
        for b in &self.buses {
            for (i, fx) in b.effects.iter().enumerate() {
                fx.validate().map_err(|m| {
                    format!("bus '{}' effect {i} ({}): {m}", b.name, fx.type_name())
                })?;
            }
        }
        for (i, fx) in self.master_effects.iter().enumerate() {
            fx.validate()
                .map_err(|m| format!("master effect {i} ({}): {m}", fx.type_name()))?;
        }
        self.validate_lanes()
    }

    /// An automation lane on `fx.<i>.<param>` may not drive that effect to a
    /// value `Effect::validate` rejects. Checks the lane's lowest and
    /// highest point value against the target effect (what the render
    /// would apply). Lanes whose target, effect or parameter does not exist,
    /// and values that do not deserialize, are left to the render's existing
    /// skip behaviour. Only effects with a validate rule can be rejected.
    pub fn validate_lanes(&self) -> Result<(), String> {
        use crate::automation::{parse_target, set_path, Target};
        for lane in &self.automation {
            let Some(Target::Fx(i, path)) = parse_target(&lane.param) else {
                continue;
            };
            let Some((lo, hi)) = lane.range() else {
                continue;
            };
            let chain: &[Effect] = if lane.target.eq_ignore_ascii_case("master") {
                &self.master_effects
            } else if let Ok(t) = self.track_index(&lane.target) {
                &self.tracks[t].effects
            } else if let Ok(b) = self.bus_index(&lane.target) {
                &self.buses[b].effects
            } else {
                continue;
            };
            let Some(fx) = chain.get(i) else {
                continue;
            };
            let Ok(base) = serde_json::to_value(fx) else {
                continue;
            };
            for x in [lo, hi] {
                let mut v = base.clone();
                if !set_path(&mut v, &path, x) {
                    break;
                }
                let Ok(e) = serde_json::from_value::<Effect>(v) else {
                    continue;
                };
                e.validate().map_err(|m| {
                    format!(
                        "automation lane '{}' {} reaches {x}: {m}",
                        lane.target, lane.param
                    )
                })?;
            }
        }
        Ok(())
    }

    pub fn ensure_fx_ids(&mut self) -> bool {
        let mut ch = crate::fx::ensure_ids(&mut self.master_effects);
        for t in self.tracks.iter_mut() {
            ch |= crate::fx::ensure_ids(&mut t.effects);
        }
        for b in self.buses.iter_mut() {
            ch |= crate::fx::ensure_ids(&mut b.effects);
        }
        ch | crate::ids::ensure_ids(self, None)
    }

    /// `ensure_fx_ids` after an edit, telling copies from originals by
    /// where each id lived before the edit (`ids::owner_keys`).
    pub fn ensure_ids_after(&mut self, before: &crate::ids::OwnerKeys) -> bool {
        let mut ch = crate::fx::ensure_ids(&mut self.master_effects);
        for t in self.tracks.iter_mut() {
            ch |= crate::fx::ensure_ids(&mut t.effects);
        }
        for b in self.buses.iter_mut() {
            ch |= crate::fx::ensure_ids(&mut b.effects);
        }
        ch | crate::ids::ensure_ids(self, Some(before))
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

/// The audio clip a tool's arguments point at: `id` ("clip3", or a legacy
/// numeric position) or `index`.
pub fn audio_clip_index(p: &Project, a: &serde_json::Value) -> Result<usize> {
    let n = p.audio_clips.len();
    let pick = |i: usize| {
        if i < n {
            Ok(i)
        } else {
            Err(anyhow!(
                "no audio clip {i}; there are {n} (list_audio_clips)"
            ))
        }
    };
    if let Some(v) = a.get("id").or_else(|| a.get("clip")) {
        if let Some(s) = v.as_str() {
            let s = s.trim();
            if let Some(i) = p.audio_clips.iter().position(|c| c.id == s) {
                return Ok(i);
            }
            if let Ok(i) = s.parse::<usize>() {
                return pick(i);
            }
            return Err(anyhow!("no audio clip '{s}' (list_audio_clips shows ids)"));
        }
        if let Some(i) = v.as_u64() {
            return pick(i as usize);
        }
    }
    match a.get("index").and_then(|v| v.as_u64()) {
        Some(i) => pick(i as usize),
        None => Err(anyhow!(
            "give the clip's id (\"clip3\") or index (list_audio_clips)"
        )),
    }
}
