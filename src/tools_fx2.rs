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
                fxn.validate().map_err(|m| anyhow!("place_fx {t}: {m}. Nothing was changed."))?;
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
            description: "Loudness-matched A/B of two project versions (snapshot names or 'current'; default A = newest snapshot, B = current). Louder always sounds better, so the louder version is turned down to the quieter one's integrated LUFS before judging. Returns the gain applied, the matched score, crest, stereo correlation and spectral-band deltas (B minus A), a technical verdict (measurements only; taste is the listener's, see ab_listen), and two level-matched WAVs (ab_<a>.wav / ab_<b>.wav in the renders folder) to listen to blind.",
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
                    format!("B ('{bn}') scores {ds} points higher on the technical checks at matched loudness (a measurement, not taste: listen with ab_listen)")
                } else if ds <= -2 {
                    format!("A ('{an}') scores {} points higher on the technical checks at matched loudness (a measurement, not taste: listen with ab_listen)", -ds)
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
    use crate::render;

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

    #[test]
    fn freq_splitter_is_reachable_through_the_generic_effect_tools() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "bus", "preset": "acid_bass"}))
            .unwrap();
        let name = e.project.tracks[0].name.clone();
        let r = e
            .call(
                "add_effect",
                &json!({"track": name, "type": "freq_splitter"}),
            )
            .unwrap();
        assert_eq!(r["chain"][0], "freq_splitter", "{r}");
        assert_eq!(r["index"], 0, "{r}");
        assert_eq!(e.project.tracks[0].effects[0].type_name(), "freq_splitter");
        let r = e
            .call(
                "tweak_effect",
                &json!({"track": name, "index": 0, "params": {"band": "mid", "low_hz": 400.0, "high_hz": 3000.0, "taps": 257}}),
            )
            .unwrap();
        assert_eq!(r["band"], "mid", "{r}");
        assert_eq!(r["low_hz"], 400.0, "{r}");
        assert_eq!(r["high_hz"], 3000.0, "{r}");
        assert_eq!(r["taps"], 257, "{r}");
        assert!(e
            .call(
                "tweak_effect",
                &json!({"track": name, "index": 0, "params": {"band": "sub"}})
            )
            .is_err());
        assert!(e
            .call(
                "tweak_effect",
                &json!({"track": name, "index": 0, "params": {"bogus": 1}})
            )
            .is_err());
        assert!(e
            .call(
                "tweak_effect",
                &json!({"track": name, "index": 0, "params": {"taps": 1}})
            )
            .is_err());
        // a rejected tweak leaves the chain exactly as it was
        match &e.project.tracks[0].effects[0] {
            crate::fx::Effect::FreqSplitter(p) => assert_eq!(p.taps, 257),
            other => panic!("unexpected effect {}", other.type_name()),
        }
        let d = e
            .call("describe_effect", &json!({"track": name, "index": 0}))
            .unwrap();
        assert_eq!(d["type"], "freq_splitter", "{d}");
        let params = d["parameters"].to_string();
        for k in ["low_hz", "high_hz", "taps", "band", "gain_db", "mix"] {
            assert!(params.contains(k), "describe_effect misses {k}: {d}");
        }
        let d = e
            .call("describe_effect", &json!({"type": "freq_splitter"}))
            .unwrap();
        assert_eq!(d["type"], "freq_splitter", "{d}");
    }

    fn fx_render_bits(fx: &crate::fx::Effect) -> Vec<u32> {
        let n = 4096usize;
        let l0: Vec<f32> = (0..n)
            .map(|i| ((i as f32) * 0.037).sin() * 0.5 + ((i as f32) * 0.0021).cos() * 0.2)
            .collect();
        let r0: Vec<f32> = l0.iter().rev().copied().collect();
        let (mut l, mut r) = (l0, r0);
        let triggers = std::collections::HashMap::new();
        let ctx = crate::fx::FxContext {
            step_secs: 0.1,
            triggers: &triggers,
            offset: 0,
            levels: None,
        };
        fx.process(&mut l, &mut r, &ctx);
        l.iter().chain(&r).map(|v| v.to_bits()).collect()
    }

    /// One effect type with a validate() rule: a rejected tweak leaves the
    /// effect bit-identical (params, serialized state, render), says so
    /// truthfully, and a valid tweak equals the effect built straight from
    /// the merged params. One #[test] per effect (macro below) so a failure
    /// names the effect.
    fn check_rejected_tweak(kind: &str, bad: Value, good: Value) {
        let mut e = eng();
        e.call("add_track", &json!({"name": "t", "preset": "acid_bass"}))
            .unwrap();
        let name = e.project.tracks[0].name.clone();
        e.call("add_effect", &json!({"track": name, "type": kind}))
            .unwrap();
        assert_eq!(e.project.tracks[0].effects.len(), 1, "{kind}: fresh track");
        assert_eq!(
            e.project.tracks[0].effects[0].type_name(),
            kind,
            "the effect under test must be at index 0"
        );
        let before_json = serde_json::to_value(&e.project.tracks[0].effects[0]).unwrap();
        let before_bits = fx_render_bits(&e.project.tracks[0].effects[0]);
        let err = e
            .call(
                "tweak_effect",
                &json!({"track": name, "index": 0, "params": bad}),
            )
            .expect_err(kind)
            .to_string();
        assert!(err.contains("Nothing was changed"), "{kind}: {err}");
        let after_json = serde_json::to_value(&e.project.tracks[0].effects[0]).unwrap();
        assert_eq!(
            before_json, after_json,
            "{kind}: state changed by a rejected tweak"
        );
        assert_eq!(
            before_bits,
            fx_render_bits(&e.project.tracks[0].effects[0]),
            "{kind}: render changed by a rejected tweak"
        );
        // valid tweak: same effect as building it directly
        e.call(
            "tweak_effect",
            &json!({"track": name, "index": 0, "params": good}),
        )
        .unwrap();
        let mut v = before_json.clone();
        if let (Some(o), Some(g)) = (v.as_object_mut(), good.as_object()) {
            for (k, x) in g {
                o.insert(k.clone(), x.clone());
            }
        }
        let direct: crate::fx::Effect = serde_json::from_value(v).unwrap();
        assert_eq!(
            fx_render_bits(&e.project.tracks[0].effects[0]),
            fx_render_bits(&direct),
            "{kind}: valid tweak renders differently from the direct build"
        );
    }

    macro_rules! rejected_tweak_test {
        ($name:ident, $kind:expr, $bad:expr, $good:expr) => {
            #[test]
            fn $name() {
                check_rejected_tweak($kind, $bad, $good);
            }
        };
    }

    rejected_tweak_test!(
        freq_splitter_rejected_tweak_changes_nothing,
        "freq_splitter",
        json!({"taps": 1}),
        json!({"low_hz": 300.0})
    );
    rejected_tweak_test!(
        granular_rejected_tweak_changes_nothing,
        "granular",
        json!({"mix": 2.0}),
        json!({"mix": 0.3})
    );
    rejected_tweak_test!(
        freq_shift_rejected_tweak_changes_nothing,
        "freq_shift",
        json!({"shift_hz": 9000.0}),
        json!({"shift_hz": 50.0})
    );
    rejected_tweak_test!(
        ring_mod_rejected_tweak_changes_nothing,
        "ring_mod",
        json!({"shape": "nope"}),
        json!({"freq_hz": 200.0})
    );
    rejected_tweak_test!(
        stereo_shaper_rejected_tweak_changes_nothing,
        "stereo_shaper",
        json!({"delay_ms": 99.0}),
        json!({"delay_ms": 10.0})
    );
    rejected_tweak_test!(
        vinyl_rejected_tweak_changes_nothing,
        "vinyl",
        json!({"age": 2.0}),
        json!({"age": 0.5})
    );
    rejected_tweak_test!(
        waveshaper_rejected_tweak_changes_nothing,
        "waveshaper",
        json!({"drive_db": 99.0}),
        json!({"drive_db": 6.0})
    );
    rejected_tweak_test!(
        vocoder_rejected_tweak_changes_nothing,
        "vocoder",
        json!({"bands": 2}),
        json!({"bands": 16})
    );
    rejected_tweak_test!(
        gross_beat_rejected_tweak_changes_nothing,
        "gross_beat",
        json!({"time": "zzz_not_a_preset"}),
        json!({})
    );

    /// A project file whose freq_splitter carries taps 4294967295 (a few
    /// bytes of JSON) must be refused on load, fast, with the project left
    /// untouched. Nothing sized by `taps` may be allocated on this path.
    #[test]
    fn load_project_rejects_hostile_effect_params_and_changes_nothing() {
        let dir = std::env::temp_dir().join("bb_fx2_hostile_load");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = eng();
        e.call("add_track", &json!({"name": "keep", "preset": "acid_bass"}))
            .unwrap();
        e.call(
            "add_effect",
            &json!({"track": "master", "type": "freq_splitter"}),
        )
        .unwrap();
        // the engine starts with default master effects; freq_splitter is last
        let fs = e.project.master_effects.len() - 1;
        assert_eq!(e.project.master_effects[fs].type_name(), "freq_splitter");
        let good = serde_json::to_value(&e.project).unwrap();
        let before = e.project.clone();
        let revision = e.revision;
        let snapshots_before = e.snapshots.len();

        let cases = [
            ("master", {
                let mut v = good.clone();
                v["master_effects"][fs]["taps"] = json!(4294967295u64);
                v
            }),
            ("track", {
                let mut v = good.clone();
                let mut fx = good["master_effects"][fs].clone();
                fx["taps"] = json!(4294967295u64);
                v["tracks"][0]["effects"] = json!([fx]);
                v
            }),
        ];
        for (what, v) in cases {
            let path = dir.join(format!("hostile_{what}.json"));
            std::fs::write(&path, v.to_string()).unwrap();
            let t0 = std::time::Instant::now();
            let err = e
                .call("load_project", &json!({"path": path}))
                .expect_err("hostile project must be refused");
            assert!(
                t0.elapsed() < std::time::Duration::from_secs(2),
                "{what}: refusal must be fast"
            );
            let msg = format!("{err:#}");
            assert!(msg.contains("taps"), "{what}: {msg}");
            assert!(msg.contains("freq_splitter"), "{what}: {msg}");
            assert_eq!(e.project, before, "{what}: project must be unchanged");
            assert_eq!(e.revision, revision, "{what}: revision must not move");
            assert_eq!(
                e.snapshots.len(),
                snapshots_before,
                "{what}: snapshots must be unchanged"
            );
        }

        // the untouched good file still loads
        let path = dir.join("good.json");
        std::fs::write(&path, good.to_string()).unwrap();
        e.call("load_project", &json!({"path": path})).unwrap();
        assert_eq!(e.project.master_effects.len(), fs + 1);
    }

    /// A snapshot sidecar entry with a hostile effect is skipped and counted;
    /// valid entries still load.
    #[test]
    fn load_project_skips_and_counts_snapshots_with_invalid_effects() {
        let dir = std::env::temp_dir().join("bb_fx2_hostile_snap");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = eng();
        e.call("add_track", &json!({"name": "keep", "preset": "acid_bass"}))
            .unwrap();
        e.call(
            "add_effect",
            &json!({"track": "master", "type": "freq_splitter"}),
        )
        .unwrap();
        let fs = e.project.master_effects.len() - 1;
        assert_eq!(e.project.master_effects[fs].type_name(), "freq_splitter");
        let good = serde_json::to_value(&e.project).unwrap();
        let mut bad = good.clone();
        bad["master_effects"][fs]["taps"] = json!(4294967295u64);
        let path = dir.join("p.json");
        std::fs::write(&path, good.to_string()).unwrap();
        let snaps = json!([
            {"name": "bad", "project": bad, "note": "", "revision": 1},
            {"name": "ok", "project": good, "note": "", "revision": 2}
        ]);
        std::fs::write(crate::tools::snapshots_path(&path), snaps.to_string()).unwrap();
        let r = e.call("load_project", &json!({"path": path})).unwrap();
        assert_eq!(r["snapshots_loaded"], 1, "{r}");
        assert_eq!(r["snapshots_skipped_invalid_effects"], 1, "{r}");
        assert_eq!(e.snapshots.len(), 1);
        assert_eq!(e.snapshots[0].0, "ok");
    }

    /// set_parameters and place_fx must validate before they change anything.
    #[test]
    fn set_parameters_and_place_fx_reject_invalid_effect_params() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "keep", "preset": "acid_bass"}))
            .unwrap();
        let name = e.project.tracks[0].name.clone();
        e.call(
            "add_effect",
            &json!({"track": name, "type": "freq_splitter"}),
        )
        .unwrap();
        assert_eq!(e.project.tracks[0].effects.len(), 1);
        assert_eq!(e.project.tracks[0].effects[0].type_name(), "freq_splitter");
        let before = e.project.clone();
        let err = e
            .call(
                "set_parameters",
                &json!({"track": name, "changes": {"fx.0.taps": 4294967295u64}}),
            )
            .expect_err("taps out of range must be refused");
        assert!(format!("{err:#}").contains("taps"), "{err:#}");
        assert_eq!(e.project, before, "set_parameters must change nothing");
        let err = e
            .call(
                "place_fx",
                &json!({"track": name, "type": "freq_splitter", "params": {"taps": 1}}),
            )
            .expect_err("taps 1 must be refused");
        assert!(format!("{err:#}").contains("taps"), "{err:#}");
        assert_eq!(e.project, before, "place_fx must change nothing");
        // a valid value still applies
        e.call(
            "set_parameters",
            &json!({"track": name, "changes": {"fx.0.taps": 129}}),
        )
        .unwrap();
        assert_ne!(e.project, before);
    }

    /// Every effect type's default parameter set must still pass the new
    /// validation, place through place_fx unchanged, and survive a save and
    /// load with identical serialized bytes. Guards against validation
    /// rejecting something that used to work.
    #[test]
    fn every_default_effect_passes_validate_place_fx_and_load_unchanged() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "keep", "preset": "acid_bass"}))
            .unwrap();
        let name = e.project.tracks[0].name.clone();
        let mut expected = Vec::new();
        for (kind, _) in fx::EFFECT_TYPES {
            let direct = fx::Effect::from_type(kind).unwrap_or_else(|| panic!("{kind}"));
            direct.validate().unwrap_or_else(|m| panic!("{kind}: {m}"));
            let before = serde_json::to_value(&direct).unwrap();
            if kind == &"sidechain" {
                // needs a source track; the direct validate above still ran
                continue;
            }
            e.call("place_fx", &json!({"track": name, "type": kind}))
                .unwrap_or_else(|err| panic!("place_fx {kind}: {err:#}"));
            let placed = e.project.tracks[0].effects.last().unwrap();
            let mut after = serde_json::to_value(placed).unwrap();
            let mut want = before.clone();
            // ids are assigned by the engine, not part of the parameter set
            after.as_object_mut().unwrap().remove("id");
            want.as_object_mut().unwrap().remove("id");
            assert_eq!(after, want, "{kind}: place_fx changed the parameter set");
            expected.push(*kind);
        }
        assert!(
            expected.len() > 20,
            "expected many effect types: {}",
            expected.len()
        );
        e.project.validate_effects().unwrap();
        let bytes = serde_json::to_string(&e.project).unwrap();
        let path = std::env::temp_dir().join("bb_fx2_every_default.json");
        std::fs::write(&path, &bytes).unwrap();
        e.call("load_project", &json!({"path": path})).unwrap();
        assert_eq!(
            serde_json::to_string(&e.project).unwrap(),
            bytes,
            "load_project changed the serialized project"
        );
    }

    /// Validation only reads parameters, so a project must render to the
    /// same bits whether it came from the live engine or through the
    /// validating load path. Renders a one-bar pattern through one of every
    /// effect type (default parameters) both ways and compares every sample.
    /// Same build on both sides; no golden file is read or changed.
    #[test]
    fn load_validate_does_not_change_render_bits_for_every_default_effect() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "keep", "preset": "acid_bass"}))
            .unwrap();
        e.call(
            "add_notes",
            &json!({"track": "keep", "pattern": "A", "replace": true, "notes": [
                {"start": 0, "len": 4, "pitch": "C2", "vel": 0.9},
                {"start": 8, "len": 4, "pitch": "G2", "vel": 0.8}
            ]}),
        )
        .unwrap();
        let dry = render::render(&e.project, &e.bank, &render::RenderOptions::default()).unwrap();
        assert!(
            dry.left.iter().any(|x| *x != 0.0),
            "the dry pattern must make sound for this comparison to mean anything"
        );
        for (kind, _) in fx::EFFECT_TYPES {
            if kind == &"sidechain" {
                continue;
            }
            e.call("place_fx", &json!({"track": "keep", "type": kind}))
                .unwrap_or_else(|err| panic!("place_fx {kind}: {err:#}"));
        }
        let before =
            render::render(&e.project, &e.bank, &render::RenderOptions::default()).unwrap();
        let path = std::env::temp_dir().join("bb_fx2_render_bits.json");
        std::fs::write(&path, serde_json::to_string(&e.project).unwrap()).unwrap();
        e.call("load_project", &json!({"path": path})).unwrap();
        let after = render::render(&e.project, &e.bank, &render::RenderOptions::default()).unwrap();
        assert_eq!(before.left.len(), after.left.len());
        assert_eq!(before.right.len(), after.right.len());
        let same = |a: &[f32], b: &[f32]| a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits());
        assert!(same(&before.left, &after.left), "left channel bits changed");
        assert!(
            same(&before.right, &after.right),
            "right channel bits changed"
        );
    }

    /// An automation lane may not drive an effect parameter to a value
    /// `Effect::validate` rejects. 4e9 taps is far past the 65537 bound (and
    /// would ask for ~16 GB at render); the test never renders, it only
    /// asserts the lane is refused when created.
    #[test]
    fn add_automation_refuses_a_lane_that_drives_taps_out_of_range() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "keep", "preset": "acid_bass"}))
            .unwrap();
        e.call(
            "add_effect",
            &json!({"track": "keep", "type": "freq_splitter"}),
        )
        .unwrap();
        assert_eq!(e.project.tracks[0].effects[0].type_name(), "freq_splitter");
        let before = e.project.clone();
        let r = e.call(
            "add_automation",
            &json!({"track": "keep", "param": "fx.0.taps",
                    "points": [{"beat": 0, "value": 4.0e9}, {"beat": 4, "value": 4.0e9}]}),
        );
        assert!(r.is_err(), "lane to 4e9 taps was accepted: {r:?}");
        assert_eq!(e.project, before, "a refused lane must change nothing");
    }

    /// The same lane inside a project file is refused by load_project.
    #[test]
    fn load_project_refuses_a_lane_that_drives_taps_out_of_range() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "keep", "preset": "acid_bass"}))
            .unwrap();
        e.call(
            "add_effect",
            &json!({"track": "keep", "type": "freq_splitter"}),
        )
        .unwrap();
        e.call(
            "add_automation",
            &json!({"track": "keep", "param": "fx.0.taps",
                    "points": [{"beat": 0, "value": 300}, {"beat": 4, "value": 400}]}),
        )
        .unwrap();
        let mut v = serde_json::to_value(&e.project).unwrap();
        v["automation"][0]["points"][1]["value"] = json!(4.0e9);
        let dir = std::env::temp_dir().join("bb_fx2_hostile_lane");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lane.json");
        std::fs::write(&path, v.to_string()).unwrap();
        let before = e.project.clone();
        let r = e.call("load_project", &json!({"path": path}));
        assert!(r.is_err(), "project with a 4e9-taps lane was loaded");
        assert_eq!(e.project, before, "a refused load must change nothing");
    }

    /// A lane that stays inside the validated range is still accepted, both
    /// through the tool and through load_project, and a lane on an effect
    /// without a validate rule or a parameter that does not exist is left
    /// alone (the render's existing behaviour).
    #[test]
    fn in_range_lanes_and_unvalidated_effects_still_load() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "keep", "preset": "acid_bass"}))
            .unwrap();
        e.call(
            "add_effect",
            &json!({"track": "keep", "type": "freq_splitter"}),
        )
        .unwrap();
        e.call(
            "add_effect",
            &json!({"track": "keep", "type": "compressor"}),
        )
        .unwrap();
        e.call(
            "add_automation",
            &json!({"track": "keep", "param": "fx.0.taps",
                    "points": [{"beat": 0, "value": 129}, {"beat": 4, "value": 4097}]}),
        )
        .unwrap();
        e.call(
            "add_automation",
            &json!({"track": "keep", "param": "fx.1.threshold_db",
                    "points": [{"beat": 0, "value": -40}, {"beat": 4, "value": -3}]}),
        )
        .unwrap();
        e.project.validate_lanes().unwrap();
        let bytes = serde_json::to_string(&e.project).unwrap();
        let path = std::env::temp_dir().join("bb_fx2_inrange_lane.json");
        std::fs::write(&path, &bytes).unwrap();
        e.call("load_project", &json!({"path": path})).unwrap();
        assert_eq!(serde_json::to_string(&e.project).unwrap(), bytes);
    }
}
