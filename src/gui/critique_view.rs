//! The critique view: the AI listener in a panel. One CRITIQUE button runs the
//! same `critique_mix` MCP tool an AI calls (genre reference chosen here),
//! then shows the score, PASS/FAIL, the problems in plain words, the band
//! balance against the reference with the tracks that own each band, the
//! loudness per section, and every fix with an APPLY button that sends that
//! fix's own tool call. Nothing here is GUI-only: an AI gets the same JSON.

use super::theme::Tokens;
use super::Studio;
use crate::project::Project;
use eframe::egui::{self, Color32, RichText};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

pub type CritJob = Option<(f64, Option<Result<Value, String>>)>;

pub struct CritiqueState {
    pub genre: usize,
    pub job: Arc<Mutex<CritJob>>,
    pub applied: Vec<usize>,
}

impl Default for CritiqueState {
    fn default() -> Self {
        CritiqueState { genre: 0, job: Arc::new(Mutex::new(None)), applied: Vec::new() }
    }
}

pub const GENRES: [&str; 3] = ["hiphop", "rnb", "none"];

impl Studio {
    /// Start critique_mix on a worker thread (one at a time).
    pub(super) fn start_critique(&mut self, now: f64) {
        let job = self.critique.job.clone();
        if matches!(job.lock().unwrap().as_ref(), Some((_, None))) {
            return;
        }
        *job.lock().unwrap() = Some((now, None));
        self.critique.applied.clear();
        let engine = self.engine.clone();
        let args = json!({"genre": GENRES[self.critique.genre]});
        std::thread::spawn(move || {
            let r = engine.lock().unwrap().call_from("critique_mix", &args, "you").map_err(|e| format!("{e:#}"));
            if let Some(j) = job.lock().unwrap().as_mut() {
                j.1 = Some(r);
            }
        });
    }

    pub(super) fn critique_done(&self) -> bool {
        matches!(self.critique.job.lock().unwrap().as_ref(), Some((_, Some(_))))
    }

    pub(super) fn critique_view(&mut self, ui: &mut egui::Ui, p: &Project) {
        let w = (ui.clip_rect().right() - ui.cursor().left() - 28.0).max(400.0);
        let h = ui.available_height();
        ui.allocate_ui(egui::vec2(w, h), |ui| {
            ui.set_max_width(w);
            egui::ScrollArea::vertical().id_salt("crit_scroll").show(ui, |ui| self.critique_inner(ui, p));
        });
    }

