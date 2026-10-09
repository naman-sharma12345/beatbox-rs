//! Stereo effects rack: filter, EQ, drive, bitcrush, tempo-synced delay,
//! reverb, chorus, compressor, sidechain ducking, stereo width, gain, limiter.

use crate::dsp::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::f32::consts::PI;

pub struct FxContext<'a> {
    /// Length of one 16th-note step in seconds.
    pub step_secs: f32,
    /// Note-on sample positions for each track (lowercase name).
    pub triggers: &'a HashMap<String, Vec<usize>>,
}

macro_rules! fx_struct {
    ($name:ident { $($field:ident : $ty:ty = $def:expr),* $(,)? }) => {
        #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
        #[serde(default)]
        pub struct $name { $(pub $field: $ty),* }
        impl Default for $name {
            fn default() -> Self { $name { $($field: $def),* } }
        }
    };
}

fx_struct!(FilterFx {
    mode: FilterMode = FilterMode::Lowpass,
    cutoff: f32 = 2000.0,
    resonance: f32 = 0.2
});
fx_struct!(EqFx {
    low_db: f32 = 0.0,
    mid_db: f32 = 0.0,
    high_db: f32 = 0.0,
    low_freq: f32 = 200.0,
    high_freq: f32 = 4000.0
});
fx_struct!(DistortionFx {
    drive: f32 = 0.5,
    mix: f32 = 1.0
});
fx_struct!(BitcrushFx {
    bits: f32 = 8.0,
    downsample: u32 = 4,
    mix: f32 = 1.0
});
fx_struct!(DelayFx {
    steps: f32 = 3.0,
    feedback: f32 = 0.35,
    mix: f32 = 0.25,
    ping_pong: bool = true,
    tone: f32 = 4000.0
});
fx_struct!(ReverbFx {
    size: f32 = 0.7,
    damping: f32 = 0.5,
    mix: f32 = 0.25,
    predelay_ms: f32 = 10.0,
    width: f32 = 1.0
});
fx_struct!(ChorusFx {
    rate_hz: f32 = 0.8,
    depth_ms: f32 = 3.0,
    mix: f32 = 0.4
});
fx_struct!(CompressorFx {
    threshold_db: f32 = -18.0,
    ratio: f32 = 4.0,
    attack_ms: f32 = 10.0,
    release_ms: f32 = 120.0,
    makeup_db: f32 = 0.0
});
fx_struct!(SidechainFx {
    source: String = "kick".to_string(),
    amount: f32 = 0.7,
    release_ms: f32 = 180.0,
    attack_ms: f32 = 3.0
});
fx_struct!(WidthFx { amount: f32 = 1.4 });
fx_struct!(GainFx { db: f32 = 0.0 });
fx_struct!(LimiterFx {
    ceiling_db: f32 = -1.0,
    release_ms: f32 = 80.0
});
fx_struct!(TransientFx {
    attack: f32 = 0.5,
    sustain: f32 = 0.0
});

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Effect {
    Filter(FilterFx),
    Eq(EqFx),
    Distortion(DistortionFx),
    Bitcrush(BitcrushFx),
    Delay(DelayFx),
    Reverb(ReverbFx),
    Chorus(ChorusFx),
    Compressor(CompressorFx),
    Sidechain(SidechainFx),
    Width(WidthFx),
    Gain(GainFx),
    Limiter(LimiterFx),
    Transient(TransientFx),
}

