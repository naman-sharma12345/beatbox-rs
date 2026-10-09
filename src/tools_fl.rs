//! FL Studio parity, as MCP tools: a playlist of reusable pattern clips,
//! reusable automation clips, mixer insert routing (bus -> bus), scale
//! snapping and ghost-note generation in the piano roll.

use crate::automation::{AutoPoint, AutomationLane};
use crate::dsp::Rng;
use crate::project::{AutomationClip, Note, Pattern, PlaylistClip, Project, Section};
use crate::theory;
use crate::tools::{f_opt, obj, s_opt, s_req, u_or, Tool};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// Prefix of the patterns the playlist compiler generates.
pub const PL: &str = "pl:";

/// Compile the playlist into the arrangement: the timeline is cut at every
/// clip boundary and each slice becomes one generated pattern holding the
/// notes of every clip active in it (looped when a clip is longer than its
/// pattern). Source patterns are never modified, so a clip stays reusable.
pub fn compile_playlist(p: &mut Project) -> Result<Vec<String>> {
    p.patterns.retain(|x| !x.name.starts_with(PL));
    if p.playlist.is_empty() {
        p.arrangement.retain(|s| !s.pattern.starts_with(PL));
        return Ok(Vec::new());
    }
    for c in &p.playlist {
        p.pattern_index(&c.pattern)?;
    }
    let mut cuts: BTreeSet<u32> = BTreeSet::new();
    cuts.insert(0);
    for c in &p.playlist {
        cuts.insert(c.start_bar);
        cuts.insert(c.start_bar + c.bars.max(1));
    }
    let cuts: Vec<u32> = cuts.into_iter().collect();
    let mut made = Vec::new();
    let mut arrangement = Vec::new();
    for w in cuts.windows(2) {
        let (b0, b1) = (w[0], w[1]);
        let active: Vec<&PlaylistClip> = p
            .playlist
            .iter()
            .filter(|c| c.start_bar <= b0 && c.start_bar + c.bars.max(1) >= b1)
            .collect();
        let label = if active.is_empty() {
            "gap".to_string()
        } else {
            active
                .iter()
                .map(|c| c.pattern.as_str())
                .collect::<Vec<_>>()
                .join("+")
        };
        let mut pat = Pattern::new(&format!("{PL}{b0}:{label}"), b1 - b0);
        let len = ((b1 - b0) * crate::project::STEPS_PER_BAR) as f32;
        for c in &active {
            let src = &p.patterns[p.pattern_index(&c.pattern)?];
            let lp = src.steps().max(1) as f32;
            let off = ((b0 - c.start_bar) * crate::project::STEPS_PER_BAR) as f32;
            for (track, notes) in &src.clips {
                if !c.tracks.is_empty() && !c.tracks.iter().any(|t| t.to_lowercase() == *track) {
                    continue;
                }
                let dst = pat.clips.entry(track.clone()).or_default();
                for n in notes {
                    let mut k = (off / lp).floor();
                    loop {
                        let t = n.start + k * lp;
                        if t >= off + len {
                            break;
                        }
                        if t >= off {
                            let mut m = n.clone();
                            m.start = t - off;
                            m.len = m.len.min(off + len - t).max(0.01);
                            dst.push(m);
                        }
                        k += 1.0;
                    }
                }
            }
        }
        pat.clips.retain(|_, v| !v.is_empty());
        made.push(pat.name.clone());
        arrangement.push(Section {
            pattern: pat.name.clone(),
            repeats: 1,
        });
        p.patterns.push(pat);
    }
    p.arrangement = arrangement;
    Ok(made)
}

fn playlist_json(p: &Project) -> Value {
    let end = p
        .playlist
        .iter()
        .map(|c| c.start_bar + c.bars)
        .max()
        .unwrap_or(0);
    json!({
        "clips": p.playlist.iter().enumerate().map(|(i, c)| json!({"id": i, "pattern": c.pattern, "start_bar": c.start_bar, "bars": c.bars, "end_bar": c.start_bar + c.bars, "lane": c.lane, "tracks": c.tracks})).collect::<Vec<_>>(),
        "song_bars": end,
        "compiled_sections": p.arrangement.iter().map(|s| s.pattern.clone()).collect::<Vec<_>>(),
    })
}

