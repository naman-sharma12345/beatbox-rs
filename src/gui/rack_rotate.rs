//! Manual rack rhythm rotation with the approved exact typed B1 eligibility.
//! Exact current typed data, not Engine content equality or serialized equality.
//! Generated named walks are exhaustive: no rest patterns and no blanket fallback.
//! The only fallbacks below compare two different enum variants, never skip fields.
use crate::project::Project;
use eframe::egui;
use std::collections::{BTreeMap, BTreeSet};

trait B1Data {
    fn exact(&self, other: &Self) -> bool;
    fn finite(&self) -> bool;
}
macro_rules! discrete {
    ($($t:ty),+ $(,)?) => {$(
        impl B1Data for $t {
            fn exact(&self, other: &Self) -> bool { self == other }
            fn finite(&self) -> bool { true }
        }
    )+};
}
discrete!(String, bool, u8, u16, u32, u64, usize, i32);
macro_rules! floating {
    ($($t:ty),+ $(,)?) => {$(
        impl B1Data for $t {
            fn exact(&self, other: &Self) -> bool { self.to_bits() == other.to_bits() }
            fn finite(&self) -> bool { self.is_finite() }
        }
    )+};
}
floating!(f32, f64);
impl<T: B1Data> B1Data for Vec<T> {
    fn exact(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().zip(other).all(|(a, b)| a.exact(b))
    }
    fn finite(&self) -> bool {
        self.iter().all(B1Data::finite)
    }
}
impl<T: B1Data> B1Data for Option<T> {
    fn exact(&self, other: &Self) -> bool {
        match (self, other) {
            (None, None) => true,
            (Some(a), Some(b)) => a.exact(b),
            _ => false,
        }
    }
    fn finite(&self) -> bool {
        self.as_ref().is_none_or(B1Data::finite)
    }
}
impl<T: B1Data> B1Data for Box<T> {
    fn exact(&self, other: &Self) -> bool {
        (**self).exact(&**other)
    }
    fn finite(&self) -> bool {
        (**self).finite()
    }
}
impl<K: Ord + B1Data, V: B1Data> B1Data for BTreeMap<K, V> {
    fn exact(&self, other: &Self) -> bool {
        // Ordered key equality includes key presence, even for empty lane values.
        self.len() == other.len()
            && self
                .iter()
                .zip(other)
                .all(|((ak, av), (bk, bv))| ak.exact(bk) && av.exact(bv))
    }
    fn finite(&self) -> bool {
        self.iter().all(|(k, v)| k.finite() && v.finite())
    }
}
impl<T: Ord + B1Data> B1Data for BTreeSet<T> {
    fn exact(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().zip(other).all(|(a, b)| a.exact(b))
    }
    fn finite(&self) -> bool {
        self.iter().all(B1Data::finite)
    }
}
macro_rules! tuple_data {
    ($first:ident:$first_i:tt $(,$t:ident:$i:tt)*) => {
        impl<$first: B1Data $(,$t: B1Data)*> B1Data for ($first, $($t,)*) {
            fn exact(&self, other: &Self) -> bool { self.$first_i.exact(&other.$first_i) $(&& self.$i.exact(&other.$i))* }
            fn finite(&self) -> bool { self.$first_i.finite() $(&& self.$i.finite())* }
        }
    };
}
tuple_data!(A:0,B:1);
tuple_data!(A:0,B:1,C:2);
tuple_data!(A:0,B:1,C:2,D:3);

// Pinned serde_json 1.0.151 default representation: positive integers (including
// zero), negative integers, or finite f64. Integer APIs overlap for small positive
// values, so test unsigned first; never convert an integer through f64.
impl B1Data for serde_json::Number {
    fn exact(&self, other: &Self) -> bool {
        if self.is_f64() || other.is_f64() {
            return self.is_f64()
                && other.is_f64()
                && match (self.as_f64(), other.as_f64()) {
                    (Some(a), Some(b)) => a.exact(&b),
                    _ => false,
                };
        }
        if self.is_u64() || other.is_u64() {
            return self.is_u64() && other.is_u64() && self.as_u64() == other.as_u64();
        }
        self.is_i64() && other.is_i64() && self.as_i64() == other.as_i64()
    }
    fn finite(&self) -> bool {
        if self.is_f64() {
            self.as_f64().is_some_and(f64::is_finite)
        } else {
            self.is_u64() || self.is_i64()
        }
    }
}
impl B1Data for serde_json::Value {
    fn exact(&self, other: &Self) -> bool {
        use serde_json::Value;
        // Exhaustive outer match deliberately catches any future Value variant.
        match self {
            Value::Null => matches!(other, Value::Null),
            Value::Bool(a) => match other {
                Value::Bool(b) => a.exact(b),
                _ => false,
            },
            Value::Number(a) => match other {
                Value::Number(b) => a.exact(b),
                _ => false,
            },
            Value::String(a) => match other {
                Value::String(b) => a.exact(b),
                _ => false,
            },
            Value::Array(a) => match other {
                Value::Array(b) => a.exact(b),
                _ => false,
            },
            Value::Object(a) => match other {
                Value::Object(b) => {
                    a.len() == b.len()
                        && a.iter().all(|(k, v)| b.get(k).is_some_and(|w| v.exact(w)))
                }
                _ => false,
            },
        }
    }
    fn finite(&self) -> bool {
        use serde_json::Value;
        match self {
            Value::Null | Value::Bool(_) | Value::String(_) => true,
            Value::Number(n) => n.finite(),
            Value::Array(a) => a.finite(),
            Value::Object(a) => a.values().all(B1Data::finite),
        }
    }
}

// Current source: src/dsp.rs, Adsr.
impl B1Data for crate::dsp::Adsr {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            attack: a_attack,
            decay: a_decay,
            sustain: a_sustain,
            release: a_release,
        } = self;
        let Self {
            attack: b_attack,
            decay: b_decay,
            sustain: b_sustain,
            release: b_release,
        } = other;
        a_attack.exact(b_attack)
            && a_decay.exact(b_decay)
            && a_sustain.exact(b_sustain)
            && a_release.exact(b_release)
    }
    fn finite(&self) -> bool {
        let Self {
            attack,
            decay,
            sustain,
            release,
        } = self;
        attack.finite() && decay.finite() && sustain.finite() && release.finite()
    }
}

// Current source: src/fx.rs, AmpSimFx.
impl B1Data for crate::fx::AmpSimFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            model: a_model,
            gain_db: a_gain_db,
            bass: a_bass,
            mid: a_mid,
            treble: a_treble,
            presence: a_presence,
            master_db: a_master_db,
            cab: a_cab,
            mic: a_mic,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            model: b_model,
            gain_db: b_gain_db,
            bass: b_bass,
            mid: b_mid,
            treble: b_treble,
            presence: b_presence,
            master_db: b_master_db,
            cab: b_cab,
            mic: b_mic,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_model.exact(b_model)
            && a_gain_db.exact(b_gain_db)
            && a_bass.exact(b_bass)
            && a_mid.exact(b_mid)
            && a_treble.exact(b_treble)
            && a_presence.exact(b_presence)
            && a_master_db.exact(b_master_db)
            && a_cab.exact(b_cab)
            && a_mic.exact(b_mic)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            model,
            gain_db,
            bass,
            mid,
            treble,
            presence,
            master_db,
            cab,
            mic,
            mix,
            id,
            bypass,
        } = self;
        model.finite()
            && gain_db.finite()
            && bass.finite()
            && mid.finite()
            && treble.finite()
            && presence.finite()
            && master_db.finite()
            && cab.finite()
            && mic.finite()
            && mix.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/project.rs, AudioClip.
impl B1Data for crate::project::AudioClip {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            id: a_id,
            track: a_track,
            sample: a_sample,
            start_beat: a_start_beat,
            offset_s: a_offset_s,
            length_s: a_length_s,
            gain_db: a_gain_db,
            pitch_st: a_pitch_st,
            pitch_voice: a_pitch_voice,
            stretch: a_stretch,
            reverse: a_reverse,
            muted: a_muted,
            take_group: a_take_group,
            stretch_mode: a_stretch_mode,
            fade_in_ms: a_fade_in_ms,
            fade_out_ms: a_fade_out_ms,
        } = self;
        let Self {
            id: b_id,
            track: b_track,
            sample: b_sample,
            start_beat: b_start_beat,
            offset_s: b_offset_s,
            length_s: b_length_s,
            gain_db: b_gain_db,
            pitch_st: b_pitch_st,
            pitch_voice: b_pitch_voice,
            stretch: b_stretch,
            reverse: b_reverse,
            muted: b_muted,
            take_group: b_take_group,
            stretch_mode: b_stretch_mode,
            fade_in_ms: b_fade_in_ms,
            fade_out_ms: b_fade_out_ms,
        } = other;
        a_id.exact(b_id)
            && a_track.exact(b_track)
            && a_sample.exact(b_sample)
            && a_start_beat.exact(b_start_beat)
            && a_offset_s.exact(b_offset_s)
            && a_length_s.exact(b_length_s)
            && a_gain_db.exact(b_gain_db)
            && a_pitch_st.exact(b_pitch_st)
            && a_pitch_voice.exact(b_pitch_voice)
            && a_stretch.exact(b_stretch)
            && a_reverse.exact(b_reverse)
            && a_muted.exact(b_muted)
            && a_take_group.exact(b_take_group)
            && a_stretch_mode.exact(b_stretch_mode)
            && a_fade_in_ms.exact(b_fade_in_ms)
            && a_fade_out_ms.exact(b_fade_out_ms)
    }
    fn finite(&self) -> bool {
        let Self {
            id,
            track,
            sample,
            start_beat,
            offset_s,
            length_s,
            gain_db,
            pitch_st,
            pitch_voice,
            stretch,
            reverse,
            muted,
            take_group,
            stretch_mode,
            fade_in_ms,
            fade_out_ms,
        } = self;
        id.finite()
            && track.finite()
            && sample.finite()
            && start_beat.finite()
            && offset_s.finite()
            && length_s.finite()
            && gain_db.finite()
            && pitch_st.finite()
            && pitch_voice.finite()
            && stretch.finite()
            && reverse.finite()
            && muted.finite()
            && take_group.finite()
            && stretch_mode.finite()
            && fade_in_ms.finite()
            && fade_out_ms.finite()
    }
}

// Current source: src/automation.rs, AutoPoint.
impl B1Data for crate::automation::AutoPoint {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            beat: a_beat,
            value: a_value,
            curve: a_curve,
        } = self;
        let Self {
            beat: b_beat,
            value: b_value,
            curve: b_curve,
        } = other;
        a_beat.exact(b_beat) && a_value.exact(b_value) && a_curve.exact(b_curve)
    }
    fn finite(&self) -> bool {
        let Self { beat, value, curve } = self;
        beat.finite() && value.finite() && curve.finite()
    }
}

// Current source: src/project.rs, AutomationClip.
impl B1Data for crate::project::AutomationClip {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            id: a_id,
            name: a_name,
            target: a_target,
            param: a_param,
            length_beats: a_length_beats,
            points: a_points,
            placements: a_placements,
        } = self;
        let Self {
            id: b_id,
            name: b_name,
            target: b_target,
            param: b_param,
            length_beats: b_length_beats,
            points: b_points,
            placements: b_placements,
        } = other;
        a_id.exact(b_id)
            && a_name.exact(b_name)
            && a_target.exact(b_target)
            && a_param.exact(b_param)
            && a_length_beats.exact(b_length_beats)
            && a_points.exact(b_points)
            && a_placements.exact(b_placements)
    }
    fn finite(&self) -> bool {
        let Self {
            id,
            name,
            target,
            param,
            length_beats,
            points,
            placements,
        } = self;
        id.finite()
            && name.finite()
            && target.finite()
            && param.finite()
            && length_beats.finite()
            && points.finite()
            && placements.finite()
    }
}

// Current source: src/automation.rs, AutomationLane.
impl B1Data for crate::automation::AutomationLane {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            id: a_id,
            target: a_target,
            param: a_param,
            points: a_points,
            enabled: a_enabled,
        } = self;
        let Self {
            id: b_id,
            target: b_target,
            param: b_param,
            points: b_points,
            enabled: b_enabled,
        } = other;
        a_id.exact(b_id)
            && a_target.exact(b_target)
            && a_param.exact(b_param)
            && a_points.exact(b_points)
            && a_enabled.exact(b_enabled)
    }
    fn finite(&self) -> bool {
        let Self {
            id,
            target,
            param,
            points,
            enabled,
        } = self;
        id.finite() && target.finite() && param.finite() && points.finite() && enabled.finite()
    }
}

// Current source: src/fx.rs, AutopanFx.
impl B1Data for crate::fx::AutopanFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            steps: a_steps,
            depth: a_depth,
            tremolo: a_tremolo,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            steps: b_steps,
            depth: b_depth,
            tremolo: b_tremolo,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_steps.exact(b_steps)
            && a_depth.exact(b_depth)
            && a_tremolo.exact(b_tremolo)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            steps,
            depth,
            tremolo,
            id,
            bypass,
        } = self;
        steps.finite() && depth.finite() && tremolo.finite() && id.finite() && bypass.finite()
    }
}

// Current source: src/instruments.rs, Bass808Params.
impl B1Data for crate::instruments::Bass808Params {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            decay: a_decay,
            punch: a_punch,
            drive: a_drive,
            sustain: a_sustain,
            gain: a_gain,
            glide_ms: a_glide_ms,
            click: a_click,
            sub: a_sub,
            tone: a_tone,
        } = self;
        let Self {
            decay: b_decay,
            punch: b_punch,
            drive: b_drive,
            sustain: b_sustain,
            gain: b_gain,
            glide_ms: b_glide_ms,
            click: b_click,
            sub: b_sub,
            tone: b_tone,
        } = other;
        a_decay.exact(b_decay)
            && a_punch.exact(b_punch)
            && a_drive.exact(b_drive)
            && a_sustain.exact(b_sustain)
            && a_gain.exact(b_gain)
            && a_glide_ms.exact(b_glide_ms)
            && a_click.exact(b_click)
            && a_sub.exact(b_sub)
            && a_tone.exact(b_tone)
    }
    fn finite(&self) -> bool {
        let Self {
            decay,
            punch,
            drive,
            sustain,
            gain,
            glide_ms,
            click,
            sub,
            tone,
        } = self;
        decay.finite()
            && punch.finite()
            && drive.finite()
            && sustain.finite()
            && gain.finite()
            && glide_ms.finite()
            && click.finite()
            && sub.finite()
            && tone.finite()
    }
}

// Current source: src/fx.rs, BitcrushFx.
impl B1Data for crate::fx::BitcrushFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            bits: a_bits,
            downsample: a_downsample,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            bits: b_bits,
            downsample: b_downsample,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_bits.exact(b_bits)
            && a_downsample.exact(b_downsample)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            bits,
            downsample,
            mix,
            id,
            bypass,
        } = self;
        bits.finite() && downsample.finite() && mix.finite() && id.finite() && bypass.finite()
    }
}

// Current source: src/project.rs, Bus.
impl B1Data for crate::project::Bus {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            id: a_id,
            name: a_name,
            effects: a_effects,
            volume_db: a_volume_db,
            pan: a_pan,
            mute: a_mute,
            output: a_output,
        } = self;
        let Self {
            id: b_id,
            name: b_name,
            effects: b_effects,
            volume_db: b_volume_db,
            pan: b_pan,
            mute: b_mute,
            output: b_output,
        } = other;
        a_id.exact(b_id)
            && a_name.exact(b_name)
            && a_effects.exact(b_effects)
            && a_volume_db.exact(b_volume_db)
            && a_pan.exact(b_pan)
            && a_mute.exact(b_mute)
            && a_output.exact(b_output)
    }
    fn finite(&self) -> bool {
        let Self {
            id,
            name,
            effects,
            volume_db,
            pan,
            mute,
            output,
        } = self;
        id.finite()
            && name.finite()
            && effects.finite()
            && volume_db.finite()
            && pan.finite()
            && mute.finite()
            && output.finite()
    }
}

// Current source: src/fx.rs, CabIrFx.
impl B1Data for crate::fx::CabIrFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            path: a_path,
            mix: a_mix,
            gain_db: a_gain_db,
            max_ms: a_max_ms,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            path: b_path,
            mix: b_mix,
            gain_db: b_gain_db,
            max_ms: b_max_ms,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_path.exact(b_path)
            && a_mix.exact(b_mix)
            && a_gain_db.exact(b_gain_db)
            && a_max_ms.exact(b_max_ms)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            path,
            mix,
            gain_db,
            max_ms,
            id,
            bypass,
        } = self;
        path.finite()
            && mix.finite()
            && gain_db.finite()
            && max_ms.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/fx.rs, ChorusFx.
impl B1Data for crate::fx::ChorusFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            rate_hz: a_rate_hz,
            depth_ms: a_depth_ms,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            rate_hz: b_rate_hz,
            depth_ms: b_depth_ms,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_rate_hz.exact(b_rate_hz)
            && a_depth_ms.exact(b_depth_ms)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            rate_hz,
            depth_ms,
            mix,
            id,
            bypass,
        } = self;
        rate_hz.finite() && depth_ms.finite() && mix.finite() && id.finite() && bypass.finite()
    }
}

// Current source: src/fx.rs, CompressorFx.
impl B1Data for crate::fx::CompressorFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            threshold_db: a_threshold_db,
            ratio: a_ratio,
            attack_ms: a_attack_ms,
            release_ms: a_release_ms,
            makeup_db: a_makeup_db,
            knee_db: a_knee_db,
            lookahead_ms: a_lookahead_ms,
            sc_hpf_hz: a_sc_hpf_hz,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            threshold_db: b_threshold_db,
            ratio: b_ratio,
            attack_ms: b_attack_ms,
            release_ms: b_release_ms,
            makeup_db: b_makeup_db,
            knee_db: b_knee_db,
            lookahead_ms: b_lookahead_ms,
            sc_hpf_hz: b_sc_hpf_hz,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_threshold_db.exact(b_threshold_db)
            && a_ratio.exact(b_ratio)
            && a_attack_ms.exact(b_attack_ms)
            && a_release_ms.exact(b_release_ms)
            && a_makeup_db.exact(b_makeup_db)
            && a_knee_db.exact(b_knee_db)
            && a_lookahead_ms.exact(b_lookahead_ms)
            && a_sc_hpf_hz.exact(b_sc_hpf_hz)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            makeup_db,
            knee_db,
            lookahead_ms,
            sc_hpf_hz,
            mix,
            id,
            bypass,
        } = self;
        threshold_db.finite()
            && ratio.finite()
            && attack_ms.finite()
            && release_ms.finite()
            && makeup_db.finite()
            && knee_db.finite()
            && lookahead_ms.finite()
            && sc_hpf_hz.finite()
            && mix.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/fx.rs, ConvolutionFx.
impl B1Data for crate::fx::ConvolutionFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            space: a_space,
            decay_s: a_decay_s,
            predelay_ms: a_predelay_ms,
            damping: a_damping,
            early: a_early,
            width: a_width,
            mix: a_mix,
            ir_path: a_ir_path,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            space: b_space,
            decay_s: b_decay_s,
            predelay_ms: b_predelay_ms,
            damping: b_damping,
            early: b_early,
            width: b_width,
            mix: b_mix,
            ir_path: b_ir_path,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_space.exact(b_space)
            && a_decay_s.exact(b_decay_s)
            && a_predelay_ms.exact(b_predelay_ms)
            && a_damping.exact(b_damping)
            && a_early.exact(b_early)
            && a_width.exact(b_width)
            && a_mix.exact(b_mix)
            && a_ir_path.exact(b_ir_path)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            space,
            decay_s,
            predelay_ms,
            damping,
            early,
            width,
            mix,
            ir_path,
            id,
            bypass,
        } = self;
        space.finite()
            && decay_s.finite()
            && predelay_ms.finite()
            && damping.finite()
            && early.finite()
            && width.finite()
            && mix.finite()
            && ir_path.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/automation.rs, Curve.
impl B1Data for crate::automation::Curve {
    fn exact(&self, other: &Self) -> bool {
        match self {
            Self::Linear => matches!(other, Self::Linear),
            Self::Step => matches!(other, Self::Step),
            Self::Smooth => matches!(other, Self::Smooth),
        }
    }
    fn finite(&self) -> bool {
        match self {
            Self::Linear => true,
            Self::Step => true,
            Self::Smooth => true,
        }
    }
}

// Current source: src/fx.rs, DeesserFx.
impl B1Data for crate::fx::DeesserFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            freq: a_freq,
            threshold_db: a_threshold_db,
            range_db: a_range_db,
            release_ms: a_release_ms,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            freq: b_freq,
            threshold_db: b_threshold_db,
            range_db: b_range_db,
            release_ms: b_release_ms,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_freq.exact(b_freq)
            && a_threshold_db.exact(b_threshold_db)
            && a_range_db.exact(b_range_db)
            && a_release_ms.exact(b_release_ms)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            freq,
            threshold_db,
            range_db,
            release_ms,
            id,
            bypass,
        } = self;
        freq.finite()
            && threshold_db.finite()
            && range_db.finite()
            && release_ms.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/fx.rs, DelayFx.
impl B1Data for crate::fx::DelayFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            wet_only: a_wet_only,
            steps: a_steps,
            feedback: a_feedback,
            mix: a_mix,
            ping_pong: a_ping_pong,
            tone: a_tone,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            wet_only: b_wet_only,
            steps: b_steps,
            feedback: b_feedback,
            mix: b_mix,
            ping_pong: b_ping_pong,
            tone: b_tone,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_wet_only.exact(b_wet_only)
            && a_steps.exact(b_steps)
            && a_feedback.exact(b_feedback)
            && a_mix.exact(b_mix)
            && a_ping_pong.exact(b_ping_pong)
            && a_tone.exact(b_tone)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            wet_only,
            steps,
            feedback,
            mix,
            ping_pong,
            tone,
            id,
            bypass,
        } = self;
        wet_only.finite()
            && steps.finite()
            && feedback.finite()
            && mix.finite()
            && ping_pong.finite()
            && tone.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/fx.rs, DistortionFx.
impl B1Data for crate::fx::DistortionFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            drive: a_drive,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            drive: b_drive,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_drive.exact(b_drive) && a_mix.exact(b_mix) && a_id.exact(b_id) && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            drive,
            mix,
            id,
            bypass,
        } = self;
        drive.finite() && mix.finite() && id.finite() && bypass.finite()
    }
}

// Current source: src/instruments.rs, DrumKind.
impl B1Data for crate::instruments::DrumKind {
    fn exact(&self, other: &Self) -> bool {
        match self {
            Self::Kick => matches!(other, Self::Kick),
            Self::Snare => matches!(other, Self::Snare),
            Self::Clap => matches!(other, Self::Clap),
            Self::ClosedHat => matches!(other, Self::ClosedHat),
            Self::OpenHat => matches!(other, Self::OpenHat),
            Self::Rim => matches!(other, Self::Rim),
            Self::Tom => matches!(other, Self::Tom),
            Self::Cowbell => matches!(other, Self::Cowbell),
            Self::Shaker => matches!(other, Self::Shaker),
            Self::Crash => matches!(other, Self::Crash),
            Self::Tabla => matches!(other, Self::Tabla),
            Self::Bayan => matches!(other, Self::Bayan),
        }
    }
    fn finite(&self) -> bool {
        match self {
            Self::Kick => true,
            Self::Snare => true,
            Self::Clap => true,
            Self::ClosedHat => true,
            Self::OpenHat => true,
            Self::Rim => true,
            Self::Tom => true,
            Self::Cowbell => true,
            Self::Shaker => true,
            Self::Crash => true,
            Self::Tabla => true,
            Self::Bayan => true,
        }
    }
}

// Current source: src/instruments.rs, DrumParams.
impl B1Data for crate::instruments::DrumParams {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            kind: a_kind,
            tune: a_tune,
            decay: a_decay,
            drive: a_drive,
            tone: a_tone,
        } = self;
        let Self {
            kind: b_kind,
            tune: b_tune,
            decay: b_decay,
            drive: b_drive,
            tone: b_tone,
        } = other;
        a_kind.exact(b_kind)
            && a_tune.exact(b_tune)
            && a_decay.exact(b_decay)
            && a_drive.exact(b_drive)
            && a_tone.exact(b_tone)
    }
    fn finite(&self) -> bool {
        let Self {
            kind,
            tune,
            decay,
            drive,
            tone,
        } = self;
        kind.finite() && tune.finite() && decay.finite() && drive.finite() && tone.finite()
    }
}

// Current source: src/fx.rs, DynamicEqFx.
impl B1Data for crate::fx::DynamicEqFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            freq: a_freq,
            q: a_q,
            threshold_db: a_threshold_db,
            ratio: a_ratio,
            range_db: a_range_db,
            attack_ms: a_attack_ms,
            release_ms: a_release_ms,
            source: a_source,
            key: a_key,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            freq: b_freq,
            q: b_q,
            threshold_db: b_threshold_db,
            ratio: b_ratio,
            range_db: b_range_db,
            attack_ms: b_attack_ms,
            release_ms: b_release_ms,
            source: b_source,
            key: b_key,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_freq.exact(b_freq)
            && a_q.exact(b_q)
            && a_threshold_db.exact(b_threshold_db)
            && a_ratio.exact(b_ratio)
            && a_range_db.exact(b_range_db)
            && a_attack_ms.exact(b_attack_ms)
            && a_release_ms.exact(b_release_ms)
            && a_source.exact(b_source)
            && a_key.exact(b_key)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            freq,
            q,
            threshold_db,
            ratio,
            range_db,
            attack_ms,
            release_ms,
            source,
            key,
            id,
            bypass,
        } = self;
        freq.finite()
            && q.finite()
            && threshold_db.finite()
            && ratio.finite()
            && range_db.finite()
            && attack_ms.finite()
            && release_ms.finite()
            && source.finite()
            && key.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/fx.rs, Effect.
impl B1Data for crate::fx::Effect {
    fn exact(&self, other: &Self) -> bool {
        match self {
            Self::Filter(a) => match other {
                Self::Filter(b) => a.exact(b),
                _ => false,
            },
            Self::Eq(a) => match other {
                Self::Eq(b) => a.exact(b),
                _ => false,
            },
            Self::Distortion(a) => match other {
                Self::Distortion(b) => a.exact(b),
                _ => false,
            },
            Self::Bitcrush(a) => match other {
                Self::Bitcrush(b) => a.exact(b),
                _ => false,
            },
            Self::Delay(a) => match other {
                Self::Delay(b) => a.exact(b),
                _ => false,
            },
            Self::Reverb(a) => match other {
                Self::Reverb(b) => a.exact(b),
                _ => false,
            },
            Self::Chorus(a) => match other {
                Self::Chorus(b) => a.exact(b),
                _ => false,
            },
            Self::Compressor(a) => match other {
                Self::Compressor(b) => a.exact(b),
                _ => false,
            },
            Self::Sidechain(a) => match other {
                Self::Sidechain(b) => a.exact(b),
                _ => false,
            },
            Self::Width(a) => match other {
                Self::Width(b) => a.exact(b),
                _ => false,
            },
            Self::Gain(a) => match other {
                Self::Gain(b) => a.exact(b),
                _ => false,
            },
            Self::Limiter(a) => match other {
                Self::Limiter(b) => a.exact(b),
                _ => false,
            },
            Self::Transient(a) => match other {
                Self::Transient(b) => a.exact(b),
                _ => false,
            },
            Self::ParametricEq(a) => match other {
                Self::ParametricEq(b) => a.exact(b),
                _ => false,
            },
            Self::Multiband(a) => match other {
                Self::Multiband(b) => a.exact(b),
                _ => false,
            },
            Self::DynamicEq(a) => match other {
                Self::DynamicEq(b) => a.exact(b),
                _ => false,
            },
            Self::Deesser(a) => match other {
                Self::Deesser(b) => a.exact(b),
                _ => false,
            },
            Self::SoftClipper(a) => match other {
                Self::SoftClipper(b) => a.exact(b),
                _ => false,
            },
            Self::Gate(a) => match other {
                Self::Gate(b) => a.exact(b),
                _ => false,
            },
            Self::Phaser(a) => match other {
                Self::Phaser(b) => a.exact(b),
                _ => false,
            },
            Self::Flanger(a) => match other {
                Self::Flanger(b) => a.exact(b),
                _ => false,
            },
            Self::Stutter(a) => match other {
                Self::Stutter(b) => a.exact(b),
                _ => false,
            },
            Self::PitchShift(a) => match other {
                Self::PitchShift(b) => a.exact(b),
                _ => false,
            },
            Self::Convolution(a) => match other {
                Self::Convolution(b) => a.exact(b),
                _ => false,
            },
            Self::Autopan(a) => match other {
                Self::Autopan(b) => a.exact(b),
                _ => false,
            },
            Self::Saturator(a) => match other {
                Self::Saturator(b) => a.exact(b),
                _ => false,
            },
            Self::Haas(a) => match other {
                Self::Haas(b) => a.exact(b),
                _ => false,
            },
            Self::GrossBeat(a) => match other {
                Self::GrossBeat(b) => a.exact(b),
                _ => false,
            },
            Self::Vocoder(a) => match other {
                Self::Vocoder(b) => a.exact(b),
                _ => false,
            },
            Self::FreqShift(a) => match other {
                Self::FreqShift(b) => a.exact(b),
                _ => false,
            },
            Self::RingMod(a) => match other {
                Self::RingMod(b) => a.exact(b),
                _ => false,
            },
            Self::StereoShaper(a) => match other {
                Self::StereoShaper(b) => a.exact(b),
                _ => false,
            },
            Self::Vinyl(a) => match other {
                Self::Vinyl(b) => a.exact(b),
                _ => false,
            },
            Self::Waveshaper(a) => match other {
                Self::Waveshaper(b) => a.exact(b),
                _ => false,
            },
            Self::AmpSim(a) => match other {
                Self::AmpSim(b) => a.exact(b),
                _ => false,
            },
            Self::CabIr(a) => match other {
                Self::CabIr(b) => a.exact(b),
                _ => false,
            },
            Self::ModDelay(a) => match other {
                Self::ModDelay(b) => a.exact(b),
                _ => false,
            },
            Self::Maximizer(a) => match other {
                Self::Maximizer(b) => a.exact(b),
                _ => false,
            },
            Self::Lofi(a) => match other {
                Self::Lofi(b) => a.exact(b),
                _ => false,
            },
            Self::Rectifier(a) => match other {
                Self::Rectifier(b) => a.exact(b),
                _ => false,
            },
            Self::FreqSplitter(a) => match other {
                Self::FreqSplitter(b) => a.exact(b),
                _ => false,
            },
            Self::Granular(a) => match other {
                Self::Granular(b) => a.exact(b),
                _ => false,
            },
        }
    }
    fn finite(&self) -> bool {
        match self {
            Self::Filter(a) => a.finite(),
            Self::Eq(a) => a.finite(),
            Self::Distortion(a) => a.finite(),
            Self::Bitcrush(a) => a.finite(),
            Self::Delay(a) => a.finite(),
            Self::Reverb(a) => a.finite(),
            Self::Chorus(a) => a.finite(),
            Self::Compressor(a) => a.finite(),
            Self::Sidechain(a) => a.finite(),
            Self::Width(a) => a.finite(),
            Self::Gain(a) => a.finite(),
            Self::Limiter(a) => a.finite(),
            Self::Transient(a) => a.finite(),
            Self::ParametricEq(a) => a.finite(),
            Self::Multiband(a) => a.finite(),
            Self::DynamicEq(a) => a.finite(),
            Self::Deesser(a) => a.finite(),
            Self::SoftClipper(a) => a.finite(),
            Self::Gate(a) => a.finite(),
            Self::Phaser(a) => a.finite(),
            Self::Flanger(a) => a.finite(),
            Self::Stutter(a) => a.finite(),
            Self::PitchShift(a) => a.finite(),
            Self::Convolution(a) => a.finite(),
            Self::Autopan(a) => a.finite(),
            Self::Saturator(a) => a.finite(),
            Self::Haas(a) => a.finite(),
            Self::GrossBeat(a) => a.finite(),
            Self::Vocoder(a) => a.finite(),
            Self::FreqShift(a) => a.finite(),
            Self::RingMod(a) => a.finite(),
            Self::StereoShaper(a) => a.finite(),
            Self::Vinyl(a) => a.finite(),
            Self::Waveshaper(a) => a.finite(),
            Self::AmpSim(a) => a.finite(),
            Self::CabIr(a) => a.finite(),
            Self::ModDelay(a) => a.finite(),
            Self::Maximizer(a) => a.finite(),
            Self::Lofi(a) => a.finite(),
            Self::Rectifier(a) => a.finite(),
            Self::FreqSplitter(a) => a.finite(),
            Self::Granular(a) => a.finite(),
        }
    }
}

// Current source: src/instruments.rs, EnsembleKind.
impl B1Data for crate::instruments::EnsembleKind {
    fn exact(&self, other: &Self) -> bool {
        match self {
            Self::Strings => matches!(other, Self::Strings),
            Self::Choir => matches!(other, Self::Choir),
            Self::Brass => matches!(other, Self::Brass),
        }
    }
    fn finite(&self) -> bool {
        match self {
            Self::Strings => true,
            Self::Choir => true,
            Self::Brass => true,
        }
    }
}

// Current source: src/instruments.rs, EnsembleParams.
impl B1Data for crate::instruments::EnsembleParams {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            kind: a_kind,
            players: a_players,
            vibrato: a_vibrato,
            detune_cents: a_detune_cents,
            attack_jitter_ms: a_attack_jitter_ms,
            brightness: a_brightness,
            vowel: a_vowel,
            noise: a_noise,
            amp_env: a_amp_env,
            stereo_spread: a_stereo_spread,
            gain: a_gain,
        } = self;
        let Self {
            kind: b_kind,
            players: b_players,
            vibrato: b_vibrato,
            detune_cents: b_detune_cents,
            attack_jitter_ms: b_attack_jitter_ms,
            brightness: b_brightness,
            vowel: b_vowel,
            noise: b_noise,
            amp_env: b_amp_env,
            stereo_spread: b_stereo_spread,
            gain: b_gain,
        } = other;
        a_kind.exact(b_kind)
            && a_players.exact(b_players)
            && a_vibrato.exact(b_vibrato)
            && a_detune_cents.exact(b_detune_cents)
            && a_attack_jitter_ms.exact(b_attack_jitter_ms)
            && a_brightness.exact(b_brightness)
            && a_vowel.exact(b_vowel)
            && a_noise.exact(b_noise)
            && a_amp_env.exact(b_amp_env)
            && a_stereo_spread.exact(b_stereo_spread)
            && a_gain.exact(b_gain)
    }
    fn finite(&self) -> bool {
        let Self {
            kind,
            players,
            vibrato,
            detune_cents,
            attack_jitter_ms,
            brightness,
            vowel,
            noise,
            amp_env,
            stereo_spread,
            gain,
        } = self;
        kind.finite()
            && players.finite()
            && vibrato.finite()
            && detune_cents.finite()
            && attack_jitter_ms.finite()
            && brightness.finite()
            && vowel.finite()
            && noise.finite()
            && amp_env.finite()
            && stereo_spread.finite()
            && gain.finite()
    }
}

