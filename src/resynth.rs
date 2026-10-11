//! Additive synthesis and resynthesis (FL's Harmor / Morphine): a sample
//! becomes a bank of sine partials, peak-picked from a short-time spectrum
//! and tracked frame to frame, and is played back from that bank. Once it is
//! partials it can be re-pitched with or without its formants, thinned to its
//! strongest partials, blurred in time (a frozen, pad-like smear), tilted
//! brighter or darker and balanced between odd and even harmonics. A tone
//! can also be built straight from a harmonic recipe (saw, square, organ,
//! bell, ...). Everything renders to a new registered sample.

use crate::analysis::fft;
use crate::dsp::SR;
use crate::engine::Engine;
use crate::samples::SampleInfo;
use crate::tools::{f_or, obj, s_opt, s_req, u_or, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::f32::consts::TAU;

const N: usize = 2048;
const HOP: usize = 256;

/// One analysis frame: (Hz, linear amplitude) per spectral peak, strongest first.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub peaks: Vec<(f32, f32)>,
}

/// Peak-pick every frame (2048-point Hann, 256 hop, parabolic interpolation),
/// keeping at most `max_partials` peaks no more than 60 dB under the frame's
/// strongest. Frame f is centred on sample f*HOP.
pub fn analyze(x: &[f32], max_partials: usize) -> Vec<Frame> {
    let win: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (TAU * i as f32 / N as f32).cos()).collect();
    let wsum: f32 = win.iter().sum();
    let frames = x.len() / HOP + 1;
    let mut out = Vec::with_capacity(frames);
    let mut re = vec![0.0f32; N];
    let mut im = vec![0.0f32; N];
    for f in 0..frames {
        for k in 0..N {
            let i = (f * HOP + k) as isize - (N / 2) as isize;
            re[k] = if i >= 0 && (i as usize) < x.len() { x[i as usize] * win[k] } else { 0.0 };
            im[k] = 0.0;
        }
        fft(&mut re, &mut im);
        let mag: Vec<f32> = (0..=N / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect();
        let top = mag.iter().cloned().fold(0.0f32, f32::max);
        let mut peaks = Vec::new();
        if top > 1e-7 {
            let floor = top * 10f32.powf(-60.0 / 20.0);
            for k in 2..N / 2 - 2 {
                let (a, b, c) = (mag[k - 1], mag[k], mag[k + 1]);
                // a peak over its two neighbours each side: the Hann window's
                // side lobes are not partials
                if b > a && b >= c && b >= mag[k - 2] && b >= mag[k + 2] && b > floor {
                    // parabolic interpolation on dB
                    let (la, lb, lc) = ((a + 1e-12).ln(), (b + 1e-12).ln(), (c + 1e-12).ln());
                    let den = la - 2.0 * lb + lc;
                    let d = if den.abs() > 1e-9 { 0.5 * (la - lc) / den } else { 0.0 };
                    let hz = (k as f32 + d.clamp(-0.5, 0.5)) * SR / N as f32;
                    let amp = (lb - 0.25 * (la - lc) * d).exp() * 2.0 / wsum;
                    peaks.push((hz, amp));
                }
            }
            peaks.sort_by(|p, q| q.1.partial_cmp(&p.1).unwrap_or(std::cmp::Ordering::Equal));
            peaks.truncate(max_partials);
        }
        out.push(Frame { peaks });
    }
    out
}

/// How a partial bank is played back.
#[derive(Clone, Debug)]
pub struct Shape {
    /// semitones up (+) or down (-)
    pub pitch_st: f32,
    /// keep the spectral envelope (formants) where it was when re-pitching
    pub keep_formants: bool,
    /// strongest partials kept per frame
    pub partials: usize,
    /// 0 = as analysed, 1 = amplitudes smeared over ~1 s (a frozen pad)
    pub blur: f32,
    /// dB per octave around 1 kHz (+ brighter, - darker)
    pub tilt_db_oct: f32,
    /// -1 = odd harmonics only (hollow, square-like), +1 = even only (the
    /// fundamental stays), 0 = as analysed
    pub odd_even: f32,
}

impl Default for Shape {
    fn default() -> Self {
        Shape { pitch_st: 0.0, keep_formants: false, partials: 64, blur: 0.0, tilt_db_oct: 0.0, odd_even: 0.0 }
    }
}