/// (type, description) for every effect — surfaced to AIs.
pub const EFFECT_TYPES: &[(&str, &str)] = &[
    ("filter", "State-variable filter. mode lowpass|highpass|bandpass|notch, cutoff Hz, resonance 0..1"),
    ("eq", "3-band EQ. low_db, mid_db, high_db, low_freq, high_freq"),
    ("distortion", "Tanh saturation. drive 0..1, mix 0..1"),
    ("bitcrush", "Lo-fi crusher. bits 1..16, downsample 1..64, mix"),
    ("delay", "Tempo-synced stereo delay. steps (16ths, 3 = dotted 8th), feedback, mix, ping_pong, tone Hz"),
    ("reverb", "Freeverb-style room. size 0..1, damping 0..1, mix, predelay_ms, width"),
    ("chorus", "Stereo chorus. rate_hz, depth_ms, mix"),
    ("compressor", "Feed-forward compressor. threshold_db, ratio, attack_ms, release_ms, makeup_db"),
    ("sidechain", "Duck this track whenever `source` track hits (EDM pumping). source, amount 0..1, release_ms"),
    ("width", "Mid/side stereo width. amount 0 = mono, 1 = unchanged, 2 = extra wide"),
    ("gain", "Simple gain. db"),
    ("limiter", "Brickwall-ish peak limiter. ceiling_db, release_ms"),
    ("transient", "Transient shaper. attack -1..1 (punch), sustain -1..1 (tail)"),
];

impl Effect {
    pub fn from_type(t: &str) -> Option<Effect> {
        let v = serde_json::json!({ "type": t.to_lowercase() });
        serde_json::from_value(v).ok()
    }

