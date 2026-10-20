//! Real recorded instruments: fetch a SoundFont into the library, list its
//! presets, put one on a track, and preview it.

use crate::dsp::SR;
use crate::instruments::Instrument;
use crate::project::Track;
use crate::soundfont::{self, SoundFontParams};
use crate::tools::{b_or, f_opt, obj, s_opt, s_req, Tool};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

/// Queue a one-shot preview of stereo audio on the attached studio's speakers.
/// Returns the fields every "play" answer carries.
pub fn queue_audition(
    e: &mut crate::engine::Engine,
    l: Vec<f32>,
    r: Vec<f32>,
    gain_db: f32,
) -> Value {
    let g = 10f32.powf(gain_db.clamp(-60.0, 12.0) / 20.0);
    let secs = l.len() as f32 / SR;
    let buf = std::sync::Arc::new((
        l.into_iter().map(|v| v * g).collect::<Vec<f32>>(),
        r.into_iter().map(|v| v * g).collect::<Vec<f32>>(),
    ));
    e.transport.pending_audition = Some(Some(buf));
    let attached = e.transport.attached;
    json!({"playing": attached, "play_seconds": (secs * 10.0).round() / 10.0, "heard_in": if attached { "studio speakers" } else { "nowhere: no studio window is attached; open the WAV path to listen" }})
}

fn merge(mut v: Value, extra: Value) -> Value {
    if let (Some(o), Some(x)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in x {
            o.insert(k.clone(), val.clone());
        }
    }
    v
}

/// A short phrase that shows off a built-in preset: drum presets play a groove
/// of single hits, everything else an arpeggio then a held chord around root.
pub fn preset_phrase(inst: &Instrument, root: f32) -> Vec<Value> {
    let mut notes = Vec::new();
    if matches!(inst, Instrument::Drum(_)) {
        for bar in 0..2u32 {
            for st in [0.0f32, 4.0, 8.0, 10.0, 12.0] {
                notes.push(json!({"start": (bar * 16) as f32 + st, "pitch": 60, "len": 2, "vel": if st == 0.0 { 0.95 } else { 0.8 }}));
            }
        }
        return notes;
    }
    let c = root.round().clamp(12.0, 108.0) as u8;
    for (i, iv) in [0u8, 4, 7, 12, 7, 4].iter().enumerate() {
        notes.push(
            json!({"start": i as f32 * 2.0, "pitch": (c + iv).min(127), "len": 2, "vel": 0.75}),
        );
    }
    for iv in [0u8, 4, 7] {
        notes.push(json!({"start": 16.0, "pitch": (c + iv).min(127), "len": 14, "vel": 0.7}));
    }
    notes
}