/// The frame's fundamental guess: the lowest peak within 20 dB of the strongest.
fn frame_f0(fr: &Frame) -> Option<f32> {
    let top = fr.peaks.iter().map(|p| p.1).fold(0.0f32, f32::max);
    fr.peaks.iter().filter(|p| p.1 > top * 0.1 && p.0 > 30.0).map(|p| p.0).fold(None, |m: Option<f32>, f| Some(m.map_or(f, |m| m.min(f))))
}

/// Spectral envelope of a frame at `hz` (linear interpolation between peaks).
fn envelope(sorted: &[(f32, f32)], hz: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    if hz <= sorted[0].0 {
        return sorted[0].1;
    }
    for w in sorted.windows(2) {
        if hz <= w[1].0 {
            let t = (hz - w[0].0) / (w[1].0 - w[0].0).max(1e-6);
            return w[0].1 + t * (w[1].1 - w[0].1);
        }
    }
    sorted[sorted.len() - 1].1 * 0.5
}

/// Apply the shape to the analysed frames (frequencies and amplitudes only).
pub fn reshape(frames: &[Frame], s: &Shape) -> Vec<Frame> {
    let ratio = 2f32.powf(s.pitch_st / 12.0);
    let oe = s.odd_even.clamp(-1.0, 1.0);
    let mut out: Vec<Frame> = frames
        .iter()
        .map(|fr| {
            let f0 = frame_f0(fr);
            let mut env: Vec<(f32, f32)> = fr.peaks.clone();
            env.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            let peaks = fr
                .peaks
                .iter()
                .take(s.partials.max(1))
                .filter_map(|&(hz, amp)| {
                    let nf = hz * ratio;
                    if nf > 0.45 * SR || nf < 20.0 {
                        return None;
                    }
                    let mut a = if s.keep_formants && ratio != 1.0 { envelope(&env, nf) } else { amp };
                    if s.tilt_db_oct != 0.0 {
                        a *= 10f32.powf(s.tilt_db_oct * (nf / 1000.0).log2() / 20.0);
                    }
                    if oe != 0.0 {
                        if let Some(f0) = f0 {
                            let h = (hz / f0).round().max(1.0) as u32;
                            let g = if h == 1 { 1.0 } else if h % 2 == 0 { (1.0 + oe).min(1.0) } else { (1.0 - oe).min(1.0) };
                            a *= g;
                        }
                    }
                    (a > 1e-7).then_some((nf, a))
                })
                .collect();
            Frame { peaks: peaks }
        })
        .collect();
    if s.blur > 0.0 {
        // smear each partial's amplitude over time: a one-pole per frequency
        // bin (1/4 semitone wide) running across the frames
        // time constant: blur seconds (blur 1 = ~1 s)
        let k = (-(HOP as f32) / (SR * s.blur.clamp(0.005, 1.0))).exp();
        let mut held: std::collections::HashMap<i32, (f32, f32)> = std::collections::HashMap::new();
        for fr in out.iter_mut() {
            let mut seen = std::collections::HashSet::new();
            for p in fr.peaks.iter_mut() {
                let bin = (48.0 * (p.0 / 440.0).log2()).round() as i32;
                seen.insert(bin);
                let e = held.entry(bin).or_insert((p.0, 0.0));
                e.1 = k * e.1 + (1.0 - k) * p.1;
                e.0 = p.0;
                p.1 = e.1;
            }
            let mut tails = Vec::new();
            for (bin, v) in held.iter_mut() {
                if !seen.contains(bin) {
                    v.1 *= k;
                    if v.1 > 1e-6 {
                        tails.push(*v);
                    }
                }
            }
            held.retain(|_, v| v.1 > 1e-6);
            fr.peaks.extend(tails);
            fr.peaks.sort_by(|p, q| q.1.partial_cmp(&p.1).unwrap_or(std::cmp::Ordering::Equal));
            fr.peaks.truncate(s.partials.max(1) * 2);
        }
    }
    out
}

