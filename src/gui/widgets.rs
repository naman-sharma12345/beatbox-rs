//! Custom-painted widgets and the studio's visual theme.

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2,
};
use std::f32::consts::PI;

pub const BG: Color32 = Color32::from_rgb(11, 13, 19);
pub const PANEL: Color32 = Color32::from_rgb(18, 21, 30);
pub const PANEL2: Color32 = Color32::from_rgb(24, 28, 40);
pub const LINE: Color32 = Color32::from_rgb(38, 43, 60);
pub const TEXT: Color32 = Color32::from_rgb(226, 230, 240);
pub const DIM: Color32 = Color32::from_rgb(128, 136, 160);
pub const ACCENT: Color32 = Color32::from_rgb(139, 92, 246);
pub const ACCENT2: Color32 = Color32::from_rgb(34, 211, 238);
pub const GOOD: Color32 = Color32::from_rgb(74, 222, 128);
pub const WARN: Color32 = Color32::from_rgb(251, 191, 36);
pub const HOT: Color32 = Color32::from_rgb(255, 92, 122);

pub fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = BG;
    v.faint_bg_color = PANEL2;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.6);
    v.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    v.window_rounding = egui::Rounding::same(10.0);
    v.widgets.noninteractive.bg_fill = PANEL2;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, LINE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.rounding = egui::Rounding::same(6.0);
    }
    v.widgets.inactive.bg_fill = PANEL2;
    v.widgets.inactive.weak_bg_fill = PANEL2;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, LINE);
    v.widgets.hovered.bg_fill = Color32::from_rgb(34, 39, 56);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(34, 39, 56);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.7));
    v.widgets.active.bg_fill = ACCENT.gamma_multiply(0.5);
    v.widgets.active.weak_bg_fill = ACCENT.gamma_multiply(0.5);
    ctx.set_visuals(v);
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(10.0, 5.0);
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(13.5));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(13.5));
    style
        .text_styles
        .insert(egui::TextStyle::Small, FontId::proportional(11.0));
    style
        .text_styles
        .insert(egui::TextStyle::Heading, FontId::proportional(18.0));
    ctx.set_style(style);
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
        .rounding(egui::Rounding::same(10.0))
        .stroke(Stroke::new(1.0_f32, LINE))
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
            Color32::from_rgb(40, 46, 66)
        } else {
            Color32::from_rgb(31, 36, 52)
        },
    );
    painter.add(Shape::line(
        arc(a0, a1, r),
        Stroke::new(3.5, Color32::from_rgb(44, 50, 70)),
    ));
    if t > 0.001 {
        painter.add(Shape::line(
            arc(a0, a0 + (a1 - a0) * t, r),
            Stroke::new(3.5, color),
        ));
    }
    let a = a0 + (a1 - a0) * t;
    painter.line_segment(
        [
            Pos2::new(c.x + (r - 12.0) * a.cos(), c.y + (r - 12.0) * a.sin()),
            Pos2::new(c.x + (r - 4.0) * a.cos(), c.y + (r - 4.0) * a.sin()),
        ],
        Stroke::new(2.5, TEXT),
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

/// Pill-shaped toggle button (M / S style).
pub fn pill(ui: &mut Ui, text: &str, on: bool, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(22.0, 18.0), Sense::click());
    let fill = if on {
        color
    } else if resp.hovered() {
        Color32::from_rgb(44, 50, 70)
    } else {
        Color32::from_rgb(33, 38, 54)
    };
    ui.painter().rect_filled(rect, 5.0, fill);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(10.5),
        if on { BG } else { DIM },
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

/// Circular score gauge 0..100.
pub fn score_ring(ui: &mut Ui, score: u32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(76.0), Sense::hover());
    let c = rect.center();
    let r = 31.0;
    let p = ui.painter();
    let ring = |from: f32, to: f32| -> Vec<Pos2> {
        (0..=48)
            .map(|i| {
                let a = from + (to - from) * i as f32 / 48.0;
                Pos2::new(c.x + r * a.cos(), c.y + r * a.sin())
            })
            .collect()
    };
    p.add(Shape::line(
        ring(-PI / 2.0, PI * 1.5),
        Stroke::new(6.0, Color32::from_rgb(38, 43, 60)),
    ));
    let t = score as f32 / 100.0;
    let col = if score >= 85 {
        GOOD
    } else if score >= 65 {
        WARN
    } else {
        HOT
    };
    p.add(Shape::line(
        ring(-PI / 2.0, -PI / 2.0 + 2.0 * PI * t),
        Stroke::new(6.0, col),
    ));
    p.text(
        c - Vec2::new(0.0, 5.0),
        Align2::CENTER_CENTER,
        score.to_string(),
        FontId::proportional(22.0),
        TEXT,
    );
    p.text(
        c + Vec2::new(0.0, 14.0),
        Align2::CENTER_CENTER,
        "MIX",
        FontId::proportional(9.5),
        DIM,
    );
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
