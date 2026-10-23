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
/// Reserve a new target atomically. Neither repeated exports nor existing stems
/// are replaced: a numeric suffix is added when the requested name is taken.
fn reserve_target(e: &crate::engine::Engine, kind: usize, args: &mut Value) -> std::io::Result<()> {
    let key = if kind == 1 { "dir" } else { "path" };
    let extension = match kind {
        2 => "mid",
        3 => "wav",
        _ => args["format"].as_str().unwrap_or("wav"),
    };
    let name = crate::samples::sample_name(&e.project.name);
    let default = if kind == 1 {
        format!("renders/{name}_stems")
    } else if kind == 3 {
        format!("renders/{name}_click.wav")
    } else {
        format!("renders/{name}.{extension}")
    };
    let base = e.resolve(args[key].as_str().unwrap_or(&default));
    if let Some(parent) = base.parent() {
        std::fs::create_dir_all(parent)?;
    }
    for n in 0..10000 {
        let target = if n == 0 {
            base.clone()
        } else {
            let stem = if kind == 1 {
                base.file_name()
            } else {
                base.file_stem()
            }
            .unwrap_or_default()
            .to_string_lossy();
            let suffix = if kind != 1 {
                base.extension()
                    .map(|x| format!(".{}", x.to_string_lossy()))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            base.with_file_name(format!("{stem}_{n}{suffix}"))
        };
        let result = if kind == 1 {
            std::fs::create_dir(&target)
        } else {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map(|_| ())
        };
        match result {
            Ok(()) => {
                args[key] = json!(target);
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "too many exports with the same name",
    ))
}

fn file_size(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} bytes")
    }
}

/// A short plain summary of a finished export: where it went, what it is, and
/// the measured loudness. Measurements describe level only, not quality.
fn export_summary(v: &Value) -> String {
    let mut t = match v["path"].as_str().or(v["dir"].as_str()) {
        Some(p) => format!("Exported to {p}\n"),
        None => "Export finished\n".to_string(),
    };
    let mut facts = Vec::new();
    if let Some(n) = v["stems"].as_array().map(|a| a.len()) {
        facts.push(format!("{n} files"));
    }
    if let Some(f) = v["format"].as_str() {
        facts.push(f.to_uppercase());
    }
    if let Some(b) = v["bits"].as_u64() {
        facts.push(format!("{b}-bit"));
    }
    if let Some(r) = v["sample_rate"].as_u64() {
        facts.push(format!("{r} Hz"));
    }
    if let Some(b) = v["bytes"].as_u64() {
        facts.push(file_size(b));
    }
    if let Some(s) = v["seconds"].as_f64() {
        facts.push(format!("{s:.1} s"));
    }
    if !facts.is_empty() {
        t.push_str(&facts.join(", "));
        t.push('\n');
    }
    if let (Some(l), Some(tp)) = (
        v["after"]["integrated_lufs"].as_f64(),
        v["after"]["true_peak_dbtp"].as_f64(),
    ) {
        t.push_str(&format!("Loudness {l:.1} LUFS, true peak {tp:.1} dBTP\n"));
    }
    if let Some(w) = v["warnings"].as_array() {
        for w in w.iter().filter_map(|w| w.as_str()) {
            t.push_str(&format!("Warning: {w}\n"));
        }
    }
    t
}

