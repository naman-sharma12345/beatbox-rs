// Adapted from SoundCraft `crates/ui-egui/src/widgets.rs` (commit eac0edd) and the fader law
// in `crates/model/src/mixer.rs` (`fader_pos_to_db`, `fader_db_to_pos`).
// Copyright (c) 2026 ArtCraft Team and the SoundCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your option;
// used in Beatbox under the MIT License. See THIRD_PARTY_NOTICES.md.

//! Console-style widgets: square S/M toggles, selector boxes, a pan knob with a
//! centre-out arc, zoned peak meters with hold and clip LEDs, and a console fader
//! with a capped handle and a dB scale. Ported to egui 0.29 (`Rounding`, no
//! `StrokeKind`, no let-chains) and to Beatbox's dB-in / dB-out conventions.

use super::theme::{bold, regular, Tokens};
use crate::console_law::{
    fader_db_to_pos, fader_pos_to_db, hw_meter_pos, FADER_MAX_DB, FADER_MIN_DB,
};
use eframe::egui::{self, pos2, vec2, Align2, Color32, Rect, Response, Sense, Stroke, Ui};

/// Square text toggle used for S / M / R etc. `on` fills with `on_color`.
pub fn text_toggle(
    ui: &mut Ui,
    size: egui::Vec2,
    text: &str,
    on: bool,
    on_color: Color32,
    tip: &str,
) -> Response {
    let t = Tokens::DARK;
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if on {
        on_color
    } else if resp.hovered() {
        t.button_hi
    } else {
        t.button
    };
    ui.painter().rect(
        r,
        egui::Rounding::same(2.0),
        fill,
        Stroke::new(1.0_f32, t.button_border),
    );
    let col = if on { t.text_dark } else { t.text };
    ui.painter().text(
        r.center(),
        Align2::CENTER_CENTER,
        text,
        bold((size.y * 0.62).clamp(8.0, 13.0)),
        col,
    );
    resp.on_hover_text(tip)
}

/// A dropdown-looking box with a down triangle; the caller opens a menu on click.
pub fn selector_box(ui: &mut Ui, width: f32, height: f32, text: &str, color: Color32) -> Response {
    let t = Tokens::DARK;
    let (r, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    ui.painter().rect(
        r,
        egui::Rounding::same(2.0),
        if resp.hovered() {
            t.button_hi
        } else {
            t.button
        },
        Stroke::new(1.0_f32, t.button_border),
    );
    // down triangle (drawn, no icon font)
    let s = height * 0.22;
    let c = pos2(r.max.x - height * 0.45, r.center().y);
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            pos2(c.x - s, c.y - s * 0.5),
            pos2(c.x + s, c.y - s * 0.5),
            pos2(c.x, c.y + s * 0.7),
        ],
        t.text_dim,
        Stroke::NONE,
    ));
    let font = regular((height * 0.62).clamp(8.0, 12.0));
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
    let avail = (width - height * 0.9).max(10.0);
    let x = r.min.x + ((avail - galley.size().x) * 0.5).max(3.0);
    let pos = pos2(x, r.center().y - galley.size().y * 0.5);
    ui.painter()
        .with_clip_rect(Rect::from_min_max(r.min, pos2(r.min.x + avail, r.max.y)).shrink(1.0))
        .galley(pos, galley, color);
    resp
}