    pub fn type_name(&self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v["type"].as_str().map(String::from))
            .unwrap_or_default()
    }

    pub fn process(&self, l: &mut [f32], r: &mut [f32], ctx: &FxContext) {
        match self {
            Effect::Filter(p) => {
                let (mut fl, mut fr) = (Svf::default(), Svf::default());
                for i in 0..l.len() {
                    l[i] = fl.process(l[i], p.cutoff, p.resonance, p.mode);
                    r[i] = fr.process(r[i], p.cutoff, p.resonance, p.mode);
                }
            }
            Effect::Eq(p) => {
                let (gl, gm, gh) = (
                    db_to_gain(p.low_db),
                    db_to_gain(p.mid_db),
                    db_to_gain(p.high_db),
                );
                for ch in [&mut *l, &mut *r] {
                    let (mut lo, mut hi) = (Svf::default(), Svf::default());
                    for s in ch.iter_mut() {
                        let low = lo.process(*s, p.low_freq, 0.0, FilterMode::Lowpass);
                        let high = hi.process(*s, p.high_freq, 0.0, FilterMode::Highpass);
                        let mid = *s - low - high;
                        *s = low * gl + mid * gm + high * gh;
                    }
                }
            }
            Effect::Distortion(p) => {
                let d = 1.0 + p.drive.clamp(0.0, 1.0) * 20.0;
                let norm = 1.0 / d.tanh();
                for s in l.iter_mut().chain(r.iter_mut()) {
                    let wet = (*s * d).tanh() * norm * (1.0 / (1.0 + p.drive * 2.0)).max(0.35);
                    *s = *s * (1.0 - p.mix) + wet * p.mix;
                }
            }
            Effect::Bitcrush(p) => {
                let levels = 2f32.powf(p.bits.clamp(1.0, 16.0));
                let ds = p.downsample.clamp(1, 64) as usize;
                for ch in [&mut *l, &mut *r] {
                    let mut held = 0.0;
                    for (i, s) in ch.iter_mut().enumerate() {
                        if i % ds == 0 {
                            held = (*s * levels).round() / levels;
                        }
                        *s = *s * (1.0 - p.mix) + held * p.mix;
                    }
                }
            }
            Effect::Delay(p) => {
                let d = ((p.steps * ctx.step_secs * SR) as usize).max(1);
                let mut bl = vec![0.0f32; d];
                let mut br = vec![0.0f32; d];
                let (mut tl, mut tr) = (Svf::default(), Svf::default());
                let fb = p.feedback.clamp(0.0, 0.95);
                for i in 0..l.len() {
                    let k = i % d;
                    let (dl, dr) = (bl[k], br[k]);
                    let (inl, inr) = (l[i], r[i]);
                    if p.ping_pong {
                        let mono = 0.5 * (inl + inr);
                        bl[k] = tl.process(mono + dr * fb, p.tone, 0.0, FilterMode::Lowpass);
                        br[k] = tr.process(dl * fb, p.tone, 0.0, FilterMode::Lowpass);
                    } else {
                        bl[k] = tl.process(inl + dl * fb, p.tone, 0.0, FilterMode::Lowpass);
                        br[k] = tr.process(inr + dr * fb, p.tone, 0.0, FilterMode::Lowpass);
                    }
                    l[i] = inl + dl * p.mix;
                    r[i] = inr + dr * p.mix;
                }
            }
            Effect::Reverb(p) => reverb(p, l, r),
            Effect::Chorus(p) => {
                let max = ((p.depth_ms + 20.0) * 0.001 * SR) as usize + 4;
                let mut bl = vec![0.0f32; max];
                let mut br = vec![0.0f32; max];
                for i in 0..l.len() {
                    let w = i % max;
                    bl[w] = l[i];
                    br[w] = r[i];
                    let t = i as f32 / SR;
                    let base = 12.0;
                    let dl = (base + p.depth_ms * (0.5 + 0.5 * (2.0 * PI * p.rate_hz * t).sin()))
                        * 0.001
                        * SR;
                    let dr = (base
                        + p.depth_ms * (0.5 + 0.5 * (2.0 * PI * p.rate_hz * t + PI / 2.0).cos()))
                        * 0.001
                        * SR;
                    let rd = |buf: &[f32], d: f32| {
                        let pos = (w as f32 - d).rem_euclid(max as f32);
                        let i0 = pos as usize % max;
                        let i1 = (i0 + 1) % max;
                        let f = pos - pos.floor();
                        buf[i0] * (1.0 - f) + buf[i1] * f
                    };
                    let (wl, wr) = (rd(&bl, dl), rd(&br, dr));
                    l[i] = l[i] * (1.0 - p.mix * 0.5) + wl * p.mix;
                    r[i] = r[i] * (1.0 - p.mix * 0.5) + wr * p.mix;
                }
            }
            Effect::Compressor(p) => {
                let att = (-1.0 / (p.attack_ms.max(0.1) * 0.001 * SR)).exp();
                let rel = (-1.0 / (p.release_ms.max(1.0) * 0.001 * SR)).exp();
                let makeup = db_to_gain(p.makeup_db);
                let mut env = 0.0f32;
                for i in 0..l.len() {
                    let x = l[i].abs().max(r[i].abs());
                    let c = if x > env { att } else { rel };
                    env = c * env + (1.0 - c) * x;
                    let db = gain_to_db(env);
                    let over = db - p.threshold_db;
                    let gr = if over > 0.0 {
                        over * (1.0 - 1.0 / p.ratio.max(1.0))
                    } else {
                        0.0
                    };
                    let g = db_to_gain(-gr) * makeup;
                    l[i] *= g;
                    r[i] *= g;
                }
            }
            Effect::Sidechain(p) => {
                let Some(trig) = ctx.triggers.get(&p.source.to_lowercase()) else {
                    return;
                };
                let rel_s = p.release_ms.max(5.0) * 0.001;
                let att_n = (p.attack_ms.max(0.1) * 0.001 * SR) as usize;
                let mut ti = 0usize;
                let mut last: Option<usize> = None;
                for i in 0..l.len() {
                    while ti < trig.len() && trig[ti] <= i {
                        last = Some(trig[ti]);
                        ti += 1;
                    }
                    if let Some(t0) = last {
                        let dt = i - t0;
                        let env = if dt < att_n {
                            dt as f32 / att_n.max(1) as f32
                        } else {
                            let x = (dt - att_n) as f32 / SR / rel_s;
                            if x >= 1.0 {
                                0.0
                            } else {
                                (1.0 - x).powf(1.6)
                            }
                        };
                        // env is "how ducked": rises quickly to 1, then recovers
                        let duck = env;
                        let g = 1.0 - p.amount.clamp(0.0, 1.0) * duck;
                        l[i] *= g;
                        r[i] *= g;
                    }
                }
            }
            Effect::Width(p) => {
                for i in 0..l.len() {
                    let m = 0.5 * (l[i] + r[i]);
                    let s = 0.5 * (l[i] - r[i]) * p.amount.clamp(0.0, 3.0);
                    l[i] = m + s;
                    r[i] = m - s;
                }
            }
            Effect::Gain(p) => {
                let g = db_to_gain(p.db);
                for s in l.iter_mut().chain(r.iter_mut()) {
                    *s *= g;
                }
            }
            Effect::Limiter(p) => limiter(p, l, r),
            Effect::Transient(p) => {
                let fast_c = (-1.0 / (0.001 * SR)).exp();
                let slow_c = (-1.0 / (0.03 * SR)).exp();
                let (mut fast, mut slow) = (0.0f32, 0.0f32);
                for i in 0..l.len() {
                    let x = l[i].abs().max(r[i].abs());
                    fast = fast_c * fast + (1.0 - fast_c) * x;
                    slow = slow_c * slow + (1.0 - slow_c) * x;
                    let diff = (fast - slow) / (slow + 1e-4);
                    let g = if diff > 0.0 {
                        1.0 + p.attack.clamp(-1.0, 1.0) * diff.min(2.0)
                    } else {
                        1.0 + p.sustain.clamp(-1.0, 1.0) * (-diff).min(1.0)
                    };
                    let g = g.clamp(0.1, 3.0);
                    l[i] *= g;
                    r[i] *= g;
                }
            }
        }
    }
}

