//! The browser view (FL's Browser): instrument presets, effects, sound
//! palettes and the project's samples, searchable, with a target track. Every
//! button sends the MCP tool an AI would (set_instrument, add_track,
//! add_effect, apply_palette, add_sample_track); the lists come from the same
//! discovery tools (list_presets, list_palettes, list_samples).

use super::theme::Tokens;
use super::Studio;
use crate::project::Project;
use eframe::egui::{self, RichText};
use serde_json::{json, Value};

#[derive(Default)]
pub struct BrowserState {
    pub search: String,
    pub track: String,
    /// list_presets + list_palettes, fetched once
    pub catalog: Option<(Value, Value)>,
}

fn matches(q: &str, a: &str, b: &str) -> bool {
    let q = q.trim().to_lowercase();
    q.is_empty() || a.to_lowercase().contains(&q) || b.to_lowercase().contains(&q)
}

impl Studio {
    pub(super) fn browser_view(&mut self, ui: &mut egui::Ui, p: &Project) {
        let tk = Tokens::DARK;
        if self.browser.catalog.is_none() {
            let mut e = self.engine.lock().unwrap();
            let pr = e.call_from("list_presets", &json!({}), "you").unwrap_or(Value::Null);
            let pa = e.call_from("list_palettes", &json!({}), "you").unwrap_or(Value::Null);
            drop(e);
            self.browser.catalog = Some((pr, pa));
        }
        if self.browser.track.is_empty() || p.track_index(&self.browser.track).is_err() {
            self.browser.track = p.tracks.first().map(|t| t.name.clone()).unwrap_or_default();
        }
        let (presets, palettes) = self.browser.catalog.clone().unwrap_or((Value::Null, Value::Null));
        let mut call: Option<(String, Value)> = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new("SEARCH").size(11.0).color(tk.text_dim));
            ui.add(egui::TextEdit::singleline(&mut self.browser.search).desired_width(220.0).hint_text("808, bell, reverb, dark..."));
            ui.add_space(12.0);
            ui.label(RichText::new("TARGET TRACK").size(11.0).color(tk.text_dim));
            egui::ComboBox::from_id_salt("browser_track").selected_text(self.browser.track.clone()).show_ui(ui, |ui| {
                for t in &p.tracks {
                    ui.selectable_value(&mut self.browser.track, t.name.clone(), &t.name);
                }
            });
            ui.label(RichText::new("every button is an MCP tool call").size(11.0).color(tk.text_dim));
        });
        ui.add_space(6.0);
        let q = self.browser.search.clone();
        let track = self.browser.track.clone();
        let h = ui.available_height() - 8.0;
        ui.columns(3, |cols| {
            // ---------- instrument presets
            let ui = &mut cols[0];
            ui.label(RichText::new("INSTRUMENTS").size(11.0).strong().color(tk.header_text));
            egui::ScrollArea::vertical().id_salt("br_inst").max_height(h).show(ui, |ui| {
                for it in presets["instrument_presets"].as_array().cloned().unwrap_or_default() {
                    let (n, d) = (it["name"].as_str().unwrap_or(""), it["description"].as_str().unwrap_or(""));
                    if !matches(&q, n, d) {
                        continue;
                    }
                    ui.horizontal(|ui| {
                        if ui.small_button("LOAD").on_hover_text(format!("set_instrument {{track: {track}, preset: {n}}}")).clicked() && !track.is_empty() {
                            call = Some(("set_instrument".into(), json!({"track": track, "preset": n})));
                        }
                        if ui.small_button("+TRACK").on_hover_text(format!("add_track {{name: {n}, preset: {n}}}")).clicked() {
                            call = Some(("add_track".into(), json!({"name": n, "preset": n})));
                        }
                        ui.label(RichText::new(n).size(12.0).color(tk.text)).on_hover_text(d);
                    });
                }
            });
            // ---------- effects
            let ui = &mut cols[1];
            ui.label(RichText::new("EFFECTS").size(11.0).strong().color(tk.header_text));
            egui::ScrollArea::vertical().id_salt("br_fx").max_height(h).show(ui, |ui| {
                for it in presets["effects"].as_array().cloned().unwrap_or_default() {
                    let (n, d) = (it["type"].as_str().unwrap_or(""), it["params"].as_str().unwrap_or(""));
                    if !matches(&q, n, d) {
                        continue;
                    }
                    ui.horizontal(|ui| {
                        if ui.small_button("ADD").on_hover_text(format!("add_effect {{track: {track}, type: {n}}}")).clicked() && !track.is_empty() {
                            call = Some(("add_effect".into(), json!({"track": track, "type": n})));
                        }
                        ui.label(RichText::new(n).size(12.0).color(tk.text)).on_hover_text(d);
                    });
                    ui.add(egui::Label::new(RichText::new(format!("    {d}")).size(10.0).color(tk.text_dim)).truncate()).on_hover_text(d);
                }
            });
            // ---------- palettes + samples
            let ui = &mut cols[2];
            ui.label(RichText::new("SOUND PALETTES").size(11.0).strong().color(tk.header_text));
            for it in palettes["palettes"].as_array().cloned().unwrap_or_default() {
                let n = it["name"].as_str().unwrap_or("");
                let d = it["description"].as_str().unwrap_or("");
                if !matches(&q, n, d) {
                    continue;
                }
                ui.horizontal(|ui| {
                    if ui.small_button("APPLY").on_hover_text(format!("apply_palette {{palette: {n}}}")).clicked() {
                        call = Some(("apply_palette".into(), json!({"palette": n})));
                    }
                    ui.label(RichText::new(n).size(12.0).color(tk.text)).on_hover_text(d);
                });
                ui.add(egui::Label::new(RichText::new(format!("    {d}")).size(10.0).color(tk.text_dim)).truncate()).on_hover_text(d);
            }
            ui.add_space(10.0);
            ui.label(RichText::new(format!("PROJECT SAMPLES ({})", p.samples.len())).size(11.0).strong().color(tk.header_text));
            if p.samples.is_empty() {
                ui.label(RichText::new("none yet: import_sample {path}, download_sample {url}").size(10.0).color(tk.text_dim));
            }
            for s in &p.samples {
                if !matches(&q, &s.name, &s.source) {
                    continue;
                }
                ui.horizontal(|ui| {
                    if ui.small_button("+TRACK").on_hover_text(format!("add_sample_track {{name: {0}, sample: {0}}}", s.name)).clicked() {
                        call = Some(("add_sample_track".into(), json!({"name": format!("{}_s", s.name), "sample": s.name})));
                    }
                    ui.label(RichText::new(&s.name).size(12.0).color(tk.text));
                    ui.label(RichText::new(format!("{:.1} s \u{00B7} {}", s.duration, s.license)).size(10.0).color(tk.text_dim));
                });
            }
        });
        if let Some((tool, args)) = call {
            self.call(&tool, args);
        }
    }
}