/// Play a partial bank: oscillators matched frame to frame by nearest
/// frequency (within 3%), born from silence and faded out when their peak
/// goes, with frequency and amplitude interpolated across each hop.
pub fn synthesize(frames: &[Frame], len: usize) -> Vec<f32> {
    struct Osc {
        hz: f32,
        amp: f32,
        phase: f32,
        t_hz: f32,
        t_amp: f32,
        alive: bool,
    }
    let mut y = vec![0.0f32; len];
    let mut osc: Vec<Osc> = Vec::new();
    for (f, fr) in frames.iter().enumerate() {
        // targets for this hop
        for o in osc.iter_mut() {
            o.alive = false;
        }
        let mut used = vec![false; osc.len()];
        for &(hz, amp) in &fr.peaks {
            let mut best: Option<usize> = None;
            let mut bd = f32::MAX;
            for (i, o) in osc.iter().enumerate() {
                if used[i] {
                    continue;
                }
                let d = (o.hz - hz).abs() / hz;
                if d < 0.03 && d < bd {
                    bd = d;
                    best = Some(i);
                }
            }
            match best {
                Some(i) => {
                    used[i] = true;
                    osc[i].t_hz = hz;
                    osc[i].t_amp = amp;
                    osc[i].alive = true;
                }
                None => {
                    osc.push(Osc { hz, amp: 0.0, phase: 0.0, t_hz: hz, t_amp: amp, alive: true });
                    used.push(true);
                }
            }
        }
        for o in osc.iter_mut() {
            if !o.alive {
                o.t_amp = 0.0;
                o.t_hz = o.hz;
            }
        }
        // frame f is centred on f*HOP: interpolate from there to the next frame
        let a0 = f * HOP;
        if a0 >= len {
            break;
        }
        let a1 = (a0 + HOP).min(len);
        let n = (a1 - a0) as f32;
        for o in osc.iter_mut() {
            let (h0, h1, g0, g1) = (o.hz, o.t_hz, o.amp, o.t_amp);
            if g0 < 1e-7 && g1 < 1e-7 {
                o.hz = h1;
                o.amp = g1;
                continue;
            }
            for (j, v) in y[a0..a1].iter_mut().enumerate() {
                let t = j as f32 / n;
                let hz = h0 + t * (h1 - h0);
                o.phase = (o.phase + TAU * hz / SR) % TAU;
                *v += (g0 + t * (g1 - g0)) * o.phase.sin();
            }
            o.hz = h1;
            o.amp = g1;
        }
        osc.retain(|o| o.amp > 1e-7 || o.alive);
    }
    y
}

/// Analyse, reshape and resynthesise `x`; the result keeps the input's peak level.
pub fn resynthesize(x: &[f32], s: &Shape) -> Vec<f32> {
    // a blurred bank rings on: analyse with a tail of silence for it to decay into
    let tail = if s.blur > 0.0 { (SR * 1.5 * s.blur) as usize } else { 0 };
    let mut xp = x.to_vec();
    xp.extend(std::iter::repeat(0.0).take(tail));
    let frames = analyze(&xp, s.partials.max(1));
    let shaped = reshape(&frames, s);
    let mut y = synthesize(&shaped, xp.len());
    let pin = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let pout = y.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if pout > 1e-6 {
        let g = pin.max(0.05) / pout;
        for v in y.iter_mut() {
            *v *= g;
        }
    }
    crate::voice_pro::fade_edges(&mut y, 64, (0.01 * SR) as usize);
    y
}

/// Harmonic amplitudes of a named recipe (index 0 = the fundamental).
pub fn recipe(name: &str, n: usize) -> Result<Vec<f32>> {
    let known = ["saw", "square", "triangle", "sine", "organ", "choir", "bell", "hollow"];
    if !known.contains(&name) {
        bail!("unknown recipe '{name}': {}", known.join(", "));
    }
    let n = n.clamp(1, 256);
    let mut v = Vec::with_capacity(n);
    for i in 1..=n {
        let h = i as f32;
        let odd = i % 2 == 1;
        v.push(match name {
            "saw" => 1.0 / h,
            "square" => if odd { 1.0 / h } else { 0.0 },
            "triangle" => if odd { 1.0 / (h * h) } else { 0.0 },
            "sine" => if i == 1 { 1.0 } else { 0.0 },
            // drawbars 16' 8' 4' 2 2/3' 2': 1, 2, 3, 4 strong, the rest faint
            "organ" => match i { 1 => 1.0, 2 => 0.8, 3 => 0.6, 4 => 0.5, 6 => 0.3, 8 => 0.25, _ => 0.02 / h },
            // soft choir/pad: a formant-ish hump around harmonics 2-5
            "choir" => (-((h - 3.5) / 2.5).powi(2)).exp() * 0.8 + 0.2 / h,
            "bell" => 1.0 / h.sqrt(),
            _ => if odd { 1.0 / h.sqrt() } else { 0.1 / h }, // hollow
        });
    }
    Ok(v)
}

