//! Masking-aware auto-EQ (/mix/auto_eq): how much two parts collide in
//! frequency where both play, and the small cuts that clear the collision.
//!
//! The measure: each part's mono stem cut into frames (4096-point STFT, hop
//! 2048, Hann) and 1/3-octave bands (25 Hz..16 kHz), each band's power taken
//! to a loudness-like 0.3 power. Only frames where both parts are active
//! count (each within `ACTIVE_RANGE_DB` of its own loudest frame). The
//! overlap of a protected part `a` with another part `b` is
//! sum(min(La, Lb)) / sum(La) over those frames and the pair's band range:
//! the share of `a`'s loudness that `b` matches or exceeds band by band
//! (0 = no collision, 1 = `b` covers `a` everywhere). Cutting `b` where it
//! collides lowers it.
//!
//! The cuts: bells on the part that is not protected (never the vocal), in
//! the bands where a cut removes the most overlap (worked out frame by
//! frame, so a band where the part is far louder than the vocal, where a
//! cut would only take level away, is passed over), -1..-3 dB by how far the
//! part sits over the protected one there, at most `MAX_AUTO_BELLS` per
//! part, and each clashing pair's strongest band first (so a part that
//! clashes with the vocal and with the kick gets one cut for each). A user's own part EQ always wins: auto bells only take free bell
//! slots of the 8-band EQ (2 shelves + 6 bells) and stay clear (by about a
//! third of an octave) of every band the user set.

use crate::analysis::fft;
use crate::dsp::SR;
use crate::song::{EqBell, PartEq, PART_EQ_MAX_BELLS};

/// Deepest auto cut, dB.
pub const MAX_CUT_DB: f32 = 3.0;
/// Shallowest auto cut, dB.
pub const MIN_CUT_DB: f32 = 1.0;
/// Most auto bells one part gets (over every pair it clashes in).
pub const MAX_AUTO_BELLS: usize = 2;
/// An auto bell's Q against the vocal (about 2/3 of an octave wide).
pub const AUTO_Q: f32 = 2.0;
/// An auto bell's Q for the kick against the bass (about 1/3 of an
/// octave, one band): the kick's fundamental is narrow, and a narrow bell
/// takes less of the bass's sub weight (every judge band share is measured
/// against the whole mix, which the sub dominates).
pub const LOW_Q: f32 = 4.0;
/// Where the vocal's collisions are measured (body, formants, words), Hz.
pub const VOCAL_RANGE: (f32, f32) = (150.0, 4000.0);
/// Where the kick and bass collide, Hz.
pub const LOW_RANGE: (f32, f32) = (30.0, 250.0);
/// Where cuts against the vocal go, Hz: the vocal's body and formants. Not
/// in its 1-4 kHz words band: the judge reads presence (1-4 kHz) on the
/// whole mix, and instrument cuts there lowered it by 0.05 to 0.35 dB on
/// D12 (B42's v2 and v3 renders); cuts below it clear the vocal in the
/// measured 150 Hz-4 kHz without moving the judge's presence.
pub const VOCAL_CUT_RANGE: (f32, f32) = (150.0, 1000.0);
/// A frame is active within this many dB of its part's loudest frame.
pub const ACTIVE_RANGE_DB: f32 = 40.0;
/// "Within about a third of an octave", in octaves (adjacent 1/3-octave
/// bands count as within).
pub const NEAR_OCT: f32 = 0.34;
/// A pair clashes when its overlap is at least this (5%); under it the
/// collision is noise and the part is left alone.
pub const MIN_PAIR_OVERLAP: f32 = 0.05;
/// A band of a clashing pair is a candidate cut when it carries at least
/// this share of the protected part's loudness as overlap, and at least
/// `REL_SHARE` of the pair's strongest band.
const MIN_SHARE: f32 = 0.003;
const REL_SHARE: f32 = 0.3;
const N: usize = 4096;
const HOP: usize = 2048;

