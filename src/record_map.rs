//! Record maps, analysis only: production effects with timestamps
//! (detect_fx), the arrangement (sections, which parts play in each bar,
//! entries and exits: arrangement_map) and a section-by-section, part-by-part
//! comparison of two renditions (score_sections). Out come times, bar numbers
//! and numbers; the optional A/B clips are listening aids written beside the
//! scores, never material for a mix.

use crate::dsp::{Fft, SR};
use crate::engine::Engine;
use crate::tools::{b_or, f_opt, obj, s_opt, s_req, Tool};
use crate::transcribe::{self as tr, Grid, HOP};
use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::f32::consts::PI;
use std::path::{Path, PathBuf};

fn hann(n: usize) -> Vec<f32> {
    (0..n).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos()).collect()
}

fn db(x: f32) -> f32 {
    10.0 * x.max(1e-12).log10()
}

fn pct(v: &[f32], p: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    s[((s.len() - 1) as f32 * p.clamp(0.0, 1.0)).round() as usize]
}

fn r1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

fn r2(x: f32) -> f32 {
    (x * 100.0).round() / 100.0
}

/// Frame features at HOP: level, spectral centroid (log2 Hz), high band
/// (> 5 kHz), low band (30-120 Hz), octave band levels.
pub struct Frames {
    pub e_db: Vec<f32>,
    pub cent: Vec<f32>,
    pub high_db: Vec<f32>,
    pub low_db: Vec<f32>,
    pub oct: Vec<[f32; 9]>,
    pub flux: Vec<f32>,
}

pub const OCT_EDGES: [f32; 10] = [40.0, 80.0, 160.0, 320.0, 640.0, 1250.0, 2500.0, 5000.0, 10000.0, 16000.0];

pub fn frames(x: &[f32]) -> Frames {
    let nfft = 2048usize;
    let bins = nfft / 2 + 1;
    let n = x.len() / HOP + 1;
    let w = hann(nfft);
    let fft = Fft::new(nfft);
    let (mut re, mut im) = (vec![0.0f32; nfft], vec![0.0f32; nfft]);
    let hz = |k: usize| k as f32 * SR / nfft as f32;
    let oct_of: Vec<i32> = (0..bins).map(|k| OCT_EDGES.windows(2).position(|e| hz(k) >= e[0] && hz(k) < e[1]).map(|i| i as i32).unwrap_or(-1)).collect();
    let mut f = Frames { e_db: vec![0.0; n], cent: vec![0.0; n], high_db: vec![0.0; n], low_db: vec![0.0; n], oct: vec![[0.0; 9]; n], flux: vec![0.0; n] };
    let mut prev = vec![0.0f32; bins];
    let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6);
    let scale = 400.0 / (nfft as f32 * peak);
    for i in 0..n {
        for k in 0..nfft {
            let j = (i * HOP + k) as isize - (nfft / 2) as isize;
            re[k] = if j >= 0 && (j as usize) < x.len() { x[j as usize] * w[k] } else { 0.0 };
            im[k] = 0.0;
        }
        fft.forward(&mut re, &mut im);
        let (mut tot, mut cw, mut hi, mut lo, mut fl) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let mut oct = [0.0f32; 9];
        for k in 1..bins {
            let m2 = re[k] * re[k] + im[k] * im[k];
            let m = m2.sqrt();
            tot += m2;
            cw += m2 * hz(k);
            if hz(k) >= 5000.0 {
                hi += m2;
            }
            if (30.0..120.0).contains(&hz(k)) {
                lo += m2;
            }
            if oct_of[k] >= 0 {
                oct[oct_of[k] as usize] += m2;
            }
            let c = (1.0 + scale * m).ln();
            if hz(k) < 12000.0 {
                fl += (c - prev[k]).max(0.0);
            }
            prev[k] = c;
        }
        let nn = (nfft * nfft) as f32;
        f.e_db[i] = db(tot / nn);
        f.cent[i] = if tot > 1e-12 { (cw / tot).max(20.0).log2() } else { 0.0 };
        f.high_db[i] = db(hi / nn);
        f.low_db[i] = db(lo / nn);
        for b in 0..9 {
            oct[b] = db(oct[b] / nn);
        }
        f.oct[i] = oct;
        f.flux[i] = if i == 0 { 0.0 } else { fl };
    }
    f
}

fn smooth(v: &[f32], w: usize) -> Vec<f32> {
    let n = v.len();
    let mut pre = vec![0.0f64; n + 1];
    for i in 0..n {
        pre[i + 1] = pre[i] + v[i] as f64;
    }
    (0..n)
        .map(|i| {
            let a = i.saturating_sub(w);
            let b = (i + w + 1).min(n);
            ((pre[b] - pre[a]) / (b - a) as f64) as f32
        })
        .collect()
}

fn mean(v: &[f32]) -> f32 {
    if v.is_empty() { 0.0 } else { v.iter().sum::<f32>() / v.len() as f32 }
}

