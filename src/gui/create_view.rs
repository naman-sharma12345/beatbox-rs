//! The create view: the one-call tools in a panel. A prompt box and a lyrics
//! box (read live, exactly as make_beat will read them), the vocal mode, and a
//! recording path for produce_song. Every button is the same MCP tool an AI
//! calls; the result's summary and reading show underneath.

use super::theme::Tokens;
use super::Studio;
use crate::project::Project;
use eframe::egui::{self, Color32, RichText};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

type Job = Option<(String, f64, Option<Result<Value, String>>)>;

/// Panel state (kept on the studio).
pub struct CreateState {
    pub prompt: String,
    pub lyrics: String,
    pub vocal: usize,
    pub path: String,
    pub mode: usize,
    pub seed: String,
    pub job: Arc<Mutex<Job>>,
}

impl Default for CreateState {
    fn default() -> Self {
        CreateState {
            prompt: "Bohemia type beat with a beat switch and a quiet section".into(),
            lyrics: "I still see your face in the morning light\nEvery little thing reminds me of the night\nOh I tried to let you go\nBut my heart don't know\n\nTere bina dil nahi lagda\nTere bina, oh, kuch nahi chalda\nStay with me, stay with me tonight\nHold me close till the morning light".into(),
            vocal: 0,
            path: String::new(),
            mode: 0,
            seed: String::new(),
            job: Arc::new(Mutex::new(None)),
        }
    }
}

const VOCAL: [&str; 4] = ["auto", "sing", "rap", "none"];
const MODES: [&str; 2] = ["rap", "sing"];

fn chip(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::none()
        .fill(color.gamma_multiply(0.25))
        .stroke(egui::Stroke::new(1.0_f32, color))
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(6.0, 2.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.0).color(Color32::WHITE));
        });
}

impl Studio {
    fn start_job(&mut self, tool: &str, args: Value, now: f64) {
        let job = self.create.job.clone();
        if matches!(job.lock().unwrap().as_ref(), Some((_, _, None))) {
            return; // one job at a time
        }
        *job.lock().unwrap() = Some((tool.to_string(), now, None));
        let engine = self.engine.clone();
        let tool = tool.to_string();
        std::thread::spawn(move || {
            let r = engine.lock().unwrap().call_from(&tool, &args, "you").map_err(|e| format!("{e:#}"));
            if let Some(j) = job.lock().unwrap().as_mut() {
                j.2 = Some(r);
            }
        });
    }

    pub(super) fn create_view(&mut self, ui: &mut egui::Ui, p: &Project) {
        // keep the panel inside what is visible (wide widgets above can widen the parent)
        let w = (ui.clip_rect().right() - ui.cursor().left() - 28.0).max(400.0);
        let h = ui.available_height();
        ui.allocate_ui(egui::vec2(w, h), |ui| {
            ui.set_max_width(w);
            egui::ScrollArea::vertical().id_salt("create_scroll").show(ui, |ui| self.create_inner(ui, p));
        });
    }