// Current source: src/fx.rs, EqBand.
impl B1Data for crate::fx::EqBand {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            kind: a_kind,
            freq: a_freq,
            gain_db: a_gain_db,
            q: a_q,
            stages: a_stages,
            enabled: a_enabled,
            auto: a_auto,
        } = self;
        let Self {
            kind: b_kind,
            freq: b_freq,
            gain_db: b_gain_db,
            q: b_q,
            stages: b_stages,
            enabled: b_enabled,
            auto: b_auto,
        } = other;
        a_kind.exact(b_kind)
            && a_freq.exact(b_freq)
            && a_gain_db.exact(b_gain_db)
            && a_q.exact(b_q)
            && a_stages.exact(b_stages)
            && a_enabled.exact(b_enabled)
            && a_auto.exact(b_auto)
    }
    fn finite(&self) -> bool {
        let Self {
            kind,
            freq,
            gain_db,
            q,
            stages,
            enabled,
            auto,
        } = self;
        kind.finite()
            && freq.finite()
            && gain_db.finite()
            && q.finite()
            && stages.finite()
            && enabled.finite()
            && auto.finite()
    }
}

// Current source: src/fx.rs, EqBandKind.
impl B1Data for crate::fx::EqBandKind {
    fn exact(&self, other: &Self) -> bool {
        match self {
            Self::Bell => matches!(other, Self::Bell),
            Self::LowShelf => matches!(other, Self::LowShelf),
            Self::HighShelf => matches!(other, Self::HighShelf),
            Self::LowCut => matches!(other, Self::LowCut),
            Self::HighCut => matches!(other, Self::HighCut),
            Self::Notch => matches!(other, Self::Notch),
        }
    }
    fn finite(&self) -> bool {
        match self {
            Self::Bell => true,
            Self::LowShelf => true,
            Self::HighShelf => true,
            Self::LowCut => true,
            Self::HighCut => true,
            Self::Notch => true,
        }
    }
}

// Current source: src/fx.rs, EqFx.
impl B1Data for crate::fx::EqFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            low_db: a_low_db,
            mid_db: a_mid_db,
            high_db: a_high_db,
            low_freq: a_low_freq,
            high_freq: a_high_freq,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            low_db: b_low_db,
            mid_db: b_mid_db,
            high_db: b_high_db,
            low_freq: b_low_freq,
            high_freq: b_high_freq,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_low_db.exact(b_low_db)
            && a_mid_db.exact(b_mid_db)
            && a_high_db.exact(b_high_db)
            && a_low_freq.exact(b_low_freq)
            && a_high_freq.exact(b_high_freq)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            low_db,
            mid_db,
            high_db,
            low_freq,
            high_freq,
            id,
            bypass,
        } = self;
        low_db.finite()
            && mid_db.finite()
            && high_db.finite()
            && low_freq.finite()
            && high_freq.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/fx.rs, FilterFx.
impl B1Data for crate::fx::FilterFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            mode: a_mode,
            cutoff: a_cutoff,
            resonance: a_resonance,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            mode: b_mode,
            cutoff: b_cutoff,
            resonance: b_resonance,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_mode.exact(b_mode)
            && a_cutoff.exact(b_cutoff)
            && a_resonance.exact(b_resonance)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            mode,
            cutoff,
            resonance,
            id,
            bypass,
        } = self;
        mode.finite() && cutoff.finite() && resonance.finite() && id.finite() && bypass.finite()
    }
}

// Current source: src/dsp.rs, FilterMode.
impl B1Data for crate::dsp::FilterMode {
    fn exact(&self, other: &Self) -> bool {
        match self {
            Self::Lowpass => matches!(other, Self::Lowpass),
            Self::Highpass => matches!(other, Self::Highpass),
            Self::Bandpass => matches!(other, Self::Bandpass),
            Self::Notch => matches!(other, Self::Notch),
        }
    }
    fn finite(&self) -> bool {
        match self {
            Self::Lowpass => true,
            Self::Highpass => true,
            Self::Bandpass => true,
            Self::Notch => true,
        }
    }
}

// Current source: src/fx.rs, FlangerFx.
impl B1Data for crate::fx::FlangerFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            rate_hz: a_rate_hz,
            sync_steps: a_sync_steps,
            delay_ms: a_delay_ms,
            depth_ms: a_depth_ms,
            feedback: a_feedback,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            rate_hz: b_rate_hz,
            sync_steps: b_sync_steps,
            delay_ms: b_delay_ms,
            depth_ms: b_depth_ms,
            feedback: b_feedback,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_rate_hz.exact(b_rate_hz)
            && a_sync_steps.exact(b_sync_steps)
            && a_delay_ms.exact(b_delay_ms)
            && a_depth_ms.exact(b_depth_ms)
            && a_feedback.exact(b_feedback)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            rate_hz,
            sync_steps,
            delay_ms,
            depth_ms,
            feedback,
            mix,
            id,
            bypass,
        } = self;
        rate_hz.finite()
            && sync_steps.finite()
            && delay_ms.finite()
            && depth_ms.finite()
            && feedback.finite()
            && mix.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/instruments.rs, FmParams.
impl B1Data for crate::instruments::FmParams {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            ratio: a_ratio,
            index: a_index,
            mod_env: a_mod_env,
            amp_env: a_amp_env,
            ratio2: a_ratio2,
            index2: a_index2,
            feedback: a_feedback,
            gain: a_gain,
            detune_cents: a_detune_cents,
        } = self;
        let Self {
            ratio: b_ratio,
            index: b_index,
            mod_env: b_mod_env,
            amp_env: b_amp_env,
            ratio2: b_ratio2,
            index2: b_index2,
            feedback: b_feedback,
            gain: b_gain,
            detune_cents: b_detune_cents,
        } = other;
        a_ratio.exact(b_ratio)
            && a_index.exact(b_index)
            && a_mod_env.exact(b_mod_env)
            && a_amp_env.exact(b_amp_env)
            && a_ratio2.exact(b_ratio2)
            && a_index2.exact(b_index2)
            && a_feedback.exact(b_feedback)
            && a_gain.exact(b_gain)
            && a_detune_cents.exact(b_detune_cents)
    }
    fn finite(&self) -> bool {
        let Self {
            ratio,
            index,
            mod_env,
            amp_env,
            ratio2,
            index2,
            feedback,
            gain,
            detune_cents,
        } = self;
        ratio.finite()
            && index.finite()
            && mod_env.finite()
            && amp_env.finite()
            && ratio2.finite()
            && index2.finite()
            && feedback.finite()
            && gain.finite()
            && detune_cents.finite()
    }
}

// Current source: src/fx.rs, FreqShiftFx.
impl B1Data for crate::fx::FreqShiftFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            shift_hz: a_shift_hz,
            stereo_hz: a_stereo_hz,
            feedback: a_feedback,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            shift_hz: b_shift_hz,
            stereo_hz: b_stereo_hz,
            feedback: b_feedback,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_shift_hz.exact(b_shift_hz)
            && a_stereo_hz.exact(b_stereo_hz)
            && a_feedback.exact(b_feedback)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            shift_hz,
            stereo_hz,
            feedback,
            mix,
            id,
            bypass,
        } = self;
        shift_hz.finite()
            && stereo_hz.finite()
            && feedback.finite()
            && mix.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/fx.rs, FreqSplitBand.
impl B1Data for crate::fx::FreqSplitBand {
    fn exact(&self, other: &Self) -> bool {
        match self {
            Self::Low => matches!(other, Self::Low),
            Self::Mid => matches!(other, Self::Mid),
            Self::High => matches!(other, Self::High),
        }
    }
    fn finite(&self) -> bool {
        match self {
            Self::Low => true,
            Self::Mid => true,
            Self::High => true,
        }
    }
}

// Current source: src/fx.rs, FreqSplitterFx.
impl B1Data for crate::fx::FreqSplitterFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            low_hz: a_low_hz,
            high_hz: a_high_hz,
            taps: a_taps,
            band: a_band,
            gain_db: a_gain_db,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            low_hz: b_low_hz,
            high_hz: b_high_hz,
            taps: b_taps,
            band: b_band,
            gain_db: b_gain_db,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_low_hz.exact(b_low_hz)
            && a_high_hz.exact(b_high_hz)
            && a_taps.exact(b_taps)
            && a_band.exact(b_band)
            && a_gain_db.exact(b_gain_db)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            low_hz,
            high_hz,
            taps,
            band,
            gain_db,
            mix,
            id,
            bypass,
        } = self;
        low_hz.finite()
            && high_hz.finite()
            && taps.finite()
            && band.finite()
            && gain_db.finite()
            && mix.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/fx.rs, GainFx.
impl B1Data for crate::fx::GainFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            db: a_db,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            db: b_db,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_db.exact(b_db) && a_id.exact(b_id) && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self { db, id, bypass } = self;
        db.finite() && id.finite() && bypass.finite()
    }
}

// Current source: src/fx.rs, GateFx.
impl B1Data for crate::fx::GateFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            threshold_db: a_threshold_db,
            range_db: a_range_db,
            attack_ms: a_attack_ms,
            hold_ms: a_hold_ms,
            release_ms: a_release_ms,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            threshold_db: b_threshold_db,
            range_db: b_range_db,
            attack_ms: b_attack_ms,
            hold_ms: b_hold_ms,
            release_ms: b_release_ms,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_threshold_db.exact(b_threshold_db)
            && a_range_db.exact(b_range_db)
            && a_attack_ms.exact(b_attack_ms)
            && a_hold_ms.exact(b_hold_ms)
            && a_release_ms.exact(b_release_ms)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            threshold_db,
            range_db,
            attack_ms,
            hold_ms,
            release_ms,
            id,
            bypass,
        } = self;
        threshold_db.finite()
            && range_db.finite()
            && attack_ms.finite()
            && hold_ms.finite()
            && release_ms.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/fx.rs, GranularFx.
impl B1Data for crate::fx::GranularFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            grain_ms: a_grain_ms,
            density_hz: a_density_hz,
            pitch_semitones: a_pitch_semitones,
            position_jitter_ms: a_position_jitter_ms,
            spread: a_spread,
            freeze_at_ms: a_freeze_at_ms,
            mix: a_mix,
            seed: a_seed,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            grain_ms: b_grain_ms,
            density_hz: b_density_hz,
            pitch_semitones: b_pitch_semitones,
            position_jitter_ms: b_position_jitter_ms,
            spread: b_spread,
            freeze_at_ms: b_freeze_at_ms,
            mix: b_mix,
            seed: b_seed,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_grain_ms.exact(b_grain_ms)
            && a_density_hz.exact(b_density_hz)
            && a_pitch_semitones.exact(b_pitch_semitones)
            && a_position_jitter_ms.exact(b_position_jitter_ms)
            && a_spread.exact(b_spread)
            && a_freeze_at_ms.exact(b_freeze_at_ms)
            && a_mix.exact(b_mix)
            && a_seed.exact(b_seed)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            grain_ms,
            density_hz,
            pitch_semitones,
            position_jitter_ms,
            spread,
            freeze_at_ms,
            mix,
            seed,
            id,
            bypass,
        } = self;
        grain_ms.finite()
            && density_hz.finite()
            && pitch_semitones.finite()
            && position_jitter_ms.finite()
            && spread.finite()
            && freeze_at_ms.finite()
            && mix.finite()
            && seed.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/instruments.rs, GranularParams.
impl B1Data for crate::instruments::GranularParams {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            sample: a_sample,
            root: a_root,
            position: a_position,
            spray: a_spray,
            scan: a_scan,
            grain_ms: a_grain_ms,
            density: a_density,
            pitch_spread_cents: a_pitch_spread_cents,
            reverse_prob: a_reverse_prob,
            cutoff: a_cutoff,
            amp_env: a_amp_env,
            gain: a_gain,
        } = self;
        let Self {
            sample: b_sample,
            root: b_root,
            position: b_position,
            spray: b_spray,
            scan: b_scan,
            grain_ms: b_grain_ms,
            density: b_density,
            pitch_spread_cents: b_pitch_spread_cents,
            reverse_prob: b_reverse_prob,
            cutoff: b_cutoff,
            amp_env: b_amp_env,
            gain: b_gain,
        } = other;
        a_sample.exact(b_sample)
            && a_root.exact(b_root)
            && a_position.exact(b_position)
            && a_spray.exact(b_spray)
            && a_scan.exact(b_scan)
            && a_grain_ms.exact(b_grain_ms)
            && a_density.exact(b_density)
            && a_pitch_spread_cents.exact(b_pitch_spread_cents)
            && a_reverse_prob.exact(b_reverse_prob)
            && a_cutoff.exact(b_cutoff)
            && a_amp_env.exact(b_amp_env)
            && a_gain.exact(b_gain)
    }
    fn finite(&self) -> bool {
        let Self {
            sample,
            root,
            position,
            spray,
            scan,
            grain_ms,
            density,
            pitch_spread_cents,
            reverse_prob,
            cutoff,
            amp_env,
            gain,
        } = self;
        sample.finite()
            && root.finite()
            && position.finite()
            && spray.finite()
            && scan.finite()
            && grain_ms.finite()
            && density.finite()
            && pitch_spread_cents.finite()
            && reverse_prob.finite()
            && cutoff.finite()
            && amp_env.finite()
            && gain.finite()
    }
}

// Current source: src/fx.rs, GrossBeatFx.
impl B1Data for crate::fx::GrossBeatFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            time: a_time,
            volume: a_volume,
            time_points: a_time_points,
            volume_points: a_volume_points,
            cycle_steps: a_cycle_steps,
            smooth_ms: a_smooth_ms,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            time: b_time,
            volume: b_volume,
            time_points: b_time_points,
            volume_points: b_volume_points,
            cycle_steps: b_cycle_steps,
            smooth_ms: b_smooth_ms,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_time.exact(b_time)
            && a_volume.exact(b_volume)
            && a_time_points.exact(b_time_points)
            && a_volume_points.exact(b_volume_points)
            && a_cycle_steps.exact(b_cycle_steps)
            && a_smooth_ms.exact(b_smooth_ms)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            time,
            volume,
            time_points,
            volume_points,
            cycle_steps,
            smooth_ms,
            mix,
            id,
            bypass,
        } = self;
        time.finite()
            && volume.finite()
            && time_points.finite()
            && volume_points.finite()
            && cycle_steps.finite()
            && smooth_ms.finite()
            && mix.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/fx.rs, HaasFx.
impl B1Data for crate::fx::HaasFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            delay_ms: a_delay_ms,
            side: a_side,
            mix: a_mix,
            low_cut_hz: a_low_cut_hz,
            level_db: a_level_db,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            delay_ms: b_delay_ms,
            side: b_side,
            mix: b_mix,
            low_cut_hz: b_low_cut_hz,
            level_db: b_level_db,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_delay_ms.exact(b_delay_ms)
            && a_side.exact(b_side)
            && a_mix.exact(b_mix)
            && a_low_cut_hz.exact(b_low_cut_hz)
            && a_level_db.exact(b_level_db)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            delay_ms,
            side,
            mix,
            low_cut_hz,
            level_db,
            id,
            bypass,
        } = self;
        delay_ms.finite()
            && side.finite()
            && mix.finite()
            && low_cut_hz.finite()
            && level_db.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/instruments.rs, Instrument.
impl B1Data for crate::instruments::Instrument {
    fn exact(&self, other: &Self) -> bool {
        match self {
            Self::Drum(a) => match other {
                Self::Drum(b) => a.exact(b),
                _ => false,
            },
            Self::Synth(a) => match other {
                Self::Synth(b) => a.exact(b),
                _ => false,
            },
            Self::Fm(a) => match other {
                Self::Fm(b) => a.exact(b),
                _ => false,
            },
            Self::Pluck(a) => match other {
                Self::Pluck(b) => a.exact(b),
                _ => false,
            },
            Self::Bass808(a) => match other {
                Self::Bass808(b) => a.exact(b),
                _ => false,
            },
            Self::Sampler(a) => match other {
                Self::Sampler(b) => a.exact(b),
                _ => false,
            },
            Self::Wavetable(a) => match other {
                Self::Wavetable(b) => a.exact(b),
                _ => false,
            },
            Self::Granular(a) => match other {
                Self::Granular(b) => a.exact(b),
                _ => false,
            },
            Self::Layer(a) => match other {
                Self::Layer(b) => a.exact(b),
                _ => false,
            },
            Self::Multisample(a) => match other {
                Self::Multisample(b) => a.exact(b),
                _ => false,
            },
            Self::Piano(a) => match other {
                Self::Piano(b) => a.exact(b),
                _ => false,
            },
            Self::Ensemble(a) => match other {
                Self::Ensemble(b) => a.exact(b),
                _ => false,
            },
            Self::SoundFont(a) => match other {
                Self::SoundFont(b) => a.exact(b),
                _ => false,
            },
        }
    }
    fn finite(&self) -> bool {
        match self {
            Self::Drum(a) => a.finite(),
            Self::Synth(a) => a.finite(),
            Self::Fm(a) => a.finite(),
            Self::Pluck(a) => a.finite(),
            Self::Bass808(a) => a.finite(),
            Self::Sampler(a) => a.finite(),
            Self::Wavetable(a) => a.finite(),
            Self::Granular(a) => a.finite(),
            Self::Layer(a) => a.finite(),
            Self::Multisample(a) => a.finite(),
            Self::Piano(a) => a.finite(),
            Self::Ensemble(a) => a.finite(),
            Self::SoundFont(a) => a.finite(),
        }
    }
}

// Current source: src/fx.rs, IrSpace.
impl B1Data for crate::fx::IrSpace {
    fn exact(&self, other: &Self) -> bool {
        match self {
            Self::Room => matches!(other, Self::Room),
            Self::Hall => matches!(other, Self::Hall),
            Self::Plate => matches!(other, Self::Plate),
            Self::Chamber => matches!(other, Self::Chamber),
            Self::Spring => matches!(other, Self::Spring),
            Self::Cathedral => matches!(other, Self::Cathedral),
        }
    }
    fn finite(&self) -> bool {
        match self {
            Self::Room => true,
            Self::Hall => true,
            Self::Plate => true,
            Self::Chamber => true,
            Self::Spring => true,
            Self::Cathedral => true,
        }
    }
}

// Current source: src/multisample.rs, KeyVelocityResponse.
impl B1Data for crate::multisample::KeyVelocityResponse {
    fn exact(&self, other: &Self) -> bool {
        let Self(a) = self;
        let Self(b) = other;
        a.exact(b)
    }
    fn finite(&self) -> bool {
        let Self(a) = self;
        a.finite()
    }
}

// Current source: src/instruments.rs, Layer.
impl B1Data for crate::instruments::Layer {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            instrument: a_instrument,
            gain_db: a_gain_db,
            transpose: a_transpose,
            key_min: a_key_min,
            key_max: a_key_max,
            vel_min: a_vel_min,
            vel_max: a_vel_max,
            delay_ms: a_delay_ms,
        } = self;
        let Self {
            instrument: b_instrument,
            gain_db: b_gain_db,
            transpose: b_transpose,
            key_min: b_key_min,
            key_max: b_key_max,
            vel_min: b_vel_min,
            vel_max: b_vel_max,
            delay_ms: b_delay_ms,
        } = other;
        a_instrument.exact(b_instrument)
            && a_gain_db.exact(b_gain_db)
            && a_transpose.exact(b_transpose)
            && a_key_min.exact(b_key_min)
            && a_key_max.exact(b_key_max)
            && a_vel_min.exact(b_vel_min)
            && a_vel_max.exact(b_vel_max)
            && a_delay_ms.exact(b_delay_ms)
    }
    fn finite(&self) -> bool {
        let Self {
            instrument,
            gain_db,
            transpose,
            key_min,
            key_max,
            vel_min,
            vel_max,
            delay_ms,
        } = self;
        instrument.finite()
            && gain_db.finite()
            && transpose.finite()
            && key_min.finite()
            && key_max.finite()
            && vel_min.finite()
            && vel_max.finite()
            && delay_ms.finite()
    }
}

// Current source: src/instruments.rs, LayerParams.
impl B1Data for crate::instruments::LayerParams {
    fn exact(&self, other: &Self) -> bool {
        let Self { layers: a_layers } = self;
        let Self { layers: b_layers } = other;
        a_layers.exact(b_layers)
    }
    fn finite(&self) -> bool {
        let Self { layers } = self;
        layers.finite()
    }
}

// Current source: src/fx.rs, LimiterFx.
impl B1Data for crate::fx::LimiterFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            ceiling_db: a_ceiling_db,
            release_ms: a_release_ms,
            true_peak: a_true_peak,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            ceiling_db: b_ceiling_db,
            release_ms: b_release_ms,
            true_peak: b_true_peak,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_ceiling_db.exact(b_ceiling_db)
            && a_release_ms.exact(b_release_ms)
            && a_true_peak.exact(b_true_peak)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            ceiling_db,
            release_ms,
            true_peak,
            id,
            bypass,
        } = self;
        ceiling_db.finite()
            && release_ms.finite()
            && true_peak.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/fx.rs, LoFiFx.
impl B1Data for crate::fx::LoFiFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            bits: a_bits,
            sample_rate_hz: a_sample_rate_hz,
            noise: a_noise,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            bits: b_bits,
            sample_rate_hz: b_sample_rate_hz,
            noise: b_noise,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_bits.exact(b_bits)
            && a_sample_rate_hz.exact(b_sample_rate_hz)
            && a_noise.exact(b_noise)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            bits,
            sample_rate_hz,
            noise,
            mix,
            id,
            bypass,
        } = self;
        bits.finite()
            && sample_rate_hz.finite()
            && noise.finite()
            && mix.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/project.rs, Macro.
impl B1Data for crate::project::Macro {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            name: a_name,
            value: a_value,
            targets: a_targets,
        } = self;
        let Self {
            name: b_name,
            value: b_value,
            targets: b_targets,
        } = other;
        a_name.exact(b_name) && a_value.exact(b_value) && a_targets.exact(b_targets)
    }
    fn finite(&self) -> bool {
        let Self {
            name,
            value,
            targets,
        } = self;
        name.finite() && value.finite() && targets.finite()
    }
}

// Current source: src/project.rs, MacroTarget.
impl B1Data for crate::project::MacroTarget {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            track: a_track,
            param: a_param,
            min: a_min,
            max: a_max,
            curve: a_curve,
        } = self;
        let Self {
            track: b_track,
            param: b_param,
            min: b_min,
            max: b_max,
            curve: b_curve,
        } = other;
        a_track.exact(b_track)
            && a_param.exact(b_param)
            && a_min.exact(b_min)
            && a_max.exact(b_max)
            && a_curve.exact(b_curve)
    }
    fn finite(&self) -> bool {
        let Self {
            track,
            param,
            min,
            max,
            curve,
        } = self;
        track.finite() && param.finite() && min.finite() && max.finite() && curve.finite()
    }
}

// Current source: src/project.rs, Marker.
impl B1Data for crate::project::Marker {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            name: a_name,
            beat: a_beat,
            time_signature: a_time_signature,
        } = self;
        let Self {
            name: b_name,
            beat: b_beat,
            time_signature: b_time_signature,
        } = other;
        a_name.exact(b_name) && a_beat.exact(b_beat) && a_time_signature.exact(b_time_signature)
    }
    fn finite(&self) -> bool {
        let Self {
            name,
            beat,
            time_signature,
        } = self;
        name.finite() && beat.finite() && time_signature.finite()
    }
}

// Current source: src/fx.rs, MaximizerFx.
impl B1Data for crate::fx::MaximizerFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            threshold_db: a_threshold_db,
            ceiling_db: a_ceiling_db,
            release_ms: a_release_ms,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            threshold_db: b_threshold_db,
            ceiling_db: b_ceiling_db,
            release_ms: b_release_ms,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_threshold_db.exact(b_threshold_db)
            && a_ceiling_db.exact(b_ceiling_db)
            && a_release_ms.exact(b_release_ms)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            threshold_db,
            ceiling_db,
            release_ms,
            id,
            bypass,
        } = self;
        threshold_db.finite()
            && ceiling_db.finite()
            && release_ms.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/project.rs, MidiLinkSave.
impl B1Data for crate::project::MidiLinkSave {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            control: a_control,
            target: a_target,
        } = self;
        let Self {
            control: b_control,
            target: b_target,
        } = other;
        a_control.exact(b_control) && a_target.exact(b_target)
    }
    fn finite(&self) -> bool {
        let Self { control, target } = self;
        control.finite() && target.finite()
    }
}

// Current source: src/project.rs, MidiSetup.
impl B1Data for crate::project::MidiSetup {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            links: a_links,
            track: a_track,
            quantize: a_quantize,
            gate_s: a_gate_s,
        } = self;
        let Self {
            links: b_links,
            track: b_track,
            quantize: b_quantize,
            gate_s: b_gate_s,
        } = other;
        a_links.exact(b_links)
            && a_track.exact(b_track)
            && a_quantize.exact(b_quantize)
            && a_gate_s.exact(b_gate_s)
    }
    fn finite(&self) -> bool {
        let Self {
            links,
            track,
            quantize,
            gate_s,
        } = self;
        links.finite() && track.finite() && quantize.finite() && gate_s.finite()
    }
}

// Current source: src/fx.rs, ModDelayFx.
impl B1Data for crate::fx::ModDelayFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            time_ms: a_time_ms,
            feedback: a_feedback,
            mix: a_mix,
            mod_rate_hz: a_mod_rate_hz,
            mod_depth: a_mod_depth,
            lpf_hz: a_lpf_hz,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            time_ms: b_time_ms,
            feedback: b_feedback,
            mix: b_mix,
            mod_rate_hz: b_mod_rate_hz,
            mod_depth: b_mod_depth,
            lpf_hz: b_lpf_hz,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_time_ms.exact(b_time_ms)
            && a_feedback.exact(b_feedback)
            && a_mix.exact(b_mix)
            && a_mod_rate_hz.exact(b_mod_rate_hz)
            && a_mod_depth.exact(b_mod_depth)
            && a_lpf_hz.exact(b_lpf_hz)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            time_ms,
            feedback,
            mix,
            mod_rate_hz,
            mod_depth,
            lpf_hz,
            id,
            bypass,
        } = self;
        time_ms.finite()
            && feedback.finite()
            && mix.finite()
            && mod_rate_hz.finite()
            && mod_depth.finite()
            && lpf_hz.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/fx.rs, MultibandFx.
impl B1Data for crate::fx::MultibandFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            low_freq: a_low_freq,
            high_freq: a_high_freq,
            low_threshold_db: a_low_threshold_db,
            mid_threshold_db: a_mid_threshold_db,
            high_threshold_db: a_high_threshold_db,
            low_ratio: a_low_ratio,
            mid_ratio: a_mid_ratio,
            high_ratio: a_high_ratio,
            attack_ms: a_attack_ms,
            release_ms: a_release_ms,
            low_gain_db: a_low_gain_db,
            mid_gain_db: a_mid_gain_db,
            high_gain_db: a_high_gain_db,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            low_freq: b_low_freq,
            high_freq: b_high_freq,
            low_threshold_db: b_low_threshold_db,
            mid_threshold_db: b_mid_threshold_db,
            high_threshold_db: b_high_threshold_db,
            low_ratio: b_low_ratio,
            mid_ratio: b_mid_ratio,
            high_ratio: b_high_ratio,
            attack_ms: b_attack_ms,
            release_ms: b_release_ms,
            low_gain_db: b_low_gain_db,
            mid_gain_db: b_mid_gain_db,
            high_gain_db: b_high_gain_db,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_low_freq.exact(b_low_freq)
            && a_high_freq.exact(b_high_freq)
            && a_low_threshold_db.exact(b_low_threshold_db)
            && a_mid_threshold_db.exact(b_mid_threshold_db)
            && a_high_threshold_db.exact(b_high_threshold_db)
            && a_low_ratio.exact(b_low_ratio)
            && a_mid_ratio.exact(b_mid_ratio)
            && a_high_ratio.exact(b_high_ratio)
            && a_attack_ms.exact(b_attack_ms)
            && a_release_ms.exact(b_release_ms)
            && a_low_gain_db.exact(b_low_gain_db)
            && a_mid_gain_db.exact(b_mid_gain_db)
            && a_high_gain_db.exact(b_high_gain_db)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            low_freq,
            high_freq,
            low_threshold_db,
            mid_threshold_db,
            high_threshold_db,
            low_ratio,
            mid_ratio,
            high_ratio,
            attack_ms,
            release_ms,
            low_gain_db,
            mid_gain_db,
            high_gain_db,
            mix,
            id,
            bypass,
        } = self;
        low_freq.finite()
            && high_freq.finite()
            && low_threshold_db.finite()
            && mid_threshold_db.finite()
            && high_threshold_db.finite()
            && low_ratio.finite()
            && mid_ratio.finite()
            && high_ratio.finite()
            && attack_ms.finite()
            && release_ms.finite()
            && low_gain_db.finite()
            && mid_gain_db.finite()
            && high_gain_db.finite()
            && mix.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/multisample.rs, MultisampleParams.
impl B1Data for crate::multisample::MultisampleParams {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            zones: a_zones,
            attack: a_attack,
            release: a_release,
            one_shot: a_one_shot,
            round_robin: a_round_robin,
            gm_hat_choke: a_gm_hat_choke,
            vel_crossfade: a_vel_crossfade,
            vel_to_gain: a_vel_to_gain,
            velocity_curve: a_velocity_curve,
            tune_cents: a_tune_cents,
            gain: a_gain,
            key_trim_db: a_key_trim_db,
            key_tune_cents: a_key_tune_cents,
            key_length_ms: a_key_length_ms,
            key_pan: a_key_pan,
            key_attack_ms: a_key_attack_ms,
            key_decay_ms: a_key_decay_ms,
            key_lowpass_hz: a_key_lowpass_hz,
            key_highpass_hz: a_key_highpass_hz,
            key_velocity_curve: a_key_velocity_curve,
            reverse_keys: a_reverse_keys,
            source: a_source,
        } = self;
        let Self {
            zones: b_zones,
            attack: b_attack,
            release: b_release,
            one_shot: b_one_shot,
            round_robin: b_round_robin,
            gm_hat_choke: b_gm_hat_choke,
            vel_crossfade: b_vel_crossfade,
            vel_to_gain: b_vel_to_gain,
            velocity_curve: b_velocity_curve,
            tune_cents: b_tune_cents,
            gain: b_gain,
            key_trim_db: b_key_trim_db,
            key_tune_cents: b_key_tune_cents,
            key_length_ms: b_key_length_ms,
            key_pan: b_key_pan,
            key_attack_ms: b_key_attack_ms,
            key_decay_ms: b_key_decay_ms,
            key_lowpass_hz: b_key_lowpass_hz,
            key_highpass_hz: b_key_highpass_hz,
            key_velocity_curve: b_key_velocity_curve,
            reverse_keys: b_reverse_keys,
            source: b_source,
        } = other;
        a_zones.exact(b_zones)
            && a_attack.exact(b_attack)
            && a_release.exact(b_release)
            && a_one_shot.exact(b_one_shot)
            && a_round_robin.exact(b_round_robin)
            && a_gm_hat_choke.exact(b_gm_hat_choke)
            && a_vel_crossfade.exact(b_vel_crossfade)
            && a_vel_to_gain.exact(b_vel_to_gain)
            && a_velocity_curve.exact(b_velocity_curve)
            && a_tune_cents.exact(b_tune_cents)
            && a_gain.exact(b_gain)
            && a_key_trim_db.exact(b_key_trim_db)
            && a_key_tune_cents.exact(b_key_tune_cents)
            && a_key_length_ms.exact(b_key_length_ms)
            && a_key_pan.exact(b_key_pan)
            && a_key_attack_ms.exact(b_key_attack_ms)
            && a_key_decay_ms.exact(b_key_decay_ms)
            && a_key_lowpass_hz.exact(b_key_lowpass_hz)
            && a_key_highpass_hz.exact(b_key_highpass_hz)
            && a_key_velocity_curve.exact(b_key_velocity_curve)
            && a_reverse_keys.exact(b_reverse_keys)
            && a_source.exact(b_source)
    }
    fn finite(&self) -> bool {
        let Self {
            zones,
            attack,
            release,
            one_shot,
            round_robin,
            gm_hat_choke,
            vel_crossfade,
            vel_to_gain,
            velocity_curve,
            tune_cents,
            gain,
            key_trim_db,
            key_tune_cents,
            key_length_ms,
            key_pan,
            key_attack_ms,
            key_decay_ms,
            key_lowpass_hz,
            key_highpass_hz,
            key_velocity_curve,
            reverse_keys,
            source,
        } = self;
        zones.finite()
            && attack.finite()
            && release.finite()
            && one_shot.finite()
            && round_robin.finite()
            && gm_hat_choke.finite()
            && vel_crossfade.finite()
            && vel_to_gain.finite()
            && velocity_curve.finite()
            && tune_cents.finite()
            && gain.finite()
            && key_trim_db.finite()
            && key_tune_cents.finite()
            && key_length_ms.finite()
            && key_pan.finite()
            && key_attack_ms.finite()
            && key_decay_ms.finite()
            && key_lowpass_hz.finite()
            && key_highpass_hz.finite()
            && key_velocity_curve.finite()
            && reverse_keys.finite()
            && source.finite()
    }
}

// Current source: src/project.rs, Note.
impl B1Data for crate::project::Note {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            start: a_start,
            len: a_len,
            pitch: a_pitch,
            vel: a_vel,
            prob: a_prob,
            offset: a_offset,
            slide_to: a_slide_to,
            id: a_id,
        } = self;
        let Self {
            start: b_start,
            len: b_len,
            pitch: b_pitch,
            vel: b_vel,
            prob: b_prob,
            offset: b_offset,
            slide_to: b_slide_to,
            id: b_id,
        } = other;
        a_start.exact(b_start)
            && a_len.exact(b_len)
            && a_pitch.exact(b_pitch)
            && a_vel.exact(b_vel)
            && a_prob.exact(b_prob)
            && a_offset.exact(b_offset)
            && a_slide_to.exact(b_slide_to)
            && a_id.exact(b_id)
    }
    fn finite(&self) -> bool {
        let Self {
            start,
            len,
            pitch,
            vel,
            prob,
            offset,
            slide_to,
            id,
        } = self;
        start.finite()
            && len.finite()
            && pitch.finite()
            && vel.finite()
            && prob.finite()
            && offset.finite()
            && slide_to.finite()
            && id.finite()
    }
}