/// Linear fit of v against its index: (total change over the span, correlation r).
fn trend(v: &[f32]) -> (f32, f32) {
    let n = v.len();
    if n < 4 {
        return (0.0, 0.0);
    }
    let mx = (n - 1) as f32 / 2.0;
    let my = mean(v);
    let (mut sxy, mut sxx, mut syy) = (0.0f32, 0.0f32, 0.0f32);
    for (i, y) in v.iter().enumerate() {
        let dx = i as f32 - mx;
        let dy = y - my;
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    let slope = sxy / sxx.max(1e-9);
    (slope * (n - 1) as f32, sxy / (sxx * syy).sqrt().max(1e-9))
}

#[derive(Clone, Debug, Serialize)]
pub struct Fx {
    pub kind: String,
    pub t0: f32,
    pub t1: f32,
    pub detail: String,
}

/// Production moves with timestamps: risers / builds, reverse swells, stops
/// and breaks, filter sweeps, impacts on a downbeat.
pub fn detect_fx(x: &[f32], grid: Grid) -> Vec<Fx> {
    let fr = SR / HOP as f32;
    let f = frames(x);
    let n = f.e_db.len();
    let dur = x.len() as f32 / SR;
    let fi = |t: f32| ((t * fr).round().max(0.0) as usize).min(n.saturating_sub(1));
    let cent = smooth(&f.cent, (0.2 * fr) as usize);
    // highs never read below 60 dB under the full level (no dB artefacts from near-silent bands)
    let high_c: Vec<f32> = f.high_db.iter().zip(f.e_db.iter()).map(|(h, e)| h.max(e - 60.0)).collect();
    let high = smooth(&high_c, (0.2 * fr) as usize);
    let lvl = smooth(&f.e_db, (0.1 * fr) as usize);
    let mut out: Vec<Fx> = Vec::new();
    let nbars = grid.bars_in(dur);
    let bar_ok = |b: usize| grid.bar_t(b) >= 0.0 && grid.bar_t(b) <= dur;
    // stops and breaks: the level falls 15 dB under its +-4 s median for >= 0.8 beat
    let step = 16usize;
    let win = (4.0 * fr) as usize;
    let refm: Vec<f32> = (0..n / step + 2).map(|i| { let c = i * step; let a = c.saturating_sub(win); let b = (c + win).min(n); if a < b { pct(&lvl[a..b], 0.6) } else { 0.0 } }).collect();
    let mut i = fi(1.5);
    let end = fi(dur - 1.5);
    while i < end {
        let r = refm[(i + step / 2) / step];
        if lvl[i] < r - 15.0 {
            let mut j = i;
            while j < end && lvl[j] < refm[(j + step / 2) / step] - 12.0 {
                j += 1;
            }
            let len = (j - i) as f32 / fr;
            if len >= 0.8 * grid.beat_s() && j < end {
                let kind = if len >= grid.bar_s() * 0.9 { "break" } else { "stop" };
                out.push(Fx { kind: kind.into(), t0: i as f32 / fr, t1: j as f32 / fr, detail: format!("{:.0} dB down for {:.1} beats", r - mean(&lvl[i..j]), len / grid.beat_s()) });
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    // risers / builds ending on a bar line: every qualifying window, then the
    // strongest climbs first, without overlaps
    let mut cands: Vec<(f32, f32, f32, f32, usize)> = Vec::new();
    for b in 1..=nbars {
        if !bar_ok(b) {
            continue;
        }
        for l in [8usize, 4, 2, 1] {
            if l > b {
                continue;
            }
            let (t0, t1) = (grid.bar_t(b - l).max(0.0), grid.bar_t(b));
            if t1 - t0 < 1.0 {
                continue;
            }
            let (a, z) = (fi(t0), fi(t1));
            if z <= a + 8 {
                continue;
            }
            let (dc, rc) = trend(&cent[a..z]);
            let k = ((z - a) as f32 * 0.15) as usize + 1;
            let hrise = mean(&high[z - k..z]) - mean(&high[a..a + k]);
            let audible = mean(&high[z - k..z]) >= mean(&lvl[z - k..z]) - 30.0;
            if dc >= 0.6 && rc >= 0.7 && hrise >= 6.0 && audible {
                cands.push((t0, t1, dc, hrise, l));
            }
        }
    }
    cands.sort_by(|a, b| (b.3 + 3.0 * b.2).partial_cmp(&(a.3 + 3.0 * a.2)).unwrap_or(std::cmp::Ordering::Equal));
    let mut risers: Vec<(f32, f32)> = Vec::new();
    for (t0, t1, dc, hrise, l) in cands {
        if risers.iter().any(|r| t0 < r.1 - 0.01 && t1 > r.0 + 0.01) {
            continue;
        }
        risers.push((t0, t1));
        out.push(Fx { kind: "riser".into(), t0, t1, detail: format!("brightness up {:.1} octaves, highs +{:.0} dB over {l} bar(s)", dc, hrise) });
    }
    // reverse swells: a smooth 10+ dB climb in the highs that peaks on a bar or half-bar line
    let hs = smooth(&high_c, (0.03 * fr) as usize);
    let flux97 = pct(&f.flux, 0.97);
    for hb in 1..nbars * 2 {
        let tl = grid.downbeat_s + hb as f32 * grid.bar_s() / 2.0;
        if tl < 1.6 || tl > dur - 0.2 {
            continue;
        }
        let (pa, pz) = (fi(tl - 0.15), fi(tl + 0.05));
        let (pk_i, pk) = (pa..=pz).map(|i| (i, hs[i])).fold((pa, f32::MIN), |m, c| if c.1 > m.1 { c } else { m });
        let (ma, mz) = (fi(tl - 1.5), pk_i.saturating_sub((0.4 * fr) as usize));
        if mz <= ma {
            continue;
        }
        let (mn_i, mn) = (ma..=mz).map(|i| (i, hs[i])).fold((ma, f32::MAX), |m, c| if c.1 < m.1 { c } else { m });
        if pk - mn < 10.0 || pk_i <= mn_i + 4 || pk < lvl[pk_i] - 25.0 {
            continue;
        }
        let ups = (mn_i + 1..=pk_i).filter(|&i| hs[i] >= hs[i - 1] - 0.05).count() as f32 / (pk_i - mn_i) as f32;
        let after = hs[(pk_i + (0.12 * fr) as usize).min(n - 1)];
        let onsets_in = (mn_i..pk_i).filter(|&i| f.flux[i] > flux97).count();
        if ups >= 0.75 && pk - after >= 4.0 && onsets_in <= 1 && !out.iter().any(|o| o.kind == "reverse_swell" && (o.t1 - tl).abs() < 0.5) {
            out.push(Fx { kind: "reverse_swell".into(), t0: mn_i as f32 / fr, t1: pk_i as f32 / fr, detail: format!("highs +{:.0} dB, cut {:.0} dB after", pk - mn, pk - after) });
        }
    }
    // filter sweeps: brightness moves an octave or more while the level holds
    let mut sweep_until = 0.0f32;
    for b in 0..nbars {
        for l in [8usize, 4, 2] {
            if b + l > nbars {
                continue;
            }
            let (t0, t1) = (grid.bar_t(b).max(0.0), grid.bar_t(b + l).min(dur));
            if t0 < sweep_until || t1 - t0 < 1.0 || out.iter().any(|o| o.kind == "riser" && o.t0 < t1 && o.t1 > t0) {
                continue;
            }
            let (a, z) = (fi(t0), fi(t1));
            if z <= a + 8 {
                continue;
            }
            let (dc, rc) = trend(&cent[a..z]);
            let (de, _) = trend(&lvl[a..z]);
            if dc.abs() >= 1.0 && rc.abs() >= 0.85 && de.abs() <= 5.0 {
                out.push(Fx { kind: if dc > 0.0 { "filter_sweep_up".into() } else { "filter_sweep_down".into() }, t0, t1, detail: format!("brightness {:+.1} octaves over {l} bars, level {:+.0} dB", dc, de) });
                sweep_until = t1;
                break;
            }
        }
    }
    // impacts: the low end jumps 9 dB on a downbeat against the whole bar before (power means)
    let pmean = |v: &[f32], a: usize, z: usize| -> f32 { if z <= a { -120.0 } else { db(v[a..z].iter().map(|d| 10f32.powf(d / 10.0)).sum::<f32>() / (z - a) as f32) } };
    for b in 1..nbars {
        let t = grid.bar_t(b);
        if t < grid.bar_s() || t > dur - 0.5 {
            continue;
        }
        let (a0, a1, z1) = (fi(t - grid.bar_s()), fi(t - 0.03), fi(t + 0.3));
        let (s0, _) = (fi(t), 0);
        let low_jump = pmean(&f.low_db, s0, z1) - pmean(&f.low_db, a0, a1);
        let lvl_jump = pmean(&f.e_db, s0, z1) - pmean(&f.e_db, a0, a1);
        if low_jump >= 9.0 && lvl_jump >= 6.0 {
            out.push(Fx { kind: "impact".into(), t0: t, t1: t + 0.3, detail: format!("low end +{:.0} dB, level +{:.0} dB over the bar before", low_jump, lvl_jump) });
        }
    }
    out.sort_by(|a, b| a.t0.partial_cmp(&b.t0).unwrap_or(std::cmp::Ordering::Equal));
    out
}

// ---------------------------------------------------------------- arrangement

#[derive(Clone, Debug, Serialize)]
pub struct Section {
    pub label: char,
    pub kind: String,
    /// 0-based first bar and bar count
    pub bar0: usize,
    pub bars: usize,
    pub energy_db: f32,
    /// mean activity level 0-9 per part
    pub parts: Vec<(String, u8)>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Arrangement {
    pub grid: Grid,
    pub bars: usize,
    pub parts: Vec<String>,
    /// per part, per bar: activity level 0-9
    pub levels: Vec<Vec<u8>>,
    pub sections: Vec<Section>,
    /// (bar, part, "enters" | "exits")
    pub events: Vec<(usize, String, String)>,
}

fn bar_db(x: &[f32], grid: &Grid, bars: usize) -> Vec<f32> {
    (0..bars)
        .map(|b| {
            let a = ((grid.bar_t(b).max(0.0)) * SR) as usize;
            let z = ((grid.bar_t(b + 1).max(0.0) * SR) as usize).min(x.len());
            if z <= a { -120.0 } else { db(x[a..z].iter().map(|v| v * v).sum::<f32>() / (z - a) as f32) }
        })
        .collect()
}

/// Sections and part activity per bar. `parts` are (name, mono audio) — the
/// stems; with none, the mix's low / mid / high bands stand in.
pub fn arrangement(mix: &[f32], parts: &[(String, Vec<f32>)], grid: Grid, min_bars: usize) -> Arrangement {
    let dur = mix.len() as f32 / SR;
    let bars = grid.bars_in(dur);
    let mut names: Vec<String> = Vec::new();
    let mut lv_db: Vec<Vec<f32>> = Vec::new();
    if parts.is_empty() {
        let f = frames(mix);
        let fr = SR / HOP as f32;
        for (name, bands) in [("low", 0..2usize), ("mid", 2..6), ("high", 6..9)] {
            names.push(name.into());
            lv_db.push((0..bars).map(|b| {
                let a = ((grid.bar_t(b).max(0.0) * fr) as usize).min(f.oct.len());
                let z = ((grid.bar_t(b + 1).max(0.0) * fr) as usize).min(f.oct.len());
                if z <= a { return -120.0; }
                let s: f32 = (a..z).map(|i| bands.clone().map(|k| 10f32.powf(f.oct[i][k] / 10.0)).sum::<f32>()).sum();
                db(s / (z - a) as f32)
            }).collect());
        }
    } else {
        for (n, x) in parts {
            names.push(n.clone());
            lv_db.push(bar_db(x, &grid, bars));
        }
    }
    let levels: Vec<Vec<u8>> = lv_db.iter().map(|v| {
        let p95 = pct(v, 0.95);
        v.iter().map(|d| (((d - (p95 - 27.0)) / 3.0).round().clamp(0.0, 9.0)) as u8).collect()
    }).collect();
    let active = |p: usize, b: usize| levels[p][b] >= 3;
    let mix_db = bar_db(mix, &grid, bars);
    // chroma per bar
    let (ch, _, _) = tr::beat_chroma(mix, grid);
    let bpb = grid.beats_per_bar;
    let bar_ch: Vec<[f32; 12]> = (0..bars).map(|b| {
        let mut c = [0.0f32; 12];
        for k in b * bpb..((b + 1) * bpb).min(ch.len()) {
            for i in 0..12 { c[i] += ch[k][i]; }
        }
        let mn = c.iter().cloned().fold(f32::MAX, f32::min);
        let nn = c.iter().map(|v| (v - mn) * (v - mn)).sum::<f32>().sqrt().max(1e-9);
        let mut o = [0.0f32; 12];
        for i in 0..12 { o[i] = (c[i] - mn) / nn; }
        o
    }).collect();
    let mtop = pct(&mix_db, 0.95);
    let feat: Vec<Vec<f32>> = (0..bars).map(|b| {
        let mut v: Vec<f32> = bar_ch[b].to_vec();
        for p in 0..names.len() { v.push(levels[p][b] as f32 / 4.5); }
        v.push(((mix_db[b] - mtop) / 6.0).max(-3.0));
        v
    }).collect();
    // boundary novelty: distance between the 4 bars before and after each bar line
    let w = 4usize.min(bars / 2).max(1);
    let mean_feat = |a: usize, z: usize| -> Vec<f32> {
        let d = feat[0].len();
        let mut m = vec![0.0f32; d];
        for f in &feat[a..z] { for i in 0..d { m[i] += f[i]; } }
        m.iter().map(|v| v / (z - a).max(1) as f32).collect()
    };
    let mut nov = vec![0.0f32; bars + 1];
    for b in 1..bars {
        let a = b.saturating_sub(w);
        let z = (b + w).min(bars);
        let (x1, x2) = (mean_feat(a, b), mean_feat(b, z));
        nov[b] = x1.iter().zip(x2.iter()).map(|(p, q)| (p - q) * (p - q)).sum::<f32>().sqrt();
    }
    let thr = pct(&nov[1..bars.max(2)], 0.5) + 0.5 * (pct(&nov[1..bars.max(2)], 0.9) - pct(&nov[1..bars.max(2)], 0.5));
    let mut order: Vec<usize> = (1..bars).filter(|&b| nov[b] > thr && nov[b] >= nov[b - 1] && nov[b] >= nov[b + 1]).collect();
    order.sort_by(|a, b| nov[*b].partial_cmp(&nov[*a]).unwrap_or(std::cmp::Ordering::Equal));
    let mut cuts: Vec<usize> = vec![0, bars];
    for b in order {
        if cuts.iter().all(|c| (*c as isize - b as isize).unsigned_abs() >= min_bars) {
            cuts.push(b);
        }
    }
    cuts.sort_unstable();
    // label: same chords, same energy (3 dB), same parts playing -> same letter
    let mut reps: Vec<([f32; 12], f32, Vec<bool>)> = Vec::new();
    let mut sections = Vec::new();
    for wdw in cuts.windows(2) {
        let (a, z) = (wdw[0], wdw[1]);
        let mut c = [0.0f32; 12];
        for b in a..z { for i in 0..12 { c[i] += bar_ch[b][i]; } }
        let nn = c.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-9);
        c.iter_mut().for_each(|v| *v /= nn);
        let e = 10.0 * (mix_db[a..z].iter().map(|d| 10f32.powf(d / 10.0)).sum::<f32>() / (z - a) as f32).max(1e-12).log10();
        let act: Vec<bool> = (0..names.len()).map(|p| (a..z).filter(|&b| active(p, b)).count() * 2 > z - a).collect();
        let found = reps.iter().position(|(rc, re, ra)| (0..12).map(|i| rc[i] * c[i]).sum::<f32>() >= 0.93 && (re - e).abs() <= 3.0 && *ra == act);
        let li = match found { Some(i) => i, None => { reps.push((c, e, act.clone())); reps.len() - 1 } };
        let parts_lv: Vec<(String, u8)> = names.iter().enumerate().map(|(p, n)| (n.clone(), ((a..z).map(|b| levels[p][b] as f32).sum::<f32>() / (z - a) as f32).round() as u8)).collect();
        sections.push(Section { label: if li < 26 { (b'A' + li as u8) as char } else { '?' }, kind: String::new(), bar0: a, bars: z - a, energy_db: r1(e), parts: parts_lv });
    }
    // kinds from repetition and energy
    let med_e = pct(&sections.iter().map(|s| s.energy_db).collect::<Vec<_>>(), 0.5);
    let count = |l: char, s: &[Section]| s.iter().filter(|x| x.label == l).count();
    let mut rep_letters: Vec<(char, f32)> = Vec::new();
    for s in &sections {
        if count(s.label, &sections) >= 2 && !rep_letters.iter().any(|r| r.0 == s.label) {
            rep_letters.push((s.label, s.energy_db));
        }
    }
    rep_letters.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let chorus = rep_letters.first().map(|r| r.0);
    let verse = rep_letters.get(1).map(|r| r.0);
    let ns = sections.len();
    for i in 0..ns {
        let l = sections[i].label;
        let e = sections[i].energy_db;
        let reps_n = count(l, &sections);
        let kind = if Some(l) == chorus {
            "chorus"
        } else if Some(l) == verse {
            "verse"
        } else if i == 0 && reps_n == 1 && e < med_e - 2.0 {
            "intro"
        } else if i == ns - 1 && reps_n == 1 && e < med_e - 2.0 {
            "outro"
        } else if reps_n == 1 && e < med_e - 5.0 {
            "break"
        } else if reps_n == 1 {
            "bridge"
        } else {
            "section"
        };
        sections[i].kind = kind.to_string();
    }
    // entries and exits: a part changes state and holds it for 2+ bars
    let mut events = Vec::new();
    for (p, n) in names.iter().enumerate() {
        for b in 1..bars {
            let now = active(p, b);
            if now != active(p, b - 1) && (b..(b + 2).min(bars)).all(|k| active(p, k) == now) && (b.saturating_sub(2)..b).all(|k| active(p, k) != now) {
                events.push((b, n.clone(), if now { "enters".to_string() } else { "exits".to_string() }));
            }
        }
    }
    events.sort_by_key(|e| e.0);
    Arrangement { grid, bars, parts: names, levels, sections, events }
}

// ---------------------------------------------------------------- A/B scoring

#[derive(Clone, Debug, Serialize)]
pub struct Score {
    pub level_db: f32,
    pub tone_db: f32,
    pub groove_pct: f32,
    pub harmony_pct: f32,
    pub overall: f32,
    /// Ours minus reference, per octave (OCT_EDGES), each relative to its own total level.
    pub bands_db: [f32; 9],
}

/// Onset times (s) of a mono signal, the same detector score_sections uses for groove.
pub fn onsets_of(x: &[f32]) -> Vec<f32> {
    onset_times(&frames(x))
}

fn onset_times(f: &Frames) -> Vec<f32> {
    let fr = SR / HOP as f32;
    tr::pick_peaks(&f.flux, 0.6, 3).into_iter().filter(|p| p.1 > 0.2).map(|p| p.0 as f32 / fr).collect()
}

/// F-measure of two onset lists within `tol` seconds.
pub fn onset_f(a: &[f32], b: &[f32], tol: f32) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut used = vec![false; b.len()];
    let mut hit = 0usize;
    for t in a {
        let i = b.partition_point(|x| *x < t - tol);
        let mut j = i;
        while j < b.len() && b[j] <= t + tol {
            if !used[j] {
                used[j] = true;
                hit += 1;
                break;
            }
            j += 1;
        }
    }
    let p = hit as f32 / b.len() as f32;
    let r = hit as f32 / a.len() as f32;
    if p + r == 0.0 { 0.0 } else { 2.0 * p * r / (p + r) }
}

fn chroma_windows(x: &[f32], t0: f32, t1: f32) -> Vec<[f32; 12]> {
    // half-second chroma windows from the shared chroma front end (a 120 bpm grid = beats of 0.5 s)
    let g = Grid { bpm: 120.0, downbeat_s: 0.0, beats_per_bar: 4 };
    let (ch, _, _) = tr::beat_chroma(x, g);
    let a = (t0 / 0.5).floor().max(0.0) as usize;
    let z = ((t1 / 0.5).ceil() as usize).min(ch.len());
    if a >= z { Vec::new() } else { ch[a..z].to_vec() }
}

fn cos12(a: &[f32; 12], b: &[f32; 12]) -> f32 {
    let ma = a.iter().cloned().fold(f32::MAX, f32::min);
    let mb = b.iter().cloned().fold(f32::MAX, f32::min);
    let (mut s, mut na, mut nb) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let (x, y) = (a[i] - ma, b[i] - mb);
        s += x * y;
        na += x * x;
        nb += y * y;
    }
    s / (na * nb).sqrt().max(1e-9)
}

/// Compare a span of `ours` (t0..t1) with the same span of `refr` shifted by
/// `offset` (refr time = ours time + offset).
pub fn score_span(ours: &(Vec<f32>, Frames, Vec<f32>), refr: &(Vec<f32>, Frames, Vec<f32>), t0: f32, t1: f32, offset: f32, ch_o: &[[f32; 12]], ch_r: &[[f32; 12]]) -> Score {
    let fr = SR / HOP as f32;
    let span = |f: &Frames, a: f32, z: f32| -> (usize, usize) {
        let n = f.e_db.len();
        (((a * fr).max(0.0) as usize).min(n), ((z * fr).max(0.0) as usize).min(n))
    };
    let (oa, oz) = span(&ours.1, t0, t1);
    let (ra, rz) = span(&refr.1, t0 + offset, t1 + offset);
    let lin_mean = |v: &[f32]| db(v.iter().map(|d| 10f32.powf(d / 10.0)).sum::<f32>() / v.len().max(1) as f32);
    let lo = lin_mean(&ours.1.e_db[oa..oz.max(oa + 1).min(ours.1.e_db.len())]);
    let lr = lin_mean(&refr.1.e_db[ra..rz.max(ra + 1).min(refr.1.e_db.len())]);
    // tonal balance: octave levels relative to each one's total
    let prof = |f: &Frames, a: usize, z: usize, tot: f32| -> [f32; 9] {
        let mut p = [0.0f32; 9];
        for k in 0..9 {
            let v: Vec<f32> = (a..z.min(f.oct.len())).map(|i| f.oct[i][k]).collect();
            p[k] = lin_mean(&v) - tot;
        }
        p
    };
    let po = prof(&ours.1, oa, oz, lo);
    let pr = prof(&refr.1, ra, rz, lr);
    let tone = ((0..9).map(|k| (po[k] - pr[k]).clamp(-30.0, 30.0).powi(2)).sum::<f32>() / 9.0).sqrt();
    let on_o: Vec<f32> = ours.2.iter().filter(|t| **t >= t0 && **t < t1).map(|t| t - t0).collect();
    let on_r: Vec<f32> = refr.2.iter().filter(|t| **t >= t0 + offset && **t < t1 + offset).map(|t| t - t0 - offset).collect();
    let groove = onset_f(&on_r, &on_o, 0.05);
    let n = ch_o.len().min(ch_r.len());
    let harmony = if n == 0 { 0.0 } else { (0..n).map(|i| cos12(&ch_o[i], &ch_r[i]).max(0.0)).sum::<f32>() / n as f32 };
    let tone_score = (100.0 - 10.0 * tone).clamp(0.0, 100.0);
    let level_score = (100.0 - 8.0 * (lo - lr).abs()).clamp(0.0, 100.0);
    let overall = 0.3 * tone_score + 0.3 * groove * 100.0 + 0.3 * harmony * 100.0 + 0.1 * level_score;
    Score { level_db: r1(lo - lr), tone_db: r1(tone), groove_pct: (groove * 100.0).round(), harmony_pct: (harmony * 100.0).round(), overall: overall.round(), bands_db: std::array::from_fn(|k| r1((po[k] - pr[k]).clamp(-30.0, 30.0))) }
}

/// Best offset (s) of refr against ours within +-max_s, from onset-envelope correlation.
pub fn align_offset(o: &Frames, r: &Frames, max_s: f32) -> f32 {
    let fr = SR / HOP as f32;
    let m = (max_s * fr) as isize;
    let (a, b) = (&o.flux, &r.flux);
    let ma = mean(a);
    let mb = mean(b);
    let mut best = (0isize, f32::MIN);
    for lag in -m..=m {
        let mut s = 0.0f32;
        for i in 0..a.len() {
            let j = i as isize + lag;
            if j >= 0 && (j as usize) < b.len() {
                s += (a[i] - ma) * (b[j as usize] - mb);
            }
        }
        if s > best.1 {
            best = (lag, s);
        }
    }
    best.0 as f32 / fr
}

// ---------------------------------------------------------------- tools

fn load_mono(e: &Engine, p: &str) -> Result<(PathBuf, Vec<f32>)> {
    let path = e.resolve(p);
    if !path.exists() {
        bail!("no file at {}", path.display());
    }
    let x = crate::samples::decode_file(&path)?;
    tr::check_audio(&x, 2.0)?;
    Ok((path, x))
}

fn out_file(e: &Engine, a: &Value, path: &Path, kind: &str) -> Result<PathBuf> {
    let dir = match s_opt(a, "out_dir") {
        Some(d) => e.resolve(&d),
        None => e.renders_dir().join("transcriptions"),
    };
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{}_{kind}.json", path.file_stem().and_then(|s| s.to_str()).unwrap_or("audio"))))
}

fn grid_json(g: &Grid, src: &str) -> Value {
    json!({"bpm": r2(g.bpm), "downbeat_s": (g.downbeat_s * 1000.0).round() / 1000.0, "beats_per_bar": g.beats_per_bar, "from": src})
}

fn detect_fx_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let (path, x) = load_mono(e, &s_req(a, "path")?)?;
    let (grid, src) = crate::tools_transcribe::grid_for(e, a, &path, &x)?;
    let fx = detect_fx(&x, grid);
    let mut counts = Map::new();
    for f in &fx {
        let c = counts.entry(f.kind.clone()).or_insert(json!(0));
        *c = json!(c.as_u64().unwrap_or(0) + 1);
    }
    let list: Vec<Value> = fx.iter().map(|f| json!({"kind": f.kind, "at": grid.label(f.t0), "until": grid.label(f.t1), "t0": r2(f.t0), "t1": r2(f.t1), "detail": f.detail})).collect();
    let out = out_file(e, a, &path, "fx")?;
    std::fs::write(&out, serde_json::to_string_pretty(&json!({"grid": grid_json(&grid, &src), "events": list}))?)?;
    Ok(json!({"grid": grid_json(&grid, &src), "count": fx.len(), "by_kind": counts, "events": list.iter().take(40).cloned().collect::<Vec<_>>(),
        "legend": "at/until = bar.beat; riser = brightness and highs climbing into a bar line; reverse_swell = a smooth climb cut on the beat; stop/break = the music drops out; filter_sweep = brightness moves an octave while the level holds; impact = a low-end hit on a downbeat",
        "detail_file": out.to_string_lossy(), "next": ["arrangement_map", "place_fx", "add_transition"]}))
}

const STEM_FILES: [&str; 4] = ["drums", "bass", "vocals", "other"];

fn read_stems(e: &Engine, dir: &str) -> Result<Vec<(String, Vec<f32>)>> {
    let d = e.resolve(dir);
    let mut out = Vec::new();
    for n in STEM_FILES {
        for ext in ["wav", "flac", "mp3"] {
            let p = d.join(format!("{n}.{ext}"));
            if p.exists() {
                out.push((n.to_string(), crate::samples::decode_file(&p)?));
                break;
            }
        }
    }
    if out.is_empty() {
        bail!("no drums/bass/vocals/other files in {}", d.display());
    }
    Ok(out)
}

/// Group a part name into one of the reference stem names (drums, bass, vocals, other).
pub fn stem_group(name: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    let has = |ws: &[&str]| ws.iter().any(|w| n.contains(w));
    if has(&["vox", "vocal", "voice", "sing", "adlib", "choir"]) {
        "vocals"
    } else if has(&["drum", "kick", "snare", "hat", "perc", "clap", "cymbal", "tom", "ride", "crash", "shaker", "beat"]) {
        "drums"
    } else if has(&["bass", "808", "sub"]) {
        "bass"
    } else {
        "other"
    }
}

/// Read our own per-track stems (any names) and sum them into reference stem groups.
/// `map` overrides the automatic grouping for named files ({"gtr":"other","vox":"vocals"}).
fn read_ours_stems(e: &Engine, dir: &str, map: Option<&serde_json::Map<String, Value>>) -> Result<Vec<(String, Vec<f32>)>> {
    let d = e.resolve(dir);
    let mut groups: Vec<(String, Vec<f32>)> = Vec::new();
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&d)?.filter_map(|x| x.ok().map(|x| x.path())).collect();
    files.sort();
    for p in files {
        let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase();
        if !["wav", "flac", "mp3"].contains(&ext.as_str()) {
            continue;
        }
        let stem = p.file_stem().and_then(|x| x.to_str()).unwrap_or("").to_string();
        let g = map.and_then(|m| m.get(&stem)).and_then(|v| v.as_str()).map(|v| v.to_string()).unwrap_or_else(|| stem_group(&stem).to_string());
        if !STEM_FILES.contains(&g.as_str()) {
            bail!("stem_map sends '{stem}' to '{g}'; use one of {:?}", STEM_FILES);
        }
        let x = crate::samples::decode_file(&p)?;
        match groups.iter_mut().find(|(k, _)| *k == g) {
            Some((_, acc)) => {
                if acc.len() < x.len() { acc.resize(x.len(), 0.0); }
                for (i, v) in x.iter().enumerate() { acc[i] += v; }
            }
            None => groups.push((g, x)),
        }
    }
    if groups.is_empty() {
        bail!("no audio files in {}", d.display());
    }
    Ok(groups)
}

