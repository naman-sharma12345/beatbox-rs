//! FX chains as one (FL Patcher's everyday use): a whole effect chain saved
//! under a name and applied to any track, bus or the master in one call,
//! plus built-in chains for the moves producers make all the time.
//! Every chain is plain add_effect JSON, so an AI can read and edit it.

use crate::engine::Engine;
use crate::fx::Effect;
use crate::tools::{b_or, effects_of, obj, s_req, Tool};
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
    let arr = v.as_array().ok_or_else(|| anyhow!("{what}: a chain is a list of effects"))?;
    let mut out = Vec::new();
    for (i, x) in arr.iter().enumerate() {
        let fx: Effect = serde_json::from_value(x.clone()).with_context(|| format!("{what}: effect {i} ({x})"))?;
        fx.validate().map_err(|m| anyhow!("{what}: effect {i}: {m}"))?;
        out.push(fx);
    }
    Ok(out)
}

fn chain_by_name(e: &Engine, name: &str) -> Result<Vec<Effect>> {
    if let Some(c) = e.project.fx_chains.get(name) {
        return Ok(c.clone());
    }
    if let Some((_, _, v)) = builtins().into_iter().find(|b| b.0.eq_ignore_ascii_case(name)) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_chains_parse_and_apply() {
        for (n, _, v) in builtins() {
            parse_chain(&v, n).unwrap_or_else(|err| panic!("{n}: {err:#}"));
        }
        let mut e = Engine::new(std::env::temp_dir().join("bb_chain_test"));
        e.call("add_track", &json!({"name": "keys", "preset": "piano"})).unwrap();
        e.call("add_track", &json!({"name": "pad", "preset": "piano"})).unwrap();
        let r = e.call("apply_fx_chain", &json!({"track": "keys", "chain": "lofi_keys"})).unwrap();
        assert_eq!(r["added"], 3);
        e.call("save_fx_chain", &json!({"track": "keys", "name": "mine"})).unwrap();
        let r = e.call("apply_fx_chain", &json!({"track": "pad", "chain": "mine", "replace": true})).unwrap();
        assert_eq!(r["chain"].as_array().unwrap().len(), 3);
        let r = e.call("apply_fx_chain", &json!({"track": "pad", "effects": [{"type": "reverb", "mix": 0.2}]})).unwrap();
        assert_eq!(r["chain"].as_array().unwrap().len(), 4);
        e.call("copy_fx_chain", &json!({"from": "pad", "to": "keys", "replace": true})).unwrap();
        assert_eq!(e.project.tracks[0].effects.len(), 4);
        assert!(e.call("apply_fx_chain", &json!({"track": "pad", "chain": "nope"})).is_err());
        assert!(e.call("apply_fx_chain", &json!({"track": "pad", "effects": [{"type": "vinyl", "age": 3.0}]})).is_err());
        assert!(e.call("save_fx_chain", &json!({"track": "pad", "name": "lofi_keys"})).is_err());
    }
}
