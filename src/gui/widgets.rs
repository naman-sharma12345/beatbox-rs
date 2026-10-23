//! Custom-painted widgets and the studio's visual theme.

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2,
};
use std::f32::consts::PI;

// The studio palette, mapped onto the ported SoundCraft design tokens (see theme.rs):
// warm charcoal surfaces, signal red and cream counters.
use super::theme::Tokens;
pub const BG: Color32 = Color32::from_rgb(20, 17, 13);
pub const PANEL: Color32 = Tokens::DARK.panel_bg;
pub const PANEL2: Color32 = Tokens::DARK.panel_bg2;
pub const LINE: Color32 = Tokens::DARK.border;
pub const TEXT: Color32 = Tokens::DARK.text;
pub const DIM: Color32 = Tokens::DARK.text_dim;
pub const ACCENT: Color32 = Tokens::DARK.accent;
pub const ACCENT2: Color32 = Tokens::DARK.counter_text;
pub const GOOD: Color32 = Tokens::DARK.meter_green;
pub const WARN: Color32 = Tokens::DARK.mute;
pub const HOT: Color32 = Tokens::DARK.rec;
pub const SOLO: Color32 = Tokens::DARK.solo;

pub fn apply_theme(ctx: &egui::Context) {
    super::theme::apply(ctx);
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// Track color from its name/instrument.
pub fn track_color(name: &str, kind: &str) -> Color32 {
    let n = name.to_lowercase();
    if n.contains("kick") {
        Color32::from_rgb(237, 67, 43)
    } else if n.contains("snare") || n.contains("clap") {
        Color32::from_rgb(218, 163, 65)
    } else if n.contains("hat") || n.contains("shaker") || n.contains("crash") {
        Color32::from_rgb(211, 195, 147)
    } else if n.contains("rim") || n.contains("tom") || n.contains("cowbell") || n.contains("perc")
    {
        Color32::from_rgb(79, 162, 150)
    } else if n.contains("bass") || n.contains("808") || n.contains("sub") {
        Color32::from_rgb(202, 142, 50)
    } else if n.contains("chord") || n.contains("pad") || n.contains("key") {
        Color32::from_rgb(214, 201, 166)
    } else if n.contains("lead") || n.contains("melody") || n.contains("bell") {
        Color32::from_rgb(192, 91, 61)
    } else {
        match kind {
            "sampler" => Color32::from_rgb(218, 163, 65),
            "drum" => Color32::from_rgb(79, 162, 150),
            "fm" => Color32::from_rgb(79, 162, 150),
            _ => Color32::from_rgb(214, 201, 166),
        }
    }
}

pub fn section_label(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(text)
            .size(11.0)
            .color(DIM)
            .strong()
            .extra_letter_spacing(1.2),
    );
}

/// A card frame.
pub fn card() -> egui::Frame {
    egui::Frame::none()
        .fill(PANEL2)
        .rounding(egui::Rounding::same(4.0))
        .stroke(Stroke::new(1.0_f32, Tokens::DARK.border))
        .inner_margin(egui::Margin::same(10.0))
}