    fn create_inner(&mut self, ui: &mut egui::Ui, _p: &Project) {
        let tk = Tokens::DARK;
        let now = ui.input(|i| i.time);
        let busy = matches!(self.create.job.lock().unwrap().as_ref(), Some((_, _, None)));
        let mut start: Option<(&'static str, Value)> = None;
        ui.columns(2, |cols| {
            // ---------------- left: inputs
            let ui = &mut cols[0];
            ui.label(RichText::new("PROMPT").size(11.0).strong().color(tk.header_text));
            ui.add(egui::TextEdit::singleline(&mut self.create.prompt).desired_width(f32::INFINITY).hint_text("Bohemia type beat with a beat switch and a quiet section"));
            ui.add_space(6.0);
            ui.label(RichText::new("LYRICS  (optional: rap or sung lines; blank lines split stanzas)").size(11.0).strong().color(tk.header_text));
            egui::ScrollArea::vertical().id_salt("lyr").max_height(230.0).show(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut self.create.lyrics).desired_width(f32::INFINITY).desired_rows(11).font(egui::TextStyle::Monospace));
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("VOCAL").size(11.0).color(tk.text_dim));
                for (i, v) in VOCAL.iter().enumerate() {
                    ui.selectable_value(&mut self.create.vocal, i, *v);
                }
                ui.label(RichText::new("SEED").size(11.0).color(tk.text_dim));
                ui.add(egui::TextEdit::singleline(&mut self.create.seed).desired_width(70.0).hint_text("fresh"));
            });
            ui.add_space(4.0);
            let mb = ui.add_enabled(!busy, egui::Button::new(RichText::new("  MAKE BEAT  ").strong()).fill(tk.accent_dark));
            if mb.clicked() {
                let mut a = json!({"prompt": self.create.prompt, "vocal": VOCAL[self.create.vocal], "out_dir": "renders"});
                if !self.create.lyrics.trim().is_empty() {
                    a["lyrics"] = json!(self.create.lyrics);
                }
                if let Ok(s) = self.create.seed.trim().parse::<u64>() {
                    a["seed"] = json!(s);
                }
                start = Some(("make_beat", a));
            }
            ui.add_space(14.0);
            ui.separator();
            ui.label(RichText::new("RECORDING TO SONG  (produce_song)").size(11.0).strong().color(tk.header_text));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.create.path).desired_width(300.0).hint_text("path to a voice memo (wav / mp3 / ogg)"));
                for (i, m) in MODES.iter().enumerate() {
                    ui.selectable_value(&mut self.create.mode, i, *m);
                }
            });
            let ps = ui.add_enabled(!busy && !self.create.path.trim().is_empty(), egui::Button::new(RichText::new("  PRODUCE SONG  ").strong()).fill(tk.accent_dark));
            if ps.clicked() {
                start = Some(("produce_song", json!({"path": self.create.path.trim(), "mode": MODES[self.create.mode], "out_dir": "renders"})));
            }
            // ---------------- right: how it is read, live; then the result
            let ui = &mut cols[1];
            ui.label(RichText::new("HOW THE PROMPT READS").size(11.0).strong().color(tk.header_text));
            let pp = crate::prompt_beat::parse_prompt(&self.create.prompt);
            ui.horizontal_wrapped(|ui| {
                if let Some(g) = &pp.genre {
                    chip(ui, &format!("genre {g}"), Color32::from_rgb(64, 132, 196));
                }
                if let Some(b) = pp.bpm {
                    chip(ui, &format!("{b:.0} BPM"), Color32::from_rgb(64, 132, 196));
                }
                if let Some(s) = &pp.scale {
                    chip(ui, &format!("{} {s}", pp.key.clone().unwrap_or_default()), Color32::from_rgb(64, 132, 196));
                }
                if let Some(m) = &pp.mood {
                    chip(ui, &format!("mood {m}"), Color32::from_rgb(150, 110, 200));
                }
                for c in &pp.contrasts {
                    chip(ui, c, Color32::from_rgb(232, 120, 60));
                }
                if let Some(d) = pp.duration_s {
                    chip(ui, &format!("{d:.0} s"), Color32::from_rgb(120, 120, 135));
                }
            });
            for r in pp.reasons.iter().take(4) {
                ui.label(RichText::new(format!("\u{2022} {r}")).size(11.0).color(tk.text_dim));
            }
            if !self.create.lyrics.trim().is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new("HOW THE LYRICS READ").size(11.0).strong().color(tk.header_text));
                let lr = crate::prompt_beat::analyze_lyrics(&self.create.lyrics);
                ui.horizontal_wrapped(|ui| {
                    chip(ui, &lr.delivery, Color32::from_rgb(96, 200, 110));
                    chip(ui, &format!("mood {}", lr.mood), Color32::from_rgb(150, 110, 200));
                    chip(ui, &format!("{} {:.0} BPM", lr.genre, lr.bpm), Color32::from_rgb(64, 132, 196));
                    chip(ui, &format!("{:.1} syll/line", lr.syllables_per_line), Color32::from_rgb(120, 120, 135));
                });
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    chip(ui, "intro 4", Color32::from_rgb(120, 120, 135));
                    for s in &lr.sections {
                        let c = if s.kind == "hook" { Color32::from_rgb(232, 120, 60) } else { Color32::from_rgb(86, 140, 220) };
                        chip(ui, &format!("{} {} bars", s.kind, s.bars), c);
                    }
                    chip(ui, "outro 4", Color32::from_rgb(120, 120, 135));
                });
                for s in lr.sections.iter().take(6) {
                    ui.label(RichText::new(format!("{:>5}  \u{201C}{}\u{201D}", s.kind, s.first_line)).size(11.0).color(tk.text_dim).family(egui::FontFamily::Monospace));
                }
            }
            ui.add_space(10.0);
            ui.separator();
            let job = self.create.job.lock().unwrap().clone();
            match job {
                None => {
                    ui.label(RichText::new("Same tools over MCP: make_beat {prompt, lyrics, vocal}, produce_song {path, mode}, read_prompt, analyze_lyrics.").size(11.0).color(tk.text_dim));
                }
                Some((tool, t0, None)) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new(format!("{tool} running\u{2026} {:.0} s", now - t0)).color(tk.text));
                    });
                }
                Some((tool, _, Some(Err(e)))) => {
                    ui.label(RichText::new(format!("{tool} failed: {e}")).color(tk.meter_red));
                }
                Some((tool, _, Some(Ok(v)))) => {
                    ui.label(RichText::new(format!("{tool} done")).strong().color(tk.meter_green));
                    let s = &v["summary"];
                    if let Some(st) = s.as_str() {
                        ui.label(RichText::new(st).color(tk.text));
                    } else if s.is_object() {
                        ui.label(RichText::new(format!("{} \u{00B7} {} BPM \u{00B7} {} \u{00B7} {} s", s["genre"].as_str().unwrap_or(""), s["bpm"], s["key"].as_str().unwrap_or(""), s["seconds"])).color(tk.text));
                    }
                    let file = v["vocal"]["file"].as_str().or(v["file"].as_str()).or(v["files"]["audio"].as_str()).unwrap_or("");
                    if !file.is_empty() {
                        ui.label(RichText::new(file).size(11.0).color(tk.text_dim).family(egui::FontFamily::Monospace));
                    }
                }
            }
        });
        if let Some((tool, a)) = start {
            self.start_job(tool, a, now);
        }
        if busy {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}