/// Build a tone from harmonic amplitudes. `inharm` stretches partial h to
/// h*sqrt(1 + inharm*h^2) (piano/bell-like); `decay` makes higher partials
/// die sooner (seconds for the fundamental's 60 dB fall; 0 = sustained).
pub fn additive_tone(hz: f32, amps: &[f32], secs: f32, inharm: f32, decay: f32, attack: f32, release: f32) -> Vec<f32> {
    let n = (secs.clamp(0.05, 30.0) * SR) as usize;
    let mut y = vec![0.0f32; n];
    for (i, &a) in amps.iter().enumerate() {
        if a.abs() < 1e-5 {
            continue;
        }
        let h = (i + 1) as f32;
        let f = hz * h * (1.0 + inharm.max(0.0) * h * h).sqrt();
        if f >= 0.45 * SR {
            break;
        }
        let rate = if decay > 0.0 { 6.9 / decay * h.powf(0.6) } else { 0.0 };
        let ph0 = (h * 1.618).fract() * TAU;
        for (j, v) in y.iter_mut().enumerate() {
            let t = j as f32 / SR;
            *v += a * (ph0 + TAU * f * t).sin() * (-rate * t).exp();
        }
    }
    let att = (attack.max(0.001) * SR) as usize;
    let rel = (release.max(0.005) * SR) as usize;
    for (j, v) in y.iter_mut().enumerate() {
        let g_in = if j < att { j as f32 / att as f32 } else { 1.0 };
        let g_out = if j + rel > n { (n - j) as f32 / rel as f32 } else { 1.0 };
        *v *= g_in * g_out;
    }
    let pk = y.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if pk > 1e-6 {
        for v in y.iter_mut() {
            *v *= 0.8 / pk;
        }
    }
    y
}

fn save(e: &mut Engine, name: &str, data: &[f32], source: String) -> Result<Value> {
    let name = crate::samples::sample_name(name);
    let path = e.samples_dir().join(format!("{name}.wav"));
    crate::render::write_wav(&path, data, data)?;
    crate::tools::register_sample(e, SampleInfo { name, path: path.to_string_lossy().to_string(), source, license: "own (beatbox)".into(), author: "beatbox".into(), duration: 0.0 })
}

fn shape_from(a: &Value) -> Shape {
    Shape {
        pitch_st: f_or(a, "pitch_semitones", 0.0).clamp(-36.0, 36.0),
        keep_formants: a["keep_formants"].as_bool().unwrap_or(false),
        partials: u_or(a, "partials", 64).clamp(1, 512) as usize,
        blur: f_or(a, "blur", 0.0).clamp(0.0, 1.0),
        tilt_db_oct: f_or(a, "tilt_db_oct", 0.0).clamp(-12.0, 12.0),
        odd_even: f_or(a, "odd_even", 0.0).clamp(-1.0, 1.0),
    }
}

fn resynthesize_sample(e: &mut Engine, a: &Value) -> Result<Value> {
    let (info, d) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
    if d.len() < N {
        bail!("sample too short to analyse (needs ~50 ms)");
    }
    if d.len() as f32 > 60.0 * SR {
        bail!("sample longer than 60 s; trim it first (edit_sample)");
    }
    let s = shape_from(a);
    let y = resynthesize(&d, &s);
    let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_resynth", info.name));
    let what = format!(
        "additive resynthesis: {} partials, pitch {:+} st{}, blur {}, tilt {:+} dB/oct, odd/even {:+}",
        s.partials, s.pitch_st, if s.keep_formants { " (formants kept)" } else { "" }, s.blur, s.tilt_db_oct, s.odd_even
    );
    let mut r = save(e, &nn, &y, format!("{} ({what})", info.source))?;
    r["partials"] = json!(s.partials);
    r["seconds"] = json!((y.len() as f32 / SR * 100.0).round() / 100.0);
    Ok(r)
}

fn analyze_partials(e: &mut Engine, a: &Value) -> Result<Value> {
    let (info, d) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
    let at = f_or(a, "at_s", 0.0).max(0.0);
    let k = u_or(a, "partials", 16).clamp(1, 128) as usize;
    let i0 = ((at * SR) as usize).min(d.len().saturating_sub(1));
    let seg = &d[i0..(i0 + N * 4).min(d.len())];
    let frames = analyze(seg, k);
    let fr = frames.get(frames.len() / 2).cloned().unwrap_or_default();
    let top = fr.peaks.iter().map(|p| p.1).fold(1e-9f32, f32::max);
    let f0 = frame_f0(&fr);
    Ok(json!({
        "sample": info.name,
        "at_s": at,
        "f0_hz": f0.map(|f| (f * 10.0).round() / 10.0),
        "partials": fr.peaks.iter().map(|(hz, amp)| json!({
            "hz": (hz * 10.0).round() / 10.0,
            "db": (20.0 * (amp / top).log10() * 10.0).round() / 10.0,
            "harmonic": f0.map(|f| ((hz / f) * 100.0).round() / 100.0),
        })).collect::<Vec<_>>(),
        "next": "resynthesize_sample {sample, partials, pitch_semitones, keep_formants, blur, tilt_db_oct, odd_even}",
    }))
}