    fn critique_inner(&mut self, ui: &mut egui::Ui, _p: &Project) {
        let tk = Tokens::DARK;
        let now = ui.input(|i| i.time);
        let busy = matches!(self.critique.job.lock().unwrap().as_ref(), Some((_, None)));
        let mut run = false;
        let mut apply: Option<(usize, String, Value)> = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new("REFERENCE").size(11.0).color(tk.text_dim));
            for (i, g) in GENRES.iter().enumerate() {
                ui.selectable_value(&mut self.critique.genre, i, *g);
            }
            ui.add_space(12.0);
            let b = ui.add_enabled(!busy, egui::Button::new(RichText::new("  CRITIQUE  ").strong()).fill(tk.accent_dark));
            if b.clicked() {
                run = true;
            }
            ui.label(RichText::new("same call over MCP: critique_mix {genre}").size(11.0).color(tk.text_dim));
        });
        ui.add_space(8.0);
        let job = self.critique.job.lock().unwrap().clone();
        match job {
            None => {
                ui.label(RichText::new("An AI listener's measurements: loudness, band balance against a genre reference, clicks, held drones, hook-vs-verse lift, the vocal over the beat. Press CRITIQUE.").color(tk.text_dim));
            }
            Some((t0, None)) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(format!("listening\u{2026} {:.0} s", now - t0)).color(tk.text));
                });
            }
            Some((_, Some(Err(e)))) => {
                ui.label(RichText::new(format!("critique_mix failed: {e}")).color(tk.meter_red));
            }
            Some((_, Some(Ok(v)))) => {
                let pass = v["verdict"] == "PASS";
                let col = if pass { tk.meter_green } else { tk.meter_red };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{:.1}", v["score"].as_f64().unwrap_or(0.0))).size(34.0).strong().color(col));
                    ui.vertical(|ui| {
                        ui.label(RichText::new(v["verdict"].as_str().unwrap_or("")).size(16.0).strong().color(col));
                        let n = &v["numbers"];
                        ui.label(RichText::new(format!("{} LUFS \u{00B7} {} dBTP \u{00B7} LRA {} \u{00B7} {} clicks", n["integrated_lufs"], n["true_peak_dbtp"], n["loudness_range_lu"], n["clicks"])).size(11.0).color(tk.text_dim));
                    });
                });
                ui.add_space(6.0);
                ui.columns(2, |cols| {
                    // ---------- left: bands vs reference + sections
                    let ui = &mut cols[0];
                    ui.label(RichText::new(format!("BANDS vs {} REFERENCE", v["reference"].as_str().unwrap_or("").to_uppercase())).size(11.0).strong().color(tk.header_text));
                    let bands = &v["numbers"]["bands"];
                    for name in ["sub", "low", "lowmid", "mid", "harsh", "air"] {
                        let b = &bands[name];
                        let d = b["vs_ref_db"].as_f64().unwrap_or(0.0) as f32;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{name:>6}")).family(egui::FontFamily::Monospace).size(11.0).color(tk.text));
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(180.0, 12.0), egui::Sense::hover());
                            let pt = ui.painter();
                            pt.rect_filled(rect, 2.0, Color32::from_gray(38));
                            let mid = rect.center().x;
                            pt.line_segment([egui::pos2(mid, rect.top()), egui::pos2(mid, rect.bottom())], egui::Stroke::new(1.0_f32, Color32::from_gray(90)));
                            let x = mid + (d / 12.0).clamp(-1.0, 1.0) * rect.width() * 0.5;
                            let c = if d.abs() > 4.0 { tk.meter_red } else { tk.meter_green };
                            let r = egui::Rect::from_two_pos(egui::pos2(mid, rect.top() + 2.0), egui::pos2(x, rect.bottom() - 2.0));
                            pt.rect_filled(r, 1.0, c);
                            ui.label(RichText::new(format!("{:+.1} dB", d)).family(egui::FontFamily::Monospace).size(11.0).color(c));
                            if let Some(o) = b["owners"].as_array() {
                                let s: Vec<String> = o.iter().take(2).map(|x| format!("{} {}%", x["track"].as_str().unwrap_or(""), x["share_pct"])).collect();
                                ui.label(RichText::new(s.join(", ")).size(10.0).color(tk.text_dim));
                            }
                        });
                    }
                    if let Some(secs) = v["numbers"]["sections"].as_array().filter(|s| !s.is_empty()) {
                        ui.add_space(8.0);
                        ui.label(RichText::new("LOUDNESS PER SECTION").size(11.0).strong().color(tk.header_text));
                        let lo = secs.iter().filter_map(|s| s["lufs"].as_f64()).fold(f64::MAX, f64::min);
                        for s in secs {
                            let l = s["lufs"].as_f64().unwrap_or(-30.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("{:>8}", s["section"].as_str().unwrap_or(""))).family(egui::FontFamily::Monospace).size(11.0).color(tk.text));
                                let wbar = (((l - lo + 2.0) / 12.0).clamp(0.05, 1.0) * 180.0) as f32;
                                let (rect, _) = ui.allocate_exact_size(egui::vec2(wbar, 10.0), egui::Sense::hover());
                                let hook = s["section"].as_str().map(|n| n.starts_with("hook") || n.starts_with("chorus") || n.starts_with("drop")).unwrap_or(false);
                                ui.painter().rect_filled(rect, 1.0, if hook { Color32::from_rgb(232, 120, 60) } else { Color32::from_rgb(86, 140, 220) });
                                ui.label(RichText::new(format!("{l:.1} LUFS")).size(10.0).color(tk.text_dim));
                            });
                        }
                    }
                    // ---------- right: problems + fixes
                    let ui = &mut cols[1];
                    ui.label(RichText::new("WHAT IS OFF").size(11.0).strong().color(tk.header_text));
                    let probs = v["problems"].as_array().cloned().unwrap_or_default();
                    if probs.is_empty() {
                        ui.label(RichText::new("nothing measurable").color(tk.meter_green));
                    }
                    for pr in &probs {
                        left(ui, RichText::new(format!("\u{2022} {}", pr.as_str().unwrap_or(""))).size(12.0).color(tk.text));
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new("FIXES  (each is the tool call an AI would send)").size(11.0).strong().color(tk.header_text));
                    for (i, f) in v["fixes"].as_array().cloned().unwrap_or_default().iter().enumerate() {
                        ui.horizontal(|ui| {
                            let done = self.critique.applied.contains(&i);
                            let b = ui.add_enabled(!done, egui::Button::new(if done { "applied" } else { "APPLY" }).small());
                            if b.clicked() {
                                apply = Some((i, f["tool"].as_str().unwrap_or("").to_string(), f["args"].clone()));
                            }
                            left(ui, RichText::new(f["why"].as_str().unwrap_or("")).size(11.0).color(tk.text));
                        });
                        let args = serde_json::to_string(&f["args"]).unwrap_or_default().replace(",\"", ", \"").replace("\":", "\": ");
                        left(ui, RichText::new(format!("    {} {}", f["tool"].as_str().unwrap_or(""), args)).size(10.0).family(egui::FontFamily::Monospace).color(tk.text_dim));
                    }
                    ui.add_space(6.0);
                    left(ui, RichText::new(v["note"].as_str().unwrap_or("")).size(10.0).italics().color(tk.text_dim));
                });
            }
        }
        if let Some((i, tool, args)) = apply {
            if !tool.is_empty() {
                self.call(&tool, args);
                self.critique.applied.push(i);
            }
        }
        if run {
            self.start_critique(now);
        }
        if busy {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}

/// A wrapped label laid out left-aligned (ui.columns uses a justified layout, which
/// spreads wrapped text glyph by glyph).
pub(super) fn left(ui: &mut egui::Ui, t: RichText) {
    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.add(egui::Label::new(t).wrap());
    });
}