// Current source: src/fx.rs, ParametricEqFx.
impl B1Data for crate::fx::ParametricEqFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            bands: a_bands,
            output_db: a_output_db,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            bands: b_bands,
            output_db: b_output_db,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_bands.exact(b_bands)
            && a_output_db.exact(b_output_db)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            bands,
            output_db,
            id,
            bypass,
        } = self;
        bands.finite() && output_db.finite() && id.finite() && bypass.finite()
    }
}

// Current source: src/project.rs, Pattern.
impl B1Data for crate::project::Pattern {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            id: a_id,
            name: a_name,
            bars: a_bars,
            clips: a_clips,
            meter: a_meter,
        } = self;
        let Self {
            id: b_id,
            name: b_name,
            bars: b_bars,
            clips: b_clips,
            meter: b_meter,
        } = other;
        a_id.exact(b_id)
            && a_name.exact(b_name)
            && a_bars.exact(b_bars)
            && a_clips.exact(b_clips)
            && a_meter.exact(b_meter)
    }
    fn finite(&self) -> bool {
        let Self {
            id,
            name,
            bars,
            clips,
            meter,
        } = self;
        id.finite() && name.finite() && bars.finite() && clips.finite() && meter.finite()
    }
}

// Current source: src/fx.rs, PhaserFx.
impl B1Data for crate::fx::PhaserFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            rate_hz: a_rate_hz,
            sync_steps: a_sync_steps,
            depth: a_depth,
            center_hz: a_center_hz,
            feedback: a_feedback,
            stages: a_stages,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            rate_hz: b_rate_hz,
            sync_steps: b_sync_steps,
            depth: b_depth,
            center_hz: b_center_hz,
            feedback: b_feedback,
            stages: b_stages,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_rate_hz.exact(b_rate_hz)
            && a_sync_steps.exact(b_sync_steps)
            && a_depth.exact(b_depth)
            && a_center_hz.exact(b_center_hz)
            && a_feedback.exact(b_feedback)
            && a_stages.exact(b_stages)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            rate_hz,
            sync_steps,
            depth,
            center_hz,
            feedback,
            stages,
            mix,
            id,
            bypass,
        } = self;
        rate_hz.finite()
            && sync_steps.finite()
            && depth.finite()
            && center_hz.finite()
            && feedback.finite()
            && stages.finite()
            && mix.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/instruments.rs, PianoParams.
impl B1Data for crate::instruments::PianoParams {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            brightness: a_brightness,
            felt: a_felt,
            hammer: a_hammer,
            decay: a_decay,
            release: a_release,
            detune_cents: a_detune_cents,
            body: a_body,
            sustain_pedal: a_sustain_pedal,
            gain: a_gain,
        } = self;
        let Self {
            brightness: b_brightness,
            felt: b_felt,
            hammer: b_hammer,
            decay: b_decay,
            release: b_release,
            detune_cents: b_detune_cents,
            body: b_body,
            sustain_pedal: b_sustain_pedal,
            gain: b_gain,
        } = other;
        a_brightness.exact(b_brightness)
            && a_felt.exact(b_felt)
            && a_hammer.exact(b_hammer)
            && a_decay.exact(b_decay)
            && a_release.exact(b_release)
            && a_detune_cents.exact(b_detune_cents)
            && a_body.exact(b_body)
            && a_sustain_pedal.exact(b_sustain_pedal)
            && a_gain.exact(b_gain)
    }
    fn finite(&self) -> bool {
        let Self {
            brightness,
            felt,
            hammer,
            decay,
            release,
            detune_cents,
            body,
            sustain_pedal,
            gain,
        } = self;
        brightness.finite()
            && felt.finite()
            && hammer.finite()
            && decay.finite()
            && release.finite()
            && detune_cents.finite()
            && body.finite()
            && sustain_pedal.finite()
            && gain.finite()
    }
}

// Current source: src/fx.rs, PitchShiftFx.
impl B1Data for crate::fx::PitchShiftFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            semitones: a_semitones,
            cents: a_cents,
            window_ms: a_window_ms,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            semitones: b_semitones,
            cents: b_cents,
            window_ms: b_window_ms,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_semitones.exact(b_semitones)
            && a_cents.exact(b_cents)
            && a_window_ms.exact(b_window_ms)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            semitones,
            cents,
            window_ms,
            mix,
            id,
            bypass,
        } = self;
        semitones.finite()
            && cents.finite()
            && window_ms.finite()
            && mix.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/project.rs, PlaylistClip.
impl B1Data for crate::project::PlaylistClip {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            id: a_id,
            pattern: a_pattern,
            start_bar: a_start_bar,
            bars: a_bars,
            tracks: a_tracks,
            lane: a_lane,
            offset_bars: a_offset_bars,
        } = self;
        let Self {
            id: b_id,
            pattern: b_pattern,
            start_bar: b_start_bar,
            bars: b_bars,
            tracks: b_tracks,
            lane: b_lane,
            offset_bars: b_offset_bars,
        } = other;
        a_id.exact(b_id)
            && a_pattern.exact(b_pattern)
            && a_start_bar.exact(b_start_bar)
            && a_bars.exact(b_bars)
            && a_tracks.exact(b_tracks)
            && a_lane.exact(b_lane)
            && a_offset_bars.exact(b_offset_bars)
    }
    fn finite(&self) -> bool {
        let Self {
            id,
            pattern,
            start_bar,
            bars,
            tracks,
            lane,
            offset_bars,
        } = self;
        id.finite()
            && pattern.finite()
            && start_bar.finite()
            && bars.finite()
            && tracks.finite()
            && lane.finite()
            && offset_bars.finite()
    }
}

// Current source: src/instruments.rs, PluckParams.
impl B1Data for crate::instruments::PluckParams {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            damping: a_damping,
            brightness: a_brightness,
            gain: a_gain,
            buzz: a_buzz,
            kind: a_kind,
        } = self;
        let Self {
            damping: b_damping,
            brightness: b_brightness,
            gain: b_gain,
            buzz: b_buzz,
            kind: b_kind,
        } = other;
        a_damping.exact(b_damping)
            && a_brightness.exact(b_brightness)
            && a_gain.exact(b_gain)
            && a_buzz.exact(b_buzz)
            && a_kind.exact(b_kind)
    }
    fn finite(&self) -> bool {
        let Self {
            damping,
            brightness,
            gain,
            buzz,
            kind,
        } = self;
        damping.finite() && brightness.finite() && gain.finite() && buzz.finite() && kind.finite()
    }
}

// Current source: src/project.rs, Project.
impl B1Data for crate::project::Project {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            name: a_name,
            bpm: a_bpm,
            swing: a_swing,
            key_root: a_key_root,
            scale: a_scale,
            tracks: a_tracks,
            patterns: a_patterns,
            arrangement: a_arrangement,
            master_effects: a_master_effects,
            master_volume_db: a_master_volume_db,
            samples: a_samples,
            buses: a_buses,
            automation: a_automation,
            macros: a_macros,
            playlist: a_playlist,
            automation_clips: a_automation_clips,
            audio_clips: a_audio_clips,
            vocal_map: a_vocal_map,
            fx_chains: a_fx_chains,
            markers: a_markers,
            tempo_points: a_tempo_points,
            schema_version: a_schema_version,
            id_seq: a_id_seq,
            midi: a_midi,
            song: a_song,
            seed_log: a_seed_log,
            tuning_cents: a_tuning_cents,
        } = self;
        let Self {
            name: b_name,
            bpm: b_bpm,
            swing: b_swing,
            key_root: b_key_root,
            scale: b_scale,
            tracks: b_tracks,
            patterns: b_patterns,
            arrangement: b_arrangement,
            master_effects: b_master_effects,
            master_volume_db: b_master_volume_db,
            samples: b_samples,
            buses: b_buses,
            automation: b_automation,
            macros: b_macros,
            playlist: b_playlist,
            automation_clips: b_automation_clips,
            audio_clips: b_audio_clips,
            vocal_map: b_vocal_map,
            fx_chains: b_fx_chains,
            markers: b_markers,
            tempo_points: b_tempo_points,
            schema_version: b_schema_version,
            id_seq: b_id_seq,
            midi: b_midi,
            song: b_song,
            seed_log: b_seed_log,
            tuning_cents: b_tuning_cents,
        } = other;
        a_name.exact(b_name)
            && a_bpm.exact(b_bpm)
            && a_swing.exact(b_swing)
            && a_key_root.exact(b_key_root)
            && a_scale.exact(b_scale)
            && a_tracks.exact(b_tracks)
            && a_patterns.exact(b_patterns)
            && a_arrangement.exact(b_arrangement)
            && a_master_effects.exact(b_master_effects)
            && a_master_volume_db.exact(b_master_volume_db)
            && a_samples.exact(b_samples)
            && a_buses.exact(b_buses)
            && a_automation.exact(b_automation)
            && a_macros.exact(b_macros)
            && a_playlist.exact(b_playlist)
            && a_automation_clips.exact(b_automation_clips)
            && a_audio_clips.exact(b_audio_clips)
            && a_vocal_map.exact(b_vocal_map)
            && a_fx_chains.exact(b_fx_chains)
            && a_markers.exact(b_markers)
            && a_tempo_points.exact(b_tempo_points)
            && a_schema_version.exact(b_schema_version)
            && a_id_seq.exact(b_id_seq)
            && a_midi.exact(b_midi)
            && a_song.exact(b_song)
            && a_seed_log.exact(b_seed_log)
            && a_tuning_cents.exact(b_tuning_cents)
    }
    fn finite(&self) -> bool {
        let Self {
            name,
            bpm,
            swing,
            key_root,
            scale,
            tracks,
            patterns,
            arrangement,
            master_effects,
            master_volume_db,
            samples,
            buses,
            automation,
            macros,
            playlist,
            automation_clips,
            audio_clips,
            vocal_map,
            fx_chains,
            markers,
            tempo_points,
            schema_version,
            id_seq,
            midi,
            song,
            seed_log,
            tuning_cents,
        } = self;
        name.finite()
            && bpm.finite()
            && swing.finite()
            && key_root.finite()
            && scale.finite()
            && tracks.finite()
            && patterns.finite()
            && arrangement.finite()
            && master_effects.finite()
            && master_volume_db.finite()
            && samples.finite()
            && buses.finite()
            && automation.finite()
            && macros.finite()
            && playlist.finite()
            && automation_clips.finite()
            && audio_clips.finite()
            && vocal_map.finite()
            && fx_chains.finite()
            && markers.finite()
            && tempo_points.finite()
            && schema_version.finite()
            && id_seq.finite()
            && midi.finite()
            && song.finite()
            && seed_log.finite()
            && tuning_cents.finite()
    }
}

// Current source: src/fx.rs, RectifierFx.
impl B1Data for crate::fx::RectifierFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            mode: a_mode,
            mix: a_mix,
            output_db: a_output_db,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            mode: b_mode,
            mix: b_mix,
            output_db: b_output_db,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_mode.exact(b_mode)
            && a_mix.exact(b_mix)
            && a_output_db.exact(b_output_db)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            mode,
            mix,
            output_db,
            id,
            bypass,
        } = self;
        mode.finite() && mix.finite() && output_db.finite() && id.finite() && bypass.finite()
    }
}

// Current source: src/fx.rs, ReverbFx.
impl B1Data for crate::fx::ReverbFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            size: a_size,
            damping: a_damping,
            mix: a_mix,
            predelay_ms: a_predelay_ms,
            width: a_width,
            mode: a_mode,
            decay_s: a_decay_s,
            diffusion: a_diffusion,
            low_cut_hz: a_low_cut_hz,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            size: b_size,
            damping: b_damping,
            mix: b_mix,
            predelay_ms: b_predelay_ms,
            width: b_width,
            mode: b_mode,
            decay_s: b_decay_s,
            diffusion: b_diffusion,
            low_cut_hz: b_low_cut_hz,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_size.exact(b_size)
            && a_damping.exact(b_damping)
            && a_mix.exact(b_mix)
            && a_predelay_ms.exact(b_predelay_ms)
            && a_width.exact(b_width)
            && a_mode.exact(b_mode)
            && a_decay_s.exact(b_decay_s)
            && a_diffusion.exact(b_diffusion)
            && a_low_cut_hz.exact(b_low_cut_hz)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            size,
            damping,
            mix,
            predelay_ms,
            width,
            mode,
            decay_s,
            diffusion,
            low_cut_hz,
            id,
            bypass,
        } = self;
        size.finite()
            && damping.finite()
            && mix.finite()
            && predelay_ms.finite()
            && width.finite()
            && mode.finite()
            && decay_s.finite()
            && diffusion.finite()
            && low_cut_hz.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/fx.rs, ReverbMode.
impl B1Data for crate::fx::ReverbMode {
    fn exact(&self, other: &Self) -> bool {
        match self {
            Self::Freeverb => matches!(other, Self::Freeverb),
            Self::FdnRoom => matches!(other, Self::FdnRoom),
            Self::FdnPlate => matches!(other, Self::FdnPlate),
            Self::FdnHall => matches!(other, Self::FdnHall),
        }
    }
    fn finite(&self) -> bool {
        match self {
            Self::Freeverb => true,
            Self::FdnRoom => true,
            Self::FdnPlate => true,
            Self::FdnHall => true,
        }
    }
}

// Current source: src/fx.rs, RingModFx.
impl B1Data for crate::fx::RingModFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            freq_hz: a_freq_hz,
            shape: a_shape,
            stereo_phase: a_stereo_phase,
            lfo_hz: a_lfo_hz,
            lfo_depth: a_lfo_depth,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            freq_hz: b_freq_hz,
            shape: b_shape,
            stereo_phase: b_stereo_phase,
            lfo_hz: b_lfo_hz,
            lfo_depth: b_lfo_depth,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_freq_hz.exact(b_freq_hz)
            && a_shape.exact(b_shape)
            && a_stereo_phase.exact(b_stereo_phase)
            && a_lfo_hz.exact(b_lfo_hz)
            && a_lfo_depth.exact(b_lfo_depth)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            freq_hz,
            shape,
            stereo_phase,
            lfo_hz,
            lfo_depth,
            mix,
            id,
            bypass,
        } = self;
        freq_hz.finite()
            && shape.finite()
            && stereo_phase.finite()
            && lfo_hz.finite()
            && lfo_depth.finite()
            && mix.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/samples.rs, SampleInfo.
impl B1Data for crate::samples::SampleInfo {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            name: a_name,
            path: a_path,
            source: a_source,
            license: a_license,
            author: a_author,
            duration: a_duration,
        } = self;
        let Self {
            name: b_name,
            path: b_path,
            source: b_source,
            license: b_license,
            author: b_author,
            duration: b_duration,
        } = other;
        a_name.exact(b_name)
            && a_path.exact(b_path)
            && a_source.exact(b_source)
            && a_license.exact(b_license)
            && a_author.exact(b_author)
            && a_duration.exact(b_duration)
    }
    fn finite(&self) -> bool {
        let Self {
            name,
            path,
            source,
            license,
            author,
            duration,
        } = self;
        name.finite()
            && path.finite()
            && source.finite()
            && license.finite()
            && author.finite()
            && duration.finite()
    }
}

// Current source: src/instruments.rs, SamplerParams.
impl B1Data for crate::instruments::SamplerParams {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            sample: a_sample,
            root: a_root,
            one_shot: a_one_shot,
            reverse: a_reverse,
            start: a_start,
            max_length: a_max_length,
            attack: a_attack,
            release: a_release,
            gain: a_gain,
            slices: a_slices,
        } = self;
        let Self {
            sample: b_sample,
            root: b_root,
            one_shot: b_one_shot,
            reverse: b_reverse,
            start: b_start,
            max_length: b_max_length,
            attack: b_attack,
            release: b_release,
            gain: b_gain,
            slices: b_slices,
        } = other;
        a_sample.exact(b_sample)
            && a_root.exact(b_root)
            && a_one_shot.exact(b_one_shot)
            && a_reverse.exact(b_reverse)
            && a_start.exact(b_start)
            && a_max_length.exact(b_max_length)
            && a_attack.exact(b_attack)
            && a_release.exact(b_release)
            && a_gain.exact(b_gain)
            && a_slices.exact(b_slices)
    }
    fn finite(&self) -> bool {
        let Self {
            sample,
            root,
            one_shot,
            reverse,
            start,
            max_length,
            attack,
            release,
            gain,
            slices,
        } = self;
        sample.finite()
            && root.finite()
            && one_shot.finite()
            && reverse.finite()
            && start.finite()
            && max_length.finite()
            && attack.finite()
            && release.finite()
            && gain.finite()
            && slices.finite()
    }
}

// Current source: src/fx.rs, SatMode.
impl B1Data for crate::fx::SatMode {
    fn exact(&self, other: &Self) -> bool {
        match self {
            Self::Tape => matches!(other, Self::Tape),
            Self::Tube => matches!(other, Self::Tube),
            Self::Transistor => matches!(other, Self::Transistor),
            Self::Diode => matches!(other, Self::Diode),
            Self::Fold => matches!(other, Self::Fold),
            Self::Exciter => matches!(other, Self::Exciter),
        }
    }
    fn finite(&self) -> bool {
        match self {
            Self::Tape => true,
            Self::Tube => true,
            Self::Transistor => true,
            Self::Diode => true,
            Self::Fold => true,
            Self::Exciter => true,
        }
    }
}

// Current source: src/fx.rs, SaturatorFx.
impl B1Data for crate::fx::SaturatorFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            mode: a_mode,
            drive_db: a_drive_db,
            tone_hz: a_tone_hz,
            bias: a_bias,
            mix: a_mix,
            output_db: a_output_db,
            oversample: a_oversample,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            mode: b_mode,
            drive_db: b_drive_db,
            tone_hz: b_tone_hz,
            bias: b_bias,
            mix: b_mix,
            output_db: b_output_db,
            oversample: b_oversample,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_mode.exact(b_mode)
            && a_drive_db.exact(b_drive_db)
            && a_tone_hz.exact(b_tone_hz)
            && a_bias.exact(b_bias)
            && a_mix.exact(b_mix)
            && a_output_db.exact(b_output_db)
            && a_oversample.exact(b_oversample)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            mode,
            drive_db,
            tone_hz,
            bias,
            mix,
            output_db,
            oversample,
            id,
            bypass,
        } = self;
        mode.finite()
            && drive_db.finite()
            && tone_hz.finite()
            && bias.finite()
            && mix.finite()
            && output_db.finite()
            && oversample.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/project.rs, Section.
impl B1Data for crate::project::Section {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            pattern: a_pattern,
            repeats: a_repeats,
        } = self;
        let Self {
            pattern: b_pattern,
            repeats: b_repeats,
        } = other;
        a_pattern.exact(b_pattern) && a_repeats.exact(b_repeats)
    }
    fn finite(&self) -> bool {
        let Self { pattern, repeats } = self;
        pattern.finite() && repeats.finite()
    }
}

// Current source: src/project.rs, SeedEntry.
impl B1Data for crate::project::SeedEntry {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            tool: a_tool,
            seed: a_seed,
            source: a_source,
            args: a_args,
            at: a_at,
        } = self;
        let Self {
            tool: b_tool,
            seed: b_seed,
            source: b_source,
            args: b_args,
            at: b_at,
        } = other;
        a_tool.exact(b_tool)
            && a_seed.exact(b_seed)
            && a_source.exact(b_source)
            && a_args.exact(b_args)
            && a_at.exact(b_at)
    }
    fn finite(&self) -> bool {
        let Self {
            tool,
            seed,
            source,
            args,
            at,
        } = self;
        tool.finite() && seed.finite() && source.finite() && args.finite() && at.finite()
    }
}

// Current source: src/project.rs, Send.
impl B1Data for crate::project::Send {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            id: a_id,
            bus: a_bus,
            db: a_db,
            pre_fader: a_pre_fader,
        } = self;
        let Self {
            id: b_id,
            bus: b_bus,
            db: b_db,
            pre_fader: b_pre_fader,
        } = other;
        a_id.exact(b_id) && a_bus.exact(b_bus) && a_db.exact(b_db) && a_pre_fader.exact(b_pre_fader)
    }
    fn finite(&self) -> bool {
        let Self {
            id,
            bus,
            db,
            pre_fader,
        } = self;
        id.finite() && bus.finite() && db.finite() && pre_fader.finite()
    }
}

// Current source: src/fx.rs, SidechainFx.
impl B1Data for crate::fx::SidechainFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            source: a_source,
            amount: a_amount,
            release_ms: a_release_ms,
            attack_ms: a_attack_ms,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            source: b_source,
            amount: b_amount,
            release_ms: b_release_ms,
            attack_ms: b_attack_ms,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_source.exact(b_source)
            && a_amount.exact(b_amount)
            && a_release_ms.exact(b_release_ms)
            && a_attack_ms.exact(b_attack_ms)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            source,
            amount,
            release_ms,
            attack_ms,
            id,
            bypass,
        } = self;
        source.finite()
            && amount.finite()
            && release_ms.finite()
            && attack_ms.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/fx.rs, SoftClipFx.
impl B1Data for crate::fx::SoftClipFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            threshold_db: a_threshold_db,
            ceiling_db: a_ceiling_db,
            drive_db: a_drive_db,
            mix: a_mix,
            oversample: a_oversample,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            threshold_db: b_threshold_db,
            ceiling_db: b_ceiling_db,
            drive_db: b_drive_db,
            mix: b_mix,
            oversample: b_oversample,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_threshold_db.exact(b_threshold_db)
            && a_ceiling_db.exact(b_ceiling_db)
            && a_drive_db.exact(b_drive_db)
            && a_mix.exact(b_mix)
            && a_oversample.exact(b_oversample)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            threshold_db,
            ceiling_db,
            drive_db,
            mix,
            oversample,
            id,
            bypass,
        } = self;
        threshold_db.finite()
            && ceiling_db.finite()
            && drive_db.finite()
            && mix.finite()
            && oversample.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/song.rs, SongState.
impl B1Data for crate::song::SongState {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            spec: a_spec,
            seed: a_seed,
            sections: a_sections,
            audio: a_audio,
            checks: a_checks,
        } = self;
        let Self {
            spec: b_spec,
            seed: b_seed,
            sections: b_sections,
            audio: b_audio,
            checks: b_checks,
        } = other;
        a_spec.exact(b_spec)
            && a_seed.exact(b_seed)
            && a_sections.exact(b_sections)
            && a_audio.exact(b_audio)
            && a_checks.exact(b_checks)
    }
    fn finite(&self) -> bool {
        let Self {
            spec,
            seed,
            sections,
            audio,
            checks,
        } = self;
        spec.finite() && seed.finite() && sections.finite() && audio.finite() && checks.finite()
    }
}

// Current source: src/soundfont.rs, SoundFontParams.
impl B1Data for crate::soundfont::SoundFontParams {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            font: a_font,
            bank: a_bank,
            program: a_program,
            drums: a_drums,
            fixed_key: a_fixed_key,
            gain: a_gain,
            release_max: a_release_max,
            transpose: a_transpose,
            preset_name: a_preset_name,
        } = self;
        let Self {
            font: b_font,
            bank: b_bank,
            program: b_program,
            drums: b_drums,
            fixed_key: b_fixed_key,
            gain: b_gain,
            release_max: b_release_max,
            transpose: b_transpose,
            preset_name: b_preset_name,
        } = other;
        a_font.exact(b_font)
            && a_bank.exact(b_bank)
            && a_program.exact(b_program)
            && a_drums.exact(b_drums)
            && a_fixed_key.exact(b_fixed_key)
            && a_gain.exact(b_gain)
            && a_release_max.exact(b_release_max)
            && a_transpose.exact(b_transpose)
            && a_preset_name.exact(b_preset_name)
    }
    fn finite(&self) -> bool {
        let Self {
            font,
            bank,
            program,
            drums,
            fixed_key,
            gain,
            release_max,
            transpose,
            preset_name,
        } = self;
        font.finite()
            && bank.finite()
            && program.finite()
            && drums.finite()
            && fixed_key.finite()
            && gain.finite()
            && release_max.finite()
            && transpose.finite()
            && preset_name.finite()
    }
}

// Current source: src/mix_rules.rs, Span.
impl B1Data for crate::mix_rules::Span {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            name: a_name,
            kind: a_kind,
            start_beat: a_start_beat,
            end_beat: a_end_beat,
        } = self;
        let Self {
            name: b_name,
            kind: b_kind,
            start_beat: b_start_beat,
            end_beat: b_end_beat,
        } = other;
        a_name.exact(b_name)
            && a_kind.exact(b_kind)
            && a_start_beat.exact(b_start_beat)
            && a_end_beat.exact(b_end_beat)
    }
    fn finite(&self) -> bool {
        let Self {
            name,
            kind,
            start_beat,
            end_beat,
        } = self;
        name.finite() && kind.finite() && start_beat.finite() && end_beat.finite()
    }
}

// Current source: src/fx.rs, StereoShaperFx.
impl B1Data for crate::fx::StereoShaperFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            preset: a_preset,
            left_from_left: a_left_from_left,
            left_from_right: a_left_from_right,
            right_from_left: a_right_from_left,
            right_from_right: a_right_from_right,
            delay_ms: a_delay_ms,
            delay_side: a_delay_side,
            invert_left: a_invert_left,
            invert_right: a_invert_right,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            preset: b_preset,
            left_from_left: b_left_from_left,
            left_from_right: b_left_from_right,
            right_from_left: b_right_from_left,
            right_from_right: b_right_from_right,
            delay_ms: b_delay_ms,
            delay_side: b_delay_side,
            invert_left: b_invert_left,
            invert_right: b_invert_right,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_preset.exact(b_preset)
            && a_left_from_left.exact(b_left_from_left)
            && a_left_from_right.exact(b_left_from_right)
            && a_right_from_left.exact(b_right_from_left)
            && a_right_from_right.exact(b_right_from_right)
            && a_delay_ms.exact(b_delay_ms)
            && a_delay_side.exact(b_delay_side)
            && a_invert_left.exact(b_invert_left)
            && a_invert_right.exact(b_invert_right)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            preset,
            left_from_left,
            left_from_right,
            right_from_left,
            right_from_right,
            delay_ms,
            delay_side,
            invert_left,
            invert_right,
            mix,
            id,
            bypass,
        } = self;
        preset.finite()
            && left_from_left.finite()
            && left_from_right.finite()
            && right_from_left.finite()
            && right_from_right.finite()
            && delay_ms.finite()
            && delay_side.finite()
            && invert_left.finite()
            && invert_right.finite()
            && mix.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/fx.rs, StutterFx.
impl B1Data for crate::fx::StutterFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            mode: a_mode,
            pattern: a_pattern,
            slice_steps: a_slice_steps,
            cycle_steps: a_cycle_steps,
            smooth_ms: a_smooth_ms,
            mix: a_mix,
            block_samples: a_block_samples,
            slice_blocks: a_slice_blocks,
            repeat_count: a_repeat_count,
            trigger_sample: a_trigger_sample,
            wrap_fade_ms: a_wrap_fade_ms,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            mode: b_mode,
            pattern: b_pattern,
            slice_steps: b_slice_steps,
            cycle_steps: b_cycle_steps,
            smooth_ms: b_smooth_ms,
            mix: b_mix,
            block_samples: b_block_samples,
            slice_blocks: b_slice_blocks,
            repeat_count: b_repeat_count,
            trigger_sample: b_trigger_sample,
            wrap_fade_ms: b_wrap_fade_ms,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_mode.exact(b_mode)
            && a_pattern.exact(b_pattern)
            && a_slice_steps.exact(b_slice_steps)
            && a_cycle_steps.exact(b_cycle_steps)
            && a_smooth_ms.exact(b_smooth_ms)
            && a_mix.exact(b_mix)
            && a_block_samples.exact(b_block_samples)
            && a_slice_blocks.exact(b_slice_blocks)
            && a_repeat_count.exact(b_repeat_count)
            && a_trigger_sample.exact(b_trigger_sample)
            && a_wrap_fade_ms.exact(b_wrap_fade_ms)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            mode,
            pattern,
            slice_steps,
            cycle_steps,
            smooth_ms,
            mix,
            block_samples,
            slice_blocks,
            repeat_count,
            trigger_sample,
            wrap_fade_ms,
            id,
            bypass,
        } = self;
        mode.finite()
            && pattern.finite()
            && slice_steps.finite()
            && cycle_steps.finite()
            && smooth_ms.finite()
            && mix.finite()
            && block_samples.finite()
            && slice_blocks.finite()
            && repeat_count.finite()
            && trigger_sample.finite()
            && wrap_fade_ms.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/fx.rs, StutterMode.
impl B1Data for crate::fx::StutterMode {
    fn exact(&self, other: &Self) -> bool {
        match self {
            Self::Gate => matches!(other, Self::Gate),
            Self::Stutter => matches!(other, Self::Stutter),
            Self::HalfTime => matches!(other, Self::HalfTime),
            Self::Reverse => matches!(other, Self::Reverse),
            Self::TapeStop => matches!(other, Self::TapeStop),
            Self::SliceRepeat => matches!(other, Self::SliceRepeat),
        }
    }
    fn finite(&self) -> bool {
        match self {
            Self::Gate => true,
            Self::Stutter => true,
            Self::HalfTime => true,
            Self::Reverse => true,
            Self::TapeStop => true,
            Self::SliceRepeat => true,
        }
    }
}

// Current source: src/instruments.rs, SynthParams.
impl B1Data for crate::instruments::SynthParams {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            osc1: a_osc1,
            osc2: a_osc2,
            osc2_semitones: a_osc2_semitones,
            osc2_cents: a_osc2_cents,
            osc_mix: a_osc_mix,
            unison: a_unison,
            unison_spread_cents: a_unison_spread_cents,
            sub_level: a_sub_level,
            noise_level: a_noise_level,
            filter_mode: a_filter_mode,
            cutoff: a_cutoff,
            resonance: a_resonance,
            filter_env_amount: a_filter_env_amount,
            filter_env: a_filter_env,
            amp_env: a_amp_env,
            lfo_rate: a_lfo_rate,
            lfo_to_cutoff: a_lfo_to_cutoff,
            lfo_to_pitch: a_lfo_to_pitch,
            pitch_env_semitones: a_pitch_env_semitones,
            pitch_env_time: a_pitch_env_time,
            drive: a_drive,
            gain: a_gain,
            glide_ms: a_glide_ms,
            stereo_spread: a_stereo_spread,
            drift_cents: a_drift_cents,
            vel_to_cutoff: a_vel_to_cutoff,
        } = self;
        let Self {
            osc1: b_osc1,
            osc2: b_osc2,
            osc2_semitones: b_osc2_semitones,
            osc2_cents: b_osc2_cents,
            osc_mix: b_osc_mix,
            unison: b_unison,
            unison_spread_cents: b_unison_spread_cents,
            sub_level: b_sub_level,
            noise_level: b_noise_level,
            filter_mode: b_filter_mode,
            cutoff: b_cutoff,
            resonance: b_resonance,
            filter_env_amount: b_filter_env_amount,
            filter_env: b_filter_env,
            amp_env: b_amp_env,
            lfo_rate: b_lfo_rate,
            lfo_to_cutoff: b_lfo_to_cutoff,
            lfo_to_pitch: b_lfo_to_pitch,
            pitch_env_semitones: b_pitch_env_semitones,
            pitch_env_time: b_pitch_env_time,
            drive: b_drive,
            gain: b_gain,
            glide_ms: b_glide_ms,
            stereo_spread: b_stereo_spread,
            drift_cents: b_drift_cents,
            vel_to_cutoff: b_vel_to_cutoff,
        } = other;
        a_osc1.exact(b_osc1)
            && a_osc2.exact(b_osc2)
            && a_osc2_semitones.exact(b_osc2_semitones)
            && a_osc2_cents.exact(b_osc2_cents)
            && a_osc_mix.exact(b_osc_mix)
            && a_unison.exact(b_unison)
            && a_unison_spread_cents.exact(b_unison_spread_cents)
            && a_sub_level.exact(b_sub_level)
            && a_noise_level.exact(b_noise_level)
            && a_filter_mode.exact(b_filter_mode)
            && a_cutoff.exact(b_cutoff)
            && a_resonance.exact(b_resonance)
            && a_filter_env_amount.exact(b_filter_env_amount)
            && a_filter_env.exact(b_filter_env)
            && a_amp_env.exact(b_amp_env)
            && a_lfo_rate.exact(b_lfo_rate)
            && a_lfo_to_cutoff.exact(b_lfo_to_cutoff)
            && a_lfo_to_pitch.exact(b_lfo_to_pitch)
            && a_pitch_env_semitones.exact(b_pitch_env_semitones)
            && a_pitch_env_time.exact(b_pitch_env_time)
            && a_drive.exact(b_drive)
            && a_gain.exact(b_gain)
            && a_glide_ms.exact(b_glide_ms)
            && a_stereo_spread.exact(b_stereo_spread)
            && a_drift_cents.exact(b_drift_cents)
            && a_vel_to_cutoff.exact(b_vel_to_cutoff)
    }
    fn finite(&self) -> bool {
        let Self {
            osc1,
            osc2,
            osc2_semitones,
            osc2_cents,
            osc_mix,
            unison,
            unison_spread_cents,
            sub_level,
            noise_level,
            filter_mode,
            cutoff,
            resonance,
            filter_env_amount,
            filter_env,
            amp_env,
            lfo_rate,
            lfo_to_cutoff,
            lfo_to_pitch,
            pitch_env_semitones,
            pitch_env_time,
            drive,
            gain,
            glide_ms,
            stereo_spread,
            drift_cents,
            vel_to_cutoff,
        } = self;
        osc1.finite()
            && osc2.finite()
            && osc2_semitones.finite()
            && osc2_cents.finite()
            && osc_mix.finite()
            && unison.finite()
            && unison_spread_cents.finite()
            && sub_level.finite()
            && noise_level.finite()
            && filter_mode.finite()
            && cutoff.finite()
            && resonance.finite()
            && filter_env_amount.finite()
            && filter_env.finite()
            && amp_env.finite()
            && lfo_rate.finite()
            && lfo_to_cutoff.finite()
            && lfo_to_pitch.finite()
            && pitch_env_semitones.finite()
            && pitch_env_time.finite()
            && drive.finite()
            && gain.finite()
            && glide_ms.finite()
            && stereo_spread.finite()
            && drift_cents.finite()
            && vel_to_cutoff.finite()
    }
}

// Current source: src/project.rs, TempoPoint.
impl B1Data for crate::project::TempoPoint {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            beat: a_beat,
            bpm: a_bpm,
            ramp: a_ramp,
        } = self;
        let Self {
            beat: b_beat,
            bpm: b_bpm,
            ramp: b_ramp,
        } = other;
        a_beat.exact(b_beat) && a_bpm.exact(b_bpm) && a_ramp.exact(b_ramp)
    }
    fn finite(&self) -> bool {
        let Self { beat, bpm, ramp } = self;
        beat.finite() && bpm.finite() && ramp.finite()
    }
}

