//! Sprint 7 chain tools: precise effect placement (`place_fx`, `move_effect`)
//! and a loudness-matched A/B comparison of two project versions.

use crate::analysis;
use crate::dsp::{db_to_gain, SR};
use crate::fx::{self, Effect};
use crate::media;
use crate::render::Mix;
use crate::tools::{b_or, effects_of, f_opt, merge, obj, reject_unknown, s_opt, s_req, Tool};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

/// Resolve "index or stable id" to a position in the chain.
fn resolve_pos(chain: &[Effect], v: &Value, what: &str) -> Result<usize> {
    if let Some(n) = v.as_u64() {
        let n = n as usize;
        if n >= chain.len() {
            bail!("{what}: no effect at index {n} (chain has {})", chain.len());
        }
        return Ok(n);
    }
    let key = v
        .as_str()
        .ok_or_else(|| anyhow!("{what}: give an index or an effect id"))?;
    // the effect's type works too ('saturator'): the first one of that type
    let by_type = || {
        let k = key.to_lowercase();
        chain.iter().position(|x| x.type_name() == k)
    };
    fx::find(chain, key).or_else(by_type).ok_or_else(|| {
        anyhow!(
            "{what}: no effect '{key}'. Chain: [{}]",
            chain
                .iter()
                .map(|x| x.id().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

fn chain_json(chain: &[Effect]) -> Value {
    json!(chain
        .iter()
        .enumerate()
        .map(|(i, x)| json!({"index": i, "id": x.id(), "type": x.type_name(), "bypassed": x.bypassed()}))
        .collect::<Vec<_>>())
}

/// Matched-loudness metrics of one mix.
fn matched(mix: &Mix, gain: f32) -> (Mix, analysis::Report) {
    let m = Mix {
        left: mix.left.iter().map(|x| x * gain).collect(),
        right: mix.right.iter().map(|x| x * gain).collect(),
        stems: Vec::new(),
        track_info: Vec::new(),
        bus_stems: Vec::new(),
        seconds: mix.seconds,
    };
    let rep = analysis::analyze(&m);
    (m, rep)
}

fn bands_of(r: &analysis::Report) -> serde_json::Map<String, Value> {
    r.master
        .bands
        .iter()
        .map(|b| (b.band.to_string(), json!(b.percent)))
        .collect()
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "place_fx",
            description: "Insert an effect at an exact spot in a track's (or 'master') chain instead of the end: position 'first' | 'last' | an integer index, or before / after another effect by index or stable id (e.g. {track:'bass', type:'saturator', before:'compressor1'}). Same params as add_effect; unknown params are rejected. Returns the resulting chain with ids.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string", "description": "Track name, bus or 'master'"},
                "type": {"type": "string", "enum": fx::EFFECT_TYPES.iter().map(|e| e.0).collect::<Vec<_>>()},
                "params": {"type": "object"},
                "position": {"type": ["integer", "string"], "description": "'first', 'last' or a 0-based index to insert at"},
                "before": {"type": ["integer", "string"], "description": "insert directly before this effect (index or id)"},
                "after": {"type": ["integer", "string"], "description": "insert directly after this effect (index or id)"},
            }), &["track", "type"]),
            run: |e, a| {
                let t = s_req(a, "type")?;
                let mut v = json!({"type": t.to_lowercase()});
                if let Some(p) = a.get("params") {
                    merge(&mut v, p);
                }
                let fxn: Effect = serde_json::from_value(v).with_context(|| format!("bad effect '{t}'"))?;
                let effective = serde_json::to_value(&fxn)?;
                if let Some(p) = a.get("params") {
                    reject_unknown(p, &effective, &format!("place_fx {t}"))?;
                }
                let track = s_req(a, "track")?;
                if let Effect::Sidechain(sc) = &fxn {
                    e.project.track_index(&sc.source).context("sidechain source track")?;
                }
                let chain = effects_of(&mut e.project, &track)?;
                let at = if let Some(b) = a.get("before") {
                    resolve_pos(chain, b, "before")?
                } else if let Some(af) = a.get("after") {
                    resolve_pos(chain, af, "after")? + 1
                } else {
                    match a.get("position") {
                        None => chain.len(),
                        Some(Value::String(s)) if s.eq_ignore_ascii_case("first") => 0,
                        Some(Value::String(s)) if s.eq_ignore_ascii_case("last") => chain.len(),
                        Some(Value::Number(n)) => (n.as_u64().unwrap_or(0) as usize).min(chain.len()),
                        Some(_) => bail!("position must be 'first', 'last' or an index"),
                    }
                };
                chain.insert(at, fxn);
                fx::ensure_ids(chain);
                Ok(json!({"track": track, "inserted_at": at, "id": chain[at].id(), "chain": chain_json(chain), "effective": effective}))
            },
        },
        Tool {
            name: "move_effect",
            description: "Reorder a track's (or 'master') effect chain: move the effect `effect` (index, stable id like 'saturator1', or its type like 'saturator' for the first of that type) to `position` ('first' | 'last' | an index) or before / after another effect. Order matters: saturate before compressing, EQ before reverb, limiter last.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string"},
                "effect": {"type": ["integer", "string"]},
                "position": {"type": ["integer", "string"], "description": "'first', 'last' or a 0-based target index"},
                "before": {"type": ["integer", "string"]},
                "after": {"type": ["integer", "string"]},
            }), &["track", "effect"]),
            run: |e, a| {
                let track = s_req(a, "track")?;
                let chain = effects_of(&mut e.project, &track)?;
                let from = resolve_pos(chain, a.get("effect").ok_or_else(|| anyhow!("missing 'effect'"))?, "effect")?;
                let item = chain.remove(from);
                let at = if let Some(b) = a.get("before") {
                    let k = resolve_pos(chain, b, "before");
                    match k {
                        Ok(k) => k,
                        Err(er) => {
                            chain.insert(from, item);
                            return Err(er);
                        }
                    }
                } else if let Some(af) = a.get("after") {
                    match resolve_pos(chain, af, "after") {
                        Ok(k) => k + 1,
                        Err(er) => {
                            chain.insert(from, item);
                            return Err(er);
                        }
                    }
                } else {
                    match a.get("position") {
                        Some(Value::String(s)) if s.eq_ignore_ascii_case("first") => 0,
                        Some(Value::String(s)) if s.eq_ignore_ascii_case("last") => chain.len(),
                        Some(Value::Number(n)) => (n.as_u64().unwrap_or(0) as usize).min(chain.len()),
                        _ => {
                            chain.insert(from, item);
                            bail!("give `position` ('first' | 'last' | index), `before` or `after`");
                        }
                    }
                };
                chain.insert(at, item);
                Ok(json!({"track": track, "moved_from": from, "moved_to": at, "chain": chain_json(chain)}))
            },
        },
        Tool {
            name: "ab_compare",
            description: "Loudness-matched A/B of two project versions (snapshot names or 'current'; default A = newest snapshot, B = current). Louder always sounds better, so the louder version is turned down to the quieter one's integrated LUFS before judging. Returns the gain applied, the matched score, crest, stereo correlation and spectral-band deltas (B minus A), a verdict, and two level-matched WAVs (ab_<a>.wav / ab_<b>.wav in the renders folder) to listen to blind.",
            mutates: false,
            schema: || obj(json!({
                "a": {"type": "string", "description": "snapshot name or 'current' (default: newest snapshot)"},
                "b": {"type": "string", "description": "snapshot name or 'current' (default: 'current')"},
                "write_wavs": {"type": "boolean", "description": "save the matched renders (default true)"},
                "match_lufs": {"type": "number", "description": "match both to this LUFS instead of the quieter one (capped at +-12 dB of gain)"},
            }), &[]),
            run: |e, a| {
                let an = s_opt(a, "a")
                    .or_else(|| e.snapshots.last().map(|(n, _)| n.clone()))
                    .ok_or_else(|| anyhow!("no snapshot to compare against; call snapshot first or pass `a`"))?;
                let bn = s_opt(a, "b").unwrap_or_else(|| "current".into());
                let (pa, pb) = (e.version(&an)?, e.version(&bn)?);
                let (ma, mb) = (e.render_version(&pa)?, e.render_version(&pb)?);
                let (la, lb) = (analysis::loudness(&ma.left, &ma.right), analysis::loudness(&mb.left, &mb.right));
                let target = f_opt(a, "match_lufs").unwrap_or_else(|| la.integrated_lufs.min(lb.integrated_lufs));
                let ga = db_to_gain((target - la.integrated_lufs).clamp(-12.0, 12.0));
                let gb = db_to_gain((target - lb.integrated_lufs).clamp(-12.0, 12.0));
                let (xa, ra) = matched(&ma, ga);
                let (xb, rb) = matched(&mb, gb);
                let (ba, bb) = (bands_of(&ra), bands_of(&rb));
                let band_delta: serde_json::Map<String, Value> = bb
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(((v.as_f64().unwrap_or(0.0) - ba.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0)) * 10.0).round() / 10.0)))
                    .collect();
                let ds = rb.score as i64 - ra.score as i64;
                let verdict = if ds >= 2 {
                    format!("B ('{bn}') wins at matched loudness by {ds} points")
                } else if ds <= -2 {
                    format!("A ('{an}') wins at matched loudness by {} points", -ds)
                } else {
                    "a tie on the measurements at matched loudness: decide by ear".to_string()
                };
                let r1 = |x: f32| (x * 10.0).round() / 10.0;
                let mut out = json!({
                    "a": an, "b": bn,
                    "raw_lufs": {"a": r1(la.integrated_lufs), "b": r1(lb.integrated_lufs)},
                    "matched_to_lufs": r1(target),
                    "gain_db": {"a": r1(20.0 * ga.log10()), "b": r1(20.0 * gb.log10())},
                    "score": {"a": ra.score, "b": rb.score, "delta": ds},
                    "crest_db": {"a": r1(ra.master.crest_db), "b": r1(rb.master.crest_db), "delta": r1(rb.master.crest_db - ra.master.crest_db)},
                    "stereo_correlation": {"a": r1(ra.stereo_correlation * 100.0) / 100.0, "b": r1(rb.stereo_correlation * 100.0) / 100.0},
                    "band_percent_delta_b_minus_a": band_delta,
                    "true_peak_dbtp_after_match": {"a": r1(analysis::loudness(&xa.left, &xa.right).true_peak_dbtp), "b": r1(analysis::loudness(&xb.left, &xb.right).true_peak_dbtp)},
                    "verdict": verdict,
                    "suggestions_b": rb.suggestions,
                });
                if b_or(a, "write_wavs", true) {
                    let dir = e.renders_dir();
                    std::fs::create_dir_all(&dir)?;
                    let slug = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect::<String>();
                    let mut blocks = Vec::new();
                    let mut paths = Vec::new();
                    for (n, m) in [(&an, &xa), (&bn, &xb)] {
                        let p = dir.join(format!("ab_{}.wav", slug(n)));
                        // a gain-matched copy can exceed full scale: protect the file, not the verdict
                        let peak = m.left.iter().chain(m.right.iter()).fold(0.0f32, |x, v| x.max(v.abs()));
                        let g = if peak > 0.99 { 0.99 / peak } else { 1.0 };
                        let (l, r): (Vec<f32>, Vec<f32>) = (m.left.iter().map(|x| x * g).collect(), m.right.iter().map(|x| x * g).collect());
                        std::fs::write(&p, media::wav_bytes(&l, &r, SR as u32))?;
                        blocks.push(media::link_block(&p, "audio/wav"));
                        paths.push(json!(p));
                    }
                    out["wavs"] = json!(paths);
                    media::attach(&mut out, blocks);
                }
                Ok(out)
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;

    fn eng() -> Engine {
        Engine::new(std::env::temp_dir().join("bb_fx2_test"))
    }

    #[test]
    fn place_and_move_fx_by_id_and_position() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "bass", "preset": "acid_bass"}))
            .unwrap();
        let name = e.project.tracks[0].name.clone();
        e.call("add_effect", &json!({"track": name, "type": "compressor"}))
            .unwrap();
        e.call("add_effect", &json!({"track": name, "type": "limiter"}))
            .unwrap();
        let r = e.call("place_fx", &json!({"track": name, "type": "saturator", "params": {"mode": "tube"}, "before": 0})).unwrap();
        assert_eq!(r["inserted_at"], 0);
        let r = e
            .call(
                "place_fx",
                &json!({"track": name, "type": "haas", "position": "last"}),
            )
            .unwrap();
        assert_eq!(r["inserted_at"], 3);
        let r = e
            .call(
                "move_effect",
                &json!({"track": name, "effect": "haas1", "position": "first"}),
            )
            .unwrap();
        assert_eq!(r["chain"][0]["type"], "haas");
        assert!(e
            .call(
                "move_effect",
                &json!({"track": name, "effect": "nope", "position": 0})
            )
            .is_err());
        assert!(e
            .call(
                "place_fx",
                &json!({"track": name, "type": "saturator", "params": {"bogus": 1}})
            )
            .is_err());
        let chain: Vec<String> = e.project.tracks[0]
            .effects
            .iter()
            .map(|x| x.type_name())
            .collect();
        assert_eq!(chain, ["haas", "saturator", "compressor", "limiter"]);
    }

    #[test]
    fn ab_compare_matches_loudness_before_judging() {
        let mut e = eng();
        e.call("generate_beat", &json!({"style": "trap", "seed": 4}))
            .ok();
        e.call("snapshot", &json!({"name": "quiet"})).unwrap();
        e.call("set_mixer", &json!({"track": "master", "volume_db": -6.0}))
            .ok();
        let r = e
            .call("ab_compare", &json!({"a": "quiet", "write_wavs": false}))
            .unwrap();
        let (ga, gb) = (
            r["gain_db"]["a"].as_f64().unwrap(),
            r["gain_db"]["b"].as_f64().unwrap(),
        );
        assert!(ga.abs() < 0.2 || gb.abs() < 0.2, "{r}");
        assert!(r["verdict"].as_str().is_some());
    }
}
