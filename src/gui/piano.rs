// Adapted from SoundCraft `crates/ui-egui/src/midi_editor.rs` (commit eac0edd): note drawing
// with velocity shading, keyboard strip, bar/grid lines, click-to-add, drag to move or resize
// with grid snapping, rubber-band and shift-click selection, arrow-key nudge/transpose, Delete,
// a header toolbar of whole-pattern ops and a velocity lane.
// Copyright (c) 2026 ArtCraft Team and the SoundCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your option;
// used in Beatbox under the MIT License. See THIRD_PARTY_NOTICES.md.

//! The piano roll panel. Notes are in Beatbox steps (16ths); the snap grid comes from the
//! PPQ timebase (`timebase::NoteValue`). Every edit is ONE engine tool call
//! (`add_notes` with `replace`, or `quantize` / `legato` / `edit_notes` for toolbar ops), so
//! a gesture is one undo step and an AI watching the project sees the same change.

use super::theme::Tokens;
use super::widgets::*;
use super::Studio;
use crate::note_edit::{apply_shift, notes_json, snap_to, NoteShift};
use crate::project::{Note, Project};
use crate::theory;
use crate::timebase::{tick_to_step, NoteValue};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke};
use serde_json::{json, Value};

const KEY_W: f32 = 40.0;
const HEADER_H: f32 = 22.0;
const VEL_H: f32 = 48.0;

/// Snap choices: label and grid in steps (None = off).
pub const SNAPS: [(&str, Option<NoteValue>); 5] = [
    ("1/4", Some(NoteValue::Quarter)),
    ("1/8", Some(NoteValue::Eighth)),
    ("1/16", Some(NoteValue::Sixteenth)),
    ("1/32", Some(NoteValue::ThirtySecond)),
    ("off", None),
];

/// Grid size in steps for a snap choice (off = 1/64 so drags stay sane).
pub fn snap_steps(i: usize) -> f32 {
    let v = SNAPS
        .get(i)
        .and_then(|s| s.1)
        .unwrap_or(NoteValue::SixtyFourth);
    tick_to_step(v.ticks())
}

#[derive(Debug, Clone, Default)]
pub struct PianoState {
    /// Indices into the (sorted) note list of the current track+pattern.
    pub selected: Vec<usize>,
    pub drag: Option<NoteDrag>,
    /// Rubber-band selection start (screen position).
    pub band: Option<Pos2>,
    /// Index into [`SNAPS`].
    pub snap: usize,
    /// (pattern, track) the selection belongs to.
    pub owner: (usize, usize),
}

#[derive(Debug, Clone)]
pub struct NoteDrag {
    pub start: Pos2,
    pub shift: NoteShift,
}

impl Studio {
    /// Replace the track's notes in the pattern with `notes` in one undoable call.
    fn write_notes(&mut self, track: &str, pattern: &str, notes: &[Note]) {
        self.call(
            "add_notes",
            json!({"track": track, "pattern": pattern, "replace": true, "notes": notes_json(notes)}),
        );
    }

