//! Sound design and plugin-style tools: semantic sound design, instrument
//! layering, macros, parameter discovery (describe_effect /
//! describe_instrument) and generic set_parameters, effect bypass and
//! reordering, multisampled instruments (zones, SFZ, CC0 packs), and
//! offline sample editing / analysis (slice, stretch, edit, analyze).

use crate::audio_edit;
use crate::automation::{self, parse_target, Target};
use crate::dsp::SR;
use crate::engine::Engine;
use crate::fx::{self, Effect};
use crate::instruments::{self, Instrument, Layer, LayerParams};
use crate::multisample::{self, MultisampleParams, Zone};
use crate::project::{Macro, MacroTarget, Note, Project, Track};
use crate::samples::{self, SampleInfo};
use crate::tools::{b_or, f_opt, f_or, instrument_from, obj, pitch_of, s_opt, s_req, u_or, Tool};
use crate::tools_studio::{chain_of, owner_of};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

// ---------------- generic parameter access ----------------

fn path_parts(path: &str) -> Vec<String> {
    path.split('.')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

fn json_set(v: &mut Value, path: &[String], x: Value) -> bool {
    let mut cur = v;
    for (i, k) in path.iter().enumerate() {
        let last = i + 1 == path.len();
        cur = match cur {
            Value::Object(o) => {
                if last {
                    if !o.contains_key(k) {
                        return false;
                    }
                    o.insert(k.clone(), x);
                    return true;
                }
                match o.get_mut(k) {
                    Some(n) => n,
                    None => return false,
                }
            }
            Value::Array(a) => {
                let Ok(idx) = k.parse::<usize>() else {
                    return false;
                };
                if last {
                    if idx >= a.len() {
                        return false;
                    }
                    a[idx] = x;
                    return true;
                }
                match a.get_mut(idx) {
                    Some(n) => n,
                    None => return false,
                }
            }
            _ => return false,
        };
    }
    false
}

/// Set any parameter (number, bool, enum string, array) on a track / bus /
/// master: volume, pan, mute, instrument.<path>, fx.<i>.<path>.
pub fn set_param(p: &mut Project, owner: &str, param: &str, x: &Value) -> Result<Value> {
    let is_master = owner.eq_ignore_ascii_case("master");
    let num = || {
        x.as_f64()
            .map(|v| v as f32)
            .ok_or_else(|| anyhow!("'{param}' needs a number"))
    };
    match param {
        "volume" | "volume_db" => {
            let v = num()?.clamp(-100.0, 12.0);
            if is_master {
                p.master_volume_db = v;
            } else if let Ok(i) = p.track_index(owner) {
                p.tracks[i].volume_db = v;
            } else {
                let b = p.bus_index(owner)?;
                p.buses[b].volume_db = v;
            }
            return Ok(json!(v));
        }
        "pan" => {
            let v = num()?.clamp(-1.0, 1.0);
            if let Ok(i) = p.track_index(owner) {
                p.tracks[i].pan = v;
            } else {
                let b = p.bus_index(owner)?;
                p.buses[b].pan = v;
            }
            return Ok(json!(v));
        }
        "mute" => {
            let m = x
                .as_bool()
                .ok_or_else(|| anyhow!("mute needs true/false"))?;
            if let Ok(i) = p.track_index(owner) {
                p.tracks[i].mute = m;
            } else {
                let b = p.bus_index(owner)?;
                p.buses[b].mute = m;
            }
            return Ok(json!(m));
        }
        _ => {}
    }
    let parts = path_parts(param);
    match parts.first().map(|s| s.as_str()) {
        Some("instrument") => {
            let i = p.track_index(owner).map_err(|_| anyhow!("only tracks have instruments ('{owner}')"))?;
            let mut v = serde_json::to_value(&p.tracks[i].instrument)?;
            if !json_set(&mut v, &parts[1..], x.clone()) {
                bail!("{} instrument has no parameter '{}' (describe_instrument lists them)", p.tracks[i].instrument.kind_name(), parts[1..].join("."));
            }
            let inst: Instrument = serde_json::from_value(v).map_err(|e| anyhow!("invalid value for {param}: {e}"))?;
            p.tracks[i].instrument = inst;
            Ok(x.clone())
        }
        Some("fx") => {
            let idx: usize = parts.get(1).and_then(|s| s.parse().ok()).ok_or_else(|| anyhow!("use fx.<index>.<param>"))?;
            let chain = crate::tools::effects_of(p, owner)?;
            let fxv = chain.get(idx).ok_or_else(|| anyhow!("no effect at index {idx} on '{owner}'"))?;
            let mut v = serde_json::to_value(fxv)?;
            if !json_set(&mut v, &parts[2..], x.clone()) {
                bail!("{} has no parameter '{}' (describe_effect lists them)", fxv.type_name(), parts[2..].join("."));
            }
            let e: Effect = serde_json::from_value(v).map_err(|e| anyhow!("invalid value for {param}: {e}"))?;
            chain[idx] = e;
            Ok(x.clone())
        }
        _ => bail!("unknown parameter '{param}'. Use volume, pan, mute, instrument.<path> or fx.<index>.<path>"),
    }
}

// ---------------- parameter schemas ----------------

fn enum_options(owner_type: &str, field: &str) -> Option<Vec<&'static str>> {
    Some(match (owner_type, field) {
        ("filter", "mode") | (_, "filter_mode") => vec!["lowpass", "highpass", "bandpass", "notch"],
        ("stutter", "mode") => vec!["gate", "stutter", "half_time", "reverse", "tape_stop"],
        ("convolution", "space") => vec!["room", "hall", "plate", "chamber", "spring", "cathedral"],
        (_, "kind") if owner_type == "parametric_eq" => vec![
            "bell",
            "low_shelf",
            "high_shelf",
            "low_cut",
            "high_cut",
            "notch",
        ],
        ("drum", "kind") => vec![
            "kick",
            "snare",
            "clap",
            "closed_hat",
            "open_hat",
            "rim",
            "tom",
            "cowbell",
            "shaker",
            "crash",
        ],
        ("ensemble", "kind") => vec!["strings", "choir", "brass"],
        (_, "osc1") | (_, "osc2") => vec!["sine", "saw", "square", "triangle", "noise"],
        ("wavetable", "table") => vec!["basic", "harmonic", "pwm", "vocal", "digital", "organ"],
        _ => return None,
    })
}

/// (min, max, unit, log) by parameter name.
fn range_of(field: &str) -> (f32, f32, &'static str, bool) {
    let f = field;
    if f.ends_with("_db") || f == "db" {
        return match f {
            "ceiling_db" => (-12.0, 0.0, "dBFS", false),
            "threshold_db" => (-60.0, 0.0, "dBFS", false),
            "range_db" => (-60.0, 12.0, "dB", false),
            _ => (-24.0, 24.0, "dB", false),
        };
    }
    if f.ends_with("_ms") {
        return (0.0, 2000.0, "ms", false);
    }
    if f.ends_with("_hz") || f.ends_with("freq") || f == "cutoff" || f == "tone" {
        return (20.0, 20000.0, "Hz", true);
    }
    match f {
        "mix" | "amount" | "depth" | "damping" | "resonance" | "spray" | "position" | "osc_mix"
        | "sub_level" | "noise_level" | "brightness" | "felt" | "hammer" | "body" | "early"
        | "width" | "stereo_spread" | "vibrato" | "vowel" | "noise" | "reverse_prob"
        | "vel_to_gain" | "sustain" | "punch_mix" | "drive" | "gain" | "size" | "feedback" => {
            (0.0, 1.0, "0..1", false)
        }
        "ratio" | "low_ratio" | "mid_ratio" | "high_ratio" => (1.0, 20.0, ":1", false),
        "steps" | "slice_steps" | "cycle_steps" | "sync_steps" => (0.0, 64.0, "16th steps", false),
        "semitones" | "transpose" | "tune" | "osc2_semitones" | "pitch_env_semitones" => {
            (-24.0, 24.0, "semitones", false)
        }
        "cents"
        | "osc2_cents"
        | "detune_cents"
        | "unison_spread_cents"
        | "pitch_spread_cents"
        | "tune_cents" => (-100.0, 100.0, "cents", false),
        "bits" => (1.0, 16.0, "bits", false),
        "downsample" => (1.0, 64.0, "x", false),
        "q" => (0.1, 30.0, "Q", false),
        "stages" => (1.0, 12.0, "count", false),
        "unison" | "players" => (1.0, 9.0, "voices", false),
        "attack" | "decay" | "release" | "decay_s" | "pitch_env_time" | "max_length" | "start" => {
            (0.0, 10.0, "s", false)
        }
        "rate_hz" | "lfo_rate" | "position_lfo_rate" => (0.0, 20.0, "Hz", false),
        "ratio2" | "index" | "index2" => (0.0, 20.0, "", false),
        "density" => (1.0, 400.0, "grains/s", false),
        "grain_ms" => (5.0, 500.0, "ms", false),
        "filter_env_amount" | "lfo_to_cutoff" => (-8.0, 8.0, "octaves", false),
        _ => (0.0, 1.0, "", false),
    }
}

