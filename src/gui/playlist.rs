// Layout ideas adapted from SoundCraft `crates/ui-egui/src/edit_window.rs` (commit eac0edd):
// a track-header column beside timeline lanes, stacked rulers (Bars|Beats, tempo), clips with
// a coloured title bar over a tinted body, alternating lane fills, bar/beat grid and playhead.
// Copyright (c) 2026 ArtCraft Team and the SoundCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your option;
// used in Beatbox under the MIT License. See THIRD_PARTY_NOTICES.md.

//! The playlist view: the song arrangement shown as clips, one lane per track, laid out on
//! the PPQ timebase (`timebase::TempoMap`, 960 ticks per quarter). Clicking a pattern clip
//! selects its pattern and track for the piano roll. Audio clips (takes, bounces) sit on
//! their track's lane; click one to select it and right-click it for the edit menu (split,
//! trim, move, duplicate, slip, fades, crossfade, consolidate, ripple delete). Every menu
//! item is one call to the same timeline tool an MCP client uses (one undo step).

use super::theme::{clip_colors, Tokens};
use super::widgets::*;
use super::Studio;
use crate::project::{AudioClip, Project};
use crate::timebase::{step_to_tick, TempoMap, TICKS_PER_QUARTER};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, Sense, Stroke};
use serde_json::{json, Value};

const HEAD_W: f32 = 150.0;
const RULER_H: f32 = 18.0;
const LANE_H: f32 = 40.0;

/// One placed clip: pattern index, start and end in ticks.
pub struct PlacedClip {
    pub pattern: usize,
    pub start: i64,
    pub end: i64,
}

/// The arrangement as tick-positioned clips (each section repeat is one clip).
pub fn placed_clips(p: &Project) -> Vec<PlacedClip> {
    let mut out = Vec::new();
    let mut at = 0i64;
    for s in p.song_sections() {
        let Ok(pi) = p.pattern_index(&s.pattern) else {
            continue;
        };
        let Some(pat) = p.patterns.get(pi) else {
            continue;
        };
        let len = step_to_tick(pat.steps() as f32);
        for _ in 0..s.repeats.max(1) {
            out.push(PlacedClip {
                pattern: pi,
                start: at,
                end: at.saturating_add(len),
            });
            at = at.saturating_add(len);
        }
    }
    out
}

