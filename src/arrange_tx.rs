//! Rebuild a song from its transcriptions: the transcribe_* files (drums,
//! bass, chords, riffs) and an arrangement_map become a project with one
//! pattern per section, the parts on the engine's own instruments, and
//! optionally a raw vocal take placed on the timeline with a plain channel
//! chain (high-pass, light de-ess, gentle compression, reverb and delay sends).

use crate::engine::Engine;
use crate::tools::{f_opt, obj, s_opt, u_or, Tool};
use crate::transcribe as tr;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

fn call(e: &mut Engine, name: &str, args: Value) -> Result<Value> {
    e.call_from(name, &args, "arrange_transcription").map_err(|err| anyhow!("{name}: {err:#}"))
}

/// Chord label -> chord state (inverse of transcribe::chord_label).
fn chord_state(label: &str) -> Option<usize> {
    (0..12 * tr::QUALITIES.len()).find(|s| tr::chord_label(*s) == label)
}

/// (bar, step in 16ths, len in 16ths, pitch, vel 0-1) from any transcribe_* JSON.
fn notes_of(v: &Value, kind: &str) -> Result<Vec<(usize, f32, f32, u8, f32)>> {
    let steps = v["steps_per_bar"].as_u64().unwrap_or(16) as f32;
    let k = 16.0 / steps;
    let mut out = Vec::new();
    if let Some(hits) = v["hits"].as_array() {
        for h in hits {
            let kind_name = h["kind"].as_str().unwrap_or("");
            let Some(i) = tr::DRUM_KINDS.iter().position(|d| *d == kind_name) else { continue };
            let bar = h["bar"].as_u64().unwrap_or(1).max(1) as usize - 1;
            out.push((bar, h["step"].as_f64().unwrap_or(0.0) as f32 * k, 1.0, tr::DRUM_GM[i], h["vel"].as_f64().unwrap_or(90.0) as f32 / 127.0));
        }
    } else if let Some(ns) = v["notes"].as_array() {
        for n in ns {
            let bar = n["bar"].as_u64().unwrap_or(1).max(1) as usize - 1;
            out.push((bar, n["step"].as_f64().unwrap_or(0.0) as f32 * k, (n["len"].as_f64().unwrap_or(1.0) as f32 * k).max(0.25), n["midi"].as_u64().unwrap_or(60) as u8, n["vel"].as_f64().unwrap_or(90.0) as f32 / 127.0));
        }
    } else if let Some(pb) = v["per_beat"].as_array() {
        let bpb = v["grid"]["beats_per_bar"].as_u64().unwrap_or(4) as usize;
        let labels: Vec<String> = pb.iter().map(|x| x.as_str().unwrap_or("N").to_string()).collect();
        let mut i = 0;
        while i < labels.len() {
            let mut j = i + 1;
            // block chords, re-struck at each bar line
            while j < labels.len() && labels[j] == labels[i] && j % bpb != 0 {
                j += 1;
            }
            if let Some(s) = chord_state(&labels[i]) {
                for p in tr::voicing(s) {
                    out.push((i / bpb, ((i % bpb) * 4) as f32, ((j - i) * 4) as f32 - 0.2, p, 0.7));
                }
            }
            i = j;
        }
    } else {
        bail!("{kind}: not a transcribe_* file (no hits, notes or per_beat)");
    }
    Ok(out)
}


type Nt = (usize, f32, f32, u8, f32);