fn describe_value(kind: &str, prefix: &str, v: &Value, out: &mut Vec<Value>) {
    if let Value::Object(o) = v {
        for (k, x) in o {
            if k == "type" {
                continue;
            }
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            match x {
                Value::Number(n) => {
                    let (lo, hi, unit, log) = range_of(k);
                    out.push(json!({"param": path, "type": "number", "value": n, "min": lo, "max": hi, "unit": unit, "log_scale": log, "automatable": true}));
                }
                Value::Bool(b) => out.push(json!({"param": path, "type": "bool", "value": b, "automatable": false})),
                Value::String(s) => match enum_options(kind, k) {
                    Some(opts) => out.push(json!({"param": path, "type": "enum", "value": s, "options": opts, "automatable": false})),
                    None => out.push(json!({"param": path, "type": "string", "value": s, "automatable": false})),
                },
                Value::Object(_) => describe_value(kind, &path, x, out),
                Value::Array(a) => {
                    out.push(json!({"param": path, "type": "array", "items": a.len(), "value": if a.len() <= 8 { x.clone() } else { json!(format!("[{} items]", a.len())) }, "automatable": false, "note": "set the whole array, or index into it: e.g. bands.0.gain_db"}));
                    for (i, item) in a.iter().enumerate().take(8) {
                        if item.is_object() {
                            describe_value(kind, &format!("{path}.{i}"), item, out);
                        }
                    }
                }
                Value::Null => {}
            }
        }
    }
}

// ---------------- semantic sound design ----------------

fn words(desc: &str) -> Vec<String> {
    desc.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}

/// Turn a description into an instrument + effect chain + what was applied.
pub fn design(desc: &str) -> Result<(Instrument, Vec<Effect>, Vec<String>)> {
    let w = words(desc);
    let has = |k: &[&str]| w.iter().any(|x| k.contains(&x.as_str()));
    let mut applied = Vec::new();
    let base = if has(&["kick", "bd"]) {
        "kick"
    } else if has(&["snare"]) {
        "snare"
    } else if has(&["clap"]) {
        "clap"
    } else if has(&["hat", "hihat", "hats"]) {
        if has(&["open"]) {
            "open_hat"
        } else {
            "hat"
        }
    } else if has(&["808"]) {
        "808"
    } else if has(&["reese"]) {
        "reese_bass"
    } else if has(&["acid", "303"]) {
        "acid_bass"
    } else if has(&["bass", "sub"]) {
        if has(&["pluck", "plucky", "house"]) {
            "pluck_bass"
        } else {
            "sub_bass"
        }
    } else if has(&["piano"]) {
        if has(&["felt", "soft", "intimate"]) {
            "felt_piano"
        } else {
            "grand_piano"
        }
    } else if has(&["rhodes", "epiano", "keys", "ep"]) {
        "epiano"
    } else if has(&["organ"]) {
        "organ"
    } else if has(&["choir", "voices", "vocal", "ooh", "aah"]) {
        if has(&["aah", "open"]) {
            "choir_aah"
        } else {
            "choir"
        }
    } else if has(&["strings", "string", "violins", "orchestral"]) {
        if has(&["staccato", "short", "spiccato"]) {
            "staccato_strings"
        } else {
            "string_section"
        }
    } else if has(&["cello", "celli"]) {
        "cello_section"
    } else if has(&["brass", "horns"]) {
        "brass_section"
    } else if has(&["bell", "bells", "glassy"]) {
        "fm_bell"
    } else if has(&["pad", "pads", "texture", "ambient", "atmosphere"]) {
        if has(&["granular", "airy", "texture", "shimmer"]) {
            "granular_pad"
        } else if has(&["evolving", "moving"]) {
            "wt_pad"
        } else {
            "warm_pad"
        }
    } else if has(&["pluck", "plucky"]) {
        "pluck_lead"
    } else if has(&["guitar"]) {
        "guitar_pluck"
    } else if has(&["supersaw", "trance", "anthem"]) {
        "supersaw"
    } else if has(&["lead", "synth"]) {
        "wt_lead"
    } else {
        bail!("couldn't tell what instrument '{desc}' is. Name one: kick, snare, clap, hat, 808, bass, piano, keys, organ, pad, strings, choir, brass, bell, pluck, lead")
    };
    applied.push(format!("base preset {base}"));
    let mut inst = instruments::preset(base).ok_or_else(|| anyhow!("missing preset {base}"))?;
    let mut fxs: Vec<Effect> = Vec::new();
    let fx = |t: &str, params: Value| -> Effect {
        let mut v = json!({"type": t});
        crate::tools::merge(&mut v, &params);
        serde_json::from_value(v).expect("valid effect")
    };
    let mut iv = serde_json::to_value(&inst)?;
    let scale = |iv: &mut Value, k: &str, f: f32| {
        if let Some(x) = iv.get(k).and_then(|x| x.as_f64()) {
            iv[k] = json!(x as f32 * f);
        }
    };
    let set = |iv: &mut Value, k: &str, x: Value| {
        if iv.get(k).is_some() {
            iv[k] = x;
        }
    };
    if has(&["warm", "mellow", "smooth"]) {
        scale(&mut iv, "cutoff", 0.6);
        scale(&mut iv, "brightness", 0.7);
        fxs.push(fx("distortion", json!({"drive": 0.12, "mix": 0.3})));
        applied.push("warm: darker filter + gentle saturation".into());
    }
    if has(&["dark", "moody", "muffled"]) {
        scale(&mut iv, "cutoff", 0.45);
        scale(&mut iv, "brightness", 0.5);
        fxs.push(fx(
            "parametric_eq",
            json!({"bands": [{"kind": "high_shelf", "freq": 5000.0, "gain_db": -5.0, "q": 0.7}]}),
        ));
        applied.push("dark: low cutoff + high shelf cut".into());
    }
    if has(&["bright", "crisp", "airy", "shiny", "sparkly"]) {
        scale(&mut iv, "cutoff", 1.8);
        scale(&mut iv, "brightness", 1.3);
        fxs.push(fx(
            "parametric_eq",
            json!({"bands": [{"kind": "high_shelf", "freq": 8000.0, "gain_db": 3.5, "q": 0.7}]}),
        ));
        applied.push("bright/airy: open filter + air shelf".into());
    }
    if has(&["dusty", "lofi", "vintage", "old", "tape", "vinyl"]) {
        fxs.push(fx(
            "bitcrush",
            json!({"bits": 11.0, "downsample": 2, "mix": 0.35}),
        ));
        fxs.push(fx(
            "chorus",
            json!({"rate_hz": 0.3, "depth_ms": 1.5, "mix": 0.25}),
        ));
        fxs.push(fx(
            "filter",
            json!({"mode": "lowpass", "cutoff": 6500.0, "resonance": 0.05}),
        ));
        applied.push("dusty: light bit reduction, tape wow, rolled-off top".into());
    }
    if has(&["punchy", "hard", "snappy", "knock", "knocking"]) {
        fxs.push(fx("transient", json!({"attack": 0.5, "sustain": -0.2})));
        set(&mut iv, "drive", json!(0.25));
        applied.push("punchy: transient attack up, tail down".into());
    }
    if has(&["compact", "tight", "short", "clipped"]) {
        scale(&mut iv, "decay", 0.55);
        if let Some(env) = iv.get_mut("amp_env") {
            env["release"] = json!(0.08);
            env["decay"] = json!((env["decay"].as_f64().unwrap_or(0.3) * 0.5) as f32);
        }
        applied.push("compact: shorter decay/release".into());
    }
    if has(&["long", "boomy", "big", "huge", "massive"]) {
        scale(&mut iv, "decay", 1.6);
        applied.push("long/big: longer decay".into());
    }
    if has(&[
        "gritty",
        "dirty",
        "distorted",
        "grimy",
        "aggressive",
        "saturated",
    ]) {
        set(&mut iv, "drive", json!(0.6));
        fxs.push(fx("distortion", json!({"drive": 0.45, "mix": 0.5})));
        fxs.push(fx(
            "soft_clipper",
            json!({"threshold_db": -6.0, "ceiling_db": -0.5}),
        ));
        applied.push("gritty: drive + saturation + soft clip".into());
    }
    if has(&["wide", "stereo", "lush"]) {
        set(&mut iv, "stereo_spread", json!(0.8));
        if iv.get("unison").and_then(|u| u.as_u64()).unwrap_or(0) == 1 {
            set(&mut iv, "unison", json!(5));
        }
        fxs.push(fx("chorus", json!({"mix": 0.35})));
        fxs.push(fx("width", json!({"amount": 1.5})));
        applied.push("wide: unison spread + chorus + width".into());
    }
    if has(&["evolving", "moving", "wobbly", "wobble"]) {
        set(
            &mut iv,
            "lfo_rate",
            json!(if has(&["wobble", "wobbly"]) { 2.0 } else { 0.2 }),
        );
        set(&mut iv, "lfo_to_cutoff", json!(1.2));
        set(&mut iv, "position_lfo_rate", json!(0.2));
        set(&mut iv, "position_lfo_depth", json!(0.3));
        applied.push("evolving: slow modulation".into());
    }
    if has(&["glide", "sliding", "slide"]) {
        set(&mut iv, "glide_ms", json!(120.0));
        applied.push("glide: 120 ms slides (use slide_to on notes)".into());
    }
    if has(&[
        "airy",
        "spacious",
        "ambient",
        "cinematic",
        "haunting",
        "ethereal",
        "atmospheric",
    ]) {
        fxs.push(fx("convolution", json!({"space": if has(&["cinematic", "haunting"]) { "hall" } else { "plate" }, "decay_s": 3.0, "mix": 0.3})));
        applied.push("spacious: convolution reverb".into());
    }
    if has(&["roomy", "room", "live"]) {
        fxs.push(fx(
            "convolution",
            json!({"space": "room", "decay_s": 0.8, "mix": 0.2}),
        ));
        applied.push("room ambience".into());
    }
    if has(&["soft", "gentle", "quiet"]) {
        if let Some(env) = iv.get_mut("amp_env") {
            env["attack"] = json!(((env["attack"].as_f64().unwrap_or(0.005)) as f32).max(0.03));
        }
        applied.push("soft: slower attack".into());
    }
    inst = serde_json::from_value(iv).context("designed instrument")?;
    Ok((inst, fxs, applied))
}

