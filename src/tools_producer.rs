//! Producer + sample-library tools: produce_track (one call, plan -> compose
//! -> mix -> critique -> revise loop -> deliver), the same steps as separate
//! tools for step-by-step steering, genre playbooks, CC0/PD kits, the local
//! sample index and the flip workflows.

use crate::producer::{self, PlanArgs, ProduceOpts};
use crate::sample_lib;
use crate::tools::{b_or, f_opt, obj, s_opt, s_req, seed_of, u_or, Tool};
use anyhow::anyhow;
use serde_json::{json, Value};

fn plan_props() -> Value {
    json!({
        "brief": {"type": "string", "description": "What you want, in words: 'dark desi hip-hop with sitar and tabla, 92 bpm, for a hard verse'"},
        "genre": {"type": "string", "description": "A playbook (list_genres): trap, melodic_rap, drill, boom_bap, desi_hiphop, lofi, rnb, phonk, afrobeats. Default: guessed from the brief"},
        "bpm": {"type": "number"},
        "key": {"type": "string", "description": "Tonic, e.g. 'F#'"},
        "scale": {"type": "string", "description": "e.g. minor, harmonic_minor, bhairav, phrygian_dominant"},
        "mood": {"type": "string", "description": "dark, sad, hype, chill, jazzy, hopeful, devotional, smooth (default: read from the brief)"},
        "duration_s": {"type": "number", "description": "Approximate song length; sections are dropped/added to fit"},
        "seed": {"type": "integer", "description": "Omit for a fresh random seed (OS entropy + time, recorded in the plan). Give one to reproduce a beat exactly (same seed + args + generator version = same song)"},
        "intent": {"type": "object", "description": "Structured creative intent - fill this in rather than relying on keywords in the brief. The plan's 'constraints' lists what was applied, adjusted or ignored.", "properties": {
            "mood": {"type": "string", "description": "dark, sad, hype, chill, jazzy, hopeful, devotional, smooth (others are kept but use the genre's default harmony)"},
            "energy": {"type": "number", "description": "0..1"},
            "emotion": {"type": "string", "description": "The emotional intent in a few words ('cold confidence', 'bittersweet memory')"},
            "hero": {"type": "string", "enum": ["motif", "groove", "bass", "texture"], "description": "What carries the beat"},
            "density": {"type": "string", "enum": ["sparse", "balanced", "dense"]},
            "rhythmic_feel": {"type": "array", "items": {"type": "string", "enum": ["half_time", "backbeat", "triplet", "swing", "straight", "bounce", "driving", "rolling", "sparse_hats"]}},
            "motif": {"type": "object", "properties": {"contour": {"type": "string", "enum": ["arch", "descending", "ascending", "wave", "static", "leap_fall"]}, "rhythm": {"type": "string", "enum": ["on_grid", "syncopated", "long_short", "triplet"]}, "density": {"type": "number", "description": "0..1 notes in the motif"}}},
            "palette": {"type": "object", "description": "{role: preset}, roles lead, harmony, counter, texture, bass, kick, snare, hat, open_hat, perc, tabla, bayan; presets from get_guide"},
            "contrasts": {"type": "array", "items": {"type": "string", "enum": ["half_time_hook", "silence_before_drop", "beat_switch", "odd_phrase", "drum_dropout", "unusual_instrument", "key_change", "bass_kick_call_response", "sparse_to_dense"]}, "description": "Purposeful contrasts to place (replace the random wildcards)"}
        }},
        "method": {"type": "string", "description": "Force a generation method: template (the genre's patterns, varied), procedural (generated from the genre's distributions), reference_guided (needs reference_path). Default: drawn per beat"},
        "authored": {"type": "object", "description": "AI-authored MIDI: {section name|kind|'*': {role: [{start, len, pitch, vel}]}} played verbatim (see author_midi)"},
        "use_samples": {"type": "boolean", "description": "Use the genre's real public-domain drum kit (downloaded once, cached). Default true; falls back to synth drums offline"},
    })
}

