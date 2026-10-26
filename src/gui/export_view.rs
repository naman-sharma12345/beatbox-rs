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
    marked_range: bool,
    section_range: bool,
    section_index: usize,
    start_marker: String,
    end_marker: String,
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
            marked_range: false,
            section_range: false,
            section_index: 0,
            start_marker: String::new(),
            end_marker: String::new(),
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
/// Resolve a saved-marker excerpt against the export snapshot, before reserving
/// any file. Seconds share the renderer's tempo curve, including ramps.
fn apply_marker_range(
    p: &crate::project::Project,
    start: &str,
    end: &str,
    args: &mut Value,
) -> Result<(), String> {
    let find = |name: &str| {
        p.markers
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| format!("Choose a saved marker: {name}"))
    };
    let first = find(start)?;
    let last = find(end)?;
    if !first.beat.is_finite()
        || !last.beat.is_finite()
        || first.beat < 0.0
        || last.beat <= first.beat
    {
        return Err("End marker must be after the start marker".into());
    }
    if last.beat > p.song_beats() {
        return Err("End marker is past the end of the song".into());
    }
    let curve = crate::tempo_curve::TempoCurve::new(p);
    args["start_s"] = json!(curve.secs_at_beat(first.beat as f64));
    args["end_s"] = json!(curve.secs_at_beat(last.beat as f64));
    Ok(())
}

/// Resolve a whole section against the immutable export snapshot before
/// reserving a target. Uses unrounded internal boundary seconds.
pub fn apply_section_range(
    p: &crate::project::Project,
    index: usize,
    args: &mut Value,
) -> Result<String, String> {
    let rows = crate::tools_markers::arrangement_positions(p).map_err(|e| e.to_string())?;
    let row = rows
        .get(index)
        .ok_or_else(|| "Choose an existing song section".to_string())?;
    let start = row["start_seconds"]
        .as_f64()
        .ok_or("Invalid section start")?;
    let end = row["end_seconds"].as_f64().ok_or("Invalid section end")?;
    if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
        return Err("Section must have a finite positive duration".into());
    }
    args["start_s"] = json!(start);
    args["end_s"] = json!(end);
    Ok(format!(
        "{}: {} x{}",
        index + 1,
        row["pattern"].as_str().unwrap_or("pattern"),
        row["repeats"]
    ))
}

