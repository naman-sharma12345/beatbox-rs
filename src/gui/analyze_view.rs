//! The analysis panel (in the browser view): pick a recording or a stem and
//! write it down by hand, one button per job: split into stems, drums to
//! MIDI, bass to MIDI, chords and key, riff or melody to MIDI, effects,
//! sections and parts, compare a render with it. Each button sends the same
//! tool an outside client would (separate_stems, transcribe_drums,
//! transcribe_bass, transcribe_chords, transcribe_melody, detect_fx,
//! arrangement_map, score_sections);
//! the work runs off the UI thread on its own engine over the same folder, so
//! the studio stays responsive, and "add to project" then runs import_midi on
//! the studio's engine.

use super::theme::Tokens;
use super::Studio;
use eframe::egui::{self, RichText};
use serde_json::{json, Value};

#[derive(Default)]
pub struct AnalyzeState {
    pub path: String,
    pub grid_from: String,
    pub bpm: String,
    pub add_to_project: bool,
    pub stems_dir: String,
    pub compare_with: String,
    pub job: Option<(String, std::sync::mpsc::Receiver<Result<Value, String>>)>,
    pub result: String,
}

/// The result as short readable lines (no paths' folders, no hints list).
fn result_lines(label: &str, v: &Value) -> String {
    let mut out = vec![format!("{label}: done")];
    if let Some(o) = v.as_object() {
        for (k, val) in o {
            if matches!(k.as_str(), "next" | "imported" | "detail") || val.is_null() {
                continue;
            }
            let txt = match val {
                Value::String(s) if k == "midi" || k.ends_with("path") => std::path::Path::new(s).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
                Value::String(s) => s.clone(),
                Value::Array(a) => a.iter().take(8).map(|x| x.as_str().map(String::from).unwrap_or_else(|| x.to_string())).collect::<Vec<_>>().join("  |  "),
                other => other.to_string(),
            };
            out.push(format!("{k}: {}", txt.chars().take(400).collect::<String>()));
        }
    }
    out.join("\n")
}