/// Rotary knob. Returns (response, committed) where committed = drag ended or value set by double-click.
pub fn knob(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    min: f32,
    max: f32,
    log: bool,
    color: Color32,
) -> (Response, bool) {
    let size = Vec2::new(64.0, 84.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let to_norm = |v: f32| -> f32 {
        if log && min > 0.0 {
            ((v.max(min)).ln() - min.ln()) / (max.ln() - min.ln())
        } else {
            (v - min) / (max - min)
        }
        .clamp(0.0, 1.0)
    };
    let from_norm = |t: f32| -> f32 {
        if log && min > 0.0 {
            (min.ln() + t * (max.ln() - min.ln())).exp()
        } else {
            min + t * (max - min)
        }
    };
    let mut t = to_norm(*value);
    if resp.dragged() {
        let d = -resp.drag_delta().y + resp.drag_delta().x * 0.5;
        let fine = if ui.input(|i| i.modifiers.shift) {
            0.2
        } else {
            1.0
        };
        t = (t + d * 0.005 * fine).clamp(0.0, 1.0);
        *value = from_norm(t);
    }
    let committed = resp.drag_stopped();
    let painter = ui.painter_at(rect);
    let c = Pos2::new(rect.center().x, rect.top() + 28.0);
    let r = 22.0;
    let a0 = PI * 0.75;
    let a1 = PI * 2.25;
    let arc = |from: f32, to: f32, rad: f32| -> Vec<Pos2> {
        let n = 32;
        (0..=n)
            .map(|i| {
                let a = from + (to - from) * i as f32 / n as f32;
                Pos2::new(c.x + rad * a.cos(), c.y + rad * a.sin())
            })
            .collect()
    };
    let hover = resp.hovered() || resp.dragged();
    painter.circle_filled(
        c,
        r - 3.0,
        if hover {
            Color32::from_rgb(62, 51, 37)
        } else {
            Color32::from_rgb(36, 30, 23)
        },
    );
    painter.add(Shape::line(
        arc(a0, a1, r),
        Stroke::new(3.5_f32, Color32::from_rgb(72, 59, 43)),
    ));
    if t > 0.001 {
        painter.add(Shape::line(
            arc(a0, a0 + (a1 - a0) * t, r),
            Stroke::new(3.5_f32, color),
        ));
    }
    let a = a0 + (a1 - a0) * t;
    painter.line_segment(
        [
            Pos2::new(c.x + (r - 12.0) * a.cos(), c.y + (r - 12.0) * a.sin()),
            Pos2::new(c.x + (r - 4.0) * a.cos(), c.y + (r - 4.0) * a.sin()),
        ],
        Stroke::new(2.5_f32, TEXT),
    );
    for i in 0..=10 {
        let a = a0 + (a1 - a0) * i as f32 / 10.0;
        painter.line_segment(
            [
                c + Vec2::angled(a) * (r + 4.0),
                c + Vec2::angled(a) * (r + 7.0),
            ],
            Stroke::new(1.0_f32, if (i as f32 / 10.0) <= t { color } else { LINE }),
        );
    }
    painter.text(
        Pos2::new(rect.center().x, rect.bottom() - 24.0),
        Align2::CENTER_CENTER,
        short(label),
        FontId::monospace(10.5),
        DIM,
    );
    painter.text(
        Pos2::new(rect.center().x, rect.bottom() - 9.0),
        Align2::CENTER_CENTER,
        format_value(*value),
        FontId::monospace(11.0),
        TEXT,
    );
    let resp = resp.on_hover_text(format!("{label}: {}", format_value(*value)));
    (resp, committed)
}

pub fn format_value(v: f32) -> String {
    let a = v.abs();
    if a >= 1000.0 {
        format!("{:.1}k", v / 1000.0)
    } else if a >= 100.0 {
        format!("{v:.0}")
    } else if a >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

fn short(label: &str) -> String {
    let l = label.replace('_', " ");
    if l.len() > 9 {
        l.split(' ')
            .map(|w| w.chars().take(4).collect::<String>())
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(10)
            .collect()
    } else {
        l
    }
}

/// Big round transport button with a play / stop glyph.
pub fn transport_button(ui: &mut Ui, playing: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(38.0), Sense::click());
    let c = rect.center();
    let col = if playing { HOT } else { ACCENT };
    let fill = if resp.hovered() {
        col
    } else {
        col.gamma_multiply(0.85)
    };
    ui.painter().circle_filled(c, 18.0, fill);
    ui.painter().circle_stroke(
        c,
        18.0,
        Stroke::new(1.0_f32, Color32::WHITE.gamma_multiply(0.25)),
    );
    if playing {
        ui.painter().rect_filled(
            Rect::from_center_size(c, Vec2::splat(12.0)),
            2.0,
            Color32::WHITE,
        );
    } else {
        let p = vec![
            Pos2::new(c.x - 5.0, c.y - 8.0),
            Pos2::new(c.x - 5.0, c.y + 8.0),
            Pos2::new(c.x + 9.0, c.y),
        ];
        ui.painter()
            .add(Shape::convex_polygon(p, Color32::WHITE, Stroke::NONE));
    }
    resp
}

/// Record button: a red dot; while a take is running the dot turns into a
/// solid red disc with a white ring.
pub fn record_button(ui: &mut Ui, recording: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    let c = rect.center();
    let p = ui.painter();
    let base = if resp.hovered() {
        HOT
    } else {
        HOT.gamma_multiply(0.85)
    };
    if recording {
        p.circle_filled(c, 14.0, base);
        p.circle_stroke(c, 9.0, Stroke::new(2.0_f32, Color32::WHITE));
    } else {
        p.circle_stroke(c, 14.0, Stroke::new(1.5_f32, base));
        p.circle_filled(c, 7.0, base);
    }
    resp
}

/// Gradient-ish filled bar used in meters/spectrum.
pub fn vgradient_bar(p: &egui::Painter, rect: Rect, bottom: Color32, top: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.left_top(), top);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(Shape::mesh(mesh));
}