/// Rotary pan knob (-1..1). Drag (up/right = right); double-click centres.
/// Returns (response, changed).
pub fn pan_knob(ui: &mut Ui, size: f32, value: &mut f32, tip: &str) -> (Response, bool) {
    let t = Tokens::DARK;
    let (r, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click_and_drag());
    let before = *value;
    if resp.dragged() {
        let d = resp.drag_delta();
        *value = (*value + (d.x - d.y) * 0.01).clamp(-1.0, 1.0);
    }
    if resp.double_clicked() {
        *value = 0.0;
    }
    let c = r.center();
    let rad = size * 0.40;
    ui.painter().circle(
        c,
        rad,
        Color32::from_rgb(26, 26, 28),
        Stroke::new(1.0_f32, Color32::from_rgb(90, 90, 94)),
    );
    let n = 24;
    let start = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    let arc = |a0: f32, a1: f32| -> Vec<egui::Pos2> {
        (0..=n)
            .map(|i| {
                let a = a0 + (a1 - a0) * i as f32 / n as f32;
                c + vec2(a.cos(), a.sin()) * (rad + 2.5)
            })
            .collect()
    };
    let mid = start + sweep * 0.5;
    let at = start + sweep * (*value * 0.5 + 0.5);
    let (a0, a1) = if at < mid { (at, mid) } else { (mid, at) };
    ui.painter().add(egui::Shape::line(
        arc(a0, a1),
        Stroke::new(2.0_f32, t.counter_text),
    ));
    let tip_pos = c + vec2(at.cos(), at.sin()) * rad * 0.85;
    ui.painter().line_segment(
        [c, tip_pos],
        Stroke::new(2.0_f32, Color32::from_rgb(230, 230, 230)),
    );
    let changed = (*value - before).abs() > f32::EPSILON;
    (resp.on_hover_text(tip), changed)
}

/// Vertical peak meter with green / yellow / red zones, a peak-hold line and a clip LED.
/// Levels are in dBFS.
pub fn zoned_meter(p: &egui::Painter, r: Rect, level_db: f32, hold_db: f32, clip: bool) {
    let t = Tokens::DARK;
    p.rect_filled(r, 0.0, Color32::from_rgb(8, 8, 8));
    let inner = Rect::from_min_max(
        pos2(r.min.x + 1.0, r.min.y + 4.0),
        pos2(r.max.x - 1.0, r.max.y - 1.0),
    );
    let h = inner.height();
    let lv = hw_meter_pos(level_db);
    if lv > 0.0 {
        let top = inner.max.y - h * lv;
        let zones = [
            (0.0, hw_meter_pos(-12.0), t.meter_green),
            (hw_meter_pos(-12.0), hw_meter_pos(-3.0), t.meter_yellow),
            (hw_meter_pos(-3.0), 1.0, t.meter_red),
        ];
        for (a, b, col) in zones {
            let y0 = inner.max.y - h * a;
            let y1 = (inner.max.y - h * b).max(top);
            if y1 < y0 {
                p.rect_filled(
                    Rect::from_min_max(pos2(inner.min.x, y1), pos2(inner.max.x, y0)),
                    0.0,
                    col,
                );
            }
        }
    }
    let hv = hw_meter_pos(hold_db);
    if hv > 0.01 {
        let y = inner.max.y - h * hv;
        p.line_segment(
            [pos2(inner.min.x, y), pos2(inner.max.x, y)],
            Stroke::new(
                1.0_f32,
                if hv > hw_meter_pos(-3.0) {
                    t.meter_red
                } else {
                    t.meter_yellow
                },
            ),
        );
    }
    let clip_r = Rect::from_min_max(r.min, pos2(r.max.x, r.min.y + 3.0));
    p.rect_filled(
        clip_r,
        0.0,
        if clip {
            t.meter_red
        } else {
            Color32::from_rgb(60, 20, 20)
        },
    );
}

/// dB scale ticks next to a meter (0, -3, -6, -12, -20, -40).
pub fn meter_scale(p: &egui::Painter, r: Rect) {
    let t = Tokens::DARK;
    let inner_h = r.height() - 5.0;
    for db in [0.0f32, -3.0, -6.0, -12.0, -20.0, -40.0] {
        let y = r.max.y - 1.0 - inner_h * hw_meter_pos(db);
        p.text(
            pos2(r.center().x, y),
            Align2::CENTER_CENTER,
            format!("{}", db.abs()),
            regular(7.5),
            t.text_dim,
        );
    }
}

