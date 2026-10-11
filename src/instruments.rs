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
    /// Tabla dayan (treble drum): ringing harmonic membrane, tuned to the
    /// note played (tune it to Sa). Velocity < 0.5 gives a muted "te/ti".
    Tabla,
    /// Tabla bayan (bass drum): "ge"/"dha" bass with the palm-pressure
    /// pitch rise (gamak).
    Bayan,
}

impl DrumKind {
    pub const ALL: [DrumKind; 12] = [
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
        DrumKind::Tabla,
        DrumKind::Bayan,
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
    /// Tone -1 (soft, dark, round) .. 1 (hard, bright, clicky).
    #[serde(default, skip_serializing_if = "is_zero_f")]
    pub tone: f32,
}

impl Default for DrumParams {
    fn default() -> Self {
        DrumParams {
            kind: DrumKind::Kick,
            tune: 0.0,
            decay: 1.0,
            drive: 0.0,
            tone: 0.0,
        }
    }
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
    /// Glide time for slides (notes with slide_to), ms (0 = 80 ms).
    pub glide_ms: f32,
    /// Stereo spread of the unison voices 0..1 (0 = mono).
    pub stereo_spread: f32,
    /// Analog drift: slow random pitch wander per voice, in cents.
    #[serde(skip_serializing_if = "is_zero_f")]
    pub drift_cents: f32,
    /// Velocity to filter cutoff, octaves at full velocity swing.
    #[serde(skip_serializing_if = "is_zero_f")]
    pub vel_to_cutoff: f32,
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
            glide_ms: 0.0,
            stereo_spread: 0.0,
            drift_cents: 0.0,
            vel_to_cutoff: 0.0,
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
    /// Second carrier detuned by this many cents (chorus / tine beating).
    #[serde(skip_serializing_if = "is_zero_f")]
    pub detune_cents: f32,
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
            detune_cents: 0.0,
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
    /// Jawari bridge buzz 0..1 (sitar/tanpura sizzle): a curved-bridge
    /// nonlinearity that keeps re-exciting the upper partials.
    #[serde(default, skip_serializing_if = "is_zero_f")]
    pub buzz: f32,
    /// String model: guitar, sitar or santoor ("" = inferred from the other knobs).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
}

fn is_zero_f(x: &f32) -> bool {
    *x == 0.0
}

impl Default for PluckParams {
    fn default() -> Self {
        PluckParams {
            damping: 0.4,
            brightness: 0.7,
            gain: 0.7,
            buzz: 0.0,
            kind: String::new(),
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
    /// Glide time for slides (notes with slide_to), ms (0 = 90 ms).
    pub glide_ms: f32,
    /// Attack click layer 0..1 (helps the 808 cut through on small speakers).
    pub click: f32,
    /// Clean sine sub under the driven layer 0..1 (keeps the low end solid
    /// however hard it is driven).
    pub sub: f32,
    /// Tone of the driven layer in octaves (-2 dark .. +2 bright).
    pub tone: f32,
}

impl Default for Bass808Params {
    fn default() -> Self {
        Bass808Params {
            decay: 1.2,
            punch: 12.0,
            drive: 0.45,
            sustain: false,
            gain: 0.85,
            glide_ms: 0.0,
            click: 0.3,
            sub: 0.4,
            tone: 0.0,
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
    /// Slice start times in seconds (from `slice_sample`). When set, note
    /// `root + i` plays slice i at its original pitch (a slice kit).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub slices: Vec<f32>,
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
            slices: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WtTable {
    /// sine -> triangle -> saw -> square
    #[default]
    Basic,
    /// harmonics fade in one by one (dark -> bright)
    Harmonic,
    /// pulse width sweep 50% -> 5%
    Pwm,
    /// vowel formants a -> e -> i -> o -> u
    Vocal,
    /// gappy bit-pattern spectra (digital, gritty)
    Digital,
    /// drawbar organ registrations
    Organ,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WavetableParams {
    pub table: WtTable,
    /// Morph position 0..1 through the table (automatable).
    pub position: f32,
    /// How far the position envelope moves the position (-1..1).
    pub position_env_amount: f32,
    pub position_env: Adsr,
    pub position_lfo_rate: f32,
    pub position_lfo_depth: f32,
    pub unison: u8,
    pub unison_spread_cents: f32,
    pub sub_level: f32,
    pub filter_mode: FilterMode,
    pub cutoff: f32,
    pub resonance: f32,
    pub filter_env_amount: f32,
    pub filter_env: Adsr,
    pub amp_env: Adsr,
    pub drive: f32,
    pub gain: f32,
    /// Stereo spread of the unison voices 0..1.
    pub stereo_spread: f32,
}

impl Default for WavetableParams {
    fn default() -> Self {
        WavetableParams {
            table: WtTable::Basic,
            position: 0.5,
            position_env_amount: 0.0,
            position_env: Adsr::new(0.01, 0.6, 0.0, 0.3),
            position_lfo_rate: 0.0,
            position_lfo_depth: 0.0,
            unison: 1,
            unison_spread_cents: 12.0,
            sub_level: 0.0,
            filter_mode: FilterMode::Lowpass,
            cutoff: 8000.0,
            resonance: 0.1,
            filter_env_amount: 0.0,
            filter_env: Adsr::new(0.005, 0.3, 0.0, 0.2),
            amp_env: Adsr::new(0.005, 0.3, 0.7, 0.25),
            drive: 0.0,
            gain: 0.55,
            stereo_spread: 0.0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GranularParams {
    /// Sample to granulate; empty = a built-in synthesized texture.
    pub sample: String,
    /// MIDI note at which grains play at original pitch.
    pub root: u8,
    /// Read position 0..1 in the source.
    pub position: f32,
    /// Random position scatter 0..1 (spray).
    pub spray: f32,
    /// Position drift per second of the note (scan), in source fractions.
    pub scan: f32,
    pub grain_ms: f32,
    /// Grains per second.
    pub density: f32,
    pub pitch_spread_cents: f32,
    /// Chance a grain plays backwards 0..1.
    pub reverse_prob: f32,
    pub cutoff: f32,
    pub amp_env: Adsr,
    pub gain: f32,
}

impl Default for GranularParams {
    fn default() -> Self {
        GranularParams {
            sample: String::new(),
            root: 60,
            position: 0.3,
            spray: 0.15,
            scan: 0.05,
            grain_ms: 90.0,
            density: 40.0,
            pitch_spread_cents: 8.0,
            reverse_prob: 0.1,
            cutoff: 12000.0,
            amp_env: Adsr::new(0.3, 0.5, 0.8, 0.8),
            gain: 0.6,
        }
    }
}

/// One source in a layered instrument (with optional key / velocity split).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Layer {
    pub instrument: Instrument,
    #[serde(default)]
    pub gain_db: f32,
    /// Semitones.
    #[serde(default)]
    pub transpose: f32,
    #[serde(default)]
    pub key_min: u8,
    #[serde(default = "key_max_default")]
    pub key_max: u8,
    #[serde(default)]
    pub vel_min: f32,
    #[serde(default = "one")]
    pub vel_max: f32,
    /// Start this layer late (ms), e.g. a clap a hair after the snare.
    #[serde(default)]
    pub delay_ms: f32,
}

fn key_max_default() -> u8 {
    127
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PianoParams {
    /// Spectral brightness 0..2 (velocity also brightens).
    pub brightness: f32,
    /// Felt damping 0..1: 0 = bright grand, 1 = soft felt piano.
    pub felt: f32,
    /// Hammer thump level 0..1.
    pub hammer: f32,
    /// Decay time scale (1 = natural).
    pub decay: f32,
    /// Damper release, seconds.
    pub release: f32,
    /// Detune between the strings of one key (cents): chorus / honky-tonk.
    pub detune_cents: f32,
    /// Soundboard body resonance 0..1.
    pub body: f32,
    pub sustain_pedal: bool,
    pub gain: f32,
}

impl Default for PianoParams {
    fn default() -> Self {
        PianoParams {
            brightness: 1.0,
            felt: 0.0,
            hammer: 0.5,
            decay: 1.0,
            release: 0.18,
            detune_cents: 1.2,
            body: 0.5,
            sustain_pedal: false,
            gain: 0.7,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EnsembleKind {
    #[default]
    Strings,
    Choir,
    Brass,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EnsembleParams {
    pub kind: EnsembleKind,
    /// Number of players in the section (1-12).
    pub players: u8,
    /// Vibrato depth 0..1.
    pub vibrato: f32,
    /// Per-player tuning spread (cents).
    pub detune_cents: f32,
    /// Random onset spread between players (ms).
    pub attack_jitter_ms: f32,
    pub brightness: f32,
    /// Choir vowel: 0 = "oo", 1 = "ah".
    pub vowel: f32,
    /// Bow / breath noise 0..1.
    pub noise: f32,
    pub amp_env: Adsr,
    pub stereo_spread: f32,
    pub gain: f32,
}

impl Default for EnsembleParams {
    fn default() -> Self {
        EnsembleParams {
            kind: EnsembleKind::Strings,
            players: 6,
            vibrato: 0.5,
            detune_cents: 8.0,
            attack_jitter_ms: 25.0,
            brightness: 0.8,
            vowel: 0.3,
            noise: 0.3,
            amp_env: Adsr::new(0.25, 0.4, 0.85, 0.6),
            stereo_spread: 0.7,
            gain: 0.5,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct LayerParams {
    pub layers: Vec<Layer>,
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
    Wavetable(WavetableParams),
    Granular(GranularParams),
    Layer(LayerParams),
    Multisample(crate::multisample::MultisampleParams),
    Piano(PianoParams),
    Ensemble(EnsembleParams),
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
            Instrument::Wavetable(_) => "wavetable",
            Instrument::Granular(_) => "granular",
            Instrument::Layer(_) => "layer",
            Instrument::Multisample(_) => "multisample",
            Instrument::Piano(_) => "piano",
            Instrument::Ensemble(_) => "ensemble",
        }
    }
    /// Stereo unison spread 0..1 (decorrelated left/right voices).
    pub fn stereo_spread(&self) -> f32 {
        match self {
            Instrument::Synth(p) if p.unison > 1 || p.osc1 == Wave::Noise => p.stereo_spread,
            Instrument::Wavetable(p) => p.stereo_spread,
            Instrument::Granular(_) => 0.6,
            Instrument::Ensemble(p) => p.stereo_spread,
            Instrument::Piano(_) => 0.35,
            Instrument::Multisample(_) => 0.0,
            Instrument::Layer(l) => l
                .layers
                .iter()
                .map(|x| x.instrument.stereo_spread())
                .fold(0.0, f32::max),
            _ => 0.0,
        }
    }

    pub fn is_drum(&self) -> bool {
        match self {
            Instrument::Drum(_) => true,
            Instrument::Layer(l) => l.layers.first().is_some_and(|x| x.instrument.is_drum()),
            _ => false,
        }
    }
}

fn drum(kind: DrumKind) -> Instrument {
    Instrument::Drum(DrumParams {
        kind,
        ..Default::default()
    })
}

/// A drum preset with tune (semitones), decay multiplier, drive and tone.
fn drum_x(kind: DrumKind, tune: f32, decay: f32, drive: f32, tone: f32) -> Instrument {
    Instrument::Drum(DrumParams {
        kind,
        tune,
        decay,
        drive,
        tone,
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
    (
        "tabla",
        "Tabla dayan: harmonic ringing treble drum, tuned to the note (soft hits = muted te/ti)",
    ),
    (
        "bayan",
        "Tabla bayan: bass 'ge' with the palm pitch rise (gamak)",
    ),
    ("sitar", "Karplus-Strong sitar with jawari bridge buzz"),
    ("santoor", "Bright hammered-string santoor"),
    (
        "bansuri",
        "Breathy bamboo flute with vibrato and meend glides (slide_to)",
    ),
    ("tanpura", "Tanpura-style drone pad (root + fifth shimmer)"),
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
    (
        "wt_lead",
        "Wavetable lead morphing saw -> square with a position envelope",
    ),
    (
        "wt_pad",
        "Slow evolving wavetable pad (harmonic table, LFO on position)",
    ),
    ("wt_vocal", "Vowel-morphing wavetable (talking lead)"),
    (
        "wt_growl",
        "Gritty digital wavetable bass with position sweep",
    ),
    ("organ", "Drawbar organ wavetable"),
    (
        "granular_pad",
        "Airy granular texture pad (built-in source, or set sample)",
    ),
    ("granular_shimmer", "Bright, sparse granular shimmer"),
    (
        "layered_kick",
        "Kick + 808 sub layer: punch and weight in one",
    ),
    (
        "layered_snare",
        "Snare + clap layer (clap 8 ms late) for a wide backbeat",
    ),
    (
        "layered_keys",
        "E-piano + soft pad layer: warm, sustained keys",
    ),
    (
        "grand_piano",
        "Modelled acoustic grand: inharmonic strings, hammer, body (built-in, no samples)",
    ),
    (
        "felt_piano",
        "Soft felt piano: muffled hammers, intimate, cinematic",
    ),
    (
        "upright_keys",
        "Darker, slightly detuned upright piano model",
    ),
    (
        "string_section",
        "Modelled string section: 8 players, body resonance, delayed vibrato",
    ),
    ("staccato_strings", "Short spiccato string section"),
    ("cello_section", "Low, warm modelled celli"),
    ("choir", "Modelled 'ooh' choir pad"),
    ("choir_aah", "Open 'aah' choir"),
    ("brass_section", "Modelled brass section swell"),
    // ---- sound palette voices (sprint 9) ----
    (
        "kick_punchy",
        "Tight punchy kick with a hard beater click (trap)",
    ),
    ("kick_tight", "Short, tight, high-tuned kick (drill)"),
    ("kick_deep", "Deep, long, soft-clicked kick (melodic / R&B)"),
    ("kick_dusty", "Round, dark, saturated boom-bap kick"),
    ("kick_grit", "Driven gritty kick (desi hip-hop)"),
    ("snare_crack", "Bright cracking snare (trap)"),
    ("snare_tight", "High, tight, short snare (drill / dhh)"),
    ("snare_dusty", "Fat, dark, saturated boom-bap snare"),
    ("snare_soft", "Soft, round, clean snare (melodic)"),
    ("clap_wide", "Wide layered clap with a room tail"),
    ("hat_crisp", "Crisp tight closed hat, made for rolls"),
    ("hat_drill", "Bright, short, high-tuned closed hat (drill)"),
    ("snare_drill", "Bright, high, cracking short snare (drill)"),
    (
        "dholak",
        "Dholak treble head: tighter, lower and drier than the tabla dayan (ta / na slaps)",
    ),
    (
        "dholak_bass",
        "Dholak bass head: a short 'ge' boom with a palm pitch rise",
    ),
    (
        "harmonium",
        "Harmonium (peti): two beating reed banks an octave apart, bellows attack",
    ),
    ("hat_dusty", "Dark, soft, slightly driven closed hat"),
    ("open_hat_airy", "Airy open hat (choked by the closed hats)"),
    ("open_hat_dusty", "Dark short open hat (boom bap)"),
    ("808_dark", "Dark round 808: big clean sub, little click"),
    (
        "808_grit",
        "Distorted gritty 808 that reads on phone speakers",
    ),
    (
        "808_slide",
        "Drill 808: long, held, tuned glides (use slide_to)",
    ),
    ("808_clean", "Clean short 808 for melodic rap / R&B"),
    (
        "bass_round",
        "Warm round finger-bass-like synth bass (boom bap)",
    ),
    (
        "keys_airy",
        "Soft airy chorused e-piano, velocity-sensitive",
    ),
    ("keys_dusty", "Warm dusty e-piano with tape-like drift"),
    (
        "pad_airy",
        "Wide airy pad: drifting voices, slow filter bloom",
    ),
    ("pad_dusty", "Dark lo-fi pad with tape-like wow"),
    ("bell_glass", "Glassy detuned FM bell (melodic trap)"),
    ("bell_dark", "Soft dark music-box bell (drill)"),
    ("pluck_soft", "Soft velocity-sensitive synth pluck"),
    (
        "lead_dark",
        "Dark detuned lead with gentle vibrato (drill / trap)",
    ),
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
        "tabla" | "tabla_dayan" | "dayan" => drum(DrumKind::Tabla),
        "bayan" | "tabla_bayan" | "dagga" => drum(DrumKind::Bayan),
        "sitar" => Instrument::Pluck(PluckParams {
            damping: 0.25,
            brightness: 0.6,
            gain: 0.6,
            buzz: 0.4,
            kind: "sitar".into(),
        }),
        "santoor" => Instrument::Pluck(PluckParams {
            damping: 0.45,
            brightness: 0.95,
            gain: 0.55,
            buzz: 0.0,
            kind: "santoor".into(),
        }),
        "bansuri" | "flute" | "indian_flute" => synth(|p| {
            p.osc1 = Wave::Sine;
            p.osc2 = Wave::Triangle;
            p.osc2_semitones = 12.0;
            p.osc_mix = 0.12;
            p.noise_level = 0.06;
            p.cutoff = 3800.0;
            p.amp_env = Adsr::new(0.06, 0.2, 0.85, 0.25);
            p.lfo_rate = 5.2;
            p.lfo_to_pitch = 0.12;
            p.glide_ms = 140.0;
            p.gain = 0.55;
        }),
        "tanpura" | "drone" => synth(|p| {
            p.drift_cents = 3.0;
            p.osc1 = Wave::Saw;
            p.osc2 = Wave::Saw;
            p.osc2_semitones = 7.0;
            p.osc_mix = 0.4;
            p.unison = 3;
            p.unison_spread_cents = 6.0;
            p.stereo_spread = 0.6;
            p.cutoff = 1500.0;
            p.resonance = 0.35;
            p.amp_env = Adsr::new(0.8, 0.5, 0.9, 1.5);
            p.lfo_rate = 0.25;
            p.lfo_to_cutoff = 0.6;
            p.gain = 0.3;
        }),
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
            p.drift_cents = 4.0;
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
            p.drift_cents = 3.0;
            p.vel_to_cutoff = 0.8;
            p.unison = 7;
            p.stereo_spread = 0.7;
            p.unison_spread_cents = 28.0;
            p.osc_mix = 0.3;
            p.osc2_semitones = 12.0;
            p.cutoff = 6500.0;
            p.amp_env = Adsr::new(0.01, 0.3, 0.8, 0.35);
            p.gain = 0.35;
        }),
        "pluck_lead" | "pluck_synth" => synth(|p| {
            p.drift_cents = 2.0;
            p.vel_to_cutoff = 1.2;
            p.unison = 3;
            p.stereo_spread = 0.4;
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
            p.drift_cents = 4.0;
            p.vel_to_cutoff = 0.8;
            p.filter_env_amount = 0.6;
            p.filter_env = Adsr::new(0.8, 1.5, 0.4, 1.0);
            p.osc2 = Wave::Triangle;
            p.unison = 5;
            p.stereo_spread = 0.6;
            p.unison_spread_cents = 16.0;
            p.cutoff = 1700.0;
            p.amp_env = Adsr::new(0.6, 0.5, 0.85, 1.4);
            p.lfo_rate = 0.3;
            p.lfo_to_cutoff = 0.3;
            p.gain = 0.35;
        }),
        "dark_pad" => synth(|p| {
            p.drift_cents = 5.0;
            p.vel_to_cutoff = 0.6;
            p.osc1 = Wave::Saw;
            p.osc2 = Wave::Square;
            p.osc2_semitones = -12.0;
            p.unison = 5;
            p.stereo_spread = 0.6;
            p.unison_spread_cents = 20.0;
            p.cutoff = 700.0;
            p.resonance = 0.3;
            p.amp_env = Adsr::new(1.0, 1.0, 0.8, 2.0);
            p.lfo_rate = 0.12;
            p.lfo_to_cutoff = 1.0;
            p.gain = 0.35;
        }),
        "strings" => synth(|p| {
            p.drift_cents = 3.0;
            p.vel_to_cutoff = 1.0;
            p.unison = 5;
            p.stereo_spread = 0.6;
            p.unison_spread_cents = 10.0;
            p.cutoff = 3200.0;
            p.amp_env = Adsr::new(0.25, 0.3, 0.9, 0.8);
            p.lfo_rate = 5.0;
            p.lfo_to_pitch = 0.08;
            p.gain = 0.35;
        }),
        "brass_stab" | "brass" => synth(|p| {
            p.drift_cents = 3.0;
            p.vel_to_cutoff = 1.2;
            p.unison = 3;
            p.stereo_spread = 0.4;
            p.unison_spread_cents = 8.0;
            p.cutoff = 800.0;
            p.filter_env_amount = 2.5;
            p.filter_env = Adsr::new(0.03, 0.25, 0.3, 0.2);
            p.amp_env = Adsr::new(0.02, 0.2, 0.7, 0.15);
            p.gain = 0.45;
        }),
        "dark_keys" | "keys" => synth(|p| {
            p.drift_cents = 2.0;
            p.vel_to_cutoff = 1.0;
            p.filter_env_amount = 1.0;
            p.filter_env = Adsr::new(0.002, 0.4, 0.2, 0.3);
            p.osc1 = Wave::Triangle;
            p.osc2 = Wave::Square;
            p.osc2_semitones = 12.0;
            p.osc_mix = 0.2;
            p.cutoff = 2000.0;
            p.amp_env = Adsr::new(0.005, 0.6, 0.4, 0.5);
            p.gain = 0.5;
        }),
        "epiano" | "rhodes" => fm(|p| {
            p.detune_cents = 4.0;
            p.ratio = 1.0;
            p.index = 1.6;
            p.mod_env = Adsr::new(0.001, 0.9, 0.15, 0.4);
            p.amp_env = Adsr::new(0.002, 1.6, 0.35, 0.5);
            p.ratio2 = 14.0;
            p.index2 = 0.35;
            p.gain = 0.5;
        }),
        "fm_bell" | "bell" => fm(|p| {
            p.detune_cents = 3.0;
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
            damping: 0.6,
            brightness: 0.8,
            gain: 0.7,
            buzz: 0.0,
            kind: "guitar".into(),
        }),
        "grand_piano" | "piano" | "acoustic_piano" => Instrument::Piano(PianoParams::default()),
        "felt_piano" => Instrument::Piano(PianoParams {
            felt: 0.85,
            brightness: 0.6,
            hammer: 0.7,
            decay: 0.8,
            release: 0.3,
            body: 0.8,
            gain: 0.85,
            ..Default::default()
        }),
        "upright_keys" | "upright" => Instrument::Piano(PianoParams {
            felt: 0.3,
            brightness: 0.8,
            detune_cents: 3.0,
            decay: 0.7,
            body: 0.9,
            ..Default::default()
        }),
        "string_section" | "orchestra_strings" => Instrument::Ensemble(EnsembleParams {
            players: 8,
            ..Default::default()
        }),
        "staccato_strings" | "spiccato" => Instrument::Ensemble(EnsembleParams {
            players: 6,
            vibrato: 0.0,
            attack_jitter_ms: 8.0,
            amp_env: Adsr::new(0.008, 0.12, 0.0, 0.12),
            brightness: 1.0,
            noise: 0.5,
            gain: 0.6,
            ..Default::default()
        }),
        "cello_section" | "celli" => Instrument::Ensemble(EnsembleParams {
            players: 6,
            brightness: 0.55,
            vibrato: 0.6,
            amp_env: Adsr::new(0.3, 0.5, 0.9, 0.7),
            ..Default::default()
        }),
        "choir" | "choir_ooh" => Instrument::Ensemble(EnsembleParams {
            kind: EnsembleKind::Choir,
            players: 8,
            vowel: 0.1,
            vibrato: 0.35,
            noise: 0.2,
            amp_env: Adsr::new(0.4, 0.5, 0.9, 0.9),
            ..Default::default()
        }),
        "choir_aah" => Instrument::Ensemble(EnsembleParams {
            kind: EnsembleKind::Choir,
            players: 8,
            vowel: 0.9,
            vibrato: 0.4,
            amp_env: Adsr::new(0.3, 0.5, 0.9, 0.8),
            ..Default::default()
        }),
        "brass_section" => Instrument::Ensemble(EnsembleParams {
            kind: EnsembleKind::Brass,
            players: 4,
            vibrato: 0.2,
            brightness: 0.9,
            noise: 0.15,
            amp_env: Adsr::new(0.12, 0.3, 0.85, 0.3),
            ..Default::default()
        }),
        "wt_lead" => Instrument::Wavetable(WavetableParams {
            position: 0.66,
            position_env_amount: 0.3,
            unison: 3,
            stereo_spread: 0.4,
            cutoff: 5000.0,
            filter_env_amount: 1.5,
            amp_env: Adsr::new(0.005, 0.25, 0.75, 0.2),
            ..Default::default()
        }),
        "wt_pad" => Instrument::Wavetable(WavetableParams {
            table: WtTable::Harmonic,
            position: 0.35,
            position_lfo_rate: 0.15,
            position_lfo_depth: 0.25,
            unison: 5,
            unison_spread_cents: 18.0,
            stereo_spread: 0.7,
            cutoff: 4000.0,
            amp_env: Adsr::new(0.6, 0.8, 0.85, 1.2),
            gain: 0.4,
            ..Default::default()
        }),
        "wt_vocal" => Instrument::Wavetable(WavetableParams {
            table: WtTable::Vocal,
            position: 0.0,
            position_env_amount: 0.9,
            position_env: Adsr::new(0.3, 0.6, 0.6, 0.4),
            cutoff: 7000.0,
            amp_env: Adsr::new(0.02, 0.3, 0.8, 0.25),
            ..Default::default()
        }),
        "wt_growl" => Instrument::Wavetable(WavetableParams {
            table: WtTable::Digital,
            position: 0.2,
            position_lfo_rate: 4.0,
            position_lfo_depth: 0.35,
            sub_level: 0.5,
            cutoff: 1800.0,
            resonance: 0.35,
            drive: 0.4,
            amp_env: Adsr::new(0.003, 0.2, 0.8, 0.12),
            ..Default::default()
        }),
        "organ" => Instrument::Wavetable(WavetableParams {
            table: WtTable::Organ,
            position: 0.4,
            amp_env: Adsr::new(0.01, 0.05, 1.0, 0.08),
            gain: 0.45,
            ..Default::default()
        }),
        "granular_pad" => Instrument::Granular(GranularParams::default()),
        "granular_shimmer" => Instrument::Granular(GranularParams {
            position: 0.6,
            spray: 0.4,
            grain_ms: 45.0,
            density: 25.0,
            pitch_spread_cents: 25.0,
            reverse_prob: 0.4,
            amp_env: Adsr::new(0.05, 0.4, 0.6, 1.0),
            gain: 0.5,
            ..Default::default()
        }),
        "layered_kick" => Instrument::Layer(LayerParams {
            layers: vec![
                Layer::of(drum(DrumKind::Kick)),
                Layer {
                    gain_db: -10.0,
                    transpose: -24.0,
                    ..Layer::of(Instrument::Bass808(Bass808Params {
                        decay: 0.5,
                        punch: 6.0,
                        drive: 0.2,
                        ..Default::default()
                    }))
                },
            ],
        }),
        "layered_snare" => Instrument::Layer(LayerParams {
            layers: vec![
                Layer::of(drum(DrumKind::Snare)),
                Layer {
                    gain_db: -6.0,
                    delay_ms: 8.0,
                    ..Layer::of(drum(DrumKind::Clap))
                },
            ],
        }),
        "layered_keys" => Instrument::Layer(LayerParams {
            layers: vec![
                Layer::of(preset("epiano")?),
                Layer {
                    gain_db: -12.0,
                    ..Layer::of(preset("warm_pad")?)
                },
            ],
        }),
        "kick_punchy" => drum_x(DrumKind::Kick, 0.0, 0.85, 0.0, 0.7),
        "kick_tight" => drum_x(DrumKind::Kick, 2.0, 0.6, 0.1, 0.6),
        "kick_deep" => drum_x(DrumKind::Kick, -2.0, 1.4, 0.0, -0.4),
        "kick_dusty" => drum_x(DrumKind::Kick, -1.0, 0.8, 0.35, -0.7),
        "kick_grit" => drum_x(DrumKind::Kick, 0.0, 0.9, 0.6, 0.3),
        "snare_crack" => drum_x(DrumKind::Snare, 1.0, 0.9, 0.1, 0.6),
        "snare_tight" => drum_x(DrumKind::Snare, 3.0, 0.65, 0.15, 0.4),
        "snare_dusty" => drum_x(DrumKind::Snare, -1.0, 1.1, 0.35, -0.6),
        "snare_soft" => drum_x(DrumKind::Snare, 0.0, 1.15, 0.0, -0.3),
        "clap_wide" => drum_x(DrumKind::Clap, 0.0, 1.2, 0.0, 0.2),
        "hat_crisp" => drum_x(DrumKind::ClosedHat, 0.0, 0.8, 0.0, 0.4),
        "hat_drill" => drum_x(DrumKind::ClosedHat, 2.0, 0.6, 0.0, 0.9),
        "snare_drill" => drum_x(DrumKind::Snare, 4.0, 0.6, 0.1, 0.85),
        "dholak" => drum_x(DrumKind::Tabla, -5.0, 0.45, 0.15, 0.5),
        "dholak_bass" | "dholak_dagga" => drum_x(DrumKind::Bayan, 2.0, 0.6, 0.2, 0.2),
        "harmonium" | "peti" => synth(|p| {
            // two reed banks an octave apart beating against each other,
            // bellows attack, a reedy band-limited pulse
            p.osc1 = Wave::Square;
            p.osc2 = Wave::Saw;
            p.osc2_semitones = 12.0;
            p.osc2_cents = 5.0;
            p.osc_mix = 0.35;
            p.unison = 2;
            p.unison_spread_cents = 4.0;
            p.cutoff = 2600.0;
            p.resonance = 0.15;
            p.amp_env = Adsr::new(0.07, 0.3, 0.9, 0.15);
            p.lfo_rate = 4.5;
            p.lfo_to_cutoff = 0.08;
            p.drift_cents = 2.0;
            p.stereo_spread = 0.3;
            p.gain = 0.32;
        }),
        "hat_dusty" => drum_x(DrumKind::ClosedHat, -2.0, 1.1, 0.2, -0.6),
        "open_hat_airy" => drum_x(DrumKind::OpenHat, 0.0, 0.8, 0.0, 0.3),
        "open_hat_dusty" => drum_x(DrumKind::OpenHat, -2.0, 0.55, 0.15, -0.6),
        "808_dark" => Instrument::Bass808(Bass808Params {
            decay: 1.6,
            punch: 10.0,
            drive: 0.45,
            click: 0.15,
            sub: 0.5,
            tone: -0.6,
            ..Default::default()
        }),
        "808_grit" => Instrument::Bass808(Bass808Params {
            decay: 1.3,
            punch: 12.0,
            drive: 0.85,
            click: 0.45,
            sub: 0.35,
            tone: 0.5,
            gain: 0.8,
            ..Default::default()
        }),
        "808_slide" => Instrument::Bass808(Bass808Params {
            decay: 1.6,
            punch: 9.0,
            drive: 0.55,
            sustain: true,
            glide_ms: 120.0,
            click: 0.35,
            sub: 0.45,
            ..Default::default()
        }),
        "808_clean" => Instrument::Bass808(Bass808Params {
            decay: 0.9,
            punch: 8.0,
            drive: 0.2,
            click: 0.2,
            sub: 0.65,
            tone: -0.5,
            ..Default::default()
        }),
        "bass_round" => synth(|p| {
            p.osc1 = Wave::Triangle;
            p.osc2 = Wave::Saw;
            p.osc_mix = 0.2;
            p.sub_level = 0.5;
            p.cutoff = 380.0;
            p.filter_env_amount = 1.5;
            p.filter_env = Adsr::new(0.002, 0.15, 0.2, 0.1);
            p.amp_env = Adsr::new(0.004, 0.4, 0.6, 0.08);
            p.vel_to_cutoff = 1.0;
            p.drift_cents = 2.0;
            p.drive = 0.1;
            p.gain = 0.6;
        }),
        "keys_airy" => fm(|p| {
            p.ratio = 1.0;
            p.index = 1.1;
            p.mod_env = Adsr::new(0.001, 1.2, 0.2, 0.6);
            p.amp_env = Adsr::new(0.003, 2.2, 0.4, 0.9);
            p.ratio2 = 14.0;
            p.index2 = 0.2;
            p.detune_cents = 6.0;
            p.gain = 0.45;
        }),
        "keys_dusty" => fm(|p| {
            p.ratio = 1.0;
            p.index = 1.4;
            p.mod_env = Adsr::new(0.001, 0.7, 0.1, 0.4);
            p.amp_env = Adsr::new(0.003, 1.4, 0.3, 0.5);
            p.ratio2 = 7.0;
            p.index2 = 0.25;
            p.detune_cents = 9.0;
            p.gain = 0.5;
        }),
        "pad_airy" => synth(|p| {
            p.osc1 = Wave::Saw;
            p.osc2 = Wave::Triangle;
            p.osc2_semitones = 12.0;
            p.osc_mix = 0.35;
            p.unison = 5;
            p.unison_spread_cents = 14.0;
            p.stereo_spread = 0.8;
            p.drift_cents = 6.0;
            p.cutoff = 1400.0;
            p.filter_env_amount = 1.2;
            p.filter_env = Adsr::new(0.9, 1.6, 0.5, 1.5);
            p.amp_env = Adsr::new(0.8, 1.0, 0.85, 2.0);
            p.lfo_rate = 0.18;
            p.lfo_to_cutoff = 0.35;
            p.vel_to_cutoff = 0.8;
            p.gain = 0.3;
        }),
        "pad_dusty" => synth(|p| {
            p.osc1 = Wave::Triangle;
            p.osc2 = Wave::Square;
            p.osc_mix = 0.25;
            p.unison = 3;
            p.unison_spread_cents = 8.0;
            p.stereo_spread = 0.5;
            p.drift_cents = 9.0;
            p.noise_level = 0.015;
            p.cutoff = 900.0;
            p.filter_env_amount = 0.6;
            p.filter_env = Adsr::new(0.6, 1.2, 0.4, 1.0);
            p.amp_env = Adsr::new(0.5, 0.8, 0.8, 1.4);
            p.lfo_rate = 0.4;
            p.lfo_to_pitch = 0.04;
            p.gain = 0.35;
        }),
        "bell_glass" => fm(|p| {
            p.ratio = 3.5;
            p.index = 2.6;
            p.mod_env = Adsr::new(0.001, 1.6, 0.05, 1.2);
            p.amp_env = Adsr::new(0.001, 2.6, 0.0, 1.8);
            p.detune_cents = 4.0;
            p.gain = 0.35;
        }),
        "bell_dark" => fm(|p| {
            p.ratio = 4.0;
            p.index = 1.2;
            p.mod_env = Adsr::new(0.001, 0.25, 0.0, 0.3);
            p.amp_env = Adsr::new(0.001, 1.8, 0.0, 1.2);
            p.detune_cents = 3.0;
            p.gain = 0.45;
        }),
        "pluck_soft" => synth(|p| {
            p.osc1 = Wave::Square;
            p.osc2 = Wave::Saw;
            p.osc_mix = 0.4;
            p.unison = 2;
            p.unison_spread_cents = 8.0;
            p.stereo_spread = 0.3;
            p.drift_cents = 2.0;
            p.cutoff = 550.0;
            p.resonance = 0.2;
            p.filter_env_amount = 3.2;
            p.filter_env = Adsr::new(0.001, 0.25, 0.0, 0.2);
            p.amp_env = Adsr::new(0.002, 0.45, 0.0, 0.3);
            p.vel_to_cutoff = 1.5;
            p.gain = 0.5;
        }),
        "lead_dark" => synth(|p| {
            p.osc1 = Wave::Saw;
            p.osc2 = Wave::Square;
            p.osc2_cents = 9.0;
            p.osc_mix = 0.4;
            p.unison = 3;
            p.unison_spread_cents = 10.0;
            p.stereo_spread = 0.4;
            p.drift_cents = 4.0;
            p.cutoff = 1500.0;
            p.resonance = 0.3;
            p.filter_env_amount = 1.0;
            p.filter_env = Adsr::new(0.01, 0.4, 0.3, 0.3);
            p.amp_env = Adsr::new(0.01, 0.3, 0.8, 0.3);
            p.lfo_rate = 5.0;
            p.lfo_to_pitch = 0.05;
            p.vel_to_cutoff = 1.0;
            p.gain = 0.26;
        }),
        _ => return None,
    })
}

impl Layer {
    pub fn of(instrument: Instrument) -> Self {
        Layer {
            instrument,
            gain_db: 0.0,
            transpose: 0.0,
            key_min: 0,
            key_max: 127,
            vel_min: 0.0,
            vel_max: 1.0,
            delay_ms: 0.0,
        }
    }
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
    render_note_slide(inst, pitch, vel, gate, bank, seed, None)
}

/// Pitch offset (semitones) of a glide toward `slide_to` that ends at the
/// note's end (`gate`) and lasts `glide` seconds.
pub fn glide_semis(t: f32, pitch: f32, slide_to: Option<f32>, glide: f32, gate: f32) -> f32 {
    let Some(target) = slide_to else { return 0.0 };
    let g = glide.max(0.005);
    let t0 = (gate - g).max(0.0);
    let x = ((t - t0) / g.min(gate.max(0.005))).clamp(0.0, 1.0);
    (target - pitch) * x * x * (3.0 - 2.0 * x)
}

/// Render one note, optionally gliding to `slide_to` (808s, synths).
pub fn render_note_slide(
    inst: &Instrument,
    pitch: f32,
    vel: f32,
    gate: f32,
    bank: &SampleBank,
    seed: u64,
    slide_to: Option<f32>,
) -> Vec<f32> {
    if slide_to.is_some() {
        match inst {
            Instrument::Bass808(p) => {
                return render_808_slide(p, pitch, vel, gate, slide_to, &mut Rng::new(seed))
            }
            Instrument::Synth(p) => {
                return render_synth_slide(p, pitch, vel, gate, &mut Rng::new(seed), slide_to)
            }
            _ => {}
        }
    }
    let mut rng = Rng::new(seed);
    let vel = vel.clamp(0.0, 1.0);
    match inst {
        Instrument::Drum(p) => render_drum(p, pitch, vel, &mut rng),
        Instrument::Synth(p) => render_synth(p, pitch, vel, gate, &mut rng),
        Instrument::Fm(p) => render_fm(p, pitch, vel, gate),
        Instrument::Pluck(p) => render_pluck(p, pitch, vel, gate, &mut rng),
        Instrument::Bass808(p) => render_808(p, pitch, vel, gate, &mut rng),
        Instrument::Sampler(p) => render_sampler(p, pitch, vel, gate, bank),
        Instrument::Wavetable(p) => {
            crate::synth_extra::render_wavetable(p, pitch, vel, gate, &mut rng)
        }
        Instrument::Granular(p) => {
            crate::synth_extra::render_granular(p, pitch, vel, gate, bank, &mut rng)
        }
        Instrument::Layer(p) => crate::synth_extra::render_layer(p, pitch, vel, gate, bank, seed),
        Instrument::Multisample(p) => {
            crate::multisample::render_multisample(p, pitch, vel, gate, bank, seed)
        }
        Instrument::Piano(p) => crate::synth_extra::render_piano(p, pitch, vel, gate, &mut rng),
        Instrument::Ensemble(p) => {
            crate::synth_extra::render_ensemble(p, pitch, vel, gate, &mut rng)
        }
    }
}

fn secs(n: f32) -> usize {
    (n.max(0.0) * SR) as usize
}

fn render_drum(p: &DrumParams, pitch: f32, vel: f32, rng: &mut Rng) -> Vec<f32> {
    use crate::voice_pro as vp;
    // round-robin: every hit draws a small deterministic variation
    let var = vp::Variation::draw(rng, 1.0);
    let tune = 2f32.powf((p.tune + (pitch - 60.0)) / 12.0);
    let d = p.decay.clamp(0.05, 8.0);
    let tone = p.tone.clamp(-1.0, 1.0);
    let mut out;
    match p.kind {
        DrumKind::Kick => out = vp::kick(tune, d, vel, tone, var, rng),
        DrumKind::Snare => out = vp::snare(tune, d, vel, tone, var, rng),
        DrumKind::Clap => out = vp::clap(tune, d, vel, tone, var, rng),
        DrumKind::ClosedHat => out = vp::metal(0, tune, d, vel, tone, var, rng),
        DrumKind::OpenHat => out = vp::metal(1, tune, d, vel, tone, var, rng),
        DrumKind::Crash => out = vp::metal(2, tune, d, vel, tone, var, rng),
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
                // band-limited squares (the naive ones aliased audibly)
                let x = osc(Wave::Square, p1, 540.0 * tune / SR, rng)
                    + osc(Wave::Square, p2, 800.0 * tune / SR, rng);
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
        DrumKind::Tabla => {
            // dayan: near-harmonic modes (Raman), the fundamental rings,
            // a bright finger slap on top; soft hits are damped (te/ti)
            let open = vel >= 0.5;
            let len = if open { 1.1 * d } else { 0.14 * d };
            out = vec![0.0; secs(len)];
            let f0 = 261.63 * tune * 2f32.powf(var.tune / 12.0);
            let modes: [(f32, f32, f32); 5] = [
                (1.0, 1.0, 3.2),
                (2.0, 0.55, 5.0),
                (3.0, 0.35, 7.0),
                (4.0, 0.22, 9.0),
                (5.0, 0.12, 12.0),
            ];
            let mut ph = [0.0f32; 5];
            let mut bp = Svf::default();
            let damp = if open { 1.0 } else { 9.0 };
            for (i, s) in out.iter_mut().enumerate() {
                let t = i as f32 / SR;
                let mut y = 0.0;
                for (k, (r, a, dec)) in modes.iter().enumerate() {
                    // the head settles: pitch glides down a touch at the onset
                    let f = f0 * r * (1.0 + 0.012 * (-t * 40.0).exp());
                    ph[k] = (ph[k] + f / SR) % 1.0;
                    y += (2.0 * PI * ph[k]).sin() * a * (-t * dec * damp / d).exp();
                }
                let slap = bp.process(rng.bipolar(), 3200.0, 0.3, FilterMode::Bandpass)
                    * (-t * 90.0).exp()
                    * 0.6;
                *s = (y * 0.55 + slap).tanh() * 0.9;
            }
        }
        DrumKind::Bayan => {
            out = vec![0.0; secs(0.9 * d)];
            let mut ph = 0.0f32;
            let mut lp = Svf::default();
            for (i, s) in out.iter_mut().enumerate() {
                let t = i as f32 / SR;
                // ge: starts low and the palm pushes the pitch up (gamak)
                let f = 82.0
                    * tune
                    * 2f32.powf(var.tune / 12.0)
                    * (1.0 + 0.35 * (1.0 - (-t * 7.0).exp()));
                ph = (ph + f / SR) % 1.0;
                let body = (2.0 * PI * ph).sin() + 0.25 * (4.0 * PI * ph).sin();
                let thump =
                    lp.process(rng.bipolar(), 400.0, 0.2, FilterMode::Lowpass) * (-t * 60.0).exp();
                *s = (body * (-t * 3.5 / d).exp() * 0.8 + thump * 0.5).tanh();
            }
        }
    }
    let drive = 1.0 + p.drive.clamp(0.0, 1.0) * 6.0;
    if drive > 1.01 {
        for s in out.iter_mut() {
            *s = (*s * drive).tanh() / drive.tanh();
        }
    }
    // headroom: the layered voices are normalised to -1 dBFS before
    // velocity, the older ones only capped there
    let layered = matches!(
        p.kind,
        DrumKind::Kick
            | DrumKind::Snare
            | DrumKind::Clap
            | DrumKind::ClosedHat
            | DrumKind::OpenHat
            | DrumKind::Crash
    );
    let pk = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let norm = if pk > 1e-6 && (layered || pk > 0.89) {
        0.89 / pk
    } else {
        1.0
    };
    let g = norm * vel * var.level.min(1.0);
    for s in out.iter_mut() {
        *s *= g;
    }
    vp::fade_edges(&mut out, 16, 256);
    // the older generators stop at a fixed length while still sounding
    // (bayan -26 dB, tom ...): close their tails with a raised-cosine fade
    // so the voice decays to zero instead of being cut (the layered voices
    // shape their own tails)
    if !layered {
        fade_tail(&mut out, 0.25);
    }
    out
}

/// Raised-cosine fade over the last `frac` of a voice (at least 5 ms).
pub(crate) fn fade_tail(out: &mut [f32], frac: f32) {
    let n = out.len();
    let f = ((n as f32 * frac) as usize).max(secs(0.005)).min(n);
    if f == 0 {
        return;
    }
    let start = n - f;
    for (j, v) in out[start..].iter_mut().enumerate() {
        let x = (j as f32 + 0.5) / f as f32;
        *v *= 0.5 + 0.5 * (PI * x).cos();
    }
}

/// One-pole DC blocker with a sub-audio corner (`hz`), for voices whose
/// waveshaping turns an asymmetric waveform into a DC offset.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SubDcBlock {
    r: f32,
    x1: f32,
    y1: f32,
}

impl SubDcBlock {
    pub(crate) fn new(hz: f32) -> Self {
        SubDcBlock {
            r: (-2.0 * PI * hz / SR).exp(),
            x1: 0.0,
            y1: 0.0,
        }
    }
    pub(crate) fn process(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + self.r * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }
}

fn render_synth(p: &SynthParams, pitch: f32, vel: f32, gate: f32, rng: &mut Rng) -> Vec<f32> {
    render_synth_slide(p, pitch, vel, gate, rng, None)
}

fn render_synth_slide(
    p: &SynthParams,
    pitch: f32,
    vel: f32,
    gate: f32,
    rng: &mut Rng,
    slide_to: Option<f32>,
) -> Vec<f32> {
    let glide = if p.glide_ms > 0.0 { p.glide_ms } else { 80.0 } * 0.001;
    let total = p.amp_env.total(gate).min(12.0);
    let drive = 1.0 + p.drive.max(0.0) * 8.0;
    // tanh drive on a resonant / filtered saw (an asymmetric waveform)
    // creates a DC offset (acid_bass: 5-11% of its peak), stepping on at
    // every note-on and off at every note-off: block it below 10 Hz and let
    // the blocker settle inside the voice
    let dc_tail = if drive > 1.01 { 0.08 } else { 0.0 };
    let n = secs(total + dc_tail);
    let mut out = vec![0.0f32; n];
    let mut dcb = SubDcBlock::new(10.0);
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
    // analog drift: each voice wanders on its own smoothed random walk
    let drift = p.drift_cents.max(0.0);
    let mut drift_now: Vec<f32> = (0..voices).map(|_| rng.bipolar() * drift).collect();
    let mut drift_to: Vec<f32> = (0..voices).map(|_| rng.bipolar() * drift).collect();
    let drift_step = (0.35 * SR) as usize;
    // velocity to brightness (0.8 is neutral)
    let vel_oct = p.vel_to_cutoff * (vel - 0.8);
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        if drift > 0.0 {
            if i % drift_step == 0 && i > 0 {
                for d in drift_to.iter_mut() {
                    *d = rng.bipolar() * drift;
                }
            }
            for (now, to) in drift_now.iter_mut().zip(drift_to.iter()) {
                *now += (to - *now) * (3.0 / SR);
            }
        }
        let lfo = (2.0 * PI * (p.lfo_rate * t + lfo_ph0)).sin();
        let mut semis = lfo * p.lfo_to_pitch + glide_semis(t, pitch, slide_to, glide, gate);
        if p.pitch_env_semitones != 0.0 {
            semis += p.pitch_env_semitones * (-t / p.pitch_env_time.max(0.001)).exp();
        }
        let f = base * 2f32.powf(semis / 12.0);
        let mut x = 0.0;
        for v in 0..voices {
            let f1 = f * 2f32.powf((detunes[v] + drift_now[v]) / 1200.0);
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
        let cutoff =
            p.cutoff * 2f32.powf(p.filter_env_amount * fenv + lfo * p.lfo_to_cutoff + vel_oct);
        let mut y = filt.process(x, cutoff, p.resonance, p.filter_mode);
        if p.resonance > 0.6 && p.filter_mode == FilterMode::Lowpass {
            // 4-pole-ish for acid squelch
            y = filt2.process(y, cutoff * 1.2, 0.0, FilterMode::Lowpass);
        }
        let mut v = if drive > 1.01 {
            (y * drive).tanh() / drive.tanh().max(0.5)
        } else {
            y
        } * p.amp_env.level(t, gate)
            * vel
            * p.gain;
        if drive > 1.01 {
            v = dcb.process(v);
        }
        *s = v;
    }
    if dc_tail > 0.0 {
        fade_tail(&mut out, dc_tail / (total + dc_tail));
    }
    out
}

fn render_fm(p: &FmParams, pitch: f32, vel: f32, gate: f32) -> Vec<f32> {
    let total = p.amp_env.total(gate).min(12.0);
    let mut out = vec![0.0f32; secs(total)];
    let f = midi_to_hz(pitch);
    let (mut pc, mut pc2, mut pm, mut pm2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut last = 0.0f32;
    // velocity also brightens FM
    let vel_index = 0.5 + vel * 0.7;
    // keyscaling: high notes get a smaller index (real tines/bars do, and
    // it keeps the sidebands under Nyquist)
    let keyscale = if f > 523.0 { (523.0 / f).sqrt() } else { 1.0 };
    // the tine modulator fades out before its sidebands would alias
    let tine_hz = f * p.ratio2;
    let tine_gain = if p.ratio2 > 0.0 {
        ((16_000.0 - tine_hz) / 6_000.0).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let f2 = f * 2f32.powf(p.detune_cents / 1200.0);
    let two = p.detune_cents != 0.0;
    let mut dcb = crate::voice_pro::SubDc::default();
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        let menv = p.mod_env.level(t, gate);
        pm = (pm + f * p.ratio / SR) % 1.0;
        let mut m =
            (2.0 * PI * pm + last * p.feedback).sin() * p.index * menv * vel_index * keyscale;
        if tine_gain > 0.0 {
            pm2 = (pm2 + tine_hz / SR) % 1.0;
            m += (2.0 * PI * pm2).sin() * p.index2 * (-t * 12.0).exp() * tine_gain;
        }
        pc = (pc + f / SR) % 1.0;
        let mut y = (2.0 * PI * pc + m).sin();
        if two {
            pc2 = (pc2 + f2 / SR) % 1.0;
            y = 0.5 * (y + (2.0 * PI * pc2 + m).sin());
        }
        last = y;
        *s = dcb.process(y * p.amp_env.level(t, gate)) * vel * p.gain;
    }
    out
}

fn pluck_kind(p: &PluckParams) -> &str {
    if !p.kind.is_empty() {
        p.kind.as_str()
    } else if p.buzz >= 0.5 {
        "sitar"
    } else if p.brightness >= 0.99 {
        "santoor"
    } else {
        "guitar"
    }
}

/// One Karplus-Strong string, tuned exactly (two-tap loss filter + allpass
/// fractional delay), excited by a shaped pick/hammer burst. Returns the
/// string's output (the loop signal).
fn ks_string(fs: f32, vel: f32, kind: &str, p: &PluckParams, n: usize, t60: f32, rng: &mut Rng) -> Vec<f32> {
    let period = (SR / fs).max(4.0);
    // loss filter H = (1-s) + s z^-1: darker = larger s (its delay is ~s samples)
    let bright = p.brightness.clamp(0.0, 1.0);
    let s = (0.45 - 0.42 * bright * (0.7 + 0.3 * vel)).clamp(0.04, 0.5);
    // santoor: thin steel strings struck by light mallets keep their upper
    // partials ringing (the shimmer); the loop low-pass loses half as much
    // per pass, so the 2-6 kHz partials live about twice as long
    let s = if kind == "santoor" { (s * 0.5).max(0.02) } else { s };
    let nd = ((period - s - 0.15).floor() as usize).max(2);
    let d = (period - nd as f32 - s).clamp(0.05, 1.2);
    let c = (1.0 - d) / (1.0 + d);
    // per-pass gain from the wanted T60
    let g = 10f32.powf(-3.0 / (t60.max(0.05) * fs));
    // excitation
    let mut ex = vec![0.0f32; nd];
    if kind == "santoor" {
        // a felt-less hammer: a short raised-cosine pulse plus a little noise.
        // The pulse width sets the brightness: a raised cosine T long has its
        // spectrum's main lobe out to 2/T, so the old ~0.56 ms pulse left
        // almost nothing above ~3.6 kHz to ring (santoor A/B, c29b6dd: longer
        // decay but no brighter onset). ~0.22 ms reaches ~9 kHz.
        let w = ((SR * 0.00028 * (1.6 - vel)) as usize).clamp(2, nd.max(3) - 1);
        let at = (nd as f32 * 0.12) as usize;
        for k in 0..w {
            let i = (at + k) % nd;
            ex[i] += 0.5 - 0.5 * (std::f32::consts::TAU * k as f32 / w as f32).cos();
        }
        for v in ex.iter_mut() {
            *v += 0.18 * rng.bipolar();
        }
    } else {
        for v in ex.iter_mut() {
            *v = rng.bipolar();
        }
    }
    // soften the burst: harder playing = brighter
    let fc = if kind == "santoor" { 3000.0 + 9000.0 * bright * (0.4 + 0.6 * vel) } else { 900.0 + 7000.0 * bright * (0.35 + 0.65 * vel) };
    let a = (-2.0 * std::f32::consts::PI * fc / SR).exp();
    let mut z = 0.0;
    for _ in 0..(if kind == "santoor" { 1 } else { 2 }) {
        for v in ex.iter_mut() {
            z = (1.0 - a) * *v + a * z;
            *v = z;
        }
    }
    // pick position comb: plucking near the bridge thins the low partials
    let beta = match kind {
        "sitar" => 0.09,
        "santoor" => 0.13,
        _ => 0.18,
    };
    let lag = ((beta * nd as f32) as usize).max(1);
    let orig = ex.clone();
    for i in 0..nd {
        ex[i] = orig[i] - orig[(i + nd - lag) % nd];
    }
    // no thump: high-pass the burst (~120 Hz one-pole)
    let hp_a = (-2.0 * std::f32::consts::PI * 120.0 / SR).exp();
    let (mut px, mut py) = (0.0f32, 0.0f32);
    for v in ex.iter_mut() {
        let y = hp_a * (py + *v - px);
        px = *v;
        py = y;
        *v = y;
    }
    let mean = ex.iter().sum::<f32>() / nd as f32;
    let pk = ex.iter().fold(0.0f32, |m, v| m.max((v - mean).abs())).max(1e-6);
    let mut buf: Vec<f32> = ex.iter().map(|v| (v - mean) / pk).collect();
    let mut out = vec![0.0f32; n];
    let (mut idx, mut prev, mut ax1, mut ay1) = (0usize, 0.0f32, 0.0f32, 0.0f32);
    for o in out.iter_mut() {
        let x = buf[idx];
        *o = x;
        let lp = (1.0 - s) * x + s * prev;
        prev = x;
        let ap = c * lp + ax1 - c * ay1;
        ax1 = lp;
        ay1 = ap;
        buf[idx] = g * ap;
        idx += 1;
        if idx == nd {
            idx = 0;
        }
    }
    out
}

fn render_pluck(p: &PluckParams, pitch: f32, vel: f32, gate: f32, rng: &mut Rng) -> Vec<f32> {
    use crate::dsp::{Biquad, BiquadKind};
    let kind = pluck_kind(p).to_string();
    let kind = kind.as_str();
    let f = midi_to_hz(pitch).clamp(20.0, 4000.0);
    let damp = p.damping.clamp(0.0, 1.0);
    let base_ring = match kind {
        "sitar" => 4.0,
        "santoor" => 2.6,
        _ => 1.8,
    } * (1.0 - 0.75 * damp);
    // higher notes die sooner, as on a real string
    let t60 = (base_ring * (220.0 / f).powf(0.35)).clamp(0.25, 8.0);
    let total = (gate + 0.35 * t60 + 0.15).clamp(0.25, 6.0);
    let n = secs(total);
    // courses: santoor strings come in detuned pairs, a guitar string beats a little
    let detunes: &[f32] = match kind {
        // a santoor course is up to four strings in near-unison: three
        // slightly detuned ones give its chorus-like shimmer
        "santoor" => &[-3.5, 0.6, 3.0],
        "guitar" => &[0.0, 1.2],
        _ => &[0.0],
    };
    let mut y = vec![0.0f32; n];
    for &cents in detunes {
        let s = ks_string(f * 2f32.powf(cents / 1200.0), vel, kind, p, n, t60, rng);
        let k = 1.0 / detunes.len() as f32;
        for (a, b) in y.iter_mut().zip(s.iter()) {
            *a += b * k;
        }
    }
    // jawari (sitar): the curved bridge adds a buzzing upper band that lives on
    // the string's envelope; generated outside the loop so it never runs away
    if p.buzz > 0.0 {
        let b = p.buzz.clamp(0.0, 1.0);
        let mut hp = Biquad::new(BiquadKind::LowCut, (f * 2.5).min(3000.0), 0.7, 0.0);
        let mut lp = Biquad::new(BiquadKind::HighCut, 6500.0, 0.6, 0.0);
        for v in y.iter_mut() {
            let r = v.abs();
            let bz = lp.process(hp.process(r * r.sqrt()));
            *v += b * 0.55 * bz;
        }
    }
    // sympathetic strings (sitar): the octave and fifth ring along, quietly
    if kind == "sitar" {
        for (ratio, amt) in [(2.0f32, 0.05f32), (1.5, 0.035)] {
            let fs = f * ratio;
            let nd = ((SR / fs) as usize).max(2);
            let g = 10f32.powf(-3.0 / (6.0 * fs));
            let mut buf = vec![0.0f32; nd];
            let mut idx = 0;
            let mut prev = 0.0;
            for v in y.iter_mut() {
                let x = buf[idx];
                let lp = 0.5 * (x + prev);
                prev = x;
                buf[idx] = g * lp + 0.004 * *v;
                idx = (idx + 1) % nd;
                *v += amt * x;
            }
        }
    }
    // the body: a few broad resonances under the string
    let body: &[(f32, f32, f32)] = match kind {
        "sitar" => &[(170.0, 3.0, 0.45), (410.0, 4.0, 0.35), (1050.0, 5.0, 0.18)],
        "santoor" => &[(260.0, 4.0, 0.35), (640.0, 5.0, 0.3), (1700.0, 6.0, 0.15)],
        _ => &[(105.0, 3.0, 0.5), (210.0, 4.0, 0.4), (470.0, 5.0, 0.2)],
    };
    let mut bands: Vec<(Biquad, f32)> = body.iter().map(|&(fr, q, g)| (Biquad::new(BiquadKind::Bandpass, fr, q, 0.0), g)).collect();
    // tone: tame the 2-5 kHz edge and the fizz above
    let (hc, edge) = match kind {
        // santoor: the steel-string edge is the instrument (was -1.5 dB)
        "santoor" => (14000.0, 1.5),
        "sitar" => (8000.0, -5.0),
        _ => (7000.0, -3.5),
    };
    let mut cut = Biquad::new(BiquadKind::HighCut, hc, 0.7, 0.0);
    // de-honk: the 800 Hz-1.5 kHz pile-up the critic measured
    let mut honk = Biquad::new(BiquadKind::Bell, 1100.0, 0.8, -2.5);
    let mut air = Biquad::new(BiquadKind::HighShelf, 7000.0, 0.7, if kind == "santoor" { 4.0 } else { 2.0 });
    let mut bell = Biquad::new(BiquadKind::Bell, 3300.0, 0.9, edge);
    let mut shelf = Biquad::new(BiquadKind::HighShelf, 6000.0, 0.7, if kind == "santoor" { 0.0 } else { -2.5 });
    let mut dc = DcBlock::default();
    let rel_rate = match kind {
        "santoor" => 2.5,
        "sitar" => 3.5,
        _ => 6.0,
    };
    let mut peak = 0.0f32;
    for (i, v) in y.iter_mut().enumerate() {
        let t = i as f32 / SR;
        let mut b = 0.0;
        for (bq, g) in bands.iter_mut() {
            b += bq.process(*v) * *g;
        }
        let mut s = 0.8 * *v + b;
        s = air.process(honk.process(shelf.process(bell.process(cut.process(s)))));
        let rel = if t > gate + 0.05 { (-(t - gate - 0.05) * rel_rate).exp() } else { 1.0 };
        *v = dc.process(s) * rel;
        peak = peak.max(v.abs());
    }
    let norm = if peak > 1e-6 { 0.9 / peak } else { 0.0 };
    for v in y.iter_mut() {
        *v *= norm * vel * p.gain;
    }
    crate::voice_pro::fade_edges(&mut y, 32, (0.01 * SR) as usize);
    y
}
fn render_808(p: &Bass808Params, pitch: f32, vel: f32, gate: f32, rng: &mut Rng) -> Vec<f32> {
    render_808_slide(p, pitch, vel, gate, None, rng)
}

fn render_808_slide(
    p: &Bass808Params,
    pitch: f32,
    vel: f32,
    gate: f32,
    slide_to: Option<f32>,
    rng: &mut Rng,
) -> Vec<f32> {
    render_808_core(p, pitch, vel, gate, slide_to, None, rng)
}

fn pro_808(p: &Bass808Params) -> crate::voice_pro::Bass808 {
    crate::voice_pro::Bass808 {
        decay: p.decay,
        punch: p.punch,
        drive: p.drive,
        sustain: p.sustain,
        gain: p.gain,
        glide_s: (if p.glide_ms > 0.0 { p.glide_ms } else { 90.0 }) * 0.001,
        click: p.click,
        sub: p.sub,
        tone: p.tone,
    }
}

fn render_808_core(
    p: &Bass808Params,
    pitch: f32,
    vel: f32,
    gate: f32,
    slide_to: Option<f32>,
    legato: Option<Legato808>,
    rng: &mut Rng,
) -> Vec<f32> {
    crate::voice_pro::bass808(
        &pro_808(p),
        pitch,
        vel,
        gate,
        slide_to,
        legato.map(|l| (l.phase, l.elapsed_s)),
        rng,
    )
}

/// Where a mono 808 voice that was slid into continues from: the phase the
/// previous (gliding) voice had reached at the hand-off and how long it had
/// been sounding (so the sustain envelope carries on instead of
/// re-triggering).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Legato808 {
    pub phase: f32,
    pub elapsed_s: f32,
}

/// Oscillator phase of an 808 voice after `n` samples (exactly the
/// accumulation the voice does), for a legato hand-off.
pub fn phase_808_after(
    p: &Bass808Params,
    pitch: f32,
    gate: f32,
    slide_to: Option<f32>,
    n: usize,
) -> f32 {
    crate::voice_pro::phase_808_after(&pro_808(p), pitch, gate, slide_to, n)
}

/// An 808 voice. With `legato` it continues a voice that glided into this
/// pitch: same phase, no punch, no click, no attack and no sustain
/// re-trigger, so the slide lands instead of re-attacking.
pub fn render_808_voice(
    p: &Bass808Params,
    pitch: f32,
    vel: f32,
    gate: f32,
    slide_to: Option<f32>,
    legato: Option<Legato808>,
) -> Vec<f32> {
    let mut rng = Rng::new(0x808 ^ (pitch * 16.0) as u64);
    render_808_core(p, pitch, vel, gate, slide_to, legato, &mut rng)
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
    if !p.slices.is_empty() && !p.reverse {
        // slice kit: note root+i plays slice i at original pitch
        let i = (pitch - p.root as f32).round();
        if i < 0.0 || i as usize >= p.slices.len() {
            return Vec::new();
        }
        let i = i as usize;
        let s0 = ((p.slices[i].max(0.0) * SR) as usize).min(data.len());
        let s1 = p
            .slices
            .get(i + 1)
            .map(|t| ((t * SR) as usize).min(data.len()))
            .unwrap_or(data.len())
            .max(s0);
        let mut len = s1 - s0;
        if !p.one_shot {
            len = len.min(secs(gate + p.release));
        }
        if p.max_length > 0.0 {
            len = len.min(secs(p.max_length));
        }
        let fade = (0.003 * SR) as usize;
        return (0..len)
            .map(|k| {
                let a = (k as f32 / (p.attack.max(0.0005) * SR)).min(1.0);
                let r = ((len - k) as f32 / fade.max(1) as f32).min(1.0);
                data[s0 + k] * a * r * vel * p.gain
            })
            .collect();
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