/// Segmented tab control. Returns the index clicked this frame, if any.
#[allow(dead_code)]
pub fn tabs(ui: &mut Ui, labels: &[&str], current: usize) -> Option<usize> {
    let mut clicked = None;
    let h = 28.0;
    let widths: Vec<f32> = labels.iter().map(|l| 20.0 + l.len() as f32 * 7.0).collect();
    let total: f32 = widths.iter().sum::<f32>() + 6.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(total, h), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 3.0, Tokens::DARK.panel_bg);
    p.rect_stroke(rect, 3.0, Stroke::new(1.0_f32, Tokens::DARK.border));
    let mut x = rect.left() + 3.0;
    for (i, (l, w)) in labels.iter().zip(widths.iter()).enumerate() {
        let r = Rect::from_min_size(Pos2::new(x, rect.top() + 3.0), Vec2::new(*w, h - 6.0));
        let resp = ui.interact(r, ui.id().with(("tab", i, *l)), Sense::click());
        let on = i == current;
        if on {
            p.rect_filled(r, 4.0, Tokens::DARK.button_hi);
        } else if resp.hovered() {
            p.rect_filled(r, 2.0, Tokens::DARK.button);
        }
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            *l,
            FontId::proportional(12.5),
            if on { TEXT } else { DIM },
        );
        if resp.clicked() {
            clicked = Some(i);
        }
        x += w;
    }
    clicked
}

/// Small rounded stat chip: dim label + value.
pub fn stat_chip(ui: &mut Ui, label: &str, value: &str, color: Color32) {
    egui::Frame::none()
        .fill(BG)
        .rounding(2.0)
        .stroke(Stroke::new(1.0_f32, Tokens::DARK.border))
        .inner_margin(egui::Margin::symmetric(8.0, 3.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.label(egui::RichText::new(label).size(9.5).color(DIM).strong());
                ui.label(
                    egui::RichText::new(value)
                        .size(12.0)
                        .color(color)
                        .monospace(),
                );
            });
        });
}

/// A small FL-style rack knob painted into `rect`: drag up/down to turn,
/// double-click to reset. Returns the final value once (on release or
/// reset), so one gesture is one undo step; while dragging the knob shows
/// the working value. `bipolar` draws the arc from the centre (pan).
#[allow(clippy::too_many_arguments)]
pub fn mini_knob(
    ui: &mut Ui,
    rect: Rect,
    id: egui::Id,
    value: f32,
    min: f32,
    max: f32,
    default: f32,
    bipolar: bool,
    color: Color32,
    tip: &str,
) -> Option<f32> {
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let mut v: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(value);
    let mut done = None;
    if resp.dragged() {
        let fine = if ui.input(|i| i.modifiers.shift) {
            0.2
        } else {
            1.0
        };
        v = (v - resp.drag_delta().y / 140.0 * (max - min) * fine).clamp(min, max);
        ui.data_mut(|d| d.insert_temp(id, v));
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove::<f32>(id));
        done = Some(v);
    }
    if resp.double_clicked() {
        ui.data_mut(|d| d.remove::<f32>(id));
        v = default;
        done = Some(default);
    }
    let resp = resp.on_hover_text(format!("{tip}: {}", format_value(v)));
    let p = ui.painter_at(rect.expand(2.0));
    let c = rect.center();
    let r = rect.width().min(rect.height()) * 0.5 - 1.5;
    let hot = resp.hovered() || resp.dragged();
    p.circle_filled(c, r, Color32::from_rgb(27, 23, 18));
    p.circle_stroke(
        c,
        r,
        Stroke::new(
            1.0_f32,
            if hot {
                Color32::from_rgb(185, 174, 152)
            } else {
                Color32::from_rgb(89, 77, 58)
            },
        ),
    );
    let ang = |t: f32| std::f32::consts::PI * (0.75 + 1.5 * t);
    let at = |a: f32, rr: f32| Pos2::new(c.x + a.cos() * rr, c.y + a.sin() * rr);
    let t = ((v - min) / (max - min)).clamp(0.0, 1.0);
    let t0 = if bipolar { 0.5 } else { 0.0 };
    let (a, b) = if t >= t0 { (t0, t) } else { (t, t0) };
    let n = 18;
    let pts: Vec<Pos2> = (0..=n)
        .map(|k| at(ang(a + (b - a) * k as f32 / n as f32), r - 1.5))
        .collect();
    if b - a > 0.002 {
        p.add(egui::Shape::line(pts, Stroke::new(2.0_f32, color)));
    }
    p.line_segment(
        [at(ang(t), r * 0.25), at(ang(t), r - 2.5)],
        Stroke::new(1.6_f32, Color32::from_rgb(239, 231, 212)),
    );
    done
}

