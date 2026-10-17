//! FX chains as one (FL Patcher's everyday use): a whole effect chain saved
//! under a name and applied to any track, bus or the master in one call,
//! plus built-in chains for the moves producers make all the time.
//! Every chain is plain add_effect JSON, so an AI can read and edit it.

use crate::engine::Engine;
use crate::fx::Effect;
use crate::tools::{b_or, effects_of, f_or, obj, s_opt, s_req, Tool};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

/// Built-in chains: (name, what it is for, effects as add_effect JSON).
fn builtins() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        ("vocal_clean", "a modern lead vocal: low cut, de-mud, compressor, presence, de-esser, short plate", json!([
            {"type": "parametric_eq", "bands": [{"kind": "low_cut", "freq": 90.0, "gain_db": 0.0, "q": 0.707}, {"kind": "bell", "freq": 300.0, "gain_db": -2.5, "q": 1.0}, {"kind": "high_shelf", "freq": 3000.0, "gain_db": 3.0, "q": 0.7}]},
            {"type": "compressor", "threshold_db": -18.0, "ratio": 3.0, "attack_ms": 8.0, "release_ms": 120.0, "makeup_db": 3.0},
            {"type": "deesser"},
            {"type": "reverb", "size": 0.35, "mix": 0.12}
        ])),
        ("vocal_radio", "telephone / radio voice for intros and ad-libs", json!([
            {"type": "parametric_eq", "bands": [{"kind": "low_cut", "freq": 400.0, "gain_db": 0.0, "q": 0.9}, {"kind": "high_cut", "freq": 3200.0, "gain_db": 0.0, "q": 0.9}, {"kind": "bell", "freq": 1500.0, "gain_db": 4.0, "q": 1.2}]},
            {"type": "saturator", "mode": "transistor", "drive_db": 9.0, "mix": 0.6}
        ])),
        ("lofi_keys", "dusty keys/chords: worn vinyl, muffled top, gentle tape saturation", json!([
            {"type": "vinyl", "crackle": 0.0, "hiss_db": -120.0, "wow_cents": 8.0, "flutter_cents": 2.0, "age": 0.5},
            {"type": "saturator", "mode": "tape", "drive_db": 6.0, "mix": 0.5},
            {"type": "parametric_eq", "bands": [{"kind": "high_cut", "freq": 4500.0, "gain_db": 0.0, "q": 0.707}, {"kind": "bell", "freq": 350.0, "gain_db": -3.0, "q": 1.0}]}
        ])),
        ("dusty_drums", "boom-bap / lo-fi drum bus: tape crush, dark top, glue", json!([
            {"type": "saturator", "mode": "tape", "drive_db": 8.0, "mix": 0.6},
            {"type": "parametric_eq", "bands": [{"kind": "high_cut", "freq": 7000.0, "gain_db": 0.0, "q": 0.707}]},
            {"type": "compressor", "threshold_db": -16.0, "ratio": 4.0, "attack_ms": 20.0, "release_ms": 90.0, "makeup_db": 3.0}
        ])),
        ("808_grit", "an 808 that reads on phone speakers: soft-clip harmonics, sub kept mono", json!([
            {"type": "saturator", "mode": "tube", "drive_db": 10.0, "mix": 0.5},
            {"type": "soft_clipper"},
            {"type": "width", "amount": 0.0}
        ])),
        ("wide_pad", "a pad that sits behind everything: low cut, slow chorus, wide, long reverb", json!([
            {"type": "parametric_eq", "bands": [{"kind": "low_cut", "freq": 200.0, "gain_db": 0.0, "q": 0.707}]},
            {"type": "chorus", "mix": 0.4},
            {"type": "width", "amount": 1.4},
            {"type": "reverb", "size": 0.85, "mix": 0.35}
        ])),
        ("clean_chorus_guitar", "an indie clean electric: bright clean amp, wide lush chorus, a dotted-eighth echo and a short plate (picked arpeggios, jangly riffs)", json!([
            {"type": "parametric_eq", "bands": [{"kind": "low_cut", "freq": 110.0, "gain_db": 0.0, "q": 0.707}, {"kind": "bell", "freq": 400.0, "gain_db": -2.0, "q": 1.0}, {"kind": "high_shelf", "freq": 3200.0, "gain_db": 4.0, "q": 0.7}]},
            {"type": "amp_sim", "model": "clean", "gain_db": 3.0, "treble": 0.7, "presence": 0.65, "cab": "2x12", "mix": 0.8},
            {"type": "chorus", "rate_hz": 0.8, "depth_ms": 4.0, "mix": 0.5},
            {"type": "width", "amount": 1.6, "low_mono_hz": 180.0},
            {"type": "delay", "steps": 3, "feedback": 0.22, "mix": 0.14, "ping_pong": true, "tone": 5000.0},
            {"type": "reverb", "mode": "fdn_plate", "size": 0.45, "mix": 0.16}
        ])),
        ("bright_cymbals", "a drum bus with air: kick and snare kept, cymbals and hats lifted above 5 kHz, gentle glue", json!([
            {"type": "parametric_eq", "bands": [{"kind": "high_shelf", "freq": 5000.0, "gain_db": 3.5, "q": 0.7}, {"kind": "bell", "freq": 3000.0, "gain_db": 1.5, "q": 0.9}, {"kind": "bell", "freq": 400.0, "gain_db": -1.5, "q": 1.0}]},
            {"type": "compressor", "threshold_db": -14.0, "ratio": 2.0, "attack_ms": 25.0, "release_ms": 120.0, "makeup_db": 1.0}
        ])),
        ("tape_master", "a warm finishing chain for the master: tape, gentle glue, ceiling -1 dBTP", json!([
            {"type": "saturator", "mode": "tape", "drive_db": 3.0, "mix": 0.4},
            {"type": "compressor", "threshold_db": -12.0, "ratio": 1.8, "attack_ms": 30.0, "release_ms": 200.0, "makeup_db": 1.0},
            {"type": "limiter", "ceiling_db": -1.0}
        ])),
        ("old_record", "the whole track as a worn record: crackle, hiss, wow, narrow band, mono", json!([
            {"type": "vinyl", "crackle": 12.0, "hiss_db": -48.0, "wow_cents": 12.0, "flutter_cents": 4.0, "age": 0.8, "mono": 0.7}
        ])),
    ]
}

