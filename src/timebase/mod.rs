// Adapted from SoundCraft `crates/time/src/lib.rs` (commit eac0edd).
// Copyright (c) 2026 ArtCraft Team and the SoundCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your option;
// used in Beatbox under the MIT License. See THIRD_PARTY_NOTICES.md.

//! PPQ timebase: ticks, a tempo/meter map, grids, timecode and counter formats.
//!
//! Groundwork for native clips. Every musical position is an integer tick at
//! [`TICKS_PER_QUARTER`] (960 PPQ); a [`TempoMap`] turns ticks into absolute
//! samples (`i64`) at a [`SampleRate`] and back, with tempo and meter changes.
//! [`GridValue`] computes grid lines and snapping, [`Timecode`] SMPTE labels
//! (drop-frame included), and [`format_position`] / [`parse_position`] the five
//! counter formats (Bars|Beats, Min:Secs, Timecode, Feet+Frames, Samples).
//!
//! The renderer still works in 16th-note *steps*; the bridge at the bottom of
//! this file ([`step_to_tick`], [`tick_to_step`], [`TempoMap::for_project`])
//! maps Beatbox's step grid onto ticks so clips can migrate one at a time.
//!
//! No panics: every conversion saturates or returns a [`TimeError`].

mod format;
mod grid;
mod tempo;
mod timecode;

pub use format::{format_length, format_position, parse_position, TimeFormat};
pub use grid::{GridValue, NoteValue};
pub use tempo::{BarBeat, MeterEvent, TempoEvent, TempoMap, TICKS_PER_QUARTER};
pub use timecode::{feet_frames, FrameRate, Timecode};

/// A position or length in samples.
pub type Samples = i64;

/// Errors from parsing or converting time values.
#[derive(Debug, Clone, PartialEq)]
pub enum TimeError {
    Parse(String, &'static str),
    SampleRate(u32),
    Tempo(f64),
    Meter(u32, u32),
}

impl std::fmt::Display for TimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TimeError::Parse(s, kind) => write!(f, "cannot parse `{s}` as a {kind} value"),
            TimeError::SampleRate(hz) => write!(f, "invalid sample rate {hz}"),
            TimeError::Tempo(bpm) => write!(f, "invalid tempo {bpm}"),
            TimeError::Meter(n, d) => write!(f, "invalid meter {n}/{d}"),
        }
    }
}

impl std::error::Error for TimeError {}

/// A sample rate (8 kHz – 768 kHz).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SampleRate(u32);

impl SampleRate {
    pub const HZ_44100: SampleRate = SampleRate(44_100);
    pub const HZ_48000: SampleRate = SampleRate(48_000);
    pub const COMMON: [u32; 8] = [
        44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
    ];

    pub fn new(hz: u32) -> Result<Self, TimeError> {
        if (8_000..=768_000).contains(&hz) {
            Ok(SampleRate(hz))
        } else {
            Err(TimeError::SampleRate(hz))
        }
    }
    pub fn hz(self) -> u32 {
        self.0
    }
    pub fn as_f64(self) -> f64 {
        f64::from(self.0)
    }
    /// Samples → seconds.
    pub fn seconds(self, s: Samples) -> f64 {
        s as f64 / self.as_f64()
    }
    /// Seconds → samples, rounded to nearest and saturated.
    pub fn samples(self, seconds: f64) -> Samples {
        to_samples(seconds * self.as_f64())
    }
}

impl Default for SampleRate {
    /// Beatbox renders at 44.1 kHz (`dsp::SR`).
    fn default() -> Self {
        SampleRate::HZ_44100
    }
}

/// Round a float sample count to `i64`, mapping NaN to 0 and saturating infinities.
pub fn to_samples(v: f64) -> Samples {
    if v.is_nan() {
        0
    } else if v >= i64::MAX as f64 {
        i64::MAX
    } else if v <= i64::MIN as f64 {
        i64::MIN
    } else {
        v.round() as i64
    }
}

/// A half-open sample range `[start, end)`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct Range {
    pub start: Samples,
    pub end: Samples,
}