impl Studio {
    pub(super) fn export_window(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.export.job {
            match rx.try_recv() {
                Ok(r) => {
                    self.export.result = match r {
                        Ok(v) => export_summary(&v),
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
                ui.label("Paths are relative to the workspace. Existing files/folders are kept; a numbered name is used.");
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
            let (tool, mut args) = self.export.request(pattern);
            let kind = self.export.kind;
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx = ctx.clone();
            self.export.result.clear();
            self.export.job = Some(rx);
            std::thread::spawn(move || {
                let mut own = crate::engine::Engine::new(workdir);
                own.project = project;
                own.bank = bank;
                let r = reserve_target(&own, kind, &mut args)
                    .map_err(|e| e.to_string())
                    .and_then(|_| {
                        own.call_from(tool, &args, "you")
                            .map_err(|e| format!("{e:#}"))
                    });
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
    fn export_summary_is_plain_and_keeps_path_and_levels() {
        let t = export_summary(
            &json!({"path":"/o/a.wav","format":"wav","bits":24,"sample_rate":44100,
            "bytes":2097152,"seconds":3.5,"after":{"integrated_lufs":-10.64,"true_peak_dbtp":-1.43},
            "warnings":["clipped"]}),
        );
        assert!(t.starts_with("Exported to /o/a.wav\n"));
        assert!(t.contains("WAV, 24-bit, 44100 Hz, 2.0 MB, 3.5 s"));
        assert!(t.contains("Loudness -10.6 LUFS, true peak -1.4 dBTP"));
        assert!(t.contains("Warning: clipped"));
        assert!(!t.contains("integrated lufs"));
        assert!(export_summary(&json!({"path":"/o/a.mid","bytes":7618})).contains("7.4 KB"));
        assert!(export_summary(&json!({"path":"/o/a.mid","bytes":300})).contains("300 bytes"));
        assert!(export_summary(&json!({"dir":"/o/s","stems":[{},{}]})).contains("2 files"));
        assert!(export_summary(&json!({"dir":"/o/stems"})).starts_with("Exported to /o/stems"));
    }
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

#[cfg(test)]
mod output_tests {
    use super::*;
    #[test]
    fn all_export_tabs_write_real_files_from_their_requests() {
        let dir = std::env::temp_dir().join(format!("gui_export_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = crate::engine::Engine::new(dir.clone());
        e.call("add_track", &json!({"name":"lead", "preset":"sub_bass"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"tiny", "bars":1}))
            .unwrap();
        e.call("add_notes", &json!({"pattern":"tiny", "track":"lead", "notes":[{"pitch":60,"start":0,"len":1,"vel":0.6}]})).unwrap();
        e.call(
            "set_arrangement",
            &json!({"sections":[{"pattern":"tiny","repeats":1}]}),
        )
        .unwrap();
        for kind in 0..4 {
            let name = match kind {
                1 => "stems",
                2 => "song.mid",
                3 => "click.wav",
                _ => "song.wav",
            };
            let s = ExportState {
                kind,
                path: name.into(),
                ..Default::default()
            };
            let (tool, mut args) = s.request("tiny");
            reserve_target(&e, kind, &mut args).unwrap();
            let r = e
                .call(tool, &args)
                .unwrap_or_else(|err| panic!("{tool}: {err:#}"));
            let key = if kind == 1 { "dir" } else { "path" };
            let path = std::path::PathBuf::from(r[key].as_str().unwrap());
            assert!(path.exists(), "missing output {path:?}");
            if kind != 1 {
                assert!(std::fs::metadata(path).unwrap().len() > 44);
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod collision_tests {
    use super::*;
    #[test]
    fn repeated_file_and_stem_exports_keep_existing_outputs() {
        let dir = std::env::temp_dir().join(format!("gui_export_collision_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let e = crate::engine::Engine::new(dir.clone());
        for kind in [0, 1, 2, 3] {
            let key = if kind == 1 { "dir" } else { "path" };
            let base = dir.join(format!("target_{kind}.wav"));
            if kind == 1 {
                std::fs::create_dir_all(&base).unwrap();
                std::fs::write(base.join("keep.wav"), b"keep").unwrap();
            } else {
                std::fs::write(&base, b"keep").unwrap();
            }
            let mut a = json!({key:base});
            reserve_target(&e, kind, &mut a).unwrap();
            assert_ne!(a[key], json!(base));
            assert_eq!(
                std::fs::read(if kind == 1 {
                    base.join("keep.wav")
                } else {
                    base
                })
                .unwrap(),
                b"keep"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
