//! Vocal resonance finder for /mix/vocal_resonance (C9): narrow, lasting
//! peaks in a vocal's long-term spectrum (a phone's or a room's ring, a
//! harsh formant) that a self-keyed dynamic EQ band can tame only when they
//! poke out. Analysis only: the bands are the project's `dynamic_eq`.

use crate::analysis::fft;
use crate::dsp::{Biquad, BiquadKind, SR};
use crate::fx::{DynamicEqFx, Effect};
use serde_json::{json, Value};

/// The tamer's bands on a vocal's chain are "auto_resonance1".."4": how a
/// rebuild finds them and takes them out.
pub const AUTO_RESONANCE_ID: &str = "auto_resonance";

/// Most bands one vocal gets.
pub const MAX_BANDS: usize = 4;
/// The bands' Q, most dip, ratio and time constants.
pub const RES_Q: f32 = 5.0;
pub const RES_RANGE_DB: f32 = -4.0;
pub const RES_RATIO: f32 = 3.0;
pub const RES_ATTACK_MS: f32 = 5.0;
pub const RES_RELEASE_MS: f32 = 80.0;
/// A band acts above its median active level plus this (dB).
pub const THRESH_OVER_MEDIAN_DB: f32 = 2.0;
/// A resonance sits this far over the spectrum around it (dB) ...
pub const EXCESS_DB: f32 = 3.0;
/// ... in at least this share of the vocal's active frames.
pub const MIN_PERSIST: f32 = 0.25;
/// Where resonances are looked for (Hz): above the voice's harmonics-dense
/// low end, below the de-esser's air.
pub const LO_HZ: f32 = 700.0;
pub const HI_HZ: f32 = 8000.0;
const N: usize = 4096;
const HOP: usize = 1024;
/// Half-width of the envelope a band is compared with, in 1/12-octave bands
/// (4 = a third of an octave each side).
const ENV_HALF: usize = 4;
const ACTIVE_RANGE_DB: f32 = 40.0;
/// Bands of padding each side of the searched range: the envelope plus one,
/// for the neighbour comparison and the 3-band frame smoothing.
const PAD: usize = ENV_HALF + 1;

/// One resonance: centre, how far the long-term spectrum sits over its
/// surroundings there (dB), the share of active frames it pokes out in, and
/// the score it was ranked by (excess x persistence).
#[derive(Clone, Debug, PartialEq)]
pub struct Resonance {
    pub hz: f32,
    pub excess_db: f32,
    pub persistence: f32,
    pub score: f32,
}

/// 1/12-octave band centres from `LO_HZ` to `HI_HZ`, padded by the envelope
/// half-width on each side so every searched band has a full envelope.
fn centres() -> Vec<f32> {
    let k0 = -(PAD as i32);
    let mut out = Vec::new();
    let mut k = k0;
    loop {
        let f = LO_HZ * 2f32.powf(k as f32 / 12.0);
        if f > HI_HZ * 2f32.powf(PAD as f32 / 12.0) {
            break;
        }
        out.push(f);
        k += 1;
    }
    out
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_by(|a, b| a.total_cmp(b));
    if v.is_empty() {
        0.0
    } else {
        v[v.len() / 2]
    }
}

fn near(a: f32, b: f32) -> bool {
    (a / b).log2().abs() < 1.0 / 3.0
}