// Current source: src/project.rs, Track.
impl B1Data for crate::project::Track {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            id: a_id,
            name: a_name,
            instrument: a_instrument,
            volume_db: a_volume_db,
            pan: a_pan,
            mute: a_mute,
            solo: a_solo,
            effects: a_effects,
            output: a_output,
            sends: a_sends,
        } = self;
        let Self {
            id: b_id,
            name: b_name,
            instrument: b_instrument,
            volume_db: b_volume_db,
            pan: b_pan,
            mute: b_mute,
            solo: b_solo,
            effects: b_effects,
            output: b_output,
            sends: b_sends,
        } = other;
        a_id.exact(b_id)
            && a_name.exact(b_name)
            && a_instrument.exact(b_instrument)
            && a_volume_db.exact(b_volume_db)
            && a_pan.exact(b_pan)
            && a_mute.exact(b_mute)
            && a_solo.exact(b_solo)
            && a_effects.exact(b_effects)
            && a_output.exact(b_output)
            && a_sends.exact(b_sends)
    }
    fn finite(&self) -> bool {
        let Self {
            id,
            name,
            instrument,
            volume_db,
            pan,
            mute,
            solo,
            effects,
            output,
            sends,
        } = self;
        id.finite()
            && name.finite()
            && instrument.finite()
            && volume_db.finite()
            && pan.finite()
            && mute.finite()
            && solo.finite()
            && effects.finite()
            && output.finite()
            && sends.finite()
    }
}

// Current source: src/fx.rs, TransientFx.
impl B1Data for crate::fx::TransientFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            attack: a_attack,
            sustain: a_sustain,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            attack: b_attack,
            sustain: b_sustain,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_attack.exact(b_attack)
            && a_sustain.exact(b_sustain)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            attack,
            sustain,
            id,
            bypass,
        } = self;
        attack.finite() && sustain.finite() && id.finite() && bypass.finite()
    }
}

// Current source: src/fx.rs, VinylFx.
impl B1Data for crate::fx::VinylFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            crackle: a_crackle,
            crackle_db: a_crackle_db,
            hiss_db: a_hiss_db,
            wow_cents: a_wow_cents,
            wow_hz: a_wow_hz,
            flutter_cents: a_flutter_cents,
            flutter_hz: a_flutter_hz,
            age: a_age,
            mono: a_mono,
            seed: a_seed,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            crackle: b_crackle,
            crackle_db: b_crackle_db,
            hiss_db: b_hiss_db,
            wow_cents: b_wow_cents,
            wow_hz: b_wow_hz,
            flutter_cents: b_flutter_cents,
            flutter_hz: b_flutter_hz,
            age: b_age,
            mono: b_mono,
            seed: b_seed,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_crackle.exact(b_crackle)
            && a_crackle_db.exact(b_crackle_db)
            && a_hiss_db.exact(b_hiss_db)
            && a_wow_cents.exact(b_wow_cents)
            && a_wow_hz.exact(b_wow_hz)
            && a_flutter_cents.exact(b_flutter_cents)
            && a_flutter_hz.exact(b_flutter_hz)
            && a_age.exact(b_age)
            && a_mono.exact(b_mono)
            && a_seed.exact(b_seed)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            crackle,
            crackle_db,
            hiss_db,
            wow_cents,
            wow_hz,
            flutter_cents,
            flutter_hz,
            age,
            mono,
            seed,
            mix,
            id,
            bypass,
        } = self;
        crackle.finite()
            && crackle_db.finite()
            && hiss_db.finite()
            && wow_cents.finite()
            && wow_hz.finite()
            && flutter_cents.finite()
            && flutter_hz.finite()
            && age.finite()
            && mono.finite()
            && seed.finite()
            && mix.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/project.rs, VocalMap.
impl B1Data for crate::project::VocalMap {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            track: a_track,
            sample: a_sample,
            words: a_words,
            notes: a_notes,
            chords: a_chords,
            sections: a_sections,
            key: a_key,
            warped: a_warped,
            phrases_pinned: a_phrases_pinned,
        } = self;
        let Self {
            track: b_track,
            sample: b_sample,
            words: b_words,
            notes: b_notes,
            chords: b_chords,
            sections: b_sections,
            key: b_key,
            warped: b_warped,
            phrases_pinned: b_phrases_pinned,
        } = other;
        a_track.exact(b_track)
            && a_sample.exact(b_sample)
            && a_words.exact(b_words)
            && a_notes.exact(b_notes)
            && a_chords.exact(b_chords)
            && a_sections.exact(b_sections)
            && a_key.exact(b_key)
            && a_warped.exact(b_warped)
            && a_phrases_pinned.exact(b_phrases_pinned)
    }
    fn finite(&self) -> bool {
        let Self {
            track,
            sample,
            words,
            notes,
            chords,
            sections,
            key,
            warped,
            phrases_pinned,
        } = self;
        track.finite()
            && sample.finite()
            && words.finite()
            && notes.finite()
            && chords.finite()
            && sections.finite()
            && key.finite()
            && warped.finite()
            && phrases_pinned.finite()
    }
}

// Current source: src/fx.rs, VocoderFx.
impl B1Data for crate::fx::VocoderFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            notes: a_notes,
            bands: a_bands,
            low_hz: a_low_hz,
            high_hz: a_high_hz,
            attack_ms: a_attack_ms,
            release_ms: a_release_ms,
            noise: a_noise,
            sibilance: a_sibilance,
            gain: a_gain,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            notes: b_notes,
            bands: b_bands,
            low_hz: b_low_hz,
            high_hz: b_high_hz,
            attack_ms: b_attack_ms,
            release_ms: b_release_ms,
            noise: b_noise,
            sibilance: b_sibilance,
            gain: b_gain,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_notes.exact(b_notes)
            && a_bands.exact(b_bands)
            && a_low_hz.exact(b_low_hz)
            && a_high_hz.exact(b_high_hz)
            && a_attack_ms.exact(b_attack_ms)
            && a_release_ms.exact(b_release_ms)
            && a_noise.exact(b_noise)
            && a_sibilance.exact(b_sibilance)
            && a_gain.exact(b_gain)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            notes,
            bands,
            low_hz,
            high_hz,
            attack_ms,
            release_ms,
            noise,
            sibilance,
            gain,
            mix,
            id,
            bypass,
        } = self;
        notes.finite()
            && bands.finite()
            && low_hz.finite()
            && high_hz.finite()
            && attack_ms.finite()
            && release_ms.finite()
            && noise.finite()
            && sibilance.finite()
            && gain.finite()
            && mix.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/dsp.rs, Wave.
impl B1Data for crate::dsp::Wave {
    fn exact(&self, other: &Self) -> bool {
        match self {
            Self::Sine => matches!(other, Self::Sine),
            Self::Saw => matches!(other, Self::Saw),
            Self::Square => matches!(other, Self::Square),
            Self::Triangle => matches!(other, Self::Triangle),
            Self::Noise => matches!(other, Self::Noise),
        }
    }
    fn finite(&self) -> bool {
        match self {
            Self::Sine => true,
            Self::Saw => true,
            Self::Square => true,
            Self::Triangle => true,
            Self::Noise => true,
        }
    }
}

// Current source: src/fx.rs, WaveshaperFx.
impl B1Data for crate::fx::WaveshaperFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            preset: a_preset,
            curve: a_curve,
            symmetric: a_symmetric,
            drive_db: a_drive_db,
            output_db: a_output_db,
            mix: a_mix,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            preset: b_preset,
            curve: b_curve,
            symmetric: b_symmetric,
            drive_db: b_drive_db,
            output_db: b_output_db,
            mix: b_mix,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_preset.exact(b_preset)
            && a_curve.exact(b_curve)
            && a_symmetric.exact(b_symmetric)
            && a_drive_db.exact(b_drive_db)
            && a_output_db.exact(b_output_db)
            && a_mix.exact(b_mix)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            preset,
            curve,
            symmetric,
            drive_db,
            output_db,
            mix,
            id,
            bypass,
        } = self;
        preset.finite()
            && curve.finite()
            && symmetric.finite()
            && drive_db.finite()
            && output_db.finite()
            && mix.finite()
            && id.finite()
            && bypass.finite()
    }
}

// Current source: src/instruments.rs, WavetableParams.
impl B1Data for crate::instruments::WavetableParams {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            table: a_table,
            position: a_position,
            position_env_amount: a_position_env_amount,
            position_env: a_position_env,
            position_lfo_rate: a_position_lfo_rate,
            position_lfo_depth: a_position_lfo_depth,
            unison: a_unison,
            unison_spread_cents: a_unison_spread_cents,
            sub_level: a_sub_level,
            filter_mode: a_filter_mode,
            cutoff: a_cutoff,
            resonance: a_resonance,
            filter_env_amount: a_filter_env_amount,
            filter_env: a_filter_env,
            amp_env: a_amp_env,
            drive: a_drive,
            gain: a_gain,
            stereo_spread: a_stereo_spread,
        } = self;
        let Self {
            table: b_table,
            position: b_position,
            position_env_amount: b_position_env_amount,
            position_env: b_position_env,
            position_lfo_rate: b_position_lfo_rate,
            position_lfo_depth: b_position_lfo_depth,
            unison: b_unison,
            unison_spread_cents: b_unison_spread_cents,
            sub_level: b_sub_level,
            filter_mode: b_filter_mode,
            cutoff: b_cutoff,
            resonance: b_resonance,
            filter_env_amount: b_filter_env_amount,
            filter_env: b_filter_env,
            amp_env: b_amp_env,
            drive: b_drive,
            gain: b_gain,
            stereo_spread: b_stereo_spread,
        } = other;
        a_table.exact(b_table)
            && a_position.exact(b_position)
            && a_position_env_amount.exact(b_position_env_amount)
            && a_position_env.exact(b_position_env)
            && a_position_lfo_rate.exact(b_position_lfo_rate)
            && a_position_lfo_depth.exact(b_position_lfo_depth)
            && a_unison.exact(b_unison)
            && a_unison_spread_cents.exact(b_unison_spread_cents)
            && a_sub_level.exact(b_sub_level)
            && a_filter_mode.exact(b_filter_mode)
            && a_cutoff.exact(b_cutoff)
            && a_resonance.exact(b_resonance)
            && a_filter_env_amount.exact(b_filter_env_amount)
            && a_filter_env.exact(b_filter_env)
            && a_amp_env.exact(b_amp_env)
            && a_drive.exact(b_drive)
            && a_gain.exact(b_gain)
            && a_stereo_spread.exact(b_stereo_spread)
    }
    fn finite(&self) -> bool {
        let Self {
            table,
            position,
            position_env_amount,
            position_env,
            position_lfo_rate,
            position_lfo_depth,
            unison,
            unison_spread_cents,
            sub_level,
            filter_mode,
            cutoff,
            resonance,
            filter_env_amount,
            filter_env,
            amp_env,
            drive,
            gain,
            stereo_spread,
        } = self;
        table.finite()
            && position.finite()
            && position_env_amount.finite()
            && position_env.finite()
            && position_lfo_rate.finite()
            && position_lfo_depth.finite()
            && unison.finite()
            && unison_spread_cents.finite()
            && sub_level.finite()
            && filter_mode.finite()
            && cutoff.finite()
            && resonance.finite()
            && filter_env_amount.finite()
            && filter_env.finite()
            && amp_env.finite()
            && drive.finite()
            && gain.finite()
            && stereo_spread.finite()
    }
}

// Current source: src/fx.rs, WidthFx.
impl B1Data for crate::fx::WidthFx {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            amount: a_amount,
            low_mono_hz: a_low_mono_hz,
            id: a_id,
            bypass: a_bypass,
        } = self;
        let Self {
            amount: b_amount,
            low_mono_hz: b_low_mono_hz,
            id: b_id,
            bypass: b_bypass,
        } = other;
        a_amount.exact(b_amount)
            && a_low_mono_hz.exact(b_low_mono_hz)
            && a_id.exact(b_id)
            && a_bypass.exact(b_bypass)
    }
    fn finite(&self) -> bool {
        let Self {
            amount,
            low_mono_hz,
            id,
            bypass,
        } = self;
        amount.finite() && low_mono_hz.finite() && id.finite() && bypass.finite()
    }
}

// Current source: src/instruments.rs, WtTable.
impl B1Data for crate::instruments::WtTable {
    fn exact(&self, other: &Self) -> bool {
        match self {
            Self::Basic => matches!(other, Self::Basic),
            Self::Harmonic => matches!(other, Self::Harmonic),
            Self::Pwm => matches!(other, Self::Pwm),
            Self::Vocal => matches!(other, Self::Vocal),
            Self::Digital => matches!(other, Self::Digital),
            Self::Organ => matches!(other, Self::Organ),
        }
    }
    fn finite(&self) -> bool {
        match self {
            Self::Basic => true,
            Self::Harmonic => true,
            Self::Pwm => true,
            Self::Vocal => true,
            Self::Digital => true,
            Self::Organ => true,
        }
    }
}

// Current source: src/multisample.rs, Zone.
impl B1Data for crate::multisample::Zone {
    fn exact(&self, other: &Self) -> bool {
        let Self {
            sample: a_sample,
            root: a_root,
            lo_note: a_lo_note,
            hi_note: a_hi_note,
            vel_lo: a_vel_lo,
            vel_hi: a_vel_hi,
            tune_cents: a_tune_cents,
            gain_db: a_gain_db,
            rr: a_rr,
        } = self;
        let Self {
            sample: b_sample,
            root: b_root,
            lo_note: b_lo_note,
            hi_note: b_hi_note,
            vel_lo: b_vel_lo,
            vel_hi: b_vel_hi,
            tune_cents: b_tune_cents,
            gain_db: b_gain_db,
            rr: b_rr,
        } = other;
        a_sample.exact(b_sample)
            && a_root.exact(b_root)
            && a_lo_note.exact(b_lo_note)
            && a_hi_note.exact(b_hi_note)
            && a_vel_lo.exact(b_vel_lo)
            && a_vel_hi.exact(b_vel_hi)
            && a_tune_cents.exact(b_tune_cents)
            && a_gain_db.exact(b_gain_db)
            && a_rr.exact(b_rr)
    }
    fn finite(&self) -> bool {
        let Self {
            sample,
            root,
            lo_note,
            hi_note,
            vel_lo,
            vel_hi,
            tune_cents,
            gain_db,
            rr,
        } = self;
        sample.finite()
            && root.finite()
            && lo_note.finite()
            && hi_note.finite()
            && vel_lo.finite()
            && vel_hi.finite()
            && tune_cents.finite()
            && gain_db.finite()
            && rr.finite()
    }
}

/// Exact equality of all fields of this build's current in-memory Project.
/// Does not make a future-schema/unknown-field preservation claim.
pub(super) fn exact_project(a: &Project, b: &Project) -> bool {
    a.exact(b)
}
pub(super) fn finite_project(p: &Project) -> bool {
    p.finite()
}

const BAD_SCHEMA: &str = "Project identity schema needs migration";
const BAD_IDS: &str = "Project has missing or duplicate object identities";
const BAD_FX_IDS: &str = "An effect chain has missing or duplicate identities";
const NONREFLEXIVE: &str = "Project contains nonreflexive values";
const NONFINITE: &str = "Project contains nonfinite values";
const CODEC_ERROR: &str = "Project cannot roundtrip through its current format";
const CODEC_CHANGE: &str = "Project format would change exact data";
const LOAD_INVALID: &str = "Project fails current load validation";
const LOAD_CHANGE: &str = "Project loading would repair timing data";
const UPKEEP_CHANGE: &str = "Project identity upkeep would change exact data";

// Mirrors ids.rs visit_body slot scope, not owner_keys map cardinality (which
// would hide duplicates). Run this before invoking any upkeep helper.
fn structural_ids(p: &Project) -> Result<(), &'static str> {
    if p.schema_version < crate::ids::SCHEMA_VERSION {
        return Err(BAD_SCHEMA);
    }
    let mut seen = BTreeSet::new();
    let mut slot = |id: &String| -> Result<(), &'static str> {
        if id.is_empty() || !seen.insert(id.clone()) {
            Err(BAD_IDS)
        } else {
            Ok(())
        }
    };
    for t in &p.tracks {
        slot(&t.id)?;
        for s in &t.sends {
            slot(&s.id)?;
        }
    }
    for b in &p.buses {
        slot(&b.id)?;
    }
    for pat in &p.patterns {
        slot(&pat.id)?;
        for notes in pat.clips.values() {
            for n in notes {
                slot(&n.id)?;
            }
        }
    }
    for c in &p.playlist {
        slot(&c.id)?;
    }
    for c in &p.audio_clips {
        slot(&c.id)?;
    }
    for l in &p.automation {
        slot(&l.id)?;
    }
    for c in &p.automation_clips {
        slot(&c.id)?;
    }
    // Effect identities are only unique WITHIN each actual upkeep chain.
    // Saved fx_chains are data, not part of the identity visitor/upkeep scope.
    for chain in std::iter::once(&p.master_effects)
        .chain(p.tracks.iter().map(|t| &t.effects))
        .chain(p.buses.iter().map(|b| &b.effects))
    {
        let mut ids = BTreeSet::new();
        for e in chain {
            if e.id().is_empty() || !ids.insert(e.id()) {
                return Err(BAD_FX_IDS);
            }
        }
    }
    Ok(())
}

/// Read-only preflight. All actual serde, timing and upkeep work is on clones.
/// No Engine, file I/O, live MIDI snapshot/restore, revision or history operation.
/// Call at explicit open AND again under the eventual commit lock, not per frame.
/// MIDI here means the stored MidiSetup exactly as represented. Save intentionally
/// replaces it with the live controller snapshot; restore changes controller state,
/// not Project. This gate promises neither controller equivalence nor file access.
pub(super) fn eligible_project(p: &Project) -> Result<(), &'static str> {
    structural_ids(p)?;
    if !finite_project(p) {
        return Err(NONFINITE);
    }
    if p.clone() != *p {
        return Err(NONREFLEXIVE);
    }
    // Direct textual JSON uses the real f32 serializer and custom deserializers.
    // This is an admissibility test, NOT the independent exact comparator.
    let bytes = serde_json::to_vec(p).map_err(|_| CODEC_ERROR)?;
    let mut decoded: Project = serde_json::from_slice(&bytes).map_err(|_| CODEC_ERROR)?;
    if !finite_project(&decoded) || decoded.clone() != decoded || !exact_project(p, &decoded) {
        return Err(CODEC_CHANGE);
    }
    decoded.validate_effects().map_err(|_| LOAD_INVALID)?;
    decoded.sanitize_timing();
    if !exact_project(p, &decoded) {
        return Err(LOAD_CHANGE);
    }
    // Only eligible data reaches this path. A complete valid MAX id_seq takes
    // ids.rs's no-fresh-slot return, never the seq += 1 branch.
    let owners = crate::ids::owner_keys(p);
    let mut upkeep = p.clone();
    let changed = upkeep.ensure_ids_after(&owners);
    if changed || !exact_project(p, &upkeep) || upkeep.clone() != upkeep {
        return Err(UPKEEP_CHANGE);
    }
    Ok(())
}

// Optional B2 primitive for the later adapter, not dispatch authority. Caller
// first applies B1 to the actual original and full expected post-Project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HistoryGate {
    NoChange,
    ContentEqualExactDifferent,
    Changed,
}
pub(super) fn history_gate(before: &Project, expected: &Project) -> HistoryGate {
    if exact_project(before, expected) {
        HistoryGate::NoChange
    } else if before == expected {
        HistoryGate::ContentEqualExactDifferent
    } else {
        HistoryGate::Changed
    }
}

// Optional pure prediction with the private engine.rs:979-997 semantics. It is
// NOT the target-only exclusion and does NOT authorize compiled source editing.
// Real Engine oracle coverage remains a later full-feature test obligation.
pub(super) fn predicts_playlist_compile(before: &Project, now: &Project) -> bool {
    if now.playlist.is_empty()
        || now.arrangement.is_empty()
        || !now
            .arrangement
            .iter()
            .all(|s| s.pattern.starts_with(crate::tools_fl::PL))
    {
        return false;
    }
    now.playlist.iter().any(|clip| {
        let a = before
            .patterns
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(&clip.pattern));
        let b = now
            .patterns
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(&clip.pattern));
        a != b
    })
}

// Manual rack rotation. These definitions precede the single final test boundary.
const ROTATE_ENTRY: &str = "Rotate rhythm...";
const ROTATE_EARLIER: &str = "Earlier by 1 step";
const ROTATE_LATER: &str = "Later by 1 step";
const ROTATE_CANCEL: &str = "Cancel";
const ROTATE_EMPTY: &str = "No hits in this pattern";
const ROTATE_DRUM: &str =
    "Rotate rhythm is available for drums and one-shot samples. Use Piano roll for this channel.";
const ROTATE_EXPIRED: &str = "Rhythm edit expired. Reopen Rotate rhythm to continue.";
const ROTATE_SCOPE: &str = "Wraps within each bar; does not shift the whole phrase. Lengths and expression stay with each hit. Hits may sustain across a bar/pattern boundary.";
const ROTATE_SHARED: &str = "Edits this source pattern wherever it is reused. Duplicate pattern first for an independent variation.";
const ROTATE_SORT: &str =
    "Hits are reordered by start and pitch. Seeded chance outcomes may change.";
const ROTATE_COMPILED: &str =
    "This source is used by a compiled playlist. Rhythm editing is not supported here yet.";
const ROTATE_EQUAL: &str =
    "This exact edit is not committable: history would not record the changed data.";
const ROTATE_UNCERTAIN: &str = "state uncertain; inspect before continuing";
const ROTATE_COUNTER: &str = "Rhythm edit counter exhausted. No edit was dispatched.";
const ROTATE_TARGET: &str = "Channel, pattern or hit identities cannot be resolved precisely";
const ROTATE_EXTENT: &str = "Invalid pattern extent, meter or hit bounds";

#[derive(Clone, Copy)]
enum Direction {
    Earlier,
    Later,
}
impl Direction {
    fn shift(self) -> f32 {
        match self {
            Self::Earlier => -1.0,
            Self::Later => 1.0,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Earlier => ROTATE_EARLIER,
            Self::Later => ROTATE_LATER,
        }
    }
}

#[derive(Clone)]
struct RotationCapture {
    project: Project,
    revision: u64,
    generation: u64,
    ti: usize,
    pi: usize,
}
struct RotationPrediction {
    project: Project,
    args: serde_json::Value,
    changed_starts: usize,
    affected: usize,
    gate: HistoryGate,
}

fn rotation_drum(t: &crate::project::Track) -> bool {
    t.instrument.is_drum()
        || matches!(&t.instrument, crate::instruments::Instrument::Sampler(s) if s.one_shot)
}
fn rotation_extent(p: &crate::project::Pattern) -> Result<(f32, f32), &'static str> {
    let (n, d) = p.meter();
    if !(1..=64).contains(&p.bars) || !(1..=32).contains(&n) || !matches!(d, 2 | 4 | 8 | 16) {
        return Err(ROTATE_EXTENT);
    }
    let bar = p.steps_per_bar();
    let extent = bar
        .checked_mul(p.bars)
        .filter(|n| *n > 0)
        .ok_or(ROTATE_EXTENT)?;
    Ok((bar as f32, extent as f32))
}
// More conservative than the compiler predicate, which is separately checked on
// the complete actual expected state. Never permits editing generated sources.
fn rotation_compiled(p: &Project, pi: usize) -> bool {
    p.patterns[pi].name.starts_with(crate::tools_fl::PL)
        || (!p.playlist.is_empty()
            && !p.arrangement.is_empty()
            && p.arrangement
                .iter()
                .all(|s| s.pattern.starts_with(crate::tools_fl::PL))
            && p.playlist
                .iter()
                .any(|c| c.pattern.eq_ignore_ascii_case(&p.patterns[pi].name)))
}
fn rotation_target(p: &Project, ti: usize, pi: usize) -> Result<(), &'static str> {
    let t = p.tracks.get(ti).ok_or(ROTATE_EXPIRED)?;
    let pat = p.patterns.get(pi).ok_or(ROTATE_EXPIRED)?;
    if !rotation_drum(t) {
        return Err(ROTATE_DRUM);
    }
    rotation_extent(pat)?;
    if rotation_compiled(p, pi) {
        return Err(ROTATE_COMPILED);
    }
    for name in [&t.name, &pat.name] {
        if name.trim().is_empty() || name.trim() != name || name.contains(',') {
            return Err(ROTATE_TARGET);
        }
        // Reject cross-kind, case and stable-ID collisions, even when a current
        // resolver happens to choose this owner first. Never invent an alias.
        let names = p
            .tracks
            .iter()
            .map(|v| (&v.name, &v.id))
            .chain(p.patterns.iter().map(|v| (&v.name, &v.id)))
            .chain(p.buses.iter().map(|v| (&v.name, &v.id)));
        let mut count = 0;
        for (other, id) in names {
            if other.to_lowercase() == name.to_lowercase() {
                count += 1;
            }
            if id.eq_ignore_ascii_case(name) {
                return Err(ROTATE_TARGET);
            }
        }
        if count != 1 {
            return Err(ROTATE_TARGET);
        }
    }
    let key = t.name.to_lowercase();
    if pat
        .clips
        .keys()
        .any(|k| k.trim().to_lowercase() == key && *k != key)
    {
        return Err(ROTATE_TARGET);
    }
    Ok(())
}
fn rotation_args(
    p: &Project,
    ti: usize,
    pi: usize,
    direction: Direction,
) -> Result<(serde_json::Value, crate::midi_ops::Selection), &'static str> {
    rotation_target(p, ti, pi)?;
    let t = &p.tracks[ti];
    let pat = &p.patterns[pi];
    let notes = pat.clips.get(&t.name.to_lowercase()).ok_or(ROTATE_EMPTY)?;
    if notes.is_empty() {
        return Err(ROTATE_EMPTY);
    }
    let ids: Vec<_> = notes.iter().map(|n| n.id.clone()).collect();
    if ids.iter().any(|id| id.is_empty() || id.trim() != id) {
        return Err(ROTATE_TARGET);
    }
    let args =
        serde_json::json!({"track":t.name,"pattern":pat.name,"ids":ids,"shift":direction.shift()});
    let resolved = crate::ids::resolve_id_refs(p, &args);
    if resolved != args
        || p.track_index(resolved["track"].as_str().ok_or(ROTATE_TARGET)?)
            .ok()
            != Some(ti)
        || p.pattern_index(resolved["pattern"].as_str().ok_or(ROTATE_TARGET)?)
            .ok()
            != Some(pi)
    {
        return Err(ROTATE_TARGET);
    }
    let selection = crate::tools_midi::selection_of(&resolved).map_err(|_| ROTATE_TARGET)?;
    let matched: Vec<_> = notes
        .iter()
        .filter(|n| selection.matches(n))
        .map(|n| n.id.clone())
        .collect();
    if matched != ids || selection.ids != ids {
        return Err(ROTATE_TARGET);
    }
    Ok((args, selection))
}
fn rotation_prediction(
    p: &Project,
    ti: usize,
    pi: usize,
    direction: Direction,
) -> Result<RotationPrediction, &'static str> {
    let (args, selection) = rotation_args(p, ti, pi, direction)?;
    let key = p.tracks[ti].name.to_lowercase();
    let pat = &p.patterns[pi];
    let (bar, extent) = rotation_extent(pat)?;
    let original = pat.clips.get(&key).ok_or(ROTATE_EMPTY)?;
    if original
        .iter()
        .any(|n| !n.start.is_finite() || n.start < 0.0 || n.start >= extent)
    {
        return Err(ROTATE_EXTENT);
    }
    let mut expected = p.clone();
    let lane = expected.patterns[pi]
        .clips
        .get_mut(&key)
        .ok_or(ROTATE_EMPTY)?;
    let affected = crate::midi_ops::rotate_rhythm(lane, &selection, direction.shift(), bar)
        .map_err(|_| ROTATE_EXTENT)?;
    if affected != original.len() || lane.len() != original.len() {
        return Err(ROTATE_TARGET);
    }
    let originals: BTreeMap<_, _> = original.iter().map(|n| (n.id.as_str(), n)).collect();
    if originals.len() != original.len() {
        return Err(ROTATE_TARGET);
    }
    let mut seen = BTreeSet::new();
    let mut changed_starts = 0;
    for n in lane.iter() {
        let old = originals.get(n.id.as_str()).ok_or(ROTATE_TARGET)?;
        if !seen.insert(n.id.as_str())
            || !n.start.is_finite()
            || n.start < 0.0
            || n.start >= extent
            || (old.start / bar).floor() != (n.start / bar).floor()
        {
            return Err(ROTATE_EXTENT);
        }
        let mut start_only = (**old).clone();
        start_only.start = n.start;
        if !start_only.exact(n) {
            return Err(ROTATE_TARGET);
        }
        changed_starts += usize::from(old.start.to_bits() != n.start.to_bits());
    }
    // The actual helper sorted the entire lane stably; replay only that documented
    // sort on ID-associated results and check the exact full single-lane delta.
    let results: BTreeMap<_, _> = lane.iter().map(|n| (n.id.as_str(), n.start)).collect();
    let mut ordered = original.clone();
    for n in &mut ordered {
        n.start = *results.get(n.id.as_str()).ok_or(ROTATE_TARGET)?;
    }
    crate::midi_ops::sort(&mut ordered);
    if !ordered.exact(lane) {
        return Err(ROTATE_TARGET);
    }
    let mut restored = expected.clone();
    *restored.patterns[pi]
        .clips
        .get_mut(&key)
        .ok_or(ROTATE_EMPTY)? = original.clone();
    if !exact_project(p, &restored) {
        return Err(ROTATE_TARGET);
    }
    eligible_project(&expected)?;
    if predicts_playlist_compile(p, &expected) {
        return Err(ROTATE_COMPILED);
    }
    let gate = history_gate(p, &expected);
    if gate == HistoryGate::ContentEqualExactDifferent {
        return Err(ROTATE_EQUAL);
    }
    Ok(RotationPrediction {
        project: expected,
        args,
        changed_starts,
        affected,
        gate,
    })
}
impl RotationCapture {
    fn open(
        e: &crate::engine::Engine,
        visible: &Project,
        ti: usize,
        pi: usize,
    ) -> Result<Self, &'static str> {
        if e.revision == u64::MAX {
            return Err(ROTATE_COUNTER);
        }
        eligible_project(&e.project)?;
        // Visible state must be the actual locked snapshot, not a hint that can
        // silently recapture a changed lane. This explicit-open scan is bounded.
        if !exact_project(&e.project, visible) {
            return Err(ROTATE_EXPIRED);
        }
        rotation_target(&e.project, ti, pi)?;
        Ok(Self {
            project: e.project.clone(),
            revision: e.revision,
            generation: e.project_load_generation,
            ti,
            pi,
        })
    }
    fn current(&self, e: &crate::engine::Engine, ti: usize, pi: usize) -> bool {
        if e.revision != self.revision
            || e.project_load_generation != self.generation
            || ti != self.ti
            || pi != self.pi
        {
            return false;
        }
        let Some(t) = e.project.tracks.get(ti) else {
            return false;
        };
        let Some(p) = e.project.patterns.get(pi) else {
            return false;
        };
        let old_t = &self.project.tracks[self.ti];
        let old_p = &self.project.patterns[self.pi];
        t.id == old_t.id
            && t.name == old_t.name
            && t.instrument.exact(&old_t.instrument)
            && p.id == old_p.id
            && p.name == old_p.name
            && p.bars == old_p.bars
            && p.meter == old_p.meter
            && match (
                p.clips.get(&t.name.to_lowercase()),
                old_p.clips.get(&old_t.name.to_lowercase()),
            ) {
                (None, None) => true,
                (Some(a), Some(b)) => a.exact(b),
                _ => false,
            }
    }
}
// Caller holds ONE real Engine mutex across every live check and the one call.
fn rotation_commit(
    e: &mut crate::engine::Engine,
    capture: &RotationCapture,
    ti: usize,
    pi: usize,
    direction: Direction,
) -> Result<(), String> {
    if e.revision == u64::MAX {
        return Err(ROTATE_COUNTER.into());
    }
    // Eligibility is independently substantive at the commit boundary, even
    // when an unrelated unversioned field has drifted since capture.
    eligible_project(&e.project).map_err(str::to_string)?;
    if !capture.current(e, ti, pi) || !exact_project(&e.project, &capture.project) {
        return Err(ROTATE_EXPIRED.into());
    }
    let prediction = rotation_prediction(&e.project, ti, pi, direction).map_err(str::to_string)?;
    if prediction.gate == HistoryGate::NoChange {
        return Ok(());
    }
    let revision = e.revision;
    let generation = e.project_load_generation;
    let history = e.undo_depth();
    // Failed handler attempts may append a failed log entry. Never retry or
    // compensate, and never describe a dispatched failure as a preflight reject.
    let result = e.call_from("rotate_notes", &prediction.args, "you");
    rotation_dispatched_result(
        e,
        capture,
        direction,
        &prediction,
        (revision, generation, history),
        result,
    )
}

// Shared by the real adapter and genuine Engine-result tests. This consumes a
// dispatched result only; it cannot call, retry, repair or rearm an intent.
fn rotation_dispatched_result(
    e: &crate::engine::Engine,
    capture: &RotationCapture,
    direction: Direction,
    prediction: &RotationPrediction,
    before: (u64, u64, (usize, usize)),
    result: anyhow::Result<serde_json::Value>,
) -> Result<(), String> {
    let (revision, generation, history) = before;
    let ti = capture.ti;
    let pi = capture.pi;
    let response = result
        .map_err(|error| format!("Rhythm edit attempt failed: {error}. {ROTATE_UNCERTAIN}"))?;
    let patterns = response["patterns"].as_array();
    let response_ok = response["affected"].as_u64() == Some(prediction.affected as u64)
        && response["track"].as_str() == Some(capture.project.tracks[ti].name.as_str())
        && response["shift"].as_f64() == Some(direction.shift() as f64)
        && response["revision"].as_u64() == revision.checked_add(1)
        && patterns.is_some_and(|rows| {
            rows.len() == 1
                && rows[0]["pattern"].as_str() == Some(capture.project.patterns[pi].name.as_str())
                && rows[0]["affected"].as_u64() == Some(prediction.affected as u64)
        });
    if !response_ok
        || !exact_project(&e.project, &prediction.project)
        || e.revision != revision + 1
        || e.project_load_generation != generation
        || e.undo_depth() != ((history.0 + 1).min(100), 0)
    {
        return Err(ROTATE_UNCERTAIN.into());
    }
    Ok(())
}

