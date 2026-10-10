//! Stereo effects rack: filter, EQ, drive, bitcrush, tempo-synced delay,
//! reverb, chorus, compressor, sidechain ducking, stereo width, gain, limiter,
//! plus the studio processors in `fx_extra` (parametric/dynamic EQ,
//! multiband, de-esser, soft clipper, gate, phaser, flanger, stutter,
//! pitch shift, convolution reverb, auto-pan). Any effect can be bypassed.

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

fn is_false(b: &bool) -> bool {
    !*b
}

macro_rules! fx_struct {
    ($name:ident { $($field:ident : $ty:ty = $def:expr),* $(,)? }) => {
        #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
        #[serde(default)]
        pub struct $name {
            $(pub $field: $ty,)*
            /// Stable id within its chain (e.g. "reverb1"); assigned by the
            /// engine, survives reordering, usable wherever an index is.
            #[serde(default, skip_serializing_if = "String::is_empty")]
            pub id: String,
            /// Bypassed effects stay in the chain but pass audio untouched.
            #[serde(skip_serializing_if = "is_false")]
            pub bypass: bool,
        }
        impl Default for $name {
            fn default() -> Self { $name { $($field: $def,)* id: String::new(), bypass: false } }
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
    width: f32 = 1.0,
    mode: ReverbMode = ReverbMode::Freeverb,
    decay_s: f32 = 0.0,
    diffusion: f32 = 0.75,
    low_cut_hz: f32 = 20.0
});

/// Reverb algorithm: the classic Freeverb, or a SoundCraft-derived 8-line
/// feedback delay network (smoother, denser, decay in seconds).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReverbMode {
    #[default]
    Freeverb,
    FdnRoom,
    FdnPlate,
    FdnHall,
}
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
    makeup_db: f32 = 0.0,
    knee_db: f32 = 0.0,
    lookahead_ms: f32 = 0.0,
    sc_hpf_hz: f32 = 0.0,
    mix: f32 = 1.0
});
fx_struct!(SidechainFx {
    source: String = "kick".to_string(),
    amount: f32 = 0.7,
    release_ms: f32 = 180.0,
    attack_ms: f32 = 3.0
});
fx_struct!(WidthFx { amount: f32 = 1.4, low_mono_hz: f32 = 0.0 });
fx_struct!(GainFx { db: f32 = 0.0 });
fx_struct!(LimiterFx {
    ceiling_db: f32 = -1.0,
    release_ms: f32 = 80.0,
    true_peak: bool = true
});
fx_struct!(TransientFx {
    attack: f32 = 0.5,
    sustain: f32 = 0.0
});

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EqBandKind {
    #[default]
    Bell,
    LowShelf,
    HighShelf,
    LowCut,
    HighCut,
    Notch,
}

/// One band of the parametric EQ.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EqBand {
    pub kind: EqBandKind,
    pub freq: f32,
    pub gain_db: f32,
    pub q: f32,
    /// Cut filters only: 1 = 12 dB/oct, 2 = 24, 4 = 48.
    pub stages: u32,
    pub enabled: bool,
}

impl Default for EqBand {
    fn default() -> Self {
        EqBand {
            kind: EqBandKind::Bell,
            freq: 1000.0,
            gain_db: 0.0,
            q: 0.707,
            stages: 1,
            enabled: true,
        }
    }
}