/// Write an automation clip's points into its lane at `at` (song beats),
/// replacing what the lane had inside the clip's span.
fn stamp(p: &mut Project, clip: &AutomationClip, at: f32) {
    let lane = match p
        .automation
        .iter()
        .position(|l| l.is_target(&clip.target, &clip.param))
    {
        Some(i) => &mut p.automation[i],
        None => {
            p.automation
                .push(AutomationLane::new(&clip.target, &clip.param));
            p.automation.last_mut().unwrap()
        }
    };
    let end = at + clip.length_beats;
    lane.points
        .retain(|x| x.beat < at - 1e-4 || x.beat > end + 1e-4);
    for pt in &clip.points {
        lane.points.push(AutoPoint {
            beat: at + pt.beat.clamp(0.0, clip.length_beats),
            value: pt.value,
            curve: pt.curve,
        });
    }
    lane.sort();
}

fn parse_points(v: &Value) -> Result<Vec<AutoPoint>> {
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow!("points must be an array of {{beat, value, curve?}}"))?;
    let mut out = Vec::new();
    for x in arr {
        let pt: AutoPoint = if let Some(a) = x.as_array() {
            serde_json::from_value(json!({"beat": a.first(), "value": a.get(1)}))?
        } else {
            serde_json::from_value(x.clone())?
        };
        out.push(pt);
    }
    if out.is_empty() {
        bail!("an automation clip needs at least one point");
    }
    Ok(out)
}

/// Snap a pitch into the scale (nearest; ties go down unless `up`).
pub fn snap_pitch(pitch: u8, root_pc: u8, iv: &[u8], mode: &str) -> u8 {
    let in_scale = |p: i32| iv.contains(&(((p - root_pc as i32).rem_euclid(12)) as u8));
    let p = pitch as i32;
    if in_scale(p) {
        return pitch;
    }
    for d in 1..12 {
        let (dn, upn) = (p - d, p + d);
        match mode {
            "up" if in_scale(upn) => return upn.clamp(0, 127) as u8,
            "down" if in_scale(dn) => return dn.clamp(0, 127) as u8,
            "up" | "down" => {}
            _ => {
                if in_scale(dn) {
                    return dn.clamp(0, 127) as u8;
                }
                if in_scale(upn) {
                    return upn.clamp(0, 127) as u8;
                }
            }
        }
    }
    pitch
}

fn patterns_of(p: &Project, a: &Value) -> Result<Vec<usize>> {
    match s_opt(a, "pattern") {
        Some(n) if n == "all" => Ok((0..p.patterns.len()).collect()),
        Some(n) => Ok(vec![p.pattern_index(&n)?]),
        None => Ok((0..p.patterns.len())
            .filter(|i| !p.patterns[*i].name.starts_with(PL))
            .collect()),
    }
}

/// Ghost notes: quiet hits on the weak 16ths around the backbeat, never
/// within a 16th of an existing hit.
pub fn ghost_notes(
    existing: &[Note],
    bars: u32,
    density: f32,
    vel: (f32, f32),
    pitch: u8,
    slots: &[u32],
    rng: &mut Rng,
) -> Vec<Note> {
    let mut out = Vec::new();
    for bar in 0..bars {
        for s in slots {
            let t = bar * 16 + s;
            let busy = existing
                .iter()
                .chain(out.iter())
                .any(|n: &Note| (n.start - t as f32).abs() < 1.0);
            if !busy && rng.chance(density.clamp(0.0, 1.0)) {
                out.push(Note::new(t as f32, 0.5, pitch, rng.range(vel.0, vel.1)));
            }
        }
    }
    out
}