/// Majority-vote a section's bars into a repeating 1-, 2- or 4-bar part:
/// a note survives when it sits on the same step and pitch in at least half
/// the repeats, so a transcriber's stray hits and drop-outs vanish while the
/// played figure stays. A last bar that departs from the figure (a fill) is
/// kept as played. Notes are (bar within section, step, len, pitch, vel).
fn consolidate(ns: &[Nt], nb: usize) -> (Vec<Nt>, usize) {
    if nb < 2 || ns.is_empty() {
        return (ns.to_vec(), 0);
    }
    let key = |n: &Nt| -> (i32, u8) { ((n.1 * 2.0).round() as i32, n.3) };
    let f1 = |a: &std::collections::BTreeSet<(i32, u8)>, b: &std::collections::BTreeSet<(i32, u8)>| -> f32 {
        if a.is_empty() && b.is_empty() { return 1.0; }
        let i = a.intersection(b).count() as f32;
        2.0 * i / (a.len() + b.len()).max(1) as f32
    };
    let bar_set = |bar: usize| -> std::collections::BTreeSet<(i32, u8)> { ns.iter().filter(|n| n.0 == bar).map(key).collect() };
    let mut best: Option<(usize, f32, Vec<Nt>)> = None;
    // the last bar may be a fill: it votes only when the section is two bars long
    let vote_n = if nb >= 3 { nb - 1 } else { nb };
    for p in [1usize, 2, 4] {
        if p > 1 && p > vote_n / 2 { break; }
        let mut rep: Vec<Nt> = Vec::new();
        for ph in 0..p {
            let bars: Vec<usize> = (ph..vote_n).step_by(p).collect();
            let need = if bars.len() >= 2 { ((bars.len() as f32) * 0.5).ceil().max(2.0) as usize } else { 1 };
            let mut acc: std::collections::BTreeMap<(i32, u8), (usize, f32, f32, f32)> = std::collections::BTreeMap::new();
            for n in ns.iter().filter(|n| bars.contains(&n.0)) {
                let e = acc.entry(key(n)).or_insert((0, 0.0, 0.0, 0.0));
                e.0 += 1;
                e.1 += n.1;
                e.2 += n.2;
                e.3 += n.4;
            }
            for (k, (c, st, ln, vl)) in acc {
                if c >= need {
                    let c = c as f32;
                    rep.push((ph, st / c, ln / c, k.1, vl / c));
                }
            }
        }
        // how well the figure explains every bar
        let mut tot = 0.0;
        for b in 0..vote_n {
            let rs: std::collections::BTreeSet<(i32, u8)> = rep.iter().filter(|n| n.0 == b % p).map(key).collect();
            tot += f1(&rs, &bar_set(b));
        }
        let score = tot / vote_n as f32;
        let better = match &best { None => true, Some((_, s, _)) => score > *s + 0.04 };
        if better { best = Some((p, score, rep)); }
    }
    let Some((p, _, rep)) = best else { return (ns.to_vec(), 0) };
    let mut out: Vec<Nt> = Vec::new();
    let last = nb - 1;
    let rs_last: std::collections::BTreeSet<(i32, u8)> = rep.iter().filter(|n| n.0 == last % p).map(key).collect();
    let fill = f1(&rs_last, &bar_set(last)) < 0.6;
    for b in 0..nb {
        if b == last && fill {
            out.extend(ns.iter().filter(|n| n.0 == last).cloned());
            continue;
        }
        out.extend(rep.iter().filter(|n| n.0 == b % p).map(|n| (b, n.1, n.2, n.3, n.4)));
    }
    (out, p)
}

fn instrument(e: &mut Engine, track: &str, spec: &str, drums: bool) -> Result<String> {
    let (k, v) = spec.split_once(':').unwrap_or(("catalog", spec));
    match k {
        "catalog" => call(e, "load_instrument", json!({"track": track, "catalog_id": v}))?,
        "font" | "instrument" => call(e, "load_instrument", json!({"track": track, "instrument": v, "drums": drums}))?,
        "gm" => call(e, "load_instrument", json!({"track": track, "program": v.parse::<u32>().unwrap_or(0), "drums": drums}))?,
        "preset" => {
            if e.project.track_index(track).is_ok() {
                call(e, "set_instrument", json!({"track": track, "preset": v}))?
            } else {
                call(e, "add_track", json!({"name": track, "preset": v}))?
            }
        }
        other => bail!("instrument '{spec}': use catalog:<id>, font:<name>, gm:<program> or preset:<name> (got '{other}')"),
    };
    Ok(spec.to_string())
}