impl Range {
    pub fn new(a: Samples, b: Samples) -> Self {
        if a <= b {
            Range { start: a, end: b }
        } else {
            Range { start: b, end: a }
        }
    }
    pub fn point(at: Samples) -> Self {
        Range { start: at, end: at }
    }
    pub fn len(&self) -> Samples {
        self.end.saturating_sub(self.start)
    }
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
    pub fn contains(&self, s: Samples) -> bool {
        s >= self.start && s < self.end
    }
    pub fn overlaps(&self, o: &Range) -> bool {
        self.start < o.end && o.start < self.end
    }
    pub fn intersect(&self, o: &Range) -> Option<Range> {
        let s = self.start.max(o.start);
        let e = self.end.min(o.end);
        (s < e).then_some(Range { start: s, end: e })
    }
    pub fn shifted(&self, by: Samples) -> Range {
        Range {
            start: self.start.saturating_add(by),
            end: self.end.saturating_add(by),
        }
    }
}

// ---------------- Beatbox bridge (not from SoundCraft) ----------------

/// Ticks in one Beatbox step (a 16th note).
pub const TICKS_PER_STEP: i64 = TICKS_PER_QUARTER / 4;

/// Beatbox step (fractional 16ths) → nearest tick. NaN maps to 0, huge values saturate.
pub fn step_to_tick(step: f32) -> i64 {
    to_samples(f64::from(step) * TICKS_PER_STEP as f64)
}

/// Tick → Beatbox step (fractional 16ths).
pub fn tick_to_step(tick: i64) -> f32 {
    (tick as f64 / TICKS_PER_STEP as f64) as f32
}

impl TempoMap {
    /// The map for a Beatbox project today: one tempo (the project BPM, clamped to the
    /// renderer's 20..400 range) and 4/4 throughout.
    pub fn for_project(p: &crate::project::Project) -> TempoMap {
        let bpm = f64::from(p.bpm.clamp(20.0, 400.0));
        TempoMap::new(bpm, 4, 4).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_rate_bounds() {
        assert!(SampleRate::new(48_000).is_ok());
        assert!(SampleRate::new(0).is_err());
        assert!(SampleRate::new(10_000_000).is_err());
        assert_eq!(SampleRate::default().hz() as f32, crate::dsp::SR);
    }

    #[test]
    fn seconds_round_trip() {
        let sr = SampleRate::HZ_44100;
        assert_eq!(sr.samples(1.0), 44_100);
        assert!((sr.seconds(22_050) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn to_samples_is_total() {
        assert_eq!(to_samples(f64::NAN), 0);
        assert_eq!(to_samples(f64::INFINITY), i64::MAX);
        assert_eq!(to_samples(f64::NEG_INFINITY), i64::MIN);
    }

    #[test]
    fn range_ops() {
        let a = Range::new(10, 0);
        assert_eq!(a, Range { start: 0, end: 10 });
        assert_eq!(a.intersect(&Range::new(5, 20)), Some(Range::new(5, 10)));
        assert_eq!(a.intersect(&Range::new(10, 20)), None);
        assert!(a.contains(9) && !a.contains(10));
        assert_eq!(a.shifted(i64::MAX).end, i64::MAX);
    }

    #[test]
    fn errors_display() {
        assert_eq!(TimeError::Meter(4, 3).to_string(), "invalid meter 4/3");
        assert!(TimeError::Parse("x".into(), "timecode")
            .to_string()
            .contains("`x`"));
    }

    #[test]
    fn steps_and_ticks() {
        assert_eq!(TICKS_PER_STEP, 240);
        assert_eq!(step_to_tick(16.0), TICKS_PER_QUARTER * 4);
        assert_eq!(step_to_tick(12.5), 3000);
        assert_eq!(step_to_tick(f32::NAN), 0);
        assert_eq!(tick_to_step(3000), 12.5);
        for s in [0.0f32, 1.0, 3.25, 63.0, 255.5] {
            assert_eq!(tick_to_step(step_to_tick(s)), s);
        }
    }

    #[test]
    fn project_map_matches_renderer_step_length() {
        let mut p = crate::project::Project::default();
        p.bpm = 92.0;
        let m = TempoMap::for_project(&p);
        let sr = SampleRate::default();
        // 16 steps (one bar) in samples == the renderer's step_secs * 16
        let want = sr.samples(f64::from(p.step_secs()) * 16.0);
        let got = m.tick_to_samples(step_to_tick(16.0), sr);
        assert!((got - want).abs() <= 1, "{got} vs {want}");
        assert_eq!(m.bar_beat_at_tick(step_to_tick(20.0)).bar, 2);
        // out-of-range bpm clamps instead of failing
        p.bpm = 9000.0;
        assert_eq!(TempoMap::for_project(&p).tempo_at_tick(0), 400.0);
    }
}