/// The resonances of a mono vocal, strongest first: at most `MAX_BANDS`, a
/// third of an octave apart, none within a third of an octave of `exclude`
/// (the de-esser's band, the user's vocal EQ bands).
pub fn find(x: &[f32], exclude: &[f32]) -> Vec<Resonance> {
    if x.len() < N {
        return Vec::new();
    }
    let c = centres();
    let bin_hz = SR / N as f32;
    let edges: Vec<(usize, usize)> = c
        .iter()
        .map(|f| {
            let lo = ((f * 2f32.powf(-1.0 / 24.0)) / bin_hz).floor() as usize;
            let hi = ((f * 2f32.powf(1.0 / 24.0)) / bin_hz).ceil() as usize;
            (lo.max(1), hi.min(N / 2).max(lo.max(1) + 1))
        })
        .collect();
    let win: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos()).collect();
    let (mut re, mut im) = (vec![0f32; N], vec![0f32; N]);
    let mut frames: Vec<Vec<f32>> = Vec::new();
    let mut tot: Vec<f32> = Vec::new();
    let mut start = 0;
    while start + N <= x.len() {
        let mut e = 0.0f32;
        for i in 0..N {
            re[i] = x[start + i] * win[i];
            im[i] = 0.0;
            e += re[i] * re[i];
        }
        fft(&mut re, &mut im);
        frames.push(edges.iter().map(|&(lo, hi)| (lo..hi).map(|k| re[k] * re[k] + im[k] * im[k]).sum::<f32>() / (hi - lo) as f32).collect());
        tot.push(e);
        start += HOP;
    }
    let max = tot.iter().cloned().fold(0.0f32, f32::max);
    let floor = (max * 10f32.powf(-ACTIVE_RANGE_DB / 10.0)).max(1e-12);
    let act: Vec<usize> = (0..frames.len()).filter(|&i| tot[i] >= floor).collect();
    if act.len() < 4 {
        return Vec::new();
    }
    let nb = c.len();
    let db = |p: f32| 10.0 * p.max(1e-20).log10();
    // long-term spectrum over the active frames, and each band's excess over
    // the median of the third of an octave around it
    let ltas: Vec<f32> = (0..nb).map(|b| db(act.iter().map(|&i| frames[i][b]).sum::<f32>() / act.len() as f32)).collect();
    let excess = |lv: &[f32], b: usize| -> f32 {
        let mut w: Vec<f32> = lv[b - ENV_HALF..=b + ENV_HALF].to_vec();
        lv[b] - median(&mut w)
    };
    let mut cands: Vec<Resonance> = Vec::new();
    for b in PAD..nb - PAD {
        let ex = excess(&ltas, b);
        if ex < EXCESS_DB || ex < excess(&ltas, b - 1) || ex < excess(&ltas, b + 1) {
            continue;
        }
        // how often it pokes out frame by frame (each frame lightly smoothed
        // across neighbouring bands so a single harmonic does not count)
        let mut hits = 0usize;
        for &i in &act {
            let sm: Vec<f32> = (b - ENV_HALF..=b + ENV_HALF)
                .map(|k| db((frames[i][k - 1] + frames[i][k] + frames[i][k + 1]) / 3.0))
                .collect();
            let mut w = sm.clone();
            if sm[ENV_HALF] - median(&mut w) >= EXCESS_DB {
                hits += 1;
            }
        }
        let persistence = hits as f32 / act.len() as f32;
        if persistence < MIN_PERSIST {
            continue;
        }
        cands.push(Resonance { hz: c[b], excess_db: ex, persistence, score: ex * persistence });
    }
    cands.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut out: Vec<Resonance> = Vec::new();
    for r in cands {
        if out.len() < MAX_BANDS && !exclude.iter().any(|&e| near(e, r.hz)) && out.iter().all(|o| !near(o.hz, r.hz)) {
            out.push(r);
        }
    }
    out
}

