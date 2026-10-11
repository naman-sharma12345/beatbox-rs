//! Custom-painted widgets and the studio's visual theme.

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2,
};
use std::f32::consts::PI;

// The studio palette, mapped onto the ported SoundCraft design tokens (see theme.rs):
// neutral charcoal surfaces, one blue accent, console green for counters.
use super::theme::Tokens;
pub const BG: Color32 = Color32::from_rgb(24, 24, 25);
pub const PANEL: Color32 = Tokens::DARK.panel_bg;
pub const PANEL2: Color32 = Tokens::DARK.panel_bg2;
pub const LINE: Color32 = Color32::from_rgb(58, 58, 61);
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
        Color32::from_rgb(255, 92, 122)
    } else if n.contains("snare") || n.contains("clap") {
        Color32::from_rgb(255, 181, 71)
    } else if n.contains("hat") || n.contains("shaker") || n.contains("crash") {
        Color32::from_rgb(74, 222, 128)
    } else if n.contains("rim") || n.contains("tom") || n.contains("cowbell") || n.contains("perc")
    {
        Color32::from_rgb(45, 212, 191)
    } else if n.contains("bass") || n.contains("808") || n.contains("sub") {
        Color32::from_rgb(167, 139, 250)
    } else if n.contains("chord") || n.contains("pad") || n.contains("key") {
        Color32::from_rgb(96, 165, 250)
    } else if n.contains("lead") || n.contains("melody") || n.contains("bell") {
        Color32::from_rgb(244, 114, 182)
    } else {
        match kind {
            "sampler" => Color32::from_rgb(251, 191, 36),
            "drum" => Color32::from_rgb(45, 212, 191),
            "fm" => Color32::from_rgb(129, 140, 248),
            _ => Color32::from_rgb(232, 121, 249),
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
    let size = Vec2::new(50.0, 62.0);
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
    let c = Pos2::new(rect.center().x, rect.top() + 22.0);
    let r = 17.0;
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
            Color32::from_rgb(54, 54, 57)
        } else {
            Color32::from_rgb(42, 42, 44)
        },
    );
    painter.add(Shape::line(
        arc(a0, a1, r),
        Stroke::new(3.5_f32, Color32::from_rgb(60, 60, 63)),
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
    let text = if hover {
        format_value(*value)
    } else {
        short(label)
    };
    painter.text(
        Pos2::new(rect.center().x, rect.bottom() - 9.0),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(10.5),
        if hover { TEXT } else { DIM },
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

/// Square console toggle (M / S style), drawn like SoundCraft's `text_toggle`.
pub fn pill(ui: &mut Ui, text: &str, on: bool, color: Color32) -> Response {
    let t = Tokens::DARK;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(22.0, 18.0), Sense::click());
    let fill = if on {
        color
    } else if resp.hovered() {
        t.button_hi
    } else {
        t.button
    };
    ui.painter()
        .rect(rect, 2.0, fill, Stroke::new(1.0_f32, t.button_border));
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(11.0),
        if on { t.text_dark } else { t.text },
    );
    resp
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
pub fn tabs(ui: &mut Ui, labels: &[&str], current: usize) -> Option<usize> {
    let mut clicked = None;
    let h = 28.0;
    let widths: Vec<f32> = labels.iter().map(|l| 22.0 + l.len() as f32 * 7.4).collect();
    let total: f32 = widths.iter().sum::<f32>() + 6.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(total, h), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 3.0, Tokens::DARK.toolbar_bg);
    p.rect_stroke(rect, 3.0, Stroke::new(1.0_f32, Tokens::DARK.border));
    let mut x = rect.left() + 3.0;
    for (i, (l, w)) in labels.iter().zip(widths.iter()).enumerate() {
        let r = Rect::from_min_size(Pos2::new(x, rect.top() + 3.0), Vec2::new(*w, h - 6.0));
        let resp = ui.interact(r, ui.id().with(("tab", i, *l)), Sense::click());
        let on = i == current;
        if on {
            p.rect_filled(r, 2.0, ACCENT);
        } else if resp.hovered() {
            p.rect_filled(r, 2.0, Tokens::DARK.button);
        }
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            *l,
            FontId::proportional(11.5),
            if on { Color32::WHITE } else { DIM },
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