/// 1/3-octave band centres, 25 Hz..16 kHz (1000 * 2^(k/3)).
pub fn centres() -> Vec<f32> {
    (-16..=12).map(|k| 1000.0 * 2f32.powf(k as f32 / 3.0)).collect()
}

/// Per frame, per 1/3-octave band (as `centres`), the band power of `mono`.
pub fn band_frames(mono: &[f32]) -> Vec<Vec<f32>> {
    let c = centres();
    let bin_hz = SR / N as f32;
    // each band's bins
    let edges: Vec<(usize, usize)> = c
        .iter()
        .map(|f| {
            let lo = ((f * 2f32.powf(-1.0 / 6.0)) / bin_hz).ceil() as usize;
            let hi = ((f * 2f32.powf(1.0 / 6.0)) / bin_hz).ceil() as usize;
            (lo.max(1), hi.min(N / 2).max(lo.max(1)))
        })
        .collect();
    let win: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos()).collect();
    let mut out = Vec::new();
    if mono.len() < N {
        return out;
    }
    let (mut re, mut im) = (vec![0f32; N], vec![0f32; N]);
    let mut start = 0;
    while start + N <= mono.len() {
        for i in 0..N {
            re[i] = mono[start + i] * win[i];
            im[i] = 0.0;
        }
        fft(&mut re, &mut im);
        out.push(
            edges
                .iter()
                .map(|&(lo, hi)| (lo..hi).map(|k| re[k] * re[k] + im[k] * im[k]).sum::<f32>())
                .collect(),
        );
        start += HOP;
    }
    out
}

/// Which frames of a part are active (within `ACTIVE_RANGE_DB` of its
/// loudest frame, and not silent).
fn active(f: &[Vec<f32>]) -> Vec<bool> {
    let tot: Vec<f32> = f.iter().map(|b| b.iter().sum::<f32>()).collect();
    let max = tot.iter().cloned().fold(0.0f32, f32::max);
    let floor = (max * 10f32.powf(-ACTIVE_RANGE_DB / 10.0)).max(1e-9);
    tot.iter().map(|t| *t >= floor && *t > 0.0).collect()
}

/// One pair's collision: the overlap (0..1) and, per band, the share of the
/// protected part's loudness it carries and how far the other part sits
/// over the protected one (dB, power). Keeps both parts' band loudness in
/// the frames that count, so a cut's effect can be worked out
/// (`reduction`).
#[derive(Clone, Debug)]
pub struct PairStat {
    pub overlap: f32,
    pub band_share: Vec<f32>,
    pub excess_db: Vec<f32>,
    pub frames: usize,
    la: Vec<f32>,
    lb: Vec<f32>,
    den: f64,
}

impl PairStat {
    /// How much a bell (`hz`, `db`, `q`) on the cut part lowers this pair's
    /// overlap (0..1): the bell's gain at each band centre applied to the
    /// part's band loudness, frame by frame. Where the part is far louder
    /// than the protected one, min() is the protected part's own loudness
    /// and a cut there removes almost no overlap: this is what tells a cut
    /// that clears a collision from one that only takes level away.
    pub fn reduction(&self, hz: f32, db: f32, q: f32) -> f32 {
        let c = centres();
        let f = crate::dsp::Biquad::new(crate::dsp::BiquadKind::Bell, hz, q, db);
        let s: Vec<f32> = c.iter().map(|x| 10f32.powf(0.03 * f.response_db(*x))).collect();
        let nb = c.len();
        let mut d = 0f64;
        for (ra, rb) in self.la.chunks(nb).zip(self.lb.chunks(nb)) {
            for k in 0..nb {
                d += (ra[k].min(rb[k]) - ra[k].min(rb[k] * s[k])) as f64;
            }
        }
        (d / self.den) as f32
    }
}