/// The threshold (dB) a self-keyed band at `hz` gets on this vocal: the
/// median of the band's envelope (the dynamic EQ's own band-pass and
/// follower) over the vocal's active 10 ms frames, plus
/// `THRESH_OVER_MEDIAN_DB`, so the band only works when the resonance pokes
/// out over its usual level.
pub fn band_threshold_db(x: &[f32], hz: f32) -> f32 {
    let mut bp = Biquad::new(BiquadKind::Bandpass, hz, RES_Q, 0.0);
    let coef = |ms: f32| (-1.0 / (ms * 0.001 * SR)).exp();
    let (att, rel) = (coef(RES_ATTACK_MS), coef(RES_RELEASE_MS));
    let hop = (0.01 * SR) as usize;
    let mut env = 0.0f32;
    let mut envs = Vec::with_capacity(x.len() / hop + 1);
    let mut lv = Vec::with_capacity(x.len() / hop + 1);
    let (mut acc, mut peak) = (0.0f32, 0.0f32);
    for (i, &s) in x.iter().enumerate() {
        let y = bp.process(s).abs();
        let c = if y > env { att } else { rel };
        env = c * env + (1.0 - c) * y;
        acc += s * s;
        peak = peak.max(env);
        if (i + 1) % hop == 0 {
            lv.push(10.0 * (acc / hop as f32).max(1e-20).log10());
            envs.push(20.0 * env.max(1e-10).log10());
            acc = 0.0;
        }
    }
    let top = lv.iter().cloned().fold(f32::MIN, f32::max);
    let mut on: Vec<f32> = envs.iter().zip(&lv).filter(|(_, l)| **l >= top - ACTIVE_RANGE_DB).map(|(e, _)| *e).collect();
    let _ = peak;
    median(&mut on) + THRESH_OVER_MEDIAN_DB
}

/// The de-esser's bands on a chain: the tamer always stays a third of an
/// octave away from them (the de-esser already works there).
pub fn deesser_bands(effects: &[Effect]) -> Vec<f32> {
    effects.iter().filter_map(|f| if let Effect::Deesser(d) = f { Some(d.freq) } else { None }).collect()
}

/// Where song_create's tamer stays out of: the de-esser's band and every
/// enabled, non-auto EQ band with gain on the vocal's chain (a person's
/// deliberate move, set through the song's vocal chain or part EQ).
pub fn exclusions(effects: &[Effect]) -> Vec<f32> {
    let mut out = Vec::new();
    for f in effects {
        match f {
            Effect::Deesser(d) => out.push(d.freq),
            Effect::ParametricEq(pq) => out.extend(pq.bands.iter().filter(|b| !b.auto && b.enabled && b.gain_db.abs() > 1e-3).map(|b| b.freq)),
            _ => {}
        }
    }
    out
}

/// The self-keyed dynamic EQ band the tamer puts on a resonance at `hz`
/// with threshold `threshold_db` (band number `k`, from 1).
pub fn band_effect(hz: f32, threshold_db: f32, k: usize) -> Effect {
    let mut fx = Effect::DynamicEq(DynamicEqFx {
        freq: hz,
        q: RES_Q,
        threshold_db,
        ratio: RES_RATIO,
        range_db: RES_RANGE_DB,
        attack_ms: RES_ATTACK_MS,
        release_ms: RES_RELEASE_MS,
        ..Default::default()
    });
    fx.set_id(&format!("{AUTO_RESONANCE_ID}{k}"));
    fx
}

/// Takes earlier tamer bands off a chain (each build measures again).
/// Returns how many.
pub fn strip(effects: &mut Vec<Effect>) -> usize {
    let n = effects.len();
    effects.retain(|f| !f.id().starts_with(AUTO_RESONANCE_ID));
    n - effects.len()
}