fn arrange(e: &mut Engine, a: &Value) -> Result<Value> {
    let parts = a.get("parts").and_then(|v| v.as_array()).cloned().ok_or_else(|| anyhow!("parts: [{{name, file, instrument}}] required"))?;
    if parts.is_empty() {
        bail!("parts is empty");
    }
    let mut loaded = Vec::new();
    let mut grid: Option<(f32, f32)> = None;
    let mut last_bar = 0usize;
    for p in &parts {
        let name = p["name"].as_str().ok_or_else(|| anyhow!("each part needs a name"))?.to_string();
        let file = e.resolve(p["file"].as_str().ok_or_else(|| anyhow!("{name}: file missing"))?);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).map_err(|err| anyhow!("{}: {err}", file.display()))?)?;
        let bpm = v["grid"]["bpm"].as_f64().unwrap_or(0.0) as f32;
        let db = v["grid"]["downbeat_s"].as_f64().unwrap_or(0.0) as f32;
        if let Some((b0, d0)) = grid {
            if (b0 - bpm).abs() > 0.5 || (d0 - db).abs() > 0.06 {
                bail!("{name}: its grid ({bpm} bpm, downbeat {db}) differs from the first part's ({b0}, {d0}); transcribe every part with the same grid_from");
            }
        } else if bpm > 0.0 {
            grid = Some((bpm, db));
        }
        let ns = notes_of(&v, &name)?;
        last_bar = last_bar.max(ns.iter().map(|n| n.0 + 1).max().unwrap_or(0));
        loaded.push((name, p.clone(), ns, v["hits"].is_array()));
    }
    let (bpm, downbeat) = grid.ok_or_else(|| anyhow!("no grid in the part files"))?;
    let bpm = f_opt(a, "bpm").unwrap_or(bpm);
    // sections: an arrangement_map file, or blocks of 8 bars
    let mut secs: Vec<(String, usize, usize)> = Vec::new();
    if let Some(f) = s_opt(a, "sections_file") {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(e.resolve(&f))?)?;
        for (i, s) in v["sections"].as_array().cloned().unwrap_or_default().iter().enumerate() {
            let bars = s["bars"].as_str().unwrap_or("");
            let (x, y) = bars.split_once('-').ok_or_else(|| anyhow!("sections_file: bars like \"1-8\" expected"))?;
            let (b0, b1): (usize, usize) = (x.trim().parse()?, y.trim().parse()?);
            if b1 >= b0 && b0 >= 1 {
                secs.push((format!("s{:02}_{}", i + 1, s["kind"].as_str().unwrap_or("section")), b0 - 1, b1 - b0 + 1));
            }
        }
    }
    if secs.is_empty() {
        let blk = u_or(a, "bars_per_pattern", 8).clamp(1, 32) as usize;
        let mut b = 0;
        while b < last_bar {
            let n = blk.min(last_bar - b);
            secs.push((format!("s{:02}", secs.len() + 1), b, n));
            b += n;
        }
    }
    if secs.is_empty() {
        bail!("no notes to arrange");
    }
    let name = s_opt(a, "name").unwrap_or_else(|| "rebuild".into());
    let mut np = json!({"name": name, "bpm": bpm, "patterns": secs.iter().map(|s| json!({"name": s.0, "bars": s.2})).collect::<Vec<_>>()});
    if let Some(k) = s_opt(a, "key") {
        let (root, scale) = k.split_once(' ').unwrap_or((k.as_str(), "major"));
        np["key"] = json!(root);
        np["scale"] = json!(scale);
    }
    call(e, "new_project", np)?;
    call(e, "set_arrangement", json!({"sections": secs.iter().map(|s| json!({"pattern": s.0, "repeats": 1})).collect::<Vec<_>>()}))?;
    let mut tracks = Vec::new();
    let mut periods: Vec<Value> = Vec::new();
    for (pname, spec, ns, is_drums) in &loaded {
        let inst = spec["instrument"].as_str().map(String::from).unwrap_or_else(|| if *is_drums { "gm:0".into() } else { "preset:keys".into() });
        let used = instrument(e, pname, &inst, *is_drums)?;
        if let Some(v) = spec["volume_db"].as_f64() {
            call(e, "set_mixer", json!({"track": pname, "volume_db": v}))?;
        }
        if let Some(v) = spec["pan"].as_f64() {
            call(e, "set_mixer", json!({"track": pname, "pan": v}))?;
        }
        let mut count = 0;
        for (sname, b0, nb) in &secs {
            let sec: Vec<Nt> = ns.iter().filter(|n| n.0 >= *b0 && n.0 < b0 + nb).map(|n| (n.0 - b0, n.1, n.2, n.3, n.4)).collect();
            let cons = spec["consolidate"].as_bool().unwrap_or_else(|| a.get("consolidate").and_then(|v| v.as_bool()).unwrap_or(false));
            let sec = if cons { let (c, per) = consolidate(&sec, *nb); periods.push(json!({"track": pname, "pattern": sname, "period_bars": per})); c } else { sec };
            let notes: Vec<Value> = sec
                .iter()
                .map(|n| json!({"start": n.0 as f32 * 16.0 + n.1, "len": n.2, "pitch": n.3, "vel": (n.4 * 100.0).round() / 100.0}))
                .collect();
            if !notes.is_empty() {
                count += notes.len();
                call(e, "add_notes", json!({"pattern": sname, "track": pname, "notes": notes}))?;
            }
        }
        tracks.push(json!({"track": pname, "instrument": used, "notes": count}));
    }
    // the vocal take, raw: start_s = where the take's first sample sits on the record's clock
    let mut vocal = Value::Null;
    if let Some(vp) = s_opt(a, "vocal") {
        let start_s = f_opt(a, "vocal_start_s").unwrap_or(0.0);
        let beat_s = 60.0 / bpm;
        let vpath = e.resolve(&vp).to_string_lossy().to_string();
        call(e, "import_sample", json!({"name": "take", "path": vpath}))?;
        call(e, "add_track", json!({"name": "vox", "preset": "pad", "volume_db": f_opt(a, "vocal_db").unwrap_or(0.0)}))?;
        let dur = e.project.samples.iter().find(|s| s.name == "take").map(|s| s.duration).unwrap_or(0.0);
        let rel = start_s - downbeat;
        let (start_beat, offset_s) = if rel >= 0.0 { (rel / beat_s, 0.0) } else { (0.0, -rel) };
        // vocal_phrases (align_phrases output: [{from_s, to_s, at_s}] or a file holding {"phrases": [...]}) puts
        // each line of the take where the record sings it; otherwise the take plays whole from vocal_start_s
        let phrases: Option<Vec<Value>> = match a.get("vocal_phrases") {
            Some(Value::Array(v)) => Some(v.clone()),
            Some(Value::String(f)) => {
                let txt = std::fs::read_to_string(e.resolve(f))?;
                let v: Value = serde_json::from_str(&txt)?;
                Some(v.get("phrases").and_then(|p| p.as_array()).cloned().ok_or_else(|| anyhow!("{} has no phrases", f))?)
            }
            _ => None,
        };
        let mut placed = 0usize;
        if let Some(ph) = &phrases {
            let n = ph.len();
            for (k, p) in ph.iter().enumerate() {
                let (Some(f0), Some(f1), Some(at)) = (p.get("from_s").and_then(|x| x.as_f64()), p.get("to_s").and_then(|x| x.as_f64()), p.get("at_s").and_then(|x| x.as_f64())) else { continue };
                let (f0, f1, at) = (f0 as f32, f1 as f32, at as f32);
                // a little air either side, never into the neighbouring phrases
                let prev_end = if k > 0 { ph[k - 1].get("to_s").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32 } else { 0.0 };
                let next_start = if k + 1 < n { ph[k + 1].get("from_s").and_then(|x| x.as_f64()).unwrap_or(dur as f64) as f32 } else { dur };
                let pre = 0.08f32.min(((f0 - prev_end) * 0.5).max(0.0));
                let post = 0.20f32.min(((next_start - f1) * 0.5).max(0.0));
                let (s0, s1) = ((f0 - pre).max(0.0), (f1 + post).min(dur));
                let t0 = at - (f0 - s0) - downbeat;
                let (sb, extra) = if t0 >= 0.0 { (t0 / beat_s, 0.0) } else { (0.0, -t0) };
                if s1 - s0 - extra < 0.1 { continue; }
                call(e, "add_audio_clip", json!({"sample": "take", "track": "vox", "start_beat": sb, "offset_s": s0 + extra, "length_s": s1 - s0 - extra, "fade_in_ms": 8, "fade_out_ms": 40}))?;
                placed += 1;
            }
        } else {
            call(e, "add_audio_clip", json!({"sample": "take", "track": "vox", "start_beat": start_beat, "offset_s": offset_s, "length_s": (dur - offset_s).max(0.1), "fade_in_ms": 5, "fade_out_ms": 30}))?;
        }
        call(e, "add_effect", json!({"track": "vox", "type": "parametric_eq", "params": {"bands": [{"kind": "low_cut", "freq": 85.0, "stages": 2}]}}))?;
        call(e, "add_effect", json!({"track": "vox", "type": "deesser", "params": {"freq": 6500.0, "threshold_db": -30.0, "range_db": 4.0, "release_ms": 60.0}}))?;
        call(e, "add_effect", json!({"track": "vox", "type": "compressor", "params": {"threshold_db": -20.0, "ratio": 2.5, "attack_ms": 10.0, "release_ms": 100.0, "makeup_db": 3.0, "knee_db": 6.0}}))?;
        call(e, "add_bus", json!({"name": "plate", "preset": "none"}))?;
        call(e, "add_effect", json!({"track": "plate", "type": "reverb", "params": {"size": 0.7, "mix": 1.0, "predelay_ms": 30.0, "mode": "fdn_plate", "decay_s": 1.8, "low_cut_hz": 300.0}}))?;
        call(e, "add_bus", json!({"name": "vdelay", "preset": "none"}))?;
        call(e, "add_effect", json!({"track": "vdelay", "type": "delay", "params": {"steps": 6.0, "feedback": 0.35, "mix": 1.0, "ping_pong": true, "tone": 4500.0}}))?;
        call(e, "set_send", json!({"track": "vox", "bus": "plate", "db": -16.0}))?;
        call(e, "set_send", json!({"track": "vox", "bus": "vdelay", "db": -25.0}))?;
        vocal = json!({"track": "vox", "start_beat": (start_beat * 100.0).round() / 100.0, "offset_s": (offset_s * 1000.0).round() / 1000.0, "phrases_placed": placed,
            "chain": "high-pass 85 Hz, de-ess, 2.5:1 compression, plate and delay sends (no denoise, gate, tuning or tone shaping)"});
    }
    Ok(json!({"project": name, "bpm": bpm, "sections": secs.iter().map(|s| json!({"pattern": s.0, "bars": format!("{}-{}", s.1 + 1, s.1 + s.2)})).collect::<Vec<_>>(),
        "tracks": tracks, "consolidated": periods, "vocal": vocal, "music_starts_at_s": (downbeat * 1000.0).round() / 1000.0,
        "next": ["match_timbre", "render", "score_sections", "balance_mix"]}))
}