fn arrangement_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let (path, x) = load_mono(e, &s_req(a, "path")?)?;
    if x.len() < (SR * 10.0) as usize {
        bail!("needs at least 10 s of music");
    }
    let (grid, src) = crate::tools_transcribe::grid_for(e, a, &path, &x)?;
    let stems = match s_opt(a, "stems_dir") {
        Some(d) => read_stems(e, &d)?,
        None => Vec::new(),
    };
    let min_bars = crate::tools::u_or(a, "min_section_bars", 4).clamp(1, 32) as usize;
    let ar = arrangement(&x, &stems, grid, min_bars);
    let secs: Vec<Value> = ar.sections.iter().map(|s| {
        let parts: Map<String, Value> = s.parts.iter().map(|(n, l)| (n.clone(), json!(l))).collect();
        json!({"label": s.label.to_string(), "kind": s.kind, "bars": format!("{}-{}", s.bar0 + 1, s.bar0 + s.bars), "t0": r2(grid.bar_t(s.bar0).max(0.0)), "t1": r2(grid.bar_t(s.bar0 + s.bars)), "energy_db": s.energy_db, "parts": parts})
    }).collect();
    let form = ar.sections.iter().map(|s| format!("{}({})", s.kind, s.bars)).collect::<Vec<_>>().join(" ");
    let activity: Map<String, Value> = ar.parts.iter().enumerate().map(|(p, n)| (n.clone(), json!(ar.levels[p].chunks(4).map(|c| c.iter().map(|v| char::from(b'0' + v)).collect::<String>()).collect::<Vec<_>>().join(" ")))).collect();
    let events: Vec<String> = ar.events.iter().map(|(b, p, w)| format!("bar {}: {p} {w}", b + 1)).collect();
    let out = out_file(e, a, &path, "arrangement")?;
    std::fs::write(&out, serde_json::to_string_pretty(&json!({"grid": grid_json(&grid, &src), "sections": secs, "activity": activity, "events": events}))?)?;
    Ok(json!({"grid": grid_json(&grid, &src), "bars": ar.bars, "form": form, "sections": secs, "activity": activity, "events": events.iter().take(40).cloned().collect::<Vec<_>>(),
        "legend": "activity: one digit per bar (0 silent .. 9 loudest for that part), 4 bars per group; same letter = same chords, energy and parts",
        "detail_file": out.to_string_lossy(), "next": ["set_arrangement", "build_structure", "score_sections"]}))
}

