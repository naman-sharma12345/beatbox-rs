//! `ab_listen`: two versions of the song (snapshots, "current", or audio
//! files), cut to the same region and matched in loudness, written as clips
//! for a listener to judge: a person with headphones, or a model that can
//! hear audio (the clips can ride along as MCP audio content).
//!
//! It never picks a winner. Loudness, peak and spectrum describe level and
//! tone balance, not whether the music is better; those facts are returned
//! as facts, and the judgement field stays empty for the listener. Blind
//! mode hides which version is which until `reveal` is called, so the
//! listener is not steered by the names either.

use crate::analysis;
use crate::dsp::{db_to_gain, SR};
use crate::engine::Engine;
use crate::render::{self, RenderOptions};
use crate::tools::{b_or, f_opt, obj, s_opt, Tool};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

/// Audio of one side: (left, right).
fn side(e: &mut Engine, what: &str, bars: Option<(f32, f32)>, secs: (f32, Option<f32>)) -> Result<(Vec<f32>, Vec<f32>)> {
    let path = e.resolve(what);
    let (mut l, mut r) = if what != "current" && path.is_file() {
        let x = crate::samples::decode_file(&path)?;
        (x.clone(), x)
    } else {
        let p = e.version(what)?;
        e.bank.sync(&p.samples);
        let step_range = bars.map(|(b0, b1)| ((p.beat_at_bar(b0) * 4.0).round() as u32, (p.beat_at_bar(b1) * 4.0).round() as u32));
        let m = render::render(&p, &e.bank, &RenderOptions { step_range, tail: 1.0, ..Default::default() })?;
        (m.left, m.right)
    };
    let s0 = ((secs.0.max(0.0) * SR) as usize).min(l.len());
    let s1 = secs.1.map(|n| (s0 + (n.max(0.1) * SR) as usize).min(l.len())).unwrap_or(l.len());
    l = l[s0..s1].to_vec();
    r = r[s0..s1.min(r.len())].to_vec();
    if l.is_empty() {
        bail!("'{what}' has no audio in that region");
    }
    Ok((l, r))
}

fn keys_dir(e: &Engine) -> PathBuf {
    e.workdir.join("ab_listen_keys")
}