fx_struct!(ParametricEqFx {
    bands: Vec<EqBand> = vec![EqBand::default()],
    output_db: f32 = 0.0
});
fx_struct!(MultibandFx {
    low_freq: f32 = 200.0,
    high_freq: f32 = 3000.0,
    low_threshold_db: f32 = -20.0,
    mid_threshold_db: f32 = -20.0,
    high_threshold_db: f32 = -22.0,
    low_ratio: f32 = 3.0,
    mid_ratio: f32 = 2.0,
    high_ratio: f32 = 2.5,
    attack_ms: f32 = 15.0,
    release_ms: f32 = 150.0,
    low_gain_db: f32 = 0.0,
    mid_gain_db: f32 = 0.0,
    high_gain_db: f32 = 0.0,
    mix: f32 = 1.0
});
fx_struct!(DynamicEqFx {
    freq: f32 = 3000.0,
    q: f32 = 1.5,
    threshold_db: f32 = -26.0,
    ratio: f32 = 3.0,
    range_db: f32 = -8.0,
    attack_ms: f32 = 3.0,
    release_ms: f32 = 90.0,
    source: String = String::new()
});
fx_struct!(DeesserFx {
    freq: f32 = 6000.0,
    threshold_db: f32 = -30.0,
    range_db: f32 = -10.0,
    release_ms: f32 = 60.0
});
fx_struct!(SoftClipFx {
    threshold_db: f32 = -4.0,
    ceiling_db: f32 = -0.3,
    drive_db: f32 = 0.0,
    mix: f32 = 1.0,
    oversample: bool = true
});
fx_struct!(GateFx {
    threshold_db: f32 = -40.0,
    range_db: f32 = -60.0,
    attack_ms: f32 = 0.5,
    hold_ms: f32 = 30.0,
    release_ms: f32 = 80.0
});
fx_struct!(PhaserFx {
    rate_hz: f32 = 0.4,
    sync_steps: f32 = 0.0,
    depth: f32 = 0.7,
    center_hz: f32 = 900.0,
    feedback: f32 = 0.4,
    stages: u32 = 6,
    mix: f32 = 0.5
});
fx_struct!(FlangerFx {
    rate_hz: f32 = 0.2,
    sync_steps: f32 = 0.0,
    delay_ms: f32 = 1.5,
    depth_ms: f32 = 2.5,
    feedback: f32 = 0.5,
    mix: f32 = 0.5
});

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StutterMode {
    #[default]
    Gate,
    Stutter,
    HalfTime,
    Reverse,
    TapeStop,
}

fx_struct!(StutterFx {
    mode: StutterMode = StutterMode::Gate,
    pattern: String = "x.x.xx.xx.x.x.xx".to_string(),
    slice_steps: f32 = 1.0,
    cycle_steps: f32 = 4.0,
    smooth_ms: f32 = 3.0,
    mix: f32 = 1.0
});
fx_struct!(GrossBeatFx {
    time: String = "none".to_string(),
    volume: String = "none".to_string(),
    time_points: String = String::new(),
    volume_points: String = String::new(),
    cycle_steps: f32 = 16.0,
    smooth_ms: f32 = 4.0,
    mix: f32 = 1.0
});
fx_struct!(VocoderFx {
    notes: String = "C3 Eb3 G3".to_string(),
    bands: u32 = 16,
    low_hz: f32 = 100.0,
    high_hz: f32 = 8000.0,
    attack_ms: f32 = 5.0,
    release_ms: f32 = 40.0,
    noise: f32 = 0.08,
    sibilance: f32 = 0.5,
    gain: f32 = 1.0,
    mix: f32 = 1.0
});
fx_struct!(PitchShiftFx {
    semitones: f32 = 12.0,
    cents: f32 = 0.0,
    window_ms: f32 = 60.0,
    mix: f32 = 1.0
});

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum IrSpace {
    Room,
    #[default]
    Hall,
    Plate,
    Chamber,
    Spring,
    Cathedral,
}

fx_struct!(ConvolutionFx {
    space: IrSpace = IrSpace::Hall,
    decay_s: f32 = 2.0,
    predelay_ms: f32 = 15.0,
    damping: f32 = 0.4,
    early: f32 = 0.5,
    width: f32 = 1.0,
    mix: f32 = 0.25
});
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SatMode {
    #[default]
    Tape,
    Tube,
    Transistor,
    Diode,
    Fold,
    Exciter,
}