impl Studio {
    pub(super) fn playlist_view(&mut self, ui: &mut egui::Ui, p: &Project) {
        let tk = Tokens::DARK;
        let map = TempoMap::for_project(p);
        let clips = placed_clips(p);
        // audio clip lengths need the decoded takes
        let take_secs: Vec<f32> = {
            let e = self.engine.lock().unwrap();
            p.audio_clips
                .iter()
                .map(|c| {
                    audio_clip_secs(
                        c,
                        e.bank
                            .get(&c.sample)
                            .map(|d| d.len() as f32 / crate::dsp::SR),
                    )
                })
                .collect()
        };
        let tick_of_beat = |b: f32| (f64::from(b) * TICKS_PER_QUARTER as f64).round() as i64;
        let audio_end = p
            .audio_clips
            .iter()
            .zip(&take_secs)
            .map(|(c, s)| tick_of_beat(c.start_beat + s * p.bpm / 60.0))
            .max()
            .unwrap_or(0);
        let total = clips
            .last()
            .map_or(TICKS_PER_QUARTER * 16, |c| c.end)
            .max(audio_end)
            .max(1);
        let playhead_beat = self.player.position_secs() * p.bpm / 60.0;
        let mut actions: Vec<(&'static str, Value)> = Vec::new();
        let lanes = p.tracks.len().max(1);
        let want_h = RULER_H * 2.0 + LANE_H * lanes as f32 + 4.0;
        // performance strip: launch any pattern live, on the next bar
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new("PERFORM")
                    .size(11.0)
                    .color(tk.ruler_text),
            );
            for pat in &p.patterns {
                let live = self.perf.as_deref() == Some(pat.name.as_str());
                let next = matches!(&self.perf_next, Some(Some(n)) if *n == pat.name);
                let label = if next {
                    format!("{} (next bar)", pat.name)
                } else {
                    pat.name.clone()
                };
                let b = ui.selectable_label(live, label).on_hover_text(format!(
                    "Loop {} ({} bars), switching on the next bar",
                    pat.name, pat.bars
                ));
                if b.clicked() {
                    let key = if pat.id.is_empty() {
                        pat.name.clone()
                    } else {
                        pat.id.clone()
                    };
                    actions.push(("perform", json!({"action": "launch", "pattern": key})));
                }
            }
            if self.perf.is_some() || self.perf_next.is_some() {
                let back = matches!(&self.perf_next, Some(None));
                let label = if back { "SONG (next bar)" } else { "SONG" };
                if ui
                    .button(label)
                    .on_hover_text("Back to the song arrangement on the next bar")
                    .clicked()
                {
                    actions.push(("perform", json!({"action": "exit"})));
                }
            }
        });
        // FL-style pattern playlist: paint, drag and resize pattern clips
        self.fl_playlist(ui, p);
        ui.add_space(6.0);
        // pattern clips on the playlist, by stable id: click to select, right-click to edit
        if false && !p.playlist.is_empty() {
            let playhead_bar = playhead_beat / 4.0;
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("CLIPS").size(11.0).color(tk.ruler_text));
                let mut order: Vec<&crate::project::PlaylistClip> = p.playlist.iter().collect();
                order.sort_by_key(|c| (c.start_bar, c.lane));
                for c in order {
                    let sel = self.clip_sel.as_deref() == Some(c.id.as_str()) && !c.id.is_empty();
                    let b = ui
                        .selectable_label(
                            sel,
                            format!(
                                "{} \u{00B7} bar {}\u{2013}{}",
                                c.pattern,
                                c.start_bar + 1,
                                c.start_bar + c.bars.max(1)
                            ),
                        )
                        .on_hover_text(format!(
                            "{} · lane {} · right-click to move, split, trim, duplicate or delete",
                            c.id, c.lane
                        ));
                    if b.clicked() || b.secondary_clicked() {
                        self.clip_sel = Some(c.id.clone());
                    }
                    let menu = pattern_clip_menu(c, playhead_bar);
                    b.context_menu(|ui| {
                        for (label, tool, args) in menu.clone() {
                            if ui.button(label).clicked() {
                                actions.push((tool, args));
                                ui.close_menu();
                            }
                        }
                    });
                }
            });
        }
        egui::ScrollArea::vertical()
            .id_salt("playlist")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (full, _) = ui.allocate_exact_size(
                    vec2(ui.available_width(), want_h.max(ui.available_height())),
                    Sense::hover(),
                );
                let pt = ui.painter_at(full);
                pt.rect_filled(full, 0.0, tk.playlist_bg);
                let tl = Rect::from_min_max(pos2(full.min.x + HEAD_W, full.min.y), full.max);
                let px_per_tick = (tl.width() - 6.0) / total as f32;
                let x_of = |tick: i64| tl.min.x + tick as f32 * px_per_tick;
                // rulers
                let bars_r = Rect::from_min_size(tl.min, vec2(tl.width(), RULER_H));
                let tempo_r =
                    Rect::from_min_size(pos2(tl.min.x, bars_r.max.y), vec2(tl.width(), RULER_H));
                pt.rect_filled(bars_r, 0.0, tk.ruler_bg);
                pt.rect_filled(tempo_r, 0.0, tk.ruler_bg);
                for (r, label) in [(bars_r, "Bars|Beats"), (tempo_r, "Tempo")] {
                    let lr = Rect::from_min_max(pos2(full.min.x, r.min.y), pos2(tl.min.x, r.max.y));
                    pt.rect_filled(lr, 0.0, tk.ruler_bg);
                    pt.text(
                        pos2(lr.max.x - 6.0, lr.center().y),
                        Align2::RIGHT_CENTER,
                        label,
                        FontId::proportional(9.5),
                        tk.ruler_text,
                    );
                }
                let tpbar = map.meter_at_tick(0).ticks_per_bar().max(1);
                let bar_px = tpbar as f32 * px_per_tick;
                let every = if bar_px > 40.0 {
                    1
                } else if bar_px > 16.0 {
                    4
                } else {
                    8
                };
                let mut bar = 1i64;
                loop {
                    let tick = map.bar_start_tick(bar);
                    if tick > total {
                        break;
                    }
                    let x = x_of(tick);
                    let major = (bar - 1) % every == 0;
                    pt.line_segment(
                        [
                            pos2(x, bars_r.max.y - if major { 10.0 } else { 4.0 }),
                            pos2(x, bars_r.max.y),
                        ],
                        Stroke::new(1.0_f32, tk.ruler_tick),
                    );
                    if major {
                        pt.text(
                            pos2(x + 3.0, bars_r.center().y - 2.0),
                            Align2::LEFT_CENTER,
                            bar.to_string(),
                            FontId::proportional(9.5),
                            tk.ruler_text,
                        );
                    }
                    // lane grid
                    pt.line_segment(
                        [pos2(x, tempo_r.max.y), pos2(x, full.max.y)],
                        Stroke::new(1.0_f32, if major { tk.bar_line } else { tk.grid_line }),
                    );
                    bar += 1;
                }
                for ev in map.tempos() {
                    let x = x_of(ev.tick);
                    pt.rect_filled(
                        Rect::from_min_size(pos2(x, tempo_r.min.y + 3.0), vec2(3.0, RULER_H - 6.0)),
                        0.0,
                        tk.tempo_ruler,
                    );
                    pt.text(
                        pos2(x + 6.0, tempo_r.center().y),
                        Align2::LEFT_CENTER,
                        format!(
                            "{:.0} BPM · {}/{}",
                            ev.bpm,
                            map.meter_at_tick(ev.tick).numerator,
                            map.meter_at_tick(ev.tick).denominator
                        ),
                        FontId::proportional(9.5),
                        tk.ruler_text,
                    );
                }
                // lanes
                let lanes_top = tempo_r.max.y;
                for (ti, t) in p.tracks.iter().enumerate() {
                    let y = lanes_top + ti as f32 * LANE_H;
                    let lane = Rect::from_min_max(pos2(tl.min.x, y), pos2(tl.max.x, y + LANE_H));
                    if ti % 2 == 1 {
                        pt.rect_filled(lane, 0.0, tk.playlist_alt.gamma_multiply(0.6));
                    }
                    let color = track_color(&t.name, t.instrument.kind_name());
                    let head = Rect::from_min_max(
                        pos2(full.min.x, y),
                        pos2(tl.min.x - 1.0, y + LANE_H - 1.0),
                    );
                    let sel_track = ti == self.selected;
                    pt.rect_filled(
                        head,
                        0.0,
                        if sel_track {
                            Color32::from_rgb(54, 58, 64)
                        } else {
                            tk.panel_bg2
                        },
                    );
                    pt.rect_filled(
                        Rect::from_min_size(head.min, vec2(4.0, head.height())),
                        0.0,
                        color,
                    );
                    pt.text(
                        pos2(head.min.x + 10.0, head.center().y - 6.0),
                        Align2::LEFT_CENTER,
                        &t.name,
                        FontId::proportional(12.0),
                        if t.mute { tk.text_dim } else { tk.text },
                    );
                    pt.text(
                        pos2(head.min.x + 10.0, head.center().y + 8.0),
                        Align2::LEFT_CENTER,
                        t.instrument.kind_name(),
                        FontId::proportional(9.0),
                        tk.text_dim,
                    );
                    if ui
                        .interact(head, ui.id().with(("pl_head", ti)), Sense::click())
                        .clicked()
                    {
                        self.selected = ti;
                    }
                    pt.line_segment(
                        [pos2(full.min.x, y + LANE_H), pos2(full.max.x, y + LANE_H)],
                        Stroke::new(1.0_f32, tk.border),
                    );
                    for (ci, c) in clips.iter().enumerate() {
                        let Some(pat) = p.patterns.get(c.pattern) else {
                            continue;
                        };
                        let notes = pat.notes(&t.name);
                        if notes.is_empty() {
                            continue;
                        }
                        let r = Rect::from_min_max(
                            pos2(x_of(c.start) + 1.0, y + 2.0),
                            pos2(x_of(c.end) - 1.0, y + LANE_H - 2.0),
                        );
                        let selected = sel_track && c.pattern == self.pattern;
                        let (body, bar_c, ink) = clip_colors(color, selected);
                        pt.rect_filled(r, 2.0, body);
                        let title = Rect::from_min_size(r.min, vec2(r.width(), 11.0));
                        pt.rect_filled(title, 2.0, bar_c);
                        pt.with_clip_rect(title).text(
                            pos2(title.min.x + 3.0, title.center().y),
                            Align2::LEFT_CENTER,
                            &pat.name,
                            FontId::proportional(8.5),
                            tk.text_dark,
                        );
                        // mini notes
                        let body_r = Rect::from_min_max(
                            pos2(r.min.x, title.max.y + 1.0),
                            pos2(r.max.x, r.max.y - 1.0),
                        );
                        let (lo, hi) = notes
                            .iter()
                            .fold((127u8, 0u8), |(l, h), n| (l.min(n.pitch), h.max(n.pitch)));
                        let span = f32::from(hi.saturating_sub(lo).max(6));
                        for n in notes {
                            let x0 = x_of(c.start + step_to_tick(n.start));
                            let x1 = x_of(c.start + step_to_tick(n.start + n.len)).max(x0 + 1.5);
                            let yy = body_r.max.y
                                - 2.0
                                - f32::from(n.pitch.saturating_sub(lo)) / span
                                    * (body_r.height() - 4.0);
                            pt.line_segment(
                                [pos2(x0, yy), pos2(x1.min(r.max.x), yy)],
                                Stroke::new(2.0_f32, ink),
                            );
                        }
                        if selected {
                            pt.rect_stroke(r, 2.0, Stroke::new(1.0_f32, Color32::WHITE));
                        }
                        let resp = ui
                            .interact(r, ui.id().with(("pl_clip", ti, ci)), Sense::click())
                            .on_hover_text(format!(
                                "{} · {} · bar {} ({} ticks)",
                                t.name,
                                pat.name,
                                map.bar_beat_at_tick(c.start).bar,
                                c.end - c.start
                            ));
                        if resp.clicked() {
                            self.selected = ti;
                            self.pattern = c.pattern;
                        }
                    }
                    // audio clips on this track's channel
                    for (ai, c) in p.audio_clips.iter().enumerate() {
                        if !c.track.eq_ignore_ascii_case(&t.name) {
                            continue;
                        }
                        let secs = take_secs[ai];
                        let (s0, s1) = (
                            tick_of_beat(c.start_beat),
                            tick_of_beat(c.start_beat + secs * p.bpm / 60.0),
                        );
                        let r = Rect::from_min_max(
                            pos2(x_of(s0.max(0)) + 1.0, y + 2.0),
                            pos2(x_of(s1).max(x_of(s0.max(0)) + 3.0) - 1.0, y + LANE_H - 2.0),
                        );
                        let selected =
                            self.clip_sel.as_deref() == Some(c.id.as_str()) && !c.id.is_empty();
                        let audio_c = Color32::from_rgb(92, 150, 190);
                        let (body, bar_c, ink) = clip_colors(audio_c, selected);
                        pt.rect_filled(r, 2.0, body);
                        let title = Rect::from_min_size(r.min, vec2(r.width(), 11.0));
                        pt.rect_filled(title, 2.0, bar_c);
                        pt.with_clip_rect(title).text(
                            pos2(title.min.x + 3.0, title.center().y),
                            Align2::LEFT_CENTER,
                            &c.sample,
                            FontId::proportional(8.5),
                            tk.text_dark,
                        );
                        // fade ramps
                        let px_s = r.width() / secs.max(1e-3);
                        for (ms, left) in [(c.fade_in_ms, true), (c.fade_out_ms, false)] {
                            if let Some(ms) = ms.filter(|m| *m > 6.0) {
                                let w = (ms / 1000.0 * px_s).min(r.width());
                                let (a, b) = if left {
                                    (pos2(r.min.x, r.max.y), pos2(r.min.x + w, title.max.y))
                                } else {
                                    (pos2(r.max.x - w, title.max.y), pos2(r.max.x, r.max.y))
                                };
                                pt.line_segment([a, b], Stroke::new(1.0_f32, ink));
                            }
                        }
                        if selected {
                            pt.rect_stroke(r, 2.0, Stroke::new(1.0_f32, Color32::WHITE));
                        }
                        let resp = ui
                            .interact(r, ui.id().with(("pl_audio", ai)), Sense::click())
                            .on_hover_text(format!(
                                "{} · {} · beat {:.2} · {:.2} s",
                                t.name, c.sample, c.start_beat, secs
                            ));
                        if resp.clicked() || resp.secondary_clicked() {
                            self.selected = ti;
                            self.clip_sel = Some(c.id.clone());
                        }
                        let id = c.id.clone();
                        let next = p
                            .audio_clips
                            .iter()
                            .filter(|o| {
                                o.track.eq_ignore_ascii_case(&c.track)
                                    && o.start_beat > c.start_beat
                            })
                            .min_by(|a, b| a.start_beat.total_cmp(&b.start_beat))
                            .map(|o| o.id.clone());
                        let track = t.name.clone();
                        resp.context_menu(|ui| {
                            for (label, tool, args) in
                                clip_menu(&id, &track, playhead_beat, next.as_deref())
                            {
                                if ui.button(label).clicked() {
                                    actions.push((tool, args));
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                }
                // playhead
                let tick = step_to_tick(self.player.position_secs() / p.step_secs().max(1e-4));
                if tick <= total {
                    let x = x_of(tick);
                    pt.line_segment(
                        [pos2(x, full.min.y), pos2(x, full.max.y)],
                        Stroke::new(1.5_f32, tk.playhead),
                    );
                }
                if p.tracks.is_empty() {
                    pt.text(
                        tl.center(),
                        Align2::CENTER_CENTER,
                        "No tracks yet",
                        FontId::proportional(14.0),
                        DIM,
                    );
                }
            });
        for (tool, args) in actions {
            self.call(tool, args);
        }
    }
}

/// Seconds an audio clip plays, given its take's length when known.
pub fn audio_clip_secs(c: &AudioClip, take_secs: Option<f32>) -> f32 {
    let avail = take_secs.map(|t| (t - c.offset_s.max(0.0)).max(0.0));
    match (c.length_s, avail) {
        (Some(l), Some(a)) => l.min(a),
        (Some(l), None) => l,
        (None, Some(a)) => a,
        (None, None) => 1.0,
    }
}

/// The audio clip edit menu: label, timeline tool, arguments.
pub fn clip_menu(
    id: &str,
    track: &str,
    playhead_beat: f32,
    next: Option<&str>,
) -> Vec<(&'static str, &'static str, Value)> {
    let mut v = vec![
        (
            "Split at playhead",
            "split_clip",
            json!({"id": id, "at_beat": playhead_beat}),
        ),
        (
            "Trim start to playhead",
            "trim_clip",
            json!({"id": id, "start_beat": playhead_beat}),
        ),
        (
            "Trim end to playhead",
            "trim_clip",
            json!({"id": id, "end_beat": playhead_beat}),
        ),
        (
            "Move to playhead",
            "move_clip",
            json!({"id": id, "start_beat": playhead_beat}),
        ),
        ("Duplicate", "duplicate_clip", json!({"id": id})),
        (
            "Slip 10 ms earlier",
            "slip_clip",
            json!({"id": id, "by_s": -0.01}),
        ),
        (
            "Slip 10 ms later",
            "slip_clip",
            json!({"id": id, "by_s": 0.01}),
        ),
        (
            "Fade in 200 ms",
            "fade_clip",
            json!({"id": id, "fade_in_ms": 200}),
        ),
        (
            "Fade out 500 ms",
            "fade_clip",
            json!({"id": id, "fade_out_ms": 500}),
        ),
        (
            "Remove fades",
            "fade_clip",
            json!({"id": id, "fade_in_ms": 0, "fade_out_ms": 0}),
        ),
        (
            "Reverse",
            "reverse_clip",
            json!({"id": id, "reverse": true}),
        ),
        (
            "Pitch up 1",
            "pitch_clip",
            json!({"id": id, "semitones": 1}),
        ),
        (
            "Pitch down 1",
            "pitch_clip",
            json!({"id": id, "semitones": -1}),
        ),
        (
            "Pitch up an octave",
            "pitch_clip",
            json!({"id": id, "semitones": 12}),
        ),
        (
            "Pitch down an octave",
            "pitch_clip",
            json!({"id": id, "semitones": -12}),
        ),
        (
            "Half speed",
            "stretch_clip",
            json!({"id": id, "factor": 2.0}),
        ),
        (
            "Double speed",
            "stretch_clip",
            json!({"id": id, "factor": 0.5}),
        ),
        ("Mute", "mute_clip", json!({"id": id, "muted": true})),
        ("Unmute", "mute_clip", json!({"id": id, "muted": false})),
        ("Use this take for its span", "comp_take", json!({"id": id})),
    ];
    if let Some(n) = next {
        v.push((
            "Crossfade with next clip",
            "crossfade_clips",
            json!({"first": id, "second": n, "ms": 30}),
        ));
    }
    v.push((
        "Consolidate track clips",
        "consolidate_clips",
        json!({"track": track}),
    ));
    v.push(("Ripple delete", "ripple_delete", json!({"id": id})));
    v.push(("Delete", "remove_audio_clip", json!({"id": id})));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_clip_menu_item_is_a_registered_tool_with_valid_args() {
        for (label, tool, args) in clip_menu("clip1", "vox", 2.0, Some("clip2")) {
            let t = crate::tools::find(tool).unwrap_or_else(|| panic!("{label}: no tool {tool}"));
            let schema = (t.schema)();
            for k in args.as_object().unwrap().keys() {
                assert!(
                    schema["properties"].get(k).is_some(),
                    "{label}: {tool} has no argument {k}"
                );
            }
            assert!(
                !label.to_lowercase().contains(&["ai", ""].join(" ")),
                "manual wording: {label}"
            );
        }
    }
}

/// The pattern clip edit menu: label, tool, arguments (each one undo step).
pub fn pattern_clip_menu(
    c: &crate::project::PlaylistClip,
    playhead_bar: f32,
) -> Vec<(&'static str, &'static str, Value)> {
    let id = c.id.as_str();
    let (s0, s1) = (
        i64::from(c.start_bar),
        i64::from(c.start_bar + c.bars.max(1)),
    );
    let mut v = vec![
        (
            "Move left a bar",
            "playlist_clip",
            json!({"action": "move", "id": id, "by_bars": -1}),
        ),
        (
            "Move right a bar",
            "playlist_clip",
            json!({"action": "move", "id": id, "by_bars": 1}),
        ),
    ];
    let at = playhead_bar.round() as i64;
    if at > s0 && at < s1 {
        v.push((
            "Split at playhead",
            "playlist_clip",
            json!({"action": "split", "id": id, "at_bar": at}),
        ));
    }
    if s1 - s0 > 1 {
        v.push((
            "Trim start a bar",
            "playlist_clip",
            json!({"action": "trim", "id": id, "start_bar": s0 + 1}),
        ));
        v.push((
            "Trim end a bar",
            "playlist_clip",
            json!({"action": "trim", "id": id, "end_bar": s1 - 1}),
        ));
    }
    v.push((
        "Extend end a bar",
        "playlist_clip",
        json!({"action": "trim", "id": id, "end_bar": s1 + 1}),
    ));
    v.push((
        "Duplicate after",
        "playlist_clip",
        json!({"action": "duplicate", "id": id}),
    ));
    v.push((
        "Make unique",
        "playlist_clip",
        json!({"action": "make_unique", "id": id}),
    ));
    v.push((
        "Lane down",
        "playlist_clip",
        json!({"action": "move", "id": id, "lane": c.lane + 1}),
    ));
    if c.lane > 0 {
        v.push((
            "Lane up",
            "playlist_clip",
            json!({"action": "move", "id": id, "lane": c.lane - 1}),
        ));
    }
    v.push((
        "Delete",
        "playlist_clip",
        json!({"action": "delete", "id": id}),
    ));
    v
}

#[cfg(test)]
mod menu_tests {
    use crate::engine::Engine;
    use serde_json::json;

    #[test]
    fn pattern_clip_menu_items_are_valid_calls() {
        let d = std::env::temp_dir().join("beatbox_pl_menu_tests");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d.clone());
        e.call("new_project", &json!({"name": "m", "bpm": 120}))
            .unwrap();
        e.call("add_track", &json!({"name": "kick", "preset": "kick"}))
            .unwrap();
        e.call("add_pattern", &json!({"name": "riff", "bars": 1}))
            .unwrap();
        e.call(
            "place_pattern",
            &json!({"pattern": "riff", "start_bar": 2, "bars": 4, "lane": 1}),
        )
        .unwrap();
        let c = e.project.playlist[0].clone();
        let menu = super::pattern_clip_menu(&c, 3.6);
        assert!(menu
            .iter()
            .any(|m| m.0 == "Split at playhead" && m.2["at_bar"] == 4));
        assert!(menu.iter().any(|m| m.0 == "Lane up"));
        for (label, tool, args) in menu {
            let mut e2 = Engine::new(d.clone());
            e2.project = e.project.clone();
            let r = e2
                .call(tool, &args)
                .unwrap_or_else(|err| panic!("{label}: {err:#}"));
            assert!(r["clips"].is_array(), "{label}");
        }
    }
}