impl Studio {
    fn analyze_job(&mut self, label: String, ctx: egui::Context, tool: &'static str, args: Value) {
        if self.analyze.job.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let eng = self.engine.clone();
        let workdir = eng.lock().unwrap().workdir.clone();
        let add = self.analyze.add_to_project;
        std::thread::spawn(move || {
            let mut own = crate::engine::Engine::new(workdir);
            let mut r = own.call_from(tool, &args, "you").map_err(|e| format!("{e:#}"));
            if let (true, Ok(v)) = (add, r.as_mut()) {
                if let Some(m) = v["midi"].as_str().map(String::from) {
                    let name = std::path::Path::new(&m).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    let imp = eng.lock().unwrap().call_from("import_midi", &json!({"path": m, "pattern": name, "set_tempo": false}), "you");
                    v["added_to_project"] = json!(imp.is_ok());
                }
            }
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.analyze.job = Some((label, rx));
        self.analyze.result.clear();
    }

    pub(super) fn analyze_panel(&mut self, ui: &mut egui::Ui) {
        let tk = Tokens::DARK;
        if let Some((label, rx)) = &self.analyze.job {
            if let Ok(r) = rx.try_recv() {
                self.analyze.result = match r {
                    Ok(v) => result_lines(label, &v),
                    Err(e) => format!("{label} failed: {e}"),
                };
                self.analyze.job = None;
            }
        }
        let busy = self.analyze.job.as_ref().map(|j| j.0.clone());
        let mut job: Option<(&'static str, Value)> = None;
        egui::CollapsingHeader::new(RichText::new("ANALYZE A RECORDING").size(11.0).strong().color(tk.header_text))
            .id_salt("br_analyze")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("FILE").size(11.0).color(tk.text_dim));
                    ui.add(egui::TextEdit::singleline(&mut self.analyze.path).desired_width(320.0).hint_text("song.wav, or a stem such as drums.wav"));
                    ui.label(RichText::new("BARS FROM").size(11.0).color(tk.text_dim));
                    ui.add(egui::TextEdit::singleline(&mut self.analyze.grid_from).desired_width(200.0).hint_text("full mix (optional)"));
                    ui.label(RichText::new("BPM").size(11.0).color(tk.text_dim));
                    ui.add(egui::TextEdit::singleline(&mut self.analyze.bpm).desired_width(50.0).hint_text("auto"));
                    ui.checkbox(&mut self.analyze.add_to_project, "add to project");
                });
                let path = self.analyze.path.trim().to_string();
                let mut args = json!({"path": path});
                if !self.analyze.grid_from.trim().is_empty() {
                    args["grid_from"] = json!(self.analyze.grid_from.trim());
                }
                if let Ok(b) = self.analyze.bpm.trim().parse::<f32>() {
                    args["bpm"] = json!(b);
                }
                let ready = !path.is_empty() && busy.is_none();
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(ready, egui::Button::new("SPLIT INTO STEMS").small()).on_hover_text(format!("separate_stems {}", json!({"path": path}))).clicked() {
                        job = Some(("separate_stems", json!({"path": path})));
                    }
                    if ui.add_enabled(ready, egui::Button::new("DRUMS TO MIDI").small()).on_hover_text(format!("transcribe_drums {args}")).clicked() {
                        job = Some(("transcribe_drums", args.clone()));
                    }
                    if ui.add_enabled(ready, egui::Button::new("BASS TO MIDI").small()).on_hover_text(format!("transcribe_bass {args}")).clicked() {
                        job = Some(("transcribe_bass", args.clone()));
                    }
                    if ui.add_enabled(ready, egui::Button::new("CHORDS + KEY").small()).on_hover_text(format!("transcribe_chords {args}")).clicked() {
                        job = Some(("transcribe_chords", args.clone()));
                    }
                    if ui.add_enabled(ready, egui::Button::new("RIFF / MELODY TO MIDI").small()).on_hover_text(format!("transcribe_melody {args}")).clicked() {
                        job = Some(("transcribe_melody", args.clone()));
                    }
                    if ui.add_enabled(ready, egui::Button::new("EFFECTS").small()).on_hover_text(format!("detect_fx {args}")).clicked() {
                        job = Some(("detect_fx", args.clone()));
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("STEMS FOLDER").size(11.0).color(tk.text_dim));
                    ui.add(egui::TextEdit::singleline(&mut self.analyze.stems_dir).desired_width(220.0).hint_text("from SPLIT INTO STEMS (optional)"));
                    let mut sa = json!({"path": path});
                    if !self.analyze.stems_dir.trim().is_empty() {
                        sa["stems_dir"] = json!(self.analyze.stems_dir.trim());
                    }
                    if let Some(b) = args.get("bpm") {
                        sa["bpm"] = b.clone();
                    }
                    if ui.add_enabled(ready, egui::Button::new("SECTIONS + PARTS").small()).on_hover_text(format!("arrangement_map {sa}")).clicked() {
                        job = Some(("arrangement_map", sa));
                    }
                    ui.label(RichText::new("COMPARE WITH").size(11.0).color(tk.text_dim));
                    ui.add(egui::TextEdit::singleline(&mut self.analyze.compare_with).desired_width(220.0).hint_text("your render.wav"));
                    let ca = json!({"ours": self.analyze.compare_with.trim(), "reference": path, "clips": true});
                    if ui.add_enabled(ready && !self.analyze.compare_with.trim().is_empty(), egui::Button::new("COMPARE").small()).on_hover_text(format!("score_sections {ca}")).clicked() {
                        job = Some(("score_sections", ca));
                    }
                });
                if let Some(b) = &busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new(format!("{b}...")).size(11.0).color(tk.text_dim));
                    });
                } else if !self.analyze.result.is_empty() {
                    egui::ScrollArea::vertical().id_salt("br_analyze_out").max_height(180.0).show(ui, |ui| {
                        ui.label(RichText::new(&self.analyze.result).size(11.0).monospace().color(tk.text));
                    });
                }
            });
        if let Some((tool, args)) = job {
            let label = tool.replace('_', " ");
            self.analyze_job(label, ui.ctx().clone(), tool, args);
        }
    }
}
