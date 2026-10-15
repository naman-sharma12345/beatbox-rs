//! Tempo automation (FL's tempo automation clip): tempo points on the song
//! timeline in beats, each a jump or a linear ramp from the previous point.
//! `TempoCurve` maps song beats to seconds and back for the renderer; with no
//! points it is the project's constant bpm, bit for bit the old timing.

use crate::project::{Project, TempoPoint};

/// Table resolution: 1/64 of a beat (a 128th note) between exact points.
const RES: f64 = 64.0;

#[derive(Clone, Debug)]
pub struct TempoCurve {
    /// Constant bpm when there are no points (the fast path).
    base_bpm: f64,
    /// Cumulative seconds at beat k / RES, up to `end_beat`.
    table: Vec<f64>,
    end_beat: f64,
    /// bpm from `end_beat` on.
    end_bpm: f64,
}

/// The bpm at a song beat for a point list (sorted by beat) over a base bpm.
pub fn bpm_at(base: f32, points: &[TempoPoint], beat: f32) -> f32 {
    let mut prev_beat = 0.0f32;
    let mut prev_bpm = base;
    for p in points {
        if beat < p.beat {
            if p.ramp && p.beat > prev_beat {
                let x = ((beat - prev_beat) / (p.beat - prev_beat)).clamp(0.0, 1.0);
                return prev_bpm + (p.bpm - prev_bpm) * x;
            }
            return prev_bpm;
        }
        prev_beat = p.beat;
        prev_bpm = p.bpm;
    }
    prev_bpm
}

impl TempoCurve {
    pub fn new(p: &Project) -> Self {
        let base = p.bpm.clamp(20.0, 400.0) as f64;
        let mut pts: Vec<TempoPoint> = p.tempo_points.iter().filter(|t| t.beat >= 0.0 && t.bpm.is_finite()).cloned().collect();
        for t in pts.iter_mut() {
            t.bpm = t.bpm.clamp(20.0, 400.0);
        }
        pts.sort_by(|a, b| a.beat.partial_cmp(&b.beat).unwrap());
        if pts.is_empty() {
            return TempoCurve { base_bpm: base, table: Vec::new(), end_beat: 0.0, end_bpm: base };
        }
        let end_beat = pts.last().map(|t| t.beat as f64).unwrap_or(0.0);
        let n = (end_beat * RES).ceil() as usize + 1;
        let mut table = Vec::with_capacity(n + 1);
        table.push(0.0f64);
        let base32 = base as f32;
        for k in 0..n {
            // midpoint rule per 1/64 beat: exact for jumps on the grid,
            // second-order for ramps
            let b = (k as f64 + 0.5) / RES;
            let bpm = bpm_at(base32, &pts, b as f32) as f64;
            let last = *table.last().unwrap();
            table.push(last + 60.0 / bpm / RES);
        }
        let end_bpm = pts.last().map(|t| t.bpm as f64).unwrap_or(base);
        TempoCurve { base_bpm: base, table, end_beat: n as f64 / RES, end_bpm }
    }

    pub fn is_constant(&self) -> bool {
        self.table.is_empty()
    }

    /// Seconds from song start to `beat`.
    pub fn secs_at_beat(&self, beat: f64) -> f64 {
        if self.table.is_empty() {
            return beat * 60.0 / self.base_bpm;
        }
        let beat = beat.max(0.0);
        if beat >= self.end_beat {
            let t_end = *self.table.last().unwrap();
            return t_end + (beat - self.end_beat) * 60.0 / self.end_bpm;
        }
        let x = beat * RES;
        let i = x.floor() as usize;
        let f = x - i as f64;
        self.table[i] + (self.table[i + 1] - self.table[i]) * f
    }

    /// Song beat at `secs` from the start (the inverse of secs_at_beat).
    pub fn beat_at_secs(&self, secs: f64) -> f64 {
        if self.table.is_empty() {
            return secs * self.base_bpm / 60.0;
        }
        let secs = secs.max(0.0);
        let t_end = *self.table.last().unwrap();
        if secs >= t_end {
            return self.end_beat + (secs - t_end) * self.end_bpm / 60.0;
        }
        let i = self.table.partition_point(|t| *t <= secs).saturating_sub(1);
        let span = (self.table[i + 1] - self.table[i]).max(1e-12);
        (i as f64 + (secs - self.table[i]) / span) / RES
    }

    /// Seconds at a song step (16th note).
    pub fn secs_at_step(&self, step: f64) -> f64 {
        self.secs_at_beat(step / 4.0)
    }
}