struct Comb {
    buf: Vec<f32>,
    idx: usize,
    store: f32,
}

impl Comb {
    fn new(n: usize) -> Self {
        Comb {
            buf: vec![0.0; n.max(1)],
            idx: 0,
            store: 0.0,
        }
    }
    fn process(&mut self, x: f32, fb: f32, damp: f32) -> f32 {
        let out = self.buf[self.idx];
        self.store = out * (1.0 - damp) + self.store * damp;
        self.buf[self.idx] = x + self.store * fb;
        self.idx = (self.idx + 1) % self.buf.len();
        out
    }
}

struct Allpass {
    buf: Vec<f32>,
    idx: usize,
}

impl Allpass {
    fn new(n: usize) -> Self {
        Allpass {
            buf: vec![0.0; n.max(1)],
            idx: 0,
        }
    }
    fn process(&mut self, x: f32) -> f32 {
        let b = self.buf[self.idx];
        let out = -x + b;
        self.buf[self.idx] = x + b * 0.5;
        self.idx = (self.idx + 1) % self.buf.len();
        out
    }
}

fn reverb(p: &ReverbFx, l: &mut [f32], r: &mut [f32]) {
    const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
    const APS: [usize; 4] = [556, 441, 341, 225];
    const SPREAD: usize = 23;
    let fb = 0.7 + p.size.clamp(0.0, 1.0) * 0.28;
    let damp = p.damping.clamp(0.0, 1.0) * 0.4;
    let mut cl: Vec<Comb> = COMBS.iter().map(|&n| Comb::new(n)).collect();
    let mut cr: Vec<Comb> = COMBS.iter().map(|&n| Comb::new(n + SPREAD)).collect();
    let mut al: Vec<Allpass> = APS.iter().map(|&n| Allpass::new(n)).collect();
    let mut ar: Vec<Allpass> = APS.iter().map(|&n| Allpass::new(n + SPREAD)).collect();
    let pre = ((p.predelay_ms.max(0.0) * 0.001 * SR) as usize).max(1);
    let mut pbuf = vec![0.0f32; pre];
    let wet1 = p.mix * (p.width / 2.0 + 0.5);
    let wet2 = p.mix * ((1.0 - p.width) / 2.0);
    for i in 0..l.len() {
        let input = (l[i] + r[i]) * 0.015;
        let k = i % pre;
        let x = pbuf[k];
        pbuf[k] = input;
        let mut ol = 0.0;
        let mut or = 0.0;
        for c in cl.iter_mut() {
            ol += c.process(x, fb, damp);
        }
        for c in cr.iter_mut() {
            or += c.process(x, fb, damp);
        }
        for a in al.iter_mut() {
            ol = a.process(ol);
        }
        for a in ar.iter_mut() {
            or = a.process(or);
        }
        let dry = 1.0 - p.mix * 0.5;
        let (nl, nr) = (
            l[i] * dry + ol * wet1 + or * wet2,
            r[i] * dry + or * wet1 + ol * wet2,
        );
        l[i] = nl;
        r[i] = nr;
    }
}

