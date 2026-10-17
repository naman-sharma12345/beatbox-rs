//! Time and volume FX in the style of FL's Gross Beat: a drawn playback-position
//! envelope (half speed, repeats, reverse, tape stops, scratches, freezes) and a
//! drawn volume envelope (trance gates, pumps, chops), both looping every
//! `cycle_steps` 16ths and both read from presets or from points.

use crate::dsp::tape_dc::TapeDcGuard;
use crate::dsp::SR;
use crate::fx::GrossBeatFx;

/// Piecewise-linear envelope on 0..1 from "u:v, u:v" points (sorted by u).
pub fn parse_points(s: &str) -> Result<Vec<(f32, f32)>, String> {
    let mut v = Vec::new();
    for part in s.split([',', ';', ' ']).filter(|x| !x.trim().is_empty()) {
        let (a, b) = part
            .split_once(':')
            .ok_or_else(|| format!("point '{part}' is not u:v (e.g. 0:0, 0.5:0.25)"))?;
        let u: f32 = a
            .trim()
            .parse()
            .map_err(|_| format!("bad position in '{part}'"))?;
        let y: f32 = b
            .trim()
            .parse()
            .map_err(|_| format!("bad value in '{part}'"))?;
        if !(0.0..=1.0).contains(&u) {
            return Err(format!("position {u} outside 0..1 in '{part}'"));
        }
        v.push((u, y));
    }
    v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    if v.is_empty() {
        return Err("no points".into());
    }
    Ok(v)
}

fn eval(pts: &[(f32, f32)], u: f32) -> f32 {
    if u <= pts[0].0 {
        return pts[0].1;
    }
    for w in pts.windows(2) {
        let ((u0, y0), (u1, y1)) = (w[0], w[1]);
        if u <= u1 {
            let t = if u1 > u0 { (u - u0) / (u1 - u0) } else { 1.0 };
            return y0 + (y1 - y0) * t;
        }
    }
    pts[pts.len() - 1].1
}

/// Time presets: (name, what it does). Positions are fractions of the cycle.
pub const TIME_PRESETS: &[(&str, &str)] = &[
    ("none", "plays in time"),
    (
        "half_speed",
        "whole cycle at half speed, an octave down (classic slow-down)",
    ),
    (
        "half_speed_end",
        "last quarter of the cycle drops to half speed",
    ),
    ("repeat_beat", "first beat of the cycle repeats four times"),
    ("repeat_end_8th", "last beat stutters an 8th note"),
    (
        "repeat_end_16th",
        "last beat stutters 16ths (a build-up roll)",
    ),
    ("reverse_end", "last beat plays backwards"),
    ("reverse", "the whole cycle plays backwards"),
    ("tape_stop", "the whole cycle slows to a stop"),
    ("tape_stop_end", "last beat slows to a stop (before a drop)"),
    ("scratch_end", "last beat is scratched back and forth"),
    (
        "freeze_end",
        "last beat holds the start of that beat (a stutter freeze)",
    ),
    (
        "double_speed",
        "plays the first half of the cycle twice as fast, twice",
    ),
];

/// Volume presets: (name, what it does).
pub const VOLUME_PRESETS: &[(&str, &str)] = &[
    ("none", "full volume"),
    ("trance_gate", "16th on/off gate"),
    ("trance_gate_8th", "8th on/off gate"),
    ("pump", "sidechain-style duck on every beat"),
    ("pump_hard", "deep, fast-release duck on every beat"),
    ("tresillo", "3-3-2 chop (x..x..x.)"),
    ("offbeat", "only the off-beats sound"),
    ("fade_in", "fades in over the cycle"),
    ("fade_out", "fades out over the cycle"),
    ("stop_end", "silent last beat (a drop gap)"),
    ("swell", "swells up toward each beat (reverse-pump)"),
];

