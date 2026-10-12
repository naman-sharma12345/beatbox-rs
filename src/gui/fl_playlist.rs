//! FL-style pattern playlist: pattern clips on numbered playlist tracks,
//! painted, dragged and resized directly on a bar grid. Click an empty cell to
//! paint the selected pattern there, drag a clip to move it (bars and lanes),
//! drag its right edge to resize it, right-click it for split / trim /
//! duplicate / delete. Every gesture is one call to the same playlist tool an
//! MCP client uses (place_pattern, playlist_clip), so it is one undo step.

use super::widgets::*;
use super::Studio;
use crate::project::Project;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, Sense, Stroke};
use serde_json::{json, Value};

const HEAD_W: f32 = 96.0;
const LANE_H: f32 = 28.0;
const RULER_H: f32 = 16.0;
const EDGE_PX: f32 = 7.0;

/// A pattern's clip colour, stable per name (FL tints each pattern).
pub fn pattern_color(name: &str) -> Color32 {
    let h = name.bytes().fold(2166136261u32, |a, b| (a ^ b as u32).wrapping_mul(16777619));
    let hue = (h % 360) as f32 / 360.0;
    let c = egui::ecolor::Hsva::new(hue, 0.42, 0.78, 1.0);
    Color32::from(c)
}

/// Where a drag lands: whole bars and lanes from pixel deltas.
pub fn snap_drag(dx: f32, dy: f32, bar_px: f32) -> (i64, i64) {
    ((dx / bar_px.max(1.0)).round() as i64, (dy / LANE_H).round() as i64)
}

/// The calls that paint `pattern` at (bar, lane): the section arrangement is
/// converted to playlist clips first when the playlist is still empty.
pub fn paint_calls(p: &Project, pattern: &str, bar: u32, lane: u32) -> Vec<(&'static str, Value)> {
    let mut v = Vec::new();
    if p.playlist.is_empty() && !p.arrangement.is_empty() {
        v.push(("arrangement_to_playlist", json!({})));
    }
    v.push(("place_pattern", json!({"pattern": pattern, "start_bar": bar, "lane": lane})));
    v
}

#[derive(Clone, Copy)]
enum Grab {
    Move,
    Resize,
}

