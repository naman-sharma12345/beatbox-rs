//! Project validation and master QC: structural problems (missing samples,
//! broken routing, illegal values, dead automation) and delivery checks on the
//! rendered master (overs, true peak, loudness, silence, DC, mono safety).

use crate::analysis::{self, Loudness};
use crate::automation::{self, parse_target, Target};
use crate::fx::Effect;
use crate::instruments::Instrument;
use crate::project::Project;
use crate::render::Mix;
use crate::samples::SampleBank;
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Warn,
    Fail,
}

#[derive(Serialize, Clone, Debug)]
pub struct Check {
    pub check: String,
    pub status: Status,
    pub message: String,
    /// Which tool call fixes it, when there is an obvious one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

fn item(check: &str, status: Status, message: String, fix: Option<&str>) -> Check {
    Check {
        check: check.to_string(),
        status,
        message,
        fix: fix.map(String::from),
    }
}

/// Delivery targets for check_master.
#[derive(Clone, Copy, Debug)]
pub struct MasterTargets {
    pub lufs: f32,
    pub lufs_tolerance: f32,
    pub true_peak_ceiling: f32,
}

impl Default for MasterTargets {
    fn default() -> Self {
        MasterTargets {
            lufs: -14.0,
            lufs_tolerance: 2.0,
            true_peak_ceiling: -1.0,
        }
    }
}

fn bad_number(v: &Value, path: &str, out: &mut Vec<String>) {
    match v {
        Value::Null => out.push(path.to_string()),
        Value::Object(o) => {
            for (k, x) in o {
                bad_number(x, &format!("{path}.{k}"), out);
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                bad_number(x, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

fn effect_issues(owner: &str, chain: &[Effect], p: &Project, out: &mut Vec<Check>) {
    for (i, fx) in chain.iter().enumerate() {
        let v = serde_json::to_value(fx).unwrap_or_default();
        let mut nulls = Vec::new();
        bad_number(&v, "", &mut nulls);
        if !nulls.is_empty() {
            out.push(item(
                "illegal_values",
                Status::Fail,
                format!(
                    "{owner} fx {i} ({}) has non-numeric/NaN params: {}",
                    fx.type_name(),
                    nulls.join(", ")
                ),
                Some("tweak_effect"),
            ));
        }
        let get = |k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
        for k in ["mix", "amount"] {
            if v.get(k).is_some() && !(0.0..=2.0).contains(&get(k)) {
                out.push(item(
                    "illegal_values",
                    Status::Warn,
                    format!("{owner} fx {i} {k} = {} is outside 0..1", get(k)),
                    Some("tweak_effect"),
                ));
            }
        }
        match fx {
            Effect::Delay(d) if d.feedback >= 0.95 => out.push(item(
                "illegal_values",
                Status::Warn,
                format!(
                    "{owner} delay feedback {} will self-oscillate (capped at 0.95)",
                    d.feedback
                ),
                Some("tweak_effect"),
            )),
            Effect::Compressor(c) if c.ratio < 1.0 => out.push(item(
                "illegal_values",
                Status::Fail,
                format!("{owner} compressor ratio {} < 1", c.ratio),
                Some("tweak_effect"),
            )),
            Effect::Filter(f) if f.cutoff <= 0.0 || f.cutoff > 22_000.0 => out.push(item(
                "illegal_values",
                Status::Fail,
                format!("{owner} filter cutoff {} Hz is out of range", f.cutoff),
                Some("tweak_effect"),
            )),
            Effect::Sidechain(sc) if p.track_index(&sc.source).is_err() => out.push(item(
                "routing",
                Status::Fail,
                format!("{owner} sidechain source '{}' does not exist", sc.source),
                Some("tweak_effect"),
            )),
            _ => {}
        }
    }
}

/// Structural checks that don't need audio.
pub fn structural(p: &Project, bank: &SampleBank) -> Vec<Check> {
    let mut out = Vec::new();
    let mut fails_before = 0;
    // tempo / swing
    if !(40.0..=300.0).contains(&p.bpm) || !p.bpm.is_finite() {
        out.push(item(
            "illegal_values",
            Status::Fail,
            format!("tempo {} BPM is outside 40..300", p.bpm),
            Some("set_tempo"),
        ));
    }
    if !(0.0..=1.0).contains(&p.swing) {
        out.push(item(
            "illegal_values",
            Status::Warn,
            format!("swing {} outside 0..1", p.swing),
            Some("set_tempo"),
        ));
    }
    // arrangement
    for (i, s) in p.song_sections().iter().enumerate() {
        if p.pattern_index(&s.pattern).is_err() {
            out.push(item(
                "arrangement",
                Status::Fail,
                format!("section {i} plays missing pattern '{}'", s.pattern),
                Some("set_arrangement"),
            ));
        }
    }
    if p.song_steps() == 0 {
        out.push(item(
            "arrangement",
            Status::Fail,
            "the song has zero length".into(),
            Some("set_arrangement"),
        ));
    }
    // duplicate names
    let mut names: Vec<String> = p.tracks.iter().map(|t| t.name.to_lowercase()).collect();
    names.extend(p.buses.iter().map(|b| b.name.to_lowercase()));
    let mut seen = std::collections::HashSet::new();
    for n in &names {
        if !seen.insert(n.clone()) {
            out.push(item(
                "illegal_values",
                Status::Fail,
                format!("name '{n}' is used twice (tracks/buses must be unique)"),
                None,
            ));
        }
    }
    // tracks
    let used: Vec<usize> = p
        .song_sections()
        .iter()
        .filter_map(|s| p.pattern_index(&s.pattern).ok())
        .collect();
    if p.tracks.is_empty() {
        out.push(item(
            "empty_tracks",
            Status::Fail,
            "project has no tracks".into(),
            Some("add_track or generate_beat"),
        ));
    }
    let any_audible = p.tracks.iter().any(|t| !t.mute);
    if !p.tracks.is_empty() && !any_audible {
        out.push(item(
            "empty_tracks",
            Status::Fail,
            "every track is muted".into(),
            Some("set_mixer"),
        ));
    }
    for t in &p.tracks {
        let notes: usize = used
            .iter()
            .map(|&pi| p.patterns[pi].notes(&t.name).len())
            .sum();
        if notes == 0 {
            out.push(item(
                "empty_tracks",
                Status::Warn,
                format!("track '{}' has no notes in any arranged pattern", t.name),
                Some("add_notes / set_steps or remove_track"),
            ));
        }
        if !t.volume_db.is_finite() || !(-60.0..=12.0).contains(&t.volume_db) {
            out.push(item(
                "illegal_values",
                Status::Fail,
                format!("track '{}' volume {} dB out of range", t.name, t.volume_db),
                Some("set_mixer"),
            ));
        }
        if !t.pan.is_finite() || !(-1.0..=1.0).contains(&t.pan) {
            out.push(item(
                "illegal_values",
                Status::Fail,
                format!("track '{}' pan {} outside -1..1", t.name, t.pan),
                Some("set_mixer"),
            ));
        }
        let iv = serde_json::to_value(&t.instrument).unwrap_or_default();
        let mut nulls = Vec::new();
        bad_number(&iv, "", &mut nulls);
        if !nulls.is_empty() {
            out.push(item(
                "illegal_values",
                Status::Fail,
                format!(
                    "track '{}' instrument has NaN params: {}",
                    t.name,
                    nulls.join(", ")
                ),
                Some("tweak_instrument"),
            ));
        }
        if let Instrument::Sampler(s) = &t.instrument {
            let known = p.samples.iter().any(|x| x.name == s.sample);
            if s.sample.is_empty() || !known {
                out.push(item(
                    "missing_samples",
                    Status::Fail,
                    format!(
                        "track '{}' uses sample '{}' which is not in the project",
                        t.name, s.sample
                    ),
                    Some("import_sample / download_sample"),
                ));
            } else if !bank.contains(&s.sample) {
                let path = p
                    .samples
                    .iter()
                    .find(|x| x.name == s.sample)
                    .map(|x| x.path.clone())
                    .unwrap_or_default();
                out.push(item(
                    "missing_samples",
                    Status::Fail,
                    format!("sample '{}' could not be loaded from {path}", s.sample),
                    Some("import_sample"),
                ));
            }
        }
        if let Some(b) = &t.output {
            if p.bus_index(b).is_err() {
                out.push(item(
                    "routing",
                    Status::Fail,
                    format!(
                        "track '{}' outputs to missing bus '{b}' (falls back to master)",
                        t.name
                    ),
                    Some("route_track"),
                ));
            }
        }
        for s in &t.sends {
            if p.bus_index(&s.bus).is_err() {
                out.push(item(
                    "routing",
                    Status::Fail,
                    format!("track '{}' sends to missing bus '{}'", t.name, s.bus),
                    Some("set_send"),
                ));
            } else if s.db > 6.0 {
                out.push(item(
                    "illegal_values",
                    Status::Warn,
                    format!(
                        "track '{}' send to '{}' is very hot ({} dB)",
                        t.name, s.bus, s.db
                    ),
                    Some("set_send"),
                ));
            }
        }
        effect_issues(&format!("track '{}'", t.name), &t.effects, p, &mut out);
    }
    for (pi, pat) in p.patterns.iter().enumerate() {
        let steps = pat.steps() as f32;
        for (track, notes) in &pat.clips {
            let outside = notes
                .iter()
                .filter(|n| n.start >= steps || n.start < 0.0)
                .count();
            let bad = notes
                .iter()
                .filter(|n| {
                    !n.start.is_finite()
                        || !n.len.is_finite()
                        || n.len <= 0.0
                        || !(0.0..=1.0).contains(&n.vel)
                })
                .count();
            if outside > 0 {
                out.push(item("illegal_values", Status::Warn, format!("pattern '{}' track '{track}': {outside} notes start outside the {} bars (never play)", pat.name, pat.bars), Some("set_pattern_length or clear")));
            }
            if bad > 0 {
                out.push(item(
                    "illegal_values",
                    Status::Fail,
                    format!(
                        "pattern '{}' track '{track}': {bad} notes have invalid length/velocity",
                        pat.name
                    ),
                    Some("clear + add_notes"),
                ));
            }
            if p.track_index(track).is_err() && !notes.is_empty() {
                out.push(item(
                    "empty_tracks",
                    Status::Warn,
                    format!(
                        "pattern '{}' has notes for missing track '{track}'",
                        pat.name
                    ),
                    None,
                ));
            }
        }
        let _ = pi;
    }
    // buses
    for b in &p.buses {
        let fed = p.tracks.iter().any(|t| {
            t.output
                .as_deref()
                .is_some_and(|o| o.eq_ignore_ascii_case(&b.name))
                || t.sends.iter().any(|s| s.bus.eq_ignore_ascii_case(&b.name))
        });
        if !fed {
            out.push(item(
                "routing",
                Status::Warn,
                format!("bus '{}' receives no tracks", b.name),
                Some("route_track / set_send"),
            ));
        }
        effect_issues(&format!("bus '{}'", b.name), &b.effects, p, &mut out);
    }
    effect_issues("master", &p.master_effects, p, &mut out);
    if !matches!(p.master_effects.last(), Some(Effect::Limiter(_))) {
        out.push(item(
            "master_chain",
            Status::Warn,
            "master chain does not end with a limiter; peaks are unprotected".into(),
            Some("add_effect {track:'master', type:'limiter'}"),
        ));
    }
    // automation
    for lane in &p.automation {
        let who = format!("automation {}:{}", lane.target, lane.param);
        let owner_fx: Option<&[Effect]> = if lane.target.eq_ignore_ascii_case("master") {
            Some(&p.master_effects)
        } else if let Ok(i) = p.track_index(&lane.target) {
            Some(&p.tracks[i].effects)
        } else if let Ok(i) = p.bus_index(&lane.target) {
            Some(&p.buses[i].effects)
        } else {
            None
        };
        let Some(chain) = owner_fx else {
            out.push(item(
                "automation",
                Status::Fail,
                format!("{who}: target '{}' does not exist", lane.target),
                Some("clear_automation"),
            ));
            continue;
        };
        if lane
            .points
            .iter()
            .any(|pt| !pt.beat.is_finite() || !pt.value.is_finite())
        {
            out.push(item(
                "automation",
                Status::Fail,
                format!("{who}: NaN points"),
                Some("set_automation_points"),
            ));
        }
        if lane.points.is_empty() {
            out.push(item(
                "automation",
                Status::Warn,
                format!("{who}: lane has no points"),
                Some("set_automation_points"),
            ));
        }
        let (lo, hi) = lane.range().unwrap_or((0.0, 0.0));
        match parse_target(&lane.param) {
            None => out.push(item(
                "automation",
                Status::Fail,
                format!("{who}: unknown parameter"),
                Some("clear_automation"),
            )),
            Some(Target::Volume) if hi > 12.0 => out.push(item(
                "automation",
                Status::Warn,
                format!("{who}: reaches +{hi} dB"),
                Some("set_automation_points"),
            )),
            Some(Target::Pan) if lo < -1.0 || hi > 1.0 => out.push(item(
                "automation",
                Status::Fail,
                format!("{who}: pan outside -1..1"),
                Some("set_automation_points"),
            )),
            Some(Target::Instrument(path)) => {
                let i = p.track_index(&lane.target).ok();
                let ok = i
                    .map(|i| serde_json::to_value(&p.tracks[i].instrument).unwrap_or_default())
                    .and_then(|v| automation::get_path(&v, &path))
                    .is_some();
                if !ok {
                    out.push(item(
                        "automation",
                        Status::Fail,
                        format!(
                            "{who}: instrument has no numeric parameter '{}'",
                            path.join(".")
                        ),
                        Some("list_automation"),
                    ));
                }
            }
            Some(Target::Fx(idx, path)) => match chain.get(idx) {
                None => out.push(item(
                    "automation",
                    Status::Fail,
                    format!("{who}: no effect at index {idx}"),
                    Some("clear_automation"),
                )),
                Some(fx) => {
                    let v = serde_json::to_value(fx).unwrap_or_default();
                    if automation::get_path(&v, &path).is_none() {
                        out.push(item(
                            "automation",
                            Status::Fail,
                            format!(
                                "{who}: {} has no numeric parameter '{}'",
                                fx.type_name(),
                                path.join(".")
                            ),
                            Some("get_effects"),
                        ));
                    } else if path
                        .last()
                        .is_some_and(|k| k.contains("cutoff") || k == "tone")
                        && lo <= 0.0
                    {
                        out.push(item(
                            "automation",
                            Status::Fail,
                            format!("{who}: frequency goes to {lo} Hz"),
                            Some("set_automation_points"),
                        ));
                    }
                }
            },
            _ => {}
        }
        let song = p.song_beats();
        if lane.points.first().is_some_and(|pt| pt.beat > song) {
            out.push(item(
                "automation",
                Status::Warn,
                format!("{who}: starts after the song ends (beat {song})"),
                Some("set_automation_points"),
            ));
        }
    }
    for c in &out {
        if c.status == Status::Fail {
            fails_before += 1;
        }
    }
    if fails_before == 0 {
        out.push(item(
            "structure",
            Status::Pass,
            "routing, samples, automation and values are valid".into(),
            None,
        ));
    }
    out
}

/// Delivery QC of a rendered mix.
pub fn master(mix: &Mix, loud: &Loudness, t: &MasterTargets) -> Vec<Check> {
    let mut out = Vec::new();
    let st = analysis::stats(&mix.left, &mix.right);
    if st.rms_dbfs < -70.0 {
        out.push(item(
            "silence",
            Status::Fail,
            "the master is silent".into(),
            Some("add notes to arranged patterns"),
        ));
        return out;
    }
    out.push(if st.clipped_samples > 0 {
        item(
            "clipping",
            Status::Fail,
            format!("{} samples at/over full scale", st.clipped_samples),
            Some("add/keep a master limiter, lower master_volume_db"),
        )
    } else {
        item(
            "clipping",
            Status::Pass,
            format!("no overs (sample peak {:.2} dBFS)", st.peak_dbfs),
            None,
        )
    });
    out.push(if loud.true_peak_dbtp > 0.0 {
        item(
            "true_peak",
            Status::Fail,
            format!(
                "true peak {:.2} dBTP > 0: inter-sample clipping",
                loud.true_peak_dbtp
            ),
            Some("tweak_effect master limiter ceiling_db -1"),
        )
    } else if loud.true_peak_dbtp > t.true_peak_ceiling {
        item(
            "true_peak",
            Status::Warn,
            format!(
                "true peak {:.2} dBTP above the {:.1} dBTP ceiling",
                loud.true_peak_dbtp, t.true_peak_ceiling
            ),
            Some("lower master limiter ceiling_db"),
        )
    } else {
        item(
            "true_peak",
            Status::Pass,
            format!("true peak {:.2} dBTP", loud.true_peak_dbtp),
            None,
        )
    });
    let dl = loud.integrated_lufs - t.lufs;
    out.push(if loud.integrated_lufs < -30.0 {
        item(
            "loudness",
            Status::Fail,
            format!(
                "integrated {:.1} LUFS is far too quiet (target {:.0})",
                loud.integrated_lufs, t.lufs
            ),
            Some("set_mixer master volume_db"),
        )
    } else if dl.abs() > t.lufs_tolerance {
        item(
            "loudness",
            Status::Warn,
            format!(
                "integrated {:.1} LUFS is {:+.1} LU from the {:.0} LUFS target",
                loud.integrated_lufs, dl, t.lufs
            ),
            Some(if dl < 0.0 {
                "raise master_volume_db (the limiter catches peaks)"
            } else {
                "lower master_volume_db"
            }),
        )
    } else {
        item(
            "loudness",
            Status::Pass,
            format!(
                "integrated {:.1} LUFS (target {:.0} +/- {:.0})",
                loud.integrated_lufs, t.lufs, t.lufs_tolerance
            ),
            None,
        )
    });
    let dc = loud.dc_offset[0].abs().max(loud.dc_offset[1].abs());
    out.push(if dc > 0.02 {
        item(
            "dc_offset",
            Status::Fail,
            format!("DC offset {dc:.4}"),
            Some("add a highpass filter at 20-30 Hz"),
        )
    } else if dc > 0.005 {
        item(
            "dc_offset",
            Status::Warn,
            format!("DC offset {dc:.4}"),
            Some("add a highpass filter at 20-30 Hz"),
        )
    } else {
        item(
            "dc_offset",
            Status::Pass,
            format!("DC offset {dc:.5}"),
            None,
        )
    });
    let corr = analysis::correlation(&mix.left, &mix.right);
    out.push(if corr < 0.0 || loud.mono_fold_db < -6.0 {
        item(
            "mono_compat",
            Status::Fail,
            format!(
                "phase problems: correlation {corr:.2}, mono fold-down {:.1} dB",
                loud.mono_fold_db
            ),
            Some("reduce width effects / check inverted layers"),
        )
    } else if loud.mono_fold_db < -3.0 {
        item(
            "mono_compat",
            Status::Warn,
            format!(
                "mono fold-down loses {:.1} dB (correlation {corr:.2})",
                -loud.mono_fold_db
            ),
            Some("narrow wide pads/leads with width amount < 1.3"),
        )
    } else {
        item(
            "mono_compat",
            Status::Pass,
            format!(
                "mono safe: correlation {corr:.2}, fold-down {:.1} dB",
                loud.mono_fold_db
            ),
            None,
        )
    });
    if loud.leading_silence_s > 2.0 {
        out.push(item(
            "silence",
            Status::Warn,
            format!(
                "{:.1} s of silence before the first sound",
                loud.leading_silence_s
            ),
            Some("set_arrangement / move notes earlier"),
        ));
    } else if loud.silent_percent > 30.0 {
        out.push(item(
            "silence",
            Status::Warn,
            format!("{:.0}% of the song is silent", loud.silent_percent),
            Some("fill gaps in the arrangement"),
        ));
    } else {
        out.push(item(
            "silence",
            Status::Pass,
            "no unwanted silence".into(),
            None,
        ));
    }
    let peaks: Vec<(String, f32)> = if mix.track_info.is_empty() {
        mix.stems
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    s.left
                        .iter()
                        .chain(s.right.iter())
                        .fold(0.0f32, |m, v| m.max(v.abs())),
                )
            })
            .collect()
    } else {
        mix.track_info
            .iter()
            .map(|t| (t.name.clone(), crate::dsp::db_to_gain(t.stats.peak_dbfs)))
            .collect()
    };
    for (name, pk) in &peaks {
        if *pk < 1e-4 {
            out.push(item(
                "empty_tracks",
                Status::Warn,
                format!("track '{name}' renders silent"),
                Some("check notes, mute/solo, volume, routing"),
            ));
        }
    }
    out
}

pub fn summarize(items: &[Check]) -> Value {
    let count = |s: Status| items.iter().filter(|c| c.status == s).count();
    let fails = count(Status::Fail);
    serde_json::json!({
        "passed": fails == 0,
        "fail": fails,
        "warn": count(Status::Warn),
        "pass": count(Status::Pass),
        "items": items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instruments;
    use crate::project::Track;

    #[test]
    fn catches_structural_problems() {
        let mut p = Project::new("v", 120.0);
        let mut t = Track::new("smp", Instrument::Sampler(Default::default()));
        t.output = Some("nowhere".into());
        t.pan = 3.0;
        p.tracks.push(t);
        p.tracks.push(Track::new(
            "lead",
            instruments::preset("pluck_lead").unwrap(),
        ));
        p.automation.push(crate::automation::AutomationLane::new(
            "lead",
            "fx.4.cutoff",
        ));
        let items = structural(&p, &SampleBank::default());
        let has = |c: &str, s: Status| items.iter().any(|i| i.check == c && i.status == s);
        assert!(has("missing_samples", Status::Fail));
        assert!(has("routing", Status::Fail));
        assert!(has("illegal_values", Status::Fail));
        assert!(has("empty_tracks", Status::Warn));
        assert!(has("automation", Status::Fail));
        assert_eq!(summarize(&items)["passed"], false);
    }
}
