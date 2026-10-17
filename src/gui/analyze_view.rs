//! The analysis panel (in the browser view): pick a recording or a stem and
//! write it down by hand, one button per job: split into stems, drums to
//! MIDI, bass to MIDI, chords and key, riff or melody to MIDI, effects,
//! sections and parts, compare a render with it. Each button sends the same
//! tool an outside client would (separate_stems, transcribe_drums,
//! transcribe_bass, transcribe_chords, transcribe_melody, detect_fx,
//! arrangement_map, score_sections);
//! the work runs off the UI thread on its own engine over the same folder, so
//! the studio stays responsive, and "add to project" then runs import_midi on
//! the studio's engine. BUILD A SONG FROM PARTS lays the written-down parts
//! out as tracks and sections (arrange_transcription), every option of that
//! tool on its own control: instrument, level, pan, how a chord part is
//! played (as written, broken up, the rhythm of another part, or the rhythm
//! heard in a recording), strum, fx chain, extra effects, tidy repeats, and
//! the vocal take with its placement.

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
    pub reattacks: bool,
    pub follow_level: bool,
    pub reattack_delta: f32,
    pub stems_dir: String,
    pub compare_with: String,
    pub compare_stems: String,
    pub job: Option<(String, std::sync::mpsc::Receiver<Result<Value, String>>)>,
    pub result: String,
    pub build: BuildState,
}

/// How a part's notes are played on its track.
pub const PLAY_MODES: [&str; 4] = [
    "as written",
    "broken chords",
    "rhythm of a part",
    "rhythm of a recording",
];

/// One row of BUILD A SONG FROM PARTS: one part (arrange_transcription parts[i]).
#[derive(Clone)]
pub struct PartRow {
    pub name: String,
    pub file: String,
    pub instrument: String,
    pub volume_db: String,
    pub pan: String,
    /// index into PLAY_MODES
    pub play: usize,
    pub style: String,
    pub rate: String,
    pub octaves: String,
    pub rhythm_part: String,
    pub rhythm_audio: String,
    pub min_share: f32,
    pub center: u8,
    pub gate: f32,
    pub octave: i32,
    pub strum: String,
    pub strum_dir: usize,
    pub fx_chain: String,
    pub effects: String,
    pub source_only: bool,
    pub tidy: bool,
}

impl Default for PartRow {
    fn default() -> Self {
        PartRow {
            name: String::new(),
            file: String::new(),
            instrument: String::new(),
            volume_db: String::new(),
            pan: String::new(),
            play: 0,
            style: "updown".into(),
            rate: String::new(),
            octaves: String::new(),
            rhythm_part: String::new(),
            rhythm_audio: String::new(),
            min_share: 0.4,
            center: 60,
            gate: 1.0,
            octave: 0,
            strum: String::new(),
            strum_dir: 0,
            fx_chain: String::new(),
            effects: String::new(),
            source_only: false,
            tidy: false,
        }
    }
}

pub const STRUM_DIRS: [&str; 3] = ["down", "up", "alternate"];

#[derive(Default)]
pub struct BuildState {
    pub parts: Vec<PartRow>,
    pub sections_file: String,
    pub bars_per_pattern: String,
    pub name: String,
    pub key: String,
    pub vocal: String,
    pub vocal_start_s: String,
    pub vocal_db: String,
    pub vocal_phrases: String,
    pub tidy: bool,
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

impl PartRow {
    /// The part as arrange_transcription takes it.
    pub fn to_json(&self) -> Value {
        let mut p = json!({"name": self.name.trim(), "file": self.file.trim()});
        if !self.instrument.trim().is_empty() {
            p["instrument"] = json!(self.instrument.trim());
        }
        if let Some(v) = num(&self.volume_db) {
            p["volume_db"] = json!(v);
        }
        if let Some(v) = num(&self.pan) {
            p["pan"] = json!(v);
        }
        match self.play {
            1 => {
                let mut a = json!({"style": if self.style.trim().is_empty() { "updown" } else { self.style.trim() }, "gate": self.gate});
                if let Some(r) = num(&self.rate) {
                    a["rate"] = json!(r);
                }
                if let Some(o) = num(&self.octaves) {
                    a["octaves"] = json!(o as i64);
                }
                p["arpeggiate"] = a;
            }
            2 if !self.rhythm_part.trim().is_empty() => {
                p["arpeggiate"] = json!({"rhythm_from": self.rhythm_part.trim(), "gate": self.gate, "octave": self.octave});
            }
            3 if !self.rhythm_audio.trim().is_empty() => {
                p["arpeggiate"] = json!({"rhythm_from_audio": self.rhythm_audio.trim(), "min_share": self.min_share, "center": self.center, "gate": self.gate, "octave": self.octave});
            }
            _ => {}
        }
        if let Some(sp) = num(&self.strum) {
            p["strum"] = json!({"spread": sp, "direction": STRUM_DIRS[self.strum_dir.min(2)]});
        }
        if !self.fx_chain.trim().is_empty() {
            p["fx_chain"] = json!(self.fx_chain.trim());
        }
        let fx: Vec<Value> = self
            .effects
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| json!({"type": t}))
            .collect();
        if !fx.is_empty() {
            p["effects"] = json!(fx);
        }
        if self.source_only {
            p["source_only"] = json!(true);
        }
        if self.tidy {
            p["consolidate"] = json!(true);
        }
        p
    }
}