fn parse_chain(v: &Value, what: &str) -> Result<Vec<Effect>> {
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow!("{what}: a chain is a list of effects"))?;
    let mut out = Vec::new();
    for (i, x) in arr.iter().enumerate() {
        let fx: Effect = serde_json::from_value(x.clone())
            .with_context(|| format!("{what}: effect {i} ({x})"))?;
        fx.validate()
            .map_err(|m| anyhow!("{what}: effect {i}: {m}"))?;
        out.push(fx);
    }
    Ok(out)
}

fn chain_by_name(e: &Engine, name: &str) -> Result<Vec<Effect>> {
    if let Some(c) = e.project.fx_chains.get(name) {
        return Ok(c.clone());
    }
    if let Some((_, _, v)) = builtins()
        .into_iter()
        .find(|b| b.0.eq_ignore_ascii_case(name))
    {
        return parse_chain(&v, name);
    }
    let mut names: Vec<String> = builtins().iter().map(|b| b.0.to_string()).collect();
    names.extend(e.project.fx_chains.keys().cloned());
    bail!("no fx chain '{name}' (have: {})", names.join(", "))
}

fn types(c: &[Effect]) -> Vec<String> {
    c.iter().map(|x| x.type_name()).collect()
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "list_fx_chains",
            description: "FX chains (FL Patcher-style presets of a whole effect chain): the built-ins (vocal_clean, vocal_radio, lofi_keys, dusty_drums, 808_grit, wide_pad, tape_master, old_record) and any saved in this project, each with its effects as add_effect JSON.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |e, _| {
                let mut v: Vec<Value> = builtins()
                    .into_iter()
                    .map(|(n, d, c)| json!({"name": n, "builtin": true, "use": d, "effects": c}))
                    .collect();
                for (n, c) in &e.project.fx_chains {
                    v.push(json!({"name": n, "builtin": false, "effects": serde_json::to_value(c)?}));
                }
                Ok(json!({"chains": v}))
            },
        },
        Tool {
            name: "apply_fx_chain",
            description: "Put a whole FX chain on a track, bus or 'master' in one call: a built-in or saved chain by `chain` name, or your own `effects` list (add_effect JSON, e.g. [{type:'saturator', mode:'tape'}, {type:'reverb', mix:0.2}]). replace=true clears the existing chain first (default: append).",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string", "description": "Track, bus or 'master'"},
                "chain": {"type": "string", "description": "Built-in or saved chain name (list_fx_chains)"},
                "effects": {"type": "array", "items": {"type": "object"}, "description": "Instead of a name: the effects, each {type, ...params}"},
                "replace": {"type": "boolean", "description": "Clear the existing chain first (default false)"}
            }), &["track"]),
            run: |e, a| {
                let track = s_req(a, "track")?;
                let chain = match (a.get("effects"), a.get("chain").and_then(|v| v.as_str())) {
                    (Some(fx), _) => parse_chain(fx, "apply_fx_chain")?,
                    (None, Some(n)) => chain_by_name(e, n)?,
                    (None, None) => bail!("apply_fx_chain: give `chain` (a name) or `effects` (a list)"),
                };
                let n = chain.len();
                let replace = b_or(a, "replace", false);
                let dst = effects_of(&mut e.project, &track)?;
                if replace {
                    dst.clear();
                }
                dst.extend(chain);
                let now = types(dst);
                e.project.ensure_fx_ids();
                Ok(json!({"track": track, "added": n, "replaced": replace, "chain": now}))
            },
        },
        Tool {
            name: "soundgoodizer",
            description: "One-knob loudness and polish (FL Soundgoodizer) on a track, bus or 'master': a 3-band compressor tuned by mode, blended in by amount, then a -1 dB ceiling. mode A (default) = even lift, B = forward mids (vocals, leads), C = brighter top (air, hats), D = heavy low end and loud. amount 0-100 (default 50). Calling it again on the same track retunes the existing one instead of stacking.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string", "description": "Track, bus or 'master'"},
                "mode": {"type": "string", "enum": ["A", "B", "C", "D"]},
                "amount": {"type": "number", "description": "0-100, default 50"}
            }), &["track"]),
            run: |e, a| {
                let track = s_req(a, "track")?;
                let mode = s_opt(a, "mode").unwrap_or_else(|| "A".into()).to_uppercase();
                let amount = f_or(a, "amount", 50.0);
                if !(0.0..=100.0).contains(&amount) {
                    bail!("amount is 0-100");
                }
                let fx = goodizer_chain(&mode, amount / 100.0)?;
                let chain = parse_chain(&fx, "soundgoodizer")?;
                let dst = effects_of(&mut e.project, &track)?;
                // retune: drop the previous soundgoodizer pair (tagged multiband + limiter right after it)
                let mut retuned = false;
                if let Some(i) = dst.iter().position(|f| serde_json::to_value(f).map(|v| v["low_freq"] == json!(GOOD_TAG)).unwrap_or(false)) {
                    let n = if i + 1 < dst.len() && serde_json::to_value(&dst[i + 1]).map(|v| v["type"] == "limiter").unwrap_or(false) { 2 } else { 1 };
                    dst.drain(i..i + n);
                    for (k, f) in chain.into_iter().enumerate() {
                        dst.insert(i + k, f);
                    }
                    retuned = true;
                } else {
                    dst.extend(chain);
                }
                let now = types(dst);
                e.project.ensure_fx_ids();
                Ok(json!({"track": track, "mode": mode, "amount": amount, "retuned": retuned, "chain": now}))
            },
        },
        Tool {
            name: "save_fx_chain",
            description: "Save the effect chain of a track, bus or 'master' under a name in the project (FL Patcher-style preset), to apply_fx_chain elsewhere. Overwrites a saved chain with the same name.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string", "description": "Track, bus or 'master' to save from"},
                "name": {"type": "string", "description": "Chain name"}
            }), &["track", "name"]),
            run: |e, a| {
                let track = s_req(a, "track")?;
                let name = s_req(a, "name")?;
                if name.trim().is_empty() {
                    bail!("save_fx_chain: empty name");
                }
                if builtins().iter().any(|b| b.0.eq_ignore_ascii_case(&name)) {
                    bail!("save_fx_chain: '{name}' is a built-in chain; pick another name");
                }
                let mut c = effects_of(&mut e.project, &track)?.clone();
                for fx in c.iter_mut() {
                    fx.set_id("");
                }
                let t = types(&c);
                e.project.fx_chains.insert(name.clone(), c);
                Ok(json!({"saved": name, "from": track, "chain": t}))
            },
        },
        Tool {
            name: "copy_fx_chain",
            description: "Copy the whole effect chain from one track/bus/'master' to another (replace=true clears the destination first, default append).",
            mutates: true,
            schema: || obj(json!({
                "from": {"type": "string"},
                "to": {"type": "string"},
                "replace": {"type": "boolean"}
            }), &["from", "to"]),
            run: |e, a| {
                let from = s_req(a, "from")?;
                let to = s_req(a, "to")?;
                let mut c = effects_of(&mut e.project, &from)?.clone();
                for fx in c.iter_mut() {
                    fx.set_id("");
                }
                let n = c.len();
                let replace = b_or(a, "replace", false);
                let dst = effects_of(&mut e.project, &to)?;
                if replace {
                    dst.clear();
                }
                dst.extend(c);
                let now = types(dst);
                e.project.ensure_fx_ids();
                Ok(json!({"from": from, "to": to, "copied": n, "chain": now}))
            },
        },
    ]
}