/// The whole tamer on one track: `x` is the track's mono audio as its
/// chain leaves it. Finds the resonances (none within a third of an octave
/// of `exclude`), appends one band per resonance at the end of the chain
/// and returns what it did: {"bands": [{hz, excess_db, persistence_pct,
/// threshold_db, max_db, q, attack_ms, release_ms, track}],
/// "skipped_near_hz": [...]}. Nothing found = the chain is untouched.
pub fn tame_track(t: &mut crate::project::Track, x: &[f32], exclude: &[f32], log: &mut Vec<String>) -> Value {
    let found = find(x, exclude);
    let r1 = |v: f32| (v * 10.0).round() / 10.0;
    let mut bands = Vec::new();
    for (k, r) in found.iter().enumerate() {
        let th = band_threshold_db(x, r.hz);
        t.effects.push(band_effect(r.hz, th, k + 1));
        log.push(format!(
            "vocal resonance: {} {:.0} Hz sits {:.1} dB over its surroundings in {:.0}% of the singing: band Q{} up to {} dB above {:.1} dB",
            t.name,
            r.hz,
            r.excess_db,
            r.persistence * 100.0,
            RES_Q,
            RES_RANGE_DB,
            th
        ));
        bands.push(json!({"hz": r.hz.round(), "excess_db": r1(r.excess_db), "persistence_pct": r1(r.persistence * 100.0), "threshold_db": r1(th), "max_db": RES_RANGE_DB, "q": RES_Q, "attack_ms": RES_ATTACK_MS, "release_ms": RES_RELEASE_MS, "track": t.name}));
    }
    if found.is_empty() {
        log.push(format!("vocal resonance: no lasting resonance found on {}", t.name));
    }
    json!({"bands": bands, "skipped_near_hz": exclude.iter().map(|f| f.round()).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A voice-like source: a band-limited saw whose pitch moves every 200
    /// ms over 150-260 Hz, plus a little noise.
    fn voice(secs: f32, seed: u32) -> Vec<f32> {
        let n = (secs * SR) as usize;
        let mut s = seed;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        let mut out = vec![0.0f32; n];
        let seg = (0.2 * SR) as usize;
        let mut ph = 0.0f32;
        let mut f0 = 200.0f32;
        for i in 0..n {
            if i % seg == 0 {
                f0 = 205.0 + 55.0 * rnd();
            }
            ph += f0 / SR;
            let mut v = 0.0;
            let mut h = 1.0f32;
            while h * f0 < 10000.0 {
                v += (2.0 * std::f32::consts::PI * h * ph).sin() / h;
                h += 1.0;
            }
            out[i] = 0.2 * v + 0.01 * rnd();
        }
        out
    }

    fn white(secs: f32, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..(secs * SR) as usize)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s as f32 / u32::MAX as f32) - 0.5
            })
            .collect()
    }

    #[test]
    fn a_narrow_peak_is_found_near_its_centre() {
        let dry = voice(6.0, 7);
        let mut bell = Biquad::new(BiquadKind::Bell, 3000.0, 8.0, 8.0);
        let ringing: Vec<f32> = dry.iter().map(|&v| bell.process(v)).collect();
        let r = find(&ringing, &[]);
        assert!(r.iter().any(|x| (x.hz / 3000.0).log2().abs() < 1.0 / 6.0), "{r:?}");
        assert!(r.len() <= MAX_BANDS);
        // the de-esser's (or a user's) band nearby blocks it
        let r2 = find(&ringing, &[3150.0]);
        assert!(r2.iter().all(|x| (x.hz / 3000.0).log2().abs() >= 1.0 / 3.0), "{r2:?}");
        // the threshold sits over the band's usual level
        let t = band_threshold_db(&ringing, 3000.0);
        assert!(t.is_finite() && t < 0.0, "{t}");
        // on a track: one cut-only band per resonance at the chain's end,
        // and a rebuild's strip takes exactly those out
        let mut tr = crate::project::Track::new("vox", crate::instruments::preset("808").unwrap());
        let before = serde_json::to_value(&tr.effects).unwrap();
        let mut log = Vec::new();
        let v = tame_track(&mut tr, &ringing, &[], &mut log);
        let bands = v["bands"].as_array().unwrap();
        assert_eq!(bands.len(), r.len());
        assert_eq!(tr.effects.iter().filter(|f| f.id().starts_with(AUTO_RESONANCE_ID)).count(), r.len());
        for f in &tr.effects {
            if let Effect::DynamicEq(d) = f {
                assert!(d.range_db < 0.0 && d.range_db >= -4.0 && d.q == RES_Q && d.ratio == RES_RATIO && d.source.is_empty(), "{d:?}");
            }
        }
        assert_eq!(strip(&mut tr.effects), r.len());
        assert_eq!(serde_json::to_value(&tr.effects).unwrap(), before);
    }

    #[test]
    fn a_flat_spectrum_gets_no_band() {
        let noise = white(6.0, 99);
        assert!(find(&noise, &[]).is_empty());
        let mut tr = crate::project::Track::new("vox", crate::instruments::preset("808").unwrap());
        let before = serde_json::to_value(&tr.effects).unwrap();
        let mut log = Vec::new();
        let v = tame_track(&mut tr, &noise, &[], &mut log);
        assert!(v["bands"].as_array().unwrap().is_empty());
        assert_eq!(serde_json::to_value(&tr.effects).unwrap(), before, "nothing found: the chain is untouched");
        // too short to measure: nothing either
        assert!(find(&noise[..2000], &[]).is_empty());
    }

    /// The tamer's bands are ordinary chain effects on the vocal, so they
    /// are in the vocal track's own render-cache key (exact canonical JSON,
    /// render::TrackKey) and no other track's: on, and every band setting,
    /// moves the vocal's key; stripping them gives the exact old key back,
    /// and the render with the tamer stripped is the old render, byte for
    /// byte, fresh or through the cache.
    #[test]
    fn tamer_settings_are_in_the_vocal_cache_key_and_off_is_bit_identical() {
        use crate::project::{Note, Project, Track};
        use crate::render::{render, render_cached, RenderOptions, TrackCache};
        use crate::samples::SampleBank;
        let mut p = Project::new("k", 120.0);
        for (n, inst) in [("vox", "sitar"), ("bass", "808")] {
            p.tracks.push(Track::new(n, crate::instruments::preset(inst).unwrap()));
            p.patterns[0].clips.insert(n.into(), vec![Note::new(0.0, 6.0, 57, 1.0), Note::new(8.0, 6.0, 60, 0.9)]);
        }
        let bank = SampleBank::default();
        let o = RenderOptions::default();
        let keys = |q: &Project| TrackCache::keys(q, &bank, &o);
        let p0 = p.clone();
        let k0 = keys(&p0);
        p.tracks[0].effects.push(band_effect(3000.0, -30.0, 1));
        let k1 = keys(&p);
        assert_ne!(k1[0], k0[0], "tamer on moves the vocal's key");
        assert_eq!(k1[1], k0[1], "and no other track's");
        for edit in 0..7 {
            let mut q = p.clone();
            match &mut q.tracks[0].effects[0] {
                Effect::DynamicEq(d) => match edit {
                    0 => d.freq = 3100.0,
                    1 => d.threshold_db = -29.5,
                    2 => d.range_db = -3.0,
                    3 => d.ratio = 2.0,
                    4 => d.q = 4.0,
                    5 => d.release_ms = 120.0,
                    _ => d.bypass = true,
                },
                _ => unreachable!(),
            }
            let kq = keys(&q);
            assert_ne!(kq[0], k1[0], "edit {edit} moves the vocal's key");
            assert_eq!(kq[1], k0[1], "edit {edit} leaves the bass's key");
        }
        let mut q = p.clone();
        q.tracks[0].effects.push(band_effect(1200.0, -28.0, 2));
        assert_ne!(keys(&q)[0], k1[0], "a second band moves it");
        // off = the old key and the old bytes
        let mut off = p.clone();
        assert_eq!(strip(&mut off.tracks[0].effects), 1);
        assert_eq!(keys(&off), k0);
        let fresh0 = render(&p0, &bank, &o).unwrap();
        let fresh1 = render(&p, &bank, &o).unwrap();
        assert_ne!(fresh1.left, fresh0.left, "the band does act on a bright part");
        let mut c = TrackCache::default();
        let a0 = render_cached(&p0, &bank, &o, Some(&mut c)).unwrap();
        let a1 = render_cached(&p, &bank, &o, Some(&mut c)).unwrap();
        let a2 = render_cached(&off, &bank, &o, Some(&mut c)).unwrap();
        let bits = |m: &crate::render::Mix| m.left.iter().chain(m.right.iter()).map(|x| x.to_bits()).collect::<Vec<u32>>();
        assert_eq!(bits(&a0), bits(&fresh0));
        assert_eq!(bits(&a1), bits(&fresh1), "tamer on through the cache == fresh");
        assert_eq!(bits(&a2), bits(&fresh0), "tamer off through the cache == the render without it");
        assert_eq!(bits(&render(&off, &bank, &o).unwrap()), bits(&fresh0));
    }
}