pub fn tools() -> Vec<Tool> {
    vec![
        // ----- playlist (pattern clips) -----
        Tool {
            name: "place_pattern",
            description: "FL-style playlist: place a pattern as a clip at a bar, for `bars` bars (loops the pattern when longer), optionally only some of its tracks (e.g. just the hats of 'hook' over a verse). Clips can overlap on any lane; the playlist is compiled into the song (generated 'pl:' sections) so render, ears and export follow it. Source patterns stay untouched and reusable.",
            mutates: true,
            schema: || obj(json!({
                "pattern": {"type": "string"},
                "start_bar": {"type": "integer", "description": "0-based bar"},
                "bars": {"type": "integer", "description": "clip length (default: the pattern's length)"},
                "tracks": {"type": "array", "items": {"type": "string"}, "description": "only these tracks of the pattern"},
                "lane": {"type": "integer"},
                "repeat": {"type": "integer", "description": "place N copies back to back (default 1)"},
            }), &["pattern", "start_bar"]),
            run: |e, a| {
                let name = s_req(a, "pattern")?;
                let pi = e.project.pattern_index(&name)?;
                if e.project.patterns[pi].name.starts_with(PL) {
                    bail!("'{name}' is a generated playlist section; place its source pattern instead");
                }
                let pname = e.project.patterns[pi].name.clone();
                let bars = u_or(a, "bars", e.project.patterns[pi].bars as u64).clamp(1, 512) as u32;
                let start = u_or(a, "start_bar", 0) as u32;
                let tracks: Vec<String> = a.get("tracks").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                for t in &tracks {
                    e.project.track_index(t)?;
                }
                if e.project.playlist.is_empty() && !e.project.arrangement.is_empty() {
                    bail!("the song uses the section arrangement; call arrangement_to_playlist first (or clear_playlist to stay with sections)");
                }
                for k in 0..u_or(a, "repeat", 1).clamp(1, 64) as u32 {
                    e.project.playlist.push(PlaylistClip { pattern: pname.clone(), start_bar: start + k * bars, bars, tracks: tracks.clone(), lane: u_or(a, "lane", 0) as u32 });
                }
                compile_playlist(&mut e.project)?;
                Ok(playlist_json(&e.project))
            },
        },
        Tool {
            name: "arrangement_to_playlist",
            description: "Convert the section arrangement (set_arrangement / build_structure) into playlist clips, one per section on lane 0, so you can then layer extra clips (place_pattern) over it.",
            mutates: true,
            schema: || obj(json!({}), &[]),
            run: |e, _| {
                let p = &mut e.project;
                p.playlist.clear();
                let mut bar = 0;
                for s in p.song_sections() {
                    if s.pattern.starts_with(PL) {
                        continue;
                    }
                    let bars = p.patterns[p.pattern_index(&s.pattern)?].bars * s.repeats.max(1);
                    p.playlist.push(PlaylistClip { pattern: s.pattern.clone(), start_bar: bar, bars, tracks: Vec::new(), lane: 0 });
                    bar += bars;
                }
                compile_playlist(p)?;
                Ok(playlist_json(p))
            },
        },
        Tool {
            name: "list_playlist",
            description: "Show the playlist: every pattern clip (id, pattern, start bar, length, lane, track filter) and the compiled song sections.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |e, _| Ok(playlist_json(&e.project)),
        },
        Tool {
            name: "remove_clip",
            description: "Remove playlist clips by id (see list_playlist), or every clip of a pattern / every clip starting at a bar.",
            mutates: true,
            schema: || obj(json!({"id": {"type": "integer"}, "pattern": {"type": "string"}, "start_bar": {"type": "integer"}}), &[]),
            run: |e, a| {
                let before = e.project.playlist.len();
                if let Some(id) = a.get("id").and_then(|v| v.as_u64()) {
                    if (id as usize) >= before {
                        bail!("no clip {id} ({before} clips)");
                    }
                    e.project.playlist.remove(id as usize);
                } else {
                    let pat = s_opt(a, "pattern");
                    let bar = a.get("start_bar").and_then(|v| v.as_u64()).map(|x| x as u32);
                    if pat.is_none() && bar.is_none() {
                        bail!("give id, pattern or start_bar");
                    }
                    e.project.playlist.retain(|c| !(pat.as_ref().map(|p| *p == c.pattern).unwrap_or(true) && bar.map(|b| b == c.start_bar).unwrap_or(true)));
                }
                let removed = before - e.project.playlist.len();
                compile_playlist(&mut e.project)?;
                let mut v = playlist_json(&e.project);
                v["removed"] = json!(removed);
                Ok(v)
            },
        },
        Tool {
            name: "clear_playlist",
            description: "Drop every playlist clip and the generated sections (back to the plain section arrangement, which is emptied).",
            mutates: true,
            schema: || obj(json!({}), &[]),
            run: |e, _| {
                e.project.playlist.clear();
                compile_playlist(&mut e.project)?;
                Ok(json!({"cleared": true, "arrangement": e.project.arrangement.len()}))
            },
        },
        // ----- automation clips -----
        Tool {
            name: "create_automation_clip",
            description: "Create a reusable automation clip (FL automation clip): a shape for one parameter (volume, pan, instrument.<param>, fx.<index|id>.<param>) on a track/bus/master, with points in beats from the clip start. Place it anywhere with place_automation_clip; editing the clip and re-placing restamps every placement.",
            mutates: true,
            schema: || obj(json!({
                "name": {"type": "string"},
                "track": {"type": "string", "description": "track, bus or master"},
                "param": {"type": "string"},
                "length_beats": {"type": "number"},
                "points": {"type": "array", "items": {}, "description": "[{beat, value, curve?}] relative to the clip start (curve linear|step|smooth)"},
            }), &["name", "track", "param", "points"]),
            run: |e, a| {
                let name = s_req(a, "name")?;
                let target = s_req(a, "track")?;
                if target != "master" && e.project.track_index(&target).is_err() && e.project.bus_index(&target).is_err() {
                    bail!("no track or bus '{target}'");
                }
                let points = parse_points(a.get("points").unwrap_or(&Value::Null))?;
                let len = f_opt(a, "length_beats").unwrap_or_else(|| points.iter().map(|p| p.beat).fold(0.0, f32::max)).max(0.25);
                let param = s_req(a, "param")?;
                let old = e.project.automation_clips.iter().position(|c| c.name == name);
                let placements = old.map(|i| e.project.automation_clips[i].placements.clone()).unwrap_or_default();
                let clip = AutomationClip { name: name.clone(), target, param, length_beats: len, points, placements: placements.clone() };
                for at in &placements {
                    stamp(&mut e.project, &clip, *at);
                }
                match old {
                    Some(i) => e.project.automation_clips[i] = clip,
                    None => e.project.automation_clips.push(clip),
                }
                Ok(json!({"clip": name, "length_beats": len, "restamped": placements.len()}))
            },
        },
        Tool {
            name: "place_automation_clip",
            description: "Place an automation clip at song positions (beats, or bars with at_bar). Writes the clip's shape into the parameter's lane over the clip span (replacing what was there) and remembers the placement.",
            mutates: true,
            schema: || obj(json!({
                "name": {"type": "string"},
                "at_beat": {"type": "number"},
                "at_bar": {"type": "number"},
                "at_section": {"type": "string", "description": "start of every occurrence of this section (pattern name)"},
                "every_beats": {"type": "number", "description": "with count: repeat placement"},
                "count": {"type": "integer"},
            }), &["name"]),
            run: |e, a| {
                let name = s_req(a, "name")?;
                let ci = e.project.automation_clips.iter().position(|c| c.name == name).ok_or_else(|| anyhow!("no automation clip '{name}'"))?;
                let mut at: Vec<f32> = Vec::new();
                if let Some(s) = s_opt(a, "at_section") {
                    at.extend(crate::ears::spans(&e.project).iter().filter(|x| x.pattern == s).map(|x| x.start_beat));
                    if at.is_empty() {
                        bail!("section '{s}' is not in the song");
                    }
                } else {
                    let b = f_opt(a, "at_beat").or_else(|| f_opt(a, "at_bar").map(|x| x * 4.0)).ok_or_else(|| anyhow!("give at_beat, at_bar or at_section"))?;
                    let every = f_opt(a, "every_beats").unwrap_or(0.0);
                    for k in 0..u_or(a, "count", 1).clamp(1, 256) {
                        at.push(b + k as f32 * every);
                    }
                }
                let clip = e.project.automation_clips[ci].clone();
                for x in &at {
                    stamp(&mut e.project, &clip, *x);
                }
                let c = &mut e.project.automation_clips[ci];
                c.placements.extend(at.iter().copied());
                c.placements.sort_by(|a, b| a.total_cmp(b));
                c.placements.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
                Ok(json!({"clip": name, "placed_at_beats": at, "placements": c.placements}))
            },
        },
        Tool {
            name: "list_automation_clips",
            description: "List automation clips with their target, length, points and placements.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |e, _| Ok(json!({"clips": e.project.automation_clips})),
        },
        // ----- mixer insert routing -----
        Tool {
            name: "route_bus",
            description: "Mixer insert routing: send a bus's output into another bus (e.g. 'drums' and 'bass' inserts into a 'beat' group with glue compression) or back to the master. Cycles are rejected. list_routing shows the result.",
            mutates: true,
            schema: || obj(json!({"bus": {"type": "string"}, "to": {"type": "string", "description": "another bus, or 'master'"}}), &["bus", "to"]),
            run: |e, a| {
                let b = s_req(a, "bus")?;
                let to = s_req(a, "to")?;
                let bi = e.project.bus_index(&b)?;
                if to == "master" {
                    e.project.buses[bi].output = None;
                } else {
                    let ti = e.project.bus_index(&to)?;
                    if ti == bi {
                        bail!("a bus cannot feed itself");
                    }
                    // follow the chain from the destination: reaching `bi` is a cycle
                    let mut cur = Some(ti);
                    let mut hops = 0;
                    while let Some(c) = cur {
                        if c == bi {
                            bail!("routing '{b}' into '{to}' would make a loop");
                        }
                        hops += 1;
                        if hops > 64 {
                            break;
                        }
                        cur = e.project.buses[c].output.as_deref().and_then(|o| e.project.bus_index(o).ok());
                    }
                    e.project.buses[bi].output = Some(e.project.buses[ti].name.clone());
                }
                Ok(json!({"buses": e.project.buses.iter().map(|x| json!({"bus": x.name, "output": x.output.clone().unwrap_or_else(|| "master".into())})).collect::<Vec<_>>(),
                    "processing_order": crate::render::bus_order(&e.project).iter().map(|i| e.project.buses[*i].name.clone()).collect::<Vec<_>>()}))
            },
        },
        // ----- piano roll -----
        Tool {
            name: "scale_snap",
            description: "Piano roll 'snap to scale': move every out-of-key note of a track (one pattern, or every pattern) into the project's key/scale (or a given key/scale). mode nearest (ties go down) | up | down. Reports what moved.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string"},
                "pattern": {"type": "string", "description": "pattern name, or 'all' (default: every non-generated pattern)"},
                "key": {"type": "string"}, "scale": {"type": "string"},
                "mode": {"type": "string", "enum": ["nearest", "up", "down"]},
            }), &["track"]),
            run: |e, a| {
                let track = s_req(a, "track")?;
                e.project.track_index(&track)?;
                let key = s_opt(a, "key").unwrap_or_else(|| e.project.key_root.clone());
                let scale = s_opt(a, "scale").unwrap_or_else(|| e.project.scale.clone());
                let root = theory::pitch_class(&key)?;
                let iv = theory::scale_intervals(&scale)?.to_vec();
                let mode = s_opt(a, "mode").unwrap_or_else(|| "nearest".into());
                let mut moved = Vec::new();
                let mut total = 0;
                for pi in patterns_of(&e.project, a)? {
                    let pname = e.project.patterns[pi].name.clone();
                    for n in e.project.patterns[pi].notes_mut(&track).iter_mut() {
                        total += 1;
                        let q = snap_pitch(n.pitch, root, &iv, &mode);
                        if q != n.pitch {
                            moved.push(json!({"pattern": pname, "start": n.start, "from": n.pitch, "to": q}));
                            n.pitch = q;
                            if let Some(s) = n.slide_to {
                                n.slide_to = Some(snap_pitch(s, root, &iv, &mode));
                            }
                        }
                    }
                }
                Ok(json!({"key": format!("{key} {scale}"), "notes": total, "moved": moved.len(), "changes": moved.into_iter().take(40).collect::<Vec<_>>()}))
            },
        },
        Tool {
            name: "generate_ghost_notes",
            description: "Add ghost notes (quiet in-between hits) to a drum track: snare ghosts around the backbeat by default, or your own 16th slots. density 0..1 = chance per free slot, velocity range default 0.18-0.32, never within a 16th of an existing hit. Seeded.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string", "description": "default snare"},
                "pattern": {"type": "string", "description": "pattern name or 'all' (default: every non-generated pattern)"},
                "density": {"type": "number", "description": "default 0.35"},
                "vel_min": {"type": "number"}, "vel_max": {"type": "number"},
                "slots": {"type": "array", "items": {"type": "integer"}, "description": "16th positions inside the bar (0-15); default [3,6,7,10,14,15]"},
                "pitch": {"type": "integer"},
                "seed": {"type": "integer"},
            }), &[]),
            run: |e, a| {
                let track = s_opt(a, "track").unwrap_or_else(|| "snare".into());
                e.project.track_index(&track)?;
                let slots: Vec<u32> = a.get("slots").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_u64().map(|y| (y % 16) as u32)).collect()).unwrap_or_else(|| vec![3, 6, 7, 10, 14, 15]);
                let density = f_opt(a, "density").unwrap_or(0.35);
                let vel = (f_opt(a, "vel_min").unwrap_or(0.18), f_opt(a, "vel_max").unwrap_or(0.32));
                let mut rng = Rng::new(u_or(a, "seed", 1));
                let mut added = Vec::new();
                for pi in patterns_of(&e.project, a)? {
                    let pat = &mut e.project.patterns[pi];
                    let bars = pat.bars;
                    let existing = pat.notes(&track).to_vec();
                    let pitch = a.get("pitch").and_then(|v| v.as_u64()).map(|x| x as u8).or_else(|| existing.first().map(|n| n.pitch)).unwrap_or(60);
                    let g = ghost_notes(&existing, bars, density, vel, pitch, &slots, &mut rng);
                    added.push(json!({"pattern": pat.name, "added": g.len()}));
                    let v = pat.notes_mut(&track);
                    v.extend(g);
                    v.sort_by(|x, y| x.start.total_cmp(&y.start));
                }
                Ok(json!({"track": track, "patterns": added}))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;

    fn eng() -> Engine {
        let d = std::env::temp_dir().join("beatbox_fl_tests");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("new_project", &json!({"name": "fl", "bpm": 120}))
            .unwrap();
        e
    }

    #[test]
    fn playlist_clips_overlap_and_loop() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "kick", "preset": "kick"}))
            .unwrap();
        e.call("add_track", &json!({"name": "hat", "preset": "hat"}))
            .unwrap();
        e.call("add_pattern", &json!({"name": "beat", "bars": 1}))
            .unwrap();
        e.call("add_notes", &json!({"pattern": "beat", "track": "kick", "notes": [{"start": 0, "len": 1, "pitch": 60}, {"start": 8, "len": 1, "pitch": 60}]})).unwrap();
        e.call("add_pattern", &json!({"name": "hats", "bars": 1}))
            .unwrap();
        e.call("add_notes", &json!({"pattern": "hats", "track": "hat", "notes": [{"start": 2, "len": 1, "pitch": 60}]})).unwrap();
        // kick loop for 4 bars, hats only over bars 2-3
        e.call(
            "place_pattern",
            &json!({"pattern": "beat", "start_bar": 0, "bars": 4}),
        )
        .unwrap();
        let r = e
            .call(
                "place_pattern",
                &json!({"pattern": "hats", "start_bar": 2, "bars": 2, "lane": 1}),
            )
            .unwrap();
        assert_eq!(r["song_bars"], 4);
        let p = &e.project;
        let kicks: usize = p
            .arrangement
            .iter()
            .map(|s| {
                p.patterns[p.pattern_index(&s.pattern).unwrap()]
                    .notes("kick")
                    .len()
            })
            .sum();
        let hats: usize = p
            .arrangement
            .iter()
            .map(|s| {
                p.patterns[p.pattern_index(&s.pattern).unwrap()]
                    .notes("hat")
                    .len()
            })
            .sum();
        assert_eq!(kicks, 8);
        assert_eq!(hats, 2);
        assert!((p.song_seconds() - 8.0).abs() < 0.01);
        // the source patterns are untouched
        assert_eq!(
            p.patterns[p.pattern_index("beat").unwrap()]
                .notes("kick")
                .len(),
            2
        );
        e.call("remove_clip", &json!({"pattern": "hats"})).unwrap();
        let p = &e.project;
        let hats: usize = p
            .arrangement
            .iter()
            .map(|s| {
                p.patterns[p.pattern_index(&s.pattern).unwrap()]
                    .notes("hat")
                    .len()
            })
            .sum();
        assert_eq!(hats, 0);
        e.call("render", &json!({})).unwrap();
    }

    #[test]
    fn automation_clip_places_and_restamps() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "pad", "preset": "warm_pad"}))
            .unwrap();
        e.call("create_automation_clip", &json!({"name": "swell", "track": "pad", "param": "volume", "length_beats": 4, "points": [[0, -24], [4, 0]]})).unwrap();
        e.call(
            "place_automation_clip",
            &json!({"name": "swell", "at_bar": 1, "every_beats": 8, "count": 2}),
        )
        .unwrap();
        let lane = e
            .project
            .automation
            .iter()
            .find(|l| l.is_target("pad", "volume"))
            .unwrap();
        assert_eq!(lane.points.len(), 4);
        assert!((lane.points[2].beat - 12.0).abs() < 1e-4);
        e.call("create_automation_clip", &json!({"name": "swell", "track": "pad", "param": "volume", "length_beats": 4, "points": [[0, -30], [2, -6], [4, 0]]})).unwrap();
        let lane = e
            .project
            .automation
            .iter()
            .find(|l| l.is_target("pad", "volume"))
            .unwrap();
        assert_eq!(lane.points.len(), 6);
    }

    #[test]
    fn bus_into_bus_routing_and_cycles() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "kick", "preset": "kick"}))
            .unwrap();
        e.call(
            "add_notes",
            &json!({"track": "kick", "notes": [{"start": 0, "len": 1, "pitch": 60}]}),
        )
        .unwrap();
        e.call("add_bus", &json!({"name": "drums", "preset": "none"}))
            .unwrap();
        e.call("add_bus", &json!({"name": "beat", "preset": "none"}))
            .unwrap();
        e.call("route_track", &json!({"track": "kick", "bus": "drums"}))
            .unwrap();
        let r = e
            .call("route_bus", &json!({"bus": "drums", "to": "beat"}))
            .unwrap();
        assert_eq!(r["processing_order"][0], "drums");
        assert!(e
            .call("route_bus", &json!({"bus": "beat", "to": "drums"}))
            .is_err());
        // muting the group silences the insert routed into it
        let loud = e.call("render", &json!({})).unwrap();
        let bi = e.project.bus_index("beat").unwrap();
        e.project.buses[bi].mute = true;
        e.revision += 1;
        let m = e.mix().unwrap();
        let pk = m.left.iter().fold(0.0f32, |a, v| a.max(v.abs()));
        assert!(pk < 1e-4, "group mute leaked {pk} ({loud})");
    }

    #[test]
    fn snap_and_ghosts() {
        let iv = theory::scale_intervals("minor").unwrap();
        // C minor: E natural (64) -> Eb (63) on a tie-down, F# (66) -> F (65)
        assert_eq!(snap_pitch(64, 0, iv, "nearest"), 63);
        assert_eq!(snap_pitch(66, 0, iv, "up"), 67);
        assert_eq!(snap_pitch(67, 0, iv, "nearest"), 67);
        let mut rng = Rng::new(3);
        let existing = vec![Note::new(4.0, 1.0, 60, 1.0), Note::new(12.0, 1.0, 60, 1.0)];
        let g = ghost_notes(
            &existing,
            4,
            1.0,
            (0.2, 0.3),
            60,
            &[3, 5, 6, 7, 10, 14, 15],
            &mut rng,
        );
        assert!(!g.is_empty());
        for n in &g {
            assert!(n.vel <= 0.3 + 1e-6);
            assert!(existing.iter().all(|x| (x.start - n.start).abs() >= 1.0));
        }
    }
}
