//! Real recorded instruments: fetch a SoundFont into the library, list its
//! presets, put one on a track, and preview it.

use crate::dsp::SR;
use crate::instruments::Instrument;
use crate::project::Track;
use crate::soundfont::{self, SoundFontParams};
use crate::tools::{b_or, f_opt, obj, s_opt, s_req, Tool};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

fn params_from(a: &Value) -> Result<SoundFontParams> {
    let mut p = match s_opt(a, "instrument") {
        Some(n) => soundfont::alias(&n).ok_or_else(|| anyhow!("unknown instrument '{n}'. Names: {}", soundfont::ALIASES.iter().map(|x| x.0).collect::<Vec<_>>().join(", ")))?,
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
            None if !p.drums => bail!("font '{}' has no preset at bank {} program {} (list_instruments shows them)", p.font, p.bank, p.program),
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
                add_at(&mut out, at(o + k as f32 * 0.5), &soundfont::render_note(p, 42.0, v, 0.1));
            }
            for b in [0.0, 2.5] {
                add_at(&mut out, at(o + b), &soundfont::render_note(p, 36.0, 0.9, 0.2));
            }
            for b in [1.0, 3.0] {
                add_at(&mut out, at(o + b), &soundfont::render_note(p, 38.0, 0.85, 0.2));
            }
        }
    } else if p.drums {
        for k in 0..4 {
            add_at(&mut out, at(k as f32), &soundfont::render_note(p, 60.0, 0.6 + 0.1 * k as f32, 0.2));
        }
    } else {
        for (i, iv) in [0.0, 4.0, 7.0, 12.0, 7.0, 4.0].iter().enumerate() {
            add_at(&mut out, at(i as f32 * 0.5), &soundfont::render_note(p, root + iv, 0.75, 0.45));
        }
        for iv in [0.0, 4.0, 7.0] {
            add_at(&mut out, at(3.5), &soundfont::render_note(p, root + iv, 0.7, 1.6));
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
                let hit = |s: &str| q.as_ref().map_or(true, |q| s.to_lowercase().contains(q.as_str()));
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
            schema: || obj(json!({"track": {"type": "string"}, "instrument": {"type": "string"}, "font": {"type": "string"}, "bank": {"type": "integer"}, "program": {"type": "integer"}, "drums": {"type": "boolean"}, "fixed_key": {"type": "integer"}, "gain": {"type": "number"}, "transpose": {"type": "integer"}, "volume_db": {"type": "number"}, "catalog_id": {"type": "string", "description": "an SFZ instrument from search_instruments (fetched on first use)"}, "max_mb": {"type": "integer"}, "max_rr": {"type": "integer", "description": "SFZ: round robins kept (default 2)"}}), &["track"]),
            run: |e, a| {
                let name = s_req(a, "track")?;
                if let Some(cid) = s_opt(a, "catalog_id") {
                    let ent = crate::sound_catalog::entry(&cid).ok_or_else(|| anyhow!("unknown catalog id '{cid}' (search_instruments)"))?;
                    match ent.format.as_str() {
                        "sfz_git" => {
                            let max = a.get("max_mb").and_then(|v| v.as_u64()).unwrap_or(400);
                            let (path, now) = crate::sound_catalog::fetch_sfz_git(&ent, max, a.get("max_rr").and_then(|v| v.as_u64()).unwrap_or(2) as u8)?;
                            let mut args = json!({"name": name, "sfz": path.to_string_lossy()});
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
            description: "Hear an instrument before using it: renders a short phrase (arpeggio + chord around root, default 52 = E3; a two-bar groove for a kit; four hits for one drum) to a WAV. Same instrument keys as load_instrument, or track to preview that track's soundfont instrument.",
            mutates: false,
            schema: || obj(json!({"instrument": {"type": "string"}, "track": {"type": "string"}, "font": {"type": "string"}, "bank": {"type": "integer"}, "program": {"type": "integer"}, "drums": {"type": "boolean"}, "fixed_key": {"type": "integer"}, "root": {"type": "number"}, "path": {"type": "string"}}), &[]),
            run: |e, a| {
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
                Ok(json!({"path": path, "seconds": (y.len() as f32 / SR * 10.0).round() / 10.0, "peak_dbfs": st.peak_dbfs, "rms_dbfs": st.rms_dbfs, "instrument": p}))
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
        e.call("generate_beat", &json!({"style": "lofi", "seed": 3})).unwrap();
        let r = e.call("load_instrument", &json!({"track": "gtr_real", "instrument": "clean_electric_guitar"})).unwrap();
        assert_eq!(r["created"], true, "{r}");
        e.call("add_pattern_notes", &json!({"track": "gtr_real", "notes": [{"step": 0, "pitch": 64, "len": 4}]})).ok();
        let pv = e.call("preview_instrument", &json!({"instrument": "drum_kit"})).unwrap();
        assert!(pv["peak_dbfs"].as_f64().unwrap() > -30.0, "{pv}");
        let ls = e.call("list_instruments", &json!({"search": "guitar"})).unwrap();
        assert!(ls["presets"].as_array().unwrap().len() >= 4, "{ls}");
        let m = e.mix().unwrap();
        assert!(m.left.iter().any(|v| v.abs() > 0.0));
    }
}
