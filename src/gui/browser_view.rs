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
    /// list_fx_chains, refetched after any browser call (saved chains change)
    pub chains: Option<Value>,
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
        if self.browser.chains.is_none() {
            let c = self.engine.lock().unwrap().call_from("list_fx_chains", &json!({}), "you").unwrap_or(Value::Null);
            self.browser.chains = Some(c);
        }
        let chains = self.browser.chains.clone().unwrap_or(Value::Null);
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
            // ---------- fx chains (Patcher-style presets) + effects
            let ui = &mut cols[1];
            ui.horizontal(|ui| {
                ui.label(RichText::new("FX CHAINS").size(11.0).strong().color(tk.header_text));
                if ui.small_button("SAVE TRACK'S CHAIN").on_hover_text(format!("save_fx_chain {{track: {track}, name: {track}_chain}}")).clicked() && !track.is_empty() {
                    call = Some(("save_fx_chain".into(), json!({"track": track, "name": format!("{track}_chain")})));
                }
            });
            for it in chains["chains"].as_array().cloned().unwrap_or_default() {
                let n = it["name"].as_str().unwrap_or("");
                let d = it["use"].as_str().unwrap_or("saved in this project");
                if !matches(&q, n, d) {
                    continue;
                }
                let fx: Vec<String> = it["effects"].as_array().map(|a| a.iter().map(|x| x["type"].as_str().unwrap_or("?").to_string()).collect()).unwrap_or_default();
                ui.horizontal(|ui| {
                    if ui.small_button("APPLY").on_hover_text(format!("apply_fx_chain {{track: {track}, chain: {n}}}")).clicked() && !track.is_empty() {
                        call = Some(("apply_fx_chain".into(), json!({"track": track, "chain": n})));
                    }
                    ui.label(RichText::new(n).size(12.0).color(tk.text)).on_hover_text(d);
                    ui.add(egui::Label::new(RichText::new(fx.join(" > ")).size(10.0).color(tk.text_dim)).truncate()).on_hover_text(d);
                });
            }
            ui.add_space(8.0);
            ui.label(RichText::new("EFFECTS").size(11.0).strong().color(tk.header_text));
            egui::ScrollArea::vertical().id_salt("br_fx").max_height((ui.available_height() - 8.0).max(120.0)).show(ui, |ui| {
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
            self.browser.chains = None;
        }
    }
}
