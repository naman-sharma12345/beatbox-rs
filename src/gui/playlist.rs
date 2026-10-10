// Layout ideas adapted from SoundCraft `crates/ui-egui/src/edit_window.rs` (commit eac0edd):
// a track-header column beside timeline lanes, stacked rulers (Bars|Beats, tempo), clips with
// a coloured title bar over a tinted body, alternating lane fills, bar/beat grid and playhead.
// Copyright (c) 2026 ArtCraft Team and the SoundCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your option;
// used in Beatbox under the MIT License. See THIRD_PARTY_NOTICES.md.

//! The playlist view: the song arrangement shown as clips, one lane per track, laid out on
//! the PPQ timebase (`timebase::TempoMap`, 960 ticks per quarter). Read-only groundwork for
//! native clips: clicking a clip selects its pattern and track for the piano roll.

use super::theme::{clip_colors, Tokens};
use super::widgets::*;
use super::Studio;
use crate::project::Project;
use crate::timebase::{step_to_tick, TempoMap, TICKS_PER_QUARTER};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, Sense, Stroke};

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
        let total = clips
            .last()
            .map_or(TICKS_PER_QUARTER * 16, |c| c.end)
            .max(1);
        let lanes = p.tracks.len().max(1);
        let want_h = RULER_H * 2.0 + LANE_H * lanes as f32 + 4.0;
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
    }
}