impl Studio {
    pub(super) fn fl_playlist(&mut self, ui: &mut egui::Ui, p: &Project) {
        let mut actions: Vec<(&'static str, Value)> = Vec::new();
        let lanes = p.playlist.iter().map(|c| c.lane + 1).max().unwrap_or(0).max(3) as usize + 1;
        let end_bar = p.playlist.iter().map(|c| c.start_bar + c.bars.max(1)).max().unwrap_or(0);
        let total_bars = (end_bar + 8).max(32);
        let h = RULER_H + LANE_H * lanes as f32 + 2.0;
        let (full, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
        let pt = ui.painter_at(full);
        pt.rect_filled(full, 4.0, Color32::from_rgb(30, 32, 36));
        let grid = Rect::from_min_max(pos2(full.min.x + HEAD_W, full.min.y + RULER_H), full.max);
        let bar_px = (grid.width() / total_bars as f32).max(10.0);
        let x_of = |bar: f32| grid.min.x + bar * bar_px;
        // ruler + bar grid
        pt.rect_filled(Rect::from_min_max(pos2(full.min.x, full.min.y), pos2(full.max.x, grid.min.y)), 0.0, Color32::from_rgb(38, 40, 45));
        pt.text(pos2(full.min.x + 8.0, full.min.y + RULER_H * 0.5), Align2::LEFT_CENTER, "PLAYLIST", FontId::proportional(10.0), DIM);
        for b in 0..=total_bars {
            let x = x_of(b as f32);
            let major = b % 4 == 0;
            pt.line_segment([pos2(x, grid.min.y), pos2(x, grid.max.y)], Stroke::new(1.0_f32, if major { Color32::from_rgb(58, 61, 68) } else { Color32::from_rgb(43, 45, 50) }));
            if major && b < total_bars {
                pt.text(pos2(x + 3.0, full.min.y + RULER_H * 0.5), Align2::LEFT_CENTER, (b + 1).to_string(), FontId::monospace(9.5), DIM);
            }
        }
        // lanes
        for l in 0..lanes {
            let y = grid.min.y + l as f32 * LANE_H;
            let head = Rect::from_min_size(pos2(full.min.x, y), vec2(HEAD_W - 2.0, LANE_H - 1.0));
            pt.rect_filled(head, 0.0, if l % 2 == 0 { Color32::from_rgb(40, 42, 47) } else { Color32::from_rgb(36, 38, 43) });
            pt.text(head.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, format!("Track {}", l + 1), FontId::proportional(11.0), TEXT);
            pt.line_segment([pos2(full.min.x, y + LANE_H), pos2(full.max.x, y + LANE_H)], Stroke::new(1.0_f32, Color32::from_rgb(24, 25, 28)));
            // paint: a click on an empty cell drops the selected pattern
            let lane_r = Rect::from_min_max(pos2(grid.min.x, y), pos2(grid.max.x, y + LANE_H));
            let resp = ui.interact(lane_r, ui.id().with(("flpl_lane", l)), Sense::click());
            if resp.clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let bar = ((pos.x - grid.min.x) / bar_px).floor().max(0.0) as u32;
                    match p.patterns.get(self.pattern).map(|pt| pt.name.clone()).filter(|n| !n.starts_with("pl:")) {
                        Some(name) => actions.extend(paint_calls(p, &name, bar, l as u32)),
                        None => self.toast = Some(("Select a pattern to paint (the generated pl: sections can't be placed)".into(), 0.0, false)),
                    }
                }
            }
        }
        // clips
        for c in &p.playlist {
            let id = ui.id().with(("flpl_clip", c.id.as_str()));
            let grab: Option<(Grab, egui::Vec2)> = ui.data(|d| d.get_temp::<(u8, f32, f32)>(id)).map(|(g, x, y)| (if g == 1 { Grab::Resize } else { Grab::Move }, vec2(x, y)));
            let (db, dl, dlen) = match grab {
                Some((Grab::Move, d)) => {
                    let (b, l) = snap_drag(d.x, d.y, bar_px);
                    (b, l, 0i64)
                }
                Some((Grab::Resize, d)) => (0, 0, (d.x / bar_px).round() as i64),
                None => (0, 0, 0),
            };
            let start = (c.start_bar as i64 + db).max(0) as f32;
            let lane = (c.lane as i64 + dl).clamp(0, lanes as i64 - 1) as f32;
            let len = (c.bars.max(1) as i64 + dlen).max(1) as f32;
            let r = Rect::from_min_size(pos2(x_of(start) + 1.0, grid.min.y + lane * LANE_H + 2.0), vec2(len * bar_px - 2.0, LANE_H - 4.0));
            let col = pattern_color(&c.pattern);
            let sel = self.clip_sel.as_deref() == Some(c.id.as_str()) && !c.id.is_empty();
            pt.rect_filled(r, 3.0, col.gamma_multiply(0.38));
            let title = Rect::from_min_size(r.min, vec2(r.width(), 10.0));
            pt.rect_filled(title, 3.0, col);
            pt.with_clip_rect(r).text(pos2(r.min.x + 4.0, title.center().y), Align2::LEFT_CENTER, &c.pattern, FontId::proportional(8.5), Color32::from_rgb(20, 20, 24));
            // the pattern's notes, tiny, so a clip reads as its music
            if let Ok(pi) = p.pattern_index(&c.pattern) {
                let pat = &p.patterns[pi];
                let steps = pat.steps().max(1) as f32;
                let body = Rect::from_min_max(pos2(r.min.x, title.max.y + 1.0), pos2(r.max.x, r.max.y - 1.0));
                let per_rep = pat.bars.max(1) as f32 * bar_px;
                let reps = (len / pat.bars.max(1) as f32).ceil() as usize;
                for (ti, (_, notes)) in pat.clips.iter().enumerate().take(8) {
                    let yy = body.min.y + 1.0 + (ti as f32 * 1.6) % (body.height() - 2.0).max(1.0);
                    for k in 0..reps {
                        for n in notes.iter().take(256) {
                            let x0 = r.min.x + k as f32 * per_rep + n.start / steps * per_rep;
                            if x0 >= r.max.x - 1.0 {
                                continue;
                            }
                            pt.line_segment([pos2(x0, yy), pos2((x0 + 1.5).min(r.max.x), yy)], Stroke::new(1.0_f32, col.gamma_multiply(0.95)));
                        }
                    }
                }
            }
            pt.rect_stroke(r, 3.0, Stroke::new(if sel { 1.5_f32 } else { 1.0 }, if sel { Color32::WHITE } else { col.gamma_multiply(0.7) }));
            // right edge grip
            let edge = Rect::from_min_max(pos2(r.max.x - EDGE_PX, r.min.y), r.max);
            pt.line_segment([pos2(r.max.x - 3.0, r.min.y + 12.0), pos2(r.max.x - 3.0, r.max.y - 3.0)], Stroke::new(1.0_f32, Color32::from_white_alpha(90)));
            let body_resp = ui.interact(r, id.with("body"), Sense::click_and_drag());
            let edge_resp = ui.interact(edge, id.with("edge"), Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            let (resp, kind) = if edge_resp.dragged() || edge_resp.drag_stopped() { (&edge_resp, 1u8) } else { (&body_resp, 0u8) };
            if resp.drag_started() {
                ui.data_mut(|d| d.insert_temp(id, (kind, 0.0f32, 0.0f32)));
            }
            if resp.dragged() {
                let d = resp.drag_delta();
                let cur = ui.data(|m| m.get_temp::<(u8, f32, f32)>(id)).unwrap_or((kind, 0.0, 0.0));
                ui.data_mut(|m| m.insert_temp(id, (cur.0, cur.1 + d.x, cur.2 + d.y)));
            }
            if resp.drag_stopped() {
                ui.data_mut(|m| m.remove::<(u8, f32, f32)>(id));
                if kind == 1 && dlen != 0 {
                    actions.push(("playlist_clip", json!({"action": "trim", "id": c.id, "end_bar": c.start_bar as i64 + len as i64})));
                } else if kind == 0 && (db != 0 || dl != 0) {
                    actions.push(("playlist_clip", json!({"action": "move", "id": c.id, "to_bar": start as i64, "lane": lane as i64})));
                }
            }
            if body_resp.clicked() {
                self.clip_sel = Some(c.id.clone());
                if let Ok(pi) = p.pattern_index(&c.pattern) {
                    self.pattern = pi;
                }
            }
            let menu = super::playlist::pattern_clip_menu(c, self.player.position_secs() * p.bpm / 240.0);
            body_resp.on_hover_text(format!("{} · bar {}–{} · drag to move, drag the edge to resize, right-click for more", c.pattern, c.start_bar + 1, c.start_bar + c.bars.max(1))).context_menu(|ui| {
                for (label, tool, args) in menu.clone() {
                    if ui.button(label).clicked() {
                        actions.push((tool, args));
                        ui.close_menu();
                    }
                }
            });
        }
        // playhead
        let bar_now = self.player.position_secs() * p.bpm / 240.0;
        if bar_now > 0.0 && bar_now < total_bars as f32 {
            let x = x_of(bar_now);
            pt.line_segment([pos2(x, full.min.y), pos2(x, full.max.y)], Stroke::new(1.5_f32, Color32::from_rgb(120, 220, 140)));
        }
        if p.playlist.is_empty() {
            pt.text(grid.center(), Align2::CENTER_CENTER, "Click a cell to paint the selected pattern here", FontId::proportional(12.0), DIM);
        }
        for (tool, args) in actions {
            self.call(tool, args);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;

    #[test]
    fn drag_snaps_to_bars_and_lanes() {
        assert_eq!(snap_drag(31.0, 0.0, 20.0), (2, 0));
        assert_eq!(snap_drag(-9.0, 30.0, 20.0), (0, 1));
        assert_eq!(snap_drag(-11.0, -15.0, 20.0), (-1, -1));
    }

    #[test]
    fn colors_are_stable_and_distinct() {
        assert_eq!(pattern_color("verse"), pattern_color("verse"));
        assert_ne!(pattern_color("verse"), pattern_color("hook"));
    }

    #[test]
    fn painting_on_a_section_song_converts_then_places() {
        let d = std::env::temp_dir().join(format!("flpl_test_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("add_pattern", &json!({"name": "verse", "bars": 2})).unwrap();
        let first = "verse".to_string();
        e.call("set_arrangement", &json!({"sections": [{"pattern": first, "repeats": 2}]})).unwrap();
        let calls = paint_calls(&e.project, &first, 6, 1);
        if !e.project.arrangement.is_empty() {
            assert_eq!(calls[0].0, "arrangement_to_playlist");
        }
        for (t, a) in calls {
            e.call(t, &a).unwrap();
        }
        assert!(e.project.playlist.iter().any(|c| c.start_bar == 6 && c.lane == 1), "{:?}", e.project.playlist.iter().map(|c| (c.start_bar, c.lane)).collect::<Vec<_>>());
    }
}