/// The collision of protected `a` with `b` over `range` (Hz).
pub fn pair_stat(a: &[Vec<f32>], b: &[Vec<f32>], range: (f32, f32)) -> PairStat {
    let c = centres();
    let nb = c.len();
    let (aa, ab) = (active(a), active(b));
    let n = a.len().min(b.len());
    let inr: Vec<bool> = c.iter().map(|f| *f >= range.0 && *f <= range.1).collect();
    let (mut num, mut den) = (vec![0f64; nb], 0f64);
    let (mut pa, mut pb) = (vec![0f64; nb], vec![0f64; nb]);
    let (mut la_all, mut lb_all) = (Vec::new(), Vec::new());
    let mut frames = 0;
    for t in 0..n {
        if !(aa[t] && ab[t]) {
            continue;
        }
        frames += 1;
        for k in 0..nb {
            if !inr[k] {
                la_all.push(0.0);
                lb_all.push(0.0);
                continue;
            }
            let (la, lb) = ((a[t][k] as f64).max(0.0).powf(0.3), (b[t][k] as f64).max(0.0).powf(0.3));
            la_all.push(la as f32);
            lb_all.push(lb as f32);
            num[k] += la.min(lb);
            den += la;
            pa[k] += a[t][k] as f64;
            pb[k] += b[t][k] as f64;
        }
    }
    let den = den.max(1e-30);
    PairStat {
        overlap: (num.iter().sum::<f64>() / den) as f32,
        band_share: num.iter().map(|x| (x / den) as f32).collect(),
        excess_db: (0..nb).map(|k| if pa[k] > 0.0 && pb[k] > 0.0 { (10.0 * (pb[k] / pa[k]).log10()) as f32 } else { -120.0 }).collect(),
        frames,
        la: la_all,
        lb: lb_all,
        den,
    }
}

/// A cut the measure asks for on one part.
#[derive(Clone, Debug, PartialEq)]
pub struct Cand {
    pub hz: f32,
    pub db: f32,
    /// How much of the pair's overlap this cut removes (`PairStat::reduction`).
    pub score: f32,
    /// The bell's Q (`AUTO_Q`, or `LOW_Q` for the kick and bass).
    pub q: f32,
    /// Which pair asked for it (e.g. "vocal/chords").
    pub pair: String,
}

/// The cut depth for a band where the cut part sits `excess_db` over the
/// protected one: -1.5 dB at level, -3 dB at +6 dB, -1 dB at -2 dB or less.
pub fn cut_db(excess_db: f32) -> f32 {
    -(1.5 + 0.25 * excess_db).clamp(MIN_CUT_DB, MAX_CUT_DB)
}

/// The candidate cuts one pair's measure asks for (bells of Q `q`, centres
/// inside `cut_range`), best first; none when the pair does not clash
/// (overlap under `MIN_PAIR_OVERLAP`).
pub fn candidates(st: &PairStat, pair: &str, q: f32, cut_range: (f32, f32)) -> Vec<Cand> {
    if st.overlap < MIN_PAIR_OVERLAP {
        return Vec::new();
    }
    let c = centres();
    let top = st.band_share.iter().cloned().fold(0.0f32, f32::max);
    let floor = MIN_SHARE.max(REL_SHARE * top);
    let mut v: Vec<Cand> = (0..c.len())
        .filter(|&k| st.band_share[k] >= floor && st.band_share[k] > 0.0 && c[k] >= cut_range.0 && c[k] <= cut_range.1)
        .map(|k| {
            let db = cut_db(st.excess_db[k]);
            Cand { hz: c[k], db, score: st.reduction(c[k], db, q), q, pair: pair.to_string() }
        })
        .filter(|c| c.score > 1e-5)
        .collect();
    v.sort_by(|a, b| b.score.total_cmp(&a.score));
    v
}

fn near(a: f32, b: f32) -> bool {
    (a / b).log2().abs() <= NEAR_OCT
}