fx_struct!(SaturatorFx {
    mode: SatMode = SatMode::Tape,
    drive_db: f32 = 6.0,
    tone_hz: f32 = 16000.0,
    bias: f32 = 0.0,
    mix: f32 = 1.0,
    output_db: f32 = 0.0,
    oversample: bool = true
});
fx_struct!(HaasFx {
    delay_ms: f32 = 14.0,
    side: i32 = 1,
    mix: f32 = 1.0,
    low_cut_hz: f32 = 150.0,
    level_db: f32 = 0.0
});
fx_struct!(AutopanFx {
    steps: f32 = 8.0,
    depth: f32 = 0.7,
    tremolo: bool = false
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
    ParametricEq(ParametricEqFx),
    Multiband(MultibandFx),
    DynamicEq(DynamicEqFx),
    Deesser(DeesserFx),
    SoftClipper(SoftClipFx),
    Gate(GateFx),
    Phaser(PhaserFx),
    Flanger(FlangerFx),
    Stutter(StutterFx),
    PitchShift(PitchShiftFx),
    Convolution(ConvolutionFx),
    Autopan(AutopanFx),
    Saturator(SaturatorFx),
    Haas(HaasFx),
    GrossBeat(GrossBeatFx),
    Vocoder(VocoderFx),
}

macro_rules! each_fx {
    ($e:expr, $p:ident => $body:expr) => {
        match $e {
            Effect::Filter($p) => $body,
            Effect::Eq($p) => $body,
            Effect::Distortion($p) => $body,
            Effect::Bitcrush($p) => $body,
            Effect::Delay($p) => $body,
            Effect::Reverb($p) => $body,
            Effect::Chorus($p) => $body,
            Effect::Compressor($p) => $body,
            Effect::Sidechain($p) => $body,
            Effect::Width($p) => $body,
            Effect::Gain($p) => $body,
            Effect::Limiter($p) => $body,
            Effect::Transient($p) => $body,
            Effect::ParametricEq($p) => $body,
            Effect::Multiband($p) => $body,
            Effect::DynamicEq($p) => $body,
            Effect::Deesser($p) => $body,
            Effect::SoftClipper($p) => $body,
            Effect::Gate($p) => $body,
            Effect::Phaser($p) => $body,
            Effect::Flanger($p) => $body,
            Effect::Stutter($p) => $body,
            Effect::PitchShift($p) => $body,
            Effect::Convolution($p) => $body,
            Effect::Autopan($p) => $body,
            Effect::Saturator($p) => $body,
            Effect::Haas($p) => $body,
            Effect::GrossBeat($p) => $body,
            Effect::Vocoder($p) => $body,
        }
    };
}

/// (type, description) for every effect — surfaced to AIs.
pub const EFFECT_TYPES: &[(&str, &str)] = &[
    ("filter", "State-variable filter. mode lowpass|highpass|bandpass|notch, cutoff Hz, resonance 0..1"),
    ("eq", "3-band EQ. low_db, mid_db, high_db, low_freq, high_freq"),
    ("distortion", "Tanh saturation. drive 0..1, mix 0..1"),
    ("bitcrush", "Lo-fi crusher. bits 1..16, downsample 1..64, mix"),
    ("delay", "Tempo-synced stereo delay. steps (16ths, 3 = dotted 8th), feedback, mix, ping_pong, tone Hz"),
    ("reverb", "Reverb. mode freeverb (default) | fdn_room | fdn_plate | fdn_hall (8-line feedback delay network: smoother, denser). size 0..1, damping 0..1, mix, predelay_ms, width; FDN only: decay_s (RT60 seconds, 0 = from size), diffusion 0..1, low_cut_hz"),
    ("chorus", "Stereo chorus. rate_hz, depth_ms, mix"),
    ("compressor", "Feed-forward compressor. threshold_db, ratio, attack_ms, release_ms, makeup_db; soft-knee lookahead mode when any of knee_db (e.g. 6), lookahead_ms (0-10), sc_hpf_hz (sidechain high-pass, e.g. 120 so the kick does not pump), mix (<1 = parallel) is set"),
    ("sidechain", "Duck this track whenever `source` track hits (EDM pumping). source, amount 0..1, release_ms"),
    ("width", "Mid/side stereo width. amount 0 = mono, 1 = unchanged, 2 = extra wide; low_mono_hz (e.g. 150) keeps everything below it mono"),
    ("gain", "Simple gain. db"),
    ("limiter", "Lookahead brickwall limiter. ceiling_db, release_ms, true_peak (default true: detects inter-sample peaks so the ceiling holds in dBTP)"),
    ("transient", "Transient shaper. attack -1..1 (punch), sustain -1..1 (tail)"),
    ("parametric_eq", "Up to 8-band parametric EQ. bands=[{kind bell|low_shelf|high_shelf|low_cut|high_cut|notch, freq Hz, gain_db, q, stages (cuts: 1=12 dB/oct, 2=24, 4=48), enabled}], output_db"),
    ("multiband", "3-band compressor (Maximus-style). low_freq/high_freq crossovers, {low,mid,high}_threshold_db, _ratio, _gain_db, attack_ms, release_ms, mix"),
    ("dynamic_eq", "Dynamic bell: cuts (range_db<0) or boosts (range_db>0) a band only when it exceeds threshold_db. freq, q, threshold_db, ratio, range_db, attack_ms, release_ms. Tames harsh resonances, boomy notes. With source (a track name, e.g. vocal) it is KEYED: the band dips by range_db while that track plays (notes or audio-clip word onsets), so a pad gives the voice its 300-900 Hz body only when the voice is there"),
    ("deesser", "De-esser: dynamically ducks highs above freq when they spike. freq Hz, threshold_db, range_db (max cut), release_ms"),
    ("soft_clipper", "Smooth soft clipper for loud drums/masters. threshold_db (knee start), ceiling_db, drive_db, mix, oversample"),
    ("gate", "Noise gate / expander. threshold_db, range_db (closed attenuation), attack_ms, hold_ms, release_ms"),
    ("phaser", "Allpass phaser. rate_hz or sync_steps (16ths per cycle), depth 0..1, center_hz, feedback, stages 2-12, mix"),
    ("flanger", "Through-zero-style flanger. rate_hz or sync_steps, delay_ms, depth_ms, feedback -0.95..0.95, mix"),
    ("stutter", "Tempo-synced gross-beat style FX. mode gate|stutter|half_time|reverse|tape_stop, pattern (gate: 'x.x.xx..' one char per 16th), slice_steps (stutter repeat length), cycle_steps, smooth_ms, mix (automate fx.<i>.mix 0->1 to apply only at a transition)"),
    ("pitch_shift", "Granular pitch shifter. semitones -24..24, cents, window_ms, mix (octave-up doubles, chipmunk/dark vocal FX)"),
    ("convolution", "Convolution reverb with synthesized impulse responses. space room|hall|plate|chamber|spring|cathedral, decay_s, predelay_ms, damping, early (reflections), width, mix"),
    ("autopan", "Tempo-synced auto-pan or tremolo. steps (16ths per cycle), depth 0..1, tremolo (true = volume instead of pan)"),
    ("saturator", "Character saturator. mode tape (soft, even+odd warmth, high-end roll-off) | tube (asymmetric, even harmonics) | transistor (hard odd-harmonic edge) | diode (clipped, gritty) | fold (wavefolder, metallic) | exciter (adds only new harmonics above a corner of tone_hz, capped 1.5-8 kHz). drive_db 0..36, tone_hz (post low-pass), bias -0.5..0.5 (asymmetry), mix, output_db, oversample (2x, less aliasing)"),
    ("gross_beat", "Gross Beat-style time + volume FX looping every cycle_steps 16ths (16 = 1 bar). time preset none|half_speed|half_speed_end|repeat_beat|repeat_end_8th|repeat_end_16th|reverse_end|reverse|tape_stop|tape_stop_end|scratch_end|freeze_end|double_speed; volume preset none|trance_gate|trance_gate_8th|pump|pump_hard|tresillo|offbeat|fade_in|fade_out|stop_end|swell; or draw them: time_points / volume_points 'u:v, u:v' (u = 0..1 through the cycle; time v = source position 0..1, volume v = gain). smooth_ms, mix (automate mix to apply it only on a transition)"),
    ("vocoder", "Channel vocoder (Vocodex-style): the track's own audio (a vocal) is the modulator, a built-in detuned saw chord on notes ('C3 Eb3 G3' or MIDI '48,51,55', up to 8) is the carrier, so the chord speaks the words: robot voice, talk-box lead, vocoded hook. bands 4..40 (more = clearer words), low_hz/high_hz band range, attack_ms/release_ms (band envelope), noise 0..1 (carrier noise for consonants), sibilance 0..1 (the voice's own top above 5 kHz passes through), gain, mix"),
    ("haas", "Haas widener: delays one side by delay_ms (1..40) for width without comb filtering the mix. side 1 = delay right, -1 = delay left, low_cut_hz keeps bass centred (only highs are widened), level_db trims the delayed side, mix"),
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

    pub fn id(&self) -> &str {
        each_fx!(self, p => p.id.as_str())
    }

    pub fn set_id(&mut self, id: &str) {
        each_fx!(self, p => p.id = id.to_string())
    }

    /// Parameter checks serde cannot express (preset names, drawn points).
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Effect::GrossBeat(p) => crate::fx_time::check(p),
            Effect::Vocoder(p) => crate::fx_vocoder::check(p),
            _ => Ok(()),
        }
    }

    pub fn bypassed(&self) -> bool {
        each_fx!(self, p => p.bypass)
    }

    pub fn set_bypass(&mut self, on: bool) {
        each_fx!(self, p => p.bypass = on)
    }

    pub fn process(&self, l: &mut [f32], r: &mut [f32], ctx: &FxContext) {
        if self.bypassed() {
            return;
        }
        use crate::fx_extra as x;
        match self {
            Effect::ParametricEq(p) => x::parametric_eq(p, l, r),
            Effect::Multiband(p) => x::multiband(p, l, r),
            Effect::DynamicEq(p) => {
                if p.source.is_empty() {
                    x::dynamic_eq(p, l, r)
                } else if let Some(trig) = ctx.triggers.get(&p.source.to_lowercase()) {
                    x::keyed_dynamic_eq(p, trig, l, r)
                }
            }
            Effect::Deesser(p) => x::deesser(p, l, r),
            Effect::SoftClipper(p) => x::soft_clipper(p, l, r),
            Effect::Gate(p) => x::gate(p, l, r),
            Effect::Phaser(p) => x::phaser(p, l, r, ctx.step_secs),
            Effect::Flanger(p) => x::flanger(p, l, r, ctx.step_secs),
            Effect::Stutter(p) => x::stutter(p, l, r, ctx.step_secs),
            Effect::PitchShift(p) => x::pitch_shift(p, l, r),
            Effect::Convolution(p) => x::convolution(p, l, r),
            Effect::Autopan(p) => x::autopan(p, l, r, ctx.step_secs),
            Effect::Saturator(p) => crate::fx_sat::saturator(p, l, r),
            Effect::Haas(p) => crate::fx_sat::haas(p, l, r),
            Effect::GrossBeat(p) => crate::fx_time::gross_beat(p, l, r, ctx.step_secs),
            Effect::Vocoder(p) => crate::fx_vocoder::vocoder(p, l, r),
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
            Effect::Compressor(p)
                if p.knee_db > 0.0 || p.lookahead_ms > 0.0 || p.sc_hpf_hz > 0.0 || p.mix < 1.0 =>
            {
                crate::sc_dsp::compress(
                    &crate::sc_dsp::CompParams {
                        threshold_db: p.threshold_db,
                        ratio: p.ratio,
                        attack_ms: p.attack_ms,
                        release_ms: p.release_ms,
                        knee_db: p.knee_db,
                        makeup_db: p.makeup_db,
                        sc_hpf_hz: p.sc_hpf_hz,
                        lookahead_ms: p.lookahead_ms,
                        mix: p.mix,
                    },
                    l,
                    r,
                );
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
                // duck level when the current trigger arrived: a retrigger
                // during the release ramps on from there (no jump back to 0)
                let mut from = 0.0f32;
                let mut prev = 0.0f32;
                for i in 0..l.len() {
                    while ti < trig.len() && trig[ti] <= i {
                        if last != Some(trig[ti]) {
                            from = prev;
                        }
                        last = Some(trig[ti]);
                        ti += 1;
                    }
                    if let Some(t0) = last {
                        let dt = i - t0;
                        let env = if dt < att_n {
                            from + (1.0 - from) * dt as f32 / att_n.max(1) as f32
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
                        prev = duck;
                        let g = 1.0 - p.amount.clamp(0.0, 1.0) * duck;
                        l[i] *= g;
                        r[i] *= g;
                    }
                }
            }
            Effect::Width(p) => {
                // low_mono_hz > 0: the side signal is high-passed there, so the
                // low end stays mono (bass translates on phones and clubs)
                let mut hp = Svf::default();
                let mono_lo = p.low_mono_hz.clamp(0.0, 500.0);
                for i in 0..l.len() {
                    let m = 0.5 * (l[i] + r[i]);
                    let mut s = 0.5 * (l[i] - r[i]);
                    if mono_lo > 0.0 {
                        s = hp.process(s, mono_lo, 0.0, FilterMode::Highpass);
                    }
                    let s = s * p.amount.clamp(0.0, 3.0);
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
    if p.mode != ReverbMode::Freeverb {
        use crate::sc_dsp::{fdn_reverb, FdnParams, HALL, PLATE, ROOM};
        let (cfg, default_decay) = match p.mode {
            ReverbMode::FdnPlate => (&PLATE, 2.5),
            ReverbMode::FdnHall => (&HALL, 3.5),
            _ => (&ROOM, 1.2),
        };
        let decay = if p.decay_s > 0.0 {
            p.decay_s
        } else {
            default_decay * (0.4 + 1.2 * p.size.clamp(0.0, 1.0))
        };
        let fp = FdnParams {
            decay_s: decay,
            size: p.size,
            damping_hz: 18000.0 * 0.12f32.powf(p.damping.clamp(0.0, 1.0)),
            diffusion: p.diffusion,
            predelay_ms: p.predelay_ms,
            width: p.width,
            low_cut_hz: p.low_cut_hz,
            mix: p.mix,
        };
        fdn_reverb(cfg, &fp, l, r);
        return;
    }
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

/// Brickwall limiter. With `true_peak` the output is verified with a 4x
/// oversampled (Kaiser windowed-sinc) true-peak meter and re-limited with a
/// tighter internal ceiling until the measured dBTP meets the setting.
fn limiter(p: &LimiterFx, l: &mut [f32], r: &mut [f32]) {
    if !p.true_peak {
        limiter_pass(p, p.ceiling_db.min(0.0), l, r);
        return;
    }
    let (l0, r0) = (l.to_vec(), r.to_vec());
    let target = p.ceiling_db.min(0.0);
    let mut internal = target;
    for _ in 0..4 {
        l.copy_from_slice(&l0);
        r.copy_from_slice(&r0);
        limiter_pass(p, internal, l, r);
        let tp = crate::resample::true_peak(l).max(crate::resample::true_peak(r));
        let tp_db = gain_to_db(tp);
        if tp_db <= target + 0.01 {
            return;
        }
        internal -= tp_db - target + 0.05;
    }
    // last resort: static trim so the ceiling holds exactly
    let tp = crate::resample::true_peak(l).max(crate::resample::true_peak(r));
    let g = db_to_gain(target) / tp.max(1e-9);
    if g < 1.0 {
        for v in l.iter_mut().chain(r.iter_mut()) {
            *v *= g;
        }
    }
}

fn limiter_pass(p: &LimiterFx, ceiling_db: f32, l: &mut [f32], r: &mut [f32]) {
    let ceiling = db_to_gain(ceiling_db);
    let look = (0.002 * SR) as usize;
    let rel = (-1.0 / (p.release_ms.max(1.0) * 0.001 * SR)).exp();
    let n = l.len();
    // inter-sample peak estimate: 4x polyphase windowed-sinc (16 taps per
    // phase) at 1/4, 1/2, 3/4 between samples, plus a hair of margin
    const HALF: isize = 8;
    let kernels: Vec<Vec<f32>> = (1..4)
        .map(|ph| {
            let frac = ph as f32 / 4.0;
            let h: Vec<f32> = (-HALF + 1..=HALF)
                .map(|k| {
                    let t = k as f32 - frac;
                    let sinc = (PI * t).sin() / (PI * t);
                    sinc * (0.5 + 0.5 * (PI * t / HALF as f32).cos())
                })
                .collect();
            let sum: f32 = h.iter().sum();
            h.into_iter().map(|v| v / sum).collect()
        })
        .collect();
    let isp = |x: &[f32], i: usize| -> f32 {
        let mut m = x[i].abs();
        let lo = i as isize - HALF + 1;
        let inside = lo >= 0 && (i as isize + HALF) < n as isize;
        for h in &kernels {
            let mut acc = 0.0f32;
            if inside {
                let w = &x[lo as usize..lo as usize + h.len()];
                for (c, v) in h.iter().zip(w) {
                    acc += c * v;
                }
            } else {
                for (j, c) in h.iter().enumerate() {
                    let k = lo + j as isize;
                    if k >= 0 && k < n as isize {
                        acc += c * x[k as usize];
                    }
                }
            }
            m = m.max(acc.abs());
        }
        m * 1.06
    };
    // required gain per sample, then look-ahead min + smoothing
    let mut need: Vec<f32> = (0..n)
        .map(|i| {
            let x = if p.true_peak {
                isp(l, i).max(isp(r, i))
            } else {
                l[i].abs().max(r[i].abs())
            };
            if x > ceiling {
                ceiling / x
            } else {
                1.0
            }
        })
        .collect();
    // exact sliding minimum over the lookahead window ahead of each sample
    let mut held = vec![1.0f32; n];
    let mut dq: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    for i in (0..n).rev() {
        while dq.back().map(|&j| need[j] >= need[i]).unwrap_or(false) {
            dq.pop_back();
        }
        dq.push_back(i);
        while dq.front().map(|&j| j > i + look).unwrap_or(false) {
            dq.pop_front();
        }
        held[i] = need[dq[0]];
    }
    // box-average the held curve over the previous `look` samples: every
    // sample in that box already covers the peak, so the average still
    // reaches the required gain in time, but as a ramp instead of a step
    // (a gain step on a loud 808 is an audible click)
    let mut acc = 0.0f64;
    for i in 0..n {
        acc += held[i] as f64;
        if i > look {
            acc -= held[i - look - 1] as f64;
        }
        let cnt = (i.min(look) + 1) as f64;
        need[i] = (acc / cnt) as f32;
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

/// Give every effect in a chain a unique stable id ("<type><n>"); returns
/// true when anything changed (new effects, legacy projects, duplicates).
pub fn ensure_ids(chain: &mut [Effect]) -> bool {
    let mut seen: Vec<String> = Vec::new();
    let mut changed = false;
    for i in 0..chain.len() {
        let cur = chain[i].id().to_string();
        if cur.is_empty() || seen.contains(&cur) {
            let t = chain[i].type_name();
            let mut n = 1;
            let id = loop {
                let cand = format!("{t}{n}");
                if !seen.contains(&cand) && !chain.iter().any(|e| e.id() == cand) {
                    break cand;
                }
                n += 1;
            };
            chain[i].set_id(&id);
            seen.push(id);
            changed = true;
        } else {
            seen.push(cur);
        }
    }
    changed
}

/// Position of an effect addressed by index ("2", 2) or stable id ("reverb1").
pub fn find(chain: &[Effect], key: &str) -> Option<usize> {
    let k = key.trim();
    if let Ok(i) = k.parse::<usize>() {
        return (i < chain.len()).then_some(i);
    }
    chain.iter().position(|e| e.id().eq_ignore_ascii_case(k))
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
    fn limiter_holds_true_peak_ceiling() {
        // bright, dense material slammed 8 dB into a -1.2 dB ceiling
        let t = HashMap::new();
        let mut rng = crate::dsp::Rng::new(9);
        let mut l: Vec<f32> = (0..44100)
            .map(|i| {
                let s = (i as f32 * 0.31).sin() + 0.6 * (i as f32 * 1.7).sin();
                (s + 0.5 * rng.bipolar()) * 1.6
            })
            .collect();
        let mut r = l.clone();
        Effect::Limiter(LimiterFx {
            ceiling_db: -1.2,
            release_ms: 60.0,
            ..Default::default()
        })
        .process(&mut l, &mut r, &ctx(&t));
        let tp = gain_to_db(crate::analysis::true_peak(&l).max(crate::analysis::true_peak(&r)));
        assert!(tp <= -1.15, "true peak {tp} dBTP over a -1.2 ceiling");
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
    fn limiter_gain_ramps_instead_of_stepping() {
        // a quiet 60 Hz sine that jumps (at a zero crossing) to a level the
        // limiter must pull down: the gain change may not be a click
        let t = HashMap::new();
        let w = 2.0 * PI * 60.0 / SR;
        let mut l: Vec<f32> = (0..30000)
            .map(|i| (i as f32 * w).sin() * if i < 7350 { 0.3 } else { 1.6 })
            .collect();
        assert!(crate::ears::clicks(&l, 5).is_empty());
        let mut r = l.clone();
        Effect::Limiter(LimiterFx {
            ceiling_db: -1.0,
            release_ms: 80.0,
            ..Default::default()
        })
        .process(&mut l, &mut r, &ctx(&t));
        let c = crate::ears::clicks(&l, 5);
        assert!(c.is_empty(), "limiter clicks: {c:?}");
    }

    #[test]
    fn limiter_respects_ceiling() {
        let t = HashMap::new();
        let mut l: Vec<f32> = (0..10000).map(|i| (i as f32 * 0.05).sin() * 3.0).collect();
        let mut r = l.clone();
        Effect::Limiter(LimiterFx {
            ceiling_db: -1.0,
            release_ms: 50.0,
            ..Default::default()
        })
        .process(&mut l, &mut r, &ctx(&t));
        let c = db_to_gain(-1.0) + 1e-6;
        assert!(l.iter().all(|x| x.abs() <= c));
    }

    #[test]
    fn sidechain_retrigger_is_continuous() {
        // a second kick inside the release must not snap the gain back up
        let t = HashMap::from([("kick".to_string(), vec![1000usize, 4000])]);
        let mut l = vec![1.0f32; 20000];
        let mut r = l.clone();
        Effect::Sidechain(SidechainFx {
            amount: 0.8,
            release_ms: 300.0,
            ..Default::default()
        })
        .process(&mut l, &mut r, &ctx(&t));
        let jump = l
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(jump < 0.01, "gain jumps by {jump}");
        assert!(l[4200] < 0.3);
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

    #[test]
    fn keyed_dynamic_eq_dips_only_while_the_key_plays() {
        // a 500 Hz tone; the vocal key plays from sample 20000
        let t = HashMap::from([("vocal".to_string(), vec![20000usize, 24000])]);
        let n = 60000;
        let mut l: Vec<f32> = (0..n).map(|i| (2.0 * std::f32::consts::PI * 500.0 * i as f32 / SR).sin() * 0.5).collect();
        let mut r = l.clone();
        let fx = Effect::DynamicEq(DynamicEqFx { freq: 520.0, q: 0.85, range_db: -6.0, source: "vocal".into(), release_ms: 300.0, attack_ms: 10.0, ..Default::default() });
        fx.process(&mut l, &mut r, &ctx(&t));
        let rms = |a: &[f32]| (a.iter().map(|x| x * x).sum::<f32>() / a.len() as f32).sqrt();
        let before = rms(&l[10000..19000]);
        let during = rms(&l[26000..34000]);
        let after = rms(&l[54000..60000]);
        assert!((before - 0.3535).abs() < 0.02, "untouched before the key: {before}");
        assert!(during < before * 0.65, "dips under the key: {during} vs {before}");
        assert!(after > before * 0.95, "recovers after: {after}");
        // no key track: untouched
        let mut l2 = vec![0.3f32; 1000];
        let mut r2 = l2.clone();
        Effect::DynamicEq(DynamicEqFx { source: "nobody".into(), ..Default::default() }).process(&mut l2, &mut r2, &ctx(&t));
        assert_eq!(l2[999], 0.3);
    }
}