struct RotationPanel {
    capture: RotationCapture,
    previews: [Result<RotationPrediction, &'static str>; 2],
    serial: u64,
    armed: bool,
    error: Option<String>,
    initial_focus: bool,
    keys_released: bool,
}
impl RotationPanel {
    // One-shot private intent consumer, used before validation or a dispatch.
    fn consume(&mut self, serial: u64) -> Result<(), String> {
        if !self.armed || serial != self.serial {
            return Err(ROTATE_EXPIRED.into());
        }
        self.armed = false;
        Ok(())
    }
    fn record_result(&mut self, result: Result<(), String>) -> Result<(), String> {
        // Diagnostics never restore ownership of a consumed intent.
        if let Err(error) = &result {
            self.error = Some(error.clone());
        }
        result
    }
}
#[derive(Clone, Copy)]
enum RotationIntent {
    Cancel,
    Outside,
    Commit(u64, Direction),
}
#[derive(Default)]
pub(super) struct RotationState {
    panel: Option<RotationPanel>,
    serial: u64,
    entry: Option<RotationEntry>,
    controls: Vec<egui::Id>,
    window_layer: Option<egui::LayerId>,
    pending: Option<RotationIntent>,
    owned_time: Option<(egui::ViewportId, u64)>,
}
// Records copied ONLY from a just-rendered real button Response. No Context is
// retained in Context temporary state, preventing a reference cycle.
struct RotationEntry {
    pass: u64,
    viewport: egui::ViewportId,
    id: egui::Id,
    visible: bool,
    enabled: bool,
}

pub(super) fn rotation_state(
    ctx: &egui::Context,
) -> std::sync::Arc<std::sync::Mutex<RotationState>> {
    let id = egui::Id::new("rack-rotation-local-state");
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<std::sync::Arc<std::sync::Mutex<RotationState>>>(id)
            .clone()
    })
}