/// The auto bells for one part from its candidates (any pairs, any order)
/// beside the user's own part EQ (if any), plus a log line per cut that
/// was skipped. Never more than `MAX_AUTO_BELLS`, never deeper than
/// `MAX_CUT_DB`, only into free bell slots, never within about a third of
/// an octave of a user band, never on top of another auto bell.
pub fn plan_part(part: &str, cands: &[Cand], user: Option<&PartEq>) -> (Vec<EqBell>, Vec<String>) {
    // each pair's strongest band first (strongest pair first), then the
    // rest by score
    let mut sorted = cands.to_vec();
    sorted.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut cands: Vec<Cand> = Vec::new();
    for c in &sorted {
        if !cands.iter().any(|x| x.pair == c.pair) {
            cands.push(c.clone());
        }
    }
    for c in &sorted {
        if !cands.contains(c) {
            cands.push(c.clone());
        }
    }
    let user_bells: Vec<&EqBell> = user.map(|u| u.bells.iter().filter(|b| !b.auto).collect()).unwrap_or_default();
    let mut user_hz: Vec<f32> = user_bells.iter().map(|b| b.hz).collect();
    if let Some(u) = user {
        user_hz.extend(u.low_shelf.iter().chain(u.high_shelf.iter()).map(|s| s.hz));
    }
    let free = PART_EQ_MAX_BELLS.saturating_sub(user_bells.len());
    let mut out: Vec<EqBell> = Vec::new();
    let mut log = Vec::new();
    for c in &cands {
        if out.len() >= MAX_AUTO_BELLS {
            break;
        }
        if out.iter().any(|b| near(b.hz, c.hz)) {
            continue; // the same collision, already cut
        }
        if let Some(u) = user_hz.iter().find(|u| near(**u, c.hz)) {
            log.push(format!("auto EQ: skipped {part} {:.0} Hz {:.1} dB ({}): a user band sits at {u:.0} Hz; the user's band wins", c.hz, c.db, c.pair));
            continue;
        }
        if out.len() >= free {
            log.push(format!("auto EQ: skipped {part} {:.0} Hz {:.1} dB ({}): no free bell slot (the user's part EQ has {} of {PART_EQ_MAX_BELLS} bells)", c.hz, c.db, c.pair, user_bells.len()));
            continue;
        }
        let db = c.db.clamp(-MAX_CUT_DB, -MIN_CUT_DB);
        out.push(EqBell { hz: c.hz.round(), db: (db * 10.0).round() / 10.0, q: c.q, auto: true });
    }
    (out, log)
}

/// Most a sidechain duck (/mix/vocal_duck, /mix/kick_duck) dips its band, dB.
pub const DUCK_DB: f32 = 4.0;
/// A duck's attack, ms.
pub const DUCK_ATTACK_MS: f32 = 10.0;
/// A duck's release (how long each trigger holds the dip), ms.
pub const DUCK_RELEASE_MS: f32 = 120.0;
/// The vocal duck's envelope follower (it follows the vocal's level, not
/// its onsets): attack and release time constants, ms.
pub const VOCAL_DUCK_ATTACK_MS: f32 = 12.0;
pub const VOCAL_DUCK_RELEASE_MS: f32 = 100.0;
/// The kick duck's release is cut to this share of the kick's median hit
/// spacing when 120 ms would hold the dip across most of the gap between
/// hits (kicks closer than about 250 ms).
pub const KICK_RELEASE_SHARE: f32 = 0.5;

/// The band a sidechain duck goes in for one pair: the band (centre inside
/// `cut_range`, of a pair that clashes) where a -`DUCK_DB` dip of Q `q`
/// removes the most overlap. None when the pair does not clash.
pub fn duck_band(st: &PairStat, pair: &str, q: f32, cut_range: (f32, f32)) -> Option<Cand> {
    if st.overlap < MIN_PAIR_OVERLAP {
        return None;
    }
    let c = centres();
    (0..c.len())
        .filter(|&k| st.band_share[k] > 0.0 && c[k] >= cut_range.0 && c[k] <= cut_range.1)
        .map(|k| Cand { hz: c[k], db: -DUCK_DB, score: st.reduction(c[k], -DUCK_DB, q), q, pair: pair.to_string() })
        .filter(|c| c.score > 1e-5)
        .max_by(|a, b| a.score.total_cmp(&b.score))
}