// ---------------- sample helpers ----------------

pub(crate) fn sample_data(e: &mut Engine, name: &str) -> Result<(SampleInfo, Vec<f32>)> {
    let info = e
        .project
        .samples
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "no sample '{name}'. Samples: [{}]",
                e.project
                    .samples
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    e.bank.sync(std::slice::from_ref(&info));
    let d = e
        .bank
        .get(&info.name)
        .ok_or_else(|| anyhow!("sample '{name}' failed to load from {}", info.path))?
        .to_vec();
    Ok((info, d))
}

/// Write mono data as a new sample file and register it.
fn save_new_sample(
    e: &mut Engine,
    base: &SampleInfo,
    new_name: &str,
    data: &[f32],
    note: &str,
) -> Result<Value> {
    let name = samples::sample_name(new_name);
    let path = e.samples_dir().join(format!("{name}.wav"));
    crate::render::write_wav(&path, data, data)?;
    let info = SampleInfo {
        name,
        path: path.to_string_lossy().to_string(),
        source: format!("{} ({note})", base.source),
        license: base.license.clone(),
        author: base.author.clone(),
        duration: 0.0,
    };
    crate::tools::register_sample(e, info)
}

pub(crate) fn audio_for(e: &mut Engine, a: &Value) -> Result<(String, Vec<f32>, Vec<f32>)> {
    if let Some(n) = s_opt(a, "sample") {
        let (info, d) = sample_data(e, &n)?;
        return Ok((info.name, d.clone(), d));
    }
    if let Some(p) = s_opt(a, "path") {
        let path = e.resolve(&p);
        let (l, r) = samples::decode_stereo(&path)?;
        return Ok((
            path.file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or(p),
            l,
            r,
        ));
    }
    let m = e.mix()?;
    Ok(("mix".into(), m.left.clone(), m.right.clone()))
}

fn ensure_layer(inst: &Instrument) -> LayerParams {
    match inst {
        Instrument::Layer(l) => l.clone(),
        other => LayerParams {
            layers: vec![Layer::of(other.clone())],
        },
    }
}

/// Download a VSCO 2 CE pack and build its zones.
fn install_pack(
    e: &mut Engine,
    pack: &multisample::Pack,
    max_rr: u8,
    max_layers: usize,
) -> Result<(Vec<Zone>, usize)> {
    let tree: Value = ureq::get(&format!(
        "https://api.github.com/repos/{}/git/trees/master?recursive=1",
        multisample::PACK_REPO
    ))
    .set("User-Agent", "beatbox")
    .call()
    .map_err(|err| anyhow!("can't list the instrument pack repository: {err}"))?
    .into_json()?;
    let prefix = format!("{}/", pack.folder);
    let mut entries: Vec<(String, u8, i32, u8)> = Vec::new();
    for item in tree["tree"].as_array().into_iter().flatten() {
        let Some(path) = item["path"].as_str() else {
            continue;
        };
        if !path.starts_with(&prefix)
            || !path.to_lowercase().ends_with(".wav")
            || path[prefix.len()..].contains('/')
        {
            continue;
        }
        if let Some((root, d, rr)) = multisample::parse_sample_name(path) {
            if rr < max_rr {
                entries.push((path.to_string(), root, d, rr));
            }
        }
    }
    if entries.is_empty() {
        bail!("pack '{}' has no mappable samples", pack.name);
    }
    // keep the loudest `max_layers` dynamics (most useful for beats)
    let mut dyns: Vec<i32> = entries.iter().map(|x| x.2).collect();
    dyns.sort_unstable();
    dyns.dedup();
    if dyns.len() > max_layers {
        let keep: Vec<i32> = dyns[dyns.len() - max_layers..].to_vec();
        entries.retain(|x| keep.contains(&x.2));
    }
    let dir = e.samples_dir().join(format!("pack_{}", pack.name));
    std::fs::create_dir_all(&dir)?;
    let mut named = Vec::new();
    let mut downloaded = 0;
    for (path, root, d, rr) in &entries {
        let file = path.rsplit('/').next().unwrap_or(path);
        let local = dir.join(file);
        if !local.exists() {
            let url = format!(
                "https://raw.githubusercontent.com/{}/master/{}",
                multisample::PACK_REPO,
                path.split('/')
                    .map(|s| s.replace(' ', "%20").replace('#', "%23"))
                    .collect::<Vec<_>>()
                    .join("/")
            );
            let resp = ureq::get(&url)
                .call()
                .map_err(|err| anyhow!("download {file}: {err}"))?;
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut resp.into_reader(), &mut buf)?;
            std::fs::write(&local, &buf)?;
            downloaded += 1;
        }
        let sname = samples::sample_name(&format!(
            "{}_{}",
            pack.name,
            file.rsplit_once('.').map(|x| x.0).unwrap_or(file)
        ));
        if !e.project.samples.iter().any(|s| s.name == sname) {
            let info = SampleInfo {
                name: sname.clone(),
                path: local.to_string_lossy().to_string(),
                source: format!(
                    "https://github.com/{}/tree/master/{}",
                    multisample::PACK_REPO,
                    pack.folder
                ),
                license: multisample::PACK_LICENSE.into(),
                author: "Versilian Studios".into(),
                duration: 0.0,
            };
            let mut data = samples::decode_file(std::path::Path::new(&info.path))?;
            // trim leading silence so notes speak on time
            audio_edit::trim(&mut data, -60.0);
            e.bank.insert(&sname, data);
            e.project.samples.push(info);
        }
        named.push((sname, *root, *d, *rr));
    }
    Ok((multisample::zones_from(&named), downloaded))
}