fn rotation_fresh_key(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.events.iter().any(|event| matches!(event,
        egui::Event::Key { key: egui::Key::Enter | egui::Key::Space, pressed: true, repeat: false, modifiers, .. }
        if !modifiers.alt && !modifiers.ctrl && !modifiers.command && !modifiers.shift)))
}
fn rotation_pointer_release(ctx: &egui::Context) -> bool {
    ctx.input(|i| {
        i.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    ..
                }
            )
        })
    })
}
impl RotationState {
    pub(super) fn active(&self) -> bool {
        self.panel.is_some()
    }
    pub(super) fn owns_frame(&self, ctx: &egui::Context) -> bool {
        self.active()
            || self.owned_time == Some((ctx.viewport_id(), ctx.input(|i| i.time.to_bits())))
    }
    pub(super) fn own_frame(&mut self, ctx: &egui::Context) {
        self.owned_time = Some((ctx.viewport_id(), ctx.input(|i| i.time.to_bits())));
    }
    fn owns_focus(&self, ctx: &egui::Context, id: egui::Id) -> bool {
        self.controls.contains(&id)
            || self
                .window_layer
                .is_some_and(|layer| ctx.read_response(id).is_some_and(|r| r.layer_id == layer))
    }
    // This window has its own layer. Include its genuine close/title controls,
    // not just content Buttons; never surrender another window's/control's focus.
    pub(super) fn cancel(&mut self, ctx: &egui::Context) {
        if let Some(id) = ctx
            .memory(|m| m.focused())
            .filter(|id| self.owns_focus(ctx, *id))
        {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
        self.panel = None;
        self.pending = None;
        self.controls.clear();
        self.window_layer = None;
    }
    fn close_owned(&mut self, ctx: &egui::Context, return_to_entry: bool) {
        let safe = ctx
            .memory(|m| m.focused())
            .is_none_or(|id| self.owns_focus(ctx, id));
        let entry = self
            .entry
            .as_ref()
            .filter(|r| {
                r.pass == ctx.cumulative_pass_nr()
                    && r.viewport == ctx.viewport_id()
                    && r.visible
                    && r.enabled
            })
            .map(|r| r.id);
        self.cancel(ctx);
        if return_to_entry && safe {
            if let Some(id) = entry {
                ctx.memory_mut(|m| m.request_focus(id));
            }
        }
    }
    pub(super) fn open(
        &mut self,
        ctx: &egui::Context,
        e: &crate::engine::Engine,
        visible: &Project,
        ti: usize,
        pi: usize,
    ) -> Result<(), &'static str> {
        self.cancel(ctx);
        self.serial = self
            .serial
            .checked_add(1)
            .filter(|n| *n < u64::MAX)
            .ok_or(ROTATE_COUNTER)?;
        let capture = RotationCapture::open(e, visible, ti, pi)?;
        self.own_frame(ctx);
        let previews = [Direction::Earlier, Direction::Later]
            .map(|d| rotation_prediction(&capture.project, ti, pi, d));
        self.panel = Some(RotationPanel {
            capture,
            previews,
            serial: self.serial,
            armed: true,
            error: None,
            initial_focus: true,
            keys_released: false,
        });
        Ok(())
    }
    // Only the immediately preceding actual render pass can offer keyboard entry
    // masking. No Context::read_response fallback or synthetic widget is used.
    pub(super) fn entry_focused(&self, ctx: &egui::Context) -> bool {
        self.entry.as_ref().is_some_and(|entry| {
            entry.pass.checked_add(1) == Some(ctx.cumulative_pass_nr())
                && entry.viewport == ctx.viewport_id()
                && entry.enabled
                && entry.visible
                && ctx.memory(|m| m.focused()) == Some(entry.id)
        })
    }
    pub(super) fn footer(&mut self, ui: &mut egui::Ui, p: &Project, ti: usize, pi: usize) -> bool {
        if p.tracks.is_empty() || p.patterns.get(pi).is_none() {
            return false;
        }
        let mut open = false;
        let width = ui.clip_rect().width();
        ui.scope(|ui| {
            ui.set_max_width(width);
            ui.horizontal_wrapped(|ui| {
                let reason = match p.tracks.get(ti) {
                    None => Some(ROTATE_EXPIRED),
                    Some(t) if !rotation_drum(t) => Some(ROTATE_DRUM),
                    Some(_) => None,
                };
                let prior_focus = ui.memory(|m| m.focused());
                let response = ui.add_enabled(reason.is_none(), egui::Button::new(ROTATE_ENTRY));
                if response.has_focus() {
                    response.scroll_to_me(None);
                }
                open = (response.clicked_by(egui::PointerButton::Primary)
                    && rotation_pointer_release(ui.ctx()))
                    || (response.clicked()
                        && rotation_fresh_key(ui.ctx())
                        && prior_focus == Some(response.id));
                self.entry = Some(RotationEntry {
                    pass: ui.ctx().cumulative_pass_nr(),
                    viewport: ui.ctx().viewport_id(),
                    id: response.id,
                    visible: response.rect.intersects(response.interact_rect),
                    enabled: response.enabled(),
                });
                ui.add(
                    egui::Label::new(
                        p.tracks
                            .get(ti)
                            .map(|t| format!("Selected channel: {}", t.name))
                            .unwrap_or_else(|| "No channel selected".into()),
                    )
                    .wrap(),
                );
                if let Some(reason) = reason {
                    ui.add(egui::Label::new(reason).wrap());
                }
            });
        });
        open
    }
    // Dialog renders at the pre-global seam. Commit is delayed until this pass's
    // genuine rack entry has rendered, so missing/hidden owners cannot dispatch.
    pub(super) fn prepare(
        &mut self,
        ctx: &egui::Context,
        e: &crate::engine::Engine,
        ti: usize,
        pi: usize,
    ) -> Option<&'static str> {
        let Some(panel) = &mut self.panel else {
            return None;
        };
        let stale = !panel.capture.current(e, ti, pi);
        if stale
            || ctx.input(|i| {
                !i.focused || i.viewport().focused == Some(false) || i.viewport().close_requested()
            })
        {
            self.cancel(ctx);
            return stale.then_some(ROTATE_EXPIRED);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.pending = Some(RotationIntent::Cancel);
            return None;
        }
        let fresh_key = rotation_fresh_key(ctx);
        let pointer_release = rotation_pointer_release(ctx);
        let prior_focus = ctx.memory(|m| m.focused());
        let first = panel.initial_focus;
        let ready = panel.keys_released && !first;
        if !ctx.input(|i| i.key_down(egui::Key::Enter) || i.key_down(egui::Key::Space)) {
            panel.keys_released = true;
        }
        let mut open = true;
        let mut controls = Vec::new();
        let mut intent = None;
        let owner = &panel.capture;
        let t = &owner.project.tracks[owner.ti];
        let pat = &owner.project.patterns[owner.pi];
        let count = pat.notes(&t.name).len();
        let window = egui::Window::new("Rotate rhythm")
            .id(egui::Id::new(("rack-rotate-dialog", panel.serial)))
            .open(&mut open)
            .collapsible(false)
            .default_width(440.0)
            .max_width((ctx.screen_rect().width() - 24.0).max(96.0))
            .max_height((ctx.screen_rect().height() - 24.0).max(96.0))
            .vscroll(true)
            .show(ctx, |ui| {
                ui.add(egui::Label::new(format!("Channel: {}", t.name)).wrap());
                ui.add(egui::Label::new(format!("Pattern: {}", pat.name)).wrap());
                let (n, d) = pat.meter();
                ui.label(format!("Meter: {n}/{d}; {} bars; {count} hits", pat.bars));
                ui.add(
                    egui::Label::new(format!(
                        "All {count} hits in this channel and pattern; each stays in its own bar"
                    ))
                    .wrap(),
                );
                for warning in [ROTATE_SCOPE, ROTATE_SHARED, ROTATE_SORT] {
                    ui.add(egui::Label::new(warning).wrap());
                }
                if count == 0 {
                    ui.label(ROTATE_EMPTY);
                }
                if let Some(error) = &panel.error {
                    ui.add(egui::Label::new(error).wrap());
                }
                if count > 0 {
                    for (direction, prediction) in [Direction::Earlier, Direction::Later]
                        .into_iter()
                        .zip(&panel.previews)
                    {
                        match prediction {
                            Ok(p) => {
                                ui.add(
                                    egui::Label::new(format!(
                                        "{}: {} changed starts by hit identity; {} matched hits",
                                        direction.label(),
                                        p.changed_starts,
                                        p.affected
                                    ))
                                    .wrap(),
                                );
                            }
                            Err(reason) => {
                                ui.add(
                                    egui::Label::new(format!("{}: {reason}", direction.label()))
                                        .wrap(),
                                );
                            }
                        }
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    // Cancel is a real control, initially focused. No default submit.
                    let cancel = ui.button(ROTATE_CANCEL);
                    if first {
                        cancel.request_focus();
                    }
                    if cancel.has_focus() {
                        cancel.scroll_to_me(None);
                    }
                    if (cancel.clicked_by(egui::PointerButton::Primary) && pointer_release)
                        || (cancel.clicked()
                            && ready
                            && fresh_key
                            && prior_focus == Some(cancel.id))
                    {
                        intent = Some(RotationIntent::Cancel);
                    }
                    controls.push(cancel.id);
                    if count > 0 {
                        for (direction, prediction) in [Direction::Earlier, Direction::Later]
                            .into_iter()
                            .zip(&panel.previews)
                        {
                            let enabled = panel.armed && prediction.is_ok();
                            let sense = if enabled {
                                egui::Sense::click()
                            } else {
                                egui::Sense::hover()
                            };
                            let response = ui.add_enabled(
                                enabled,
                                egui::Button::new(direction.label()).sense(sense),
                            );
                            if response.has_focus() {
                                response.scroll_to_me(None);
                            }
                            if enabled
                                && !first
                                && ((response.clicked_by(egui::PointerButton::Primary)
                                    && pointer_release)
                                    || (response.clicked()
                                        && ready
                                        && fresh_key
                                        && prior_focus == Some(response.id)))
                            {
                                intent = Some(RotationIntent::Commit(panel.serial, direction));
                            }
                            controls.push(response.id);
                        }
                    }
                });
            });
        panel.initial_focus = false;
        self.controls = controls;
        self.window_layer = window.as_ref().map(|shown| shown.response.layer_id);
        let outside = window.as_ref().is_some_and(|shown| {
            ctx.input(|i| {
                i.pointer.any_pressed()
                    && i.pointer
                        .interact_pos()
                        .is_some_and(|p| !shown.response.rect.contains(p))
            })
        });
        self.pending = if outside {
            Some(RotationIntent::Outside)
        } else if open && window.is_some() {
            intent
        } else {
            Some(RotationIntent::Cancel)
        };
        None
    }
    pub(super) fn after_workspace(
        &mut self,
        ctx: &egui::Context,
        e: &mut crate::engine::Engine,
        ti: usize,
        pi: usize,
    ) -> Result<(), String> {
        if self.panel.is_none() {
            return Ok(());
        }
        let present = self.entry.as_ref().is_some_and(|entry| {
            entry.pass == ctx.cumulative_pass_nr()
                && entry.viewport == ctx.viewport_id()
                && entry.enabled
                && entry.visible
        });
        if !present
            || !self
                .panel
                .as_ref()
                .is_some_and(|p| p.capture.current(e, ti, pi))
        {
            self.cancel(ctx);
            return Err(ROTATE_EXPIRED.into());
        }
        match self.pending.take() {
            None => Ok(()),
            Some(RotationIntent::Cancel) => {
                self.close_owned(ctx, true);
                Ok(())
            }
            Some(RotationIntent::Outside) => {
                self.close_owned(ctx, false);
                Ok(())
            }
            Some(RotationIntent::Commit(serial, direction)) => {
                let panel = self.panel.as_mut().ok_or(ROTATE_EXPIRED)?;
                // Retire before ANY validation/callback, including failures. A
                // diagnostic panel cannot be a reusable or multipass intent.
                panel.consume(serial)?;
                let next = self.serial.checked_add(1).filter(|n| *n < u64::MAX);
                let result = if let Some(next) = next {
                    self.serial = next;
                    rotation_commit(e, &panel.capture, ti, pi, direction)
                } else {
                    Err(ROTATE_COUNTER.into())
                };
                match panel.record_result(result) {
                    Ok(()) => {
                        self.close_owned(ctx, true);
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::{Effect, FreqShiftFx, GainFx, ParametricEqFx};
    use crate::instruments::{DrumKind, DrumParams, Instrument};
    use crate::project::*;
    use serde_json::{json, Number, Value};

    // Every slot is assigned manually. No fixture calls upkeep or an Engine.
    fn fixture() -> Project {
        let mut p = Project {
            schema_version: crate::ids::SCHEMA_VERSION,
            id_seq: 20,
            master_effects: vec![gain("shared")],
            ..Default::default()
        };
        let mut t = Track::new(
            "drums",
            Instrument::Drum(DrumParams {
                kind: DrumKind::Kick,
                tune: 0.0,
                decay: 1.0,
                drive: 0.0,
                tone: 0.0,
            }),
        );
        t.id = "trk1".into();
        t.effects = vec![gain("shared")];
        t.sends.push(Send {
            id: "snd2".into(),
            bus: "return".into(),
            db: 0.0,
            pre_fader: false,
        });
        p.tracks = vec![t];
        let mut b = Bus::new("return");
        b.id = "bus3".into();
        b.effects = vec![gain("shared")];
        p.buses = vec![b];
        let mut a = Pattern::new("A", 2);
        a.id = "pat4".into();
        let mut n = Note::new(0.0, 1.0, 60, 0.5);
        n.id = "n5".into();
        a.clips.insert("drums".into(), vec![n.clone()]);
        let mut b = Pattern::new("B", 2);
        b.id = "pat6".into();
        n.id = "n7".into();
        b.clips.insert("drums".into(), vec![n]);
        p.patterns = vec![a, b];
        p.playlist = vec![PlaylistClip {
            id: "pl8".into(),
            pattern: "A".into(),
            start_bar: 0,
            bars: 2,
            tracks: vec![],
            lane: 0,
            offset_bars: 0,
        }];
        p.audio_clips = vec![serde_json::from_value(
            json!({"id":"clip9","track":"drums","sample":"take","start_beat":0}),
        )
        .unwrap()];
        p.automation = vec![crate::automation::AutomationLane {
            id: "lane10".into(),
            target: "master".into(),
            param: "volume".into(),
            enabled: true,
            points: vec![crate::automation::AutoPoint {
                beat: 0.0,
                value: 0.5,
                curve: crate::automation::Curve::Linear,
            }],
        }];
        p.automation_clips = vec![AutomationClip {
            id: "ac11".into(),
            name: "fade".into(),
            target: "master".into(),
            param: "volume".into(),
            length_beats: 4.0,
            points: vec![],
            placements: vec![0.0],
        }];
        p.midi = Some(MidiSetup {
            links: vec![MidiLinkSave {
                control: "cc 7".into(),
                target: "master.volume".into(),
            }],
            track: "drums".into(),
            quantize: 1.0,
            gate_s: 0.5,
        });
        p.seed_log = vec![SeedEntry {
            tool: "fixture".into(),
            seed: 1,
            source: "given".into(),
            args: json!({"nested":[null,true,"x",-2,18446744073709551615u64,1.25]}),
            at: 0,
        }];
        p.song = Some(crate::song::SongState {
            spec: json!({"x":[1.25,false]}),
            seed: 1,
            sections: vec![],
            audio: None,
            checks: json!(["ok", null]),
        });
        p.fx_chains.insert(
            "saved".into(),
            vec![Effect::ParametricEq(ParametricEqFx::default())],
        );
        p
    }
    fn gain(id: &str) -> Effect {
        Effect::Gain(GainFx {
            id: id.into(),
            ..Default::default()
        })
    }
    fn note(p: &mut Project) -> &mut Note {
        &mut p.patterns[0].clips.get_mut("drums").unwrap()[0]
    }
    fn reject_unchanged(p: &Project, expected: &'static str) {
        let original = p.clone();
        assert_eq!(eligible_project(p), Err(expected));
        assert!(exact_project(p, &original));
    }

    #[test]
    fn exact_catches_identity_ignored_by_content_equality() {
        let a = fixture();
        let mut b = a.clone();
        note(&mut b).id = "replacement".into();
        assert_eq!(a, b);
        assert!(!exact_project(&a, &b));
    }
    #[test]
    fn exact_catches_signed_zero_vector_order_and_map_presence() {
        let a = fixture();
        let mut b = a.clone();
        note(&mut b).start = -0.0;
        assert_eq!(a, b);
        assert!(!exact_project(&a, &b));
        let mut b = a.clone();
        b.patterns.swap(0, 1);
        assert!(!exact_project(&a, &b));
        let mut b = a.clone();
        b.patterns[0].clips.insert("empty".into(), vec![]);
        assert!(!exact_project(&a, &b));
        let mut c = b.clone();
        c.patterns[0].clips.remove("empty");
        assert!(exact_project(&a, &c));
        let mut b = a.clone();
        let mut n = note(&mut b).clone();
        n.id = "n12".into();
        n.pitch = 61;
        b.patterns[0].clips.get_mut("drums").unwrap().push(n);
        let mut c = b.clone();
        c.patterns[0].clips.get_mut("drums").unwrap().swap(0, 1);
        assert!(!exact_project(&b, &c));
    }
    #[test]
    fn unrelated_saved_fx_nested_band_and_bypass_are_exact() {
        let a = fixture();
        let mut b = a.clone();
        if let Effect::ParametricEq(q) = &mut b.fx_chains.get_mut("saved").unwrap()[0] {
            q.bands[0].q += 0.25;
        }
        assert!(!exact_project(&a, &b));
        let mut b = a.clone();
        if let Effect::Gain(g) = &mut b.buses[0].effects[0] {
            g.bypass = true;
        }
        assert!(!exact_project(&a, &b));
    }
    #[test]
    fn actual_json_variants_numbers_and_nested_key_presence() {
        let f = |x| Value::Number(Number::from_f64(x).unwrap());
        assert!(!f(-0.0).exact(&f(0.0)));
        assert!(!json!(1u64).exact(&f(1.0)));
        assert!(!json!(u64::MAX).exact(&json!(u64::MAX - 1)));
        assert!(!json!(-1i64).exact(&json!(1u64)));
        assert!(!Value::Null.exact(&json!({})));
        let a = json!({"a":[null,true,"x",{"z":f(-0.0)}]});
        let mut b = a.clone();
        b["a"][3]["z"] = f(0.0);
        assert!(!a.exact(&b));
        b = a.clone();
        b["a"][3]["added"] = Value::Null;
        assert!(!a.exact(&b));
        assert!(a.exact(&a.clone()) && a.finite());
        let mut p = fixture();
        p.seed_log[0].args = a.clone();
        p.song.as_mut().unwrap().spec = a;
        assert_eq!(eligible_project(&p), Ok(()));
        let mut q = p.clone();
        q.song.as_mut().unwrap().checks = json!({"new":false});
        assert!(!exact_project(&p, &q));
    }
    #[test]
    fn every_note_float_target_and_unrelated_rejects_nan_and_infinities() {
        let fields: [fn(&mut Note) -> &mut f32; 5] = [
            |n| &mut n.start,
            |n| &mut n.len,
            |n| &mut n.vel,
            |n| &mut n.prob,
            |n| &mut n.offset,
        ];
        for pattern in 0..2 {
            for field in fields {
                for x in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                    let mut p = fixture();
                    *field(&mut p.patterns[pattern].clips.get_mut("drums").unwrap()[0]) = x;
                    assert!(!finite_project(&p));
                    reject_unchanged(&p, NONFINITE);
                }
            }
        }
    }
    #[test]
    fn unrelated_and_nested_nonfinite_parameters_are_not_hidden_by_serde() {
        for x in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut p = fixture();
            p.midi.as_mut().unwrap().gate_s = x;
            reject_unchanged(&p, NONFINITE);
            let mut p = fixture();
            p.automation[0].points[0].value = x;
            reject_unchanged(&p, NONFINITE);
            let mut p = fixture();
            p.audio_clips[0].length_s = Some(x);
            reject_unchanged(&p, NONFINITE);
            let mut p = fixture();
            p.automation_clips[0].placements.push(x);
            reject_unchanged(&p, NONFINITE);
            let mut p = fixture();
            if let Effect::ParametricEq(q) = &mut p.fx_chains.get_mut("saved").unwrap()[0] {
                q.bands[0].freq = x;
            }
            reject_unchanged(&p, NONFINITE);
            let mut p = fixture();
            let mut m = crate::multisample::MultisampleParams::default();
            m.key_velocity_curve.0.insert(60, x);
            p.tracks[0].instrument = Instrument::Multisample(m);
            reject_unchanged(&p, NONFINITE);
            let mut p = fixture();
            p.tracks[0].instrument = Instrument::Synth(crate::instruments::SynthParams {
                amp_env: crate::dsp::Adsr {
                    attack: x,
                    decay: 0.25,
                    sustain: 0.5,
                    release: 0.25,
                },
                ..Default::default()
            });
            reject_unchanged(&p, NONFINITE);
        }
    }
    #[test]
    fn omitted_negative_zero_offset_rejected_but_negative_zero_start_admitted() {
        let mut p = fixture();
        note(&mut p).offset = -0.0;
        assert!(finite_project(&p));
        reject_unchanged(&p, CODEC_CHANGE);
        let mut p = fixture();
        note(&mut p).start = -0.0;
        assert_eq!(eligible_project(&p), Ok(()));
        let q: Project = serde_json::from_slice(&serde_json::to_vec(&p).unwrap()).unwrap();
        assert_eq!(
            q.patterns[0].clips["drums"][0].start.to_bits(),
            (-0.0f32).to_bits()
        );
        assert!(exact_project(&p, &q));
        let mut p = fixture();
        p.tuning_cents = -0.0;
        reject_unchanged(&p, CODEC_CHANGE);
    }
    #[test]
    fn actual_one_sixteenth_rotation_uses_separate_history_gate_for_both_zeros() {
        for start in [0.0f32, -0.0] {
            for shift in [-1.0f32, 1.0] {
                let mut before = fixture();
                before.patterns[0].meter = Some((1, 16));
                note(&mut before).start = start;
                assert_eq!(eligible_project(&before), Ok(()));
                let mut expected = before.clone();
                let affected = crate::midi_ops::rotate_rhythm(
                    expected.patterns[0].clips.get_mut("drums").unwrap(),
                    &crate::midi_ops::Selection::default(),
                    shift,
                    1.0,
                )
                .unwrap();
                assert_eq!(affected, 1);
                assert_eq!(eligible_project(&expected), Ok(()));
                assert_eq!(before, expected);
                // Literal four-case oracle, independent of history_gate and
                // the exact comparator. The real helper above is the witness.
                let (expected_bits, expected_gate) = match (start.to_bits(), shift.to_bits()) {
                    (0x0000_0000, 0xbf80_0000) => (0x0000_0000, HistoryGate::NoChange),
                    (0x0000_0000, 0x3f80_0000) => (0x0000_0000, HistoryGate::NoChange),
                    (0x8000_0000, 0xbf80_0000) => (0x8000_0000, HistoryGate::NoChange),
                    (0x8000_0000, 0x3f80_0000) => {
                        (0x0000_0000, HistoryGate::ContentEqualExactDifferent)
                    }
                    _ => unreachable!(),
                };
                assert_eq!(note(&mut expected).start.to_bits(), expected_bits);
                assert_eq!(history_gate(&before, &expected), expected_gate);
                // Pure helper has no Engine/history handle to consume a retained redo.
                // Full adapter must verify retained redo against the real Engine later.
            }
        }
        let a = fixture();
        let mut b = a.clone();
        note(&mut b).start = 1.0;
        assert_eq!(history_gate(&a, &b), HistoryGate::Changed);
    }
    #[test]
    fn timing_repairs_and_real_effect_validation_are_rejected_not_admitted() {
        for (bpm, swing) in [(0.0, 0.0), (401.0, 0.0), (120.0, -0.25), (120.0, 1.25)] {
            let mut p = fixture();
            p.bpm = bpm;
            p.swing = swing;
            reject_unchanged(&p, LOAD_CHANGE);
        }
        let mut p = fixture();
        p.master_effects = vec![Effect::FreqShift(FreqShiftFx {
            id: "freq1".into(),
            shift_hz: 5001.0,
            ..Default::default()
        })];
        reject_unchanged(&p, LOAD_INVALID);
        let mut p = fixture();
        p.master_effects = vec![Effect::FreqShift(FreqShiftFx {
            id: "freq1".into(),
            ..Default::default()
        })];
        p.automation[0].param = "fx.0.shift_hz".into();
        p.automation[0].points[0].value = 6000.0;
        reject_unchanged(&p, LOAD_INVALID);
    }
    #[test]
    fn custom_multisample_keys_use_real_decode_not_policy_or_normalization() {
        let mut p = fixture();
        let mut m = crate::multisample::MultisampleParams::default();
        m.key_pan.insert(128, 0.5);
        p.tracks[0].instrument = Instrument::Multisample(m);
        reject_unchanged(&p, CODEC_ERROR);
        let mut p = fixture();
        let mut m = crate::multisample::MultisampleParams::default();
        m.key_velocity_curve.0.insert(128, 1.0);
        p.tracks[0].instrument = Instrument::Multisample(m);
        reject_unchanged(&p, CODEC_ERROR);
        let mut p = fixture();
        let mut m = crate::multisample::MultisampleParams::default();
        m.key_pan.insert(127, 0.5);
        m.key_velocity_curve.0.insert(0, 1.25);
        m.reverse_keys.insert(255);
        p.tracks[0].instrument = Instrument::Multisample(m);
        assert_eq!(eligible_project(&p), Ok(()));
    }
    // Mutating only the requested slot lets each missing/duplicate case prove it
    // is visited, not accidentally rejected because another fixture slot is bad.
    fn slot_at(p: &mut Project, i: usize) -> &mut String {
        match i {
            0 => &mut p.tracks[0].id,
            1 => &mut p.tracks[0].sends[0].id,
            2 => &mut p.buses[0].id,
            3 => &mut p.patterns[0].id,
            4 => &mut p.patterns[0].clips.get_mut("drums").unwrap()[0].id,
            5 => &mut p.patterns[1].id,
            6 => &mut p.patterns[1].clips.get_mut("drums").unwrap()[0].id,
            7 => &mut p.playlist[0].id,
            8 => &mut p.audio_clips[0].id,
            9 => &mut p.automation[0].id,
            10 => &mut p.automation_clips[0].id,
            _ => panic!("fixture slot out of range"),
        }
    }
    #[test]
    fn all_global_slots_missing_or_duplicate_reject_before_max_upkeep() {
        assert_eq!(eligible_project(&fixture()), Ok(()));
        for i in 0..11 {
            let mut p = fixture();
            p.id_seq = u64::MAX;
            slot_at(&mut p, i).clear();
            reject_unchanged(&p, BAD_IDS);
            let mut p = fixture();
            p.id_seq = u64::MAX;
            let other = if i == 0 { 1 } else { 0 };
            let id = slot_at(&mut p, other).clone();
            *slot_at(&mut p, i) = id;
            reject_unchanged(&p, BAD_IDS);
        }
    }
    #[test]
    fn fx_scope_is_each_chain_and_saved_chains_do_not_allocate() {
        let mut p = fixture();
        p.id_seq = u64::MAX;
        assert_eq!(eligible_project(&p), Ok(()));
        for i in 0..3 {
            let mut p = fixture();
            p.id_seq = u64::MAX;
            let chain = match i {
                0 => &mut p.master_effects,
                1 => &mut p.tracks[0].effects,
                _ => &mut p.buses[0].effects,
            };
            chain.push(gain("shared"));
            reject_unchanged(&p, BAD_FX_IDS);
            let mut p = fixture();
            p.id_seq = u64::MAX;
            let chain = match i {
                0 => &mut p.master_effects,
                1 => &mut p.tracks[0].effects,
                _ => &mut p.buses[0].effects,
            };
            chain.push(gain(""));
            reject_unchanged(&p, BAD_FX_IDS);
        }
    }
    #[test]
    fn max_counter_is_inert_and_schema_policy_is_current_representation_only() {
        let mut p = fixture();
        p.id_seq = u64::MAX;
        let before = p.clone();
        assert_eq!(eligible_project(&p), Ok(()));
        assert!(exact_project(&p, &before));
        p.schema_version = crate::ids::SCHEMA_VERSION - 1;
        reject_unchanged(&p, BAD_SCHEMA);
        p.schema_version = crate::ids::SCHEMA_VERSION + 1;
        assert_eq!(eligible_project(&p), Ok(()));
        p.bpm = f32::INFINITY;
        reject_unchanged(&p, NONFINITE);
        // Structural errors take precedence over finite rejection as required.
        p.patterns[0].id.clear();
        reject_unchanged(&p, BAD_IDS);
    }
    #[test]
    fn stored_midi_song_and_seed_are_exact_without_live_controller_claim() {
        let a = fixture();
        assert_eq!(eligible_project(&a), Ok(()));
        let mut b = a.clone();
        b.midi.as_mut().unwrap().links[0].target = "different".into();
        assert!(!exact_project(&a, &b));
        let mut b = a.clone();
        b.seed_log[0].args["nested"][0] = json!(0);
        assert!(!exact_project(&a, &b));
        let mut b = a.clone();
        b.song.as_mut().unwrap().spec["added"] = Value::Null;
        assert!(!exact_project(&a, &b));
    }
    #[test]
    fn compiler_prediction_covers_literal_prefix_and_first_name_option_semantics() {
        let mut a = fixture();
        a.arrangement = vec![Section {
            pattern: "pl:0".into(),
            repeats: 1,
        }];
        let mut b = a.clone();
        note(&mut b).start = 1.0;
        assert!(predicts_playlist_compile(&a, &b));
        b.playlist[0].pattern = "a".into();
        assert!(predicts_playlist_compile(&a, &b));
        b.playlist[0].pattern = " A ".into();
        assert!(!predicts_playlist_compile(&a, &b));
        b.playlist[0].pattern = "pat4".into();
        assert!(!predicts_playlist_compile(&a, &b));
        b.playlist[0].pattern = "missing".into();
        assert!(!predicts_playlist_compile(&a, &b));
        b.playlist[0].pattern = "A".into();
        b.patterns[0].name = "renamed".into();
        assert!(predicts_playlist_compile(&a, &b));
        b = a.clone();
        let mut duplicate = b.patterns[0].clone();
        duplicate.clips.get_mut("drums").unwrap()[0].start = 2.0;
        b.patterns.push(duplicate);
        assert!(!predicts_playlist_compile(&a, &b));
        note(&mut b).start = 1.0;
        assert!(predicts_playlist_compile(&a, &b));
        for sections in [
            vec![],
            vec![Section {
                pattern: "PL:0".into(),
                repeats: 1,
            }],
            vec![
                Section {
                    pattern: "pl:0".into(),
                    repeats: 1,
                },
                Section {
                    pattern: "A".into(),
                    repeats: 1,
                },
            ],
        ] {
            b.arrangement = sections;
            assert!(!predicts_playlist_compile(&a, &b));
        }
        b.arrangement = a.arrangement.clone();
        b.playlist.clear();
        assert!(!predicts_playlist_compile(&a, &b));
        // An unrelated source's nonreflexive data still triggers the predicate,
        // but B1 must reject it before either prediction or dispatch.
        let mut p = fixture();
        p.arrangement = a.arrangement;
        p.playlist[0].pattern = "B".into();
        p.patterns[1].clips.get_mut("drums").unwrap()[0].start = f32::NAN;
        assert!(predicts_playlist_compile(&p, &p.clone()));
        reject_unchanged(&p, NONFINITE);
    }

    #[test]
    fn future_engine_dispatch_oracle_for_compiler_prediction_all_unrun() {
        // Authored only. No Engine is constructed by the static delivery process.
        // Observable generated pattern creation is independent of this local
        // predictor: only real call_from dispatch can produce it in this test.
        for case in 0..8 {
            let mut before = fixture();
            before.arrangement = vec![Section {
                pattern: "pl:0".into(),
                repeats: 1,
            }];
            match case {
                0 => {}
                1 => before.arrangement.clear(),
                2 => before.playlist.clear(),
                3 => before.arrangement.push(Section {
                    pattern: "B".into(),
                    repeats: 1,
                }),
                4 => before.arrangement[0].pattern = "PL:0".into(),
                5 => before.playlist[0].pattern = "a".into(),
                6 => before.playlist[0].pattern = " A ".into(),
                7 => before.playlist[0].pattern = "missing".into(),
                _ => unreachable!(),
            }
            let mut expected = before.clone();
            crate::midi_ops::rotate_rhythm(
                expected.patterns[0].clips.get_mut("drums").unwrap(),
                &crate::midi_ops::Selection {
                    ids: vec!["n5".into()],
                    ..Default::default()
                },
                1.0,
                16.0,
            )
            .unwrap();
            let predicted = predicts_playlist_compile(&before, &expected);
            let mut e = crate::engine::Engine::new(std::path::PathBuf::new());
            e.project = before;
            e.call_from(
                "rotate_notes",
                &json!({"track":"drums","pattern":"A","ids":["n5"],"shift":1}),
                "you",
            )
            .unwrap();
            let generated = e
                .project
                .patterns
                .iter()
                .any(|p| p.name.starts_with(crate::tools_fl::PL));
            assert_eq!(predicted, generated, "case {case}");
            if !predicted {
                assert!(exact_project(&expected, &e.project));
            }
        }
    }

    // Same literal lexer as the fixed inventory, duplicated locally so the old
    // 21-source manual inventory stays completely unchanged.
    fn literals(src: &str) -> Vec<String> {
        let mut out = vec![];
        let mut cur: Option<String> = None;
        for line in src.lines() {
            let ch: Vec<char> = line.chars().collect();
            let mut i = 0;
            while i < ch.len() {
                let c = ch[i];
                if let Some(s) = cur.as_mut() {
                    if c == '\\' && i + 1 < ch.len() {
                        s.push(ch[i + 1]);
                        i += 2;
                        continue;
                    }
                    if c == '"' {
                        out.push(cur.take().unwrap());
                    } else {
                        s.push(c);
                    }
                } else {
                    if c == '/' && ch.get(i + 1) == Some(&'/') {
                        break;
                    }
                    if c == '\'' && ch.get(i + 2) == Some(&'\'') {
                        i += 3;
                        continue;
                    }
                    if c == '"' {
                        cur = Some(String::new());
                    }
                }
                i += 1;
            }
            if let Some(s) = cur.as_mut() {
                s.push('\n');
            }
        }
        out
    }
    #[test]
    fn supplemental_manual_guard_scans_all_helper_literals_before_final_boundary() {
        let full = include_str!("rack_rotate.rs");
        let (src, tail) = full.split_once("\n#[cfg(test)]").unwrap();
        assert!(tail.trim_start().starts_with("mod tests {"));
        assert!(!src.contains("#[cfg(test)]"));
        let labels = literals(src);
        for label in [
            BAD_SCHEMA,
            BAD_IDS,
            BAD_FX_IDS,
            NONREFLEXIVE,
            NONFINITE,
            CODEC_ERROR,
            CODEC_CHANGE,
            LOAD_INVALID,
            LOAD_CHANGE,
            UPKEEP_CHANGE,
        ] {
            assert!(
                labels.iter().any(|s| s == label),
                "missing diagnostic: {label}"
            );
        }
        for lit in &labels {
            let ident = !lit.is_empty()
                && lit
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if ident {
                continue;
            }
            let low = lit.to_lowercase();
            let words: Vec<_> = low
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|w| !w.is_empty())
                .collect();
            assert!(
                !words.iter().any(|w| crate::gui_words::BANNED.contains(w)),
                "{lit}"
            );
            for phrase in ["make with", "tool call", "auto-produce", "auto produce"] {
                assert!(!low.contains(phrase), "{lit}");
            }
        }
        let code = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for needle in [
            "{args}\")",
            "{args} ",
            "registry().len()",
            "tools::registry()",
            "e.tool",
            ".summary",
            "e.source",
            "\"YOU\"",
            "\"SYS\"",
            "\"EXT\"",
            "\"PORT",
            "ACTIVITY",
            "to_string_pretty(",
            "summarize_args",
        ] {
            assert!(!code.contains(needle), "{needle}");
        }
        for name in [
            "make_beat",
            "produce_song",
            "critique_mix",
            "generate_beat",
            "sing_lyrics",
            "vocal_to_song",
            "produce_track",
        ] {
            assert!(!src.contains(&format!("\"{name}\"")));
        }
        // No command is dispatched by this dependency. The later production
        // adapter must add registered-action coverage for its actual invocation.
        for pat in ["call(\"", "call_from(\"", "Some((\""] {
            let mut rest = src;
            while let Some(k) = rest.find(pat) {
                rest = &rest[k + pat.len()..];
                let name: String = rest.chars().take_while(|c| *c != '"').collect();
                assert!(
                    crate::tools::find(&name).is_some(),
                    "unregistered action: {name}"
                );
            }
        }
    }

    // Full-feature additions. All Rust/Engine/UI tests are authored, not run here.
    fn rotation_fixture() -> Project {
        let mut p = fixture();
        p.playlist.clear();
        p
    }
    fn rotation_engine(p: Project) -> crate::engine::Engine {
        let mut e = crate::engine::Engine::new(std::path::PathBuf::new());
        e.project = p;
        e
    }
    type RotationObservation = (
        Project,
        u64,
        u64,
        (usize, usize),
        Vec<(String, String, bool, String)>,
        String,
    );
    fn rotation_unchanged(e: &crate::engine::Engine) -> RotationObservation {
        (
            e.project.clone(),
            e.revision,
            e.project_load_generation,
            e.undo_depth(),
            e.log
                .iter()
                .map(|l| (l.tool.clone(), l.summary.clone(), l.ok, l.source.clone()))
                .collect(),
            format!("{:?}", e.transport),
        )
    }
    fn assert_rotation_unchanged(e: &crate::engine::Engine, before: &RotationObservation) {
        assert!(exact_project(&e.project, &before.0));
        assert_eq!(
            (e.revision, e.project_load_generation, e.undo_depth()),
            (before.1, before.2, before.3)
        );
        assert_eq!(
            e.log
                .iter()
                .map(|l| (l.tool.clone(), l.summary.clone(), l.ok, l.source.clone()))
                .collect::<Vec<_>>(),
            before.4
        );
        assert_eq!(format!("{:?}", e.transport), before.5);
    }
    #[test]
    fn rotation_real_adapter_full_project_both_directions_meters_stable_ids_and_history() {
        for meter in [(4, 4), (3, 4), (6, 8), (7, 8)] {
            for direction in [Direction::Earlier, Direction::Later] {
                let mut p = rotation_fixture();
                p.patterns[0].meter = Some(meter);
                let bar = p.patterns[0].steps_per_bar() as f32;
                let mut notes = Vec::new();
                for (i, start) in [bar * 2.0 - 0.25, 0.0, bar, 0.0, bar - 0.5]
                    .into_iter()
                    .enumerate()
                {
                    let mut n = Note::new(
                        start,
                        bar + 3.0,
                        60 + i as u8 % 2,
                        if i == 0 { 0.0 } else { 0.5 },
                    );
                    n.id = format!("n{}", 100 + i);
                    n.prob = if i == 0 { 0.0 } else { 0.5 };
                    n.offset = 0.125;
                    n.slide_to = Some(64);
                    notes.push(n);
                }
                p.patterns[0].clips.insert("drums".into(), notes);
                eligible_project(&p).unwrap();
                let mut e = rotation_engine(p.clone());
                let capture = RotationCapture::open(&e, &p, 0, 0).unwrap();
                let expected = rotation_prediction(&p, 0, 0, direction).unwrap();
                assert_eq!(expected.affected, 5);
                assert_eq!(expected.changed_starts, 5);
                assert!(expected.gate == HistoryGate::Changed);
                let transport = format!("{:?}", e.transport);
                rotation_commit(&mut e, &capture, 0, 0, direction).unwrap();
                assert!(exact_project(&e.project, &expected.project));
                assert_eq!(e.undo_depth(), (1, 0));
                assert_eq!(e.revision, 1);
                assert_eq!(e.log.len(), 1);
                assert_eq!(e.log[0].tool, "rotate_notes");
                assert_eq!(e.log[0].source, "you");
                assert!(e.log[0].ok);
                assert_eq!(format!("{:?}", e.transport), transport);
                e.call_from("undo", &json!({}), "you").unwrap();
                assert!(exact_project(&e.project, &p));
                e.call_from("redo", &json!({}), "you").unwrap();
                assert!(exact_project(&e.project, &expected.project));
            }
        }
    }
    #[test]
    fn rotation_real_b2_one_sixteenth_signed_zero_keeps_redo_and_makes_zero_calls() {
        for start in [0.0f32, -0.0f32] {
            for direction in [Direction::Earlier, Direction::Later] {
                let mut p = rotation_fixture();
                p.patterns[0].meter = Some((1, 16));
                note(&mut p).start = start;
                // Independent four-case oracle for the actual helper, not the
                // adapter's history gate or its Direction::shift implementation.
                let (shift, expected_start_bits, noncommittable) =
                    match (start.to_bits(), direction) {
                        (0x0000_0000, Direction::Earlier) => (-1.0, 0x0000_0000, false),
                        (0x0000_0000, Direction::Later) => (1.0, 0x0000_0000, false),
                        (0x8000_0000, Direction::Earlier) => (-1.0, 0x8000_0000, false),
                        (0x8000_0000, Direction::Later) => (1.0, 0x0000_0000, true),
                        _ => unreachable!(),
                    };
                let mut witness = p.clone();
                assert_eq!(
                    crate::midi_ops::rotate_rhythm(
                        witness.patterns[0].clips.get_mut("drums").unwrap(),
                        &crate::midi_ops::Selection {
                            ids: vec!["n5".into()],
                            ..Default::default()
                        },
                        shift,
                        1.0,
                    )
                    .unwrap(),
                    1
                );
                assert_eq!(note(&mut witness).start.to_bits(), expected_start_bits);
                let mut e = rotation_engine(p.clone());
                e.call_from(
                    "set_note_velocity",
                    &json!({"track":"drums","pattern":"A","ids":["n5"],"velocity":0.25}),
                    "you",
                )
                .unwrap();
                e.call_from("undo", &json!({}), "you").unwrap();
                assert!(exact_project(&e.project, &p));
                assert_eq!(e.undo_depth(), (0, 1));
                let before = rotation_unchanged(&e);
                let capture = RotationCapture::open(&e, &p, 0, 0).unwrap();
                let result = rotation_commit(&mut e, &capture, 0, 0, direction);
                if noncommittable {
                    assert_eq!(result.unwrap_err(), ROTATE_EQUAL);
                } else {
                    assert!(result.is_ok());
                }
                assert_rotation_unchanged(&e, &before);
                assert_eq!(e.log.iter().filter(|l| l.tool == "rotate_notes").count(), 0);
            }
        }
    }
    #[test]
    fn rotation_missing_and_present_empty_maps_remain_distinct_and_never_dispatch() {
        for present in [false, true] {
            let mut p = rotation_fixture();
            if present {
                p.patterns[0].clips.insert("drums".into(), vec![]);
            } else {
                p.patterns[0].clips.remove("drums");
            }
            let mut e = rotation_engine(p.clone());
            let capture = RotationCapture::open(&e, &p, 0, 0).unwrap();
            let before = rotation_unchanged(&e);
            assert_eq!(
                rotation_commit(&mut e, &capture, 0, 0, Direction::Later).unwrap_err(),
                ROTATE_EMPTY
            );
            assert_rotation_unchanged(&e, &before);
            assert_eq!(e.project.patterns[0].clips.contains_key("drums"), present);
        }
    }
    #[test]
    fn rotation_eligibility_at_open_and_commit_rejects_unrelated_invalid_data_without_repair() {
        for case in 0..8 {
            let p = rotation_fixture();
            let mut e = rotation_engine(p.clone());
            let capture = RotationCapture::open(&e, &p, 0, 0).unwrap();
            match case {
                0 => e.project.patterns[1].clips.get_mut("drums").unwrap()[0].start = f32::NAN,
                1 => e.project.patterns[1].clips.get_mut("drums").unwrap()[0].len = f32::INFINITY,
                2 => {
                    e.project.patterns[1].clips.get_mut("drums").unwrap()[0].prob =
                        f32::NEG_INFINITY
                }
                3 => e.project.patterns[1].clips.get_mut("drums").unwrap()[0].offset = -0.0,
                4 => e.project.automation[0].id.clear(),
                5 => e.project.tracks[0].sends[0].id = e.project.patterns[1].id.clone(),
                6 => e.project.schema_version = crate::ids::SCHEMA_VERSION - 1,
                7 => e.project.master_effects[0] = gain(""),
                _ => unreachable!(),
            }
            let before = rotation_unchanged(&e);
            assert!(RotationCapture::open(&e, &e.project, 0, 0).is_err());
            assert!(rotation_commit(&mut e, &capture, 0, 0, Direction::Later).is_err());
            assert_rotation_unchanged(&e, &before);
        }
    }
    #[test]
    fn rotation_counter_limits_valid_max_ids_and_checked_serial() {
        let mut p = rotation_fixture();
        p.id_seq = u64::MAX;
        let mut e = rotation_engine(p.clone());
        let capture = RotationCapture::open(&e, &p, 0, 0).unwrap();
        rotation_commit(&mut e, &capture, 0, 0, Direction::Later).unwrap();
        assert_eq!(e.project.id_seq, u64::MAX);
        assert_eq!(e.project.schema_version, p.schema_version);
        e.revision = u64::MAX;
        let before = rotation_unchanged(&e);
        assert_eq!(
            RotationCapture::open(&e, &e.project, 0, 0).err(),
            Some(ROTATE_COUNTER)
        );
        assert_rotation_unchanged(&e, &before);
        let mut state = RotationState {
            serial: u64::MAX,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        assert_eq!(state.open(&ctx, &e, &e.project, 0, 0), Err(ROTATE_COUNTER));
        assert!(!state.active());
        assert_rotation_unchanged(&e, &before);
        e.revision = 0;
        e.project.tracks[0].id.clear();
        assert!(eligible_project(&e.project).is_err());
        assert_eq!(e.project.id_seq, u64::MAX);
    }
    #[test]
    fn rotation_resolver_agreement_rejects_aliases_collisions_trim_and_default_filter_loss() {
        for case in 0..8 {
            let mut p = rotation_fixture();
            match case {
                0 => {
                    p.patterns[0].clips.insert(" DRUMS ".into(), vec![]);
                }
                1 => p.tracks[0].name = " drums".into(),
                2 => p.tracks[0].name = "drums,other".into(),
                3 => p.buses[0].name = "DRUMS".into(),
                4 => p.patterns[0].name = p.tracks[0].id.clone(),
                5 => note(&mut p).id = " n5 ".into(),
                6 => note(&mut p).pitch = 128,
                7 => note(&mut p).vel = 2.0,
                _ => unreachable!(),
            }
            let e = rotation_engine(p.clone());
            assert!(
                RotationCapture::open(&e, &p, 0, 0).is_err()
                    || rotation_prediction(&p, 0, 0, Direction::Later).is_err()
            );
        }
        let p = rotation_fixture();
        let (args, selection) = rotation_args(&p, 0, 0, Direction::Earlier).unwrap();
        assert_eq!(
            args,
            json!({"track":"drums","pattern":"A","ids":["n5"],"shift":-1.0})
        );
        assert_eq!(selection.ids, vec!["n5"]);
    }
    #[test]
    fn rotation_frozen_owner_lane_bits_selection_and_generation_never_recapture() {
        for case in 0..8 {
            let p = rotation_fixture();
            let mut e = rotation_engine(p.clone());
            let capture = RotationCapture::open(&e, &p, 0, 0).unwrap();
            let (mut ti, mut pi) = (0, 0);
            match case {
                0 => e.revision += 1,
                1 => e.project_load_generation += 1,
                2 => e.project.tracks[0].name = "renamed".into(),
                3 => note(&mut e.project).vel = f32::from_bits(0.5f32.to_bits() + 1),
                4 => e.project.patterns.swap(0, 1),
                5 => e.project.patterns[0].meter = Some((3, 4)),
                6 => ti = 1,
                7 => pi = 1,
                _ => unreachable!(),
            }
            let before = rotation_unchanged(&e);
            assert!(rotation_commit(&mut e, &capture, ti, pi, Direction::Later).is_err());
            assert_rotation_unchanged(&e, &before);
        }
        let p = rotation_fixture();
        let mut e = rotation_engine(p.clone());
        note(&mut e.project).start = 0.25;
        assert_eq!(
            RotationCapture::open(&e, &p, 0, 0).err(),
            Some(ROTATE_EXPIRED)
        );
    }
    #[test]
    fn rotation_compiler_conservatism_preserves_mixed_and_empty_prerequisites() {
        for case in 0..4 {
            let mut p = fixture();
            p.arrangement = vec![Section {
                pattern: "pl:0".into(),
                repeats: 1,
            }];
            match case {
                0 => {}
                1 => p.arrangement.clear(),
                2 => p.arrangement.push(Section {
                    pattern: "B".into(),
                    repeats: 1,
                }),
                3 => p.playlist.clear(),
                _ => unreachable!(),
            }
            let e = rotation_engine(p.clone());
            let capture = RotationCapture::open(&e, &p, 0, 0);
            if case == 0 {
                assert_eq!(capture.err(), Some(ROTATE_COMPILED));
            } else {
                assert!(capture.is_ok());
            }
        }
    }
    #[test]
    fn rotation_real_engine_saturated_history_undo_redo_is_exact_not_blind_depth_plus_one() {
        let mut e = rotation_engine(rotation_fixture());
        for i in 0..100 {
            e.call_from("set_note_velocity", &json!({"track":"drums","pattern":"A","ids":["n5"],"velocity":if i % 2 == 0 {0.25} else {0.75}}), "you").unwrap();
        }
        assert_eq!(e.undo_depth(), (100, 0));
        let p = e.project.clone();
        let capture = RotationCapture::open(&e, &p, 0, 0).unwrap();
        let expected = rotation_prediction(&p, 0, 0, Direction::Earlier).unwrap();
        rotation_commit(&mut e, &capture, 0, 0, Direction::Earlier).unwrap();
        assert_eq!(e.undo_depth(), (100, 0));
        e.call_from("undo", &json!({}), "you").unwrap();
        assert!(exact_project(&e.project, &p));
        e.call_from("redo", &json!({}), "you").unwrap();
        assert!(exact_project(&e.project, &expected.project));
    }
    #[test]
    fn rotation_full_production_labels_and_registered_action_precede_final_test_boundary() {
        let full = include_str!("rack_rotate.rs");
        let (production, tail) = full.split_once("\n#[cfg(test)]").unwrap();
        assert!(tail.trim_start().starts_with("mod tests {"));
        assert!(!production.contains("#[cfg(test)]"));
        let labels = literals(production);
        for label in [
            ROTATE_ENTRY,
            ROTATE_EARLIER,
            ROTATE_LATER,
            ROTATE_CANCEL,
            ROTATE_EMPTY,
            ROTATE_DRUM,
            ROTATE_EXPIRED,
            ROTATE_SCOPE,
            ROTATE_SHARED,
            ROTATE_SORT,
            ROTATE_COMPILED,
            ROTATE_EQUAL,
            ROTATE_UNCERTAIN,
            ROTATE_COUNTER,
            ROTATE_TARGET,
            ROTATE_EXTENT,
            "All {count} hits in this channel and pattern; each stays in its own bar",
            "Channel: {}",
            "Pattern: {}",
            "Meter: {n}/{d}; {} bars; {count} hits",
            "Selected channel: {}",
            "No channel selected",
            "Rotate rhythm",
            "{}: {} changed starts by hit identity; {} matched hits",
            "{}: {reason}",
            "Rhythm edit attempt failed: {error}. {ROTATE_UNCERTAIN}",
        ] {
            assert!(labels.iter().any(|s| s == label), "unscanned {label}");
        }
        assert_eq!(production.matches("call_from(\"rotate_notes\"").count(), 1);
        assert!(crate::tools::find("rotate_notes").unwrap().mutates);
        assert!(
            production.rfind("impl RotationState").unwrap() < full.find("\n#[cfg(test)]").unwrap()
        );
    }

    // G2: finite typed boundary tables. Commit uses a fresh valid capture before
    // injecting each invalid value; the production B1 gate precedes snapshot drift.
    fn boundary_reject(mutator: impl Fn(&mut Project), reason: &'static str) {
        let valid = fixture();
        let mut e = rotation_engine(valid.clone());
        e.call(
            "set_note_velocity",
            &json!({"track":"drums","pattern":"A","ids":["n5"],"velocity":0.25}),
        )
        .unwrap();
        e.call("undo", &json!({})).unwrap();
        assert!(exact_project(&e.project, &valid));
        assert_eq!(e.undo_depth(), (0, 1));
        let capture = RotationCapture::open(&e, &valid, 0, 0).unwrap();
        mutator(&mut e.project);
        let before = rotation_unchanged(&e);
        assert_eq!(eligible_project(&e.project), Err(reason));
        assert_eq!(
            RotationCapture::open(&e, &e.project, 0, 0).err(),
            Some(reason)
        );
        assert_eq!(
            rotation_commit(&mut e, &capture, 0, 0, Direction::Later),
            Err(reason.into())
        );
        assert_rotation_unchanged(&e, &before);
    }
    #[test]
    fn rotation_g2_all_identity_slots_fx_families_schema_and_max_at_both_boundaries() {
        for index in 0..11 {
            for duplicate in [false, true] {
                boundary_reject(
                    |p| {
                        p.id_seq = u64::MAX;
                        let other = if index == 0 { 1 } else { 0 };
                        let replacement = if duplicate {
                            slot_at(p, other).clone()
                        } else {
                            String::new()
                        };
                        *slot_at(p, index) = replacement;
                    },
                    BAD_IDS,
                );
            }
        }
        for family in 0..3 {
            for duplicate in [false, true] {
                boundary_reject(
                    |p| {
                        let chain = match family {
                            0 => &mut p.master_effects,
                            1 => &mut p.tracks[0].effects,
                            _ => &mut p.buses[0].effects,
                        };
                        chain.push(gain(if duplicate { "shared" } else { "" }));
                    },
                    BAD_FX_IDS,
                );
            }
        }
        boundary_reject(
            |p| p.schema_version = crate::ids::SCHEMA_VERSION - 1,
            BAD_SCHEMA,
        );
        for schema in [crate::ids::SCHEMA_VERSION, crate::ids::SCHEMA_VERSION + 1] {
            for seq in [20, u64::MAX] {
                let mut p = fixture();
                p.schema_version = schema;
                p.id_seq = seq;
                // Each live chain intentionally reuses "shared", valid per chain.
                let mut e = rotation_engine(p.clone());
                let c = RotationCapture::open(&e, &p, 0, 0).unwrap();
                let expected = rotation_prediction(&p, 0, 0, Direction::Later).unwrap();
                rotation_commit(&mut e, &c, 0, 0, Direction::Later).unwrap();
                assert!(exact_project(&e.project, &expected.project));
                assert_eq!((e.project.schema_version, e.project.id_seq), (schema, seq));
            }
        }
    }
    #[test]
    fn rotation_g2_every_note_float_and_nested_witnesses_both_boundaries() {
        let fields: [fn(&mut Note) -> &mut f32; 5] = [
            |n| &mut n.start,
            |n| &mut n.len,
            |n| &mut n.vel,
            |n| &mut n.prob,
            |n| &mut n.offset,
        ];
        for pi in 0..2 {
            for field in fields {
                for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                    boundary_reject(
                        |p| *field(&mut p.patterns[pi].clips.get_mut("drums").unwrap()[0]) = value,
                        NONFINITE,
                    );
                }
            }
        }
        let witnesses: [fn(&mut Project, f32); 8] = [
            |p, v| p.midi.as_mut().unwrap().gate_s = v,
            |p, v| p.automation[0].points[0].value = v,
            |p, v| p.audio_clips[0].length_s = Some(v),
            |p, v| p.automation_clips[0].placements.push(v),
            |p, v| {
                if let Effect::ParametricEq(q) = &mut p.fx_chains.get_mut("saved").unwrap()[0] {
                    q.bands[0].freq = v
                }
            },
            |p, v| {
                if let Instrument::Drum(d) = &mut p.tracks[0].instrument {
                    d.decay = v
                }
            },
            |p, v| p.bpm = v,
            |p, v| p.swing = v,
        ];
        for witness in witnesses {
            for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                boundary_reject(|p| witness(p, value), NONFINITE);
            }
        }
        boundary_reject(|p| note(p).offset = -0.0, CODEC_CHANGE);
        boundary_reject(|p| p.bpm = 1.0, LOAD_CHANGE);
        boundary_reject(|p| p.swing = 2.0, LOAD_CHANGE);
        boundary_reject(
            |p| {
                p.master_effects[0] = Effect::FreqShift(FreqShiftFx {
                    id: "shared".into(),
                    shift_hz: 6000.0,
                    ..Default::default()
                });
            },
            LOAD_INVALID,
        );
        boundary_reject(
            |p| {
                p.id_seq = u64::MAX;
                p.patterns[1].clips.get_mut("drums").unwrap()[0].start = f32::NAN;
            },
            NONFINITE,
        );
        // Unrelated playlist-source NaN, target not a source, compiled prerequisite.
        boundary_reject(
            |p| {
                p.playlist[0].pattern = "B".into();
                p.arrangement = vec![Section {
                    pattern: "pl:retained".into(),
                    repeats: 1,
                }];
                p.patterns[1].clips.get_mut("drums").unwrap()[0].start = f32::NAN;
            },
            NONFINITE,
        );
    }
    #[test]
    fn rotation_g3_target_acceptance_real_resolver_and_full_matched_ids() {
        for case in 0..4 {
            let mut p = rotation_fixture();
            match case {
                0 => {}
                1 => {
                    p.tracks[0].name = "7".into();
                    for pat in &mut p.patterns {
                        let lane = pat.clips.remove("drums").unwrap();
                        pat.clips.insert("7".into(), lane);
                    }
                    p.patterns[0].name = "13".into();
                }
                2 => {
                    p.tracks[0].instrument =
                        Instrument::Sampler(crate::instruments::SamplerParams {
                            one_shot: true,
                            ..Default::default()
                        });
                }
                _ => {
                    p.tracks[0].name = "DrUmS".into();
                    p.patterns[0].name = "a".into();
                }
            }
            eligible_project(&p).unwrap();
            let (args, sel) = rotation_args(&p, 0, 0, Direction::Later).unwrap();
            assert_eq!(crate::ids::resolve_id_refs(&p, &args), args);
            assert_eq!(p.track_index(args["track"].as_str().unwrap()).unwrap(), 0);
            assert_eq!(
                p.pattern_index(args["pattern"].as_str().unwrap()).unwrap(),
                0
            );
            let lane = p.patterns[0].notes(&p.tracks[0].name);
            assert_eq!(
                lane.iter()
                    .filter(|n| sel.matches(n))
                    .map(|n| &n.id)
                    .collect::<Vec<_>>(),
                lane.iter().map(|n| &n.id).collect::<Vec<_>>()
            );
            let mut e = rotation_engine(p.clone());
            let c = RotationCapture::open(&e, &p, 0, 0).unwrap();
            let expected = rotation_prediction(&p, 0, 0, Direction::Later).unwrap();
            rotation_commit(&mut e, &c, 0, 0, Direction::Later).unwrap();
            assert!(exact_project(&e.project, &expected.project));
        }
        for case in 0..9 {
            let mut p = rotation_fixture();
            match case {
                0 => note(&mut p).id.clear(),
                1 => p.tracks.clear(),
                2 => p.tracks[0].instrument = Instrument::Synth(Default::default()),
                3 => p.patterns[1].name = "a".into(),
                4 => p.patterns[0].name = " A".into(),
                5 => p.patterns[0].name = "A,B".into(),
                6 => p.buses[0].name = "drums".into(),
                7 => p.tracks[0].name = p.patterns[0].id.clone(),
                _ => {
                    p.patterns[0].clips.insert("DRUMS".into(), vec![]);
                }
            }
            let mut e = rotation_engine(p.clone());
            let before = rotation_unchanged(&e);
            if let Ok(c) = RotationCapture::open(&e, &p, 0, 0) {
                assert!(rotation_commit(&mut e, &c, 0, 0, Direction::Later).is_err());
            }
            assert_rotation_unchanged(&e, &before);
        }
    }
    // The preview/dispatch share this ONE actual-operation oracle table. No
    // replacement rotation arithmetic; by-ID field restoration proves the delta.
    #[test]
    fn rotation_g3_every_bar_edges_fractional_layers_ties_sort_and_f32_neighbors() {
        for meter in [(4, 4), (3, 4), (6, 8), (7, 8), (1, 16)] {
            for direction in [Direction::Earlier, Direction::Later] {
                let mut p = rotation_fixture();
                p.patterns[0].bars = 3;
                p.patterns[0].meter = Some(meter);
                let bar = p.patterns[0].steps_per_bar() as f32;
                let mut lane = Vec::new();
                for b in 0..3 {
                    let lo = b as f32 * bar;
                    let hi = (b + 1) as f32 * bar;
                    for start in [lo, hi - 1.0, lo + 0.25, f32::from_bits(hi.to_bits() - 1)] {
                        let mut n = Note::new(start, bar + 2.0, 60 + (lane.len() % 2) as u8, 0.0);
                        n.id = format!("edge{}", lane.len());
                        n.prob = 0.0;
                        n.offset = 0.125;
                        n.slide_to = Some(64);
                        lane.push(n);
                    }
                    if b > 0 {
                        let mut n = Note::new(f32::from_bits(lo.to_bits() + 1), 0.5, 60, 0.5);
                        n.id = format!("successor{b}");
                        lane.push(n);
                    }
                }
                let mut tie = lane[0].clone();
                tie.id = "tie".into();
                lane.insert(1, tie);
                lane.reverse();
                p.patterns[0].clips.insert("drums".into(), lane);
                eligible_project(&p).unwrap();
                let (args, sel) = rotation_args(&p, 0, 0, direction).unwrap();
                let mut oracle = p.clone();
                let affected = crate::midi_ops::rotate_rhythm(
                    oracle.patterns[0].clips.get_mut("drums").unwrap(),
                    &sel,
                    direction.shift(),
                    bar,
                )
                .unwrap();
                let original = &p.patterns[0].clips["drums"];
                let valid = oracle.patterns[0].clips["drums"].iter().all(|n| {
                    let old = original.iter().find(|o| o.id == n.id).unwrap();
                    n.start >= 0.0
                        && n.start < bar * 3.0
                        && (old.start / bar).floor() == (n.start / bar).floor()
                });
                let prediction = rotation_prediction(&p, 0, 0, direction);
                if !valid {
                    assert_eq!(prediction.err(), Some(ROTATE_EXTENT));
                    continue;
                }
                let gate = history_gate(&p, &oracle);
                if gate == HistoryGate::ContentEqualExactDifferent {
                    assert_eq!(prediction.err(), Some(ROTATE_EQUAL));
                    continue;
                }
                let prediction = prediction.unwrap();
                assert!(exact_project(&prediction.project, &oracle));
                let changed = oracle.patterns[0].clips["drums"]
                    .iter()
                    .filter(|n| {
                        original
                            .iter()
                            .find(|o| o.id == n.id)
                            .unwrap()
                            .start
                            .to_bits()
                            != n.start.to_bits()
                    })
                    .count();
                assert_eq!(
                    (prediction.changed_starts, prediction.affected),
                    (changed, affected)
                );
                assert_eq!(prediction.args, args);
                let mut e = rotation_engine(p.clone());
                let c = RotationCapture::open(&e, &p, 0, 0).unwrap();
                rotation_commit(&mut e, &c, 0, 0, direction).unwrap();
                assert!(exact_project(&e.project, &oracle));
                if gate == HistoryGate::Changed {
                    e.call("undo", &json!({})).unwrap();
                    assert!(exact_project(&e.project, &p));
                    e.call("redo", &json!({})).unwrap();
                    assert!(exact_project(&e.project, &oracle));
                }
            }
        }
    }
    #[test]
    fn rotation_g3_order_only_content_equal_and_noop_keep_actual_redo_branch() {
        for unsorted in [false, true] {
            for direction in [Direction::Earlier, Direction::Later] {
                let mut p = rotation_fixture();
                p.patterns[0].meter = Some((1, 16));
                let mut second = note(&mut p).clone();
                second.id = "n99".into();
                second.pitch = 61;
                let lane = p.patterns[0].clips.get_mut("drums").unwrap();
                lane.push(second);
                if unsorted {
                    lane.reverse();
                }
                let mut e = rotation_engine(p.clone());
                e.call(
                    "set_note_velocity",
                    &json!({"track":"drums","pattern":"A","ids":["n5"],"velocity":0.25}),
                )
                .unwrap();
                e.call("undo", &json!({})).unwrap();
                let before = rotation_unchanged(&e);
                let c = RotationCapture::open(&e, &p, 0, 0).unwrap();
                // Vector order is content-visible for different pitches. The
                // resulting gate, not a preview-count shortcut, controls dispatch.
                let mut oracle = p.clone();
                crate::midi_ops::rotate_rhythm(
                    oracle.patterns[0].clips.get_mut("drums").unwrap(),
                    &crate::midi_ops::Selection::default(),
                    direction.shift(),
                    1.0,
                )
                .unwrap();
                let gate = history_gate(&p, &oracle);
                let r = rotation_commit(&mut e, &c, 0, 0, direction);
                match gate {
                    HistoryGate::NoChange => {
                        r.unwrap();
                        assert_rotation_unchanged(&e, &before);
                    }
                    HistoryGate::ContentEqualExactDifferent => {
                        assert_eq!(r.unwrap_err(), ROTATE_EQUAL);
                        assert_rotation_unchanged(&e, &before);
                    }
                    HistoryGate::Changed => {
                        r.unwrap();
                        assert!(exact_project(&e.project, &oracle));
                        assert_eq!(e.log.len(), before.4.len() + 1);
                    }
                }
            }
        }
        // Equal musical notes differing only in stable IDs: stable sorting is
        // exact-order significant yet never licensed by ordinary equality alone.
        let mut a = rotation_fixture();
        let mut n = note(&mut a).clone();
        n.id = "other-id".into();
        a.patterns[0].clips.get_mut("drums").unwrap().push(n);
        let mut b = a.clone();
        b.patterns[0].clips.get_mut("drums").unwrap().reverse();
        assert_eq!(
            history_gate(&a, &b),
            HistoryGate::ContentEqualExactDifferent
        );
    }
    #[test]
    fn rotation_g3_actual_engine_compiler_oracle_finite_name_and_arrangement_table() {
        for case in 0..12 {
            let mut p = fixture();
            let mut retained = Pattern::new("pl:retained", 2);
            retained.id = "compiled-marker".into();
            p.patterns.push(retained);
            p.arrangement = vec![Section {
                pattern: "pl:retained".into(),
                repeats: 1,
            }];
            match case {
                0 => {}
                1 => p.playlist[0].pattern = "a".into(),
                2 => p.playlist[0].pattern = " A ".into(),
                3 => p.playlist[0].pattern = "pat4".into(),
                4 => p.playlist[0].pattern = "missing".into(),
                5 => p.playlist.clear(),
                6 => p.arrangement.clear(),
                7 => p.arrangement.push(Section {
                    pattern: "A".into(),
                    repeats: 1,
                }),
                8 => p.arrangement[0].pattern = "PL:retained".into(),
                9 => p.playlist[0].pattern = "B".into(),
                10 | 11 => {
                    let mut duplicate = p.patterns[0].clone();
                    duplicate.id = "duplicate-pattern".into();
                    duplicate.clips.get_mut("drums").unwrap()[0].id = "duplicate-note".into();
                    p.patterns.insert(0, duplicate);
                }
                _ => unreachable!(),
            }
            let ti = 0;
            let pi = if case == 10 { 1 } else { 0 };
            let args = json!({"track":"drums","pattern":p.patterns[pi].id,"ids":[p.patterns[pi].clips["drums"][0].id],"shift":1});
            let resolved = crate::ids::resolve_id_refs(&p, &args);
            // Duplicate names resolve by the real tool's first-name rules; this
            // oracle deliberately rotates the first matching lane, not a fiction.
            let actual_pi = p
                .pattern_index(resolved["pattern"].as_str().unwrap())
                .unwrap();
            let mut expected = p.clone();
            let bar = expected.patterns[actual_pi].steps_per_bar() as f32;
            let sel = crate::tools_midi::selection_of(&resolved).unwrap();
            crate::midi_ops::rotate_rhythm(
                expected.patterns[actual_pi].clips.get_mut("drums").unwrap(),
                &sel,
                1.0,
                bar,
            )
            .unwrap();
            let predicted = predicts_playlist_compile(&p, &expected);
            let mut e = rotation_engine(p.clone());
            e.call_from("rotate_notes", &args, "you").unwrap();
            if predicted {
                assert!(!exact_project(&e.project, &expected));
                assert!(!e.project.patterns.iter().any(|p| p.id == "compiled-marker"));
            } else {
                assert!(exact_project(&e.project, &expected));
            }
            // Boundary rejects compiled targets (including valid MAX) or naming
            // ambiguity, while untouched sources with noncompiled use can commit.
            let mut guarded = rotation_engine(p.clone());
            guarded.project.id_seq = u64::MAX;
            let before = rotation_unchanged(&guarded);
            let c = RotationCapture::open(&guarded, &guarded.project, ti, pi);
            if predicted || case >= 10 {
                assert!(c.is_err());
                assert_rotation_unchanged(&guarded, &before);
            }
        }
    }
    #[test]
    fn rotation_g4_real_dispatched_failure_and_mismatched_success_use_production_consumer() {
        for failed in [true, false] {
            let p = rotation_fixture();
            let mut e = rotation_engine(p.clone());
            let c = RotationCapture::open(&e, &p, 0, 0).unwrap();
            let mut expected = rotation_prediction(&p, 0, 0, Direction::Later).unwrap();
            let before = rotation_unchanged(&e);
            let ctx = egui::Context::default();
            let mut state = RotationState::default();
            state.open(&ctx, &e, &p, 0, 0).unwrap();
            let panel = state.panel.as_mut().unwrap();
            let serial = panel.serial;
            panel.consume(serial).unwrap();
            assert!(!panel.armed);
            let args = if failed {
                json!({"track":"drums","pattern":"A","ids":["n5"],"shift":1e100})
            } else {
                expected.args.clone()
            };
            let result = e.call_from("rotate_notes", &args, "you");
            assert_eq!(result.is_err(), failed);
            if !failed {
                expected.affected += 1;
            }
            let outcome = rotation_dispatched_result(
                &e,
                &c,
                Direction::Later,
                &expected,
                (before.1, before.2, before.3),
                result,
            );
            let outcome = panel.record_result(outcome);
            assert!(outcome.unwrap_err().contains(ROTATE_UNCERTAIN));
            assert!(!panel.armed);
            assert!(panel.error.as_ref().unwrap().contains(ROTATE_UNCERTAIN));
            assert!(panel.consume(serial).is_err());
            assert!(!panel.armed);
            assert_eq!(e.log.len(), before.4.len() + 1);
            assert_eq!(e.log.last().unwrap().ok, !failed);
            if failed {
                assert!(exact_project(&e.project, &p));
                assert_eq!((e.revision, e.undo_depth()), (before.1, before.3));
            }
            // No second call, retry or compensation is present in the consumer.
            assert_eq!(e.log.iter().filter(|l| l.tool == "rotate_notes").count(), 1);
        }
    }
    #[test]
    fn rotation_g4_persistence_is_separate_real_midi_snapshots_and_failed_file_semantics() {
        let dir = std::env::temp_dir().join(format!(
            "rotate-persistence-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("project.beatbox.json");
        let mut e = rotation_engine(rotation_fixture());
        let p = e.project.clone();
        let c = RotationCapture::open(&e, &p, 0, 0).unwrap();
        rotation_commit(&mut e, &c, 0, 0, Direction::Later).unwrap();
        e.call("snapshot", &json!({"name":"rotation"})).unwrap();
        let midi = crate::midi_in::snapshot(&e);
        let mut expected = e.project.clone();
        expected.midi = midi;
        let generation = e.project_load_generation;
        let history = e.undo_depth();
        let snapshots = e.snapshots.clone();
        let revision = e.revision;
        e.call("save_project", &json!({"path":path})).unwrap();
        assert!(exact_project(&e.project, &expected));
        assert_eq!(e.undo_depth(), history);
        let saved: Project = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(exact_project(&saved, &expected));
        e.call("load_project", &json!({"path":path})).unwrap();
        assert_eq!(e.project_load_generation, generation + 1);
        assert_eq!(e.snapshots.len(), 1);
        assert_eq!(
            (e.revision, e.undo_depth()),
            (revision + 1, (history.0 + 1, 0))
        );
        for ((name, snap), (old_name, old)) in e.snapshots.iter().zip(&snapshots) {
            assert_eq!(
                (name, &snap.note, snap.revision),
                (old_name, &old.note, old.revision)
            );
            assert!(exact_project(&snap.project, &old.project));
        }
        let mut musical = e.project.clone();
        musical.midi = expected.midi.clone();
        assert!(exact_project(&musical, &expected));
        let stored = e.project.midi.clone();
        let generation = e.project_load_generation;
        let current = e.project.clone();
        let snapshots = e.snapshots.len();
        assert!(e
            .call("load_project", &json!({"path":dir.join("absent")}))
            .is_err());
        assert!(exact_project(&e.project, &current));
        assert_eq!(
            (e.project_load_generation, e.snapshots.len()),
            (generation, snapshots)
        );
        // A directory is an actual write failure even for root. Save's MIDI
        // snapshot precedes write and is intentionally NOT asserted unchanged.
        let expected_midi = crate::midi_in::snapshot(&e);
        let mut failed_expected = current.clone();
        failed_expected.midi = expected_midi;
        assert!(e.call("save_project", &json!({"path":dir})).is_err());
        assert!(exact_project(&e.project, &failed_expected));
        assert_eq!(e.project_load_generation, generation);
        assert!(stored.exact(&current.midi));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rotation_g2_revision_limits_and_extra_nested_finite_witnesses() {
        let p = rotation_fixture();
        let mut e = rotation_engine(p.clone());
        let c = RotationCapture::open(&e, &p, 0, 0).unwrap();
        e.revision = u64::MAX;
        let before = rotation_unchanged(&e);
        assert_eq!(
            rotation_commit(&mut e, &c, 0, 0, Direction::Later),
            Err(ROTATE_COUNTER.into())
        );
        assert_rotation_unchanged(&e, &before);
        let clean = rotation_engine(rotation_fixture());
        let unchanged = rotation_unchanged(&clean);
        let mut exhausted = RotationState {
            serial: u64::MAX,
            ..Default::default()
        };
        assert_eq!(
            exhausted.open(&egui::Context::default(), &clean, &clean.project, 0, 0),
            Err(ROTATE_COUNTER)
        );
        assert!(!exhausted.active());
        assert_rotation_unchanged(&clean, &unchanged);
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            boundary_reject(
                |p| {
                    let mut m = crate::multisample::MultisampleParams::default();
                    m.key_velocity_curve.0.insert(60, value);
                    p.tracks[0].instrument = Instrument::Multisample(m);
                },
                NONFINITE,
            );
            boundary_reject(
                |p| {
                    p.tracks[0].instrument = Instrument::Synth(crate::instruments::SynthParams {
                        amp_env: crate::dsp::Adsr {
                            attack: value,
                            decay: 0.25,
                            sustain: 0.5,
                            release: 0.25,
                        },
                        ..Default::default()
                    });
                },
                NONFINITE,
            );
            boundary_reject(
                |p| {
                    if let Effect::Gain(g) = &mut p.buses[0].effects[0] {
                        g.db = value;
                    }
                },
                NONFINITE,
            );
        }
    }
    #[test]
    fn rotation_g4_saturation_with_retained_redo_and_exact_whole_project_history() {
        let mut e = rotation_engine(rotation_fixture());
        for i in 0..101 {
            e.call("set_note_velocity",&json!({"track":"drums","pattern":"A","ids":["n5"],"velocity":if i%2==0 {0.25}else{0.75}})).unwrap();
        }
        assert_eq!(e.undo_depth(), (100, 0));
        e.call("undo", &json!({})).unwrap();
        assert_eq!(e.undo_depth(), (99, 1));
        let before = e.project.clone();
        let c = RotationCapture::open(&e, &before, 0, 0).unwrap();
        let expected = rotation_prediction(&before, 0, 0, Direction::Earlier).unwrap();
        let revision = e.revision;
        let logs = e.log.len();
        rotation_commit(&mut e, &c, 0, 0, Direction::Earlier).unwrap();
        assert_eq!(
            (e.undo_depth(), e.revision, e.log.len()),
            ((100, 0), revision + 1, logs + 1)
        );
        assert!(exact_project(&e.project, &expected.project));
        e.call("undo", &json!({})).unwrap();
        assert!(exact_project(&e.project, &before));
        e.call("redo", &json!({})).unwrap();
        assert!(exact_project(&e.project, &expected.project));
    }

    #[test]
    fn rotation_g2_max_compiler_risk_drift_rejects_without_dispatch_or_upkeep_repair() {
        let mut p = fixture();
        p.id_seq = u64::MAX;
        let mut e = rotation_engine(p.clone());
        let c = RotationCapture::open(&e, &p, 0, 0).unwrap();
        e.project.arrangement = vec![Section {
            pattern: "pl:retained".into(),
            repeats: 1,
        }];
        let before = rotation_unchanged(&e);
        assert_eq!(eligible_project(&e.project), Ok(()));
        assert_eq!(
            RotationCapture::open(&e, &e.project, 0, 0).err(),
            Some(ROTATE_COMPILED)
        );
        // This is intentionally snapshot-expiry, not a false claim that the
        // later compiler gate ran through an already changed full snapshot.
        assert_eq!(
            rotation_commit(&mut e, &c, 0, 0, Direction::Later),
            Err(ROTATE_EXPIRED.into())
        );
        assert_rotation_unchanged(&e, &before);
        assert_eq!(e.project.id_seq, u64::MAX);
    }

    mod rotation_ui_tests {
        use super::super::super::{audio_check, piano, player::Player, workspace, QuitCloseState};
        use super::*;
        use crate::engine::Engine;
        use eframe::egui::{Pos2, Rect, Vec2};
        use std::sync::{Arc, Mutex};
        fn rotation_studio(
            ctx: &egui::Context,
            bars: u32,
            tracks: usize,
        ) -> super::super::super::Studio {
            let mut engine = Engine::new(std::env::temp_dir().join(format!(
                "beatbox-empty-pattern-fixture-{}",
                std::process::id()
            )));
            engine
                .call(
                    "new_project",
                    &json!({"patterns":[{"name":"A","bars":bars}]}),
                )
                .unwrap();
            for i in 0..tracks {
                engine.call("add_track", &json!({"name":format!("row{i}"),"preset":if i % 2 == 0 {"kick"} else {"pluck_lead"}})).unwrap();
            }
            // Future execution may open/start idle audio output via Player::new.
            // Setup add_track may render calibration notes in memory on cache miss.
            // Passive frames must preserve project/transport/player state below;
            // they do not issue play, seek, audition, poll or launch Studio.
            super::super::super::Studio {
                saved_rev: 0,
                seen_project_load: 0,
                confirm_new: false,
                quit_close: QuitCloseState::default(),
                mixer_routing: false,
                open_list: false,
                pending_open: None,
                engine: Arc::new(Mutex::new(engine)),
                player: Player::new(),
                project_path: None,
                listen: String::new(),
                server_ok: false,
                show_about: false,
                export: Default::default(),
                pattern: 0,
                selected: 0,
                step_velocity: Default::default(),
                rendered: None,
                rendering: None,
                render_state: Default::default(),
                live: None,
                live_applied: false,
                inspector: None,
                dragging: false,
                toast: None,
                render_job: None,
                // The existing header uses this flag only for its initial open
                // default. Remove the empty opener next frame; no report is injected.
                mix_readout: None,
                mix_job: None,
                mix_readout_revision: None,
                audio_check: audio_check::State::default(),
                mix_comparison: None,
                mix_comparison_job: None,
                comparison_start_s: 0.0,
                comparison_length_s: 8.0,
                screenshot: None,
                native_capture: None,
                arrangement_capture: None,
                p0a_capture: None,
                frames: 0,
                shot_requested: false,
                perf: None,
                perf_next: None,
                perf_rec: None,
                view: 0,
                auto_sel: None,
                piano: piano::PianoState {
                    snap: 2,
                    ..Default::default()
                },
                vocal_wave: None,
                browser: Default::default(),
                analyze: Default::default(),
                clip_sel: None,
                marker_name: String::new(),
                tempo_edit_bpm: 120.0,
                tempo_edit_ramp: false,
                rail_search: String::new(),
                note_editor: false,
                workspace: workspace::Workspace::capture(ctx, 0),
            }
        }

        struct Frame {
            actions: Vec<super::super::super::shortcuts::Action>,
            output: egui::FullOutput,
            entry: Option<egui::Response>,
            controls: Vec<egui::Response>,
        }
        fn key(key: egui::Key, pressed: bool) -> egui::Event {
            egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }
        }
        fn pointer(pos: Pos2, pressed: bool) -> Vec<egui::Event> {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        }
        fn frame(
            ctx: &egui::Context,
            s: &mut super::super::super::Studio,
            dpi: f32,
            events: Vec<egui::Event>,
            multipass: bool,
        ) -> Frame {
            let first = ctx.cumulative_pass_nr();
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(960.0, 1100.0))),
                focused: true,
                time: Some(ctx.input(|i| i.time) + 1.0 / 60.0),
                events,
                ..Default::default()
            };
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .native_pixels_per_point = Some(dpi);
            let state = rotation_state(ctx);
            let active_at_start = state.lock().unwrap().active();
            let velocity_at_start = s.step_velocity.draft.is_some();
            let p = s.engine.lock().unwrap().project.clone();
            let mut actions = Vec::new();
            let mut entry = None;
            let mut controls = Vec::new();
            let output = ctx.run(input, |ctx| {
                widget_metadata(ctx, true);
                let pass_actions = s.rotation_shortcuts(ctx, velocity_at_start, active_at_start);
                for action in &pass_actions {
                    s.shortcut(*action, &p);
                }
                actions.extend(pass_actions);
                egui::CentralPanel::default().show(ctx, |ui| s.sequencer(ui, &p));
                {
                    let rotation = state.lock().unwrap();
                    entry = rotation
                        .entry
                        .as_ref()
                        .filter(|r| r.pass == ctx.cumulative_pass_nr())
                        .map(|r| ctx.read_response(r.id).expect("real current-pass footer"));
                    // prepare rendered these real Buttons in THIS pass. Reading
                    // only their exact registered IDs cannot select an old control.
                    controls = rotation
                        .controls
                        .iter()
                        .map(|id| {
                            ctx.read_response(*id)
                                .expect("real current-pass dialog Button")
                        })
                        .collect();
                }
                {
                    let mut rotation = state.lock().unwrap();
                    let mut e = s.engine.lock().unwrap();
                    let _result = rotation.after_workspace(ctx, &mut e, s.selected, s.pattern);
                }
                if multipass && ctx.cumulative_pass_nr() == first {
                    ctx.request_discard("rotation authored multipass case");
                }
                // Metadata has already been registered. Suppress end_pass debug
                // overlays so paint/clip assertions see production galleys only.
                widget_metadata(ctx, false);
            });
            Frame {
                actions,
                output,
                entry,
                controls,
            }
        }
        // egui 0.29.1 registers WidgetInfo only with debug assertions and this
        // opt-in. Non-debug builds must fail explicitly, never skip acceptance.
        fn widget_metadata(ctx: &egui::Context, enabled: bool) {
            #[cfg(debug_assertions)]
            ctx.style_mut(|style| style.debug.show_interactive_widgets = enabled);
            #[cfg(not(debug_assertions))]
            {
                let _ = (ctx, enabled);
                panic!("Rotate UI acceptance requires egui debug_assertions for real WidgetInfo");
            }
        }
        fn setup(dpi: f32) -> (egui::Context, super::super::super::Studio) {
            let ctx = egui::Context::default();
            widget_metadata(&ctx, true);
            let mut s = rotation_studio(&ctx, 2, 0);
            s.engine = Arc::new(Mutex::new(rotation_engine(rotation_fixture())));
            frame(&ctx, &mut s, dpi, vec![], false);
            frame(&ctx, &mut s, dpi, vec![], false);
            (ctx, s)
        }
        #[test]
        fn rotation_ordinary_pointer_open_initial_cancel_fresh_key_commit_and_multipass_once() {
            for dpi in [1.0, 2.0] {
                let (ctx, mut s) = setup(dpi);
                let entry = frame(&ctx, &mut s, dpi, vec![], false).entry.unwrap();
                let before = rotation_unchanged(&s.engine.lock().unwrap());
                for pressed in [true, false] {
                    frame(
                        &ctx,
                        &mut s,
                        dpi,
                        pointer(entry.rect.center(), pressed),
                        false,
                    );
                }
                let opened = frame(&ctx, &mut s, dpi, vec![], false);
                assert_eq!(opened.controls.len(), 3);
                assert!(opened.controls[0].has_focus());
                assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
                // Initial released input arms only later fresh focused activation.
                let released = frame(&ctx, &mut s, dpi, vec![], false);
                let later = &released.controls[2];
                ctx.memory_mut(|m| m.request_focus(later.id));
                let result = frame(&ctx, &mut s, dpi, vec![key(egui::Key::Enter, true)], true);
                assert!(result.actions.is_empty());
                let e = s.engine.lock().unwrap();
                assert_eq!(e.log.iter().filter(|l| l.tool == "rotate_notes").count(), 1);
                assert_eq!(e.undo_depth(), (1, 0));
                assert_eq!(e.revision, before.1 + 1);
                let expected = rotation_prediction(&before.0, 0, 0, Direction::Later).unwrap();
                assert!(exact_project(&e.project, &expected.project));
                drop(e);
                let mut e = s.engine.lock().unwrap();
                e.call("undo", &json!({})).unwrap();
                assert!(exact_project(&e.project, &before.0));
                e.call("redo", &json!({})).unwrap();
                assert!(exact_project(&e.project, &expected.project));
                assert!(!rotation_state(&ctx).lock().unwrap().active());
            }
        }
        #[test]
        fn rotation_keyboard_open_held_repeat_cancel_and_closing_global_keys_are_isolated() {
            for dpi in [1.0, 2.0] {
                for activation in [egui::Key::Space, egui::Key::Enter] {
                    let (ctx, mut s) = setup(dpi);
                    let entry = frame(&ctx, &mut s, dpi, vec![], false).entry.unwrap();
                    ctx.memory_mut(|m| m.request_focus(entry.id));
                    let before = rotation_unchanged(&s.engine.lock().unwrap());
                    assert!(frame(&ctx, &mut s, dpi, vec![key(activation, true)], false)
                        .actions
                        .is_empty());
                    assert!(rotation_state(&ctx).lock().unwrap().active());
                    for _ in 0..2 {
                        assert!(frame(&ctx, &mut s, dpi, vec![key(activation, true)], false)
                            .actions
                            .is_empty());
                    }
                    assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
                    frame(&ctx, &mut s, dpi, vec![key(activation, false)], false);
                    let close = frame(
                        &ctx,
                        &mut s,
                        dpi,
                        vec![key(egui::Key::Escape, true), key(egui::Key::Home, true)],
                        true,
                    );
                    assert!(close.actions.is_empty());
                    assert!(!rotation_state(&ctx).lock().unwrap().active());
                    assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
                }
            }
        }
        #[test]
        fn rotation_real_dialog_full_warning_paint_and_controls_at_both_native_scales() {
            for dpi in [1.0, 2.0] {
                let (ctx, mut s) = setup(dpi);
                let entry = frame(&ctx, &mut s, dpi, vec![], false).entry.unwrap();
                for pressed in [true, false] {
                    frame(
                        &ctx,
                        &mut s,
                        dpi,
                        pointer(entry.rect.center(), pressed),
                        false,
                    );
                }
                frame(&ctx, &mut s, dpi, vec![], false);
                let visible = frame(&ctx, &mut s, dpi, vec![], false);
                for label in [
                    ROTATE_SCOPE,
                    ROTATE_SHARED,
                    ROTATE_SORT,
                    ROTATE_EARLIER,
                    ROTATE_LATER,
                    ROTATE_CANCEL,
                ] {
                    assert!(visible.output.shapes.iter().any(|shape| matches!(&shape.shape,
                        egui::Shape::Text(text) if text.galley.text() == label
                            && shape.clip_rect.expand(1.0 / dpi).contains_rect(Rect::from_min_size(text.pos, text.galley.size())))), "unpainted or clipped {label}");
                }
                for response in &visible.controls {
                    assert!(response.interact_rect.contains_rect(response.rect));
                }
            }
        }
        // Real workspace harness: same pre-global, DockArea/Viewer, finish order
        // as update, without native Frame, render pump or an audio launch.
        struct WorkspaceFrame {
            frame: Frame,
            widgets: Vec<(egui::Response, Option<egui::WidgetInfo>)>,
            finish_errors: Vec<String>,
        }
        fn workspace_frame(
            ctx: &egui::Context,
            s: &mut super::super::super::Studio,
            size: Vec2,
            dpi: f32,
            events: Vec<egui::Event>,
            focused: bool,
            discard: bool,
        ) -> WorkspaceFrame {
            let first = ctx.cumulative_pass_nr();
            let time = ctx.input(|i| i.time) + 1.0 / 30.0;
            let mut raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                time: Some(time),
                events,
                focused,
                ..Default::default()
            };
            let viewport = raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
            viewport.native_pixels_per_point = Some(dpi);
            viewport.inner_rect = raw.screen_rect;
            viewport.focused = Some(focused);
            let state = rotation_state(ctx);
            let active = state.lock().unwrap().active();
            let velocity = s.step_velocity.draft.is_some();
            let (p, rev, undo, log) = {
                let e = s.engine.lock().unwrap();
                (
                    e.project.clone(),
                    e.revision,
                    e.undo_depth(),
                    (
                        e.log.iter().map(|l| l.summary.clone()).collect(),
                        e.log.iter().map(|l| l.source.clone()).collect(),
                    ),
                )
            };
            let mut actions = Vec::new();
            let mut entry = None;
            let mut controls = Vec::new();
            let mut widgets = Vec::new();
            let mut finish_errors = Vec::new();
            let output = ctx.run(raw, |ctx| {
                widget_metadata(ctx, true);
                assert_eq!(
                    ctx.input(|i| i.time).to_bits(),
                    time.to_bits(),
                    "discard must retain the explicit real-cycle timestamp"
                );
                s.project_lifecycle_ui(ctx);
                let current = s.rotation_shortcuts(ctx, velocity, active);
                for action in &current {
                    s.shortcut(*action, &p);
                }
                actions.extend(current);
                s.workspace_ui(ctx, &p, rev, &log, undo);
                {
                    let state = state.lock().unwrap();
                    entry = state
                        .entry
                        .as_ref()
                        .filter(|r| r.pass == ctx.cumulative_pass_nr())
                        .and_then(|r| ctx.read_response(r.id));
                    controls = state
                        .controls
                        .iter()
                        .filter_map(|id| ctx.read_response(*id))
                        .collect();
                }
                let ids = ctx.viewport(|v| {
                    v.this_pass
                        .widgets
                        .layers()
                        .flat_map(|(_, ws)| ws.iter())
                        .map(|w| (w.id, v.this_pass.widgets.info(w.id).cloned()))
                        .collect::<Vec<_>>()
                });
                widgets = ids
                    .into_iter()
                    .filter_map(|(id, info)| ctx.read_response(id).map(|r| (r, info)))
                    .collect();
                assert!(
                    widgets.iter().any(|(_, info)| info
                        .as_ref()
                        .is_some_and(|i| i.typ == egui::WidgetType::Button && i.label.is_some())),
                    "real workspace must populate labeled Button metadata"
                );
                for response in entry.iter().chain(controls.iter()) {
                    assert!(
                        widgets.iter().any(|(r, info)| r.id == response.id
                            && info.as_ref().is_some_and(
                                |i| i.typ == egui::WidgetType::Button && i.label.is_some()
                            )),
                        "real Rotate entry/control must populate Button metadata"
                    );
                }
                let mut rotation = state.lock().unwrap();
                let mut e = s.engine.lock().unwrap();
                if let Err(error) = rotation.after_workspace(ctx, &mut e, s.selected, s.pattern) {
                    finish_errors.push(error);
                }
                if discard && ctx.cumulative_pass_nr() == first {
                    ctx.request_discard("Rotate explicit-time workspace acceptance");
                }
                // register_widget_info has populated this pass. Disable only
                // debug painting before end_pass; next discard pass re-enables
                // collection before rendering. Never credit overlay label text.
                widget_metadata(ctx, false);
            });
            WorkspaceFrame {
                frame: Frame {
                    actions,
                    output,
                    entry,
                    controls,
                },
                widgets,
                finish_errors,
            }
        }
        fn wf(
            ctx: &egui::Context,
            s: &mut super::super::super::Studio,
            events: Vec<egui::Event>,
        ) -> WorkspaceFrame {
            workspace_frame(ctx, s, Vec2::new(1400.0, 1100.0), 1.0, events, true, false)
        }
        fn label_control(f: &WorkspaceFrame, label: &str) -> egui::Response {
            f.widgets
                .iter()
                .find(|(r, info)| {
                    r.enabled()
                        && info.as_ref().and_then(|i| i.label.as_deref()) == Some(label)
                        && r.sense.click
                })
                .map(|(r, _)| r.clone())
                .unwrap_or_else(|| panic!("missing actual enabled control {label}"))
        }
        fn click_response(
            ctx: &egui::Context,
            s: &mut super::super::super::Studio,
            r: &egui::Response,
        ) -> WorkspaceFrame {
            assert!(r.interact_rect.is_positive());
            let point = r.interact_rect.center();
            wf(ctx, s, pointer(point, true));
            wf(ctx, s, pointer(point, false))
        }
        fn click_label(
            ctx: &egui::Context,
            s: &mut super::super::super::Studio,
            label: &str,
        ) -> WorkspaceFrame {
            let f = wf(ctx, s, vec![]);
            let r = label_control(&f, label);
            click_response(ctx, s, &r)
        }
        fn open_real(ctx: &egui::Context, s: &mut super::super::super::Studio) -> WorkspaceFrame {
            let f = wf(ctx, s, vec![]);
            let entry = f.frame.entry.expect("actual workspace rack footer");
            click_response(ctx, s, &entry);
            wf(ctx, s, vec![]);
            let opened = wf(ctx, s, vec![]);
            assert!(rotation_state(ctx).lock().unwrap().active());
            assert!(opened.frame.controls[0].has_focus());
            opened
        }
        // This is a saved-layout INPUT, not a fabricated response or production
        // hook. Workspace::load performs its real serde/restore path. Its config
        // path is process-isolated by the test's single subprocess, not an env
        // mutation shared with parallel tests. No subprocess is run during authoring.
        fn layout_file(dock: &egui_dock::DockState<workspace::Pane>) -> std::path::PathBuf {
            let root = std::path::PathBuf::from(
                std::env::var_os("BEATBOX_ROTATE_LAYOUT_SANDBOX").expect("isolated layout worker"),
            );
            let path = root.join("beatbox/studio-layout-v1.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                serde_json::to_vec(
                    &json!({"version":1,"dock":dock,"pinned_transport":true,"mode":0}),
                )
                .unwrap(),
            )
            .unwrap();
            path
        }
        fn dock_for(case: usize) -> egui_dock::DockState<workspace::Pane> {
            use egui_dock::{DockState, NodeIndex};
            use workspace::Pane;
            let mut dock = DockState::new(vec![Pane::Sequencer]);
            if case == 1 || case == 2 {
                dock.main_surface_mut()
                    .split_left(NodeIndex::root(), 0.24, vec![Pane::Library]);
            }
            if case == 3 || case == 4 {
                dock = DockState::new(vec![Pane::Library]);
                let float = dock.add_window(vec![Pane::Sequencer]);
                dock.get_window_state_mut(float)
                    .unwrap()
                    .set_position(Pos2::new(350.0, 400.0))
                    .set_size(Vec2::new(850.0, 550.0));
            }
            dock
        }
        fn workspace_setup(case: usize) -> (egui::Context, super::super::super::Studio) {
            let (ctx, mut s) = setup(1.0);
            s.engine.lock().unwrap().workdir = std::path::PathBuf::from(
                std::env::var_os("BEATBOX_ROTATE_LAYOUT_SANDBOX").unwrap(),
            );
            if case == 0 {
                s.workspace = workspace::Workspace::capture(&ctx, 0);
            } else {
                layout_file(&dock_for(case));
                s.workspace = workspace::Workspace::load(&ctx, 0);
            }
            for _ in 0..3 {
                wf(&ctx, &mut s, vec![]);
            }
            if case == 2 || case == 4 {
                s.workspace.flush().unwrap();
                s.workspace = workspace::Workspace::load(&ctx, 0);
                for _ in 0..3 {
                    wf(&ctx, &mut s, vec![]);
                }
            }
            (ctx, s)
        }
        fn player_snapshot(s: &super::super::super::Studio) -> (bool, u64, f64, f32, bool, String) {
            (
                s.player.is_playing(),
                s.player.seek_revision,
                s.player.abs_position_secs(),
                s.player.position_secs(),
                s.player.auditioning(),
                format!("{:?}", s.engine.lock().unwrap().transport),
            )
        }
        fn assert_player_editor_inert(
            s: &super::super::super::Studio,
            b: &(bool, u64, f64, f32, bool, String),
        ) {
            let a = player_snapshot(s);
            assert_eq!((a.0, a.1, a.4, &a.5), (b.0, b.1, b.4, &b.5));
            if b.0 {
                assert!(a.2 >= b.2, "natural playback clock cannot run backwards");
            } else {
                assert_eq!(
                    (a.2.to_bits(), a.3.to_bits()),
                    (b.2.to_bits(), b.3.to_bits())
                );
            }
        }
        fn tab_event(shift: bool, pressed: bool) -> egui::Event {
            let mut modifiers = egui::Modifiers::NONE;
            modifiers.shift = shift;
            egui::Event::Key {
                key: egui::Key::Tab,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            }
        }
        fn workspace_topology_and_foreign_focus_cases() {
            for case in 0..5 {
                let (ctx, mut s) = workspace_setup(case);
                let before = rotation_unchanged(&s.engine.lock().unwrap());
                let player = player_snapshot(&s);
                let f = open_real(&ctx, &mut s);
                assert_eq!(f.frame.controls.len(), 3);
                assert!(f.finish_errors.is_empty());
                let texts = painted_texts(&f.frame.output, 1.0);
                for label in [
                    ROTATE_SCOPE,
                    ROTATE_SHARED,
                    ROTATE_SORT,
                    ROTATE_EARLIER,
                    ROTATE_LATER,
                    ROTATE_CANCEL,
                ] {
                    assert!(
                        texts.iter().any(|t| t == label),
                        "topology {case} clipped {label}"
                    );
                }
                let entry = f.frame.entry.as_ref().unwrap();
                assert_eq!(ctx.viewport_id(), egui::ViewportId::ROOT);
                assert_eq!(
                    rotation_state(&ctx)
                        .lock()
                        .unwrap()
                        .entry
                        .as_ref()
                        .unwrap()
                        .viewport,
                    egui::ViewportId::ROOT
                );
                assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
                assert_player_editor_inert(&s, &player);
                let cancel = f.frame.controls[0].clone();
                click_response(&ctx, &mut s, &cancel);
                assert!(!rotation_state(&ctx).lock().unwrap().active());
                assert_eq!(ctx.memory(|m| m.focused()), Some(entry.id));
                if case == 1 || case == 2 {
                    let f = open_real(&ctx, &mut s);
                    let search = f
                        .widgets
                        .iter()
                        .find(|(_, info)| {
                            info.as_ref()
                                .is_some_and(|i| i.typ == egui::WidgetType::TextEdit)
                        })
                        .map(|(r, _)| r.clone())
                        .expect("real Library search TextEdit");
                    // This pointer press is processed in prepare before the actual
                    // Library TextEdit acquires focus during workspace rendering.
                    click_response(&ctx, &mut s, &search);
                    assert_eq!(ctx.memory(|m| m.focused()), Some(search.id));
                    wf(&ctx, &mut s, vec![egui::Event::Text("kick".into())]);
                    assert_eq!(s.rail_search, "kick");
                    assert_eq!(ctx.memory(|m| m.focused()), Some(search.id));
                    assert!(!rotation_state(&ctx).lock().unwrap().active());
                    assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
                    assert_player_editor_inert(&s, &player);
                }
            }
        }
        fn workspace_pending_visibility_and_id_cases() {
            use egui_dock::{DockState, NodeIndex};
            use workspace::Pane;
            for case in 0..6 {
                let (ctx, mut s) = workspace_setup(1);
                let f = open_real(&ctx, &mut s);
                let old_entry = f.frame.entry.unwrap().id;
                let before = rotation_unchanged(&s.engine.lock().unwrap());
                let state = rotation_state(&ctx);
                // Set a real user intent through the focused direction, then
                // alter the actual dock before finish in the same pass below.
                let later = f.frame.controls[2].clone();
                ctx.memory_mut(|m| m.request_focus(later.id));
                let raw = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 1100.0))),
                    time: Some(ctx.input(|i| i.time) + 1.0 / 30.0),
                    focused: true,
                    events: vec![key(egui::Key::Enter, true)],
                    ..Default::default()
                };
                let _output = ctx.run(raw, |ctx| {
                    let actions = s.rotation_shortcuts(ctx, false, true);
                    assert!(actions.is_empty());
                    assert!(matches!(
                        state.lock().unwrap().pending,
                        Some(RotationIntent::Commit(_, Direction::Later))
                    ));
                    let mut dock = match case {
                        0 => DockState::new(vec![Pane::Mixer]),   // switch
                        1 => DockState::new(vec![Pane::Library]), // hide/remove
                        2 => DockState::new(vec![Pane::Library, Pane::Sequencer]), // inactive rack tab
                        3 => DockState::new(vec![]), // remove entire rack
                        _ => dock_for(3),            // moved/floating, new real widget IDs
                    };
                    if case == 5 {
                        dock = DockState::new(vec![Pane::Sequencer]);
                        dock.main_surface_mut().split_right(
                            NodeIndex::root(),
                            0.5,
                            vec![Pane::Library],
                        );
                    }
                    layout_file(&dock);
                    s.workspace = workspace::Workspace::load(ctx, 0);
                    let p = s.engine.lock().unwrap().project.clone();
                    let undo = s.engine.lock().unwrap().undo_depth();
                    s.workspace_ui(ctx, &p, before.1, &(vec![], vec![]), undo);
                    let mut rotation = state.lock().unwrap();
                    let mut e = s.engine.lock().unwrap();
                    let new_id = rotation
                        .entry
                        .as_ref()
                        .filter(|r| r.pass == ctx.cumulative_pass_nr())
                        .map(|r| r.id);
                    if case >= 4 {
                        assert_ne!(
                            new_id,
                            Some(old_entry),
                            "move must exercise a real ID change"
                        );
                    }
                    // Current implementation allows a still-visible moved entry
                    // only if its captured identity remains valid. A missing one
                    // must expire without committing a stale response.
                    let present = rotation.entry.as_ref().is_some_and(|r| {
                        r.pass == ctx.cumulative_pass_nr()
                            && r.viewport == ctx.viewport_id()
                            && r.visible
                            && r.enabled
                    });
                    let expected = rotation_prediction(&before.0, 0, 0, Direction::Later).unwrap();
                    let result = rotation.after_workspace(ctx, &mut e, s.selected, s.pattern);
                    if case < 4 || !present {
                        assert_eq!(result, Err(ROTATE_EXPIRED.into()));
                        assert!(!rotation.active());
                        assert_rotation_unchanged(&e, &before);
                    } else {
                        result.unwrap();
                        assert!(!rotation.active());
                        assert!(exact_project(&e.project, &expected.project));
                        assert_eq!(
                            (e.revision, e.undo_depth(), e.log.len()),
                            (before.1 + 1, (before.3 .0 + 1, 0), before.4.len() + 1)
                        );
                        assert_eq!(e.log.last().unwrap().tool, "rotate_notes");
                    }
                });
                if case < 4 {
                    assert_ne!(ctx.memory(|m| m.focused()), Some(old_entry));
                }
            }
        }
        fn workspace_input_focus_lifecycle_cases() {
            // Fresh actual keyboard entry, Tab/Shift+Tab and window chrome.
            for activation in [egui::Key::Enter, egui::Key::Space] {
                let (ctx, mut s) = workspace_setup(1);
                let f = wf(&ctx, &mut s, vec![]);
                let entry = f.frame.entry.unwrap();
                ctx.memory_mut(|m| m.request_focus(entry.id));
                let before = rotation_unchanged(&s.engine.lock().unwrap());
                workspace_frame(
                    &ctx,
                    &mut s,
                    Vec2::new(1400.0, 1100.0),
                    1.0,
                    vec![key(activation, true)],
                    true,
                    true,
                );
                assert!(rotation_state(&ctx).lock().unwrap().active());
                let mut repeated = key(activation, true);
                if let egui::Event::Key { repeat, .. } = &mut repeated {
                    *repeat = true;
                }
                wf(&ctx, &mut s, vec![repeated]);
                assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
                wf(&ctx, &mut s, vec![key(activation, false)]);
                let f = wf(&ctx, &mut s, vec![]);
                assert!(f.frame.controls[0].has_focus());
                wf(&ctx, &mut s, vec![tab_event(false, true)]);
                let f = wf(&ctx, &mut s, vec![tab_event(false, false)]);
                assert_eq!(ctx.memory(|m| m.focused()), Some(f.frame.controls[1].id));
                wf(&ctx, &mut s, vec![tab_event(true, true)]);
                let f = wf(&ctx, &mut s, vec![tab_event(true, false)]);
                assert_eq!(ctx.memory(|m| m.focused()), Some(f.frame.controls[0].id));
                // Observe the genuine close widget in this window's layer; do
                // not guess an ID/rectangle from old geometry or draw a fake X.
                let layer = rotation_state(&ctx).lock().unwrap().window_layer.unwrap();
                let content: Vec<_> = f.frame.controls.iter().map(|r| r.id).collect();
                let chrome = f
                    .widgets
                    .iter()
                    .filter(|(r, _)| {
                        r.layer_id == layer && r.sense.click && !content.contains(&r.id)
                    })
                    .min_by(|(a, _), (b, _)| a.rect.area().total_cmp(&b.rect.area()))
                    .map(|(r, _)| r.clone())
                    .expect("actual Window close widget");
                click_response(&ctx, &mut s, &chrome);
                assert!(!rotation_state(&ctx).lock().unwrap().active());
                assert_eq!(ctx.memory(|m| m.focused()), Some(entry.id));
                assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
            }
            for direction in 1..=2 {
                for pointer_activation in [false, true] {
                    let (ctx, mut s) = workspace_setup(1);
                    let f = open_real(&ctx, &mut s);
                    let before = s.engine.lock().unwrap().project.clone();
                    let expected = rotation_prediction(
                        &before,
                        0,
                        0,
                        if direction == 1 {
                            Direction::Earlier
                        } else {
                            Direction::Later
                        },
                    )
                    .unwrap();
                    let player = player_snapshot(&s);
                    let r = f.frame.controls[direction].clone();
                    if pointer_activation {
                        click_response(&ctx, &mut s, &r);
                    } else {
                        // Real Tab traversal from initial Cancel, not a fake
                        // focused Response or direct-memory direction request.
                        for _ in 0..direction {
                            wf(&ctx, &mut s, vec![tab_event(false, true)]);
                            wf(&ctx, &mut s, vec![tab_event(false, false)]);
                        }
                        assert_eq!(ctx.memory(|m| m.focused()), Some(r.id));
                        workspace_frame(
                            &ctx,
                            &mut s,
                            Vec2::new(1400.0, 1100.0),
                            1.0,
                            vec![key(egui::Key::Enter, true)],
                            true,
                            true,
                        );
                    }
                    assert!(exact_project(
                        &s.engine.lock().unwrap().project,
                        &expected.project
                    ));
                    assert_eq!(s.engine.lock().unwrap().undo_depth(), (1, 0));
                    assert_player_editor_inert(&s, &player);
                    click_label(&ctx, &mut s, "Undo");
                    assert!(exact_project(&s.engine.lock().unwrap().project, &before));
                    click_label(&ctx, &mut s, "Redo");
                    assert!(exact_project(
                        &s.engine.lock().unwrap().project,
                        &expected.project
                    ));
                }
            }
            // Serial exhaustion at consumption disarms BEFORE validation. Repeated
            // passes of the genuine focused input may not retry/rearm that panel.
            let (ctx, mut s) = workspace_setup(1);
            let f = open_real(&ctx, &mut s);
            let before = rotation_unchanged(&s.engine.lock().unwrap());
            rotation_state(&ctx).lock().unwrap().serial = u64::MAX;
            click_response(&ctx, &mut s, &f.frame.controls[2]);
            {
                let state = rotation_state(&ctx);
                let state = state.lock().unwrap();
                assert!(!state.panel.as_ref().unwrap().armed);
                assert_eq!(
                    state.panel.as_ref().unwrap().error.as_deref(),
                    Some(ROTATE_COUNTER)
                );
            }
            for _ in 0..2 {
                wf(&ctx, &mut s, vec![key(egui::Key::Enter, true)]);
            }
            assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
            // OS blur and actual expired close passes keep globals suppressed,
            // then release + next distinct-time Home resumes ordinary input.
            for blur in [false, true] {
                let (ctx, mut s) = workspace_setup(1);
                open_real(&ctx, &mut s);
                let before = rotation_unchanged(&s.engine.lock().unwrap());
                let player = player_snapshot(&s);
                let events = if blur {
                    vec![key(egui::Key::Home, true)]
                } else {
                    vec![key(egui::Key::Escape, true), key(egui::Key::Home, true)]
                };
                let close = workspace_frame(
                    &ctx,
                    &mut s,
                    Vec2::new(1400.0, 1100.0),
                    1.0,
                    events,
                    !blur,
                    true,
                );
                assert!(close.frame.actions.is_empty());
                assert!(!rotation_state(&ctx).lock().unwrap().active());
                assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
                assert_player_editor_inert(&s, &player);
                wf(
                    &ctx,
                    &mut s,
                    vec![key(egui::Key::Home, false), key(egui::Key::Escape, false)],
                );
                if let Some(id) = ctx.memory(|m| m.focused()) {
                    ctx.memory_mut(|m| m.surrender_focus(id));
                }
                let next = wf(&ctx, &mut s, vec![key(egui::Key::Home, true)]);
                assert!(next
                    .frame
                    .actions
                    .contains(&super::super::super::shortcuts::Action::SeekStart));
                assert_eq!(s.player.seek_revision, player.1 + 1);
            }
        }
        fn workspace_version_and_project_ui_cases() {
            for case in 0..9 {
                let (ctx, mut s) = workspace_setup(1);
                open_real(&ctx, &mut s);
                match case {
                    0 => s.engine.lock().unwrap().project.tracks[0].name = "renamed".into(),
                    1 => s.engine.lock().unwrap().project.patterns.swap(0, 1),
                    2 => s.engine.lock().unwrap().project.tracks.clear(),
                    3 => s.pattern = 1,
                    4 => s.engine.lock().unwrap().project.patterns[0].meter = Some((3, 4)),
                    5 => note(&mut s.engine.lock().unwrap().project).start = 0.25,
                    6 => s.engine.lock().unwrap().revision += 1,
                    7 => s.engine.lock().unwrap().project_load_generation += 1,
                    _ => {
                        let mut e = s.engine.lock().unwrap();
                        e.call(
                            "set_note_velocity",
                            &json!({"track":"drums","pattern":"A","ids":["n5"],"velocity":0.25}),
                        )
                        .unwrap();
                    }
                }
                let before = rotation_unchanged(&s.engine.lock().unwrap());
                let f = workspace_frame(
                    &ctx,
                    &mut s,
                    Vec2::new(1400.0, 1100.0),
                    1.0,
                    vec![key(egui::Key::Enter, true), key(egui::Key::Home, true)],
                    true,
                    true,
                );
                assert!(f.frame.actions.is_empty());
                assert!(!rotation_state(&ctx).lock().unwrap().active());
                assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
            }
            for failed in [false, true] {
                let (ctx, mut s) = workspace_setup(1);
                let dir = s.engine.lock().unwrap().workdir.clone();
                let path = if failed {
                    dir.clone()
                } else {
                    dir.join("lifecycle.beatbox.json")
                };
                s.project_path = Some(path.clone());
                open_real(&ctx, &mut s);
                let before = s.engine.lock().unwrap().project.clone();
                let snapshot = crate::midi_in::snapshot(&s.engine.lock().unwrap());
                // Outside Save button cancels Rotate, but is still an intentional
                // production UI action, including failed-write MIDI replacement.
                click_label(&ctx, &mut s, "Save");
                let mut expected = before;
                expected.midi = snapshot;
                assert!(exact_project(&s.engine.lock().unwrap().project, &expected));
                assert!(!rotation_state(&ctx).lock().unwrap().active());
                if !failed {
                    // Actual File > Open > project selection uses production
                    // lifecycle dialogs, extracted byte-for-byte from update.
                    open_real(&ctx, &mut s);
                    click_label(&ctx, &mut s, "File");
                    click_label(&ctx, &mut s, "Open project...");
                    click_label(&ctx, &mut s, "lifecycle");
                    wf(&ctx, &mut s, vec![]);
                    assert!(!rotation_state(&ctx).lock().unwrap().active());
                    assert!(s.engine.lock().unwrap().project_load_generation > 0);
                    let mut musical = s.engine.lock().unwrap().project.clone();
                    musical.midi = expected.midi.clone();
                    assert!(exact_project(&musical, &expected));
                    // Existing filename which cannot parse: actual open picker.
                    std::fs::write(dir.join("broken.beatbox.json"), "not a project").unwrap();
                    open_real(&ctx, &mut s);
                    click_label(&ctx, &mut s, "File");
                    click_label(&ctx, &mut s, "Open project...");
                    click_label(&ctx, &mut s, "broken");
                    wf(&ctx, &mut s, vec![]);
                    assert!(!rotation_state(&ctx).lock().unwrap().active());
                    assert!(s.toast.as_ref().unwrap().0.contains("Could not open"));
                }
                open_real(&ctx, &mut s);
                click_label(&ctx, &mut s, "File");
                click_label(&ctx, &mut s, "New project");
                if s.confirm_new {
                    click_label(&ctx, &mut s, "Discard changes");
                }
                wf(&ctx, &mut s, vec![]);
                assert!(!rotation_state(&ctx).lock().unwrap().active());
                assert!(s.engine.lock().unwrap().project.tracks.is_empty());
                assert_eq!(
                    s.engine
                        .lock()
                        .unwrap()
                        .log
                        .iter()
                        .filter(|l| l.tool == "rotate_notes")
                        .count(),
                    0
                );
            }
        }
        fn workspace_combined_and_mutual_cases() {
            use super::super::super::shortcuts::Action;
            for k in [
                egui::Key::Home,
                egui::Key::Space,
                egui::Key::M,
                egui::Key::S,
            ] {
                // Inactive pointer open is later than ordinary globals. Compare
                // to the real shortcut's actual independent effect, not a claim
                // that the editor retroactively suppresses already dispatched input.
                let (ctx, mut s) = workspace_setup(1);
                let f = wf(&ctx, &mut s, vec![]);
                let entry = f.frame.entry.unwrap();
                wf(&ctx, &mut s, pointer(entry.rect.center(), true));
                let before = rotation_unchanged(&s.engine.lock().unwrap());
                let player = player_snapshot(&s);
                let mut events = pointer(entry.rect.center(), false);
                events.push(key(k, true));
                let f = wf(&ctx, &mut s, events);
                let expected = match k {
                    egui::Key::Home => Action::SeekStart,
                    egui::Key::Space => Action::PlayStop,
                    egui::Key::M => Action::MuteSelected,
                    _ => Action::SoloSelected,
                };
                assert!(f.frame.actions.contains(&expected));
                match k {
                    egui::Key::Home => {
                        assert_eq!(s.player.seek_revision, player.1 + 1);
                        assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
                    }
                    egui::Key::Space => {
                        assert_eq!(s.player.is_playing(), !player.0);
                        assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
                    }
                    _ => {
                        let e = s.engine.lock().unwrap();
                        assert_eq!(e.revision, before.1 + 1);
                        assert_eq!(e.undo_depth(), (before.3 .0 + 1, 0));
                        assert!(!rotation_state(&ctx).lock().unwrap().active());
                    }
                }
                assert_eq!(
                    s.engine
                        .lock()
                        .unwrap()
                        .log
                        .iter()
                        .filter(|l| l.tool == "rotate_notes")
                        .count(),
                    0
                );
                if s.player.is_playing() {
                    s.player.stop();
                }
                // Active pointer intent + the same key suppresses globals; only
                // the intentional Rotate contributes one revision/log/history.
                let (ctx, mut s) = workspace_setup(1);
                let f = open_real(&ctx, &mut s);
                let later = f.frame.controls[2].clone();
                let before = s.engine.lock().unwrap().project.clone();
                let expected = rotation_prediction(&before, 0, 0, Direction::Later).unwrap();
                let player = player_snapshot(&s);
                wf(&ctx, &mut s, pointer(later.rect.center(), true));
                let mut events = pointer(later.rect.center(), false);
                events.push(key(k, true));
                let f = workspace_frame(
                    &ctx,
                    &mut s,
                    Vec2::new(1400.0, 1100.0),
                    1.0,
                    events,
                    true,
                    true,
                );
                assert!(f.frame.actions.is_empty());
                assert!(exact_project(
                    &s.engine.lock().unwrap().project,
                    &expected.project
                ));
                assert_player_editor_inert(&s, &player);
            }
            // Keyboard-open + independent key is owned before globals.
            let (ctx, mut s) = workspace_setup(1);
            let f = wf(&ctx, &mut s, vec![]);
            ctx.memory_mut(|m| m.request_focus(f.frame.entry.unwrap().id));
            let player = player_snapshot(&s);
            let f = wf(
                &ctx,
                &mut s,
                vec![key(egui::Key::Enter, true), key(egui::Key::M, true)],
            );
            assert!(f.frame.actions.is_empty());
            assert!(!s.engine.lock().unwrap().project.tracks[0].mute);
            assert_player_editor_inert(&s, &player);
            // Both orders use actual footer buttons; simultaneous keyboard intent
            // on Rotate plus velocity pointer still leaves only one owner.
            for order in 0..3 {
                let (ctx, mut s) = workspace_setup(1);
                if order == 0 {
                    click_label(&ctx, &mut s, "Step velocity...");
                    assert!(s.step_velocity.draft.is_some());
                    click_label(&ctx, &mut s, ROTATE_ENTRY);
                } else if order == 1 {
                    open_real(&ctx, &mut s);
                    click_label(&ctx, &mut s, "Step velocity...");
                } else {
                    let f = wf(&ctx, &mut s, vec![]);
                    let rotation = f.frame.entry.as_ref().unwrap();
                    let velocity = label_control(&f, "Step velocity...");
                    wf(&ctx, &mut s, pointer(velocity.rect.center(), true));
                    ctx.memory_mut(|m| m.request_focus(rotation.id));
                    let mut events = pointer(velocity.rect.center(), false);
                    events.push(key(egui::Key::Enter, true));
                    wf(&ctx, &mut s, events);
                }
                let rotation = rotation_state(&ctx).lock().unwrap().active();
                let velocity = s.step_velocity.draft.is_some();
                assert_ne!(rotation, velocity, "exactly one real editor owns the draft");
                assert_eq!(
                    s.engine
                        .lock()
                        .unwrap()
                        .log
                        .iter()
                        .filter(|l| l.tool == "rotate_notes")
                        .count(),
                    0
                );
            }
        }
        #[test]
        fn rotation_g5_g7_real_workspace_finite_topology_focus_lifecycle_input_transport() {
            // Public Workspace::load reads process environment. Keep all real
            // restore/persist tests in an isolated executable invocation rather
            // than racing the rest of the old default-feature suite's HOME.
            if std::env::var_os("BEATBOX_ROTATE_LAYOUT_SANDBOX").is_none() {
                let dir =
                    std::env::temp_dir().join(format!("rotate-layout-{}", std::process::id()));
                assert!(!dir.exists());
                std::fs::create_dir(&dir).unwrap();
                let module = module_path!().split_once("::").unwrap().1;
                let name=format!("{module}::rotation_g5_g7_real_workspace_finite_topology_focus_lifecycle_input_transport");
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", &name, "--nocapture", "--test-threads=1"])
                    .env("XDG_CONFIG_HOME", &dir)
                    .env("BEATBOX_ROTATE_LAYOUT_SANDBOX", &dir)
                    .output()
                    .unwrap();
                std::fs::remove_dir_all(&dir).unwrap();
                assert!(
                    String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                    "worker must execute the exact acceptance test:\nstdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(
                    output.status.success(),
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return;
            }
            workspace_topology_and_foreign_focus_cases();
            workspace_pending_visibility_and_id_cases();
            workspace_input_focus_lifecycle_cases();
            workspace_version_and_project_ui_cases();
            workspace_combined_and_mutual_cases();
            workspace_geometry_and_disabled_cases();
            workspace_floating_collapse_and_active_undo_cases();
            workspace_playing_clock_and_full_tab_cycle();
            workspace_positive_one_shot_and_numeric_target_cases();
            workspace_real_generation_only_id_reusing_reload();
        }
        fn painted_texts(output: &egui::FullOutput, dpi: f32) -> Vec<String> {
            fn walk(shape: &egui::Shape, clip: Rect, dpi: f32, texts: &mut Vec<String>) {
                match shape {
                    egui::Shape::Text(t)
                        if clip
                            .expand(1.0 / dpi)
                            .contains_rect(Rect::from_min_size(t.pos, t.galley.size())) =>
                    {
                        texts.push(t.galley.text().to_owned())
                    }
                    egui::Shape::Vec(children) => {
                        for shape in children {
                            walk(shape, clip, dpi, texts)
                        }
                    }
                    _ => {}
                }
            }
            let mut texts = Vec::new();
            for shape in &output.shapes {
                walk(&shape.shape, shape.clip_rect, dpi, &mut texts);
            }
            texts
        }
        fn workspace_geometry_and_disabled_cases() {
            // Finite representatives, not dpi x meter x topology x field product.
            // Default/long-name/enlarged/narrow at each scale; one short scroll
            // representative; real saved/floating topologies covered separately.
            for (dpi, size, enlarged) in [
                (1.0, Vec2::new(1400.0, 1100.0), false),
                (2.0, Vec2::new(1400.0, 1100.0), false),
                (1.0, Vec2::new(660.0, 1000.0), true),
                (2.0, Vec2::new(660.0, 1000.0), true),
                (1.0, Vec2::new(800.0, 500.0), false),
            ] {
                let (ctx, mut s) = workspace_setup(0);
                if enlarged {
                    ctx.style_mut(|style| {
                        for font in style.text_styles.values_mut() {
                            font.size *= 1.3;
                        }
                    });
                    let mut e = s.engine.lock().unwrap();
                    let name =
                        "Layered drums with sustained fractional hits and a long channel name";
                    e.project.tracks[0].name = name.into();
                    for pat in &mut e.project.patterns {
                        let lane = pat.clips.remove("drums").unwrap();
                        pat.clips.insert(name.to_lowercase(), lane);
                    }
                }
                let size_frame = |s: &mut super::super::super::Studio, events| {
                    workspace_frame(&ctx, s, size, dpi, events, true, false)
                };
                for _ in 0..3 {
                    size_frame(&mut s, vec![]);
                }
                let mut f = size_frame(&mut s, vec![]);
                let entry = f
                    .frame
                    .entry
                    .clone()
                    .expect("rendered entry even before scroll reveal");
                // A real recorded entry receiving focus asks its existing rack
                // scroll area to reveal it. No prior coordinates or fake widgets.
                ctx.memory_mut(|m| m.request_focus(entry.id));
                for _ in 0..5 {
                    f = size_frame(&mut s, vec![]);
                }
                let entry = f.frame.entry.unwrap();
                assert!(entry.interact_rect.contains_rect(entry.rect));
                let selected = painted_texts(&f.frame.output, dpi);
                assert!(selected.iter().any(|t| t
                    == &format!(
                        "Selected channel: {}",
                        s.engine.lock().unwrap().project.tracks[0].name
                    )));
                size_frame(&mut s, vec![key(egui::Key::Enter, true)]);
                size_frame(&mut s, vec![key(egui::Key::Enter, false)]);
                let before = rotation_unchanged(&s.engine.lock().unwrap());
                let player = player_snapshot(&s);
                let mut seen = std::collections::BTreeSet::new();
                // Cancel's initial focus may start at the bottom. Wheel upward
                // first, then scan down through the real window's scroll viewport.
                for delta in [200.0, -100.0] {
                    for _ in 0..18 {
                        let f = size_frame(&mut s, vec![]);
                        let layer = rotation_state(&ctx)
                            .lock()
                            .unwrap()
                            .window_layer
                            .expect("real dialog layer");
                        let bounds =
                            ctx.read_response(layer.id)
                                .map(|r| r.rect)
                                .unwrap_or_else(|| {
                                    f.frame
                                        .controls
                                        .iter()
                                        .map(|r| r.rect)
                                        .reduce(Rect::union)
                                        .unwrap()
                                });
                        seen.extend(painted_texts(&f.frame.output, dpi));
                        size_frame(
                            &mut s,
                            vec![
                                egui::Event::PointerMoved(bounds.center()),
                                egui::Event::MouseWheel {
                                    unit: egui::MouseWheelUnit::Point,
                                    delta: Vec2::new(0.0, delta),
                                    modifiers: egui::Modifiers::NONE,
                                },
                            ],
                        );
                    }
                }
                for label in [
                    ROTATE_SCOPE,
                    ROTATE_SHARED,
                    ROTATE_SORT,
                    ROTATE_EARLIER,
                    ROTATE_LATER,
                    ROTATE_CANCEL,
                ] {
                    assert!(
                        seen.contains(label),
                        "not fully painted/revealed {label}; dpi={dpi} size={size:?}"
                    );
                }
                let f = size_frame(&mut s, vec![]);
                for r in &f.frame.controls {
                    ctx.memory_mut(|m| m.request_focus(r.id));
                    let mut revealed = size_frame(&mut s, vec![]);
                    for _ in 0..5 {
                        revealed = size_frame(&mut s, vec![]);
                    }
                    let actual = revealed
                        .frame
                        .controls
                        .iter()
                        .find(|a| a.id == r.id)
                        .unwrap();
                    assert!(actual.interact_rect.contains_rect(actual.rect));
                }
                assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
                assert_player_editor_inert(&s, &player);
            }
            for case in 0..5 {
                let (ctx, mut s) = workspace_setup(1);
                {
                    let mut e = s.engine.lock().unwrap();
                    match case {
                        0 => {
                            e.project.patterns[0].clips.remove("drums");
                        }
                        1 => {
                            e.project.patterns[0].clips.insert("drums".into(), vec![]);
                        }
                        2 => {
                            e.project.tracks[0].instrument = Instrument::Synth(Default::default());
                        }
                        3 => {
                            e.project.patterns[0].meter = Some((1, 16));
                            note(&mut e.project).start = -0.0;
                        }
                        _ => {
                            e.project.patterns[1].clips.get_mut("drums").unwrap()[0].offset = -0.0;
                        }
                    }
                }
                let f = wf(&ctx, &mut s, vec![]);
                let entry = f.frame.entry.unwrap();
                let before = rotation_unchanged(&s.engine.lock().unwrap());
                if case == 2 {
                    assert!(!entry.enabled());
                    let texts = painted_texts(&f.frame.output, 1.0);
                    assert!(texts.iter().any(|t| t == ROTATE_DRUM));
                } else {
                    click_response(&ctx, &mut s, &entry);
                    let f = wf(&ctx, &mut s, vec![]);
                    if case == 4 {
                        assert!(!rotation_state(&ctx).lock().unwrap().active());
                        assert!(s.toast.as_ref().unwrap().0.contains(CODEC_CHANGE));
                    } else if case < 2 {
                        assert_eq!(f.frame.controls.len(), 1);
                        assert!(painted_texts(&f.frame.output, 1.0)
                            .iter()
                            .any(|t| t == ROTATE_EMPTY));
                    } else {
                        assert_eq!(f.frame.controls.len(), 3);
                        assert!(f.frame.controls[0].enabled());
                        // Earlier preserves -0 exactly: a valid zero-call no-op.
                        // Later produces +0: Engine-equal, noncommittable.
                        assert!(f.frame.controls[1].enabled());
                        assert!(!f.frame.controls[2].enabled());
                        assert!(painted_texts(&f.frame.output, 1.0)
                            .iter()
                            .any(|t| t.contains(ROTATE_EQUAL)));
                    }
                }
                assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
            }
            // Explicit independent trackless/no-pattern cases. Old ruler/zero
            // offset/last-step/deadline tests remain byte-exact and UNRUN too.
            for no_pattern in [false, true] {
                let (ctx, mut s) = workspace_setup(1);
                {
                    let mut e = s.engine.lock().unwrap();
                    e.project.tracks.clear();
                    if no_pattern {
                        e.project.patterns.clear();
                    }
                }
                let f = wf(&ctx, &mut s, vec![]);
                assert!(f.frame.entry.is_none());
                assert!(!painted_texts(&f.frame.output, 1.0)
                    .iter()
                    .any(|t| t == ROTATE_ENTRY));
            }
        }
        fn workspace_real_generation_only_id_reusing_reload() {
            let (ctx, mut s) = workspace_setup(1);
            let path = s
                .engine
                .lock()
                .unwrap()
                .workdir
                .join("generation-only.beatbox.json");
            s.project_path = Some(path.clone());
            click_label(&ctx, &mut s, "Save");
            open_real(&ctx, &mut s);
            let (before, generation) = {
                let e = s.engine.lock().unwrap();
                (e.project.clone(), e.project_load_generation)
            };
            s.engine
                .lock()
                .unwrap()
                .call_from("load_project", &json!({"path":path}), "you")
                .unwrap();
            {
                let e = s.engine.lock().unwrap();
                assert!(exact_project(&e.project, &before));
                assert_eq!(e.project_load_generation, generation + 1);
            }
            let observed = rotation_unchanged(&s.engine.lock().unwrap());
            let f = workspace_frame(
                &ctx,
                &mut s,
                Vec2::new(1400.0, 1100.0),
                1.0,
                vec![key(egui::Key::Enter, true), key(egui::Key::Home, true)],
                true,
                true,
            );
            assert!(f.frame.actions.is_empty());
            assert!(!rotation_state(&ctx).lock().unwrap().active());
            assert_rotation_unchanged(&s.engine.lock().unwrap(), &observed);
        }
        fn workspace_positive_one_shot_and_numeric_target_cases() {
            for numeric in [false, true] {
                let (ctx, mut s) = workspace_setup(1);
                {
                    let mut e = s.engine.lock().unwrap();
                    if numeric {
                        e.project.tracks[0].name = "7".into();
                        for p in &mut e.project.patterns {
                            let lane = p.clips.remove("drums").unwrap();
                            p.clips.insert("7".into(), lane);
                        }
                        e.project.patterns[0].name = "13".into();
                    } else {
                        e.project.tracks[0].instrument =
                            Instrument::Sampler(crate::instruments::SamplerParams {
                                one_shot: true,
                                ..Default::default()
                            });
                    }
                }
                let f = open_real(&ctx, &mut s);
                let before = s.engine.lock().unwrap().project.clone();
                let expected = rotation_prediction(&before, 0, 0, Direction::Later).unwrap();
                assert!(f.frame.controls.iter().all(|r| r.enabled()));
                let texts = painted_texts(&f.frame.output, 1.0);
                assert!(texts
                    .iter()
                    .any(|t| t == &format!("Channel: {}", before.tracks[0].name)));
                assert!(texts
                    .iter()
                    .any(|t| t == &format!("Pattern: {}", before.patterns[0].name)));
                click_response(&ctx, &mut s, &f.frame.controls[2]);
                assert!(exact_project(
                    &s.engine.lock().unwrap().project,
                    &expected.project
                ));
                assert_eq!(
                    s.engine
                        .lock()
                        .unwrap()
                        .log
                        .iter()
                        .filter(|l| l.tool == "rotate_notes")
                        .count(),
                    1
                );
            }
        }
        fn workspace_playing_clock_and_full_tab_cycle() {
            let (ctx, mut s) = workspace_setup(1);
            s.player.play();
            let initial = player_snapshot(&s);
            let before = rotation_unchanged(&s.engine.lock().unwrap());
            let f = open_real(&ctx, &mut s);
            assert_player_editor_inert(&s, &initial);
            let layer = rotation_state(&ctx).lock().unwrap().window_layer.unwrap();
            let ids: Vec<_> = f
                .frame
                .controls
                .iter()
                .filter(|r| r.enabled())
                .map(|r| r.id)
                .collect();
            let mut observed = std::collections::HashSet::new();
            observed.insert(ctx.memory(|m| m.focused()).unwrap());
            for _ in 0..4 {
                wf(&ctx, &mut s, vec![tab_event(false, true)]);
                let f = wf(&ctx, &mut s, vec![tab_event(false, false)]);
                if let Some(id) = ctx.memory(|m| m.focused()) {
                    let response = ctx
                        .read_response(id)
                        .expect("real current-pass traversal control");
                    if response.layer_id == layer {
                        assert!(rotation_state(&ctx).lock().unwrap().owns_focus(&ctx, id));
                    }
                    observed.insert(id);
                }
                assert!(f.frame.actions.is_empty());
            }
            assert!(
                ids.iter().all(|id| observed.contains(id)),
                "ordinary Tab must reach every enabled direction and Cancel"
            );
            for _ in 0..4 {
                wf(&ctx, &mut s, vec![tab_event(true, true)]);
                wf(&ctx, &mut s, vec![tab_event(true, false)]);
            }
            wf(&ctx, &mut s, vec![key(egui::Key::Escape, true)]);
            assert_player_editor_inert(&s, &initial);
            assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
            wf(&ctx, &mut s, vec![key(egui::Key::Escape, false)]);
            let f = open_real(&ctx, &mut s);
            let state = s.engine.lock().unwrap().project.clone();
            let expected = rotation_prediction(&state, 0, 0, Direction::Earlier).unwrap();
            let clock = player_snapshot(&s);
            click_response(&ctx, &mut s, &f.frame.controls[1]);
            assert!(exact_project(
                &s.engine.lock().unwrap().project,
                &expected.project
            ));
            assert_player_editor_inert(&s, &clock);
            open_real(&ctx, &mut s);
            s.engine.lock().unwrap().project_load_generation += 1;
            let clock = player_snapshot(&s);
            wf(&ctx, &mut s, vec![]);
            assert!(!rotation_state(&ctx).lock().unwrap().active());
            assert_player_editor_inert(&s, &clock);
            s.player.stop();
        }
        fn workspace_floating_collapse_and_active_undo_cases() {
            let (ctx, mut s) = workspace_setup(4);
            let f = open_real(&ctx, &mut s);
            let before = rotation_unchanged(&s.engine.lock().unwrap());
            // egui_dock's floating body uses a real CollapsingHeader, distinct
            // from the noncollapsible Rotate window. Click its actual Response.
            let layer = rotation_state(&ctx).lock().unwrap().window_layer.unwrap();
            let collapse = f
                .widgets
                .iter()
                .find(|(r, info)| {
                    r.layer_id == f.frame.entry.as_ref().unwrap().layer_id
                        && r.layer_id != layer
                        && info
                            .as_ref()
                            .is_some_and(|i| i.typ == egui::WidgetType::CollapsingHeader)
                })
                .map(|(r, _)| r.clone())
                .expect("real floating-body collapse header");
            click_response(&ctx, &mut s, &collapse);
            for _ in 0..10 {
                wf(&ctx, &mut s, vec![]);
            }
            assert!(!rotation_state(&ctx).lock().unwrap().active());
            assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
            let (ctx, mut s) = workspace_setup(1);
            {
                let mut e = s.engine.lock().unwrap();
                e.call(
                    "set_note_velocity",
                    &json!({"track":"drums","pattern":"A","ids":["n5"],"velocity":0.25}),
                )
                .unwrap();
            }
            let f = open_real(&ctx, &mut s);
            let before = rotation_unchanged(&s.engine.lock().unwrap());
            let player = player_snapshot(&s);
            let cancel = f.frame.controls[0].clone();
            wf(&ctx, &mut s, pointer(cancel.rect.center(), true));
            let mut events = pointer(cancel.rect.center(), false);
            let mut mods = egui::Modifiers::NONE;
            mods.command = true;
            mods.ctrl = !cfg!(target_os = "macos");
            mods.mac_cmd = cfg!(target_os = "macos");
            events.push(egui::Event::Key {
                key: egui::Key::Z,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: mods,
            });
            let f = workspace_frame(
                &ctx,
                &mut s,
                Vec2::new(1400.0, 1100.0),
                1.0,
                events,
                true,
                true,
            );
            assert!(f.frame.actions.is_empty());
            assert_rotation_unchanged(&s.engine.lock().unwrap(), &before);
            assert_player_editor_inert(&s, &player);
        }
    }
}
