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
        panel_bg: Color32::from_rgb(26, 32, 41),
        panel_bg2: Color32::from_rgb(37, 46, 58),
        border: Color32::from_rgb(52, 65, 83),
        border_light: Color32::from_rgb(135, 150, 170),
        text: Color32::from_rgb(242, 245, 250),
        text_dim: Color32::from_rgb(181, 193, 209),
        text_dark: Color32::from_rgb(17, 21, 27),
        header_text: Color32::from_rgb(242, 245, 250),
        counter_bg: Color32::from_rgb(17, 21, 27),
        counter_text: Color32::from_rgb(242, 245, 250),
        accent: Color32::from_rgb(97, 213, 238),
        accent_dark: Color32::from_rgb(38, 63, 74),
        button: Color32::from_rgb(37, 46, 58),
        button_hi: Color32::from_rgb(52, 65, 83),
        button_border: Color32::from_rgb(135, 150, 170),
        name_field: Color32::from_rgb(37, 46, 58),
        name_field_sel: Color32::from_rgb(38, 63, 74),
        ruler_bg: Color32::from_rgb(26, 32, 41),
        ruler_text: Color32::from_rgb(181, 193, 209),
        ruler_tick: Color32::from_rgb(135, 150, 170),
        tempo_ruler: Color32::from_rgb(255, 196, 107),
        playlist_bg: Color32::from_rgb(17, 21, 27),
        playlist_alt: Color32::from_rgb(26, 32, 41),
        grid_line: Color32::from_rgb(37, 46, 58),
        bar_line: Color32::from_rgb(52, 65, 83),
        playhead: Color32::from_rgb(242, 245, 250),
        solo: Color32::from_rgb(255, 196, 107),
        mute: Color32::from_rgb(255, 196, 107),
        rec: Color32::from_rgb(255, 101, 116),
        auto_read: Color32::from_rgb(97, 213, 238),
        meter_green: Color32::from_rgb(121, 221, 180),
        meter_yellow: Color32::from_rgb(255, 196, 107),
        meter_red: Color32::from_rgb(255, 101, 116),
        fader_track: Color32::from_rgb(17, 21, 27),
        strip_bg: Color32::from_rgb(26, 32, 41),
        strip_section: Color32::from_rgb(17, 21, 27),
        slot_bg: Color32::from_rgb(17, 21, 27),
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
    v.selection.bg_fill = t.accent_dark;
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
    v.widgets.active.fg_stroke = Stroke::new(1.0, t.text);
    v.widgets.active.bg_stroke = Stroke::new(1.5, t.accent);
    v.widgets.open.bg_fill = t.button_hi;
    v.widgets.open.weak_bg_fill = t.button_hi;
    ctx.set_visuals(v);
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 4.0);
    style.spacing.button_padding = Vec2::new(8.0, 4.0);
    style.spacing.interact_size.y = 24.0;
    style.interaction.tooltip_delay = 0.4;
    for (ts, size) in [
        (egui::TextStyle::Body, 13.0),
        (egui::TextStyle::Button, 13.0),
        (egui::TextStyle::Small, 12.0),
        (egui::TextStyle::Heading, 17.0),
    ] {
        style.text_styles.insert(ts, regular(size));
    }
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, mono(12.0));
    ctx.set_style(style);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn contrast(a: Color32, b: Color32) -> f64 {
        let luminance = |c: Color32| {
            let linear = |v: u8| {
                let v = f64::from(v) / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
        };
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
    #[test]
    fn graphite_text_and_action_contrast() {
        let t = Tokens::DARK;
        for bg in [
            t.counter_bg,
            t.panel_bg,
            t.panel_bg2,
            t.button_hi,
            t.accent_dark,
        ] {
            for fg in [t.text, t.text_dim] {
                assert!(contrast(fg, bg) >= 4.5);
            }
        }
        for fg in [t.accent, t.rec, t.solo] {
            assert!(contrast(fg, t.panel_bg) >= 3.0);
            assert!(contrast(t.text_dark, fg) >= 4.5);
        }
        assert!(contrast(t.button_border, t.button) >= 3.0);
    }
}
