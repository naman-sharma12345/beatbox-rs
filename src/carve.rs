//! Arrangement-level EQ carving: give the low end to the kick and the bass.
//!
//! Every non-bass track gets a high-pass (low cut) sized to its role, and
//! when a bass or 808 plays, the melodic beds (keys, pads, leads) also get a
//! gentle low-mid dip around 300 Hz, which is where stacked chord voicings and
//! bass harmonics build mud. The carve is one parametric EQ with the id
//! `carve` at the end of each chain (appended, so index-addressed automation
//! on earlier effects keeps pointing at the same effect), so running it again replaces it instead
//! of stacking another.

use crate::engine::Engine;
use crate::fx::{Effect, EqBand, EqBandKind, ParametricEqFx};
use crate::instruments::{DrumKind, Instrument};
use crate::tools_mix::role_of;
use serde_json::{json, Value};

pub const CARVE_ID: &str = "carve";

/// High-pass and low-mid dip for one track, or None to leave it alone.
/// Returns (low cut Hz, optional (bell Hz, gain dB)).
pub fn carve_for(
    name: &str,
    inst: &Instrument,
    bass_present: bool,
) -> Option<(f32, Option<(f32, f32)>)> {
    let first = match inst {
        Instrument::Layer(l) => l.layers.first().map(|x| &x.instrument).unwrap_or(inst),
        other => other,
    };
    if let Instrument::Drum(d) = first {
        // tabla's bayan and any kick own their lows
        if matches!(d.kind, DrumKind::Kick | DrumKind::Bayan) {
            return None;
        }
        if d.kind == DrumKind::Tabla {
            return Some((90.0, None));
        }
    }
    let n = name.to_lowercase();
    if n.contains("bayan") || n.contains("dholak_bass") || n.contains("dagga") {
        return None;
    }
    let mud = |g: f32| if bass_present { Some((320.0, g)) } else { None };
    Some(match role_of(name, inst) {
        "kick" | "bass" | "fx" => return None,
        "snare" => (110.0, None),
        "hats" => (320.0, None),
        "cymbal" => (260.0, None),
        "perc" if n.contains("tabla") || n.contains("dholak") => (90.0, None),
        "perc" => (160.0, None),
        "pad" => (if bass_present { 220.0 } else { 120.0 }, mud(-4.5)),
        "keys" => (if bass_present { 190.0 } else { 100.0 }, mud(-3.5)),
        // leads, plucks, bells, Indian melodic voices
        _ => (if bass_present { 170.0 } else { 100.0 }, mud(-2.5)),
    })
}

/// Does a bass/808 track play anywhere in the song?
pub fn bass_plays(e: &Engine) -> bool {
    e.project.tracks.iter().any(|t| {
        role_of(&t.name, &t.instrument) == "bass"
            && e.project
                .patterns
                .iter()
                .any(|p| !p.notes(&t.name).is_empty())
    })
}

/// Carve every track (or `only` these). Returns what was set per track.
pub fn carve(e: &mut Engine, only: &[String]) -> Value {
    let bass = bass_plays(e);
    let mut rows = Vec::new();
    for t in e.project.tracks.iter_mut() {
        if !only.is_empty() && !only.iter().any(|o| o.eq_ignore_ascii_case(&t.name)) {
            continue;
        }
        let existing = t.effects.iter().position(|x| x.id() == CARVE_ID);
        let Some((hp, bell)) = carve_for(&t.name, &t.instrument, bass) else {
            if let Some(i) = existing {
                t.effects.remove(i);
            }
            continue;
        };
        let mut bands = vec![EqBand {
            kind: EqBandKind::LowCut,
            freq: hp,
            q: 0.707,
            stages: 2,
            ..Default::default()
        }];
        if let Some((f, g)) = bell {
            bands.push(EqBand {
                kind: EqBandKind::Bell,
                freq: f,
                gain_db: g,
                q: 0.9,
                ..Default::default()
            });
        }
        let mut fx = ParametricEqFx {
            bands,
            ..Default::default()
        };
        fx.id = CARVE_ID.into();
        let fx = Effect::ParametricEq(fx);
        match existing {
            Some(i) => t.effects[i] = fx,
            None => t.effects.push(fx),
        }
        rows.push(json!({"track": t.name, "low_cut_hz": hp, "low_mid_dip": bell.map(|(f, g)| json!({"hz": f, "db": g}))}));
    }
    e.project.ensure_fx_ids();
    json!({"bass_present": bass, "carved": rows})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn carve_cuts_beds_not_kick_or_bass_and_is_idempotent() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_carve_tests"));
        e.call(
            "generate_beat",
            &json!({"style": "boom_bap", "bars": 2, "seed": 3}),
        )
        .unwrap();
        let r = carve(&mut e, &[]);
        assert_eq!(r["bass_present"], true);
        let carved = |e: &Engine, t: &str| {
            let i = e.project.track_index(t).unwrap();
            e.project.tracks[i]
                .effects
                .iter()
                .filter(|x| x.id() == CARVE_ID)
                .count()
        };
        assert_eq!(carved(&e, "kick"), 0);
        assert_eq!(carved(&e, "bass"), 0);
        assert_eq!(carved(&e, "chords"), 1);
        assert_eq!(carved(&e, "hat"), 1);
        carve(&mut e, &[]);
        assert_eq!(carved(&e, "chords"), 1, "running it twice must not stack");
    }
}