/// A tiny piano-keys glyph button (the rack's "open in piano roll").
pub fn keys_button(ui: &mut Ui, rect: Rect, id: egui::Id, on: bool, color: Color32) -> Response {
    let resp = ui
        .interact(rect, id, Sense::click())
        .on_hover_text("Open in the piano roll");
    let p = ui.painter_at(rect);
    let bg = if on {
        color.gamma_multiply(0.45)
    } else if resp.hovered() {
        Color32::from_rgb(62, 51, 37)
    } else {
        Color32::from_rgb(36, 30, 23)
    };
    p.rect_filled(rect, 3.0, bg);
    let inner = rect.shrink2(Vec2::new(3.0, 4.0));
    let kw = inner.width() / 4.0;
    for k in 0..4 {
        let x = inner.left() + k as f32 * kw;
        p.rect_filled(
            Rect::from_min_size(
                Pos2::new(x + 0.5, inner.top()),
                Vec2::new(kw - 1.0, inner.height()),
            ),
            1.0,
            Color32::from_rgb(239, 231, 212),
        );
    }
    for k in [1.0f32, 2.0, 3.0] {
        if k == 2.0 {
            continue;
        }
        let x = inner.left() + k * kw - kw * 0.3;
        p.rect_filled(
            Rect::from_min_size(
                Pos2::new(x, inner.top()),
                Vec2::new(kw * 0.6, inner.height() * 0.58),
            ),
            0.5,
            Color32::from_rgb(20, 17, 13),
        );
    }
    resp
}

#[cfg(test)]
mod pointer_tests {
    use super::*;
    use eframe::egui::{Event, Modifiers, PointerButton, RawInput};

    /// Drive mini_knob with real pointer events (what a mouse produces after
    /// the windowing layer has mapped pixels to points) and return its result.
    fn drag_knob(steps: &[f32], shift: bool, ppp: f32) -> Option<f32> {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(ppp);
        let rect = Rect::from_min_size(Pos2::new(100.0, 100.0), Vec2::splat(20.0));
        // events in points (the windowing layer maps pixels through ppp)
        let c = rect.center();
        let mods = Modifiers {
            shift,
            ..Default::default()
        };
        let mut frames: Vec<Vec<Event>> = vec![
            vec![],
            vec![Event::PointerMoved(c)],
            vec![Event::PointerButton {
                pos: c,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: mods,
            }],
        ];
        let mut y = c.y;
        for dy in steps {
            y += dy;
            frames.push(vec![Event::PointerMoved(Pos2::new(c.x, y))]);
        }
        frames.push(vec![Event::PointerButton {
            pos: Pos2::new(c.x, y),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: mods,
        }]);
        frames.push(vec![]);
        let mut out = None;
        for (k, ev) in frames.into_iter().enumerate() {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0))),
                events: ev,
                modifiers: mods,
                time: Some(k as f64 * 0.05),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if let Some(v) = mini_knob(
                        ui,
                        rect,
                        egui::Id::new("knob"),
                        0.0,
                        -1.0,
                        1.0,
                        0.0,
                        true,
                        Color32::WHITE,
                        "pan",
                    ) {
                        out = Some(v);
                    }
                });
            });
        }
        out
    }

    #[test]
    fn a_mouse_drag_up_turns_the_knob_relatively_and_shift_is_fine() {
        let up = [-10.0_f32; 7]; // 70 points up
        let normal = drag_knob(&up, false, 1.0).expect("drag released -> value");
        assert!(normal > 0.4 && normal <= 1.0, "{normal}");
        let fine = drag_knob(&up, true, 1.0).expect("fine drag");
        assert!(fine > 0.0 && fine < normal * 0.4, "{fine} vs {normal}");
        // HiDPI: the same drag in points gives the same turn at 2x scale
        let hidpi = drag_knob(&up, false, 2.0).expect("hidpi drag");
        assert!((hidpi - normal).abs() < 0.05, "{hidpi} vs {normal}");
        // dragging down turns it the other way; a bare click changes nothing
        let down = drag_knob(&[10.0_f32; 7], false, 1.0).unwrap();
        assert!(down < -0.4, "{down}");
        assert_eq!(drag_knob(&[], false, 1.0), None);
    }
}