// ---------------- tools ----------------

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "design_sound",
            description: "Semantic sound design: describe a sound in words ('warm dusty keys', 'punchy compact kick', 'gritty 808 with glide', 'airy wide pad', 'dark felt piano', 'haunting choir', 'cinematic strings') and get an instrument + effect chain built from presets and parameter moves. Applies to `track` (created if missing; replace_effects=false keeps the current chain). Returns what each word did so you can refine with set_parameters.",
            mutates: true,
            schema: || obj(json!({"description": {"type": "string"}, "track": {"type": "string"}, "replace_effects": {"type": "boolean"}, "volume_db": {"type": "number"}}), &["description", "track"]),
            run: |e, a| {
                let (inst, fxs, applied) = design(&s_req(a, "description")?)?;
                let name = s_req(a, "track")?;
                let created = e.project.track_index(&name).is_err();
                if created { e.project.tracks.push(Track::new(&name, inst.clone())); }
                let i = e.project.track_index(&name)?;
                e.project.tracks[i].instrument = inst.clone();
                if b_or(a, "replace_effects", true) {
                    let n = e.project.tracks[i].effects.len();
                    for k in (0..n).rev() { crate::tools_studio::reindex_fx_lanes(&mut e.project, &name, k); }
                    e.project.tracks[i].effects = fxs.clone();
                } else {
                    e.project.tracks[i].effects.extend(fxs.clone());
                }
                if let Some(v) = f_opt(a, "volume_db") { e.project.tracks[i].volume_db = v; }
                Ok(json!({"track": name, "created": created, "instrument": inst.kind_name(), "effects": e.project.tracks[i].effects.iter().map(|x| x.type_name()).collect::<Vec<_>>(), "applied": applied}))
            },
        },
        Tool {
            name: "layer_instrument",
            description: "Stack sounds on one track (Layer Channel): adds a layer (preset or instrument object) with gain_db, transpose, key_min/key_max (keyboard split), vel_min/vel_max (velocity layer) and delay_ms (e.g. clap 8 ms after the snare). The track's current instrument becomes layer 0. remove=<index> deletes a layer. Max 4 layers.",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}, "preset": {"type": "string"}, "instrument": {"type": "object"}, "gain_db": {"type": "number"}, "transpose": {"type": "number"}, "key_min": {}, "key_max": {}, "vel_min": {"type": "number"}, "vel_max": {"type": "number"}, "delay_ms": {"type": "number"}, "remove": {"type": "integer"}}), &["track"]),
            run: |e, a| {
                let i = e.project.track_index(&s_req(a, "track")?)?;
                let mut lp = ensure_layer(&e.project.tracks[i].instrument);
                if let Some(r) = a.get("remove").and_then(|v| v.as_u64()) {
                    if (r as usize) >= lp.layers.len() { bail!("no layer {r}"); }
                    lp.layers.remove(r as usize);
                    if lp.layers.is_empty() { bail!("can't remove the last layer"); }
                } else {
                    let inst = instrument_from(a)?.ok_or_else(|| anyhow!("give preset or instrument for the new layer"))?;
                    if matches!(inst, Instrument::Layer(_)) { bail!("layers can't be nested"); }
                    if lp.layers.len() >= 4 { bail!("max 4 layers"); }
                    lp.layers.push(Layer {
                        gain_db: f_or(a, "gain_db", -3.0),
                        transpose: f_or(a, "transpose", 0.0),
                        key_min: a.get("key_min").map(pitch_of).transpose()?.unwrap_or(0),
                        key_max: a.get("key_max").map(pitch_of).transpose()?.unwrap_or(127),
                        vel_min: f_or(a, "vel_min", 0.0),
                        vel_max: f_or(a, "vel_max", 1.0),
                        delay_ms: f_or(a, "delay_ms", 0.0),
                        ..Layer::of(inst)
                    });
                }
                let summary: Vec<Value> = lp.layers.iter().map(|l| json!({"instrument": l.instrument.kind_name(), "gain_db": l.gain_db, "transpose": l.transpose, "keys": [l.key_min, l.key_max], "vel": [l.vel_min, l.vel_max], "delay_ms": l.delay_ms})).collect();
                if lp.layers.len() == 1 {
                    e.project.tracks[i].instrument = lp.layers[0].instrument.clone();
                } else {
                    e.project.tracks[i].instrument = Instrument::Layer(lp);
                }
                Ok(json!({"track": e.project.tracks[i].name, "layers": summary}))
            },
        },
        Tool {
            name: "create_macro",
            description: "Create a macro knob: one 0..1 value mapped onto several parameters at once (e.g. 'energy' = lead filter cutoff 800->8000 Hz + reverb mix 0.4->0.15 + drum bus drive). targets=[{track, param, min, max, curve}] with param volume, pan, instrument.<path> or fx.<i>.<path>; curve 1 = linear, 2 = slow start. Then set_macro to turn it (or automate by calling set_macro per section).",
            mutates: true,
            schema: || obj(json!({"name": {"type": "string"}, "targets": {"type": "array", "items": {"type": "object", "properties": {"track": {"type": "string"}, "param": {"type": "string"}, "min": {"type": "number"}, "max": {"type": "number"}, "curve": {"type": "number"}}, "required": ["track", "param", "min", "max"]}}, "value": {"type": "number"}}), &["name", "targets"]),
            run: |e, a| {
                let name = s_req(a, "name")?;
                let arr = a.get("targets").and_then(|v| v.as_array()).ok_or_else(|| anyhow!("targets must be an array"))?;
                let mut targets = Vec::new();
                for t in arr {
                    let owner = owner_of(&e.project, &s_req(t, "track")?)?;
                    let param = s_req(t, "param")?;
                    crate::tools_studio::static_value(&e.project, &owner, &param)?;
                    targets.push(MacroTarget { track: owner, param, min: f_or(t, "min", 0.0), max: f_or(t, "max", 1.0), curve: f_or(t, "curve", 1.0).clamp(0.1, 10.0) });
                }
                e.project.macros.retain(|m| !m.name.eq_ignore_ascii_case(&name));
                let value = f_or(a, "value", 0.5).clamp(0.0, 1.0);
                e.project.macros.push(Macro { name: name.clone(), value, targets });
                let applied = apply_macro(&mut e.project, &name, value)?;
                Ok(json!({"macro": name, "value": value, "applied": applied}))
            },
        },
        Tool {
            name: "set_macro",
            description: "Turn a macro knob (0..1): every mapped parameter moves together. list=true returns all macros.",
            mutates: true,
            schema: || obj(json!({"name": {"type": "string"}, "value": {"type": "number", "minimum": 0, "maximum": 1}, "list": {"type": "boolean"}}), &[]),
            run: |e, a| {
                if b_or(a, "list", false) || s_opt(a, "name").is_none() {
                    return Ok(json!({"macros": e.project.macros.iter().map(|m| json!({"name": m.name, "value": m.value, "targets": m.targets.iter().map(|t| format!("{}:{} {}..{}", t.track, t.param, t.min, t.max)).collect::<Vec<_>>()})).collect::<Vec<_>>()}));
                }
                let name = s_req(a, "name")?;
                let v = f_opt(a, "value").ok_or_else(|| anyhow!("missing value"))?.clamp(0.0, 1.0);
                let applied = apply_macro(&mut e.project, &name, v)?;
                Ok(json!({"macro": name, "value": v, "applied": applied}))
            },
        },
        Tool {
            name: "describe_effect",
            description: "Parameter schema of an effect (plugin.describe): every parameter with type, current/default value, min/max, unit, log-scale flag, enum options and whether it is automatable. Give track + index for a live instance (shows current values and its automation path fx.<i>.<param>), or type for the defaults of any effect type (omit both to list all types).",
            mutates: false,
            schema: || obj(json!({"track": {"type": "string"}, "index": {"type": ["integer", "string"], "description": "Effect position (0-based) or its stable id, e.g. \"reverb1\" (see get_effects)"}, "type": {"type": "string"}}), &[]),
            run: |e, a| {
                let (fxv, prefix, owner) = match (s_opt(a, "track"), s_opt(a, "type")) {
                    (Some(t), _) => {
                        let owner = owner_of(&e.project, &t)?;
                        let idx = u_or(a, "index", 0) as usize;
                        let chain = chain_of(&e.project, &owner);
                        let f = chain.get(idx).cloned().ok_or_else(|| anyhow!("no effect {idx} on '{owner}' ({} effects)", chain.len()))?;
                        (f, format!("fx.{idx}"), Some(owner))
                    }
                    (None, Some(t)) => (Effect::from_type(&t).ok_or_else(|| anyhow!("unknown effect type '{t}'"))?, String::new(), None),
                    (None, None) => return Ok(json!({"effect_types": fx::EFFECT_TYPES.iter().map(|(n, d)| json!({"type": n, "description": d})).collect::<Vec<_>>()})),
                };
                let kind = fxv.type_name();
                let mut params = Vec::new();
                describe_value(&kind, "", &serde_json::to_value(&fxv)?, &mut params);
                let desc = fx::EFFECT_TYPES.iter().find(|(n, _)| *n == kind).map(|x| x.1).unwrap_or("");
                Ok(json!({"type": kind, "description": desc, "bypassed": fxv.bypassed(), "owner": owner, "automation_prefix": prefix, "parameters": params, "set_with": "set_parameters {track, changes: {\"fx.<i>.<param>\": value}}"}))
            },
        },
        Tool {
            name: "describe_instrument",
            description: "Parameter schema of a track's instrument (or of an instrument kind / preset): every parameter with type, value, range, unit, enum options; plus the presets of that kind. Use before set_parameters / automation.",
            mutates: false,
            schema: || obj(json!({"track": {"type": "string"}, "preset": {"type": "string"}}), &[]),
            run: |e, a| {
                let inst = match (s_opt(a, "track"), s_opt(a, "preset")) {
                    (Some(t), _) => { let i = e.project.track_index(&t)?; e.project.tracks[i].instrument.clone() }
                    (None, Some(p)) => instruments::preset(&p).ok_or_else(|| anyhow!("unknown preset '{p}'"))?,
                    _ => bail!("give track or preset"),
                };
                let kind = inst.kind_name();
                let mut params = Vec::new();
                describe_value(kind, "", &serde_json::to_value(&inst)?, &mut params);
                let presets: Vec<&str> = instruments::PRESETS.iter().filter(|(n, _)| instruments::preset(n).map(|p| p.kind_name() == kind).unwrap_or(false)).map(|(n, _)| *n).collect();
                let mut extra = json!({});
                if let Instrument::Drum(d) = &inst {
                    extra = json!({"effective": {"tune_hz_approx": (match d.kind { instruments::DrumKind::Kick => 48.0, instruments::DrumKind::Tom => 110.0, _ => 200.0 }) * 2f32.powf(d.tune / 12.0), "decay_multiplier": d.decay}});
                }
                Ok(json!({"kind": kind, "parameters": params, "presets_of_this_kind": presets, "automation_prefix": "instrument", "info": extra}))
            },
        },
        Tool {
            name: "set_parameters",
            description: "Set many parameters in one atomic call (plugin.set_parameters): changes = {\"instrument.cutoff\": 900, \"instrument.amp_env.release\": 0.4, \"fx.1.mix\": 0.2, \"fx.0.bands.1.gain_db\": -3, \"fx.2.mode\": \"highpass\", \"volume\": -4, \"pan\": 0.2}. Numbers, bools, enum strings and whole arrays are accepted. All-or-nothing: if one change is invalid nothing is applied.",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string", "description": "track, bus or master"}, "changes": {"type": "object"}}), &["track", "changes"]),
            run: |e, a| {
                let owner = owner_of(&e.project, &s_req(a, "track")?)?;
                let ch = a.get("changes").and_then(|v| v.as_object()).ok_or_else(|| anyhow!("changes must be an object {{param: value}}"))?;
                if ch.is_empty() { bail!("no changes"); }
                let mut out = Map::new();
                for (k, v) in ch { out.insert(k.clone(), set_param(&mut e.project, &owner, k, v)?); }
                let auto: Vec<String> = ch.keys().filter(|k| e.project.automation.iter().any(|l| l.enabled && !l.points.is_empty() && l.is_target(&owner, k))).cloned().collect();
                let mut r = json!({"track": owner, "set": out});
                if !auto.is_empty() { r["warnings"] = json!(auto.iter().map(|k| format!("automation on '{k}' overrides this static value")).collect::<Vec<_>>()); }
                Ok(r)
            },
        },
        Tool {
            name: "bypass_effect",
            description: "Bypass (or re-enable) an effect without removing it: A/B an effect in place. on=true bypasses, false enables; omit to toggle.",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}, "index": {"type": ["integer", "string"], "description": "Effect position (0-based) or its stable id, e.g. \"reverb1\" (see get_effects)"}, "on": {"type": "boolean"}}), &["track", "index"]),
            run: |e, a| {
                let owner = owner_of(&e.project, &s_req(a, "track")?)?;
                let idx = u_or(a, "index", 0) as usize;
                let chain = crate::tools::effects_of(&mut e.project, &owner)?;
                let fx = chain.get_mut(idx).ok_or_else(|| anyhow!("no effect at index {idx}"))?;
                let on = a.get("on").and_then(|v| v.as_bool()).unwrap_or(!fx.bypassed());
                fx.set_bypass(on);
                Ok(json!({"track": owner, "index": idx, "effect": fx.type_name(), "bypassed": on}))
            },
        },
        Tool {
            name: "reorder_effects",
            description: "Reorder an effect chain: move from -> to, or give the full new order as indices (order: [2,0,1]). Automation lanes on fx.<i> follow their effects.",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}, "from": {"type": ["integer", "string"], "description": "position or stable id"}, "to": {"type": ["integer", "string"]}, "order": {"type": "array", "items": {"type": "integer"}}}), &["track"]),
            run: |e, a| {
                let owner = owner_of(&e.project, &s_req(a, "track")?)?;
                let n = chain_of(&e.project, &owner).len();
                let order: Vec<usize> = match a.get("order").and_then(|v| v.as_array()) {
                    Some(o) => o.iter().map(|x| x.as_u64().map(|v| v as usize).ok_or_else(|| anyhow!("order must be integers"))).collect::<Result<_>>()?,
                    None => {
                        let f = u_or(a, "from", u64::MAX) as usize;
                        let t = u_or(a, "to", u64::MAX) as usize;
                        if f >= n || t >= n { bail!("from/to must be < {n}"); }
                        let mut v: Vec<usize> = (0..n).collect();
                        let x = v.remove(f);
                        v.insert(t, x);
                        v
                    }
                };
                let mut sorted = order.clone();
                sorted.sort_unstable();
                if sorted != (0..n).collect::<Vec<_>>() { bail!("order must be a permutation of 0..{}", n.saturating_sub(1)); }
                let chain = crate::tools::effects_of(&mut e.project, &owner)?;
                let old = chain.clone();
                *chain = order.iter().map(|&i| old[i].clone()).collect();
                let names: Vec<String> = chain.iter().map(|x| x.type_name()).collect();
                for l in e.project.automation.iter_mut() {
                    if !l.target.eq_ignore_ascii_case(&owner) { continue; }
                    if let Some(Target::Fx(i, path)) = parse_target(&l.param) {
                        if let Some(newi) = order.iter().position(|&o| o == i) { l.param = format!("fx.{newi}.{}", path.join(".")); }
                    }
                }
                Ok(json!({"track": owner, "chain": names}))
            },
        },
        Tool {
            name: "add_multisample_track",
            description: "Create a multisampled instrument track (key zones x velocity layers x round robin, SFZ-style): zones=[{sample, root, lo_note, hi_note, vel_lo, vel_hi (0..1), tune_cents, gain_db, rr}] using registered samples, or sfz=<path to .sfz> (samples relative to it are imported). Each note plays from the nearest zone with crossfades between velocity layers. release (s), one_shot, round_robin.",
            mutates: true,
            schema: || obj(json!({"name": {"type": "string"}, "zones": {"type": "array", "items": {"type": "object"}}, "sfz": {"type": "string"}, "release": {"type": "number"}, "one_shot": {"type": "boolean"}, "round_robin": {"type": "boolean"}, "volume_db": {"type": "number"}}), &["name"]),
            run: |e, a| {
                let name = s_req(a, "name")?;
                let mut params = MultisampleParams { release: f_or(a, "release", 0.35), one_shot: b_or(a, "one_shot", false), round_robin: b_or(a, "round_robin", true), ..Default::default() };
                if let Some(sfz) = s_opt(a, "sfz") {
                    let path = e.resolve(&sfz);
                    let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
                    let (regions, rel) = multisample::parse_sfz(&text)?;
                    if let Some(r) = rel { if a.get("release").is_none() { params.release = r; } }
                    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                    for (k, r) in regions.iter().enumerate() {
                        let sp = dir.join(&r.sample);
                        let sname = samples::sample_name(&format!("{}_{}", name, sp.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| k.to_string())));
                        if !e.project.samples.iter().any(|s| s.name == sname) {
                            crate::tools::register_sample(e, SampleInfo { name: sname.clone(), path: sp.to_string_lossy().to_string(), source: path.to_string_lossy().to_string(), license: "see sfz".into(), author: String::new(), duration: 0.0 })?;
                        }
                        params.zones.push(Zone { sample: sname, root: r.root, lo_note: r.lo, hi_note: r.hi, vel_lo: r.vel_lo as f32 / 127.0, vel_hi: (r.vel_hi as f32 + 1.0) / 128.0, tune_cents: r.tune, gain_db: r.volume, rr: r.seq.saturating_sub(1) });
                    }
                    params.source = path.to_string_lossy().to_string();
                } else {
                    let zs = a.get("zones").and_then(|v| v.as_array()).ok_or_else(|| anyhow!("give zones or sfz"))?;
                    for z in zs {
                        let sample = s_req(z, "sample")?;
                        if !e.project.samples.iter().any(|s| s.name == sample) { bail!("no sample '{sample}' (import_sample / download_sample first)"); }
                        let root = z.get("root").map(pitch_of).transpose()?.unwrap_or(60);
                        params.zones.push(Zone { sample, root, lo_note: z.get("lo_note").map(pitch_of).transpose()?.unwrap_or(0), hi_note: z.get("hi_note").map(pitch_of).transpose()?.unwrap_or(127), vel_lo: f_or(z, "vel_lo", 0.0), vel_hi: f_or(z, "vel_hi", 1.0), tune_cents: f_or(z, "tune_cents", 0.0), gain_db: f_or(z, "gain_db", 0.0), rr: u_or(z, "rr", 0) as u8 });
                    }
                }
                if params.zones.is_empty() { bail!("no zones"); }
                let n = params.zones.len();
                let inst = Instrument::Multisample(params);
                match e.project.track_index(&name) {
                    Ok(i) => e.project.tracks[i].instrument = inst,
                    Err(_) => { let mut t = Track::new(&name, inst); t.volume_db = f_or(a, "volume_db", 0.0); e.project.tracks.push(t); }
                }
                Ok(json!({"track": name, "zones": n}))
            },
        },
        Tool {
            name: "install_instrument_pack",
            description: "Download a real CC0 multisampled instrument (VSCO 2 Community Edition) and create a track playing it: upright_piano, violins_sus, violins_spic, violins_pizz, violins_trem, violas_sus, celli_sus, celli_spic, celli_pizz, contrabass_sus, harp, flute_sus, clarinet_sus, oboe_sus, bassoon_sus, horn_sus, trumpet_sus, trombone_sus, tuba_sus (aliases: piano, strings, cellos, horn). Zones, velocity layers and round robins are built from the file names and the licence is recorded. max_layers / max_rr limit the download. Offline? Use the built-in modelled presets grand_piano, felt_piano, string_section, choir instead. list=true shows the catalog.",
            mutates: true,
            schema: || obj(json!({"pack": {"type": "string"}, "track": {"type": "string"}, "max_layers": {"type": "integer"}, "max_rr": {"type": "integer"}, "list": {"type": "boolean"}, "volume_db": {"type": "number"}}), &[]),
            run: |e, a| {
                if b_or(a, "list", false) || s_opt(a, "pack").is_none() {
                    return Ok(json!({"packs": multisample::PACKS.iter().map(|p| json!({"pack": p.name, "description": p.description})).collect::<Vec<_>>(), "license": multisample::PACK_LICENSE}));
                }
                let pk = multisample::pack(&s_req(a, "pack")?).ok_or_else(|| anyhow!("unknown pack. list=true shows the catalog"))?;
                let (zones, downloaded) = install_pack(e, pk, u_or(a, "max_rr", 2) as u8, u_or(a, "max_layers", 3) as usize)?;
                let name = s_opt(a, "track").unwrap_or_else(|| pk.name.to_string());
                let n = zones.len();
                let inst = Instrument::Multisample(MultisampleParams { zones, release: pk.release, one_shot: pk.one_shot, source: format!("{} ({})", pk.folder, multisample::PACK_LICENSE), ..Default::default() });
                match e.project.track_index(&name) {
                    Ok(i) => e.project.tracks[i].instrument = inst,
                    Err(_) => { let mut t = Track::new(&name, inst); t.volume_db = f_or(a, "volume_db", -3.0); e.project.tracks.push(t); }
                }
                Ok(json!({"track": name, "pack": pk.name, "zones": n, "files_downloaded": downloaded, "license": multisample::PACK_LICENSE}))
            },
        },
        Tool {
            name: "slice_sample",
            description: "Slice a sample (breakbeat, vocal phrase, loop) at its transients into a playable slice kit: creates a sampler track where note root+i plays slice i. sensitivity 0..1, min_gap_ms, max_slices, or equal=N for an even grid. write_pattern=true writes the original order into the current pattern at the project tempo so you can rearrange it with note edits.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "track": {"type": "string"}, "method": {"type": "string", "enum": ["onsets", "transients"], "description": "onsets (energy, default) or transients (spectral flux, better on dense breaks)"}, "sensitivity": {"type": "number"}, "min_gap_ms": {"type": "number"}, "max_slices": {"type": "integer"}, "equal": {"type": "integer"}, "root": {}, "write_pattern": {"type": "boolean"}, "pattern": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, data) = sample_data(e, &s_req(a, "sample")?)?;
                let mut on: Vec<usize> = match a.get("equal").and_then(|v| v.as_u64()) {
                    Some(nn) => { let nn = nn.clamp(2, 64) as usize; (0..nn).map(|i| i * data.len() / nn).collect() }
                    None if s_opt(a, "method").as_deref() == Some("transients") => crate::sc_dsp::detect_transients(&data, f_or(a, "sensitivity", 0.5)),
                    None => audio_edit::onsets(&data, f_or(a, "sensitivity", 0.5), f_or(a, "min_gap_ms", 70.0)),
                };
                if on.first().copied().unwrap_or(1) > (0.02 * SR) as usize { on.insert(0, 0); }
                if let Some(first) = on.first_mut() { *first = 0; }
                on.truncate(u_or(a, "max_slices", 32).clamp(1, 64) as usize);
                let root = a.get("root").map(pitch_of).transpose()?.unwrap_or(36);
                let slices: Vec<f32> = on.iter().map(|&s| s as f32 / SR).collect();
                let name = s_opt(a, "track").unwrap_or_else(|| format!("{}_slices", info.name));
                let inst = Instrument::Sampler(instruments::SamplerParams { sample: info.name.clone(), root, one_shot: true, slices: slices.clone(), ..Default::default() });
                match e.project.track_index(&name) { Ok(i) => e.project.tracks[i].instrument = inst, Err(_) => e.project.tracks.push(Track::new(&name, inst)) }
                let mut written = 0;
                if b_or(a, "write_pattern", true) {
                    let pi = crate::tools::pattern_idx(&e.project, a)?;
                    let step = e.project.step_secs();
                    let steps = e.project.patterns[pi].steps() as f32;
                    let mut notes = Vec::new();
                    for (k, t) in slices.iter().enumerate() {
                        let st = t / step;
                        if st >= steps { break; }
                        let next = slices.get(k + 1).copied().unwrap_or(data.len() as f32 / SR);
                        notes.push(Note::new((st * 4.0).round() / 4.0, ((next - t) / step).max(0.25), (root as usize + k).min(127) as u8, 0.9));
                    }
                    written = notes.len();
                    *e.project.patterns[pi].notes_mut(&name) = notes;
                }
                Ok(json!({"track": name, "slices": slices.len(), "map": slices.iter().enumerate().map(|(k, t)| json!({"note": crate::theory::note_name((root as usize + k).min(127) as u8), "start_s": (t * 1000.0).round() / 1000.0})).collect::<Vec<_>>(), "notes_written": written}))
            },
        },
        Tool {
            name: "stretch_sample",
            description: "Offline time-stretch (WSOLA, pitch kept) and/or pitch-shift (length kept) a sample into a new sample: factor (>1 slower), or target_bpm with source_bpm (detected if omitted) to warp a loop to the song tempo (target_bpm defaults to the project's), semitones for pitch. Returns the new sample name.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "factor": {"type": "number"}, "source_bpm": {"type": "number"}, "target_bpm": {"type": "number"}, "semitones": {"type": "number"}, "new_name": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, data) = sample_data(e, &s_req(a, "sample")?)?;
                let mut note = Vec::new();
                let factor = match f_opt(a, "factor") {
                    Some(f) => f,
                    None if a.get("target_bpm").is_some() || a.get("source_bpm").is_some() => {
                        let src = f_opt(a, "source_bpm").unwrap_or_else(|| audio_edit::estimate_bpm(&data).0);
                        if src <= 0.0 { bail!("couldn't detect the loop tempo; pass source_bpm"); }
                        let dst = f_opt(a, "target_bpm").unwrap_or(e.project.bpm);
                        note.push(format!("{src} -> {dst} BPM"));
                        src / dst
                    }
                    None => 1.0,
                };
                let mut out = audio_edit::time_stretch(&data, factor.clamp(0.25, 4.0));
                if (factor - 1.0).abs() > 1e-3 { note.push(format!("x{factor:.3} length")); }
                if let Some(st) = f_opt(a, "semitones").filter(|s| s.abs() > 1e-3) {
                    out = audio_edit::pitch_shift(&out, st);
                    note.push(format!("{st:+} st"));
                }
                if note.is_empty() { bail!("nothing to do: give factor, target_bpm/source_bpm or semitones"); }
                let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_stretched", info.name));
                let mut r = save_new_sample(e, &info, &nn, &out, &note.join(", "))?;
                r["seconds"] = json!((out.len() as f32 / SR * 100.0).round() / 100.0);
                Ok(r)
            },
        },
        Tool {
            name: "edit_sample",
            description: "Non-destructive sample editing into a new sample: trim_silence_db (cut leading/trailing audio below this, e.g. -50), start_s + length_s or length_beats (cut a region at the project tempo), reverse, fade_in_ms / fade_out_ms, normalize_db (peak) or normalize_lufs, gain_db, highpass_hz. Applied in that order. new_name defaults to <sample>_edit.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "trim_silence_db": {"type": "number"}, "start_s": {"type": "number"}, "length_s": {"type": "number"}, "length_beats": {"type": "number"}, "reverse": {"type": "boolean"}, "fade_in_ms": {"type": "number"}, "fade_out_ms": {"type": "number"}, "normalize_db": {"type": "number"}, "normalize_lufs": {"type": "number"}, "gain_db": {"type": "number"}, "highpass_hz": {"type": "number"}, "new_name": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, mut d) = sample_data(e, &s_req(a, "sample")?)?;
                let mut done = Vec::new();
                if let Some(t) = f_opt(a, "trim_silence_db") { let (x, y) = audio_edit::trim(&mut d, t); done.push(format!("trim {:.0}/{:.0} ms", x as f32 / SR * 1000.0, y as f32 / SR * 1000.0)); }
                let start = (f_or(a, "start_s", 0.0).max(0.0) * SR) as usize;
                let len = f_opt(a, "length_s").map(|s| (s * SR) as usize).or_else(|| f_opt(a, "length_beats").map(|b| (b * 60.0 / e.project.bpm * SR) as usize));
                if start > 0 || len.is_some() {
                    let s0 = start.min(d.len());
                    let s1 = len.map(|l| (s0 + l).min(d.len())).unwrap_or(d.len());
                    d = d[s0..s1].to_vec();
                    done.push(format!("region {:.3}s +{:.3}s", s0 as f32 / SR, (s1 - s0) as f32 / SR));
                }
                if b_or(a, "reverse", false) { audio_edit::reverse(&mut d); done.push("reverse".into()); }
                let (fi, fo) = (f_or(a, "fade_in_ms", 0.0), f_or(a, "fade_out_ms", 0.0));
                if fi > 0.0 || fo > 0.0 { audio_edit::fade(&mut d, fi, fo); done.push(format!("fades {fi}/{fo} ms")); }
                if let Some(hz) = f_opt(a, "highpass_hz") {
                    let mut f = crate::dsp::Biquad::new(crate::dsp::BiquadKind::LowCut, hz, 0.707, 0.0);
                    for v in d.iter_mut() { *v = f.process(*v); }
                    done.push(format!("highpass {hz} Hz"));
                }
                if let Some(p) = f_opt(a, "normalize_db") { let g = audio_edit::normalize(&mut d, p); done.push(format!("normalize {g:+.1} dB")); }
                if let Some(t) = f_opt(a, "normalize_lufs") {
                    let l = crate::analysis::loudness(&d, &d).integrated_lufs;
                    if l > -70.0 { let g = crate::dsp::db_to_gain(t - l); d.iter_mut().for_each(|v| *v *= g); done.push(format!("loudness {l:.1} -> {t} LUFS")); }
                }
                if let Some(g) = f_opt(a, "gain_db") { let k = crate::dsp::db_to_gain(g); d.iter_mut().for_each(|v| *v *= k); done.push(format!("gain {g:+} dB")); }
                if done.is_empty() { bail!("no edits given"); }
                if d.is_empty() { bail!("the edit left no audio"); }
                let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_edit", info.name));
                let mut r = save_new_sample(e, &info, &nn, &d, &done.join(", "))?;
                r["edits"] = json!(done);
                Ok(r)
            },
        },
        Tool {
            name: "denoise_sample",
            description: "Noise reduction (FL Edison denoise) into a new sample: a spectral gate reads the noise floor per frequency from the quietest frames and pulls bins near it down. Use it on a hissy recording, an old tape or a vocal take before tuning. strength 0..1 (default 0.8), floor_db = the deepest cut (default -24; -12 is gentle). new_name defaults to <sample>_denoised.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "strength": {"type": "number"}, "floor_db": {"type": "number"}, "new_name": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, d) = sample_data(e, &s_req(a, "sample")?)?;
                let strength = f_or(a, "strength", 0.8).clamp(0.0, 1.0);
                let floor = f_or(a, "floor_db", -24.0).clamp(-60.0, 0.0);
                if d.len() < 4096 {
                    bail!("sample too short to read a noise floor (needs ~0.1 s)");
                }
                let (y, reduced) = crate::speech_song::denoise(&d, floor, strength);
                let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_denoised", info.name));
                let what = format!("denoise strength {strength} floor {floor} dB");
                let mut r = save_new_sample(e, &info, &nn, &y, &what)?;
                r["noise_reduced_db"] = json!((reduced * 10.0).round() / 10.0);
                Ok(r)
            },
        },
        Tool {
            name: "analyze_audio",
            description: "Listen to any audio: a registered sample (sample), a file (path: wav/mp3/flac/ogg) or the current mix (neither). Returns tempo (BPM + confidence), key candidates, onset count/times, LUFS, true peak, loudness range, crest, spectral balance, stereo width. Use it to check a downloaded loop's tempo/key before stretch_sample, or to inspect a bounce.",
            mutates: false,
            schema: || obj(json!({"sample": {"type": "string"}, "path": {"type": "string"}, "max_onsets": {"type": "integer"}}), &[]),
            run: |e, a| {
                let (name, l, r) = audio_for(e, a)?;
                let prof = audio_edit::profile(&name, &l, &r);
                let mono: Vec<f32> = l.iter().zip(r.iter()).map(|(x, y)| 0.5 * (x + y)).collect();
                let on = audio_edit::onsets(&mono, 0.5, 60.0);
                let keys = audio_edit::estimate_key(&mono);
                let mut v = serde_json::to_value(&prof)?;
                v["key_candidates"] = json!(keys.iter().map(|(k, c)| json!({"key": k, "score": c})).collect::<Vec<_>>());
                v["key_detail"] = audio_edit::key_report(&mono);
                v["onsets"] = json!(on.len());
                v["onset_times_s"] = json!(on.iter().take(u_or(a, "max_onsets", 32) as usize).map(|s| (*s as f32 / SR * 1000.0).round() / 1000.0).collect::<Vec<_>>());
                Ok(v)
            },
        },
    ]
}