/// Source position (fraction of the cycle) for output fraction u.
fn time_preset(name: &str, u: f32) -> Option<f32> {
    let end = |f: &dyn Fn(f32) -> f32| {
        if u < 0.75 {
            u
        } else {
            0.75 + f((u - 0.75) / 0.25) * 0.25
        }
    };
    Some(match name {
        "none" | "" => u,
        "half_speed" => u * 0.5,
        "half_speed_end" => end(&|t| t * 0.5),
        "repeat_beat" => (u * 4.0).fract() * 0.25,
        "repeat_end_8th" => end(&|t| (t * 2.0).fract() * 0.5),
        "repeat_end_16th" => end(&|t| (t * 4.0).fract() * 0.25),
        "reverse_end" => end(&|t| 1.0 - t),
        "reverse" => 1.0 - u,
        "tape_stop" => u - 0.5 * u * u,
        "tape_stop_end" => end(&|t| t - 0.5 * t * t),
        "scratch_end" => end(&|t| 0.5 - 0.5 * (t * std::f32::consts::TAU * 2.0).cos() * (1.0 - t)),
        "freeze_end" => end(&|t| (t * 16.0).fract() * 0.06),
        "double_speed" => (u * 2.0).fract(),
        _ => return None,
    })
}

fn volume_preset(name: &str, u: f32, cycle_steps: f32) -> Option<f32> {
    let steps = cycle_steps.max(1.0);
    let s = u * steps; // position in 16ths
    let beat = (s / 4.0).fract();
    Some(match name {
        "none" | "" => 1.0,
        "trance_gate" => {
            if s.fract() < 0.55 {
                1.0
            } else {
                0.0
            }
        }
        "trance_gate_8th" => {
            if (s / 2.0).fract() < 0.55 {
                1.0
            } else {
                0.0
            }
        }
        "pump" => 0.35 + 0.65 * (beat / 0.45).min(1.0).powf(0.6),
        "pump_hard" => 0.05 + 0.95 * (beat / 0.3).min(1.0).powf(0.5),
        "tresillo" => {
            let k = (s as usize) % 8;
            if matches!(k, 0 | 3 | 6) {
                1.0
            } else if matches!(k, 1 | 4) {
                0.5
            } else {
                0.0
            }
        }
        "offbeat" => {
            if beat >= 0.5 {
                1.0
            } else {
                0.0
            }
        }
        "fade_in" => u,
        "fade_out" => 1.0 - u,
        "stop_end" => {
            if u < 0.75 {
                1.0
            } else {
                0.0
            }
        }
        "swell" => beat.powf(2.0),
        _ => return None,
    })
}