/// Crossover the soundgoodizer's compressor uses; also how a re-call finds it.
const GOOD_TAG: f32 = 180.0;
/// Band level the makeup is calibrated to.
const NOMINAL_DB: f32 = -14.0;

/// Mode presets: (low thr, ratio, gain), (mid ...), (high ...); amount 0..1
/// blends the compressor in and scales the band lifts.
fn goodizer_chain(mode: &str, k: f32) -> Result<Value> {
    let p: [(f32, f32, f32); 3] = match mode {
        "A" => [(-20.0, 2.5, 2.0), (-20.0, 2.5, 2.0), (-22.0, 2.5, 2.5)],
        "B" => [(-20.0, 2.5, 1.0), (-24.0, 3.5, 3.5), (-22.0, 2.5, 2.0)],
        "C" => [(-20.0, 2.0, 1.0), (-20.0, 2.5, 1.5), (-26.0, 3.0, 4.5)],
        "D" => [(-26.0, 4.0, 4.0), (-24.0, 3.0, 3.0), (-24.0, 3.0, 3.0)],
        m => bail!("mode '{m}': use A, B, C or D"),
    };
    let k = k.clamp(0.0, 1.0);
    // amount scales the compression itself (threshold toward 0 dB, ratio
    // toward 1:1) rather than a dry/wet blend: the crossover's phase shift
    // would comb against the dry signal. Makeup gives back what the
    // compressor takes from a band at a typical mix level (-14 dBFS) plus
    // the mode's lift, so the result is denser AND louder, as in FL.
    let eff = |b: (f32, f32, f32)| {
        let (t, r) = (b.0 * k, 1.0 + (b.1 - 1.0) * k);
        (t, r, (NOMINAL_DB - t).max(0.0) * (1.0 - 1.0 / r) + b.2 * k)
    };
    let (l, m, h) = (eff(p[0]), eff(p[1]), eff(p[2]));
    Ok(json!([
        {"type": "multiband", "low_freq": GOOD_TAG, "high_freq": 4000.0,
         "low_threshold_db": l.0, "low_ratio": l.1, "low_gain_db": l.2,
         "mid_threshold_db": m.0, "mid_ratio": m.1, "mid_gain_db": m.2,
         "high_threshold_db": h.0, "high_ratio": h.1, "high_gain_db": h.2,
         "attack_ms": 10.0, "release_ms": 120.0, "mix": 1.0},
        {"type": "limiter", "ceiling_db": -1.0}
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soundgoodizer_adds_then_retunes() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_chain_tests"));
        e.call("add_track", &json!({"name": "keys"})).unwrap();
        let r = e
            .call(
                "soundgoodizer",
                &json!({"track": "keys", "mode": "b", "amount": 70}),
            )
            .unwrap();
        assert_eq!(r["chain"], json!(["multiband", "limiter"]), "{r}");
        assert_eq!(r["retuned"], false);
        let r = e
            .call(
                "soundgoodizer",
                &json!({"track": "keys", "mode": "D", "amount": 30}),
            )
            .unwrap();
        assert_eq!(r["chain"], json!(["multiband", "limiter"]), "{r}");
        assert_eq!(r["retuned"], true);
        let fx = serde_json::to_value(
            &e.project
                .tracks
                .iter()
                .find(|t| t.name == "keys")
                .unwrap()
                .effects[0],
        )
        .unwrap();
        // D at 30%: threshold -26*0.3, ratio 1+3*0.3
        assert!(
            (fx["low_threshold_db"].as_f64().unwrap() + 7.8).abs() < 1e-3,
            "{fx}"
        );
        assert!(
            (fx["low_ratio"].as_f64().unwrap() - 1.9).abs() < 1e-3,
            "{fx}"
        );
        assert!(e
            .call("soundgoodizer", &json!({"track": "keys", "mode": "Z"}))
            .is_err());
        assert!(e
            .call("soundgoodizer", &json!({"track": "keys", "amount": 150}))
            .is_err());
        e.call("soundgoodizer", &json!({"track": "master"}))
            .unwrap();
    }

    #[test]
    fn builtin_chains_parse_and_apply() {
        for (n, _, v) in builtins() {
            parse_chain(&v, n).unwrap_or_else(|err| panic!("{n}: {err:#}"));
        }
        let mut e = Engine::new(std::env::temp_dir().join("bb_chain_test"));
        e.call("add_track", &json!({"name": "keys", "preset": "piano"}))
            .unwrap();
        e.call("add_track", &json!({"name": "pad", "preset": "piano"}))
            .unwrap();
        let r = e
            .call(
                "apply_fx_chain",
                &json!({"track": "keys", "chain": "lofi_keys"}),
            )
            .unwrap();
        assert_eq!(r["added"], 3);
        e.call("save_fx_chain", &json!({"track": "keys", "name": "mine"}))
            .unwrap();
        let r = e
            .call(
                "apply_fx_chain",
                &json!({"track": "pad", "chain": "mine", "replace": true}),
            )
            .unwrap();
        assert_eq!(r["chain"].as_array().unwrap().len(), 3);
        let r = e
            .call(
                "apply_fx_chain",
                &json!({"track": "pad", "effects": [{"type": "reverb", "mix": 0.2}]}),
            )
            .unwrap();
        assert_eq!(r["chain"].as_array().unwrap().len(), 4);
        e.call(
            "copy_fx_chain",
            &json!({"from": "pad", "to": "keys", "replace": true}),
        )
        .unwrap();
        assert_eq!(e.project.tracks[0].effects.len(), 4);
        assert!(e
            .call("apply_fx_chain", &json!({"track": "pad", "chain": "nope"}))
            .is_err());
        assert!(e
            .call(
                "apply_fx_chain",
                &json!({"track": "pad", "effects": [{"type": "vinyl", "age": 3.0}]})
            )
            .is_err());
        assert!(e
            .call(
                "save_fx_chain",
                &json!({"track": "pad", "name": "lofi_keys"})
            )
            .is_err());
    }
}