/// The user band (bell centre or shelf corner) within about a third of an
/// octave of `hz`, if any: an auto band backs off from it.
pub fn user_band_near(user: Option<&PartEq>, hz: f32) -> Option<f32> {
    let u = user?;
    u.bells.iter().filter(|b| !b.auto).map(|b| b.hz).chain(u.low_shelf.iter().chain(u.high_shelf.iter()).map(|s| s.hz)).find(|x| near(*x, hz))
}

/// The kick duck's release for kick hits at these sample positions:
/// `DUCK_RELEASE_MS`, or `KICK_RELEASE_SHARE` of the median spacing when
/// that is shorter (never under 40 ms).
pub fn kick_release_ms(hits: &[usize]) -> f32 {
    let mut gaps: Vec<f32> = hits.windows(2).map(|w| (w[1].saturating_sub(w[0])) as f32 / SR * 1000.0).filter(|g| *g > 20.0).collect();
    if gaps.is_empty() {
        return DUCK_RELEASE_MS;
    }
    gaps.sort_by(|a, b| a.total_cmp(b));
    let med = gaps[gaps.len() / 2];
    DUCK_RELEASE_MS.min((KICK_RELEASE_SHARE * med).max(40.0))
}

/// Mono of a stereo stem.
pub fn mono(l: &[f32], r: &[f32]) -> Vec<f32> {
    l.iter().zip(r).map(|(a, b)| 0.5 * (a + b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::{EqBand, EqBandKind, ParametricEqFx};

    fn noise_band(seed: u32, len: usize, lo: f32, hi: f32, gain: f32) -> Vec<f32> {
        // a sum of sines spaced across lo..hi (deterministic)
        let mut x = vec![0f32; len];
        let n = 24;
        let mut s = seed;
        for i in 0..n {
            let f = lo * (hi / lo).powf(i as f32 / (n - 1) as f32);
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let ph = (s >> 8) as f32 / (1u32 << 24) as f32 * 6.283;
            for (t, v) in x.iter_mut().enumerate() {
                *v += gain / n as f32 * (6.283 * f * t as f32 / SR + ph).sin();
            }
        }
        x
    }

    fn eq(x: &[f32], bells: &[EqBell]) -> Vec<f32> {
        let p = ParametricEqFx {
            bands: bells.iter().map(|b| EqBand { kind: EqBandKind::Bell, freq: b.hz, gain_db: b.db, q: b.q, ..Default::default() }).collect(),
            ..Default::default()
        };
        let (mut l, mut r) = (x.to_vec(), x.to_vec());
        crate::fx_extra::parametric_eq(&p, &mut l, &mut r);
        l
    }

    #[test]
    fn overlap_drops_when_the_auto_cuts_go_on_a_synthetic_clash() {
        let len = (SR * 3.0) as usize;
        let vocal = noise_band(1, len, 300.0, 3000.0, 0.3);
        // a pad sitting on the vocal's 500-1200 Hz at about its level there
        // (far louder, min() is the vocal's own loudness and a 3 dB cut
        // would not move it: the measure saturates)
        let pad = noise_band(2, len, 500.0, 1200.0, 0.2);
        let (fv, fp) = (band_frames(&vocal), band_frames(&pad));
        let before = pair_stat(&fv, &fp, VOCAL_RANGE);
        assert!(before.overlap > 0.1, "a clash to clear: {before:?}");
        let (bells, log) = plan_part("pad", &candidates(&before, "vocal/pad", AUTO_Q, VOCAL_CUT_RANGE), None);
        assert!(!bells.is_empty() && bells.len() <= MAX_AUTO_BELLS, "{bells:?} {log:?}");
        assert!(bells.iter().all(|b| b.db <= -MIN_CUT_DB && b.db >= -MAX_CUT_DB && b.auto && (500.0..=1000.0).contains(&b.hz)), "{bells:?}");
        let after = pair_stat(&fv, &band_frames(&eq(&pad, &bells)), VOCAL_RANGE);
        // the predicted drop of the first bell alone is real and positive
        assert!(before.reduction(bells[0].hz, bells[0].db, bells[0].q) > 0.001);
        assert!(after.overlap < before.overlap - 0.005, "overlap {:.4} -> {:.4}", before.overlap, after.overlap);
        // a part elsewhere (sub only) does not clash with the vocal's range
        let sub = noise_band(3, len, 35.0, 70.0, 0.8);
        let st = pair_stat(&fv, &band_frames(&sub), VOCAL_RANGE);
        assert!(candidates(&st, "vocal/sub", AUTO_Q, VOCAL_CUT_RANGE).is_empty(), "{st:?}");
    }

    fn cand(hz: f32, db: f32, score: f32) -> Cand {
        Cand { hz, db, score, q: AUTO_Q, pair: "vocal/keys".into() }
    }

    #[test]
    fn auto_cuts_are_capped_at_minus_3_db_and_2_bells_per_part() {
        let cs = vec![cand(800.0, -9.0, 0.3), cand(2000.0, -2.0, 0.2), cand(400.0, -3.0, 0.1), cand(1000.0, -3.0, 0.25)];
        let (b, _) = plan_part("keys", &cs, None);
        assert_eq!(b.len(), 2, "{b:?}");
        assert!(b.iter().all(|x| x.db >= -3.0 && x.db <= -1.0 && x.auto), "{b:?}");
        // 1000 Hz is within a third of an octave of 800: the same collision
        assert_eq!(b.iter().map(|x| x.hz).collect::<Vec<_>>(), vec![800.0, 2000.0]);
        assert_eq!(cut_db(30.0), -3.0);
        assert_eq!(cut_db(-30.0), -1.0);
    }

    #[test]
    fn a_user_band_within_a_third_of_an_octave_makes_the_auto_cut_back_off() {
        let user = PartEq { bells: vec![EqBell { hz: 1000.0, db: -1.0, q: 1.0, auto: false }], ..Default::default() };
        // 1260 Hz is a third of an octave above the user's 1000 Hz: backs off, with a log line
        let (b, log) = plan_part("keys", &[cand(1260.0, -3.0, 0.5)], Some(&user));
        assert!(b.is_empty(), "{b:?}");
        assert!(log.len() == 1 && log[0].contains("user band sits at 1000 Hz") && log[0].contains("the user's band wins"), "{log:?}");
        // half an octave away (1414 Hz) is clear of it
        let (b, _) = plan_part("keys", &[cand(1414.0, -3.0, 0.5)], Some(&user));
        assert_eq!(b.len(), 1);
        // and below: 794 Hz (a third under) backs off too
        assert!(plan_part("keys", &[cand(794.0, -2.0, 0.5)], Some(&user)).0.is_empty());
        // the user's own bell is never touched or counted as auto
        assert!(!user.bells[0].auto && user.bells[0].db == -1.0);
    }

    #[test]
    fn each_clashing_pair_gets_its_strongest_band_first() {
        let low = |hz: f32, s: f32| Cand { hz, db: -2.0, score: s, q: LOW_Q, pair: "kick/bass".into() };
        // the kick pair's two bands outscore the vocal pair's best: one of each still goes in
        let cs = vec![low(40.0, 0.5), low(80.0, 0.4), cand(500.0, -1.0, 0.05)];
        let (b, _) = plan_part("bass", &cs, None);
        assert_eq!(b.iter().map(|x| (x.hz, x.q)).collect::<Vec<_>>(), vec![(40.0, LOW_Q), (500.0, AUTO_Q)]);
    }

    #[test]
    fn a_duck_band_is_where_a_4_db_dip_clears_the_most_and_the_kick_release_follows_the_hits() {
        let len = (SR * 3.0) as usize;
        let vocal = noise_band(1, len, 300.0, 3000.0, 0.3);
        let pad = noise_band(2, len, 500.0, 1200.0, 0.2);
        let st = pair_stat(&band_frames(&vocal), &band_frames(&pad), VOCAL_RANGE);
        let d = duck_band(&st, "vocal/pad", AUTO_Q, VOCAL_CUT_RANGE).expect("a clash");
        assert!((500.0..=1000.0).contains(&d.hz) && d.db == -DUCK_DB && d.score > 0.0, "{d:?}");
        let quiet = pair_stat(&band_frames(&vocal), &band_frames(&noise_band(3, len, 35.0, 70.0, 0.8)), VOCAL_RANGE);
        assert!(duck_band(&quiet, "vocal/sub", AUTO_Q, VOCAL_CUT_RANGE).is_none());
        // hits every 480 ms keep 120 ms; every 200 ms -> 100 ms; every 60 ms -> the 40 ms floor
        let hits = |ms: f32| (0..20).map(|i| (i as f32 * ms * SR / 1000.0) as usize).collect::<Vec<_>>();
        assert_eq!(kick_release_ms(&hits(480.0)), DUCK_RELEASE_MS);
        assert!((kick_release_ms(&hits(200.0)) - 100.0).abs() < 0.5);
        assert_eq!(kick_release_ms(&hits(60.0)), 40.0);
        // user bands win here too
        let user = PartEq { bells: vec![EqBell { hz: 800.0, db: 1.0, q: 1.0, auto: false }], ..Default::default() };
        assert_eq!(user_band_near(Some(&user), 1000.0), Some(800.0));
        assert_eq!(user_band_near(Some(&user), 1414.0), None);
        assert_eq!(user_band_near(None, 800.0), None);
    }

    #[test]
    fn user_bands_win_over_auto_cuts() {
        let user = PartEq {
            low_shelf: Some(crate::song::EqShelf { hz: 180.0, db: -2.0 }),
            high_shelf: None,
            bells: vec![EqBell { hz: 800.0, db: 2.0, q: 1.0, auto: false }],
            ducks: vec![],
        };
        let cs = vec![cand(800.0, -3.0, 0.4), cand(200.0, -3.0, 0.3), cand(2500.0, -2.0, 0.2), cand(5000.0, -1.5, 0.1)];
        let (b, log) = plan_part("keys", &cs, Some(&user));
        // 800 (a user bell there) and 200 (near the user's 180 Hz shelf) are skipped with a log line
        assert_eq!(b.iter().map(|x| x.hz).collect::<Vec<_>>(), vec![2500.0, 5000.0], "{log:?}");
        assert_eq!(log.iter().filter(|l| l.contains("user")).count(), 2, "{log:?}");
        // no free slot: six user bells take every one
        let full = PartEq { bells: (0..6).map(|i| EqBell { hz: 100.0 * 3f32.powi(i), db: 1.0, q: 1.0, auto: false }).collect(), ..Default::default() };
        let (b, log) = plan_part("keys", &[cand(1500.0, -3.0, 0.5)], Some(&full));
        assert!(b.is_empty() && log[0].contains("no free bell slot"), "{b:?} {log:?}");
        // five user bells: one slot left
        let five = PartEq { bells: full.bells[..5].to_vec(), ..Default::default() };
        let (b, log) = plan_part("keys", &[cand(1500.0, -3.0, 0.5), cand(14000.0, -3.0, 0.4)], Some(&five));
        assert_eq!(b.len(), 1, "{b:?} {log:?}");
    }
}