fn apply_macro(p: &mut Project, name: &str, value: f32) -> Result<Vec<String>> {
    let mi = p
        .macros
        .iter()
        .position(|m| m.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            anyhow!(
                "no macro '{name}'. Macros: [{}]",
                p.macros
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    p.macros[mi].value = value;
    let targets = p.macros[mi].targets.clone();
    let mut out = Vec::new();
    for t in targets {
        let x = value.powf(t.curve);
        let log = automation::is_log_param(&t.param) && t.min > 0.0 && t.max > 0.0;
        let v = if log {
            (t.min.ln() + (t.max.ln() - t.min.ln()) * x).exp()
        } else {
            t.min + (t.max - t.min) * x
        };
        set_param(p, &t.track, &t.param, &json!(v))?;
        out.push(format!("{}:{} = {:.3}", t.track, t.param, v));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eng() -> Engine {
        Engine::new(std::env::temp_dir().join("beatbox_sound_tests"))
    }

    #[test]
    fn denoise_sample_lowers_the_hiss_floor() {
        let mut e = eng();
        let dir = std::env::temp_dir().join("bb_denoise_test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut rng = crate::dsp::Rng::new(3);
        // 1 s hiss, then 1 s tone + hiss
        let x: Vec<f32> = (0..(2.0 * SR) as usize)
            .map(|i| 0.02 * rng.bipolar() + if i as f32 > SR { 0.4 * (i as f32 * 440.0 / SR * std::f32::consts::TAU).sin() } else { 0.0 })
            .collect();
        let wav = dir.join("hiss.wav");
        crate::render::write_wav(&wav, &x, &x).unwrap();
        e.call("import_sample", &json!({"path": wav.to_string_lossy(), "name": "hiss"})).unwrap();
        let r = e.call("denoise_sample", &json!({"sample": "hiss"})).unwrap();
        // (the tone dominates the total, so the overall figure is small)
        assert!(r["noise_reduced_db"].as_f64().unwrap() <= 0.0, "{r}");
        let (_, y) = sample_data(&mut e, r["sample"].as_str().unwrap()).unwrap();
        let rms = |s: &[f32]| (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
        let q = (0.8 * SR) as usize;
        assert!(rms(&y[2048..q]) < 0.5 * rms(&x[2048..q]), "hiss not reduced");
    }

    #[test]
    fn design_and_describe_and_set() {
        let mut e = eng();
        let r = e
            .call(
                "design_sound",
                &json!({"description": "warm dusty keys", "track": "keys"}),
            )
            .unwrap();
        assert!(r["applied"].as_array().unwrap().len() >= 3, "{r}");
        e.call(
            "design_sound",
            &json!({"description": "punchy compact kick", "track": "kick"}),
        )
        .unwrap();
        e.call(
            "design_sound",
            &json!({"description": "gritty 808 with glide", "track": "bass"}),
        )
        .unwrap();
        e.call(
            "design_sound",
            &json!({"description": "airy wide pad", "track": "pad"}),
        )
        .unwrap();
        let d = e
            .call("describe_effect", &json!({"track": "keys", "index": 0}))
            .unwrap();
        assert!(d["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["param"] == "drive"));
        let d = e
            .call("describe_effect", &json!({"type": "parametric_eq"}))
            .unwrap();
        assert!(d["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["param"] == "bands.0.kind" && p["type"] == "enum"));
        let di = e
            .call("describe_instrument", &json!({"track": "bass"}))
            .unwrap();
        assert!(di["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["param"] == "glide_ms"));
        e.call("set_parameters", &json!({"track": "keys", "changes": {"instrument.index": 3.0, "fx.0.mix": 0.5, "volume": -4}})).unwrap();
        assert!(e
            .call(
                "set_parameters",
                &json!({"track": "keys", "changes": {"fx.0.mix": 0.1, "instrument.nope": 1}})
            )
            .is_err());
        // atomic: the failed call left fx.0.mix at 0.5
        assert!(
            matches!(&e.project.tracks[0].effects[0], Effect::Distortion(d) if (d.mix - 0.5).abs() < 1e-6)
        );
    }

    #[test]
    fn layers_macros_bypass_reorder() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "snare", "preset": "snare"}))
            .unwrap();
        e.call(
            "layer_instrument",
            &json!({"track": "snare", "preset": "clap", "delay_ms": 8}),
        )
        .unwrap();
        assert!(
            matches!(&e.project.tracks[0].instrument, Instrument::Layer(l) if l.layers.len() == 2)
        );
        e.call("add_effect", &json!({"track": "snare", "type": "filter"}))
            .unwrap();
        e.call("add_effect", &json!({"track": "snare", "type": "reverb"}))
            .unwrap();
        e.call(
            "add_automation",
            &json!({"track": "snare", "param": "fx.0.cutoff", "points": [[0, 500], [4, 5000]]}),
        )
        .unwrap();
        e.call(
            "reorder_effects",
            &json!({"track": "snare", "from": 0, "to": 1}),
        )
        .unwrap();
        assert_eq!(e.project.tracks[0].effects[1].type_name(), "filter");
        assert!(e
            .project
            .automation
            .iter()
            .any(|l| l.param == "fx.1.cutoff"));
        e.call("bypass_effect", &json!({"track": "snare", "index": 0}))
            .unwrap();
        assert!(e.project.tracks[0].effects[0].bypassed());
        e.call("create_macro", &json!({"name": "space", "targets": [{"track": "snare", "param": "fx.0.mix", "min": 0.0, "max": 0.6}, {"track": "snare", "param": "volume", "min": -6, "max": 0}]})).unwrap();
        e.call("set_macro", &json!({"name": "space", "value": 1.0}))
            .unwrap();
        assert_eq!(e.project.tracks[0].volume_db, 0.0);
        assert!(
            matches!(&e.project.tracks[0].effects[0], Effect::Reverb(r) if (r.mix - 0.6).abs() < 1e-5)
        );
    }

    #[test]
    fn sample_edit_slice_stretch_and_multisample() {
        let mut e = eng();
        // a synthetic 4-hit loop at 120 BPM
        let n = (2.0 * SR) as usize;
        let mut x = vec![0.0f32; n];
        let mut rng = crate::dsp::Rng::new(3);
        for k in 0..4 {
            let s = k * n / 4;
            for j in 0..3000 {
                x[s + j] = rng.bipolar() * (-(j as f32) / 400.0).exp() * 0.7;
            }
        }
        let path = std::env::temp_dir().join("beatbox_sound_tests/loop.wav");
        crate::render::write_wav(&path, &x, &x).unwrap();
        e.call("import_sample", &json!({"path": path, "name": "loop"}))
            .unwrap();
        let s = e.call("slice_sample", &json!({"sample": "loop"})).unwrap();
        assert_eq!(s["slices"], 4, "{s}");
        let a = e.call("analyze_audio", &json!({"sample": "loop"})).unwrap();
        assert_eq!(a["onsets"], 4);
        let st = e
            .call("stretch_sample", &json!({"sample": "loop", "factor": 1.25}))
            .unwrap();
        assert!((st["seconds"].as_f64().unwrap() - 2.5).abs() < 0.05);
        let ed = e.call("edit_sample", &json!({"sample": "loop", "length_beats": 2, "reverse": true, "fade_out_ms": 20, "normalize_db": -1})).unwrap();
        assert!(
            (ed["duration"].as_f64().unwrap() - 1.0).abs() < 0.02,
            "{ed}"
        );
        e.call("add_multisample_track", &json!({"name": "ms", "zones": [{"sample": "loop", "root": 60, "hi_note": 64}, {"sample": "loop_edit", "root": 70, "lo_note": 65}]})).unwrap();
        e.call("add_notes", &json!({"track": "ms", "notes": [{"start": 0, "pitch": 60}, {"start": 4, "pitch": 72}]})).unwrap();
        assert!(e.analyze().unwrap().master.rms_dbfs > -50.0);
        // sfz
        let sfz = std::env::temp_dir().join("beatbox_sound_tests/kit.sfz");
        std::fs::write(
            &sfz,
            "<region> sample=loop.wav lokey=0 hikey=127 pitch_keycenter=60",
        )
        .unwrap();
        let r = e
            .call("add_multisample_track", &json!({"name": "sfz", "sfz": sfz}))
            .unwrap();
        assert_eq!(r["zones"], 1);
    }
}