/// A console fader with a capped handle and a dB scale. `db` in -144..+12.
/// Drag moves (Ctrl/Cmd = fine), double-click or Alt-click resets to 0 dB.
/// Returns (response, committed): committed is true when a drag ends or a reset happens.
pub fn console_fader(ui: &mut Ui, r: Rect, id: egui::Id, db: &mut f32) -> (Response, bool) {
    let t = Tokens::DARK;
    let resp = ui.interact(r, id, Sense::click_and_drag());
    let slot = Rect::from_center_size(
        pos2(r.max.x - r.width() * 0.32, r.center().y),
        vec2(5.0, r.height() - 14.0),
    );
    let p = ui.painter();
    p.rect_filled(slot, 2.0, t.fader_track);
    for mark in [
        12.0f32, 6.0, 0.0, -5.0, -10.0, -15.0, -20.0, -30.0, -40.0, -60.0,
    ] {
        let y = slot.max.y - slot.height() * fader_db_to_pos(mark);
        p.line_segment(
            [pos2(r.min.x + 12.0, y), pos2(slot.min.x - 3.0, y)],
            Stroke::new(1.0_f32, Color32::from_rgb(90, 90, 92)),
        );
        p.text(
            pos2(r.min.x, y),
            Align2::LEFT_CENTER,
            format!("{}", mark.abs()),
            regular(7.5),
            Color32::from_rgb(150, 150, 150),
        );
    }
    let mut committed = false;
    if resp.double_clicked() || (resp.clicked() && ui.input(|i| i.modifiers.alt)) {
        *db = 0.0;
        committed = true;
    } else if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
        let dy = resp.drag_delta().y / slot.height().max(1.0) * if fine { 0.1 } else { 1.0 };
        let np = (fader_db_to_pos(*db) - dy).clamp(0.0, 1.0);
        *db = fader_pos_to_db(np).clamp(FADER_MIN_DB, FADER_MAX_DB);
    }
    if resp.drag_stopped() {
        committed = true;
    }
    let y = slot.max.y - slot.height() * fader_db_to_pos(*db);
    let cap_w = (r.width() * 0.62).min(30.0);
    let cap = Rect::from_center_size(pos2(slot.center().x, y), vec2(cap_w, 22.0));
    let hot = resp.hovered() || resp.dragged();
    p.rect(
        cap,
        2.0,
        if hot {
            Color32::from_rgb(88, 88, 90)
        } else {
            Color32::from_rgb(70, 70, 72)
        },
        Stroke::new(1.0_f32, Color32::from_rgb(20, 20, 20)),
    );
    p.line_segment(
        [
            pos2(cap.min.x + 2.0, cap.center().y),
            pos2(cap.max.x - 2.0, cap.center().y),
        ],
        Stroke::new(2.0_f32, Color32::from_rgb(230, 230, 230)),
    );
    for dy in [-6.0, -3.0, 3.0, 6.0] {
        p.line_segment(
            [
                pos2(cap.min.x + 3.0, cap.center().y + dy),
                pos2(cap.max.x - 3.0, cap.center().y + dy),
            ],
            Stroke::new(1.0_f32, Color32::from_rgb(40, 40, 40)),
        );
    }
    (resp, committed)
}

/// A black LCD-style counter box with green digits (transport, fader readouts).
pub fn counter_box(p: &egui::Painter, r: Rect, text: &str, size: f32) {
    let t = Tokens::DARK;
    p.rect(
        r,
        2.0,
        t.counter_bg,
        Stroke::new(1.0_f32, t.border_light.gamma_multiply(0.6)),
    );
    p.text(
        r.center(),
        Align2::CENTER_CENTER,
        text,
        super::theme::mono(size),
        t.counter_text,
    );
}

/// The light name plate at the bottom of a strip.
pub fn name_plate(p: &egui::Painter, r: Rect, name: &str, selected: bool) {
    let t = Tokens::DARK;
    p.rect(
        r,
        2.0,
        if selected {
            t.name_field_sel
        } else {
            t.name_field
        },
        Stroke::new(1.0_f32, Color32::BLACK),
    );
    p.with_clip_rect(r).text(
        r.center(),
        Align2::CENTER_CENTER,
        name,
        bold(11.5),
        t.text_dark,
    );
}