fn additive_sample(e: &mut Engine, a: &Value) -> Result<Value> {
    let note = s_opt(a, "note").unwrap_or_else(|| "C3".into());
    let midi = crate::theory::parse_note(&note)? as f32;
    let hz = 440.0 * 2f32.powf((midi - 69.0) / 12.0);
    let n = u_or(a, "partials", 32) as usize;
    let mut amps = if let Some(arr) = a["harmonics"].as_array() {
        arr.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect::<Vec<_>>()
    } else {
        recipe(&s_opt(a, "recipe").unwrap_or_else(|| "saw".into()), n)?
    };
    if amps.is_empty() || amps.iter().all(|x| x.abs() < 1e-6) {
        bail!("no harmonic has any level");
    }
    // the same shaping as resynthesis, on the recipe
    let tilt = f_or(a, "tilt_db_oct", 0.0);
    let oe = f_or(a, "odd_even", 0.0).clamp(-1.0, 1.0);
    for (i, x) in amps.iter_mut().enumerate() {
        let h = (i + 1) as f32;
        if tilt != 0.0 {
            *x *= 10f32.powf(tilt * h.log2() / 20.0);
        }
        if oe != 0.0 && i > 0 {
            *x *= if (i + 1) % 2 == 0 { (1.0 + oe).min(1.0) } else { (1.0 - oe).min(1.0) };
        }
    }
    let secs = f_or(a, "seconds", 2.0);
    let y = additive_tone(hz, &amps, secs, f_or(a, "inharmonicity", 0.0).clamp(0.0, 0.01), f_or(a, "decay_s", 0.0).max(0.0), f_or(a, "attack_s", 0.01), f_or(a, "release_s", 0.2));
    let name = s_opt(a, "name").unwrap_or_else(|| format!("additive_{}_{}", s_opt(a, "recipe").unwrap_or_else(|| "custom".into()), note.replace('#', "s")));
    let mut r = save(e, &name, &y, format!("additive synthesis by beatbox: {note}, {} harmonics", amps.len()))?;
    r["hz"] = json!((hz * 100.0).round() / 100.0);
    r["next"] = json!("add_track {preset:'sampler'} with this sample, or resynthesize_sample to blur/morph it");
    Ok(r)
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "resynthesize_sample",
            description: "Additive resynthesis (FL Harmor/Morphine) into a new sample: the sample is turned into its sine partials and played back from them, shaped on the way. partials = how many of the strongest to keep (64 default; 8-16 = glassy, thin), pitch_semitones re-pitches (keep_formants:true keeps a voice's vowel colour), blur 0..1 smears it in time into a frozen pad, tilt_db_oct brightens (+) or darkens (-), odd_even -1 (odd only, hollow) .. +1 (even only). new_name defaults to <sample>_resynth.",
            mutates: true,
            schema: || obj(json!({
                "sample": {"type": "string"},
                "partials": {"type": "integer"},
                "pitch_semitones": {"type": "number"},
                "keep_formants": {"type": "boolean"},
                "blur": {"type": "number"},
                "tilt_db_oct": {"type": "number"},
                "odd_even": {"type": "number"},
                "new_name": {"type": "string"}
            }), &["sample"]),
            run: resynthesize_sample,
        },
        Tool {
            name: "analyze_partials",
            description: "List a sample's strongest sine partials at a moment (at_s): Hz, level in dB under the strongest, and harmonic number over the detected fundamental. Use it to see what resynthesize_sample will work with, or whether a sound is harmonic (integers) or bell-like.",
            mutates: false,
            schema: || obj(json!({"sample": {"type": "string"}, "at_s": {"type": "number"}, "partials": {"type": "integer"}}), &["sample"]),
            run: analyze_partials,
        },
        Tool {
            name: "additive_sample",
            description: "Additive synthesis (FL Harmor's harmonic editor) into a new sample: a note built from harmonics. recipe = saw, square, triangle, sine, organ, choir, bell or hollow (or give harmonics: [amp of h1, h2, ...]); partials (32); tilt_db_oct, odd_even as in resynthesize_sample; inharmonicity 0..0.01 stretches partials (piano/bell); decay_s makes upper partials die first (a pluck; 0 = sustained); seconds, attack_s, release_s.",
            mutates: true,
            schema: || obj(json!({
                "note": {"type": "string", "description": "e.g. C3 or a MIDI number"},
                "recipe": {"type": "string", "enum": ["saw", "square", "triangle", "sine", "organ", "choir", "bell", "hollow"]},
                "harmonics": {"type": "array", "items": {"type": "number"}},
                "partials": {"type": "integer"},
                "tilt_db_oct": {"type": "number"},
                "odd_even": {"type": "number"},
                "inharmonicity": {"type": "number"},
                "decay_s": {"type": "number"},
                "seconds": {"type": "number"},
                "attack_s": {"type": "number"},
                "release_s": {"type": "number"},
                "name": {"type": "string"}
            }), &[]),
            run: additive_sample,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saw(hz: f32, secs: f32) -> Vec<f32> {
        let amps = recipe("saw", 20).unwrap();
        additive_tone(hz, &amps, secs, 0.0, 0.0, 0.01, 0.05)
    }

    fn pitch(x: &[f32]) -> f32 {
        crate::speech_song::median_pitch(x)
    }

    #[test]
    fn analysis_finds_the_harmonics() {
        let x = saw(220.0, 0.5);
        let fr = analyze(&x, 8);
        let mid = &fr[fr.len() / 2];
        let f0 = frame_f0(mid).unwrap();
        assert!((f0 - 220.0).abs() < 3.0, "f0 {f0}");
        let mut hz: Vec<f32> = mid.peaks.iter().map(|p| p.0).collect();
        hz.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for (i, h) in hz.iter().take(4).enumerate() {
            assert!((h / 220.0 - (i + 1) as f32).abs() < 0.03, "{hz:?}");
        }
    }

    #[test]
    fn resynthesis_keeps_pitch_and_repitches() {
        let x = saw(220.0, 0.6);
        let y = resynthesize(&x, &Shape::default());
        assert_eq!(y.len(), x.len());
        let a = (0.15 * SR) as usize;
        let b = (0.45 * SR) as usize;
        let p0 = pitch(&x[a..b]);
        let p1 = pitch(&y[a..b]);
        assert!((p0 - p1).abs() < 0.3, "{p0} vs {p1}");
        let up = resynthesize(&x, &Shape { pitch_st: 7.0, ..Default::default() });
        let p2 = pitch(&up[a..b]);
        assert!((p2 - p0 - 7.0).abs() < 0.4, "{p0} -> {p2}");
        // odd only: the 2nd harmonic is gone
        let odd = resynthesize(&x, &Shape { odd_even: -1.0, ..Default::default() });
        let fr = analyze(&odd[a..b], 12);
        let m = &fr[fr.len() / 2];
        let top = m.peaks.iter().map(|p| p.1).fold(0.0f32, f32::max);
        let h2 = m.peaks.iter().filter(|p| (p.0 - 440.0).abs() < 8.0).map(|p| p.1).fold(0.0f32, f32::max);
        assert!(h2 < top * 0.05, "h2 {h2} top {top}");
        // blur rings on past the end
        let bl = resynthesize(&x, &Shape { blur: 0.8, ..Default::default() });
        assert!(bl.len() > x.len());
    }

    #[test]
    fn recipes_and_tools() {
        assert!(recipe("nope", 8).is_err());
        let sq = recipe("square", 6).unwrap();
        assert_eq!(sq[1], 0.0);
        assert!((sq[2] - 1.0 / 3.0).abs() < 1e-6);
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_resynth_tests"));
        e.call("new_project", &json!({"bpm": 100, "patterns": [{"name": "a", "bars": 1}]})).unwrap();
        let r = e.call("additive_sample", &json!({"note": "A3", "recipe": "organ", "seconds": 0.6, "name": "org"})).unwrap();
        assert!((r["hz"].as_f64().unwrap() - 220.0).abs() < 0.1, "{r}");
        let p = e.call("analyze_partials", &json!({"sample": "org", "at_s": 0.2, "partials": 6})).unwrap();
        assert!((p["f0_hz"].as_f64().unwrap() - 220.0).abs() < 3.0, "{p}");
        let s = e.call("resynthesize_sample", &json!({"sample": "org", "partials": 8, "pitch_semitones": -12, "new_name": "org_low"})).unwrap();
        assert!(e.project.samples.iter().any(|x| x.name == "org_low"), "{s}");
    }
}