pub fn tools() -> Vec<Tool> {
    vec![Tool {
        name: "arrange_transcription",
        description: "Build a project from transcriptions: parts=[{name, file (a transcribe_drums / transcribe_bass / transcribe_chords / transcribe_melody JSON), instrument (catalog:<id> | font:<name> | gm:<program> | preset:<name>), volume_db, pan}] become tracks, one pattern per section (sections_file = an arrangement_map JSON, else bars_per_pattern blocks), every note on its bar. vocal + vocal_start_s place a take on the timeline with a plain channel chain (high-pass, de-ess, compression, reverb and delay sends). All parts must share one grid (transcribe them with the same grid_from).",
        mutates: true,
        schema: || obj(json!({
            "parts": {"type": "array", "items": {"type": "object", "properties": {"name": {"type": "string"}, "file": {"type": "string"}, "instrument": {"type": "string"}, "volume_db": {"type": "number"}, "pan": {"type": "number"}}, "required": ["name", "file"]}},
            "sections_file": {"type": "string"},
            "bars_per_pattern": {"type": "integer"},
            "name": {"type": "string"}, "bpm": {"type": "number"}, "key": {"type": "string", "description": "e.g. \"D minor\""},
            "vocal": {"type": "string", "description": "a vocal take to place"},
            "vocal_start_s": {"type": "number", "description": "where the take's first sample sits on the transcription's clock (s)"},
            "vocal_db": {"type": "number"},
            "consolidate": {"type": "boolean", "description": "majority-vote each section's bars into its repeating 1/2/4-bar figure (cleans transcription noise, keeps a last-bar fill); per part too"},
            "vocal_phrases": {"anyOf": [{"type": "string"}, {"type": "array", "items": {"type": "object"}}], "description": "align_phrases output (its out file, or the phrases array): each line of the take placed where the record sings it, instead of one clip from vocal_start_s"}
        }), &["parts"]),
        run: arrange,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_labels_round_trip_and_notes_land_in_sections() {
        assert_eq!(chord_state("Dm").map(tr::chord_label).as_deref(), Some("Dm"));
        assert!(chord_state("N").is_none());
        let v = json!({"grid": {"bpm": 120, "downbeat_s": 0.0, "beats_per_bar": 4}, "steps_per_bar": 16,
            "notes": [{"bar": 1, "step": 0, "len": 4, "midi": 38, "vel": 100}, {"bar": 3, "step": 8, "len": 2, "midi": 41, "vel": 90}]});
        let dir = std::env::temp_dir().join("bb_arrange_tx");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bass.json"), v.to_string()).unwrap();
        let ch = json!({"grid": {"bpm": 120, "downbeat_s": 0.0, "beats_per_bar": 4}, "per_beat": ["Dm", "Dm", "Dm", "Dm", "A#", "A#", "F", "F"]});
        std::fs::write(dir.join("chords.json"), ch.to_string()).unwrap();
        let mut e = Engine::new(dir.clone());
        let r = e.call("arrange_transcription", &json!({"parts": [{"name": "bass", "file": "bass.json", "instrument": "preset:sub_bass"}, {"name": "keys", "file": "chords.json", "instrument": "preset:keys"}], "bars_per_pattern": 2})).unwrap();
        assert_eq!(r["sections"].as_array().unwrap().len(), 2, "{r}");
        let p2 = e.project.patterns.iter().find(|p| p.name == "s02").unwrap();
        assert!(p2.notes("bass").iter().any(|n| (n.start - 8.0).abs() < 1e-3 && n.pitch == 41), "{:?}", p2.notes("bass"));
        let p1 = e.project.patterns.iter().find(|p| p.name == "s01").unwrap();
        assert!(p1.notes("keys").iter().any(|n| (n.start - 16.0).abs() < 1e-3), "chords re-struck at bar 2");
    }
}

#[cfg(test)]
mod consolidate_tests {
    use super::*;
    #[test]
    fn majority_vote_removes_strays_and_keeps_fill() {
        // 4 bars of kick on 0,8 and snare on 4,12; a stray hat in bar 1, a dropped kick in bar 2, a fill in bar 3
        let mut ns: Vec<Nt> = Vec::new();
        for b in 0..4 {
            for st in [0.0, 8.0] { if !(b == 2 && st == 8.0) { ns.push((b, st, 1.0, 36, 0.9)); } }
            if b < 3 { for st in [4.0, 12.0] { ns.push((b, st, 1.0, 38, 0.8)); } }
        }
        ns.push((1, 6.0, 1.0, 42, 0.5));
        for st in [4.0, 10.0, 12.0, 13.0, 14.0, 15.0] { ns.push((3, st, 1.0, 45, 0.8)); }
        let (out, p) = consolidate(&ns, 4);
        assert_eq!(p, 1);
        assert!(!out.iter().any(|n| n.3 == 42), "stray hat removed");
        assert!(out.iter().any(|n| n.0 == 2 && n.3 == 36 && (n.1 - 8.0).abs() < 0.1), "dropped kick restored");
        assert!(out.iter().filter(|n| n.0 == 3 && n.3 == 45).count() == 6, "fill kept");
    }
}