impl BuildState {
    /// The whole build as arrange_transcription takes it.
    pub fn to_json(&self, bpm: Option<f64>) -> Value {
        let parts: Vec<Value> = self
            .parts
            .iter()
            .filter(|r| !r.name.trim().is_empty() && !r.file.trim().is_empty())
            .map(PartRow::to_json)
            .collect();
        let mut a = json!({"parts": parts});
        for (k, v) in [
            ("sections_file", &self.sections_file),
            ("name", &self.name),
            ("key", &self.key),
            ("vocal", &self.vocal),
        ] {
            if !v.trim().is_empty() {
                a[k] = json!(v.trim());
            }
        }
        if let Some(b) = num(&self.bars_per_pattern) {
            a["bars_per_pattern"] = json!(b as i64);
        }
        if let Some(b) = bpm {
            a["bpm"] = json!(b);
        }
        if let Some(v) = num(&self.vocal_start_s) {
            a["vocal_start_s"] = json!(v);
        }
        if let Some(v) = num(&self.vocal_db) {
            a["vocal_db"] = json!(v);
        }
        if !self.vocal_phrases.trim().is_empty() {
            a["vocal_phrases"] = json!(self.vocal_phrases.trim());
        }
        if self.tidy {
            a["consolidate"] = json!(true);
        }
        a
    }
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
                Value::String(s) if k == "midi" || k.ends_with("path") => std::path::Path::new(s)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default(),
                Value::String(s) => s.clone(),
                Value::Array(a) => a
                    .iter()
                    .take(8)
                    .map(|x| {
                        x.as_str()
                            .map(String::from)
                            .unwrap_or_else(|| x.to_string())
                    })
                    .collect::<Vec<_>>()
                    .join("  |  "),
                other => other.to_string(),
            };
            out.push(format!(
                "{k}: {}",
                txt.chars().take(400).collect::<String>()
            ));
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
            let mut r = own
                .call_from(tool, &args, "you")
                .map_err(|e| format!("{e:#}"));
            if let (true, Ok(v)) = (add, r.as_mut()) {
                if let Some(m) = v["midi"].as_str().map(String::from) {
                    let name = std::path::Path::new(&m)
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let imp = eng.lock().unwrap().call_from(
                        "import_midi",
                        &json!({"path": m, "pattern": name, "set_tempo": false}),
                        "you",
                    );
                    v["added_to_project"] = json!(imp.is_ok());
                }
            }
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.analyze.job = Some((label, rx));
        self.analyze.result.clear();
    }

    fn build_rows(&mut self, ui: &mut egui::Ui, ready: bool) -> Option<Value> {
        let tk = Tokens::DARK;
        let mut out = None;
        let b = &mut self.analyze.build;
        let lab = |ui: &mut egui::Ui, t: &str| {
            ui.label(RichText::new(t).size(11.0).color(tk.text_dim));
        };
        ui.horizontal_wrapped(|ui| {
            lab(ui, "SECTIONS FILE");
            ui.add(
                egui::TextEdit::singleline(&mut b.sections_file)
                    .desired_width(180.0)
                    .hint_text("from SECTIONS + PARTS (optional)"),
            );
            lab(ui, "BARS PER PATTERN");
            ui.add(
                egui::TextEdit::singleline(&mut b.bars_per_pattern)
                    .desired_width(36.0)
                    .hint_text("8"),
            );
            lab(ui, "SONG NAME");
            ui.add(egui::TextEdit::singleline(&mut b.name).desired_width(110.0));
            lab(ui, "KEY");
            ui.add(
                egui::TextEdit::singleline(&mut b.key)
                    .desired_width(70.0)
                    .hint_text("D minor"),
            );
            ui.checkbox(&mut b.tidy, "tidy repeats")
                .on_hover_text("each section's bars voted into its repeating 1, 2 or 4 bar figure");
        });
        ui.horizontal_wrapped(|ui| {
            lab(ui, "VOCAL TAKE");
            ui.add(
                egui::TextEdit::singleline(&mut b.vocal)
                    .desired_width(180.0)
                    .hint_text("take.wav (optional)"),
            );
            lab(ui, "STARTS AT (s)");
            ui.add(egui::TextEdit::singleline(&mut b.vocal_start_s).desired_width(50.0));
            lab(ui, "LEVEL dB");
            ui.add(egui::TextEdit::singleline(&mut b.vocal_db).desired_width(40.0));
            lab(ui, "LINE PLACEMENT");
            ui.add(
                egui::TextEdit::singleline(&mut b.vocal_phrases)
                    .desired_width(160.0)
                    .hint_text("phrase times file (optional)"),
            );
        });
        let mut remove = None;
        for (i, r) in b.parts.iter_mut().enumerate() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    lab(ui, "PART");
                    ui.add(egui::TextEdit::singleline(&mut r.name).desired_width(70.0).hint_text("guitar"));
                    lab(ui, "NOTES FILE");
                    ui.add(egui::TextEdit::singleline(&mut r.file).desired_width(170.0).hint_text("from a TO MIDI button"));
                    lab(ui, "INSTRUMENT");
                    ui.add(egui::TextEdit::singleline(&mut r.instrument).desired_width(120.0).hint_text("preset:keys"));
                    lab(ui, "dB");
                    ui.add(egui::TextEdit::singleline(&mut r.volume_db).desired_width(36.0));
                    lab(ui, "PAN");
                    ui.add(egui::TextEdit::singleline(&mut r.pan).desired_width(36.0));
                    ui.checkbox(&mut r.tidy, "tidy repeats");
                    ui.checkbox(&mut r.source_only, "rhythm source only").on_hover_text("load this part only to lend its rhythm to another part; it gets no track");
                    if ui.small_button("REMOVE").clicked() {
                        remove = Some(i);
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    lab(ui, "PLAY");
                    egui::ComboBox::from_id_salt(("br_play", i)).selected_text(PLAY_MODES[r.play.min(3)]).show_ui(ui, |ui| {
                        for (k, m) in PLAY_MODES.iter().enumerate() {
                            ui.selectable_value(&mut r.play, k, *m);
                        }
                    });
                    match r.play {
                        1 => {
                            lab(ui, "STYLE");
                            ui.add(egui::TextEdit::singleline(&mut r.style).desired_width(70.0).hint_text("updown"));
                            lab(ui, "RATE (steps)");
                            ui.add(egui::TextEdit::singleline(&mut r.rate).desired_width(36.0).hint_text("2"));
                            lab(ui, "OCTAVES");
                            ui.add(egui::TextEdit::singleline(&mut r.octaves).desired_width(30.0).hint_text("1"));
                        }
                        2 => {
                            lab(ui, "FROM PART");
                            ui.add(egui::TextEdit::singleline(&mut r.rhythm_part).desired_width(90.0).hint_text("riff"));
                        }
                        3 => {
                            lab(ui, "RECORDING");
                            ui.add(egui::TextEdit::singleline(&mut r.rhythm_audio).desired_width(170.0).hint_text("the part's stem, e.g. other.wav"));
                            ui.add(egui::Slider::new(&mut r.min_share, 0.1..=1.0).text("in share of bars"))
                                .on_hover_text("a 16th step is played when it is struck in at least this share of the section's bars");
                            ui.add(egui::Slider::new(&mut r.center, 36..=96).text("around note"));
                        }
                        _ => {}
                    }
                    if r.play > 0 {
                        ui.add(egui::Slider::new(&mut r.gate, 0.1..=1.0).text("note length"));
                    }
                    if r.play >= 2 {
                        ui.add(egui::Slider::new(&mut r.octave, -2..=2).text("octave"));
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    lab(ui, "STRUM (steps)");
                    ui.add(egui::TextEdit::singleline(&mut r.strum).desired_width(36.0).hint_text("off"));
                    egui::ComboBox::from_id_salt(("br_strum", i)).selected_text(STRUM_DIRS[r.strum_dir.min(2)]).show_ui(ui, |ui| {
                        for (k, d) in STRUM_DIRS.iter().enumerate() {
                            ui.selectable_value(&mut r.strum_dir, k, *d);
                        }
                    });
                    lab(ui, "FX CHAIN");
                    ui.add(egui::TextEdit::singleline(&mut r.fx_chain).desired_width(140.0).hint_text("clean_chorus_guitar"));
                    lab(ui, "EXTRA EFFECTS");
                    ui.add(egui::TextEdit::singleline(&mut r.effects).desired_width(160.0).hint_text("reverb, delay"));
                });
            });
        }
        if let Some(i) = remove {
            b.parts.remove(i);
        }
        let bpm = self.analyze.bpm.trim().parse::<f64>().ok();
        let b = &mut self.analyze.build;
        ui.horizontal(|ui| {
            if ui.small_button("ADD PART").clicked() {
                b.parts.push(PartRow::default());
            }
            let args = b.to_json(bpm);
            let has = args["parts"].as_array().is_some_and(|p| !p.is_empty());
            if ui
                .add_enabled(ready && has, egui::Button::new("BUILD SONG").small())
                .on_hover_text("Build a song from the transcribed parts")
                .clicked()
            {
                out = Some(args);
            }
        });
        out
    }

    /// arrange_transcription on the studio's own engine (it builds the open project).
    fn build_job(&mut self, ctx: egui::Context, args: Value) {
        if self.analyze.job.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let eng = self.engine.clone();
        std::thread::spawn(move || {
            let r = eng
                .lock()
                .unwrap()
                .call_from("arrange_transcription", &args, "you")
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.analyze.job = Some(("build song".into(), rx));
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
        let mut build: Option<Value> = None;
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
                    let mut da = args.clone();
                    if self.analyze.follow_level {
                        da["follow_level"] = json!(true);
                    }
                    if ui.add_enabled(ready, egui::Button::new("DRUMS TO MIDI").small()).on_hover_text(format!("transcribe_drums {da}")).clicked() {
                        job = Some(("transcribe_drums", da));
                    }
                    ui.checkbox(&mut self.analyze.follow_level, "follow soft sections").on_hover_text("drums: read each hit against the level around it, so a softer section keeps its hats");
                    let mut ba = args.clone();
                    if self.analyze.reattack_delta <= 0.0 {
                        self.analyze.reattack_delta = 0.07;
                    }
                    if self.analyze.reattacks {
                        ba["reattacks"] = json!(true);
                        ba["reattack_delta"] = json!(self.analyze.reattack_delta);
                    }
                    if ui.add_enabled(ready, egui::Button::new("BASS TO MIDI").small()).on_hover_text(format!("transcribe_bass {ba}")).clicked() {
                        job = Some(("transcribe_bass", ba));
                    }
                    ui.checkbox(&mut self.analyze.reattacks, "split re-struck notes").on_hover_text("bass: a held note struck again on the same pitch becomes two notes");
                    if self.analyze.reattacks {
                        ui.add(egui::Slider::new(&mut self.analyze.reattack_delta, 0.02..=0.3).text("attack threshold")).on_hover_text("lower finds softer re-picks");
                    }
                    if ui.add_enabled(ready, egui::Button::new("CHORDS + KEY").small()).on_hover_text("Find the chords and key of this recording").clicked() {
                        job = Some(("transcribe_chords", args.clone()));
                    }
                    if ui.add_enabled(ready, egui::Button::new("RIFF / MELODY TO MIDI").small()).on_hover_text("Turn the riff or melody into notes").clicked() {
                        job = Some(("transcribe_melody", args.clone()));
                    }
                    if ui.add_enabled(ready, egui::Button::new("PICKED GUITAR TO MIDI").small()).on_hover_text("Turn picked guitar into notes, one note per pluck (arpeggios)").clicked() {
                        let mut a = args.clone();
                        a["mode"] = json!("picked");
                        a["name"] = json!("picked");
                        job = Some(("transcribe_melody", a));
                    }
                    if ui.add_enabled(ready, egui::Button::new("EFFECTS").small()).on_hover_text("Hear which effects are on the recording").clicked() {
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
                    ui.add(egui::TextEdit::singleline(&mut self.analyze.compare_stems).desired_width(160.0).hint_text("your stems folder (optional)"));
                    let mut ca = json!({"ours": self.analyze.compare_with.trim(), "reference": path, "clips": true});
                    if !self.analyze.compare_stems.trim().is_empty() && !self.analyze.stems_dir.trim().is_empty() {
                        ca["ours_stems"] = json!(self.analyze.compare_stems.trim());
                        ca["reference_stems"] = json!(self.analyze.stems_dir.trim());
                    }
                    if ui.add_enabled(ready && !self.analyze.compare_with.trim().is_empty(), egui::Button::new("COMPARE").small()).on_hover_text(format!("score_sections {ca}")).clicked() {
                        job = Some(("score_sections", ca));
                    }
                });
                egui::CollapsingHeader::new(RichText::new("BUILD A SONG FROM PARTS").size(11.0).strong().color(tk.header_text))
                    .id_salt("br_build")
                    .default_open(false)
                    .show(ui, |ui| {
                        if let Some(a) = self.build_rows(ui, busy.is_none()) {
                            build = Some(a);
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
        if let Some(args) = build {
            self.build_job(ui.ctx().clone(), args);
        }
    }
}

#[cfg(test)]
mod build_form_tests {
    use super::*;

    #[test]
    fn build_form_sends_only_options_the_tool_takes() {
        let mut b = BuildState {
            bars_per_pattern: "8".into(),
            vocal: "take.wav".into(),
            vocal_db: "-2".into(),
            tidy: true,
            ..Default::default()
        };
        b.parts.push(PartRow {
            name: "gtr".into(),
            file: "chords.json".into(),
            instrument: "preset:keys".into(),
            play: 3,
            rhythm_audio: "other.wav".into(),
            min_share: 0.5,
            center: 64,
            strum: "0.5".into(),
            strum_dir: 2,
            fx_chain: "clean_chorus_guitar".into(),
            effects: "reverb, delay".into(),
            ..Default::default()
        });
        b.parts.push(PartRow {
            name: "riff".into(),
            file: "riff.json".into(),
            source_only: true,
            ..Default::default()
        });
        b.parts.push(PartRow {
            name: "keys".into(),
            file: "chords.json".into(),
            play: 1,
            rate: "2".into(),
            ..Default::default()
        });
        b.parts.push(PartRow {
            name: "lead".into(),
            file: "chords.json".into(),
            play: 2,
            rhythm_part: "riff".into(),
            octave: 1,
            ..Default::default()
        });
        b.parts.push(PartRow::default()); // an empty row is skipped
        let a = b.to_json(Some(96.0));
        let parts = a["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 4, "{a}");
        assert_eq!(parts[0]["arpeggiate"]["rhythm_from_audio"], "other.wav");
        assert_eq!(parts[0]["arpeggiate"]["center"], 64);
        assert_eq!(parts[0]["strum"]["direction"], "alternate");
        assert_eq!(parts[0]["effects"][1]["type"], "delay");
        assert_eq!(parts[1]["source_only"], true);
        assert_eq!(parts[2]["arpeggiate"]["style"], "updown");
        assert_eq!(parts[3]["arpeggiate"]["rhythm_from"], "riff");
        assert_eq!(a["bars_per_pattern"], 8);
        let t = crate::tools::find("arrange_transcription").expect("tool");
        let schema = (t.schema)();
        let props = &schema["properties"];
        for k in a.as_object().unwrap().keys() {
            assert!(props.get(k).is_some(), "song option {k} not in the tool");
        }
        let pp = &props["parts"]["items"]["properties"];
        for p in parts {
            for k in p.as_object().unwrap().keys() {
                assert!(pp.get(k).is_some(), "part option {k} not in the tool");
            }
        }
    }
}
