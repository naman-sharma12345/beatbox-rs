// Adapted from SoundCraft `crates/ui-egui/src/theme.rs` (commit eac0edd).
// Copyright (c) 2026 ArtCraft Team and the SoundCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your option;
// used in Beatbox under the MIT License. See THIRD_PARTY_NOTICES.md.

//! Design tokens: a dark, neutral studio palette (charcoal surfaces, one accent,
//! console-style counters and meters). Ported to egui 0.29: `Rounding` instead of
//! `CornerRadius`, no bundled bold font (egui 0.29 ships none, so `bold` is the
//! proportional face; callers add `.strong()` where it matters).

use eframe::egui::{self, Color32, FontFamily, FontId, Stroke, Vec2};

#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    pub window_bg: Color32,
    pub panel_bg: Color32,
    pub panel_bg2: Color32,
    pub toolbar_bg: Color32,
    pub border: Color32,
    pub border_light: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_dark: Color32,
    pub header_text: Color32,
    pub counter_bg: Color32,
    pub counter_text: Color32,
    pub accent: Color32,
    pub accent_dark: Color32,
    pub button: Color32,
    pub button_hi: Color32,
    pub button_border: Color32,
    pub name_field: Color32,
    pub name_field_sel: Color32,
    pub ruler_bg: Color32,
    pub ruler_text: Color32,
    pub ruler_tick: Color32,
    pub tempo_ruler: Color32,
    pub playlist_bg: Color32,
    pub playlist_alt: Color32,
    pub grid_line: Color32,
    pub bar_line: Color32,
    pub selection: Color32,
    pub playhead: Color32,
    pub solo: Color32,
    pub mute: Color32,
    pub rec: Color32,
    pub auto_read: Color32,
    pub auto_write: Color32,
    pub meter_green: Color32,
    pub meter_yellow: Color32,
    pub meter_red: Color32,
    pub fader_track: Color32,
    pub strip_bg: Color32,
    pub strip_section: Color32,
    pub slot_bg: Color32,
}

impl Tokens {
    pub const DARK: Tokens = Tokens {
        window_bg: Color32::from_rgb(30, 30, 30),
        panel_bg: Color32::from_rgb(37, 37, 38),
        panel_bg2: Color32::from_rgb(44, 44, 45),
        toolbar_bg: Color32::from_rgb(30, 30, 31),
        border: Color32::from_rgb(14, 14, 14),
        border_light: Color32::from_rgb(64, 64, 66),
        text: Color32::from_rgb(214, 214, 214),
        text_dim: Color32::from_rgb(150, 150, 152),
        text_dark: Color32::from_rgb(16, 16, 16),
        header_text: Color32::from_rgb(196, 196, 198),
        counter_bg: Color32::from_rgb(4, 4, 4),
        counter_text: Color32::from_rgb(104, 220, 120),
        accent: Color32::from_rgb(64, 132, 196),
        accent_dark: Color32::from_rgb(40, 92, 146),
        button: Color32::from_rgb(58, 58, 60),
        button_hi: Color32::from_rgb(78, 78, 80),
        button_border: Color32::from_rgb(20, 20, 20),
        name_field: Color32::from_rgb(208, 208, 208),
        name_field_sel: Color32::from_rgb(232, 232, 232),
        ruler_bg: Color32::from_rgb(40, 40, 41),
        ruler_text: Color32::from_rgb(178, 178, 180),
        ruler_tick: Color32::from_rgb(96, 96, 98),
        tempo_ruler: Color32::from_rgb(52, 128, 88),
        playlist_bg: Color32::from_rgb(36, 36, 37),
        playlist_alt: Color32::from_rgb(40, 40, 41),
        grid_line: Color32::from_rgb(52, 52, 54),
        bar_line: Color32::from_rgb(66, 66, 70),
        selection: Color32::from_rgba_premultiplied(40, 70, 110, 110),
        playhead: Color32::from_rgb(232, 60, 50),
        solo: Color32::from_rgb(222, 196, 52),
        mute: Color32::from_rgb(232, 148, 40),
        rec: Color32::from_rgb(214, 52, 46),
        auto_read: Color32::from_rgb(96, 200, 110),
        auto_write: Color32::from_rgb(220, 70, 60),
        meter_green: Color32::from_rgb(60, 200, 80),
        meter_yellow: Color32::from_rgb(230, 210, 60),
        meter_red: Color32::from_rgb(230, 50, 40),
        fader_track: Color32::from_rgb(12, 12, 12),
        strip_bg: Color32::from_rgb(46, 46, 48),
        strip_section: Color32::from_rgb(36, 36, 38),
        slot_bg: Color32::from_rgb(28, 28, 29),
    };
}

/// Track colours tinted for clip bodies: (body, title bar, content ink).
pub fn clip_colors(c: Color32, selected: bool) -> (Color32, Color32, Color32) {
    let k = if selected { 0.95 } else { 0.62 };
    let scale = |v: u8, f: f32| (f32::from(v) * f) as u8;
    let body = Color32::from_rgb(scale(c.r(), k), scale(c.g(), k), scale(c.b(), k));
    let bar = Color32::from_rgb(scale(c.r(), 0.9), scale(c.g(), 0.9), scale(c.b(), 0.9));
    let ink = if selected {
        Color32::from_rgb(20, 20, 24)
    } else {
        Color32::from_rgb(12, 12, 14)
    };
    (body, bar, ink)
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// Apply the studio visuals and spacing to the context.
pub fn apply(ctx: &egui::Context) {
    let t = Tokens::DARK;
    let mut v = egui::Visuals::dark();
    v.panel_fill = t.panel_bg;
    v.window_fill = t.panel_bg2;
    v.extreme_bg_color = t.slot_bg;
    v.faint_bg_color = t.panel_bg2;
    v.override_text_color = Some(t.text);
    v.selection.bg_fill = t.accent;
    v.selection.stroke = Stroke::new(1.0_f32, t.accent);
    v.window_rounding = egui::Rounding::same(6.0);
    v.menu_rounding = egui::Rounding::same(4.0);
    v.widgets.noninteractive.bg_fill = t.panel_bg2;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, t.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, t.text);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.rounding = egui::Rounding::same(3.0);
        w.bg_stroke = Stroke::new(1.0_f32, t.button_border);
    }
    v.widgets.inactive.bg_fill = t.button;
    v.widgets.inactive.weak_bg_fill = t.button;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, t.text);
    v.widgets.hovered.bg_fill = t.button_hi;
    v.widgets.hovered.weak_bg_fill = t.button_hi;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, t.border_light);
    v.widgets.active.bg_fill = t.accent_dark;
    v.widgets.active.weak_bg_fill = t.accent_dark;
    v.widgets.open.weak_bg_fill = t.button_hi;
    ctx.set_visuals(v);
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(6.0, 4.0);
    style.spacing.button_padding = Vec2::new(8.0, 3.0);
    style.interaction.tooltip_delay = 0.4;
    for (ts, size) in [
        (egui::TextStyle::Body, 13.0),
        (egui::TextStyle::Button, 12.5),
        (egui::TextStyle::Small, 10.5),
        (egui::TextStyle::Heading, 17.0),
    ] {
        style.text_styles.insert(ts, regular(size));
    }
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, mono(12.0));
    ctx.set_style(style);
}
