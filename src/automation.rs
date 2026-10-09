//! Automation lanes: breakpoint envelopes over song time (in beats) that
//! drive a track's volume / pan, any numeric instrument parameter, or any
//! numeric effect parameter. Lanes are plain serde data on the project.
//!
//! Targets (the `param` of a lane):
//! - `volume` (dB, overrides the fader while the lane exists), `pan` (-1..1)
//! - `instrument.<path>` e.g. `instrument.cutoff`, `instrument.amp_env.release`
//!   (sampled at every note-on)
//! - `fx.<index>.<param>` e.g. `fx.0.cutoff`, `fx.2.mix` (evaluated per
//!   64-sample block)
//!
//! The owner of a lane is a track name, a bus name or `master`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Curve {
    /// Straight line to the next point.
    #[default]
    Linear,
    /// Hold this value until the next point.
    Step,
    /// S-shaped (smoothstep) ease to the next point.
    Smooth,
}

impl Curve {
    pub fn parse(s: &str) -> Option<Curve> {
        match s.to_lowercase().as_str() {
            "linear" | "line" | "ramp" => Some(Curve::Linear),
            "step" | "hold" | "jump" => Some(Curve::Step),
            "smooth" | "ease" | "s" | "s_curve" => Some(Curve::Smooth),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AutoPoint {
    /// Song position in beats (quarter notes) from the start of the song.
    pub beat: f32,
    pub value: f32,
    /// Shape of the segment from this point to the next one.
    #[serde(default)]
    pub curve: Curve,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AutomationLane {
    /// Track name, bus name or "master".
    pub target: String,
    /// `volume`, `pan`, `instrument.<path>` or `fx.<index>.<param>`.
    pub param: String,
    #[serde(default)]
    pub points: Vec<AutoPoint>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

impl AutomationLane {
    pub fn new(target: &str, param: &str) -> Self {
        AutomationLane {
            target: target.to_string(),
            param: param.to_string(),
            points: Vec::new(),
            enabled: true,
        }
    }

    pub fn sort(&mut self) {
        self.points.sort_by(|a, b| a.beat.total_cmp(&b.beat));
    }

    pub fn is_target(&self, owner: &str, param: &str) -> bool {
        self.target.eq_ignore_ascii_case(owner) && self.param.eq_ignore_ascii_case(param)
    }

    /// Value at song position `beat`. Holds the first/last value outside the
    /// point range. `None` when the lane has no points.
    pub fn value_at(&self, beat: f32) -> Option<f32> {
        let pts = &self.points;
        let first = pts.first()?;
        if beat <= first.beat {
            return Some(first.value);
        }
        let last = pts.last()?;
        if beat >= last.beat {
            return Some(last.value);
        }
        // binary search for the segment
        let idx = pts.partition_point(|p| p.beat <= beat);
        let a = &pts[idx - 1];
        let b = &pts[idx];
        let span = (b.beat - a.beat).max(1e-9);
        let t = ((beat - a.beat) / span).clamp(0.0, 1.0);
        let t = match a.curve {
            Curve::Linear => t,
            Curve::Step => 0.0,
            Curve::Smooth => t * t * (3.0 - 2.0 * t),
        };
        Some(a.value + (b.value - a.value) * t)
    }

    pub fn range(&self) -> Option<(f32, f32)> {
        if self.points.is_empty() {
            return None;
        }
        let lo = self
            .points
            .iter()
            .map(|p| p.value)
            .fold(f32::INFINITY, f32::min);
        let hi = self
            .points
            .iter()
            .map(|p| p.value)
            .fold(f32::NEG_INFINITY, f32::max);
        Some((lo, hi))
    }
}

/// The parsed shape of a lane's `param`.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Volume,
    Pan,
    Instrument(Vec<String>),
    Fx(usize, Vec<String>),
}

pub fn parse_target(param: &str) -> Option<Target> {
    let p = param.trim().to_lowercase();
    match p.as_str() {
        "volume" | "volume_db" | "vol" => return Some(Target::Volume),
        "pan" => return Some(Target::Pan),
        _ => {}
    }
    let mut parts = p.split('.');
    match parts.next()? {
        "instrument" | "inst" => {
            let path: Vec<String> = parts.map(String::from).collect();
            (!path.is_empty() && path.iter().all(|s| !s.is_empty()))
                .then_some(Target::Instrument(path))
        }
        "fx" | "effect" => {
            let idx = parts.next()?.parse().ok()?;
            let path: Vec<String> = parts.map(String::from).collect();
            (!path.is_empty() && path.iter().all(|s| !s.is_empty()))
                .then_some(Target::Fx(idx, path))
        }
        _ => None,
    }
}

/// Canonical spelling of a param (so `vol` and `volume_db` share a lane).
pub fn canonical(param: &str) -> Option<String> {
    Some(match parse_target(param)? {
        Target::Volume => "volume".into(),
        Target::Pan => "pan".into(),
        Target::Instrument(p) => format!("instrument.{}", p.join(".")),
        Target::Fx(i, p) => format!("fx.{i}.{}", p.join(".")),
    })
}

/// Read a numeric value at a dotted path in a serde value.
pub fn get_path(v: &Value, path: &[String]) -> Option<f64> {
    let mut cur = v;
    for k in path {
        cur = cur.get(k)?;
    }
    cur.as_f64()
}

/// Write a number at a dotted path, keeping integers integers. Returns false
/// if the path does not exist or is not numeric.
pub fn set_path(v: &mut Value, path: &[String], x: f32) -> bool {
    let mut cur = v;
    for k in path {
        match cur.get_mut(k) {
            Some(n) => cur = n,
            None => return false,
        }
    }
    if cur.is_u64() || cur.is_i64() {
        *cur = Value::from(x.round().max(0.0) as u64);
        true
    } else if cur.is_f64() {
        *cur = serde_json::Number::from_f64(x as f64)
            .map(Value::Number)
            .unwrap_or(Value::Null);
        true
    } else {
        false
    }
}

/// Sensible (min, max) defaults for a parameter name, used by
/// generate_automation and validation.
pub fn default_range(param: &str) -> (f32, f32) {
    let leaf = param.rsplit('.').next().unwrap_or(param).to_lowercase();
    match leaf.as_str() {
        "volume" | "volume_db" | "vol" => (-30.0, 0.0),
        "pan" => (-1.0, 1.0),
        "cutoff" | "tone" | "high_freq" => (200.0, 12000.0),
        "low_freq" => (40.0, 600.0),
        "resonance" | "mix" | "amount" | "drive" | "damping" | "size" | "feedback" => (0.0, 1.0),
        "db" | "low_db" | "mid_db" | "high_db" | "makeup_db" => (-12.0, 6.0),
        "bits" => (4.0, 16.0),
        "decay_s" => (0.3, 6.0),
        "knee_db" => (0.0, 12.0),
        "rate_hz" | "lfo_rate" => (0.1, 8.0),
        _ => (0.0, 1.0),
    }
}

/// True for parameters that feel logarithmic (frequencies).
pub fn is_log_param(param: &str) -> bool {
    let leaf = param.rsplit('.').next().unwrap_or(param).to_lowercase();
    matches!(leaf.as_str(), "cutoff" | "tone" | "low_freq" | "high_freq") || leaf.ends_with("_hz")
}

/// A semantic automation shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    SweepUp,
    SweepDown,
    FadeIn,
    FadeOut,
    Pump,
    Lfo,
}

impl Shape {
    pub fn parse(s: &str) -> Option<Shape> {
        Some(match s.to_lowercase().replace('-', "_").as_str() {
            "riser" | "sweep_up" | "build" | "rise" => Shape::SweepUp,
            "sweep_down" | "fall" | "drop_sweep" | "downlifter" => Shape::SweepDown,
            "fade_in" => Shape::FadeIn,
            "fade_out" => Shape::FadeOut,
            "pump" | "duck" => Shape::Pump,
            "lfo" | "wobble" => Shape::Lfo,
            _ => return None,
        })
    }
    pub const NAMES: [&'static str; 7] = [
        "riser",
        "sweep_up",
        "sweep_down",
        "fade_in",
        "fade_out",
        "pump",
        "lfo",
    ];
}

/// Map 0..1 onto [lo, hi], logarithmically for frequency params.
fn lerp_param(lo: f32, hi: f32, t: f32, log: bool) -> f32 {
    if log && lo > 0.0 && hi > 0.0 {
        (lo.ln() + (hi.ln() - lo.ln()) * t).exp()
    } else {
        lo + (hi - lo) * t
    }
}

/// Generate breakpoints for `shape` from `start` to `end` beats.
/// `rate_beats` is the period for pump / lfo (e.g. 1 = quarter note, 0.5 = 8th).
pub fn generate(
    shape: Shape,
    start: f32,
    end: f32,
    lo: f32,
    hi: f32,
    rate_beats: f32,
    log: bool,
) -> Vec<AutoPoint> {
    let end = end.max(start + 0.25);
    let len = end - start;
    let pt = |beat: f32, value: f32, curve: Curve| AutoPoint { beat, value, curve };
    match shape {
        Shape::SweepUp | Shape::SweepDown => {
            // 9 points with an exponential-feeling acceleration toward the end
            let (a, b) = if shape == Shape::SweepUp {
                (lo, hi)
            } else {
                (hi, lo)
            };
            (0..=8)
                .map(|i| {
                    let x = i as f32 / 8.0;
                    let t = if shape == Shape::SweepUp {
                        x * x
                    } else {
                        1.0 - (1.0 - x) * (1.0 - x)
                    };
                    let v = lerp_param(a, b, t, log);
                    pt(start + len * x, v, Curve::Linear)
                })
                .collect()
        }
        Shape::FadeIn => vec![pt(start, lo, Curve::Smooth), pt(end, hi, Curve::Linear)],
        Shape::FadeOut => vec![pt(start, hi, Curve::Smooth), pt(end, lo, Curve::Linear)],
        Shape::Pump => {
            let rate = rate_beats.clamp(0.125, 16.0);
            let mut v = Vec::new();
            let mut b = start;
            while b < end - 1e-4 {
                v.push(pt(b, lo, Curve::Smooth));
                v.push(pt((b + rate * 0.6).min(end), hi, Curve::Linear));
                b += rate;
            }
            v.push(pt(end, hi, Curve::Linear));
            v
        }
        Shape::Lfo => {
            let rate = rate_beats.clamp(0.0625, 64.0);
            let per_cycle = 8;
            let n = ((len / rate) * per_cycle as f32).ceil().clamp(2.0, 4096.0) as usize;
            (0..=n)
                .map(|i| {
                    let beat = start + len * i as f32 / n as f32;
                    let phase = (beat - start) / rate;
                    let s = 0.5 - 0.5 * (std::f32::consts::TAU * phase).cos();
                    pt(beat, lerp_param(lo, hi, s, log), Curve::Smooth)
                })
                .collect()
        }
    }
}

/// Per-block evaluation helper: values of `lane` for each block of
/// `block` samples, for a render that starts at `beat0` with `beats_per_sample`.
/// Song time wraps every `song_beats` (multi-loop renders).
pub fn block_values(
    lane: &AutomationLane,
    n_samples: usize,
    block: usize,
    beat0: f32,
    beats_per_sample: f32,
    song_beats: f32,
) -> Vec<f32> {
    let blocks = n_samples.div_ceil(block.max(1)).max(1);
    (0..blocks)
        .map(|k| {
            let mut b = beat0 + (k * block) as f32 * beats_per_sample;
            if song_beats > 0.0 && b >= song_beats {
                b %= song_beats;
            }
            lane.value_at(b).unwrap_or(0.0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(points: &[(f32, f32, Curve)]) -> AutomationLane {
        let mut l = AutomationLane::new("pad", "volume");
        l.points = points
            .iter()
            .map(|&(beat, value, curve)| AutoPoint { beat, value, curve })
            .collect();
        l
    }

    #[test]
    fn curves_interpolate() {
        let l = lane(&[(0.0, 0.0, Curve::Linear), (4.0, 1.0, Curve::Linear)]);
        assert!((l.value_at(2.0).unwrap() - 0.5).abs() < 1e-6);
        assert_eq!(l.value_at(-1.0), Some(0.0));
        assert_eq!(l.value_at(10.0), Some(1.0));
        let s = lane(&[(0.0, 0.0, Curve::Step), (4.0, 1.0, Curve::Linear)]);
        assert_eq!(s.value_at(3.9), Some(0.0));
        let m = lane(&[(0.0, 0.0, Curve::Smooth), (4.0, 1.0, Curve::Linear)]);
        let q = m.value_at(1.0).unwrap();
        assert!(q < 0.25 && q > 0.0, "smoothstep eases in: {q}");
    }

    #[test]
    fn targets_parse() {
        assert_eq!(parse_target("vol"), Some(Target::Volume));
        assert_eq!(
            parse_target("fx.1.cutoff"),
            Some(Target::Fx(1, vec!["cutoff".into()]))
        );
        assert_eq!(
            canonical("instrument.amp_env.release").as_deref(),
            Some("instrument.amp_env.release")
        );
        assert!(parse_target("fx.x.cutoff").is_none());
        assert!(parse_target("banana").is_none());
    }

    #[test]
    fn shapes_have_right_direction() {
        let up = generate(Shape::SweepUp, 0.0, 16.0, 200.0, 12000.0, 1.0, true);
        assert!(up.first().unwrap().value < up.last().unwrap().value);
        assert!((up.last().unwrap().beat - 16.0).abs() < 1e-4);
        let out = generate(Shape::FadeOut, 8.0, 16.0, -60.0, 0.0, 1.0, false);
        assert_eq!(out[0].value, 0.0);
        assert_eq!(out[1].value, -60.0);
        let lfo = generate(Shape::Lfo, 0.0, 4.0, 0.0, 1.0, 1.0, false);
        let l = AutomationLane {
            points: lfo,
            ..AutomationLane::new("a", "pan")
        };
        // tempo-synced: one full cycle per beat -> min at every beat, max halfway
        assert!(l.value_at(1.0).unwrap() < 0.05);
        assert!(l.value_at(1.5).unwrap() > 0.95);
        let pump = generate(Shape::Pump, 0.0, 4.0, 0.2, 1.0, 1.0, false);
        assert!(pump.len() >= 8);
    }
}