pub fn check(p: &GrossBeatFx) -> Result<(), String> {
    if p.time_points.trim().is_empty() && time_preset(&p.time, 0.5).is_none() {
        return Err(format!(
            "unknown time preset '{}'. Presets: {}",
            p.time,
            TIME_PRESETS
                .iter()
                .map(|x| x.0)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if p.volume_points.trim().is_empty() && volume_preset(&p.volume, 0.5, 16.0).is_none() {
        return Err(format!(
            "unknown volume preset '{}'. Presets: {}",
            p.volume,
            VOLUME_PRESETS
                .iter()
                .map(|x| x.0)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !p.time_points.trim().is_empty() {
        parse_points(&p.time_points)?;
    }
    if !p.volume_points.trim().is_empty() {
        parse_points(&p.volume_points)?;
    }
    Ok(())
}

fn read(src: &[f32], pos: f32) -> f32 {
    if pos < 0.0 {
        return 0.0;
    }
    let i = pos as usize;
    let f = pos - i as f32;
    let a = src.get(i).copied().unwrap_or(0.0);
    let b = src.get(i + 1).copied().unwrap_or(0.0);
    a + (b - a) * f
}

pub fn gross_beat(p: &GrossBeatFx, l: &mut [f32], r: &mut [f32], step_secs: f32) {
    gross_beat_at(p, l, r, step_secs, 0)
}

/// `gross_beat` for a buffer that starts `offset` samples into the song:
/// cycles count from the song's start, so a window matches the same samples
/// of a whole-song pass (source audio from before the buffer reads as silence).
pub fn gross_beat_at(p: &GrossBeatFx, l: &mut [f32], r: &mut [f32], step_secs: f32, offset: usize) {
    gross_beat_guarded(p, l, r, step_secs, offset, true)
}

/// `gross_beat_at` without the tape-stop DC guard (tests compare against it).
#[cfg(test)]
pub(crate) fn gross_beat_unguarded(
    p: &GrossBeatFx,
    l: &mut [f32],
    r: &mut [f32],
    step_secs: f32,
    offset: usize,
) {
    gross_beat_guarded(p, l, r, step_secs, offset, false)
}

/// Where the tape-stop ramp of a time preset starts (fraction of the cycle),
/// for the presets the DC guard runs on.
fn tape_ramp_start(p: &GrossBeatFx) -> Option<f32> {
    if !p.time_points.trim().is_empty() {
        return None;
    }
    match p.time.as_str() {
        "tape_stop" => Some(0.0),
        "tape_stop_end" => Some(0.75),
        _ => None,
    }
}

/// `gross_beat_at` with the tape-stop DC guard switchable (`dc_guard`; the
/// rack always runs it on). The guard (`crate::dsp::tape_dc`, a 20 Hz 2-pole
/// high-pass on the wet read, before the volume envelope) runs only on the
/// ramp of the `tape_stop` / `tape_stop_end` time presets; every other
/// preset, and drawn time points, never read it and come out the same bit
/// for bit either way. `tape_stop_end` plays at unity speed until 3/4 of the
/// cycle, so the guard fades in over the first quarter of its ramp (speed
/// 1 -> 0.75) instead of switching on with a step.
fn gross_beat_guarded(
    p: &GrossBeatFx,
    l: &mut [f32],
    r: &mut [f32],
    step_secs: f32,
    offset: usize,
    dc_guard: bool,
) {
    let mix = p.mix.clamp(0.0, 1.0);
    if mix <= 0.0 || check(p).is_err() {
        return;
    }
    let n = l.len();
    let cycle = ((p.cycle_steps.max(1.0) * step_secs * SR) as usize).max(64);
    let tp = parse_points(&p.time_points).ok();
    let vp = parse_points(&p.volume_points).ok();
    let (src_l, src_r) = (l.to_vec(), r.to_vec());
    // positions jump (repeats, reverse): crossfade around jumps
    let fade = ((p.smooth_ms.max(0.5) * 0.001 * SR) as usize).max(1);
    let pos_of = |i: usize| -> f32 {
        let c0 = (i / cycle) * cycle;
        let u = (i - c0) as f32 / cycle as f32;
        let f = match &tp {
            Some(pts) => eval(pts, u).clamp(-1.0, 2.0),
            None => time_preset(&p.time, u).unwrap_or(u),
        };
        c0 as f32 + f * cycle as f32
    };
    let mut prev_pos = -1.0f32;
    let mut jump_at = usize::MAX;
    let mut jump_from = 0.0f32;
    let mut smooth_g = 1.0f32;
    let a = (-1.0 / (fade as f32)).exp();
    // source at song position `pos` (seconds of the song, in samples)
    let rd = |src: &[f32], pos: f32| read(src, pos - offset as f32);
    // tape-stop DC guard: the ramp start, the guard on the current ramp's read
    // (per channel), the guard carried on the old read through a jump's
    // crossfade, and the cycle the current one was warmed for
    let ramp = if dc_guard { tape_ramp_start(p) } else { None };
    let mut dc = [TapeDcGuard::default(); 2];
    let mut dc_old = [TapeDcGuard::default(); 2];
    let mut dc_old_on = false;
    let mut dc_cycle: Option<usize> = None;
    let u_of = |j: usize| -> f32 {
        let c0 = (j / cycle) * cycle;
        (j - c0) as f32 / cycle as f32
    };
    for i in 0..n {
        let j = i + offset;
        let pos = pos_of(j);
        // a discontinuity (> 4 samples off the running position) starts a crossfade
        if prev_pos >= 0.0 && (pos - (prev_pos + 1.0)).abs() > 4.0 && (pos - prev_pos).abs() > 4.0 {
            jump_at = i;
            jump_from = prev_pos;
            if ramp.is_some() {
                // the old read keeps its guard through the crossfade
                dc_old = dc;
                dc_old_on = dc_cycle.is_some();
            }
        }
        let (mut wl, mut wr) = (rd(&src_l, pos), rd(&src_r, pos));
        if let Some(u0) = ramp {
            let c0 = (j / cycle) * cycle;
            let u = u_of(j);
            if u >= u0 {
                if dc_cycle != Some(c0) {
                    // a new ramp: warm the guard on the source playing at unity
                    // speed into the ramp's first sample, then on this ramp's
                    // reads up to here (a buffer that starts mid-ramp)
                    dc_cycle = Some(c0);
                    let mut je = c0;
                    while u_of(je) < u0 {
                        je += 1;
                    }
                    let pe = pos_of(je);
                    let pre = crate::dsp::tape_dc::preroll();
                    for g in dc.iter_mut() {
                        g.reset();
                    }
                    for k in (1..=pre).rev() {
                        let q = pe - k as f32;
                        dc[0].process(rd(&src_l, q));
                        dc[1].process(rd(&src_r, q));
                    }
                    for jj in je..j {
                        let q = pos_of(jj);
                        dc[0].process(rd(&src_l, q));
                        dc[1].process(rd(&src_r, q));
                    }
                }
                let (fl, fr) = (dc[0].process(wl), dc[1].process(wr));
                if u0 > 0.0 {
                    let xf = ((u - u0) / ((1.0 - u0) * 0.25)).min(1.0);
                    wl += (fl - wl) * xf;
                    wr += (fr - wr) * xf;
                } else {
                    wl = fl;
                    wr = fr;
                }
            }
        }
        if jump_at != usize::MAX && i - jump_at < fade {
            let t = (i - jump_at) as f32 / fade as f32;
            let old = jump_from + (i - jump_at) as f32 + 1.0;
            let (mut ol, mut or) = (rd(&src_l, old), rd(&src_r, old));
            if dc_old_on {
                ol = dc_old[0].process(ol);
                or = dc_old[1].process(or);
            }
            wl = wl * t + ol * (1.0 - t);
            wr = wr * t + or * (1.0 - t);
        }
        prev_pos = pos;
        let c0 = (j / cycle) * cycle;
        let u = (j - c0) as f32 / cycle as f32;
        let g = match &vp {
            Some(pts) => eval(pts, u).clamp(0.0, 1.5),
            None => volume_preset(&p.volume, u, p.cycle_steps).unwrap_or(1.0),
        };
        // smooth gain steps (no clicks on gates)
        smooth_g = g + (smooth_g - g) * a;
        wl *= smooth_g;
        wr *= smooth_g;
        l[i] = src_l[i] * (1.0 - mix) + wl * mix;
        r[i] = src_r[i] * (1.0 - mix) + wr * mix;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / SR).sin() * 0.5)
            .collect()
    }

    /// Worst |mean| of the 100 ms before each cycle ends (where the stop has
    /// frozen; the next cycle's jump comes after it).
    fn stop_tail_dc(x: &[f32], cycle: usize) -> f32 {
        let tail = (0.1 * SR) as usize;
        (1..=x.len() / cycle)
            .map(|k| {
                (x[k * cycle - tail..k * cycle]
                    .iter()
                    .map(|v| *v as f64)
                    .sum::<f64>()
                    / tail as f64)
                    .abs() as f32
            })
            .fold(0.0, f32::max)
    }

    /// B75: the tape-stop DC guard on gross_beat's tape_stop and tape_stop_end
    /// presets (no volume envelope, so the frozen stop would hold its value).
    #[test]
    fn gross_beat_tape_stop_dc_guard() {
        let step = 0.125;
        let cycle = (16.0 * step * SR) as usize;
        for (preset, bound) in [("tape_stop", 0.03f32), ("tape_stop_end", 0.04)] {
            let p = GrossBeatFx {
                time: preset.into(),
                ..Default::default()
            };
            let (mut off, mut on, mut re_off, mut re_on) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            let fade = ((p.smooth_ms.max(0.5) * 0.001 * SR) as usize).max(1);
            for f in [55.0f32, 110.0, 220.0] {
                for k in 0..8 {
                    let ph = k as f32 * std::f32::consts::PI / 4.0;
                    let x: Vec<f32> = (0..2 * cycle)
                        .map(|i| (i as f32 * f * std::f32::consts::TAU / SR + ph).sin() * 0.5)
                        .collect();
                    let (mut l0, mut r0) = (x.clone(), x.clone());
                    gross_beat_guarded(&p, &mut l0, &mut r0, step, 0, false);
                    let (mut l1, mut r1) = (x.clone(), x.clone());
                    gross_beat_guarded(&p, &mut l1, &mut r1, step, 0, true);
                    assert!(l1.iter().all(|v| v.is_finite() && v.abs() < 1.5));
                    off = off.max(stop_tail_dc(&l0, cycle));
                    on = on.max(stop_tail_dc(&l1, cycle));
                    // the re-trigger: whole source periods just after the
                    // jump's crossfade into the next cycle
                    let w = ((if f < 100.0 {
                        2.0
                    } else if f < 200.0 {
                        3.0
                    } else {
                        6.0
                    }) / f
                        * SR)
                        .round() as usize;
                    let s = cycle + fade;
                    let dc = |y: &[f32]| {
                        (y[s..s + w].iter().map(|v| *v as f64).sum::<f64>() / w as f64).abs() as f32
                    };
                    re_off = re_off.max(dc(&l0));
                    re_on = re_on.max(dc(&l1));
                }
            }
            eprintln!("gross_beat {preset} DC (worst mean of the last 100 ms): guard off {off:.4} FS, on {on:.4} FS; at the re-trigger: off {re_off:.4}, on {re_on:.4}");
            assert!(off > 0.10, "{preset}: the unguarded stop drifts: {off}");
            assert!(on < bound, "{preset}: the guarded stop: {on}");
            assert!(
                re_on < 0.015,
                "{preset}: no blip at the re-trigger: {re_on}"
            );
        }
        // every other preset (and drawn points) is the same bit for bit
        let n = cycle + 5000;
        for (t, _) in TIME_PRESETS {
            if t.starts_with("tape_stop") {
                continue;
            }
            for p in [
                GrossBeatFx {
                    time: t.to_string(),
                    volume: "pump".into(),
                    ..Default::default()
                },
                GrossBeatFx {
                    time: "tape_stop".into(),
                    time_points: "0:0, 1:0.5".into(),
                    ..Default::default()
                },
            ] {
                let (mut a, mut b) = (tone(n), tone(n));
                gross_beat_guarded(&p, &mut a, &mut b, step, 0, false);
                let (mut c, mut d) = (tone(n), tone(n));
                gross_beat(&p, &mut c, &mut d, step);
                assert!(a == c && b == d, "{t}");
            }
        }
        // a buffer that starts into the song (silent before it and for 0.5 s
        // after, the live-splice convention), also mid-ramp,
        // matches the whole-song pass exactly
        for preset in ["tape_stop", "tape_stop_end"] {
            let p = GrossBeatFx {
                time: preset.into(),
                cycle_steps: 4.0,
                ..Default::default()
            };
            for off in [50_000usize, 21_000] {
                let x: Vec<f32> = (0..150_000)
                    .map(|i| {
                        if i >= off + 22_050 {
                            (i as f32 * 61.0 * std::f32::consts::TAU / SR).sin() * 0.5
                        } else {
                            0.0
                        }
                    })
                    .collect();
                let (mut fl, mut fr) = (x.clone(), x.clone());
                gross_beat_at(&p, &mut fl, &mut fr, step, 0);
                let (mut wl, mut wr) = (x[off..].to_vec(), x[off..].to_vec());
                gross_beat_at(&p, &mut wl, &mut wr, step, off);
                assert!(
                    fl[off..] == wl[..] && fr[off..] == wr[..],
                    "{preset} offset {off}"
                );
            }
        }
    }

    #[test]
    fn presets_and_points() {
        let step = 0.125; // 120 BPM
        let n = (SR * step * 16.0) as usize;
        for (t, _) in TIME_PRESETS {
            for (v, _) in VOLUME_PRESETS {
                let p = GrossBeatFx {
                    time: t.to_string(),
                    volume: v.to_string(),
                    ..Default::default()
                };
                let (mut l, mut r) = (tone(n), tone(n));
                gross_beat(&p, &mut l, &mut r, step);
                assert!(l.iter().all(|x| x.is_finite() && x.abs() < 1.5), "{t}/{v}");
            }
        }
        // stop_end silences the last beat
        let p = GrossBeatFx {
            volume: "stop_end".into(),
            ..Default::default()
        };
        let (mut l, mut r) = (tone(n), tone(n));
        gross_beat(&p, &mut l, &mut r, step);
        let tail: f32 = l[n * 7 / 8..].iter().map(|x| x.abs()).sum::<f32>() / (n / 8) as f32;
        assert!(tail < 0.01, "tail {tail}");
        // half speed of a 440 Hz tone is ~220 Hz: count zero crossings
        let p = GrossBeatFx {
            time: "half_speed".into(),
            ..Default::default()
        };
        let (mut l, mut r) = (tone(n), tone(n));
        gross_beat(&p, &mut l, &mut r, step);
        let zc = l[100..n - 100]
            .windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count() as f32
            / ((n - 200) as f32 / SR);
        assert!((zc - 220.0).abs() < 15.0, "half speed {zc} Hz");
        assert!(check(&GrossBeatFx {
            time: "warp9".into(),
            ..Default::default()
        })
        .is_err());
        assert!(check(&GrossBeatFx {
            volume_points: "0:1, 0.5:0, 1:1".into(),
            ..Default::default()
        })
        .is_ok());
        assert!(parse_points("0:1, 2:0").is_err());
    }
}
