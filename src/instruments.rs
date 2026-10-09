//! Sound sources: synthesized drums, a subtractive synth, FM, Karplus-Strong
//! pluck, an 808 bass and a pitched sampler. Every instrument renders one note
//! into a mono buffer.

use crate::dsp::*;
use crate::samples::SampleBank;
use serde::{Deserialize, Serialize};
use std::f32::consts::PI;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DrumKind {
    Kick,
    Snare,
    Clap,
    ClosedHat,
    OpenHat,
    Rim,
    Tom,
    Cowbell,
    Shaker,
    Crash,
}

impl DrumKind {
    pub const ALL: [DrumKind; 10] = [
        DrumKind::Kick,
        DrumKind::Snare,
        DrumKind::Clap,
        DrumKind::ClosedHat,
        DrumKind::OpenHat,
        DrumKind::Rim,
        DrumKind::Tom,
        DrumKind::Cowbell,
        DrumKind::Shaker,
        DrumKind::Crash,
    ];
}

fn one() -> f32 {
    1.0
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DrumParams {
    pub kind: DrumKind,
    /// Pitch offset in semitones.
    #[serde(default)]
    pub tune: f32,
    /// Decay multiplier (1 = default length).
    #[serde(default = "one")]
    pub decay: f32,
    /// Saturation amount 0..1.
    #[serde(default)]
    pub drive: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SynthParams {
    pub osc1: Wave,
    pub osc2: Wave,
    /// osc2 pitch offset in semitones.
    pub osc2_semitones: f32,
    /// osc2 fine detune in cents.
    pub osc2_cents: f32,
    /// 0 = only osc1, 1 = only osc2.
    pub osc_mix: f32,
    /// Number of stacked detuned voices (1..=9) — supersaw style.
    pub unison: u8,
    pub unison_spread_cents: f32,
    /// Sine sub oscillator one octave down, 0..1.
    pub sub_level: f32,
    pub noise_level: f32,
    pub filter_mode: FilterMode,
    pub cutoff: f32,
    pub resonance: f32,
    /// Filter envelope depth in octaves (can be negative).
    pub filter_env_amount: f32,
    pub filter_env: Adsr,
    pub amp_env: Adsr,
    pub lfo_rate: f32,
    /// LFO to filter cutoff, in octaves.
    pub lfo_to_cutoff: f32,
    /// LFO to pitch, in semitones (vibrato).
    pub lfo_to_pitch: f32,
    /// Pitch drop from this many semitones above at note start (0 = off).
    pub pitch_env_semitones: f32,
    pub pitch_env_time: f32,
    pub drive: f32,
    pub gain: f32,
}

impl Default for SynthParams {
    fn default() -> Self {
        SynthParams {
            osc1: Wave::Saw,
            osc2: Wave::Saw,
            osc2_semitones: 0.0,
            osc2_cents: 7.0,
            osc_mix: 0.5,
            unison: 1,
            unison_spread_cents: 15.0,
            sub_level: 0.0,
            noise_level: 0.0,
            filter_mode: FilterMode::Lowpass,
            cutoff: 4000.0,
            resonance: 0.15,
            filter_env_amount: 0.0,
            filter_env: Adsr::new(0.005, 0.3, 0.0, 0.2),
            amp_env: Adsr::new(0.005, 0.25, 0.7, 0.2),
            lfo_rate: 0.0,
            lfo_to_cutoff: 0.0,
            lfo_to_pitch: 0.0,
            pitch_env_semitones: 0.0,
            pitch_env_time: 0.05,
            drive: 0.0,
            gain: 0.6,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FmParams {
    /// Modulator : carrier frequency ratio.
    pub ratio: f32,
    /// Modulation index (brightness).
    pub index: f32,
    pub mod_env: Adsr,
    pub amp_env: Adsr,
    /// Second modulator ratio for metallic tines (0 = off).
    pub ratio2: f32,
    pub index2: f32,
    pub feedback: f32,
    pub gain: f32,
}

impl Default for FmParams {
    fn default() -> Self {
        FmParams {
            ratio: 2.0,
            index: 2.0,
            mod_env: Adsr::new(0.001, 0.6, 0.2, 0.4),
            amp_env: Adsr::new(0.002, 1.0, 0.3, 0.5),
            ratio2: 0.0,
            index2: 0.0,
            feedback: 0.0,
            gain: 0.6,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PluckParams {
    /// 0 = long ring, 1 = very short.
    pub damping: f32,
    /// Excitation brightness 0..1.
    pub brightness: f32,
    pub gain: f32,
}

impl Default for PluckParams {
    fn default() -> Self {
        PluckParams {
            damping: 0.4,
            brightness: 0.7,
            gain: 0.7,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Bass808Params {
    /// Decay time in seconds.
    pub decay: f32,
    /// Initial pitch punch in semitones.
    pub punch: f32,
    /// Saturation 0..1.
    pub drive: f32,
    /// If true, the note is held for its full length instead of free decay.
    pub sustain: bool,
    pub gain: f32,
}

impl Default for Bass808Params {
    fn default() -> Self {
        Bass808Params {
            decay: 1.2,
            punch: 12.0,
            drive: 0.35,
            sustain: false,
            gain: 0.85,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SamplerParams {
    /// Name of a sample registered in the project.
    pub sample: String,
    /// MIDI note at which the sample plays at original pitch.
    pub root: u8,
    /// Play the whole sample regardless of note length.
    pub one_shot: bool,
    pub reverse: bool,
    /// Start offset 0..1 of the sample length.
    pub start: f32,
    /// Fixed playback length cap in seconds (0 = no cap).
    pub max_length: f32,
    pub attack: f32,
    pub release: f32,
    pub gain: f32,
}

impl Default for SamplerParams {
    fn default() -> Self {
        SamplerParams {
            sample: String::new(),
            root: 60,
            one_shot: true,
            reverse: false,
            start: 0.0,
            max_length: 0.0,
            attack: 0.001,
            release: 0.05,
            gain: 0.9,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Instrument {
    Drum(DrumParams),
    Synth(SynthParams),
    Fm(FmParams),
    Pluck(PluckParams),
    Bass808(Bass808Params),
    Sampler(SamplerParams),
}

impl Instrument {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Instrument::Drum(_) => "drum",
            Instrument::Synth(_) => "synth",
            Instrument::Fm(_) => "fm",
            Instrument::Pluck(_) => "pluck",
            Instrument::Bass808(_) => "bass808",
            Instrument::Sampler(_) => "sampler",
        }
    }
    pub fn is_drum(&self) -> bool {
        matches!(self, Instrument::Drum(_))
    }
}

fn drum(kind: DrumKind) -> Instrument {
    Instrument::Drum(DrumParams {
        kind,
        tune: 0.0,
        decay: 1.0,
        drive: 0.0,
    })
}

fn synth(f: impl FnOnce(&mut SynthParams)) -> Instrument {
    let mut p = SynthParams::default();
    f(&mut p);
    Instrument::Synth(p)
}

fn fm(f: impl FnOnce(&mut FmParams)) -> Instrument {
    let mut p = FmParams::default();
    f(&mut p);
    Instrument::Fm(p)
}

/// Named instrument presets with a one-line description.
pub const PRESETS: &[(&str, &str)] = &[
    ("kick", "Punchy synthesized kick drum"),
    ("snare", "Tight snare with noise body"),
    ("clap", "Layered hand clap"),
    ("hat", "Closed hi-hat"),
    ("open_hat", "Open hi-hat"),
    ("rim", "Rim shot / side stick"),
    ("tom", "Tunable tom"),
    ("cowbell", "808 cowbell (great for phonk)"),
    ("shaker", "Shaker for afro / latin grooves"),
    ("crash", "Crash cymbal"),
    ("808", "Long sub 808 with punch and drive"),
    ("sub_bass", "Clean sine sub bass"),
    ("acid_bass", "Resonant 303-style acid bass"),
    ("reese_bass", "Detuned moving reese bass (dnb)"),
    ("pluck_bass", "Short filtered house bass"),
    ("supersaw", "Huge 7-voice supersaw lead"),
    ("pluck_lead", "Snappy filtered pluck synth"),
    ("chip_lead", "8-bit square lead"),
    ("warm_pad", "Slow warm detuned pad"),
    ("dark_pad", "Dark evolving pad with LFO filter"),
    ("strings", "Ensemble strings with vibrato"),
    ("brass_stab", "Filtered brass stab"),
    ("dark_keys", "Moody triangle keys"),
    ("epiano", "FM electric piano (Rhodes-ish)"),
    ("fm_bell", "Glassy FM bell"),
    ("marimba", "Woody FM mallet"),
    ("guitar_pluck", "Karplus-Strong plucked string"),
    ("koto", "Bright short plucked string"),
];

pub fn preset(name: &str) -> Option<Instrument> {
    let n = name.trim().to_lowercase().replace([' ', '-'], "_");
    Some(match n.as_str() {
        "kick" => drum(DrumKind::Kick),
        "snare" => drum(DrumKind::Snare),
        "clap" => drum(DrumKind::Clap),
        "hat" | "hihat" | "closed_hat" => drum(DrumKind::ClosedHat),
        "open_hat" | "openhat" => drum(DrumKind::OpenHat),
        "rim" | "rimshot" => drum(DrumKind::Rim),
        "tom" => drum(DrumKind::Tom),
        "cowbell" => drum(DrumKind::Cowbell),
        "shaker" | "perc" => drum(DrumKind::Shaker),
        "crash" | "cymbal" => drum(DrumKind::Crash),
        "808" | "bass808" => Instrument::Bass808(Bass808Params::default()),
        "sub_bass" | "sub" => synth(|p| {
            p.osc1 = Wave::Sine;
            p.osc_mix = 0.0;
            p.cutoff = 2000.0;
            p.amp_env = Adsr::new(0.004, 0.1, 1.0, 0.08);
            p.gain = 0.8;
        }),
        "acid_bass" | "acid" => synth(|p| {
            p.osc1 = Wave::Saw;
            p.osc_mix = 0.0;
            p.cutoff = 350.0;
            p.resonance = 0.85;
            p.filter_env_amount = 3.2;
            p.filter_env = Adsr::new(0.002, 0.18, 0.1, 0.1);
            p.amp_env = Adsr::new(0.002, 0.2, 0.75, 0.05);
            p.drive = 0.5;
            p.gain = 0.5;
        }),
        "reese_bass" | "reese" => synth(|p| {
            p.osc1 = Wave::Saw;
            p.osc2 = Wave::Saw;
            p.osc2_cents = 22.0;
            p.unison = 2;
            p.unison_spread_cents = 12.0;
            p.cutoff = 900.0;
            p.resonance = 0.2;
            p.amp_env = Adsr::new(0.01, 0.1, 1.0, 0.1);
            p.lfo_rate = 0.25;
            p.lfo_to_cutoff = 0.6;
            p.sub_level = 0.5;
            p.drive = 0.3;
            p.gain = 0.45;
        }),
        "pluck_bass" => synth(|p| {
            p.osc1 = Wave::Square;
            p.osc2 = Wave::Saw;
            p.osc_mix = 0.3;
            p.cutoff = 300.0;
            p.filter_env_amount = 2.5;
            p.filter_env = Adsr::new(0.001, 0.12, 0.0, 0.1);
            p.amp_env = Adsr::new(0.002, 0.25, 0.3, 0.06);
            p.sub_level = 0.6;
            p.gain = 0.55;
        }),
        "supersaw" => synth(|p| {
            p.unison = 7;
            p.unison_spread_cents = 28.0;
            p.osc_mix = 0.3;
            p.osc2_semitones = 12.0;
            p.cutoff = 6500.0;
            p.amp_env = Adsr::new(0.01, 0.3, 0.8, 0.35);
            p.gain = 0.35;
        }),
        "pluck_lead" | "pluck_synth" => synth(|p| {
            p.unison = 3;
            p.unison_spread_cents = 12.0;
            p.cutoff = 700.0;
            p.resonance = 0.25;
            p.filter_env_amount = 4.0;
            p.filter_env = Adsr::new(0.001, 0.22, 0.0, 0.2);
            p.amp_env = Adsr::new(0.002, 0.35, 0.0, 0.25);
            p.gain = 0.5;
        }),
        "chip_lead" | "chiptune" => synth(|p| {
            p.osc1 = Wave::Square;
            p.osc_mix = 0.0;
            p.cutoff = 12000.0;
            p.amp_env = Adsr::new(0.001, 0.1, 0.7, 0.05);
            p.gain = 0.35;
        }),
        "warm_pad" | "pad" => synth(|p| {
            p.osc2 = Wave::Triangle;
            p.unison = 5;
            p.unison_spread_cents = 16.0;
            p.cutoff = 1700.0;
            p.amp_env = Adsr::new(0.6, 0.5, 0.85, 1.4);
            p.lfo_rate = 0.3;
            p.lfo_to_cutoff = 0.3;
            p.gain = 0.35;
        }),
        "dark_pad" => synth(|p| {
            p.osc1 = Wave::Saw;
            p.osc2 = Wave::Square;
            p.osc2_semitones = -12.0;
            p.unison = 5;
            p.unison_spread_cents = 20.0;
            p.cutoff = 700.0;
            p.resonance = 0.3;
            p.amp_env = Adsr::new(1.0, 1.0, 0.8, 2.0);
            p.lfo_rate = 0.12;
            p.lfo_to_cutoff = 1.0;
            p.gain = 0.35;
        }),
        "strings" => synth(|p| {
            p.unison = 5;
            p.unison_spread_cents = 10.0;
            p.cutoff = 3200.0;
            p.amp_env = Adsr::new(0.25, 0.3, 0.9, 0.8);
            p.lfo_rate = 5.0;
            p.lfo_to_pitch = 0.08;
            p.gain = 0.35;
        }),
        "brass_stab" | "brass" => synth(|p| {
            p.unison = 3;
            p.unison_spread_cents = 8.0;
            p.cutoff = 800.0;
            p.filter_env_amount = 2.5;
            p.filter_env = Adsr::new(0.03, 0.25, 0.3, 0.2);
            p.amp_env = Adsr::new(0.02, 0.2, 0.7, 0.15);
            p.gain = 0.45;
        }),
        "dark_keys" | "keys" => synth(|p| {
            p.osc1 = Wave::Triangle;
            p.osc2 = Wave::Square;
            p.osc2_semitones = 12.0;
            p.osc_mix = 0.2;
            p.cutoff = 2000.0;
            p.amp_env = Adsr::new(0.005, 0.6, 0.4, 0.5);
            p.gain = 0.5;
        }),
        "epiano" | "rhodes" => fm(|p| {
            p.ratio = 1.0;
            p.index = 1.6;
            p.mod_env = Adsr::new(0.001, 0.9, 0.15, 0.4);
            p.amp_env = Adsr::new(0.002, 1.6, 0.35, 0.5);
            p.ratio2 = 14.0;
            p.index2 = 0.35;
            p.gain = 0.5;
        }),
        "fm_bell" | "bell" => fm(|p| {
            p.ratio = 3.5;
            p.index = 4.0;
            p.mod_env = Adsr::new(0.001, 1.4, 0.0, 1.0);
            p.amp_env = Adsr::new(0.001, 2.2, 0.0, 1.6);
            p.gain = 0.4;
        }),
        "marimba" | "mallet" => fm(|p| {
            p.ratio = 4.0;
            p.index = 2.5;
            p.mod_env = Adsr::new(0.001, 0.08, 0.0, 0.1);
            p.amp_env = Adsr::new(0.001, 0.5, 0.0, 0.3);
            p.gain = 0.6;
        }),
        "guitar_pluck" | "pluck" | "guitar" => Instrument::Pluck(PluckParams::default()),
        "koto" => Instrument::Pluck(PluckParams {
            damping: 0.7,
            brightness: 0.95,
            gain: 0.7,
        }),
        _ => return None,
    })
}

/// Render one note to a mono buffer.
pub fn render_note(
    inst: &Instrument,
    pitch: f32,
    vel: f32,
    gate: f32,
    bank: &SampleBank,
    seed: u64,
) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let vel = vel.clamp(0.0, 1.0);
    match inst {
        Instrument::Drum(p) => render_drum(p, pitch, vel, &mut rng),
        Instrument::Synth(p) => render_synth(p, pitch, vel, gate, &mut rng),
        Instrument::Fm(p) => render_fm(p, pitch, vel, gate),
        Instrument::Pluck(p) => render_pluck(p, pitch, vel, gate, &mut rng),
        Instrument::Bass808(p) => render_808(p, pitch, vel, gate),
        Instrument::Sampler(p) => render_sampler(p, pitch, vel, gate, bank),
    }
}

fn secs(n: f32) -> usize {
    (n.max(0.0) * SR) as usize
}

fn render_drum(p: &DrumParams, pitch: f32, vel: f32, rng: &mut Rng) -> Vec<f32> {
    let tune = 2f32.powf((p.tune + (pitch - 60.0)) / 12.0);
    let d = p.decay.clamp(0.05, 8.0);
    let mut out;
    match p.kind {
        DrumKind::Kick => {
            let len = 0.7 * d;
            out = vec![0.0; secs(len)];
            let mut ph = 0.0f32;
            for (i, s) in out.iter_mut().enumerate() {
                let t = i as f32 / SR;
                let f = 48.0 * tune * (1.0 + 2.6 * (-t * 32.0).exp());
                ph = (ph + f / SR) % 1.0;
                let body = (2.0 * PI * ph).sin() * (-t * 5.5 / d).exp();
                let click = rng.bipolar() * (-t * 350.0).exp() * 0.25;
                *s = (1.6 * (body + click)).tanh();
            }
        }
        DrumKind::Snare => {
            out = vec![0.0; secs(0.38 * d)];
            let mut hp = Svf::default();
            let mut ph = 0.0f32;
            for (i, s) in out.iter_mut().enumerate() {
                let t = i as f32 / SR;
                let f = 190.0 * tune * (1.0 + 0.4 * (-t * 60.0).exp());
                ph = (ph + f / SR) % 1.0;
                let tone = (2.0 * PI * ph).sin() * (-t * 28.0).exp() * 0.55;
                let n = hp.process(rng.bipolar(), 1800.0 * tune, 0.1, FilterMode::Highpass);
                *s = tone + n * (-t * 15.0 / d).exp() * 0.8;
            }
        }
        DrumKind::Clap => {
            out = vec![0.0; secs(0.45 * d)];
            let mut bp = Svf::default();
            for (i, s) in out.iter_mut().enumerate() {
                let t = i as f32 / SR;
                let n = bp.process(rng.bipolar(), 1150.0 * tune, 0.35, FilterMode::Bandpass);
                let mut env = 0.0f32;
                for k in 0..3 {
                    let dt = t - k as f32 * 0.011;
                    if dt >= 0.0 {
                        env = env.max((-dt * 190.0).exp());
                    }
                }
                let dt = t - 0.033;
                if dt >= 0.0 {
                    env = env.max((-dt * 13.0 / d).exp() * 0.75);
                }
                *s = n * env * 2.2;
            }
        }
        DrumKind::ClosedHat | DrumKind::OpenHat => {
            let open = p.kind == DrumKind::OpenHat;
            let len = if open { 0.65 * d } else { 0.09 * d + 0.03 };
            out = vec![0.0; secs(len)];
            let mut hp = Svf::default();
            // metallic: sum of detuned squares + noise, like the 808
            let ratios = [2.0, 3.0, 4.16, 5.43, 6.79, 8.21];
            let mut phases = [0.0f32; 6];
            for (i, s) in out.iter_mut().enumerate() {
                let t = i as f32 / SR;
                let mut metal = 0.0;
                for (k, r) in ratios.iter().enumerate() {
                    phases[k] = (phases[k] + 205.0 * r * tune / SR) % 1.0;
                    metal += if phases[k] < 0.5 { 1.0 } else { -1.0 };
                }
                let x = metal / 6.0 * 0.6 + rng.bipolar() * 0.5;
                let y = hp.process(x, 7500.0 * tune.sqrt(), 0.2, FilterMode::Highpass);
                let env = if open {
                    (-t * 6.5 / d).exp()
                } else {
                    (-t * 60.0 / d).exp()
                };
                *s = y * env * 1.1;
            }
        }
        DrumKind::Rim => {
            out = vec![0.0; secs(0.07 * d + 0.02)];
            let mut bp = Svf::default();
            for (i, s) in out.iter_mut().enumerate() {
                let t = i as f32 / SR;
                let tone = (2.0 * PI * 1700.0 * tune * t).sin() * 0.6
                    + (2.0 * PI * 480.0 * tune * t).sin() * 0.4;
                let n = bp.process(rng.bipolar(), 2500.0, 0.4, FilterMode::Bandpass);
                *s = (tone + n * 0.5) * (-t * 90.0 / d).exp();
            }
        }
        DrumKind::Tom => {
            out = vec![0.0; secs(0.5 * d)];
            let mut ph = 0.0f32;
            for (i, s) in out.iter_mut().enumerate() {
                let t = i as f32 / SR;
                let f = 115.0 * tune * (1.0 + 0.5 * (-t * 18.0).exp());
                ph = (ph + f / SR) % 1.0;
                *s = (2.0 * PI * ph).sin() * (-t * 8.0 / d).exp();
            }
        }
        DrumKind::Cowbell => {
            out = vec![0.0; secs(0.45 * d)];
            let mut bp = Svf::default();
            let (mut p1, mut p2) = (0.0f32, 0.0f32);
            for (i, s) in out.iter_mut().enumerate() {
                let t = i as f32 / SR;
                p1 = (p1 + 540.0 * tune / SR) % 1.0;
                p2 = (p2 + 800.0 * tune / SR) % 1.0;
                let x = (if p1 < 0.5 { 1.0 } else { -1.0 }) + (if p2 < 0.5 { 1.0 } else { -1.0 });
                let y = bp.process(x * 0.5, 800.0 * tune, 0.45, FilterMode::Bandpass);
                let env = 0.6 * (-t * 60.0).exp() + 0.4 * (-t * 9.0 / d).exp();
                *s = y * env * 1.6;
            }
        }
        DrumKind::Shaker => {
            out = vec![0.0; secs(0.12 * d + 0.03)];
            let mut bp = Svf::default();
            for (i, s) in out.iter_mut().enumerate() {
                let t = i as f32 / SR;
                let n = bp.process(rng.bipolar(), 6500.0 * tune, 0.3, FilterMode::Bandpass);
                let env = (t / 0.012).min(1.0) * (-t * 32.0 / d).exp();
                *s = n * env * 1.3;
            }
        }
        DrumKind::Crash => {
            out = vec![0.0; secs(2.2 * d)];
            let mut hp = Svf::default();
            for (i, s) in out.iter_mut().enumerate() {
                let t = i as f32 / SR;
                let n = hp.process(
                    rng.bipolar(),
                    4500.0 * tune.sqrt(),
                    0.1,
                    FilterMode::Highpass,
                );
                *s = n * (-t * 2.0 / d).exp() * 0.7;
            }
        }
    }
    let drive = 1.0 + p.drive.clamp(0.0, 1.0) * 6.0;
    for s in out.iter_mut() {
        *s = if drive > 1.01 {
            (*s * drive).tanh() / drive.tanh()
        } else {
            *s
        } * vel;
    }
    out
}

fn render_synth(p: &SynthParams, pitch: f32, vel: f32, gate: f32, rng: &mut Rng) -> Vec<f32> {
    let total = p.amp_env.total(gate).min(12.0);
    let n = secs(total);
    let mut out = vec![0.0f32; n];
    let voices = p.unison.clamp(1, 9) as usize;
    let base = midi_to_hz(pitch);
    let mut phases1: Vec<f32> = (0..voices).map(|_| rng.f32()).collect();
    let mut phases2: Vec<f32> = (0..voices).map(|_| rng.f32()).collect();
    let detunes: Vec<f32> = (0..voices)
        .map(|v| {
            if voices == 1 {
                0.0
            } else {
                (v as f32 / (voices - 1) as f32 - 0.5) * 2.0 * p.unison_spread_cents
            }
        })
        .collect();
    let norm = 1.0 / (voices as f32).sqrt();
    let mut sub_ph = 0.0f32;
    let mut filt = Svf::default();
    let mut filt2 = Svf::default();
    let lfo_ph0 = rng.f32();
    let drive = 1.0 + p.drive.max(0.0) * 8.0;
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        let lfo = (2.0 * PI * (p.lfo_rate * t + lfo_ph0)).sin();
        let mut semis = lfo * p.lfo_to_pitch;
        if p.pitch_env_semitones != 0.0 {
            semis += p.pitch_env_semitones * (-t / p.pitch_env_time.max(0.001)).exp();
        }
        let f = base * 2f32.powf(semis / 12.0);
        let mut x = 0.0;
        for v in 0..voices {
            let f1 = f * 2f32.powf(detunes[v] / 1200.0);
            let f2 = f1 * 2f32.powf((p.osc2_semitones * 100.0 + p.osc2_cents) / 1200.0);
            let dt1 = (f1 / SR).min(0.49);
            let dt2 = (f2 / SR).min(0.49);
            let a = osc(p.osc1, phases1[v], dt1, rng);
            let b = osc(p.osc2, phases2[v], dt2, rng);
            phases1[v] = (phases1[v] + dt1) % 1.0;
            phases2[v] = (phases2[v] + dt2) % 1.0;
            x += a * (1.0 - p.osc_mix) + b * p.osc_mix;
        }
        x *= norm;
        if p.sub_level > 0.0 {
            sub_ph = (sub_ph + f * 0.5 / SR) % 1.0;
            x += (2.0 * PI * sub_ph).sin() * p.sub_level;
        }
        if p.noise_level > 0.0 {
            x += rng.bipolar() * p.noise_level;
        }
        let fenv = p.filter_env.level(t, gate);
        let cutoff = p.cutoff * 2f32.powf(p.filter_env_amount * fenv + lfo * p.lfo_to_cutoff);
        let mut y = filt.process(x, cutoff, p.resonance, p.filter_mode);
        if p.resonance > 0.6 && p.filter_mode == FilterMode::Lowpass {
            // 4-pole-ish for acid squelch
            y = filt2.process(y, cutoff * 1.2, 0.0, FilterMode::Lowpass);
        }
        if drive > 1.01 {
            y = (y * drive).tanh() / drive.tanh().max(0.5);
        }
        *s = y * p.amp_env.level(t, gate) * vel * p.gain;
    }
    out
}

fn render_fm(p: &FmParams, pitch: f32, vel: f32, gate: f32) -> Vec<f32> {
    let total = p.amp_env.total(gate).min(12.0);
    let mut out = vec![0.0f32; secs(total)];
    let f = midi_to_hz(pitch);
    let (mut pc, mut pm, mut pm2) = (0.0f32, 0.0f32, 0.0f32);
    let mut last = 0.0f32;
    // velocity also brightens FM
    let vel_index = 0.5 + vel * 0.7;
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        let menv = p.mod_env.level(t, gate);
        pm = (pm + f * p.ratio / SR) % 1.0;
        let mut m = (2.0 * PI * pm + last * p.feedback).sin() * p.index * menv * vel_index;
        if p.ratio2 > 0.0 {
            pm2 = (pm2 + f * p.ratio2 / SR) % 1.0;
            m += (2.0 * PI * pm2).sin() * p.index2 * (-t * 12.0).exp();
        }
        pc = (pc + f / SR) % 1.0;
        let y = (2.0 * PI * pc + m).sin();
        last = y;
        *s = y * p.amp_env.level(t, gate) * vel * p.gain;
    }
    out
}

fn render_pluck(p: &PluckParams, pitch: f32, vel: f32, gate: f32, rng: &mut Rng) -> Vec<f32> {
    let f = midi_to_hz(pitch).max(20.0);
    let period = (SR / f).max(2.0);
    let len = period as usize;
    let total = (gate + 1.5 * (1.0 - p.damping) + 0.2).clamp(0.2, 6.0);
    let mut out = vec![0.0f32; secs(total)];
    let mut buf: Vec<f32> = (0..len).map(|_| rng.bipolar()).collect();
    // brightness: pre-filter the excitation
    let smooth = 1.0 - p.brightness.clamp(0.0, 1.0);
    for _ in 0..(smooth * 4.0) as usize {
        let first = buf[0];
        for i in 0..len {
            let next = if i + 1 < len { buf[i + 1] } else { first };
            buf[i] = 0.5 * (buf[i] + next);
        }
    }
    let decay = 0.999 - p.damping.clamp(0.0, 1.0) * 0.02;
    let mut idx = 0usize;
    let mut dc = DcBlock::default();
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        let cur = buf[idx];
        let nxt = buf[(idx + 1) % len];
        buf[idx] = decay * 0.5 * (cur + nxt);
        idx = (idx + 1) % len;
        let rel = if t > gate + 0.05 {
            (-(t - gate - 0.05) * 6.0).exp()
        } else {
            1.0
        };
        *s = dc.process(cur) * rel * vel * p.gain;
    }
    out
}

fn render_808(p: &Bass808Params, pitch: f32, vel: f32, gate: f32) -> Vec<f32> {
    let f = midi_to_hz(pitch);
    let total = if p.sustain {
        gate + 0.15
    } else {
        p.decay.max(0.1) + 0.1
    }
    .min(8.0);
    let mut out = vec![0.0f32; secs(total)];
    let drive = 1.0 + p.drive.clamp(0.0, 1.0) * 5.0;
    let mut ph = 0.0f32;
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        let fr = f * 2f32.powf(p.punch * (-t * 40.0).exp() / 12.0);
        ph = (ph + fr / SR) % 1.0;
        let env = if p.sustain {
            let r = if t > gate {
                (-(t - gate) * 30.0).exp()
            } else {
                1.0
            };
            (0.75 + 0.25 * (-t * 8.0).exp()) * r
        } else {
            (-t * 4.5 / p.decay.max(0.1)).exp()
        };
        let attack = (t / 0.002).min(1.0);
        let y = (2.0 * PI * ph).sin();
        *s = (y * drive).tanh() / drive.tanh() * env * attack * vel * p.gain;
    }
    out
}

fn render_sampler(
    p: &SamplerParams,
    pitch: f32,
    vel: f32,
    gate: f32,
    bank: &SampleBank,
) -> Vec<f32> {
    let Some(sample) = bank.get(&p.sample) else {
        return Vec::new();
    };
    let data: Vec<f32> = if p.reverse {
        sample.iter().rev().copied().collect()
    } else {
        sample.to_vec()
    };
    if data.is_empty() {
        return Vec::new();
    }
    let rate = 2f32.powf((pitch - p.root as f32) / 12.0);
    let start = (p.start.clamp(0.0, 0.99) * data.len() as f32) as usize;
    let avail = (data.len() - start) as f32 / rate / SR;
    let mut len = if p.one_shot {
        avail
    } else {
        (gate + p.release).min(avail)
    };
    if p.max_length > 0.0 {
        len = len.min(p.max_length);
    }
    let n = secs(len);
    let mut out = vec![0.0f32; n];
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        let pos = start as f32 + i as f32 * rate;
        let a = (t / p.attack.max(0.0005)).min(1.0);
        let end_t = len - t;
        let r = (end_t / p.release.max(0.001)).min(1.0);
        let rel = if p.one_shot && p.max_length <= 0.0 {
            1.0f32.min(end_t / 0.003)
        } else {
            r
        };
        *s = lerp_read(&data, pos) * a * rel.max(0.0) * vel * p.gain;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak(v: &[f32]) -> f32 {
        v.iter().fold(0.0, |m, x| m.max(x.abs()))
    }

    #[test]
    fn every_preset_renders_audible_finite_audio() {
        let bank = SampleBank::default();
        for (name, _) in PRESETS {
            let inst = preset(name).unwrap_or_else(|| panic!("preset {name} missing"));
            let buf = render_note(&inst, 48.0, 0.9, 0.4, &bank, 1);
            assert!(!buf.is_empty(), "{name} empty");
            assert!(buf.iter().all(|x| x.is_finite()), "{name} not finite");
            let p = peak(&buf);
            assert!(p > 0.01, "{name} silent ({p})");
            assert!(p < 4.0, "{name} too hot ({p})");
        }
    }

    #[test]
    fn instruments_roundtrip_json() {
        for (name, _) in PRESETS {
            let inst = preset(name).unwrap();
            let s = serde_json::to_string(&inst).unwrap();
            let back: Instrument = serde_json::from_str(&s).unwrap();
            assert_eq!(inst, back);
        }
    }

    #[test]
    fn partial_synth_json_uses_defaults() {
        let inst: Instrument = serde_json::from_str(r#"{"type":"synth","cutoff":500}"#).unwrap();
        match inst {
            Instrument::Synth(p) => {
                assert_eq!(p.cutoff, 500.0);
                assert_eq!(p.unison, 1);
            }
            _ => panic!(),
        }
    }
}