fn plan_args(a: &Value) -> PlanArgs {
    PlanArgs {
        brief: s_opt(a, "brief").unwrap_or_default(),
        genre: s_opt(a, "genre"),
        bpm: f_opt(a, "bpm"),
        key: s_opt(a, "key"),
        scale: s_opt(a, "scale"),
        mood: s_opt(a, "mood"),
        duration_s: f_opt(a, "duration_s"),
        seed: if a.get("seed").map(|v| !v.is_null()).unwrap_or(false) {
            seed_of(a)
        } else {
            crate::creative::fresh_seed()
        },
        seed_source: if a.get("seed").map(|v| !v.is_null()).unwrap_or(false)
            && !crate::tools::seed_was_fresh()
        {
            "explicit".into()
        } else {
            "entropy".into()
        },
        use_samples: b_or(a, "use_samples", true),
        flip_sample: None,
        method: s_opt(a, "method"),
        reference: None,
        authored: parse_authored(&a["authored"]),
        intent: a["intent"].clone(),
    }
}

fn parse_authored(
    v: &Value,
) -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, Vec<crate::project::Note>>>
{
    serde_json::from_value(v.clone()).unwrap_or_default()
}

fn merge(mut a: Value, b: Value) -> Value {
    if let (Value::Object(x), Value::Object(y)) = (&mut a, b) {
        x.extend(y);
    }
    a
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "produce_track",
            description: "THE PRODUCER. One call from a brief to a finished, mixed, mastered beat. Decisions are hierarchical: a creative direction (mood, energy, intent, hero element) chooses complementary core material (method: template/procedural/reference-guided/AI-authored; tempo, mode, key, progressions with modal/borrowed colour, a shaped motif, a generated drum groove, 808 behaviour, palette, arrangement form), then controlled variation (per-section groove variation and fills, motif development, counter lines, 1-3 purposeful wildcards). Fresh seed per call unless one is given; every decision and its reason is in the plan, with provenance. Near-repeats of recent beats are regenerated (novelty check). The critic is a technical guardrail only. Plans, composes (drum grammar with hat rolls / ghost notes / 808 glides / tabla tihais, bass locked to the kick, voiced chords, lead from the motif with call & response and meend/glide ornaments, counter-melody in hooks), picks sounds (real PD/CC0 kit + presets), builds buses/sends/sidechain, balances to genre targets, masters, then LISTENS (check_master, detect_artifacts, analyze_mix, analyze_sections, detect_key, compare_to_reference) and fixes technical failures only (loudness, peaks, clicks, masking/mud, a dead hook) - never the musical content. Keeps the best iteration in the project. out_dir writes <genre>_<seed>.mp3/.json/.plan.json. Returns per-iteration scores, the critic's findings, the producer's reasoning and the plan.",
            mutates: true,
            schema: || obj(merge(plan_props(), json!({
                "reference_path": {"type": "string", "description": "Reference track: its tonal balance/dynamics join the score and the critic"},
                "max_iterations": {"type": "integer", "description": "Critique/revise rounds (default 4)"},
                "out_dir": {"type": "string", "description": "Write the master (mp3 when ffmpeg exists, else wav), project and plan here"},
                "mp3": {"type": "boolean", "description": "default true"},
                "novelty": {"type": "boolean", "description": "Check the beat against recent outputs and regenerate near-repeats (default true; with an explicit seed it only reports)"},
                "novelty_threshold": {"type": "number", "description": "Minimum distance to recent beats (default 0.25)"},
                "history_path": {"type": "string", "description": "Novelty history file (default beatbox_history/fingerprints.jsonl)"},
            })), &[]),
            run: |e, a| {
                let mut args = plan_args(a);
                args.reference = s_opt(a, "reference_path").map(|p| e.resolve(&p).to_string_lossy().to_string());
                if args.brief.is_empty() && args.genre.is_none() {
                    return Err(anyhow!("give a brief or a genre (list_genres)"));
                }
                let o = ProduceOpts {
                    max_iterations: u_or(a, "max_iterations", 4).clamp(1, 10) as usize,
                    reference: s_opt(a, "reference_path").map(|p| e.resolve(&p).to_string_lossy().to_string()),
                    out_dir: s_opt(a, "out_dir").map(|p| e.resolve(&p)),
                    mp3: b_or(a, "mp3", true),
                    novelty: {
                        let d = crate::novelty::NoveltyOpts::default();
                        Some(crate::novelty::NoveltyOpts {
                            enabled: b_or(a, "novelty", true),
                            threshold: a["novelty_threshold"].as_f64().map(|t| t as f32).unwrap_or(d.threshold),
                            history: s_opt(a, "history_path").map(|p| e.resolve(&p)).or(d.history.clone()),
                            ..d
                        })
                    },
                };
                producer::produce(e, &args, &o)
            },
        },
        Tool {
            name: "plan_track",
            description: "Step 1 of producing by hand: write a structured plan (genre, mood, bpm, key/scale, verse + hook progressions, the motif, palette, drum grammar choices, sections with energy/layers/lead treatment/transitions, mix knobs) and the producer's reasoning. Nothing is written to the project. Edit the plan JSON if you like, then apply_plan.",
            mutates: false,
            schema: || obj(plan_props(), &[]),
            run: |_, a| {
                let p = producer::plan_track(&plan_args(a))?;
                let applied: Vec<&crate::creative::Constraint> = p.constraints.iter().filter(|c| c.status != "ignored").collect();
                let ignored: Vec<&crate::creative::Constraint> = p.constraints.iter().filter(|c| c.status == "ignored").collect();
                Ok(json!({"seed": p.seed, "constraints": {"applied": applied, "ignored": ignored}, "plan": p, "next": "apply_plan {plan} then critique_track"}))
            },
        },
        Tool {
            name: "apply_plan",
            description: "Step 2: build the project from a plan (replaces the current project, undoable): tracks, one pattern per section, arrangement, buses/sends/sidechain/EQ carving; then balance_mix + master_assistant unless mix=false.",
            mutates: true,
            schema: || obj(json!({"plan": {"type": "object"}, "mix": {"type": "boolean", "description": "default true"}}), &["plan"]),
            run: |e, a| {
                let mut plan = producer::plan_from_value(&a["plan"])?;
                if let Some(k) = plan.sample_kit.clone() {
                    if sample_lib::install_kit(e, &k).is_err() {
                        plan.sample_kit = None;
                    }
                }
                if b_or(a, "mix", true) {
                    producer::apply_plan(e, &plan)
                } else {
                    producer::compose(e, &plan)
                }
            },
        },
        Tool {
            name: "critique_track",
            description: "Step 3: diagnostics, not a quality rating. Listen to the current project and report heuristic numbers: technical (delivery QC, artifacts, mix analyzer) and musical (hook lift vs verse, energy curve vs the plan, motif repetition across section kinds, hook vs verse melodic density, key clarity, length), plus reference match when given. Every finding carries the concrete fix revise_track will apply. Pass the plan for plan-aware checks.",
            mutates: false,
            schema: || obj(json!({"plan": {"type": "object"}, "reference_path": {"type": "string"}}), &["plan"]),
            run: |e, a| {
                let plan = producer::plan_from_value(&a["plan"])?;
                let r = s_opt(a, "reference_path").map(|p| e.resolve(&p).to_string_lossy().to_string());
                let c = producer::critique(e, &plan, r.as_deref())?;
                Ok(serde_json::to_value(c)?)
            },
        },
        Tool {
            name: "revise_track",
            description: "Step 4: apply a critique's fixes to the plan (contrast/hook lift, motif variation, lead density, sidechain/low end, per-track level offsets, remaster target, tonic anchor) and rebuild + remix the project from the revised plan. Returns the revised plan and what changed; loop critique_track -> revise_track until the score stops improving.",
            mutates: true,
            schema: || obj(json!({"plan": {"type": "object"}, "critique": {"type": "object"}, "max_fixes": {"type": "integer", "description": "default 3"}}), &["plan", "critique"]),
            run: |e, a| {
                let mut plan = producer::plan_from_value(&a["plan"])?;
                let c: producer::Critique = serde_json::from_value(a["critique"].clone()).map_err(|err| anyhow!("invalid critique: {err}"))?;
                let changes = producer::revise(&mut plan, &c, u_or(a, "max_fixes", 3) as usize);
                if changes.is_empty() {
                    return Ok(json!({"changes": [], "note": "nothing actionable left: the critique's findings have no automatic fix", "plan": plan}));
                }
                let r = producer::apply_plan(e, &plan)?;
                Ok(json!({"changes": changes, "plan": plan, "result": r}))
            },
        },
        Tool {
            name: "list_genres",
            description: "Genre playbooks the producer knows (data, not code): name, aliases, BPM range, scales and a one-line description.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |_, _| {
                Ok(json!(producer::playbooks().iter().map(|p| json!({"name": p.name, "aliases": p.aliases, "bpm": p.bpm, "scales": p.scales, "description": p.description})).collect::<Vec<_>>()))
            },
        },
        Tool {
            name: "describe_genre",
            description: "The full playbook of a genre: tempo range, swing, scales, progressions by mood, drum grammar (kick/snare/hat/perc/tabla step patterns, ghost notes, hat rolls, half-time), bass/harmony/lead/counter rules and sounds, arrangement template with energy and layers, and mix targets.",
            mutates: false,
            schema: || obj(json!({"genre": {"type": "string"}}), &["genre"]),
            run: |_, a| {
                let g = s_req(a, "genre")?;
                let p = producer::playbook(&g).or_else(|_| producer::guess_genre(&g).ok_or_else(|| anyhow!("unknown genre '{g}'")))?;
                Ok(producer::describe_playbook(p))
            },
        },
        // ----- sample library
        Tool {
            name: "install_kit",
            description: "Download a real public-domain drum-machine kit (cached under beatbox_samples/kits, licence file kept): tr808 (trap/drill/R&B), lm2 (LinnDrum, boom-bap), rz1 (lo-fi). register=true also imports every hit into the project. list=true shows the kits.",
            mutates: true,
            schema: || obj(json!({"kit": {"type": "string"}, "register": {"type": "boolean"}, "list": {"type": "boolean"}}), &[]),
            run: |e, a| {
                if b_or(a, "list", false) || s_opt(a, "kit").is_none() {
                    return Ok(json!(sample_lib::KITS.iter().map(|k| json!({"kit": k.name, "description": k.description, "license": k.license, "hits": k.files.iter().map(|f| f.0).collect::<Vec<_>>()})).collect::<Vec<_>>()));
                }
                let k = s_req(a, "kit")?;
                let files = sample_lib::install_kit(e, &k)?;
                let kit = sample_lib::kit(&k)?;
                let mut names = Vec::new();
                if b_or(a, "register", false) {
                    for (role, path) in &files {
                        let v = crate::tools::register_sample(e, crate::samples::SampleInfo { name: format!("{}_{role}", kit.name), path: path.to_string_lossy().into(), source: kit.source.into(), license: kit.license.into(), author: String::new(), duration: 0.0 })?;
                        names.push(v["sample"].clone());
                    }
                }
                Ok(json!({"kit": kit.name, "files": files.iter().map(|(r, p)| json!({"hit": r, "path": p.to_string_lossy()})).collect::<Vec<_>>(), "registered": names, "license": kit.license}))
            },
        },
        Tool {
            name: "index_samples",
            description: "Scan folders of audio (wav/flac/mp3/ogg/aiff) into the sample index: role (kick/snare/clap/hat/cymbal/perc/bass/vocal/loop/melodic/pad/fx, from the name or from length + spectrum), YIN pitch and note, BPM for loops, onset count, brightness, level and licence. Default dirs: the project's sample folder (kits, downloads, flips).",
            mutates: false,
            schema: || obj(json!({"dirs": {"type": "array", "items": {"type": "string"}}, "max_files": {"type": "integer", "description": "default 2000"}}), &[]),
            run: |e, a| {
                let dirs: Vec<std::path::PathBuf> = match a.get("dirs").and_then(|v| v.as_array()) {
                    Some(v) => v.iter().filter_map(|x| x.as_str()).map(|s| e.resolve(s)).collect(),
                    None => vec![e.samples_dir()],
                };
                sample_lib::index_dirs(e, &dirs, u_or(a, "max_files", 2000) as usize)
            },
        },
        Tool {
            name: "find_samples",
            description: "Search the local sample index (index_samples first): by role, words in the name/path, key fit (pitched samples in the key score higher), BPM (loops near the tempo or double/half), max duration. Defaults key/bpm to the project's when match_project=true.",
            mutates: false,
            schema: || obj(json!({"role": {"type": "string"}, "query": {"type": "string"}, "key": {"type": "string", "description": "'F# minor'"}, "bpm": {"type": "number"}, "max_duration": {"type": "number"}, "match_project": {"type": "boolean"}, "limit": {"type": "integer", "description": "default 10"}}), &[]),
            run: |e, a| {
                let mp = b_or(a, "match_project", false);
                let q = sample_lib::Query {
                    role: s_opt(a, "role"),
                    text: s_opt(a, "query"),
                    key: s_opt(a, "key").or_else(|| mp.then(|| format!("{} {}", e.project.key_root, e.project.scale))),
                    bpm: f_opt(a, "bpm").or_else(|| mp.then_some(e.project.bpm)),
                    max_duration: f_opt(a, "max_duration"),
                    limit: u_or(a, "limit", 10) as usize,
                };
                let r = sample_lib::find(e, &q);
                if r.is_empty() {
                    return Ok(json!({"results": [], "hint": "index is empty or nothing matched: run install_kit / download_sample then index_samples"}));
                }
                Ok(json!({"results": r.iter().map(|(s, x)| json!({"score": (s * 100.0).round() / 100.0, "path": x.path, "role": x.role, "duration": x.duration, "note": x.note, "bpm": x.bpm, "license": x.license})).collect::<Vec<_>>(), "next": "import_sample {path} then add_sample_track / flip_sample"}))
            },
        },
        Tool {
            name: "flip_sample",
            description: "Flip a sample like a producer: detect its pitch (YIN) and re-pitch it onto a stable note of the project key (sampler-style, speed follows pitch), chop it on transients into a slice kit, and program a fresh 2-bar phrase from the chops (slice 0 on the downbeat, call & response, varied repeats) across the pattern. Credits travel with the new sample.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "track": {"type": "string", "description": "default 'flip'"}, "pattern": {"type": "string"}, "fit_key": {"type": "boolean", "description": "default true"}, "max_slices": {"type": "integer", "description": "default 12"}, "density": {"type": "number", "description": "0..1 chop density (default 0.6)"}, "seed": {"type": "integer"}}), &["sample"]),
            run: |e, a| {
                sample_lib::flip(e, &sample_lib::FlipOpts {
                    sample: s_req(a, "sample")?,
                    track: s_opt(a, "track").unwrap_or_else(|| "flip".into()),
                    pattern: s_opt(a, "pattern"),
                    fit_key: b_or(a, "fit_key", true),
                    max_slices: u_or(a, "max_slices", 12).clamp(2, 32) as usize,
                    seed: seed_of(a),
                    density: crate::tools::f_or(a, "density", 0.6).clamp(0.1, 1.0),
                })
            },
        },
        Tool {
            name: "vocal_chop",
            description: "Turn a vocal (or any phrase) into a playable chop instrument: picks the strongest short syllables, renders each at root/3rd/5th/6th/octave of the project key (de-clicked), maps them as a slice kit (note 48 + syllable*5 + degree) and writes a call-and-response chop melody into the pattern.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "track": {"type": "string", "description": "default 'chops'"}, "pattern": {"type": "string"}, "chops": {"type": "integer", "description": "syllables to keep (default 4, max 6)"}, "seed": {"type": "integer"}}), &["sample"]),
            run: |e, a| {
                sample_lib::vocal_chop(e, &sample_lib::ChopOpts {
                    sample: s_req(a, "sample")?,
                    track: s_opt(a, "track").unwrap_or_else(|| "chops".into()),
                    pattern: s_opt(a, "pattern"),
                    chops: u_or(a, "chops", 4) as usize,
                    seed: seed_of(a),
                })
            },
        },
    ]
}
