//! Manual file delivery, rendered from a snapshot so the studio stays responsive.
use super::Studio;
use eframe::egui;
use serde_json::{json, Value};
use std::sync::mpsc::Receiver;

pub(super) struct ExportState {
    pub open: bool,
    kind: usize,
    path: String,
    format: String,
    bits: u32,
    rate: u32,
    normalize: bool,
    lufs: f32,
    ceiling: f32,
    pattern_only: bool,
    count_in: u32,
    job: Option<Receiver<Result<Value, String>>>,
    result: String,
}
impl Default for ExportState {
    fn default() -> Self {
        Self {
            open: false,
            kind: 0,
            path: String::new(),
            format: "wav".into(),
            bits: 24,
            rate: 44100,
            normalize: false,
            lufs: -14.0,
            ceiling: -1.0,
            pattern_only: false,
            count_in: 1,
            job: None,
            result: String::new(),
        }
    }
}
impl ExportState {
    fn request(&self, pattern: &str) -> (&'static str, Value) {
        let (tool, mut args) = match self.kind {
            1 => (
                "export_stems",
                json!({"format": self.format, "bits": self.bits,
                "sample_rate": self.rate, "stem_gain": "common", "include_buses": true, "mode": "pre_master"}),
            ),
            2 => (
                "export_midi",
                if self.pattern_only {
                    json!({"scope": "pattern", "pattern": pattern})
                } else {
                    json!({"scope": "song"})
                },
            ),
            3 => ("export_click", json!({"count_in_bars": self.count_in})),
            _ => {
                let mut args = json!({"format": self.format, "bits": self.bits,
                    "sample_rate": self.rate, "true_peak_ceiling": self.ceiling, "limit": true});
                if self.normalize {
                    args["target_lufs"] = json!(self.lufs);
                }
                ("export_audio", args)
            }
        };
        if !self.path.trim().is_empty() {
            args[if self.kind == 1 { "dir" } else { "path" }] = json!(self.path.trim());
        }
        (tool, args)
    }
}
impl Studio {
    pub(super) fn export_window(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.export.job {
            match rx.try_recv() {
                Ok(r) => {
                    self.export.result = match r {
                        Ok(v) => {
                            let mut text = String::new();
                            super::views::plain_report(&v, 0, &mut text);
                            text
                        }
                        Err(e) => format!("Export failed: {e}"),
                    };
                    self.export.job = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.export.result = "Export stopped before returning a result".into();
                    self.export.job = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if !self.export.open {
            return;
        }
        let mut open = true;
        let mut start = false;
        egui::Window::new("Export").open(&mut open).default_width(580.0).resizable(true).show(ctx, |ui| {
            let busy = self.export.job.is_some();
            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (i, label) in ["Audio", "Stems", "MIDI", "Click track"].iter().enumerate() {
                        if ui.selectable_value(&mut self.export.kind, i, *label).changed() { self.export.path.clear(); self.export.result.clear(); }
                    }
                });
                ui.separator();
                ui.label(if self.export.kind == 1 { "Output folder (blank: project stems folder)" } else { "Output file (blank: project name in renders folder)" });
                ui.add(egui::TextEdit::singleline(&mut self.export.path).desired_width(f32::INFINITY));
                ui.label("Paths are relative to the project workspace. Existing files at that path are replaced.");
                if self.export.kind < 2 {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Format");
                        egui::ComboBox::from_id_salt("export_format").selected_text(self.export.format.to_uppercase()).show_ui(ui, |ui| {
                            for f in ["wav", "flac", "mp3"] { ui.selectable_value(&mut self.export.format, f.to_string(), f.to_uppercase()); }
                        });
                        if self.export.format != "mp3" {
                            if self.export.format == "flac" && self.export.bits == 32 { self.export.bits = 24; }
                            egui::ComboBox::from_id_salt("export_bits").selected_text(format!("{} bit", self.export.bits)).show_ui(ui, |ui| {
                                for b in [16,24,32] { if b != 32 || self.export.format == "wav" { ui.selectable_value(&mut self.export.bits, b, format!("{b} bit")); } }
                            });
                        } else { ui.label("320 kbps; requires ffmpeg"); }
                        egui::ComboBox::from_id_salt("export_rate").selected_text(format!("{} Hz", self.export.rate)).show_ui(ui, |ui| {
                            for r in [44100,48000] { ui.selectable_value(&mut self.export.rate, r, format!("{r} Hz")); }
                        });
                    });
                    if self.export.kind == 0 {
                        ui.horizontal_wrapped(|ui| {
                            ui.checkbox(&mut self.export.normalize, "Normalize loudness");
                            ui.add_enabled(self.export.normalize, egui::DragValue::new(&mut self.export.lufs).range(-30.0..=-5.0).suffix(" LUFS"));
                            ui.label("Peak ceiling");
                            ui.add(egui::DragValue::new(&mut self.export.ceiling).range(-12.0..=0.0).speed(0.1).suffix(" dBTP"));
                        });
                        ui.label("Encoded audio is decoded and checked for loudness and true peak.");
                    } else {
                        ui.label("All tracks and buses, aligned at zero, before the master chain. One common trim keeps their relative levels.");
                    }
                } else if self.export.kind == 2 {
                    ui.checkbox(&mut self.export.pattern_only, "Selected pattern only (otherwise full song)");
                    ui.label("MIDI contains notes and timing, not audio clips or effect sounds.");
                } else {
                    ui.horizontal(|ui| { ui.label("Count-in bars"); ui.add(egui::DragValue::new(&mut self.export.count_in).range(0..=8)); });
                    ui.label("Follows the song tempo and meter; accents each downbeat.");
                }
                if ui.button("Export file(s)").clicked() { start = true; }
            });
            if busy { ui.horizontal(|ui| { ui.spinner(); ui.label("Exporting a snapshot of the project..."); }); }
            if !self.export.result.is_empty() {
                ui.separator();
                egui::ScrollArea::vertical().id_salt("export_result").max_height(180.0).show(ui, |ui| { ui.label(&self.export.result); });
            }
        });
        self.export.open = open;
        if start {
            let (project, bank, workdir) = {
                let e = self.engine.lock().unwrap();
                (e.project.clone(), e.bank.clone(), e.workdir.clone())
            };
            let pattern = project
                .patterns
                .get(self.pattern)
                .map(|p| p.name.as_str())
                .unwrap_or("");
            let (tool, args) = self.export.request(pattern);
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx = ctx.clone();
            self.export.result.clear();
            self.export.job = Some(rx);
            std::thread::spawn(move || {
                let mut own = crate::engine::Engine::new(workdir);
                own.project = project;
                own.bank = bank;
                let r = own
                    .call_from(tool, &args, "you")
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(r);
                ctx.request_repaint();
            });
        }
        if self.export.job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_requests_match_output_kind_and_scope() {
        let mut s = ExportState {
            path: "out/song.flac".into(),
            format: "flac".into(),
            normalize: true,
            ..Default::default()
        };
        let (tool, a) = s.request("verse");
        assert_eq!(tool, "export_audio");
        assert_eq!(a["target_lufs"], -14.0);
        assert_eq!(a["path"], "out/song.flac");
        s.kind = 1;
        s.path = "out/stems".into();
        let (tool, a) = s.request("verse");
        assert_eq!(tool, "export_stems");
        assert_eq!(a["dir"], "out/stems");
        assert!(a.get("path").is_none());
        assert_eq!(a["stem_gain"], "common");
        s.kind = 2;
        s.pattern_only = true;
        let (tool, a) = s.request("verse");
        assert_eq!(tool, "export_midi");
        assert_eq!(a["pattern"], "verse");
        s.kind = 3;
        s.path.clear();
        let (tool, a) = s.request("verse");
        assert_eq!(tool, "export_click");
        assert_eq!(a["count_in_bars"], 1);
        assert!(a.get("path").is_none());
    }
}