/// Render a built-in preset's phrase in a scratch engine (the project is untouched).
pub fn render_preset_preview(
    name: &str,
    root: Option<f32>,
) -> Result<(Vec<f32>, Vec<f32>, &'static str)> {
    let inst = crate::instruments::preset(name)
        .ok_or_else(|| anyhow!("unknown preset '{name}' (list_presets)"))?;
    let scratch = std::env::temp_dir().join(format!(
        "beatbox_hear_{}_{}",
        std::process::id(),
        crate::samples::sample_name(name)
    ));
    let mut s = crate::engine::Engine::new(scratch.clone());
    s.project.bpm = 120.0;
    s.project.tracks.clear();
    if let Some(p) = s.project.patterns.first_mut() {
        p.bars = 2;
        p.clips.clear();
    } else {
        s.project
            .patterns
            .push(crate::project::Pattern::new("main", 2));
    }
    s.project.arrangement.clear();
    let r = (|| -> Result<(Vec<f32>, Vec<f32>)> {
        s.call(
            "add_track",
            &json!({"name": "hear", "preset": name, "volume_db": 0.0}),
        )?;
        let dflt = if matches!(inst, Instrument::Bass808(_)) {
            36.0
        } else {
            52.0
        };
        s.call(
            "add_notes",
            &json!({"track": "hear", "notes": preset_phrase(&inst, root.unwrap_or(dflt))}),
        )?;
        let m = s.mix()?;
        Ok((m.left.clone(), m.right.clone()))
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    let (l, rr) = r?;
    Ok((
        l,
        rr,
        if matches!(inst, Instrument::Drum(_)) {
            "groove"
        } else {
            "arpeggio + chord"
        },
    ))
}

fn params_from(a: &Value) -> Result<SoundFontParams> {
    let mut p = match s_opt(a, "instrument") {
        Some(n) => soundfont::alias(&n).ok_or_else(|| {
            anyhow!(
                "unknown instrument '{n}'. Names: {}",
                soundfont::ALIASES
                    .iter()
                    .map(|x| x.0)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?,
        None => SoundFontParams::default(),
    };
    if let Some(f) = s_opt(a, "font") {
        p.font = f;
    }
    if let Some(b) = a.get("bank").and_then(|v| v.as_u64()) {
        p.bank = b as u16;
    }
    if let Some(pr) = a.get("program").and_then(|v| v.as_u64()) {
        p.program = pr.min(127) as u8;
    }
    if let Some(d) = a.get("drums").and_then(|v| v.as_bool()) {
        p.drums = d;
    }
    if let Some(k) = a.get("fixed_key").and_then(|v| v.as_u64()) {
        p.fixed_key = Some(k.min(127) as u8);
    }
    if let Some(g) = f_opt(a, "gain") {
        p.gain = g.clamp(0.0, 8.0);
    }
    if let Some(t) = a.get("transpose").and_then(|v| v.as_i64()) {
        p.transpose = t.clamp(-48, 48) as i32;
    }
    // name the preset from the font itself
    let bank = if p.drums { 128 } else { p.bank };
    if let Ok(ps) = soundfont::presets(&p.font) {
        match ps.iter().find(|x| x.0 == bank && x.1 == p.program) {
            Some(x) => {
                if p.preset_name.is_empty() || s_opt(a, "instrument").is_none() {
                    p.preset_name = x.2.clone();
                }
            }
            None if !p.drums => bail!(
                "font '{}' has no preset at bank {} program {} (list_instruments shows them)",
                p.font,
                p.bank,
                p.program
            ),
            None => {}
        }
    }
    Ok(p)
}

fn add_at(buf: &mut Vec<f32>, at: usize, y: &[f32]) {
    if buf.len() < at + y.len() {
        buf.resize(at + y.len(), 0.0);
    }
    for (i, v) in y.iter().enumerate() {
        buf[at + i] += v;
    }
}

/// A short phrase that shows off the instrument: an arpeggio and a chord, a
/// basic groove for a kit, or four hits for a single drum.
pub fn preview(p: &SoundFontParams, root: f32) -> Vec<f32> {
    let mut out = Vec::new();
    let beat = 0.5;
    let at = |b: f32| (b * beat * SR) as usize;
    if p.drums && p.fixed_key.is_none() {
        for bar in 0..2 {
            let o = bar as f32 * 4.0;
            for k in 0..8 {
                let v = if k % 2 == 0 { 0.8 } else { 0.55 };
                add_at(
                    &mut out,
                    at(o + k as f32 * 0.5),
                    &soundfont::render_note(p, 42.0, v, 0.1),
                );
            }
            for b in [0.0, 2.5] {
                add_at(
                    &mut out,
                    at(o + b),
                    &soundfont::render_note(p, 36.0, 0.9, 0.2),
                );
            }
            for b in [1.0, 3.0] {
                add_at(
                    &mut out,
                    at(o + b),
                    &soundfont::render_note(p, 38.0, 0.85, 0.2),
                );
            }
        }
    } else if p.drums {
        for k in 0..4 {
            add_at(
                &mut out,
                at(k as f32),
                &soundfont::render_note(p, 60.0, 0.6 + 0.1 * k as f32, 0.2),
            );
        }
    } else {
        for (i, iv) in [0.0, 4.0, 7.0, 12.0, 7.0, 4.0].iter().enumerate() {
            add_at(
                &mut out,
                at(i as f32 * 0.5),
                &soundfont::render_note(p, root + iv, 0.75, 0.45),
            );
        }
        for iv in [0.0, 4.0, 7.0] {
            add_at(
                &mut out,
                at(3.5),
                &soundfont::render_note(p, root + iv, 0.7, 1.6),
            );
        }
    }
    out
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "fetch_instrument",
            description: "Download a catalog item (search_instruments) into ~/.beatbox/library (once; sha256-checked; never goes into the project or git). Default id generaluser_gs = a General MIDI SoundFont with ~260 recorded instruments: pianos, clean/jazz/overdriven electric guitars, nylon/steel acoustic guitars, finger/pick/fretless basses, acoustic drum kits (standard, room, brush ...), strings, brass, winds, choir. Returns the path, size, licence and whether it was downloaded now.",
            mutates: false,
            schema: || obj(json!({"id": {"type": "string", "description": "catalog id (default generaluser_gs)"}, "max_mb": {"type": "integer", "description": "SFZ programs: refuse when the samples needed exceed this (default 400)"}, "max_rr": {"type": "integer", "description": "SFZ: round robins kept (default 2)"}}), &[]),
            run: |_e, a| {
                let id = s_opt(a, "id").unwrap_or_else(|| "generaluser_gs".into());
                if soundfont::font_entry(&id).is_none() {
                    let ent = crate::sound_catalog::entry(&id).ok_or_else(|| anyhow!("unknown id '{id}' (search_instruments lists the catalog)"))?;
                    if ent.format == "sfz_git" {
                        let max = a.get("max_mb").and_then(|v| v.as_u64()).unwrap_or(400);
                        let (path, now) = crate::sound_catalog::fetch_sfz_git(&ent, max, a.get("max_rr").and_then(|v| v.as_u64()).unwrap_or(2) as u8)?;
                        crate::sound_catalog::evict(&ent.repo);
                        return Ok(json!({"id": ent.id, "name": ent.name, "sfz": path, "downloaded_mb": (now as f64 / 1e5).round() / 10.0, "license": ent.license, "source": ent.source, "next": format!("load_instrument {{track, catalog_id:'{}'}}", ent.id)}));
                    }
                    if ent.format == "vsco_pack" {
                        bail!("'{id}' is a VSCO 2 CE multisample pack: use install_instrument_pack {{pack:'{}'}}", id.trim_start_matches("vsco_"));
                    }
                    let path = crate::sound_catalog::fetch(&ent)?;
                    return Ok(json!({"id": ent.id, "name": ent.name, "path": path, "license": ent.license, "source": ent.source, "format": ent.format}));
                }
                let (path, fresh) = soundfont::fetch_font(&id)?;
                let ent = soundfont::font_entry(&id).unwrap();
                let n = soundfont::presets(&id).map(|v| v.len()).unwrap_or(0);
                Ok(json!({"id": ent.id, "name": ent.name, "path": path, "bytes": ent.bytes, "downloaded_now": fresh, "presets": n, "license": ent.license, "source": ent.source, "next": "load_instrument {track, instrument:'clean_electric_guitar'} or list_instruments"}))
            },
        },
        Tool {
            name: "list_instruments",
            description: "Real recorded instruments you can put on a track: the curated names (clean_electric_guitar, electric_bass, drum_kit, kick, snare, closed_hat, grand_piano, strings ...) and, with presets:true, every preset (bank, program, name) in a font (default generaluser_gs, fetched on first use). search filters by name.",
            mutates: false,
            schema: || obj(json!({"font": {"type": "string"}, "presets": {"type": "boolean"}, "search": {"type": "string"}}), &[]),
            run: |_e, a| {
                let q = s_opt(a, "search").map(|s| s.to_lowercase());
                let hit = |s: &str| q.as_ref().is_none_or(|q| s.to_lowercase().contains(q.as_str()));
                let named: Vec<Value> = soundfont::ALIASES.iter().filter(|x| hit(x.0) || hit(x.1)).map(|x| json!({"instrument": x.0, "what": x.1, "drums": x.4})).collect();
                let font = s_opt(a, "font").unwrap_or_else(|| "generaluser_gs".into());
                let installed: Vec<Value> = soundfont::FONTS.iter().map(|f| json!({"id": f.id, "installed": soundfont::installed_path(f).is_file(), "bytes": f.bytes, "license": f.license})).collect();
                let mut out = json!({"instruments": named, "fonts": installed});
                if b_or(a, "presets", false) || q.is_some() {
                    let ps = soundfont::presets(&font)?;
                    out["presets"] = json!(ps.iter().filter(|p| hit(&p.2)).map(|p| json!({"bank": p.0, "program": p.1, "name": p.2, "drums": p.0 == 128})).collect::<Vec<_>>());
                }
                Ok(out)
            },
        },
        Tool {
            name: "load_instrument",
            description: "Put a real recorded instrument on a track (creates the track when it does not exist): catalog_id = any SFZ instrument from search_instruments (e.g. a CC0 electric guitar, bass or drum kit), or instrument = a curated SoundFont name (list_instruments), or font + bank + program (GM numbering; drums:true for a kit, program picks the kit). fixed_key makes a one-drum track play that drum for every note (36 kick, 38 snare, 42 closed hat, 46 open hat, 49 crash ...). gain, transpose. The font is fetched on first use.",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}, "instrument": {"type": "string"}, "font": {"type": "string"}, "bank": {"type": "integer"}, "program": {"type": "integer"}, "drums": {"type": "boolean"}, "fixed_key": {"type": "integer"}, "gain": {"type": "number"}, "transpose": {"type": "integer"}, "volume_db": {"type": "number"}, "catalog_id": {"type": "string", "description": "an SFZ instrument from search_instruments (fetched on first use)"}, "max_mb": {"type": "integer"}, "max_rr": {"type": "integer", "description": "SFZ: round robins kept (default 2)"}, "one_shot": {"type": "boolean", "description": "SFZ: play whole samples (default true for drum/percussion kits)"}}), &["track"]),
            run: |e, a| {
                let name = s_req(a, "track")?;
                if let Some(cid) = s_opt(a, "catalog_id") {
                    let ent = crate::sound_catalog::entry(&cid).ok_or_else(|| anyhow!("unknown catalog id '{cid}' (search_instruments)"))?;
                    match ent.format.as_str() {
                        "sfz_git" => {
                            let max = a.get("max_mb").and_then(|v| v.as_u64()).unwrap_or(400);
                            let (path, now) = crate::sound_catalog::fetch_sfz_git(&ent, max, a.get("max_rr").and_then(|v| v.as_u64()).unwrap_or(2) as u8)?;
                            // a kit rings out: hits play their whole sample (crash/open-hat tails),
                            // not note length + release
                            let one_shot = a.get("one_shot").and_then(|v| v.as_bool()).unwrap_or(matches!(ent.family.as_str(), "drums" | "percussion"));
                            let mut args = json!({"name": name, "sfz": path.to_string_lossy(), "one_shot": one_shot});
                            if let Some(v) = f_opt(a, "volume_db") { args["volume_db"] = json!(v); }
                            let r = e.call("add_multisample_track", &args)?;
                            return Ok(json!({"track": name, "catalog_id": ent.id, "name": ent.name, "zones": r["zones"], "downloaded_mb": (now as f64 / 1e5).round() / 10.0, "license": ent.license}));
                        }
                        "vsco_pack" => return e.call("install_instrument_pack", &json!({"pack": cid.trim_start_matches("vsco_"), "track": name})),
                        "sf2" => {}
                        f => bail!("catalog item '{cid}' ({f}) is not an instrument"),
                    }
                }
                let p = params_from(a)?;
                soundfont::load_font(&p.font)?;
                let inst = Instrument::SoundFont(p.clone());
                let created = match e.project.track_index(&name) {
                    Ok(i) => {
                        e.project.tracks[i].instrument = inst;
                        if let Some(v) = f_opt(a, "volume_db") {
                            e.project.tracks[i].volume_db = v;
                        }
                        false
                    }
                    Err(_) => {
                        if e.project.bus_index(&name).is_ok() || name.eq_ignore_ascii_case("master") {
                            bail!("'{name}' is a bus/master name; pick another track name");
                        }
                        e.project.tracks.push(Track { volume_db: f_opt(a, "volume_db").unwrap_or(-6.0), ..Track::new(&name, inst) });
                        true
                    }
                };
                Ok(json!({"track": name, "created": created, "instrument": p}))
            },
        },
        Tool {
            name: "preview_instrument",
            description: "Hear an instrument before using it: renders a short phrase (arpeggio + chord around root, default 52 = E3; a two-bar groove for a kit; four hits for one drum) to a WAV. Same instrument keys as load_instrument, track to preview that track's soundfont instrument, or preset for any built-in preset from list_presets (synths, drums, 808s...). play: true also sounds it once on the studio's speakers (gain_db trims it) without touching the project.",
            mutates: false,
            schema: || obj(json!({"instrument": {"type": "string"}, "track": {"type": "string"}, "font": {"type": "string"}, "bank": {"type": "integer"}, "program": {"type": "integer"}, "drums": {"type": "boolean"}, "fixed_key": {"type": "integer"}, "root": {"type": "number"}, "path": {"type": "string"}, "preset": {"type": "string", "description": "built-in preset name (list_presets)"}, "play": {"type": "boolean"}, "gain_db": {"type": "number"}}), &[]),
            run: |e, a| {
                if let Some(pn) = s_opt(a, "preset") {
                    let (l, r, how) = render_preset_preview(&pn, f_opt(a, "root"))?;
                    let st = crate::analysis::stats(&l, &r);
                    if st.peak_dbfs < -60.0 {
                        bail!("preset '{pn}' rendered silence");
                    }
                    let path = match s_opt(a, "path") {
                        Some(x) => e.resolve(&x),
                        None => e.renders_dir().join(format!("preview_{}.wav", crate::samples::sample_name(&pn))),
                    };
                    crate::render::write_wav(&path, &l, &r)?;
                    let out = json!({"preset": pn, "path": path, "seconds": (l.len() as f32 / SR * 10.0).round() / 10.0, "peak_dbfs": st.peak_dbfs, "rms_dbfs": st.rms_dbfs, "played_as": how});
                    return Ok(if b_or(a, "play", false) { let q = queue_audition(e, l, r, f_opt(a, "gain_db").unwrap_or(0.0)); merge(out, q) } else { out });
                }
                let p = match (s_opt(a, "track"), s_opt(a, "instrument")) {
                    (Some(t), None) => {
                        let i = e.project.track_index(&t)?;
                        match &e.project.tracks[i].instrument {
                            Instrument::SoundFont(p) => p.clone(),
                            _ => bail!("track '{t}' is not a recorded (soundfont) instrument"),
                        }
                    }
                    _ => params_from(a)?,
                };
                let y = preview(&p, f_opt(a, "root").unwrap_or(52.0));
                if y.is_empty() {
                    bail!("the instrument rendered nothing (font missing?)");
                }
                let label = s_opt(a, "instrument").or(s_opt(a, "track")).unwrap_or_else(|| format!("{}_{}", p.bank, p.program));
                let path = match s_opt(a, "path") {
                    Some(x) => e.resolve(&x),
                    None => e.renders_dir().join(format!("preview_{}.wav", crate::samples::sample_name(&label))),
                };
                crate::render::write_wav(&path, &y, &y)?;
                let st = crate::analysis::stats(&y, &y);
                let out = json!({"path": path, "seconds": (y.len() as f32 / SR * 10.0).round() / 10.0, "peak_dbfs": st.peak_dbfs, "rms_dbfs": st.rms_dbfs, "instrument": p});
                Ok(if b_or(a, "play", false) { let q = queue_audition(e, y.clone(), y, f_opt(a, "gain_db").unwrap_or(0.0)); merge(out, q) } else { out })
            },
        },
        Tool {
            name: "audition_instrument",
            description: "Hear a catalog instrument before choosing it: fetches it (once, like load_instrument) and renders a ~4 s preview to a WAV without touching the project. Melodic instruments play an arpeggio and a chord in the middle of their range (or around root); kits and percussion play a groove across their mapped hits; drones and transition swells play one held note. Returns the path, peak and RMS, key range and the number of zones, so you can compare candidates by ear or by numbers. play: true also sounds it once on the studio's speakers.",
            mutates: false,
            schema: || obj(json!({"catalog_id": {"type": "string"}, "root": {"type": "integer", "description": "MIDI note to centre the phrase on"}, "path": {"type": "string"}, "max_mb": {"type": "integer"}, "max_rr": {"type": "integer"}, "play": {"type": "boolean"}, "gain_db": {"type": "number"}}), &["catalog_id"]),
            run: |e, a| {
                let cid = s_req(a, "catalog_id")?;
                let ent = crate::sound_catalog::entry(&cid).ok_or_else(|| anyhow!("unknown catalog id '{cid}' (search_instruments)"))?;
                let scratch = std::env::temp_dir().join(format!("beatbox_audition_{}", std::process::id()));
                let mut s = crate::engine::Engine::new(scratch.clone());
                s.project.bpm = 120.0;
                s.project.tracks.clear();
                if let Some(p) = s.project.patterns.first_mut() {
                    p.bars = 2;
                    p.clips.clear();
                } else {
                    s.project.patterns.push(crate::project::Pattern::new("main", 2));
                }
                s.project.arrangement.clear();
                let mut la = json!({"track": "audition", "catalog_id": ent.id, "volume_db": 0.0});
                for k in ["max_mb", "max_rr"] {
                    if let Some(v) = a.get(k) {
                        la[k] = v.clone();
                    }
                }
                let loaded = s.call("load_instrument", &la)?;
                let ti = s.project.track_index("audition")?;
                let (roots, lo, hi): (Vec<u8>, u8, u8) = match &s.project.tracks[ti].instrument {
                    Instrument::Multisample(m) => {
                        let mut r: Vec<u8> = m.zones.iter().map(|z| z.root).collect();
                        r.sort_unstable();
                        r.dedup();
                        (r, m.zones.iter().map(|z| z.lo_note).min().unwrap_or(0), m.zones.iter().map(|z| z.hi_note).max().unwrap_or(127))
                    }
                    Instrument::SoundFont(p) => (vec![], 0, if p.drums { 81 } else { 108 }),
                    _ => (vec![], 0, 127),
                };
                // swells, chime glisses and other transition fx are one long sound per key
                let swell = ent.tags.iter().any(|t| t == "transition" || t == "swell");
                let perc = !swell && (matches!(ent.family.as_str(), "drums" | "percussion") || (roots.len() > 1 && roots.len() <= 24 && ent.tags.iter().any(|t| t.contains("drum") || t.contains("percussion") || t.contains("kit"))));
                let drone = !perc && (swell || ent.tags.iter().any(|t| t == "drone"));
                let mut notes = Vec::new();
                if drone {
                    // a drone (tanpura, shruti box) is heard as one held note at its own pitch
                    let c = a.get("root").and_then(|v| v.as_u64()).map(|v| v.min(120) as u8).unwrap_or_else(|| roots.first().copied().unwrap_or(52)).clamp(lo, hi.max(lo));
                    notes.push(json!({"start": 0.0, "pitch": c, "len": 30, "vel": 0.8}));
                } else if perc {
                    // a two-bar groove over up to four mapped hits
                    // General MIDI drum keys when the kit maps them (kick, snare, closed hat, open hat)
                    let keys: Vec<u8> = if roots.is_empty() {
                        vec![36, 38, 42, 46]
                    } else if roots.contains(&36) && (roots.contains(&38) || roots.contains(&40)) {
                        let mut k = vec![36u8, if roots.contains(&38) { 38 } else { 40 }];
                        for h in [42u8, 44, 46] {
                            if roots.contains(&h) && k.len() < 4 {
                                k.push(h);
                            }
                        }
                        for r in roots.iter().copied() {
                            if k.len() >= 3 {
                                break;
                            }
                            if !k.contains(&r) {
                                k.push(r);
                            }
                        }
                        k
                    } else {
                        roots.iter().copied().take(4).collect()
                    };
                    let k = |i: usize| keys[i.min(keys.len() - 1)];
                    for bar in 0..2u32 {
                        let o = (bar * 16) as f32;
                        for st in [0.0, 6.0, 10.0] {
                            notes.push(json!({"start": o + st, "pitch": k(0), "len": 2, "vel": 0.95}));
                        }
                        for st in [4.0, 12.0] {
                            notes.push(json!({"start": o + st, "pitch": k(1), "len": 2, "vel": 0.9}));
                        }
                        for st in (0..16).step_by(2) {
                            notes.push(json!({"start": o + st as f32, "pitch": k(2), "len": 1, "vel": if st % 4 == 0 { 0.7 } else { 0.5 }}));
                        }
                        if keys.len() > 3 {
                            notes.push(json!({"start": o + 14.0, "pitch": k(3), "len": 2, "vel": 0.8}));
                        }
                    }
                } else {
                    let mid = if roots.is_empty() { ((lo as u32 + hi as u32) / 2).clamp(36, 84) as u8 } else { roots[roots.len() / 2] };
                    let c = a.get("root").and_then(|v| v.as_u64()).map(|v| v.min(120) as u8).unwrap_or(mid).clamp(lo, hi.max(lo));
                    let c = c.saturating_sub(c % 12).max(lo.min(120));
                    for (i, iv) in [0u8, 4, 7, 12, 7, 4].iter().enumerate() {
                        notes.push(json!({"start": i as f32 * 2.0, "pitch": (c + iv).min(127), "len": 2, "vel": 0.75}));
                    }
                    for iv in [0u8, 4, 7] {
                        notes.push(json!({"start": 16.0, "pitch": (c + iv).min(127), "len": 14, "vel": 0.7}));
                    }
                }
                s.call("add_notes", &json!({"track": "audition", "notes": notes}))?;
                let m = s.mix()?;
                let path = match s_opt(a, "path") {
                    Some(x) => e.resolve(&x),
                    None => e.renders_dir().join(format!("audition_{}.wav", crate::samples::sample_name(&ent.id))),
                };
                crate::render::write_wav(&path, &m.left, &m.right)?;
                let st = crate::analysis::stats(&m.left, &m.right);
                let _ = std::fs::remove_dir_all(&scratch);
                if st.peak_dbfs < -60.0 {
                    bail!("{} rendered silence (peak {:.0} dBFS)", ent.id, st.peak_dbfs);
                }
                let out = json!({"catalog_id": ent.id, "name": ent.name, "path": path, "seconds": (m.left.len() as f32 / SR * 10.0).round() / 10.0, "peak_dbfs": st.peak_dbfs, "rms_dbfs": st.rms_dbfs, "played_as": if swell { "held swell" } else if drone { "held drone" } else if perc { "groove" } else { "arpeggio + chord" }, "zones": loaded["zones"], "key_range": [lo, hi], "license": ent.license, "downloaded_mb": loaded["downloaded_mb"]});
                Ok(if b_or(a, "play", false) { let q = queue_audition(e, m.left.clone(), m.right.clone(), f_opt(a, "gain_db").unwrap_or(0.0)); merge(out, q) } else { out })
            },
        },
        Tool {
            name: "play_audio",
            description: "Sound audio once on the studio's speakers, on top of the song and without touching the project: a WAV/FLAC/MP3/OGG path (a render, an export, a preview) or a project sample by name. stop: true stops the current preview. gain_db trims it. Answers whether a studio window is attached to hear it.",
            mutates: false,
            schema: || obj(json!({"path": {"type": "string"}, "sample": {"type": "string"}, "gain_db": {"type": "number"}, "stop": {"type": "boolean"}, "max_seconds": {"type": "number"}}), &[]),
            run: |e, a| {
                if b_or(a, "stop", false) {
                    e.transport.pending_audition = Some(None);
                    return Ok(json!({"stopped": true}));
                }
                let path = match (s_opt(a, "path"), s_opt(a, "sample")) {
                    (Some(p), _) => e.resolve(&p),
                    (None, Some(n)) => {
                        let se = e.project.samples.iter().find(|x| x.name == n).ok_or_else(|| anyhow!("no sample named '{n}' (list_samples)"))?;
                        std::path::PathBuf::from(&se.path)
                    }
                    _ => bail!("give path or sample (or stop: true)"),
                };
                let (mut l, mut r) = crate::samples::decode_stereo(&path)?;
                let cap = (f_opt(a, "max_seconds").unwrap_or(600.0).clamp(0.1, 600.0) * SR) as usize;
                l.truncate(cap);
                r.truncate(cap);
                if l.is_empty() {
                    bail!("{} has no audio", path.display());
                }
                let q = queue_audition(e, l, r, f_opt(a, "gain_db").unwrap_or(0.0));
                Ok(merge(json!({"path": path}), q))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use crate::engine::Engine;
    use serde_json::json;

    #[test]
    fn load_and_render_recorded_instruments_when_installed() {
        let e0 = crate::soundfont::font_entry("generaluser_gs").unwrap();
        if !crate::soundfont::installed_path(e0).is_file() {
            eprintln!("generaluser_gs not installed; skipping");
            return;
        }
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_sf_tests"));
        e.call("generate_beat", &json!({"style": "lofi", "seed": 3}))
            .unwrap();
        let r = e
            .call(
                "load_instrument",
                &json!({"track": "gtr_real", "instrument": "clean_electric_guitar"}),
            )
            .unwrap();
        assert_eq!(r["created"], true, "{r}");
        e.call(
            "add_pattern_notes",
            &json!({"track": "gtr_real", "notes": [{"step": 0, "pitch": 64, "len": 4}]}),
        )
        .ok();
        let pv = e
            .call("preview_instrument", &json!({"instrument": "drum_kit"}))
            .unwrap();
        assert!(pv["peak_dbfs"].as_f64().unwrap() > -30.0, "{pv}");
        let ls = e
            .call("list_instruments", &json!({"search": "guitar"}))
            .unwrap();
        assert!(ls["presets"].as_array().unwrap().len() >= 4, "{ls}");
        let m = e.mix().unwrap();
        assert!(m.left.iter().any(|v| v.abs() > 0.0));
    }

    #[test]
    fn hear_preset_queues_a_preview_for_the_studio() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_hear_tests"));
        for pn in ["kick", "acid_bass"] {
            let r = e
                .call("preview_instrument", &json!({"preset": pn, "play": true}))
                .unwrap();
            assert!(r["peak_dbfs"].as_f64().unwrap() > -40.0, "{r}");
            assert_eq!(r["playing"], false, "no studio attached: {r}");
            let q = e.transport.pending_audition.clone().unwrap().unwrap();
            assert!(
                q.0.len() > 44_100 && q.0.iter().any(|v| v.abs() > 0.01),
                "{pn}"
            );
        }
        let n_tracks = e.project.tracks.len();
        assert!(e
            .call("preview_instrument", &json!({"preset": "no_such_preset"}))
            .is_err());
        e.transport.attached = true;
        let wav = e
            .call("preview_instrument", &json!({"preset": "kick"}))
            .unwrap();
        let p = e
            .call("play_audio", &json!({"path": wav["path"], "gain_db": -6.0}))
            .unwrap();
        assert_eq!(p["playing"], true, "{p}");
        assert!(p["play_seconds"].as_f64().unwrap() > 1.0, "{p}");
        let s = e.call("play_audio", &json!({"stop": true})).unwrap();
        assert_eq!(s["stopped"], true);
        assert!(matches!(e.transport.pending_audition, Some(None)));
        assert_eq!(
            e.project.tracks.len(),
            n_tracks,
            "previews never touch the project"
        );
    }
}
