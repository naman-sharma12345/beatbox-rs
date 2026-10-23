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
    pub playhead: Color32,
    pub solo: Color32,
    pub mute: Color32,
    pub rec: Color32,
    pub auto_read: Color32,
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
        panel_bg: Color32::from_rgb(29, 25, 20),
        panel_bg2: Color32::from_rgb(36, 31, 25),
        toolbar_bg: Color32::from_rgb(24, 21, 17),
        border: Color32::from_rgb(48, 41, 32),
        border_light: Color32::from_rgb(89, 77, 58),
        text: Color32::from_rgb(241, 234, 217),
        text_dim: Color32::from_rgb(185, 174, 152),
        text_dark: Color32::from_rgb(24, 20, 16),
        header_text: Color32::from_rgb(216, 204, 181),
        counter_bg: Color32::from_rgb(19, 16, 12),
        counter_text: Color32::from_rgb(241, 234, 217),
        accent: Color32::from_rgb(232, 68, 46),
        accent_dark: Color32::from_rgb(126, 42, 29),
        button: Color32::from_rgb(43, 36, 27),
        button_hi: Color32::from_rgb(62, 51, 37),
        button_border: Color32::from_rgb(67, 55, 40),
        name_field: Color32::from_rgb(42, 35, 26),
        name_field_sel: Color32::from_rgb(83, 58, 33),
        ruler_bg: Color32::from_rgb(31, 26, 20),
        ruler_text: Color32::from_rgb(185, 174, 152),
        ruler_tick: Color32::from_rgb(97, 81, 59),
        tempo_ruler: Color32::from_rgb(179, 132, 57),
        playlist_bg: Color32::from_rgb(21, 18, 14),
        playlist_alt: Color32::from_rgb(26, 22, 17),
        grid_line: Color32::from_rgb(44, 37, 28),
        bar_line: Color32::from_rgb(66, 54, 39),
        playhead: Color32::from_rgb(232, 68, 46),
        solo: Color32::from_rgb(232, 163, 61),
        mute: Color32::from_rgb(232, 163, 61),
        rec: Color32::from_rgb(232, 68, 46),
        auto_read: Color32::from_rgb(79, 162, 150),
        meter_green: Color32::from_rgb(207, 194, 156),
        meter_yellow: Color32::from_rgb(232, 163, 61),
        meter_red: Color32::from_rgb(232, 68, 46),
        fader_track: Color32::from_rgb(16, 14, 11),
        strip_bg: Color32::from_rgb(28, 24, 19),
        strip_section: Color32::from_rgb(23, 20, 16),
        slot_bg: Color32::from_rgb(20, 17, 13),
    };
}

/// Track colours tinted for clip bodies: (body, title bar, content ink).
pub fn clip_colors(c: Color32, selected: bool) -> (Color32, Color32, Color32) {
    let k = if selected { 0.32 } else { 0.18 };
    let scale = |v: u8, f: f32| (f32::from(v) * f) as u8;
    let body = Color32::from_rgb(scale(c.r(), k), scale(c.g(), k), scale(c.b(), k));
    let bar = Color32::from_rgb(scale(c.r(), 0.46), scale(c.g(), 0.46), scale(c.b(), 0.46));
    let ink = Tokens::DARK.text;
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
    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes, family) in [
        (
            "space",
            include_bytes!("../../assets/reel/fonts/SpaceGrotesk.ttf").as_slice(),
            FontFamily::Proportional,
        ),
        (
            "mono",
            include_bytes!("../../assets/reel/fonts/JetBrainsMono.ttf").as_slice(),
            FontFamily::Monospace,
        ),
        (
            "brand",
            include_bytes!("../../assets/reel/fonts/ArchivoExpanded.ttf").as_slice(),
            FontFamily::Name("brand".into()),
        ),
    ] {
        fonts
            .font_data
            .insert(name.into(), egui::FontData::from_static(bytes));
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, name.into());
    }
    ctx.set_fonts(fonts);
    let t = Tokens::DARK;
    let mut v = egui::Visuals::dark();
    v.panel_fill = t.panel_bg;
    v.window_fill = t.panel_bg2;
    v.extreme_bg_color = t.slot_bg;
    v.faint_bg_color = t.panel_bg2;
    v.override_text_color = Some(t.text);
    v.selection.bg_fill = t.accent;
    v.selection.stroke = Stroke::new(1.0_f32, t.accent);
    v.window_rounding = egui::Rounding::same(8.0);
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
        w.rounding = egui::Rounding::same(5.0);
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
        (egui::TextStyle::Small, 11.5),
        (egui::TextStyle::Heading, 17.0),
    ] {
        style.text_styles.insert(ts, regular(size));
    }
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, mono(12.0));
    ctx.set_style(style);
}
