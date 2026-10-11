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
}