pub fn tools() -> Vec<Tool> {
    vec![Tool {
        name: "ab_listen",
        description: "Render two versions (snapshot names, 'current', or audio file paths) over the same region, loudness-matched, as A.wav / B.wav plus A_then_B.wav (A, a second of silence, B) for a listener: a person, or a model that hears audio (attach_audio sends the clips as MCP audio). Returns level facts only and never names a winner: loudness and spectrum are not taste, so the judgement is the listener's. blind hides which version is which until reveal {session}.",
        mutates: false,
        schema: || obj(json!({
            "a": {"type": "string", "description": "snapshot name, 'current' or an audio file (default: newest snapshot)"},
            "b": {"type": "string", "description": "same (default 'current')"},
            "from_bar": {"type": "number", "description": "1-based bar where the region starts (project versions)"},
            "to_bar": {"type": "number", "description": "bar where it ends (exclusive)"},
            "start_s": {"type": "number", "description": "seconds into the rendered region / file to start (default 0)"},
            "length_s": {"type": "number", "description": "seconds to keep (default 20)"},
            "blind": {"type": "boolean", "description": "shuffle which version is A and which is B (default false)"},
            "attach_audio": {"type": "boolean", "description": "send both clips as audio content (default false)"},
            "reveal": {"type": "string", "description": "a blind session id: say which version was A and which was B"}
        }), &[]),
        run: |e, a| {
            if let Some(sid) = s_opt(a, "reveal") {
                let f = keys_dir(e).join(format!("{sid}.json"));
                let v: Value = serde_json::from_slice(&std::fs::read(&f).map_err(|_| anyhow!("no blind session '{sid}'"))?)?;
                return Ok(json!({"session": sid, "A": v["A"], "B": v["B"]}));
            }
            let an = s_opt(a, "a")
                .or_else(|| e.snapshots.last().map(|(n, _)| n.clone()))
                .ok_or_else(|| anyhow!("nothing to compare against: pass `a` (a snapshot, 'current' or a file) or take a snapshot first"))?;
            let bn = s_opt(a, "b").unwrap_or_else(|| "current".into());
            let bars = match (f_opt(a, "from_bar"), f_opt(a, "to_bar")) {
                (Some(x), Some(y)) if y > x => Some((x, y)),
                (Some(x), None) => Some((x, x + 8.0)),
                (None, None) => None,
                _ => bail!("to_bar must be after from_bar"),
            };
            let secs = (f_opt(a, "start_s").unwrap_or(0.0), Some(f_opt(a, "length_s").unwrap_or(20.0).clamp(1.0, 120.0)));
            let (mut sa, mut sb) = (side(e, &an, bars, secs)?, side(e, &bn, bars, secs)?);
            let (la, lb) = (analysis::loudness(&sa.0, &sa.1), analysis::loudness(&sb.0, &sb.1));
            // match to the quieter one: louder always sounds "better" at first
            let target = la.integrated_lufs.min(lb.integrated_lufs);
            let (ga, gb) = ((target - la.integrated_lufs).clamp(-24.0, 0.0), (target - lb.integrated_lufs).clamp(-24.0, 0.0));
            for (s, g) in [(&mut sa, ga), (&mut sb, gb)] {
                let k = db_to_gain(g);
                s.0.iter_mut().chain(s.1.iter_mut()).for_each(|x| *x *= k);
            }
            let blind = b_or(a, "blind", false);
            let flip = blind && (crate::creative::fresh_seed() & 1 == 1);
            let (first, second, fname, sname) = if flip { (&sb, &sa, &bn, &an) } else { (&sa, &sb, &an, &bn) };
            let sid = format!("ab{}", crate::creative::fresh_seed() % 1_000_000);
            let dir = e.renders_dir().join("ab_listen").join(&sid);
            std::fs::create_dir_all(&dir)?;
            let (pa, pb, pab) = (dir.join("A.wav"), dir.join("B.wav"), dir.join("A_then_B.wav"));
            render::write_wav(&pa, &first.0, &first.1)?;
            render::write_wav(&pb, &second.0, &second.1)?;
            let gap = vec![0.0f32; SR as usize];
            let cat = |x: &Vec<f32>, y: &Vec<f32>| [x.as_slice(), &gap, y.as_slice()].concat();
            render::write_wav(&pab, &cat(&first.0, &second.0), &cat(&first.1, &second.1))?;
            if blind {
                std::fs::create_dir_all(keys_dir(e))?;
                std::fs::write(keys_dir(e).join(format!("{sid}.json")), json!({"A": fname, "B": sname}).to_string())?;
            }
            let facts = |l: &[f32], r: &[f32]| {
                let lu = analysis::loudness(l, r);
                let peak = l.iter().chain(r.iter()).fold(0.0f32, |m, x| m.max(x.abs()));
                json!({"lufs": (lu.integrated_lufs * 10.0).round() / 10.0, "peak_dbfs": ((20.0 * peak.max(1e-9).log10()) * 10.0).round() / 10.0,
                    "seconds": ((l.len() as f32 / SR) * 100.0).round() / 100.0})
            };
            let r1 = |x: f32| (x * 10.0).round() / 10.0;
            let mut out = json!({
                "session": sid,
                "A": if blind { json!("hidden (blind)") } else { json!(fname) },
                "B": if blind { json!("hidden (blind)") } else { json!(sname) },
                "files": {"A": pa.to_string_lossy(), "B": pb.to_string_lossy(), "A_then_B": pab.to_string_lossy()},
                "matched_to_lufs": r1(target),
                "level_change_db": if flip { json!({"A": r1(gb), "B": r1(ga)}) } else { json!({"A": r1(ga), "B": r1(gb)}) },
                "facts": {"A": facts(&first.0, &first.1), "B": facts(&second.0, &second.1)},
                "judgement": null,
                "how_to_judge": "Listen to A_then_B (or A and B) at one comfortable volume. Which feels better and why: groove, vocal clarity, emotion, punch, space. These numbers describe level, not taste; they never decide.",
                "next": if blind { format!("after listening: ab_listen {{\"reveal\": \"{sid}\"}}") } else { "keep the version the listener prefers (restore_snapshot)".into() },
            });
            if b_or(a, "attach_audio", false) {
                crate::media::attach(&mut out, vec![
                    crate::media::audio_block(&crate::media::wav_bytes(&first.0, &first.1, SR as u32), "audio/wav"),
                    crate::media::audio_block(&crate::media::wav_bytes(&second.0, &second.1, SR as u32), "audio/wav"),
                ]);
            }
            Ok(out)
        },
    }]
}

#[cfg(test)]
mod tests {
    use crate::engine::Engine;
    use serde_json::json;

    #[test]
    fn ab_listen_matches_levels_and_never_picks_a_winner() {
        let dir = std::env::temp_dir().join(format!("bb_ab_listen_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("new_project", &json!({"name": "ab", "bpm": 120})).unwrap();
        e.call("add_track", &json!({"name": "keys", "preset": "pad", "volume_db": 0})).unwrap();
        e.call("add_notes", &json!({"track": "keys", "notes": [{"start": 0, "len": 16, "pitch": 60}, {"start": 16, "len": 16, "pitch": 67}]})).unwrap();
        e.call("snapshot", &json!({"name": "quiet"})).unwrap();
        e.call("set_mixer", &json!({"track": "keys", "volume_db": 6})).unwrap();
        let v = e.call("ab_listen", &json!({"a": "quiet", "b": "current", "length_s": 3})).unwrap();
        assert!(v["judgement"].is_null(), "{v}");
        assert!(!v.to_string().to_lowercase().contains("wins"));
        let (la, lb) = (v["facts"]["A"]["lufs"].as_f64().unwrap(), v["facts"]["B"]["lufs"].as_f64().unwrap());
        assert!((la - lb).abs() < 0.3, "matched: {la} vs {lb}");
        assert!(v["level_change_db"]["B"].as_f64().unwrap() < -3.0, "the louder one was turned down");
        for k in ["A", "B", "A_then_B"] {
            assert!(std::path::Path::new(v["files"][k].as_str().unwrap()).is_file());
        }
        // blind: names hidden until reveal
        let b = e.call("ab_listen", &json!({"a": "quiet", "b": "current", "length_s": 2, "blind": true})).unwrap();
        assert_eq!(b["A"], "hidden (blind)");
        let r = e.call("ab_listen", &json!({"reveal": b["session"]})).unwrap();
        let mut got = vec![r["A"].as_str().unwrap().to_string(), r["B"].as_str().unwrap().to_string()];
        got.sort();
        assert_eq!(got, vec!["current", "quiet"]);
    }
}