fn limiter(p: &LimiterFx, l: &mut [f32], r: &mut [f32]) {
    let ceiling = db_to_gain(p.ceiling_db.min(0.0));
    let look = (0.002 * SR) as usize;
    let rel = (-1.0 / (p.release_ms.max(1.0) * 0.001 * SR)).exp();
    let n = l.len();
    // required gain per sample, then look-ahead min + smoothing
    let mut need: Vec<f32> = (0..n)
        .map(|i| {
            let x = l[i].abs().max(r[i].abs());
            if x > ceiling {
                ceiling / x
            } else {
                1.0
            }
        })
        .collect();
    // spread minima backwards over the lookahead window
    let mut run = 1.0f32;
    let mut countdown = 0usize;
    for i in (0..n).rev() {
        if need[i] < run || countdown == 0 {
            run = need[i];
            countdown = look;
        } else {
            countdown -= 1;
        }
        need[i] = need[i].min(run);
    }
    let mut g = 1.0f32;
    for i in 0..n {
        let target = need[i];
        g = if target < g {
            target
        } else {
            rel * g + (1.0 - rel) * target
        };
        l[i] = (l[i] * g).clamp(-ceiling, ceiling);
        r[i] = (r[i] * g).clamp(-ceiling, ceiling);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(t: &HashMap<String, Vec<usize>>) -> FxContext<'_> {
        FxContext {
            step_secs: 0.125,
            triggers: t,
        }
    }

    #[test]
    fn every_effect_type_parses_and_stays_finite() {
        let t = HashMap::from([("kick".to_string(), vec![0usize, 5000, 20000])]);
        for (name, _) in EFFECT_TYPES {
            let fx = Effect::from_type(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(&fx.type_name(), name);
            let mut l: Vec<f32> = (0..30000).map(|i| (i as f32 * 0.03).sin() * 0.8).collect();
            let mut r = l.clone();
            fx.process(&mut l, &mut r, &ctx(&t));
            assert!(l.iter().chain(r.iter()).all(|x| x.is_finite()), "{name}");
        }
    }

    #[test]
    fn limiter_respects_ceiling() {
        let t = HashMap::new();
        let mut l: Vec<f32> = (0..10000).map(|i| (i as f32 * 0.05).sin() * 3.0).collect();
        let mut r = l.clone();
        Effect::Limiter(LimiterFx {
            ceiling_db: -1.0,
            release_ms: 50.0,
        })
        .process(&mut l, &mut r, &ctx(&t));
        let c = db_to_gain(-1.0) + 1e-6;
        assert!(l.iter().all(|x| x.abs() <= c));
    }

    #[test]
    fn sidechain_ducks_after_trigger() {
        let t = HashMap::from([("kick".to_string(), vec![1000usize])]);
        let mut l = vec![1.0f32; 20000];
        let mut r = l.clone();
        Effect::Sidechain(SidechainFx::default()).process(&mut l, &mut r, &ctx(&t));
        assert_eq!(l[500], 1.0);
        assert!(l[1300] < 0.5);
        assert!(l[19000] > 0.99);
    }
}