    pub(super) fn piano_roll(&mut self, ui: &mut egui::Ui, p: &Project) {
        let tk = Tokens::DARK;
        let (Some(t), Some(pat)) = (p.tracks.get(self.selected), p.patterns.get(self.pattern))
        else {
            return;
        };
        if self.piano.owner != (self.pattern, self.selected) {
            self.piano.selected.clear();
            self.piano.drag = None;
            self.piano.owner = (self.pattern, self.selected);
        }
        let tname = t.name.clone();
        let pname = pat.name.clone();
        let color = track_color(&t.name, t.instrument.kind_name());
        let notes: Vec<Note> = pat.notes(&t.name).to_vec();
        self.piano.selected.retain(|i| *i < notes.len());
        let steps = pat.steps() as f32;
        let is_drum = t.instrument.is_drum();
        let (lo, hi) = if notes.is_empty() {
            (48u8, 72u8)
        } else {
            let (l, h) = notes
                .iter()
                .fold((127u8, 0u8), |(l, h), n| (l.min(n.pitch), h.max(n.pitch)));
            let (l, h) = (l.saturating_sub(3), h.saturating_add(3).min(127));
            if h - l < 12 {
                (l.saturating_sub((12 - (h - l)) / 2), (h + 6).min(127))
            } else {
                (l, h)
            }
        };
        let (full, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
        let painter = ui.painter_at(full);
        painter.rect_filled(full, 0.0, tk.panel_bg);
        // ---- header: title, hint, snap selector, ops ----
        let header = Rect::from_min_size(full.min, vec2(full.width(), HEADER_H));
        painter.rect_filled(header, 0.0, tk.panel_bg2);
        painter.text(
            pos2(header.min.x + 8.0, header.center().y),
            Align2::LEFT_CENTER,
            "PIANO ROLL",
            FontId::proportional(10.5),
            tk.header_text,
        );
        painter.text(
            pos2(header.min.x + 84.0, header.center().y),
            Align2::LEFT_CENTER,
            format!(
                "{} · {} · {} notes · {} sel",
                t.name,
                pat.name,
                notes.len(),
                self.piano.selected.len()
            ),
            FontId::proportional(10.0),
            tk.text_dim,
        );
        let bar = Rect::from_min_max(
            pos2(header.max.x - 290.0, header.min.y + 1.0),
            pos2(header.max.x - 4.0, header.max.y - 1.0),
        );
        let mut tb = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bar)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        tb.spacing_mut().item_spacing.x = 3.0;
        let sel_or_all = |sel: &[usize]| -> Value {
            if sel.is_empty() {
                json!({})
            } else {
                // select the chosen notes' pitches over their step range
                let ns: Vec<&Note> = sel.iter().filter_map(|i| notes.get(*i)).collect();
                let from = ns.iter().map(|n| n.start).fold(f32::MAX, f32::min);
                let to = ns.iter().map(|n| n.start).fold(0.0f32, f32::max) + 0.001;
                let pitches: Vec<u8> = ns.iter().map(|n| n.pitch).collect();
                json!({"from": from, "to": to, "pitches": pitches})
            }
        };
        let mut op: Option<(&str, Value)> = None;
        if tb.small_button("Legato").clicked() {
            op = Some(("legato", json!({})));
        }
        if tb
            .small_button("Vel-")
            .on_hover_text("velocity -10%")
            .clicked()
        {
            op = Some(("edit_notes", json!({"vel_add": -0.1})));
        }
        if tb
            .small_button("Vel+")
            .on_hover_text("velocity +10%")
            .clicked()
        {
            op = Some(("edit_notes", json!({"vel_add": 0.1})));
        }
        let g = snap_steps(self.piano.snap);
        if tb
            .small_button("Quantize")
            .on_hover_text("Quantize starts to the snap grid")
            .clicked()
        {
            op = Some(("quantize", json!({"grid": g, "strength": 1.0})));
        }
        egui::ComboBox::from_id_salt(("snap", self.selected))
            .width(52.0)
            .selected_text(format!(
                "snap {}",
                SNAPS[self.piano.snap.min(SNAPS.len() - 1)].0
            ))
            .show_ui(&mut tb, |ui| {
                for (i, (label, _)) in SNAPS.iter().enumerate() {
                    ui.selectable_value(&mut self.piano.snap, i, *label);
                }
            });
        if let Some((id, mut a)) = op {
            if let (Some(obj), Value::Object(sel)) =
                (a.as_object_mut(), sel_or_all(&self.piano.selected))
            {
                obj.extend(sel);
                obj.insert("track".into(), json!(tname));
                obj.insert("pattern".into(), json!(pname));
            }
            self.call(id, a);
        }
        // ---- geometry ----
        let roll = Rect::from_min_max(
            pos2(full.min.x + KEY_W, header.max.y + 2.0),
            pos2(full.max.x - 4.0, full.max.y - VEL_H - 4.0),
        );
        let keys = Rect::from_min_max(pos2(full.min.x, roll.min.y), pos2(roll.min.x, roll.max.y));
        let vel = Rect::from_min_max(
            pos2(roll.min.x, roll.max.y + 4.0),
            pos2(roll.max.x, full.max.y - 2.0),
        );
        let rows = f32::from(hi - lo + 1);
        let row_h = roll.height() / rows;
        let cell = roll.width() / steps.max(1.0);
        let x_of = |s: f32| roll.min.x + s * cell;
        let y_of = |pitch: i32| roll.min.y + (i32::from(hi) - pitch) as f32 * row_h;
        let pp = painter.with_clip_rect(roll.union(keys));
        for i in 0..=(hi - lo) {
            let pitch = hi - i;
            let y = roll.min.y + f32::from(i) * row_h;
            let black = matches!(pitch % 12, 1 | 3 | 6 | 8 | 10);
            pp.rect_filled(
                Rect::from_min_size(pos2(roll.min.x, y), vec2(roll.width(), row_h)),
                0.0,
                if black {
                    Color32::from_rgb(30, 30, 32)
                } else {
                    Color32::from_rgb(40, 40, 42)
                },
            );
            let key =
                Rect::from_min_size(pos2(keys.min.x + 2.0, y), vec2(KEY_W - 4.0, row_h - 0.5));
            pp.rect_filled(
                key,
                1.0,
                if black {
                    Color32::from_rgb(20, 20, 20)
                } else {
                    Color32::from_rgb(225, 225, 225)
                },
            );
            if pitch % 12 == 0 {
                pp.line_segment(
                    [pos2(roll.min.x, y + row_h), pos2(roll.max.x, y + row_h)],
                    Stroke::new(1.0_f32, Color32::from_rgb(64, 64, 68)),
                );
                if row_h > 6.0 {
                    pp.text(
                        pos2(key.max.x - 2.0, key.center().y),
                        Align2::RIGHT_CENTER,
                        theory::note_name(pitch),
                        FontId::proportional((row_h * 0.85).clamp(6.0, 9.0)),
                        Color32::from_rgb(40, 40, 40),
                    );
                }
            }
        }
        // grid lines at the snap value; beats and bars stronger
        let gs = g.max(0.25);
        let mut s = 0.0;
        while s <= steps + 0.001 {
            let bar_line = (s % 16.0).abs() < 1e-3;
            let beat = (s % 4.0).abs() < 1e-3;
            if gs * cell > 4.0 || beat {
                pp.line_segment(
                    [pos2(x_of(s), roll.min.y), pos2(x_of(s), roll.max.y)],
                    Stroke::new(
                        1.0_f32,
                        if bar_line {
                            Color32::from_rgb(90, 90, 96)
                        } else if beat {
                            Color32::from_rgb(64, 64, 68)
                        } else {
                            Color32::from_rgb(48, 48, 52)
                        },
                    ),
                );
            }
            s += gs;
        }
        // notes (drawn with the in-progress drag applied)
        let shown = match &self.piano.drag {
            Some(d) => apply_shift(&notes, &self.piano.selected, &d.shift, steps, g),
            None => notes.clone(),
        };
        let note_rect = |n: &Note| {
            Rect::from_min_max(
                pos2(x_of(n.start), y_of(i32::from(n.pitch)) + 0.5),
                pos2(
                    x_of(n.start + n.len).max(x_of(n.start) + 3.0),
                    y_of(i32::from(n.pitch)) + row_h - 0.5,
                ),
            )
        };
        for (i, n) in shown.iter().enumerate() {
            if n.pitch < lo || n.pitch > hi {
                continue;
            }
            let r = note_rect(n);
            let sel = self.piano.selected.contains(&i);
            let k = 0.45 + 0.55 * n.vel.clamp(0.0, 1.0);
            let fill = if sel {
                Color32::WHITE
            } else {
                Color32::from_rgb(
                    (f32::from(color.r()) * k) as u8,
                    (f32::from(color.g()) * k) as u8,
                    (f32::from(color.b()) * k) as u8,
                )
            };
            pp.rect(r, 2.0, fill, Stroke::new(1.0_f32, Color32::BLACK));
        }
        // playhead
        if let Some(ps) = super::locate(p, self.player.position_secs())
            .filter(|(pi, _)| *pi == self.pattern)
            .map(|(_, s)| s)
        {
            pp.line_segment(
                [pos2(x_of(ps), roll.min.y), pos2(x_of(ps), roll.max.y)],
                Stroke::new(1.5_f32, tk.playhead),
            );
        }
        // ---- interaction ----
        let resp = ui.interact(
            roll,
            ui.id().with(("roll", self.selected)),
            Sense::click_and_drag(),
        );
        let hit = |pos: Pos2| -> Option<(usize, bool)> {
            notes.iter().enumerate().rev().find_map(|(i, n)| {
                let r = note_rect(n).expand2(vec2(0.0, 0.5));
                r.contains(pos).then_some((i, pos.x > r.max.x - 5.0))
            })
        };
        if resp.drag_started() {
            if let Some(pt) = resp.interact_pointer_pos() {
                match hit(pt) {
                    Some((i, edge)) => {
                        if !self.piano.selected.contains(&i) {
                            self.piano.selected = vec![i];
                        }
                        self.piano.drag = Some(NoteDrag {
                            start: pt,
                            shift: NoteShift {
                                index: i,
                                resize: edge,
                                d_steps: 0.0,
                                d_semi: 0,
                            },
                        });
                    }
                    None => self.piano.band = Some(pt),
                }
                self.dragging = true;
            }
        }
        if resp.dragged() {
            if let (Some(d), Some(pt)) = (&mut self.piano.drag, resp.interact_pointer_pos()) {
                d.shift.d_steps = snap_to((pt.x - d.start.x) / cell, g);
                d.shift.d_semi = if d.shift.resize {
                    0
                } else {
                    -((pt.y - d.start.y) / row_h).round() as i32
                };
            }
        }
        if let (Some(a), Some(b)) = (self.piano.band, ui.ctx().pointer_latest_pos()) {
            let band = Rect::from_two_pos(a, b);
            painter.rect(
                band,
                0.0,
                Color32::from_rgba_unmultiplied(120, 170, 255, 40),
                Stroke::new(1.0_f32, Color32::from_rgb(120, 170, 255)),
            );
            if resp.drag_stopped() {
                self.piano.selected = notes
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| note_rect(n).intersects(band))
                    .map(|(i, _)| i)
                    .collect();
                self.piano.band = None;
                self.dragging = false;
            }
        }
        if resp.drag_stopped() {
            if let Some(d) = self.piano.drag.take() {
                if d.shift.d_steps.abs() > 1e-4 || d.shift.d_semi != 0 {
                    let moved = apply_shift(&notes, &self.piano.selected, &d.shift, steps, g);
                    self.write_notes(&tname, &pname, &moved);
                    self.piano.selected.clear();
                }
            }
            self.dragging = false;
        }
        if resp.clicked() {
            if let Some(pt) = resp.interact_pointer_pos() {
                match hit(pt) {
                    Some((i, _)) => {
                        if ui.input(|x| x.modifiers.shift) {
                            if let Some(k) = self.piano.selected.iter().position(|s| *s == i) {
                                self.piano.selected.remove(k);
                            } else {
                                self.piano.selected.push(i);
                            }
                        } else {
                            self.piano.selected = vec![i];
                        }
                    }
                    None => {
                        let pitch = (i32::from(hi) - ((pt.y - roll.min.y) / row_h).floor() as i32)
                            .clamp(0, 127);
                        let start = ((pt.x - roll.min.x) / cell / g).floor() * g;
                        let len = if is_drum { 1.0 } else { g.max(1.0) * 2.0 };
                        self.call(
                            "add_notes",
                            json!({"track": tname, "pattern": pname, "notes": [{"start": start.max(0.0), "len": len, "pitch": pitch, "vel": 0.8}]}),
                        );
                        self.piano.selected.clear();
                    }
                }
            }
        }
        if resp.secondary_clicked() {
            if let Some((i, _)) = resp.interact_pointer_pos().and_then(hit) {
                let mut rest = notes.clone();
                rest.remove(i);
                self.write_notes(&tname, &pname, &rest);
                self.piano.selected.clear();
            }
        }
        // keyboard while hovering: Cmd+A, arrows (Shift = octave / bar), Delete
        if ui.rect_contains_pointer(full) && !ui.ctx().wants_keyboard_input() {
            let (all, up, down, left, right, shift, del) = ui.input(|i| {
                (
                    i.modifiers.command && i.key_pressed(egui::Key::A),
                    i.key_pressed(egui::Key::ArrowUp) && !i.modifiers.alt,
                    i.key_pressed(egui::Key::ArrowDown) && !i.modifiers.alt,
                    i.key_pressed(egui::Key::ArrowLeft),
                    i.key_pressed(egui::Key::ArrowRight),
                    i.modifiers.shift,
                    i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace),
                )
            });
            if all {
                self.piano.selected = (0..notes.len()).collect();
            }
            let semi = (i32::from(up) - i32::from(down)) * if shift { 12 } else { 1 };
            let dx = (f32::from(u8::from(right)) - f32::from(u8::from(left)))
                * if shift { 16.0 } else { g };
            if (semi != 0 || dx != 0.0) && !self.piano.selected.is_empty() {
                let d = NoteShift {
                    index: self.piano.selected[0],
                    resize: false,
                    d_steps: dx,
                    d_semi: semi,
                };
                let moved = apply_shift(&notes, &self.piano.selected, &d, steps, g);
                self.write_notes(&tname, &pname, &moved);
                // keep the selection: notes stay in the same order unless they cross
                self.piano.selected.clear();
            }
            if del && !self.piano.selected.is_empty() {
                let rest: Vec<Note> = notes
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !self.piano.selected.contains(i))
                    .map(|(_, n)| n.clone())
                    .collect();
                self.write_notes(&tname, &pname, &rest);
                self.piano.selected.clear();
            }
        }
        // ---- velocity lane ----
        painter.rect_filled(vel, 0.0, Color32::from_rgb(28, 28, 30));
        painter.text(
            pos2(full.min.x + 4.0, vel.center().y),
            Align2::LEFT_CENTER,
            "VEL",
            FontId::proportional(9.0),
            tk.text_dim,
        );
        let vid = ui.id().with(("vel", self.selected, self.pattern));
        let vresp = ui.interact(vel, vid, Sense::click_and_drag());
        // live velocity edit kept in egui memory until the drag ends (one undo step)
        let mut live: Option<(usize, f32)> = ui.ctx().data(|d| d.get_temp(vid));
        if vresp.clicked() || vresp.dragged() {
            if let Some(pt) = vresp.interact_pointer_pos() {
                let near = notes
                    .iter()
                    .enumerate()
                    .map(|(i, n)| (i, (x_of(n.start) - pt.x).abs()))
                    .filter(|(_, dx)| *dx < 8.0)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                let target = live.map(|l| l.0).or(near.map(|n| n.0));
                if let Some(i) = target {
                    let v = ((vel.max.y - 2.0 - pt.y) / (vel.height() - 4.0)).clamp(0.02, 1.0);
                    live = Some((i, v));
                    ui.ctx().data_mut(|d| d.insert_temp(vid, (i, v)));
                    self.dragging = true;
                }
            }
        }
        for (i, n) in notes.iter().enumerate() {
            let v = match live {
                Some((li, lv)) if li == i => lv,
                _ => n.vel,
            };
            let x = x_of(n.start) + 1.0;
            let h = (vel.height() - 4.0) * v.clamp(0.0, 1.0);
            let col = if self.piano.selected.contains(&i) {
                Color32::WHITE
            } else {
                color
            };
            painter.line_segment(
                [pos2(x, vel.max.y - 2.0), pos2(x, vel.max.y - 2.0 - h)],
                Stroke::new(3.0_f32, col),
            );
            painter.circle_filled(pos2(x, vel.max.y - 2.0 - h), 2.5, col);
        }
        if vresp.drag_stopped() || (vresp.clicked() && !vresp.dragged()) {
            if let Some((i, v)) = live {
                let mut out = notes.clone();
                if let Some(n) = out.get_mut(i) {
                    n.vel = v;
                }
                self.write_notes(&tname, &pname, &out);
            }
            ui.ctx().data_mut(|d| d.remove::<(usize, f32)>(vid));
            self.dragging = false;
        }
    }
}
