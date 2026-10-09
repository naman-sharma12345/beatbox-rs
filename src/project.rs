//! The song model: tracks, patterns of notes, an arrangement, a master bus.
//! Everything is plain serde data so a project is one JSON file.

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
                }),
                Effect::Limiter(LimiterFx {
                    ceiling_db: -1.0,
                    release_ms: 80.0,
                }),
            ],
            master_volume_db: 0.0,
            samples: Vec::new(),
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

    pub fn song_seconds(&self) -> f32 {
        self.song_steps() as f32 * self.step_secs()
    }
}
