//! Tools for sound palettes: list them, apply one to the project, fetch the
//! curated CC0 one-shots into the sample library, and audition a palette's
//! voices as one-shot WAVs.

use crate::dsp::SR;
use crate::engine::Engine;
use crate::palette::{self, ApplyOpts};
use crate::tools::{b_or, obj, s_opt, s_req, Tool};
use anyhow::Result;
use serde_json::{json, Value};

fn strings(a: &Value, k: &str) -> Vec<String> {
    a.get(k)
        .and_then(|v| v.as_array())
        .map(|xs| {
            xs.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn oneshots(e: &mut Engine, a: &Value) -> Result<Value> {
    let name = s_req(a, "palette")?;
    let pal = palette::get(&name)?;
    let dir =
        e.resolve(&s_opt(a, "out_dir").unwrap_or_else(|| format!("renders/palette_{}", pal.name)));
    std::fs::create_dir_all(&dir)?;
    let mut files = Vec::new();
    for (role, preset) in pal.voices {
        let Some(inst) = palette::voice(pal, role) else {
            continue;
        };
        let pitch = if *role == "bass" { 36.0 } else { 60.0 };
        let gate = if *role == "pad" { 1.5 } else { 0.5 };
        let x = crate::instruments::render_note(&inst, pitch, 0.9, gate, &e.bank, 1);
        let path = dir.join(format!("{}_{role}_{preset}.wav", pal.name));
        crate::render::write_wav(&path, &x, &x)?;
        let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let rms = (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt();
        files.push(json!({
            "role": role,
            "preset": preset,
            "path": path.to_string_lossy(),
            "seconds": (x.len() as f32 / SR * 100.0).round() / 100.0,
            "peak_db": (crate::dsp::gain_to_db(peak) * 10.0).round() / 10.0,
            "rms_db": (crate::dsp::gain_to_db(rms) * 10.0).round() / 10.0,
        }));
    }
    Ok(json!({"palette": pal.name, "files": files}))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "list_palettes",
            description: "Curated sound palettes (dark_trap, dhh_grit, boom_bap_dusty, drill_slide, melodic_airy): for each, the genres it suits, its character tags, the voice (preset) per role (kick, snare, clap, hat, open_hat, perc, bass, keys, pad, lead, bell, pluck), its CC0 sample kit and a mix hint. Pick sounds by character instead of one preset at a time.",
            mutates: false,
            schema: || obj(json!({"palette": {"type": "string", "description": "one palette (default: all)"}}), &[]),
            run: |_, a| {
                if let Some(n) = s_opt(a, "palette") {
                    return Ok(palette::describe(palette::get(&n)?));
                }
                Ok(json!({
                    "palettes": palette::PALETTES.iter().map(palette::describe).collect::<Vec<_>>(),
                    "roles": palette::ROLES,
                    "curated_samples": palette::manifest().len(),
                    "next": "apply_palette {palette} (add use_samples:true for the CC0 one-shot kit)",
                }))
            },
        },
        Tool {
            name: "apply_palette",
            description: "Swap the project's tracks to a sound palette's voices by role (kick, snare, clap, hats, 808/bass, keys, pad, lead, bell, pluck), level-matched so the mix balance holds. use_samples:true uses the palette's curated CC0 drum one-shots (downloaded once, checksum-verified). Indian instruments, vocals, fx, crashes and featured modelled instruments (piano, strings, choir, multisamples) are kept unless force:true. roles limits it (e.g. [\"kick\",\"snare\"]). A genre name works too (trap -> dark_trap).",
            mutates: true,
            schema: || obj(json!({
                "palette": {"type": "string"},
                "use_samples": {"type": "boolean"},
                "roles": {"type": "array", "items": {"type": "string"}},
                "force": {"type": "boolean"},
                "level_match": {"type": "boolean", "description": "default true"},
                "mix": {"type": "boolean", "description": "also apply the palette's mix moves: EQ carve (low cuts, 300 Hz dip on beds when a bass plays) and per-palette moves (drill: shorter 808 holds, kick ducking, brighter hats/snare). Default true"}
            }), &["palette"]),
            run: |e, a| {
                let name = s_req(a, "palette")?;
                palette::apply(e, &name, &ApplyOpts {
                    use_samples: b_or(a, "use_samples", false),
                    roles: strings(a, "roles"),
                    force: b_or(a, "force", false),
                    level_match: b_or(a, "level_match", true),
                    mix: b_or(a, "mix", true),
                })
            },
        },
        Tool {
            name: "install_palette_samples",
            description: "Fetch the curated CC0 one-shots (Sonic Pi's freesound CC0 set + Michael Fischer's real TR-808 set, pinned commits, SHA-256 verified) and index them into the sample library with role and character tags, so find_samples text:'dusty kick' finds them. Filter by ids, role or tag.",
            mutates: false,
            schema: || obj(json!({
                "ids": {"type": "array", "items": {"type": "string"}},
                "role": {"type": "string"},
                "tag": {"type": "string"}
            }), &[]),
            run: |e, a| {
                palette::install_samples(e, &strings(a, "ids"), s_opt(a, "role").as_deref(), s_opt(a, "tag").as_deref())
            },
        },
        Tool {
            name: "audition_palette",
            description: "Render one one-shot per role of a palette to WAVs (out_dir, default renders/palette_<name>) with peak/RMS, to hear or analyze a palette before applying it.",
            mutates: false,
            schema: || obj(json!({"palette": {"type": "string"}, "out_dir": {"type": "string"}}), &["palette"]),
            run: oneshots,
        },
    ]
}