fn score_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let (po, xo) = load_mono(e, &s_req(a, "ours")?)?;
    let (pr, xr) = load_mono(e, &s_req(a, "reference")?)?;
    let fo = frames(&xo);
    let fr_ = frames(&xr);
    let offset = match f_opt(a, "offset_s") {
        Some(v) => v,
        None => align_offset(&fo, &fr_, crate::tools::f_or(a, "max_offset_s", 8.0).clamp(0.0, 60.0)),
    };
    let on_o = onset_times(&fo);
    let on_r = onset_times(&fr_);
    let ours = (Vec::new(), fo, on_o);
    let refr = (Vec::new(), fr_, on_r);
    let dur_o = xo.len() as f32 / SR;
    // sections: given (t0/t1/name), or from arrangement_map's file, or 8-bar blocks of ours' grid
    let mut spans: Vec<(String, f32, f32)> = Vec::new();
    if let Some(arr) = a.get("sections").and_then(|v| v.as_array()) {
        for (i, s) in arr.iter().enumerate() {
            let t0 = s.get("t0").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            let t1 = s.get("t1").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            if t1 > t0 {
                spans.push((s.get("name").and_then(|v| v.as_str()).map(String::from).unwrap_or(format!("section {}", i + 1)), t0, t1));
            }
        }
    } else if let Some(f) = s_opt(a, "sections_file") {
        // a sections file (arrangement_map's, or bars only) is on the reference's clock: times come
        // from its t0/t1, else from its bars on its own grid or the reference's
        let v: Value = serde_json::from_str(&std::fs::read_to_string(e.resolve(&f))?)?;
        let grid = match (v["grid"]["bpm"].as_f64(), v["grid"]["downbeat_s"].as_f64()) {
            (Some(bpm), Some(db)) => crate::transcribe::Grid::new(bpm as f32, db as f32, v["grid"]["beats_per_bar"].as_u64().unwrap_or(4) as usize),
            _ => crate::tools_transcribe::grid_for(e, a, &pr, &xr)?.0,
        };
        for s in v["sections"].as_array().cloned().unwrap_or_default() {
            let bars = s["bars"].as_str().unwrap_or("");
            let from_bars = bars.split_once('-').and_then(|(x, y)| Some((x.trim().parse::<usize>().ok()?, y.trim().parse::<usize>().ok()?)));
            let (t0, t1) = match (s["t0"].as_f64(), s["t1"].as_f64(), from_bars) {
                (Some(x), Some(y), _) if y > x => (x as f32, y as f32),
                (_, _, Some((b0, b1))) if b1 >= b0 && b0 >= 1 => (grid.bar_t(b0 - 1), grid.bar_t(b1)),
                _ => continue,
            };
            spans.push((format!("{} {}", s["kind"].as_str().unwrap_or(""), bars).trim().to_string(), (t0 - offset).max(0.0), (t1 - offset).min(dur_o)));
        }
    } else {
        let (grid, _) = crate::tools_transcribe::grid_for(e, a, &po, &xo)?;
        let bars = grid.bars_in(dur_o);
        let mut b = 0;
        while b < bars {
            let z = (b + 8).min(bars);
            spans.push((format!("bars {}-{}", b + 1, z), grid.bar_t(b).max(0.0), grid.bar_t(z).min(dur_o)));
            b = z;
        }
    }
    if spans.is_empty() {
        bail!("no sections to score");
    }
    let mut rows = Vec::new();
    let mut tot = 0.0;
    let ch_o_all = chroma_windows(&xo, 0.0, dur_o);
    let ch_r_all = chroma_windows(&xr, 0.0, xr.len() as f32 / SR);
    let cut = |c: &[[f32; 12]], t0: f32, t1: f32| -> Vec<[f32; 12]> {
        let a = (t0 / 0.5).floor().max(0.0) as usize;
        let z = ((t1 / 0.5).ceil().max(0.0) as usize).min(c.len());
        if a >= z { Vec::new() } else { c[a..z].to_vec() }
    };
    for (name, t0, t1) in &spans {
        let s = score_span(&ours, &refr, *t0, *t1, offset, &cut(&ch_o_all, *t0, *t1), &cut(&ch_r_all, t0 + offset, t1 + offset));
        tot += s.overall * (t1 - t0);
        rows.push(json!({"section": name, "t0": r2(*t0), "t1": r2(*t1), "overall": s.overall, "groove_pct": s.groove_pct, "harmony_pct": s.harmony_pct, "tone_diff_db": s.tone_db, "tone_bands_db": s.bands_db.to_vec(), "level_diff_db": s.level_db}));
    }
    let total_t: f32 = spans.iter().map(|s| s.2 - s.1).sum();
    // per part: the same scores on matching stems, plus each part's share of the whole
    let mut parts = Vec::new();
    if let (Some(od), Some(rd)) = (s_opt(a, "ours_stems"), s_opt(a, "reference_stems")) {
        let so = read_ours_stems(e, &od, a.get("stem_map").and_then(|v| v.as_object()))?;
        let sr = read_stems(e, &rd)?;
        let share = |v: &[(String, Vec<f32>)], n: &str| -> f32 {
            let tot: f32 = v.iter().map(|(_, x)| x.iter().map(|s| s * s).sum::<f32>()).sum::<f32>().max(1e-12);
            v.iter().find(|(k, _)| k == n).map(|(_, x)| db(x.iter().map(|s| s * s).sum::<f32>() / tot)).unwrap_or(-120.0)
        };
        // share of each octave's energy held by each stem (ours vs reference): shows which part owns a band
        let band_energy = |v: &[(String, Vec<f32>)]| -> Vec<(String, [f32; 9])> {
            v.iter().map(|(k, x)| {
                let f = frames(x);
                let mut e = [0.0f32; 9];
                for fr in &f.oct { for b in 0..9 { e[b] += 10f32.powf(fr[b] / 10.0); } }
                (k.clone(), e)
            }).collect()
        };
        let share_pct = |v: &[(String, [f32; 9])], n: &str| -> [f32; 9] {
            let mut out = [0.0f32; 9];
            if let Some((_, e)) = v.iter().find(|(k, _)| k == n) {
                for b in 0..9 {
                    let t: f32 = v.iter().map(|(_, x)| x[b]).sum::<f32>().max(1e-12);
                    out[b] = (100.0 * e[b] / t).round();
                }
            }
            out
        };
        let (beo, ber) = (band_energy(&so), band_energy(&sr));
        for (n, x) in &so {
            if let Some((_, y)) = sr.iter().find(|(k, _)| k == n) {
                let fo2 = frames(x);
                let fr2 = frames(y);
                let (oo, rr) = (onset_times(&fo2), onset_times(&fr2));
                let o2 = (Vec::new(), fo2, oo);
                let r2_ = (Vec::new(), fr2, rr);
                let d = (x.len() as f32 / SR).min(y.len() as f32 / SR - offset.max(0.0));
                let co = chroma_windows(x, 0.0, d);
                let cr = chroma_windows(y, offset.max(0.0), d + offset.max(0.0));
                let s = score_span(&o2, &r2_, 0.0, d.max(1.0), offset, &co, &cr);
                parts.push(json!({"part": n, "overall": s.overall, "groove_pct": s.groove_pct, "harmony_pct": s.harmony_pct, "tone_diff_db": s.tone_db,
                    "share_diff_db": r1(share(&so, n) - share(&sr, n)), "tone_bands_db": s.bands_db.to_vec(),
                    "band_share_pct": share_pct(&beo, n).to_vec(), "ref_band_share_pct": share_pct(&ber, n).to_vec()}));
            }
        }
    }
    // A/B listening clips: reference span, 1 s gap, ours, level-matched (listening aids only)
    let mut clips = Vec::new();
    if b_or(a, "clips", false) {
        let dir = match s_opt(a, "out_dir") { Some(d) => e.resolve(&d), None => e.renders_dir().join("ab") };
        std::fs::create_dir_all(&dir)?;
        let (ol, or_) = crate::samples::decode_stereo(&po)?;
        let (rl, rr) = crate::samples::decode_stereo(&pr)?;
        let max_s = crate::tools::f_or(a, "clip_seconds", 15.0).clamp(3.0, 30.0);
        for (i, (name, t0, t1)) in spans.iter().enumerate() {
            let len = (t1 - t0).min(max_s);
            let seg = |l: &[f32], r: &[f32], t: f32| -> (Vec<f32>, Vec<f32>) {
                let a = ((t.max(0.0)) * SR) as usize;
                let z = (((t + len).max(0.0)) * SR) as usize;
                let z = z.min(l.len());
                if a >= z { (Vec::new(), Vec::new()) } else { (l[a..z].to_vec(), r[a..z].to_vec()) }
            };
            let (mut al, mut ar_) = seg(&rl, &rr, t0 + offset);
            let (bl, br) = seg(&ol, &or_, *t0);
            if al.is_empty() || bl.is_empty() {
                continue;
            }
            let rms = |l: &[f32], r: &[f32]| ((l.iter().chain(r.iter()).map(|v| v * v).sum::<f32>()) / (2 * l.len()).max(1) as f32).sqrt().max(1e-9);
            let g = rms(&bl, &br) / rms(&al, &ar_);
            al.iter_mut().for_each(|v| *v *= g);
            ar_.iter_mut().for_each(|v| *v *= g);
            let gap = vec![0.0f32; SR as usize];
            let l: Vec<f32> = al.iter().chain(gap.iter()).chain(bl.iter()).cloned().collect();
            let r: Vec<f32> = ar_.iter().chain(gap.iter()).chain(br.iter()).cloned().collect();
            let p = dir.join(format!("ab_{:02}.wav", i + 1));
            crate::render::write_wav(&p, &l, &r)?;
            clips.push(json!({"section": name, "path": p.to_string_lossy()}));
        }
    }
    Ok(json!({"offset_s": r2(offset), "overall": if total_t > 0.0 { (tot / total_t).round() } else { 0.0 }, "sections": rows, "parts": parts, "clips": clips,
        "legend": "overall 0-100 = 30% tone + 30% groove (onsets within 50 ms) + 30% harmony (chroma) + 10% level; tone_diff_db = RMS difference of the octave balance; tone_bands_db = that difference per octave (40-80, 80-160, 160-320, 320-640, 640-1250, 1.25-2.5k, 2.5-5k, 5-10k, 10-16k Hz; + = ours louder in that band); band_share_pct / ref_band_share_pct (with ours_stems + reference_stems) = how much of each octave each part holds; level_diff_db and share_diff_db = ours minus reference",
        "next": ["match_eq", "balance_mix", "set_section_mix"]}))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "detect_fx",
            description: "Find the production moves in a recording, with times and bar.beat positions: risers and builds into a bar line, reverse swells, stops and breaks, filter sweeps up or down, impacts on a downbeat. Works on a full mix or one stem; pass the full mix as grid_from to share its bar numbers. Writes the list to a JSON file.",
            mutates: false,
            schema: || obj(json!({
                "path": {"type": "string", "description": "audio file (mix or stem)"},
                "bpm": {"type": "number"}, "downbeat_s": {"type": "number"},
                "grid_from": {"type": "string", "description": "take the bar grid from this file (the full mix)"},
                "beats_per_bar": {"type": "integer"}, "out_dir": {"type": "string"}
            }), &["path"]),
            run: detect_fx_tool,
        },
        Tool {
            name: "arrangement_map",
            description: "Map a song's arrangement: sections (intro, verse, chorus, bridge, break, outro, with a letter so repeats share one), each part's activity per bar (0-9) and the bars where parts enter or exit. Give stems_dir (the folder separate_stems wrote: drums, bass, vocals, other) for real parts; without it the low, mid and high bands of the mix stand in. Writes a JSON file.",
            mutates: false,
            schema: || obj(json!({
                "path": {"type": "string", "description": "the full mix"},
                "stems_dir": {"type": "string"},
                "bpm": {"type": "number"}, "downbeat_s": {"type": "number"}, "beats_per_bar": {"type": "integer"},
                "min_section_bars": {"type": "integer", "description": "shortest section (default 4)"},
                "out_dir": {"type": "string"}
            }), &["path"]),
            run: arrangement_tool,
        },
        Tool {
            name: "score_sections",
            description: "Score how close one rendition is to another, section by section and part by part: groove (onsets within 50 ms), harmony (chroma), tonal balance (octave bands) and level, 0-100 each section. Sections come from `sections` [{name,t0,t1}] (ours' times), an arrangement_map file (sections_file) or 8-bar blocks. The reference is aligned automatically (offset_s to force). ours_stems + reference_stems add per-part scores, each part's share of the mix and of each octave (ours_stems may hold per-track files such as render's stems_dir; they are summed into drums/bass/vocals/other by name, or by stem_map {track: group}). Each section also reports tone_bands_db, the tonal difference per octave, so you know which band to fix. clips:true writes A/B listening clips (reference, 1 s gap, ours, level-matched).",
            mutates: false,
            schema: || obj(json!({
                "ours": {"type": "string"}, "reference": {"type": "string"},
                "sections": {"type": "array", "items": {"type": "object"}},
                "sections_file": {"type": "string"},
                "bpm": {"type": "number"},
                "offset_s": {"type": "number", "description": "reference time minus ours' time"},
                "max_offset_s": {"type": "number"},
                "ours_stems": {"type": "string"}, "reference_stems": {"type": "string"},
                "stem_map": {"type": "object", "description": "ours stem file name -> drums|bass|vocals|other (overrides the automatic grouping)"},
                "clips": {"type": "boolean"}, "clip_seconds": {"type": "number"}, "out_dir": {"type": "string"}
            }), &["ours", "reference"]),
            run: score_tool,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::{Biquad, BiquadKind, Rng};

    fn beat(bpm: f32, secs: f32, amp: f32) -> Vec<f32> {
        let n = (secs * SR) as usize;
        let mut x = vec![0.0f32; n];
        let bs = 60.0 / bpm;
        let mut t = 0.0;
        while t < secs {
            let i0 = (t * SR) as usize;
            for k in 0..(0.2 * SR) as usize {
                let tt = k as f32 / SR;
                if i0 + k < n {
                    x[i0 + k] += amp * (2.0 * PI * (55.0 + 90.0 * (-tt * 30.0).exp()) * tt).sin() * (-tt * 15.0).exp();
                }
            }
            t += bs;
        }
        x
    }

    #[test]
    fn finds_a_riser_a_stop_and_an_impact() {
        let bpm = 120.0; // bar 2 s
        let g = Grid::new(bpm, 0.0, 4);
        let mut x = beat(bpm, 24.0, 0.5);
        let mut rng = Rng::new(3);
        // a 2-bar noise riser into bar 5 (t = 4..8 s): a low-cut sweeping up, getting louder
        let mut hp = Biquad::new(BiquadKind::LowCut, 200.0, 0.7, 0.0);
        for i in (4.0 * SR) as usize..(8.0 * SR) as usize {
            let p = (i as f32 / SR - 4.0) / 4.0;
            if i % 256 == 0 {
                hp = Biquad::new(BiquadKind::LowCut, 200.0 * 2f32.powf(p * 5.0), 0.7, 0.0);
            }
            x[i] += 0.3 * p * p * hp.process(rng.bipolar());
        }
        // a stop: one bar of silence (bar 8, t = 14..16 s), then a big low hit on bar 9
        for v in &mut x[(14.0 * SR) as usize..(16.0 * SR) as usize] {
            *v = 0.0;
        }
        for k in 0..(0.5 * SR) as usize {
            let tt = k as f32 / SR;
            x[(16.0 * SR) as usize + k] += 0.9 * (2.0 * PI * 45.0 * tt).sin() * (-tt * 4.0).exp();
        }
        let fx = detect_fx(&x, g);
        let kinds: Vec<(String, f32)> = fx.iter().map(|f| (f.kind.clone(), f.t0)).collect();
        assert!(fx.iter().any(|f| f.kind == "riser" && (f.t1 - 8.0).abs() < 0.1), "{kinds:?}");
        assert!(fx.iter().any(|f| (f.kind == "break" || f.kind == "stop") && (f.t0 - 14.0).abs() < 0.5), "{kinds:?}");
        assert!(fx.iter().filter(|f| f.kind == "impact" || f.kind == "reverse_swell").count() <= 3, "false alarms {kinds:?}");
        assert!(fx.iter().any(|f| f.kind == "impact" && (f.t0 - 16.0).abs() < 0.1), "{kinds:?}");
    }

    #[test]
    fn arrangement_sees_parts_enter_and_sections_repeat() {
        let bpm = 120.0;
        let g = Grid::new(bpm, 0.0, 4);
        let secs = 48.0; // 24 bars: 8 drums only, 8 drums + bass, 8 drums only
        let drums = beat(bpm, secs, 0.5);
        let mut bass = vec![0.0f32; drums.len()];
        for i in (16.0 * SR) as usize..(32.0 * SR) as usize {
            bass[i] = 0.4 * (2.0 * PI * 73.4 * i as f32 / SR).sin();
        }
        let mix: Vec<f32> = drums.iter().zip(bass.iter()).map(|(a, b)| a + b).collect();
        let ar = arrangement(&mix, &[("drums".into(), drums), ("bass".into(), bass)], g, 4);
        assert!(ar.events.iter().any(|(b, p, w)| *b == 8 && p == "bass" && w == "enters"), "{:?}", ar.events);
        assert!(ar.events.iter().any(|(b, p, w)| *b == 16 && p == "bass" && w == "exits"), "{:?}", ar.events);
        let starts: Vec<usize> = ar.sections.iter().map(|s| s.bar0).collect();
        assert!(starts.contains(&8) && starts.contains(&16), "{starts:?}");
        let l: Vec<char> = ar.sections.iter().map(|s| s.label).collect();
        assert_eq!(l.first(), l.last(), "{l:?}");
    }

    #[test]
    fn identical_renditions_score_high_and_shifted_is_realigned() {
        let x = beat(120.0, 12.0, 0.5);
        let mut y = vec![0.0f32; (0.3 * SR) as usize];
        y.extend_from_slice(&x);
        let fo = frames(&x);
        let fy = frames(&y);
        let off = align_offset(&fo, &fy, 2.0);
        assert!((off - 0.3).abs() < 0.03, "offset {off}");
        let (oo, oy) = (onset_times(&fo), onset_times(&fy));
        let a = (Vec::new(), fo, oo);
        let b = (Vec::new(), fy, oy);
        let s = score_span(&a, &b, 1.0, 10.0, off, &[], &[]);
        assert!(s.groove_pct >= 90.0 && s.tone_db < 1.5, "{s:?}");
        assert!(onset_f(&[1.0, 2.0], &[1.02, 3.0], 0.05) > 0.4);
    }
}

#[cfg(test)]
mod stem_group_tests {
    use super::stem_group;
    #[test]
    fn groups_track_names() {
        assert_eq!(stem_group("vox"), "vocals");
        assert_eq!(stem_group("Lead Vocal"), "vocals");
        assert_eq!(stem_group("drums"), "drums");
        assert_eq!(stem_group("hats_top"), "drums");
        assert_eq!(stem_group("808"), "bass");
        assert_eq!(stem_group("gtr"), "other");
        assert_eq!(stem_group("chords"), "other");
    }
}