/// Validate and describe an excerpt before reserving any output file.
pub fn range_preview(
    p: &crate::project::Project,
    section: Option<usize>,
    markers: Option<(&str, &str)>,
) -> Result<String, String> {
    if section.is_some() && markers.is_some() {
        return Err("Choose a section OR a marker range, not both".into());
    }
    let mut args = json!({});
    let label = if let Some(index) = section {
        apply_section_range(p, index, &mut args)?
    } else if let Some((first, last)) = markers {
        apply_marker_range(p, first, last, &mut args)?;
        format!("{first} to {last}")
    } else {
        return Ok("Full song, including the exporter's normal effect tail".into());
    };
    let start = args["start_s"].as_f64().ok_or("Invalid range start")?;
    let end = args["end_s"].as_f64().ok_or("Invalid range end")?;
    Ok(format!(
        "{label}: {start:.3}s to {end:.3}s | {:.3}s | no extra end tail",
        end - start
    ))
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
    // A name typed without a file type gets the chosen format's extension,
    // so the export opens in any player.
    let base = if kind != 1 && base.extension().is_none() {
        base.with_extension(extension)
    } else {
        base
    };
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
    if let Some(window) = v["marker_range"].as_str() {
        t.push_str(&format!("Markers: {window}\n"));
    }
    if let Some(section) = v["section_range"].as_str() {
        t.push_str(&format!("Section: {section}\n"));
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
                        if ui.checkbox(&mut self.export.section_range,"One song section").changed() && self.export.section_range { self.export.marked_range=false; }
                        if self.export.section_range {
                            let project=self.engine.lock().unwrap().project.clone();
                            match crate::tools_markers::arrangement_positions(&project) {
                                Ok(rows) => {
                                    let selected=rows.get(self.export.section_index).map(|r| format!("{}: {} x{}", self.export.section_index+1,r["pattern"].as_str().unwrap_or("pattern"),r["repeats"])).unwrap_or_else(|| "Choose section".into());
                                    egui::ComboBox::from_id_salt("export_song_section").selected_text(selected).show_ui(ui,|ui| {
                                        for (i,row) in rows.iter().enumerate() {
                                            ui.selectable_value(&mut self.export.section_index,i,format!("{}: {} x{}",i+1,row["pattern"].as_str().unwrap_or("pattern"),row["repeats"]));
                                        }
                                    });
                                    ui.label("Includes every repeat of this section, with tempo and meter changes. No extra ring-out after its end. Uses the section list at the moment you export.");
                                }
                                Err(err) => {ui.label(format!("Section export unavailable: {err}"));}
                            }
                        }
                        if ui.checkbox(&mut self.export.marked_range,"Between saved markers").changed() && self.export.marked_range { self.export.section_range=false; }
                        if self.export.marked_range {
                            let markers={self.engine.lock().unwrap().project.markers.clone()};
                            ui.horizontal_wrapped(|ui| {
                                ui.label("Start");
                                egui::ComboBox::from_id_salt("export_start_marker").selected_text(if self.export.start_marker.is_empty() {"Choose marker"} else {&self.export.start_marker}).show_ui(ui,|ui| {
                                    for m in &markers {ui.selectable_value(&mut self.export.start_marker,m.name.clone(),&m.name);}
                                });
                                ui.label("End");
                                egui::ComboBox::from_id_salt("export_end_marker").selected_text(if self.export.end_marker.is_empty() {"Choose marker"} else {&self.export.end_marker}).show_ui(ui,|ui| {
                                    for m in &markers {ui.selectable_value(&mut self.export.end_marker,m.name.clone(),&m.name);}
                                });
                            });
                            ui.label("Exports only this interval, with tempo changes. No extra ring-out after the end marker.");
                        }

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
                let preview = if self.export.kind==0 {
                    let project=self.engine.lock().unwrap().project.clone();
                    range_preview(&project,
                        self.export.section_range.then_some(self.export.section_index),
                        self.export.marked_range.then_some((self.export.start_marker.as_str(),self.export.end_marker.as_str())))
                } else { Ok(String::new()) };
                match &preview {
                    Ok(text) if !text.is_empty() => {ui.label(text);}
                    Err(err) => {ui.colored_label(egui::Color32::from_rgb(240,150,110),format!("Choose a valid range: {err}"));}
                    _ => {}
                }
                if ui.add_enabled(preview.is_ok(),egui::Button::new("Export file(s)")).clicked() { start = true; }
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
            let marker_range = if kind == 0 && self.export.marked_range {
                Some((
                    self.export.start_marker.clone(),
                    self.export.end_marker.clone(),
                ))
            } else {
                None
            };
            if let Some((start, end)) = &marker_range {
                if let Err(err) = apply_marker_range(&project, start, end, &mut args) {
                    self.export.result = format!("Export failed: {err}");
                    return;
                }
            }

            let section_range = if kind == 0 && self.export.section_range {
                match apply_section_range(&project, self.export.section_index, &mut args) {
                    Ok(label) => Some(label),
                    Err(err) => {
                        self.export.result = format!("Export failed: {err}");
                        return;
                    }
                }
            } else {
                None
            };

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
                let r = r.map(|mut v| {
                    if let Some(label) = section_range {
                        v["section_range"] = json!(label);
                    }
                    if let Some((start, end)) = marker_range {
                        v["marker_range"] = json!(format!("{start} to {end}"));
                    }
                    v
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

#[cfg(test)]
mod extension_tests {
    use super::*;

    fn reserved(e: &crate::engine::Engine, kind: usize, args: Value) -> std::path::PathBuf {
        let key = if kind == 1 { "dir" } else { "path" };
        let mut a = args;
        reserve_target(e, kind, &mut a).unwrap();
        std::path::PathBuf::from(a[key].as_str().unwrap())
    }

    #[test]
    fn typed_names_without_a_type_get_the_selected_formats_extension() {
        let dir = std::env::temp_dir().join(format!("gui_export_ext_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let e = crate::engine::Engine::new(dir.clone());
        let ext = |p: &std::path::Path| p.extension().map(|x| x.to_string_lossy().to_string());

        // audio: the chosen format decides the extension
        let wav = reserved(&e, 0, json!({"path": dir.join("a_song"), "format": "wav"}));
        assert_eq!(ext(&wav).as_deref(), Some("wav"));
        let flac = reserved(&e, 0, json!({"path": dir.join("b_song"), "format": "flac"}));
        assert_eq!(ext(&flac).as_deref(), Some("flac"));
        // MIDI and click track ignore the audio format
        let mid = reserved(&e, 2, json!({"path": dir.join("c_song"), "format": "flac"}));
        assert_eq!(ext(&mid).as_deref(), Some("mid"));
        let click = reserved(&e, 3, json!({"path": dir.join("d_song"), "format": "flac"}));
        assert_eq!(ext(&click).as_deref(), Some("wav"));
        // a name that already has a type keeps it
        let kept = reserved(
            &e,
            0,
            json!({"path": dir.join("e_song.mp3"), "format": "wav"}),
        );
        assert_eq!(ext(&kept).as_deref(), Some("mp3"));
        // a stems folder is a folder: no extension is added
        let folder = reserved(&e, 1, json!({"dir": dir.join("f_stems")}));
        assert_eq!(ext(&folder), None);
        // a taken name is never replaced: the typed name still gets a number
        std::fs::write(dir.join("g_song.wav"), b"keep").unwrap();
        let next = reserved(&e, 0, json!({"path": dir.join("g_song"), "format": "wav"}));
        assert_ne!(next, dir.join("g_song.wav"));
        assert_eq!(ext(&next).as_deref(), Some("wav"));
        assert_eq!(std::fs::read(dir.join("g_song.wav")).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod marker_export_tests {
    use super::*;
    #[test]
    fn marker_ranges_use_tempo_curve_and_reject_bad_boundaries() {
        let mut p = crate::project::Project {
            bpm: 120.0,
            ..Default::default()
        };
        p.tempo_points = vec![crate::project::TempoPoint {
            beat: 4.0,
            bpm: 60.0,
            ramp: false,
        }];
        p.markers = vec![
            crate::project::Marker {
                name: "in".into(),
                beat: 2.0,
                time_signature: None,
            },
            crate::project::Marker {
                name: "out".into(),
                beat: 5.0,
                time_signature: None,
            },
        ];
        let mut a = json!({"format":"wav"});
        apply_marker_range(&p, "in", "out", &mut a).unwrap();
        assert_eq!(a["start_s"], 1.0);
        assert_eq!(a["end_s"], 3.0);
        assert_eq!(a["format"], "wav");
        for (start, end) in [("out", "in"), ("in", "in"), ("missing", "out")] {
            let mut a = json!({});
            assert!(apply_marker_range(&p, start, end, &mut a).is_err());
            assert_eq!(a, json!({}));
        }
        p.markers[1].beat = 100000.0;
        assert!(apply_marker_range(&p, "in", "out", &mut json!({})).is_err());
    }
}

#[cfg(test)]
mod marker_output_tests {
    use super::*;
    #[test]
    fn saved_renamed_markers_and_ramps_export_real_unchanged_snapshot() {
        let dir = std::env::temp_dir().join(format!("marker-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = crate::engine::Engine::new(dir.clone());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call(
            "add_notes",
            &json!({"track":"keys","notes":[{"start":8,"len":4,"pitch":60,"vel":0.8}]}),
        )
        .unwrap();
        e.call("add_tempo_point", &json!({"beat":4,"bpm":60,"ramp":false}))
            .unwrap();
        e.call("add_tempo_point", &json!({"beat":8,"bpm":100,"ramp":true}))
            .unwrap();
        for (name, beat) in [("In", 2.0), ("Out", 6.0)] {
            e.call("add_marker", &json!({"name":name,"beat":beat}))
                .unwrap();
        }
        e.call("rename_marker", &json!({"name":"In","new_name":"Intro"}))
            .unwrap();
        e.call("save_project", &json!({"path":"roundtrip.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"roundtrip.json"}))
            .unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let curve = crate::tempo_curve::TempoCurve::new(&e.project);
        let expected = curve.secs_at_beat(6.) - curve.secs_at_beat(2.);
        for format in ["wav", "flac"] {
            let mut a = json!({"path":format!("excerpt.{format}"),"format":format,"bits":24,"sample_rate":48000,"limit":true});
            apply_marker_range(&e.project, "Intro", "Out", &mut a).unwrap();
            reserve_target(&e, 0, &mut a).unwrap();
            let v = e.call("export_audio", &a).unwrap();
            assert!((v["seconds"].as_f64().unwrap() - expected).abs() < 0.0051);
            let decoded = crate::samples::decode_stereo_native(std::path::Path::new(
                a["path"].as_str().unwrap(),
            ))
            .unwrap();
            assert_eq!(decoded.2, 48000);
            assert!((decoded.0.len() as f64 / 48000. - expected).abs() < 0.001);
            assert!(decoded.0.iter().any(|s| s.abs() > 0.001));
            assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        }
        let mut a = json!({});
        assert!(apply_marker_range(&e.project, "In", "Out", &mut a).is_err());
        assert_eq!(a, json!({}));
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod section_export_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/section-export-test".into());
        e.call(
            "load_project",
            &json!({"path":"/tmp/recorded-beat-project/recorded-groove.json"}),
        )
        .unwrap();
        e.project.patterns[0].bars = 2;
        e.project.patterns[0].meter = Some((7, 8));
        e.call(
            "duplicate_pattern",
            &json!({"pattern":"groove","new_name":"Break"}),
        )
        .unwrap();
        e.project.patterns[1].bars = 1;
        e.project.patterns[1].meter = Some((3, 4));
        let id = e.project.patterns[0].id.clone();
        e.call(
            "set_arrangement",
            &json!({"sections":[{"pattern":id,"repeats":2},{"pattern":"BREAK","repeats":3}]}),
        )
        .unwrap();
        e.call("add_tempo_point", &json!({"beat":7,"bpm":120,"ramp":true}))
            .unwrap();
        e.call("add_tempo_point", &json!({"beat":16,"bpm":75}))
            .unwrap();
        e
    }
    #[test]
    fn section_export_exact_boundary_scope() {
        let e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        let mut args = json!({"format":"wav","bits":24});
        let label = beatbox::gui::section_export_range(&e.project, 1, &mut args).unwrap();
        assert_eq!(label, "2: Break x3");
        let c = beatbox::tempo_curve::TempoCurve::new(&e.project);
        assert_eq!(args["start_s"].as_f64().unwrap(), c.secs_at_beat(14.));
        assert_eq!(args["end_s"].as_f64().unwrap(), c.secs_at_beat(23.));
        assert_eq!(args["bits"], 24);
        assert_eq!(b, serde_json::to_value(&e.project).unwrap());
    }
    #[test]
    fn section_export_snapshot_and_rejects() {
        let mut e = fixture();
        let p = e.project.clone();
        e.call("set_section_repeats", &json!({"section":0,"repeats":3}))
            .unwrap();
        let mut a = json!({});
        beatbox::gui::section_export_range(&p, 1, &mut a).unwrap();
        let mut b = json!({});
        beatbox::gui::section_export_range(&e.project, 1, &mut b).unwrap();
        assert_ne!(a, b);
        let original = json!({"keep":true});
        let mut a = original.clone();
        assert!(beatbox::gui::section_export_range(&p, 2, &mut a).is_err());
        assert_eq!(a, original);
        e.project.arrangement[0].pattern = "missing".into();
        assert!(beatbox::gui::section_export_range(&e.project, 1, &mut a).is_err());
        assert_eq!(a, original);
        e.project.arrangement.clear();
        e.call("place_pattern", &json!({"pattern":"groove","start_bar":0}))
            .unwrap();
        assert!(beatbox::gui::section_export_range(&e.project, 0, &mut a).is_err());
        assert_eq!(a, original);
    }
    #[test]
    fn section_export_implicit_and_render_excerpt() {
        let mut e = fixture();
        e.project.arrangement.clear();
        let mut a = json!({});
        beatbox::gui::section_export_range(&e.project, 0, &mut a).unwrap();
        assert_eq!(a["start_s"], 0.0);
        let c = beatbox::tempo_curve::TempoCurve::new(&e.project);
        assert_eq!(a["end_s"].as_f64().unwrap(), c.secs_at_beat(7.));
    }
}

#[cfg(test)]
mod export_preview_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/export-preview-test".into());
        e.call(
            "load_project",
            &json!({"path":"/tmp/recorded-beat-project/recorded-groove.json"}),
        )
        .unwrap();
        e.call(
            "duplicate_pattern",
            &json!({"pattern":"groove","new_name":"Break"}),
        )
        .unwrap();
        e.call(
            "set_arrangement",
            &json!({"sections":[{"pattern":"groove","repeats":1},{"pattern":"Break","repeats":1}]}),
        )
        .unwrap();
        e
    }
    #[test]
    fn preview_ranges_exact_and_readonly() {
        let mut e = fixture();
        e.call("add_marker", &json!({"name":"Start","beat":0}))
            .unwrap();
        e.call("add_marker", &json!({"name":"Hook","beat":16}))
            .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        let rev = e.revision;
        assert_eq!(
            beatbox::gui::export_range_preview(&e.project, Some(1), None).unwrap(),
            "2: Break x1: 10.667s to 21.333s | 10.667s | no extra end tail"
        );
        assert_eq!(
            beatbox::gui::export_range_preview(&e.project, None, Some(("Start", "Hook"))).unwrap(),
            "Start to Hook: 0.000s to 10.667s | 10.667s | no extra end tail"
        );
        assert!(beatbox::gui::export_range_preview(&e.project, None, None)
            .unwrap()
            .starts_with("Full song"));
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        assert_eq!(rev, e.revision);
    }
    #[test]
    fn preview_invalid_choices_and_outdated_refs() {
        let mut e = fixture();
        for (section, markers) in [
            (Some(2), None),
            (Some(0), Some(("a", "b"))),
            (None, Some(("missing", "missing"))),
        ] {
            assert!(beatbox::gui::export_range_preview(&e.project, section, markers).is_err());
        }
        for (n, b) in [("a", 8.), ("b", 4.), ("far", 100.)] {
            e.call("add_marker", &json!({"name":n,"beat":b})).unwrap();
        }
        for pair in [("a", "b"), ("a", "a"), ("a", "far")] {
            assert!(beatbox::gui::export_range_preview(&e.project, None, Some(pair)).is_err());
        }
        e.project.arrangement.pop();
        assert!(beatbox::gui::export_range_preview(&e.project, Some(1), None).is_err());
        e.project.arrangement[0].pattern = "missing".into();
        assert!(beatbox::gui::export_range_preview(&e.project, Some(0), None).is_err());
    }
    #[test]
    fn preview_uses_changed_tempo_meter_and_mode() {
        let mut e = fixture();
        e.project.patterns[0].bars = 1;
        e.project.patterns[0].meter = Some((7, 8));
        e.call(
            "add_tempo_point",
            &json!({"beat":3.5,"bpm":180,"ramp":true}),
        )
        .unwrap();
        let rows = beatbox::tools_markers::arrangement_positions(&e.project).unwrap();
        let start = rows[1]["start_seconds"].as_f64().unwrap();
        assert!(
            beatbox::gui::export_range_preview(&e.project, Some(1), None)
                .unwrap()
                .contains(&format!("{start:.3}s to"))
        );
        e.project.arrangement.clear();
        e.call("place_pattern", &json!({"pattern":"groove","start_bar":0}))
            .unwrap();
        assert!(beatbox::gui::export_range_preview(&e.project, Some(0), None).is_err());
        assert!(beatbox::gui::export_range_preview(&e.project, None, None).is_ok());
    }
}