/// Samples from the song start to `beat`, by piecewise integration through
/// the tempo map: the samples of each segment between tempo events, summed
/// (a jump holds its bpm: db * 60 * SR / bpm; a linear ramp in beats from
/// bpm0 to bpm1: 60 * SR * ln(bpm_end / bpm0) / slope, slope = d bpm / d beat).
/// Exact, with no table: a boundary whose sample count is a whole number
/// comes out as that number. Same point rules as `TempoCurve::new` (beats
/// >= 0, finite, bpm clamped to 20..400, sorted; the base bpm before the
/// first point).
pub fn samples_at_beat(p: &Project, beat: f64) -> f64 {
    let base = p.bpm.clamp(20.0, 400.0) as f64;
    let mut pts: Vec<TempoPoint> = p.tempo_points.iter().filter(|t| t.beat >= 0.0 && t.bpm.is_finite()).cloned().collect();
    for t in pts.iter_mut() {
        t.bpm = t.bpm.clamp(20.0, 400.0);
    }
    pts.sort_by(|a, b| a.beat.partial_cmp(&b.beat).unwrap());
    let k = 60.0 * crate::dsp::SR as f64;
    let beat = beat.max(0.0);
    let (mut b0, mut v0) = (0.0f64, base);
    let mut n = 0.0f64;
    for t in &pts {
        let (b1, v1) = (t.beat as f64, t.bpm as f64);
        let end = beat.min(b1);
        if end > b0 {
            n += if t.ramp && b1 > b0 && v1 != v0 {
                let slope = (v1 - v0) / (b1 - b0);
                k * ((v0 + slope * (end - b0)) / v0).ln() / slope
            } else {
                (end - b0) * k / v0
            };
        }
        if beat <= b1 {
            return n;
        }
        b0 = b1;
        v0 = v1;
    }
    n + (beat - b0) * k / v0
}

/// The sample a note at `beat` starts on: `samples_at_beat` truncated, as the
/// renderer truncates note onsets (a value within 1e-6 of a whole number is
/// that number).
pub fn sample_index_at_beat(p: &Project, beat: f64) -> usize {
    (samples_at_beat(p, beat) + 1e-6).floor().max(0.0) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_tempo_is_the_old_timing() {
        let p = Project::new("t", 90.0);
        let c = TempoCurve::new(&p);
        assert!(c.is_constant());
        assert!((c.secs_at_beat(8.0) - 8.0 * 60.0 / 90.0).abs() < 1e-9);
        assert!((c.beat_at_secs(c.secs_at_beat(13.25)) - 13.25).abs() < 1e-9);
    }

    #[test]
    fn jumps_and_ramps_map_both_ways() {
        let mut p = Project::new("t", 120.0);
        // 120 for 4 beats, then jump to 60
        p.tempo_points.push(TempoPoint { beat: 4.0, bpm: 60.0, ramp: false });
        let c = TempoCurve::new(&p);
        assert!((c.secs_at_beat(4.0) - 2.0).abs() < 1e-6);
        assert!((c.secs_at_beat(6.0) - 4.0).abs() < 1e-6);
        assert!((c.secs_at_beat(10.0) - 8.0).abs() < 1e-6, "past the last point: {}", c.secs_at_beat(10.0));
        // ramp 120 -> 60 over beats 0..8: t = 8 * 60 / 60 * ln(2) = 5.545 s
        let mut q = Project::new("t", 120.0);
        q.tempo_points.push(TempoPoint { beat: 8.0, bpm: 60.0, ramp: true });
        let r = TempoCurve::new(&q);
        let want = 8.0 * 60.0 / (120.0 - 60.0) * (120.0f64 / 60.0).ln();
        assert!((r.secs_at_beat(8.0) - want).abs() < 1e-3, "{} vs {want}", r.secs_at_beat(8.0));
        for b in [0.3, 2.0, 7.9, 8.0, 11.5] {
            assert!((r.beat_at_secs(r.secs_at_beat(b)) - b).abs() < 1e-6, "round trip at {b}");
        }
        assert!((bpm_at(120.0, &q.tempo_points, 4.0) - 90.0).abs() < 1e-4);
    }

    /// B75: the exact piecewise integration agrees with the closed forms and
    /// with the renderer's table.
    #[test]
    fn piecewise_samples_match_the_closed_forms() {
        let sr = crate::dsp::SR as f64;
        let mut p = Project::new("t", 120.0);
        assert_eq!(samples_at_beat(&p, 8.0), 4.0 * sr, "constant: 8 beats at 120 = 4 s");
        // 120 for 4 beats, then 60: beat 6 = 2 s + 2 s
        p.tempo_points.push(TempoPoint { beat: 4.0, bpm: 60.0, ramp: false });
        assert_eq!(samples_at_beat(&p, 6.0), 4.0 * sr);
        assert_eq!(sample_index_at_beat(&p, 6.0), 176_400);
        // a ramp 120 -> 60 over beats 0..8: 8 * 60 / 60 * ln 2 s, then 60
        let mut q = Project::new("t", 120.0);
        q.tempo_points.push(TempoPoint { beat: 8.0, bpm: 60.0, ramp: true });
        let want = 8.0 * (2.0f64).ln() * sr;
        assert!((samples_at_beat(&q, 8.0) - want).abs() < 1e-6, "{} vs {want}", samples_at_beat(&q, 8.0));
        assert!((samples_at_beat(&q, 9.0) - want - sr).abs() < 1e-6);
        // the table integrates a ramp by the midpoint rule per 1/64 beat: over
        // this 8-beat ramp its error grows to about 0.11 samples at the end
        let c = TempoCurve::new(&q);
        for b in [0.5, 3.3, 7.99, 8.0, 12.0] {
            let d = samples_at_beat(&q, b) - c.secs_at_beat(b) * sr;
            assert!(d.abs() < 0.25, "beat {b}: {d} samples from the table");
        }
    }
}
