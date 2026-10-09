//! AI ears v3: stereo image per band, punch / transient metrics, the vocal
//! pocket for rap (with a proxy vocal when there is no vocal track) and
//! reference_match v2 (loudness-matched, section-aligned, ordered actions).
//!
//! Every analysis works on plain stereo buffers so the tools can run it on
//! the mix, one soloed track, a snapshot or a file.

use crate::analysis;
use crate::dsp::{gain_to_db, Rng, SR};
use crate::project::Project;
use serde_json::{json, Value};
use std::f32::consts::PI;

fn r1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}
fn r2(x: f32) -> f32 {
    (x * 100.0).round() / 100.0
}
fn db(x: f64) -> f32 {
    (10.0 * x.max(1e-12).log10()) as f32
}

// ---------------------------------------------------------------- filters

/// RBJ biquad (direct form I).
#[derive(Clone, Copy)]
pub struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    x: [f32; 2],
    y: [f32; 2],
}

impl Biquad {
    fn new(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Biquad {
            b: [b0 / a0, b1 / a0, b2 / a0],
            a: [a1 / a0, a2 / a0],
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }
    pub fn lowpass(f: f32) -> Self {
        let w = 2.0 * PI * f.min(SR * 0.45) / SR;
        let (c, al) = (w.cos(), w.sin() / (2.0 * std::f32::consts::FRAC_1_SQRT_2));
        Self::new(
            (1.0 - c) / 2.0,
            1.0 - c,
            (1.0 - c) / 2.0,
            1.0 + al,
            -2.0 * c,
            1.0 - al,
        )
    }
    pub fn highpass(f: f32) -> Self {
        let w = 2.0 * PI * f.max(5.0) / SR;
        let (c, al) = (w.cos(), w.sin() / (2.0 * std::f32::consts::FRAC_1_SQRT_2));
        Self::new(
            (1.0 + c) / 2.0,
            -(1.0 + c),
            (1.0 + c) / 2.0,
            1.0 + al,
            -2.0 * c,
            1.0 - al,
        )
    }
    #[inline]
    pub fn run(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// Band-limit a signal to [lo, hi] Hz (24 dB/oct each side; lo <= 20 or
/// hi >= 20000 leaves that side open).
pub fn band(x: &[f32], lo: f32, hi: f32) -> Vec<f32> {
    let mut f: Vec<Biquad> = Vec::new();
    if lo > 20.0 {
        f.push(Biquad::highpass(lo));
        f.push(Biquad::highpass(lo));
    }
    if hi < 20000.0 {
        f.push(Biquad::lowpass(hi));
        f.push(Biquad::lowpass(hi));
    }
    x.iter()
        .map(|&s| f.iter_mut().fold(s, |acc, b| b.run(acc)))
        .collect()
}

// ---------------------------------------------------------------- stereo image

pub const IMAGE_BANDS: [(f32, f32); 5] = [
    (20.0, 120.0),
    (120.0, 500.0),
    (500.0, 2000.0),
    (2000.0, 8000.0),
    (8000.0, 20000.0),
];

#[derive(Debug, Clone, Copy, Default)]
pub struct Image {
    pub correlation: f32,
    /// side RMS / mid RMS
    pub width: f32,
    /// L - R energy in dB
    pub balance_db: f32,
    /// energy lost when the two channels are summed to mono (0 = none)
    pub mono_loss_db: f32,
    /// band energy relative to full scale, dB (to know if the band matters)
    pub level_db: f32,
}

pub fn image(l: &[f32], r: &[f32]) -> Image {
    let n = l.len().min(r.len());
    let (mut ll, mut rr, mut lr, mut mm, mut ss) = (0f64, 0f64, 0f64, 0f64, 0f64);
    for i in 0..n {
        let (a, b) = (l[i] as f64, r[i] as f64);
        ll += a * a;
        rr += b * b;
        lr += a * b;
        mm += (0.5 * (a + b)).powi(2);
        ss += (0.5 * (a - b)).powi(2);
    }
    if n == 0 || ll + rr < 1e-12 {
        return Image {
            correlation: 1.0,
            level_db: -120.0,
            ..Default::default()
        };
    }
    Image {
        correlation: (lr / (ll * rr).sqrt().max(1e-12)) as f32,
        width: (ss / mm.max(1e-12)).sqrt() as f32,
        balance_db: db(ll / rr.max(1e-12)),
        // mono = (l+r)/2 per channel -> 2*mm vs ll+rr
        mono_loss_db: db(2.0 * mm / (ll + rr)).min(0.0),
        level_db: db((ll + rr) / (2 * n) as f64),
    }
}

/// Windows (start_s, end_s) where the low/mid content is out of phase.
pub fn phase_issues(l: &[f32], r: &[f32]) -> Vec<(f32, f32, f32)> {
    let lo = band(l, 20.0, 500.0);
    let ro = band(r, 20.0, 500.0);
    let win = (0.4 * SR) as usize;
    let mut out: Vec<(f32, f32, f32)> = Vec::new();
    let mut i = 0;
    while i + win <= lo.len().min(ro.len()) {
        let im = image(&lo[i..i + win], &ro[i..i + win]);
        if im.correlation < 0.0 && im.level_db > -45.0 {
            let t0 = i as f32 / SR;
            match out.last_mut() {
                Some(x) if (x.1 - t0).abs() < 0.01 => {
                    x.1 = t0 + 0.4;
                    x.2 = x.2.min(im.correlation)
                }
                _ => out.push((t0, t0 + 0.4, im.correlation)),
            }
        }
        i += win;
    }
    out
}

pub fn stereo_image(
    l: &[f32],
    r: &[f32],
    sections: &[(String, usize, usize)],
    tracks_hint: &[(String, f32)],
) -> Value {
    let mut bands = Vec::new();
    let mut findings: Vec<Value> = Vec::new();
    let mut low_ok = true;
    for (lo, hi) in IMAGE_BANDS {
        let bl = band(l, lo, hi);
        let br = band(r, lo, hi);
        let im = image(&bl, &br);
        if lo < 100.0 && im.level_db > -50.0 && im.correlation < 0.9 {
            low_ok = false;
        }
        bands.push(json!({"band_hz": [lo, hi], "correlation": r2(im.correlation), "width": r2(im.width), "balance_db": r1(im.balance_db), "mono_loss_db": r1(im.mono_loss_db), "level_db": r1(im.level_db)}));
        if im.level_db > -50.0 && im.balance_db.abs() > 1.5 {
            findings.push(json!({"severity": 0.4, "message": format!("{:.0}-{:.0} Hz leans {} by {:.1} dB", lo, hi, if im.balance_db > 0.0 {"left"} else {"right"}, im.balance_db.abs()), "suggested_call": {"tool": "list_routing", "args": {}}}));
        }
        if lo >= 500.0 && im.level_db > -50.0 && im.mono_loss_db < -3.0 {
            findings.push(json!({"severity": 0.5, "message": format!("{:.0}-{:.0} Hz loses {:.1} dB in mono (too wide / phasey)", lo, hi, -im.mono_loss_db), "suggested_call": {"tool": "add_effect", "args": {"track": "master", "type": "width", "params": {"amount": 0.85}}}}));
        }
    }
    if !low_ok {
        // the stereo-spread culprit in the low end, if we know it
        let culprit = tracks_hint
            .iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .map(|t| t.0.clone());
        findings.push(json!({"severity": 0.7, "message": "the low end (< 120 Hz) is not mono: it collapses on club systems and phones", "suggested_call": {"tool": "add_effect", "args": {"track": culprit.unwrap_or_else(|| "bass".into()), "type": "width", "params": {"amount": 0.0}}}}));
    }
    let pi = phase_issues(l, r);
    if !pi.is_empty() {
        findings.push(json!({"severity": 0.6, "message": format!("{} window(s) where the lows/mids are out of phase (correlation < 0), first at {:.1} s", pi.len(), pi[0].0), "suggested_call": {"tool": "spectrum", "args": {"start_s": pi[0].0, "end_s": pi[0].1}}}));
    }
    let per_section: Vec<Value> = sections
        .iter()
        .map(|(name, s0, s1)| {
            let im = image(&l[*s0..*s1], &r[*s0..*s1]);
            json!({"section": name, "correlation": r2(im.correlation), "width": r2(im.width)})
        })
        .collect();
    let full = image(l, r);
    json!({
        "overall": {"correlation": r2(full.correlation), "width": r2(full.width), "balance_db": r1(full.balance_db), "mono_loss_db": r1(full.mono_loss_db)},
        "bands": bands,
        "low_end_mono_ok": low_ok,
        "phase_issues": pi.iter().take(10).map(|x| json!({"start_s": r2(x.0), "end_s": r2(x.1), "correlation": r2(x.2)})).collect::<Vec<_>>(),
        "per_section": per_section,
        "findings": findings,
    })
}

// ---------------------------------------------------------------- punch

/// 1 ms RMS envelope (dB).
fn env_db_1ms(x: &[f32]) -> Vec<f32> {
    let hop = (0.001 * SR) as usize;
    x.chunks(hop)
        .map(|c| {
            let e: f64 = c.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / c.len() as f64;
            db(e)
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct Punch {
    pub hits: usize,
    pub attack_ms: f32,
    pub transient_to_sustain_db: f32,
    pub micro_crest_db: f32,
    /// level of the first 10 ms over the median of the surrounding 200 ms
    pub punch_index_db: f32,
}

fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// Punch of a buffer measured at known onsets (sample positions).
pub fn punch_at(l: &[f32], r: &[f32], onsets: &[usize]) -> Punch {
    let n = l.len().min(r.len());
    let mono: Vec<f32> = (0..n).map(|i| 0.5 * (l[i] + r[i])).collect();
    let env = env_db_1ms(&mono);
    let (mut att, mut tts, mut crest, mut idx) = (vec![], vec![], vec![], vec![]);
    for &o in onsets {
        let k = o / (0.001 * SR) as usize;
        if k + 160 >= env.len() || k < 100 {
            continue;
        }
        // attack: 10% -> 90% of the peak amplitude in the first 30 ms
        let w = &env[k..k + 30];
        let (pi, pk) =
            w.iter().enumerate().fold(
                (0, -200.0f32),
                |a, (i, v)| if *v > a.1 { (i, *v) } else { a },
            );
        let lin: Vec<f32> = w.iter().map(|d| 10f32.powf(d / 20.0)).collect();
        let p = 10f32.powf(pk / 20.0);
        let t10 = lin.iter().position(|v| *v >= 0.1 * p).unwrap_or(0);
        let t90 = lin.iter().position(|v| *v >= 0.9 * p).unwrap_or(pi);
        att.push((t90.saturating_sub(t10)) as f32);
        let peak15 = env[k..k + 15].iter().cloned().fold(-200.0, f32::max);
        let sus: f64 = env[k + 30..k + 150]
            .iter()
            .map(|d| 10f64.powf(*d as f64 / 10.0))
            .sum::<f64>()
            / 120.0;
        tts.push(peak15 - db(sus));
        // micro crest over 50 ms: sample peak vs RMS
        let s0 = o;
        let s1 = (o + (0.05 * SR) as usize).min(n);
        let pkv = mono[s0..s1].iter().fold(0.0f32, |a, v| a.max(v.abs()));
        let rms = (mono[s0..s1]
            .iter()
            .map(|v| (*v as f64).powi(2))
            .sum::<f64>()
            / (s1 - s0).max(1) as f64)
            .sqrt();
        crest.push(gain_to_db(pkv) - gain_to_db(rms as f32));
        let head: f64 = env[k..k + 10]
            .iter()
            .map(|d| 10f64.powf(*d as f64 / 10.0))
            .sum::<f64>()
            / 10.0;
        let mut around: Vec<f32> = env[k - 100..k + 100].to_vec();
        idx.push(db(head) - median(&mut around));
    }
    let m = |v: &mut Vec<f32>| median(v);
    Punch {
        hits: att.len(),
        attack_ms: m(&mut att),
        transient_to_sustain_db: r1(m(&mut tts)),
        micro_crest_db: r1(m(&mut crest)),
        punch_index_db: r1(m(&mut idx)),
    }
}

/// Onsets per drum role (kick / snare) in whole-song sample positions.
pub fn role_onsets(p: &Project, roles: &[&str]) -> Vec<usize> {
    let (events, _) = crate::render::schedule(p, &Default::default());
    let mut out = Vec::new();
    for (t, ev) in p.tracks.iter().zip(events.iter()) {
        let role = crate::tools_mix::role_of(&t.name, &t.instrument);
        if roles.contains(&role) && !t.mute {
            out.extend(ev.iter().map(|e| e.start));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

pub fn punch_json(name: &str, p: &Punch) -> Value {
    json!({"source": name, "hits": p.hits, "attack_ms": p.attack_ms, "transient_to_sustain_db": p.transient_to_sustain_db, "micro_crest_db": p.micro_crest_db, "punch_index_db": p.punch_index_db})
}

/// Findings for master punch (+ optional pre-master comparison).
pub fn punch_findings(master: &Punch, pre: Option<&Punch>, drum_bus: bool) -> Vec<Value> {
    let mut f = Vec::new();
    if master.hits > 0 && master.punch_index_db < 4.0 {
        let track = if drum_bus { "drums" } else { "kick" };
        f.push(json!({"severity": 0.6, "message": format!("kick/snare barely poke out of the mix (punch index {:.1} dB, aim for 5+)", master.punch_index_db), "suggested_call": {"tool": "add_effect", "args": {"track": track, "type": "transient", "params": {"attack": 0.4, "sustain": -0.1}}}}));
    }
    if let Some(pre) = pre {
        let loss = pre.transient_to_sustain_db - master.transient_to_sustain_db;
        if loss > 2.0 {
            f.push(json!({"severity": 0.55, "message": format!("the master chain flattens the drum transients by {loss:.1} dB (limiter/compressor too hard)"), "suggested_call": {"tool": "master_assistant", "args": {"target_lufs": -11.0}}}));
        }
    }
    if master.hits > 0 && master.transient_to_sustain_db < 3.0 {
        f.push(json!({"severity": 0.4, "message": format!("drum hits are mostly sustain ({:.1} dB transient over body): they read as thuds", master.transient_to_sustain_db), "suggested_call": {"tool": "add_effect", "args": {"track": "kick", "type": "transient", "params": {"attack": 0.5}}}}));
    }
    f
}

// ---------------------------------------------------------------- vocal pocket

pub const ZONES: [(&str, f32, f32); 3] = [
    ("body", 200.0, 500.0),
    ("intelligibility", 1000.0, 4000.0),
    ("sibilance", 5000.0, 8000.0),
];

/// A proxy rap vocal: speech-shaped noise (body + formant region, tilted
/// like a voice), gated by a 16th-note flow at the project tempo. The level
/// is set per call relative to the beat. Unvalidated design (see research
/// doc §4.7): use it to compare beats with each other, not as truth.
pub fn proxy_vocal(n: usize, bpm: f32, seed: u64) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let raw: Vec<f32> = (0..n).map(|_| rng.bipolar()).collect();
    // speech spectrum: 150 Hz - 6 kHz, emphasis around 300 Hz and 1-3 kHz
    let body = band(&raw, 150.0, 600.0);
    let form = band(&raw, 900.0, 3500.0);
    let sib = band(&raw, 4500.0, 7500.0);
    let step = (60.0 / bpm / 4.0 * SR) as usize;
    // a flow: syllables on most 16ths, breaths every bar end
    let pattern = [
        1.0, 0.7, 1.0, 0.0, 1.0, 0.8, 0.9, 0.6, 1.0, 0.7, 1.0, 0.0, 1.0, 0.9, 0.0, 0.0,
    ];
    let mut out = vec![0.0f32; n];
    for i in 0..n {
        let s = i / step.max(1);
        let ph = (i % step.max(1)) as f32 / step.max(1) as f32;
        let gate = pattern[s % 16] * (PI * ph).sin().max(0.0).powf(0.6);
        out[i] = gate * (1.0 * body[i] + 0.8 * form[i] + 0.25 * sib[i]);
    }
    out
}

fn zone_energy(x: &[f32], lo: f32, hi: f32) -> f64 {
    let b = band(x, lo, hi);
    b.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / b.len().max(1) as f64
}

/// Masking of a vocal by the beat in one zone: mean over 50 ms frames
/// (where the vocal sounds) of the probability the beat covers it, from
/// the beat-to-vocal ratio (0 dB -> 0.5, +6 dB -> ~0.9).
fn zone_masking(beat: &[f32], vocal: &[f32], lo: f32, hi: f32) -> (f32, f32) {
    let b = band(beat, lo, hi);
    let v = band(vocal, lo, hi);
    let hop = (0.05 * SR) as usize;
    let (mut acc, mut cnt, mut ratio) = (0.0f32, 0usize, 0.0f32);
    for (cb, cv) in b.chunks(hop).zip(v.chunks(hop)) {
        let eb: f64 = cb.iter().map(|x| (*x as f64).powi(2)).sum();
        let ev: f64 = cv.iter().map(|x| (*x as f64).powi(2)).sum();
        if ev < 1e-9 * hop as f64 {
            continue;
        }
        let d = db(eb / ev.max(1e-12));
        acc += 1.0 / (1.0 + (-d / 2.7).exp());
        ratio += d;
        cnt += 1;
    }
    if cnt == 0 {
        (0.0, -60.0)
    } else {
        (acc / cnt as f32, ratio / cnt as f32)
    }
}

/// Scale `v` so its (K-weighted) loudness sits `rel_lu` relative to `beat`.
pub fn level_match(beat_l: &[f32], beat_r: &[f32], v: &mut [f32], rel_lu: f32) {
    let lb = analysis::loudness(beat_l, beat_r).integrated_lufs;
    let lv = analysis::loudness(v, v).integrated_lufs;
    if lb.is_finite() && lv.is_finite() && lv > -90.0 {
        let g = 10f32.powf((lb + rel_lu - lv) / 20.0);
        v.iter_mut().for_each(|x| *x *= g);
    }
}

pub struct PocketSection {
    pub name: String,
    pub occupancy_db: Vec<(String, f32)>,
    pub masked: Vec<(String, f32)>,
    pub free_space: f32,
}

pub fn vocal_pocket_section(name: &str, bl: &[f32], br: &[f32], vocal: &[f32]) -> PocketSection {
    let mono: Vec<f32> = bl.iter().zip(br).map(|(a, b)| 0.5 * (a + b)).collect();
    let total: f64 =
        mono.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / mono.len().max(1) as f64;
    let mut occ = Vec::new();
    let mut masked = Vec::new();
    let mut intel = 0.5;
    for (z, lo, hi) in ZONES {
        occ.push((
            z.to_string(),
            r1(db(zone_energy(&mono, lo, hi) / total.max(1e-12))),
        ));
        let (m, _) = zone_masking(&mono, vocal, lo, hi);
        if z == "intelligibility" {
            intel = m;
        }
        masked.push((z.to_string(), r2(m)));
    }
    PocketSection {
        name: name.to_string(),
        occupancy_db: occ,
        masked,
        free_space: r1(100.0 * (1.0 - intel)),
    }
}

// ---------------------------------------------------------------- reference match v2

/// Short-term (3 s window, 1 s hop) loudness series.
pub fn st_series(l: &[f32], r: &[f32]) -> Vec<f32> {
    let win = (3.0 * SR) as usize;
    let hop = SR as usize;
    let mut out = Vec::new();
    let mut i = 0;
    while i + win <= l.len().min(r.len()) {
        out.push(analysis::loudness(&l[i..i + win], &r[i..i + win]).integrated_lufs);
        i += hop;
    }
    out
}

/// Pick the reference's "hook" (loudest sustained part) and "verse"
/// (median-loud part) windows, each `len_s` long. Returns sample ranges.
pub fn ref_windows(l: &[f32], r: &[f32], len_s: f32) -> Vec<(String, usize, usize)> {
    let st = st_series(l, r);
    let n = l.len().min(r.len());
    let w = (len_s.max(4.0) as usize).max(3);
    if st.len() <= w {
        return vec![("whole".into(), 0, n)];
    }
    // mean loudness of each w-second window starting at each second
    let means: Vec<(usize, f32)> = (0..st.len() - w + 1)
        .map(|i| (i, st[i..i + w].iter().sum::<f32>() / w as f32))
        .collect();
    let mut sorted = means.clone();
    sorted.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    let hook = sorted.last().unwrap().0;
    let verse = sorted[sorted.len() / 2].0;
    let to = |s: usize| {
        (
            (s as f32 * SR) as usize,
            (((s + w) as f32 + 2.0) * SR) as usize,
        )
    };
    let (h0, h1) = to(hook);
    let (v0, v1) = to(verse);
    vec![
        ("hook".into(), h0.min(n), h1.min(n)),
        ("verse".into(), v0.min(n), v1.min(n)),
    ]
}

/// Six tonal bands in dB relative to the window's total (loudness-matched
/// by construction: only the shape is compared).
pub const TONAL: [(&str, f32, f32); 6] = [
    ("sub", 20.0, 60.0),
    ("bass", 60.0, 250.0),
    ("low_mid", 250.0, 800.0),
    ("mid", 800.0, 3000.0),
    ("presence", 3000.0, 8000.0),
    ("air", 8000.0, 20000.0),
];

pub fn tonal_shape(l: &[f32], r: &[f32]) -> Vec<f32> {
    let mono: Vec<f32> = l.iter().zip(r).map(|(a, b)| 0.5 * (a + b)).collect();
    let es: Vec<f64> = TONAL
        .iter()
        .map(|(_, lo, hi)| zone_energy(&mono, *lo, *hi))
        .collect();
    let tot: f64 = es.iter().sum::<f64>().max(1e-12);
    es.iter().map(|e| db(e / tot)).collect()
}

/// Peak-to-short-term-loudness ratio (PSR, dB): true-peak-ish sample peak
/// over the 3 s short-term max.
pub fn psr(l: &[f32], r: &[f32]) -> f32 {
    let pk = l.iter().chain(r).fold(0.0f32, |a, v| a.max(v.abs()));
    let st = st_series(l, r);
    let mx = st.iter().cloned().fold(-120.0f32, f32::max);
    if st.is_empty() {
        return 0.0;
    }
    gain_to_db(pk) - mx
}

/// Onsets from a spectral-flux-like broadband envelope (for punch on a
/// reference file, where there are no notes).
pub fn audio_onsets(l: &[f32], r: &[f32]) -> Vec<usize> {
    let mono: Vec<f32> = l.iter().zip(r).map(|(a, b)| 0.5 * (a + b)).collect();
    let env = env_db_1ms(&mono);
    let mut out = Vec::new();
    let mut last = 0usize;
    for k in 20..env.len() {
        let base = env[k - 20..k - 2].iter().cloned().fold(-200.0f32, f32::max);
        if env[k] > -40.0 && env[k] - base > 6.0 && k - last > 90 {
            out.push(k * (0.001 * SR) as usize);
            last = k;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f32, n: usize, a: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * PI * f * i as f32 / SR).sin() * a)
            .collect()
    }

    #[test]
    fn image_identities() {
        let x = sine(440.0, 44100, 0.5);
        let neg: Vec<f32> = x.iter().map(|v| -v).collect();
        let a = image(&x, &x);
        assert!(a.correlation > 0.99 && a.width < 0.01 && a.mono_loss_db > -0.1);
        let b = image(&x, &neg);
        assert!(b.correlation < -0.99 && b.mono_loss_db < -40.0);
        let mut rng = Rng::new(3);
        let n1: Vec<f32> = (0..44100).map(|_| rng.bipolar()).collect();
        let n2: Vec<f32> = (0..44100).map(|_| rng.bipolar()).collect();
        let c = image(&n1, &n2);
        assert!(c.correlation.abs() < 0.05);
        assert!((c.mono_loss_db + 3.0).abs() < 0.5, "{}", c.mono_loss_db);
    }

    #[test]
    fn stereo_low_end_flagged() {
        // a sub that is out of phase between channels must fail the mono check
        let s = sine(55.0, 88200, 0.5);
        let neg: Vec<f32> = s.iter().map(|v| -v * 0.8).collect();
        let v = stereo_image(&s, &neg, &[], &[("bass".into(), 1.0)]);
        assert_eq!(v["low_end_mono_ok"], false);
        assert!(!v["phase_issues"].as_array().unwrap().is_empty());
        let ok = stereo_image(&s, &s, &[], &[]);
        assert_eq!(ok["low_end_mono_ok"], true);
    }

    #[test]
    fn punch_separates_tight_from_squashed() {
        // decaying hits every 0.5 s vs the same hits with a constant bed
        let n = (SR * 4.0) as usize;
        let mut tight = vec![0.0f32; n];
        let step = (SR * 0.5) as usize;
        let onsets: Vec<usize> = (1..7).map(|k| k * step).collect();
        for &o in &onsets {
            for j in 0..step.min(n - o) {
                let t = j as f32 / SR;
                tight[o + j] += (2.0 * PI * 60.0 * t).sin() * (-t * 30.0).exp() * 0.8;
            }
        }
        let bed = sine(200.0, n, 0.3);
        let squashed: Vec<f32> = tight.iter().zip(&bed).map(|(a, b)| a * 0.5 + b).collect();
        let a = punch_at(&tight, &tight, &onsets);
        let b = punch_at(&squashed, &squashed, &onsets);
        assert!(a.hits >= 5);
        assert!(a.punch_index_db > b.punch_index_db + 3.0, "{a:?} {b:?}");
        assert!(a.transient_to_sustain_db > b.transient_to_sustain_db);
        let found = audio_onsets(&tight, &tight);
        assert!(found.len() >= 5, "{}", found.len());
    }

    #[test]
    fn pocket_sees_a_busy_midrange() {
        let n = (SR * 3.0) as usize;
        let v = {
            let mut v = proxy_vocal(n, 90.0, 1);
            let quiet = sine(60.0, n, 0.3);
            level_match(&quiet, &quiet, &mut v, -3.0);
            v
        };
        let empty = sine(60.0, n, 0.3);
        let mut rng = Rng::new(5);
        let busy_noise: Vec<f32> = (0..n).map(|_| rng.bipolar()).collect();
        let busy_mid = band(&busy_noise, 1000.0, 4000.0);
        let busy: Vec<f32> = empty
            .iter()
            .zip(&busy_mid)
            .map(|(a, b)| a + 2.0 * b)
            .collect();
        let a = vocal_pocket_section("a", &empty, &empty, &v);
        let b = vocal_pocket_section("b", &busy, &busy, &v);
        assert!(
            a.free_space > b.free_space + 20.0,
            "{} vs {}",
            a.free_space,
            b.free_space
        );
    }

    #[test]
    fn tonal_shape_tracks_brightness() {
        let mut rng = Rng::new(9);
        let x: Vec<f32> = (0..44100).map(|_| rng.bipolar()).collect();
        let dark = band(&x, 20.0, 800.0);
        let a = tonal_shape(&x, &x);
        let b = tonal_shape(&dark, &dark);
        assert!(a[5] > b[5] + 10.0);
        let w = ref_windows(&x, &x, 8.0);
        assert!(!w.is_empty());
    }
}
