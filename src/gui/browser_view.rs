//! The browser view (FL's Browser): instrument presets, effects, sound
//! palettes and the project's samples, searchable, with a target track. Every
//! button sends the same MCP tool an MCP client would (set_instrument, add_track,
//! add_effect, apply_palette, add_sample_track); the lists come from the same
//! discovery tools (list_presets, list_palettes, list_samples). The sound
//! library panel searches the recorded-instrument catalog (search_instruments,
//! audition_instrument, load_instrument) and imports from a URL
//! (import_from_url, list_imports, export_credits); downloads run off the UI
//! thread first, then the same tool runs on the engine.

use super::theme::Tokens;
use super::Studio;
use crate::project::Project;
use eframe::egui::{self, RichText};
use serde_json::{json, Value};

#[derive(Default)]
pub struct BrowserState {
    pub search: String,
    pub section: usize,
    /// first open starts on the friendly Sounds list
    pub opened: bool,
    pub track: String,
    /// list_presets + list_palettes, fetched once
    pub catalog: Option<(Value, Value)>,
    /// list_fx_chains, refetched after any browser call (saved chains change)
    pub chains: Option<Value>,
    /// sound library: import field, catalog search, background job
    pub url: String,
    pub url_name: String,
    pub lib_hits: Option<(String, Value)>,
    pub imports: Option<Value>,
    pub credits: String,
    pub job: Option<(String, std::sync::mpsc::Receiver<Result<Value, String>>)>,
    pub job_result: String,
    /// the audio editor's sample, region and band
    pub edit: SampleEdit,
    /// soundgoodizer row: mode 0-3 (A-D) and amount 1-100 (0 = not set yet)
    pub good_mode: usize,
    pub good_amount: f32,
    /// MIDI row: controllers found by FIND (a list, or the reason there is none)
    pub midi_ports: Option<Value>,
}

/// Audio editor (Edison) form: the chosen sample, a time region and a band.
#[derive(Default)]
pub struct SampleEdit {
    pub sample: String,
    pub start: f32,
    pub end: f32,
    pub lo: f32,
    pub hi: f32,
    pub gain: f32,
    pub outside: bool,
    pub fade_ms: f32,
    /// spectrogram of `spec_for` (fetched once per sample) and the drag anchor
    pub spec: Option<Value>,
    pub spec_for: String,
    pub anchor: Option<egui::Pos2>,
}

fn matches(q: &str, a: &str, b: &str) -> bool {
    let q = q.trim().to_lowercase();
    q.is_empty() || a.to_lowercase().contains(&q) || b.to_lowercase().contains(&q)
}

/// Audio files dropped on the window become sounds in the project, each on its own
/// channel (FL users drag samples in from their file manager). Returns the names
/// added and a message for every file that could not be used.
pub(super) fn import_dropped(
    engine: &mut crate::engine::Engine,
    paths: &[std::path::PathBuf],
) -> (Vec<String>, Vec<String>) {
    let (mut added, mut failed) = (Vec::new(), Vec::new());
    for path in paths {
        let file = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("that file")
            .to_string();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !["wav", "mp3", "flac", "ogg"].contains(&ext.as_str()) {
            failed.push(format!("{file}: use a wav, mp3, flac or ogg file"));
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|f| f.to_str())
            .unwrap_or("sample");
        let name = crate::samples::sample_name(stem);
        let imported = match engine.call_from(
            "import_sample",
            &json!({"path": path.to_string_lossy(), "name": name}),
            "you",
        ) {
            Ok(v) => v,
            Err(e) => {
                failed.push(format!("{file}: {e:#}"));
                continue;
            }
        };
        // a taken sound name is kept as it is: the new file arrives under a numbered name
        let name = imported
            .get("sample")
            .and_then(|v| v.as_str())
            .unwrap_or(&name)
            .to_string();
        let track = format!("{name}_s");
        match engine.call_from(
            "add_sample_track",
            &json!({"name": track, "sample": name}),
            "you",
        ) {
            Ok(_) => added.push(name),
            Err(e) => failed.push(format!("{file}: {e:#}")),
        }
    }
    (added, failed)
}

impl Studio {
    pub(super) fn browser_view(&mut self, ui: &mut egui::Ui, p: &Project) {
        let tk = Tokens::DARK;
        self.poll_library_job();
        if !self.browser.opened {
            self.browser.opened = true;
            self.browser.section = 1;
        }
        if self.browser.catalog.is_none() {
            let mut e = self.engine.lock().unwrap();
            let pr = e
                .call_from("list_presets", &json!({}), "you")
                .unwrap_or(Value::Null);
            let pa = e
                .call_from("list_palettes", &json!({}), "you")
                .unwrap_or(Value::Null);
            drop(e);
            self.browser.catalog = Some((pr, pa));
        }
        if self.browser.chains.is_none() {
            let c = self
                .engine
                .lock()
                .unwrap()
                .call_from("list_fx_chains", &json!({}), "you")
                .unwrap_or(Value::Null);
            self.browser.chains = Some(c);
        }
        let chains = self.browser.chains.clone().unwrap_or(Value::Null);
        if self.browser.track.is_empty() || p.track_index(&self.browser.track).is_err() {
            self.browser.track = p.tracks.first().map(|t| t.name.clone()).unwrap_or_default();
        }
        let (presets, palettes) = self
            .browser
            .catalog
            .clone()
            .unwrap_or((Value::Null, Value::Null));
        let mut call: Option<(String, Value)> = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("SEARCH").size(11.0).color(tk.text_dim));
            ui.add(
                egui::TextEdit::singleline(&mut self.browser.search)
                    .desired_width((ui.available_width() - 80.0).clamp(110.0, 220.0))
                    .hint_text("808, bell, reverb, dark..."),
            );
            ui.add_space(12.0);
            ui.label(
                RichText::new("USE ON CHANNEL")
                    .size(11.0)
                    .color(tk.text_dim),
            );
            egui::ComboBox::from_id_salt("browser_track")
                .selected_text(self.browser.track.clone())
                .show_ui(ui, |ui| {
                    for t in &p.tracks {
                        ui.selectable_value(&mut self.browser.track, t.name.clone(), &t.name);
                    }
                });
        });
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            for (i, label) in [
                (1usize, "Sounds"),
                (0, "Sample packs"),
                (2, "Effects & MIDI"),
                (3, "Palettes"),
                (4, "Analyze recording"),
            ] {
                ui.selectable_value(&mut self.browser.section, i, label);
            }
        });
        ui.separator();
        let section = self.browser.section;
        let q = self.browser.search.clone();
        let track = self.browser.track.clone();
        if section == 0 {
            ui.add_space(10.0);
            ui.label(
                RichText::new(format!("PROJECT SAMPLES ({})", p.samples.len()))
                    .size(11.0)
                    .strong()
                    .color(tk.header_text),
            );
            if p.samples.is_empty() {
                ui.label(
                    RichText::new("No project samples yet. Import a sound below, then open it in the audio editor.")
                        .size(10.0)
                        .color(tk.text_dim),
                );
            }
            for s in &p.samples {
                if !matches(&q, &s.name, &s.source) {
                    continue;
                }
                ui.horizontal(|ui| {
                    if ui
                        .small_button("+ New channel")
                        .on_hover_text("Add this sample as its own channel")
                        .clicked()
                    {
                        call = Some((
                            "add_sample_track".into(),
                            json!({"name": format!("{}_s", s.name), "sample": s.name}),
                        ));
                    }
                    if ui
                        .small_button("EDIT")
                        .on_hover_text("open in the audio editor")
                        .clicked()
                    {
                        let ed = &mut self.browser.edit;
                        *ed = SampleEdit {
                            sample: s.name.clone(),
                            start: 0.0,
                            end: s.duration,
                            lo: 2000.0,
                            hi: 8000.0,
                            gain: -60.0,
                            outside: false,
                            fade_ms: 10.0,
                            ..Default::default()
                        };
                    }
                    ui.label(RichText::new(&s.name).size(12.0).color(tk.text));
                    ui.label(
                        RichText::new(format!("{:.1} s \u{00B7} {}", s.duration, s.license))
                            .size(10.0)
                            .color(tk.text_dim),
                    );
                });
            }
            if !self.browser.edit.sample.is_empty()
                && self.browser.edit.spec_for != self.browser.edit.sample
            {
                let nm = self.browser.edit.sample.clone();
                let v = self
                    .engine
                    .lock()
                    .unwrap()
                    .call_from("sample_spectrogram", &json!({"sample": nm}), "you")
                    .ok();
                self.browser.edit.spec = v;
                self.browser.edit.spec_for = nm;
            }
            if let Some(si) = p.samples.iter().find(|x| {
                !self.browser.edit.sample.is_empty() && x.name == self.browser.edit.sample
            }) {
                if let Some(c) = audio_editor(ui, &tk, &mut self.browser.edit, si.duration) {
                    call = Some(c);
                }
            }
        }
        if section == 0 {
            self.sound_library(ui, &track);
        }
        ui.add_space(6.0);
        if section == 4 {
            self.analyze_panel(ui);
        }
        ui.add_space(6.0);
        let h = ui.available_height() - 8.0;
        let hear_busy = self.browser.job.as_ref().map(|j| j.0.clone());
        let mut hear: Option<String> = None;
        if section == 1 {
            // ---------- instrument presets
            ui.label(
                RichText::new("SOUNDS  \u{00B7}  hear one, then use it on the channel above or add it as a new channel")
                    .size(11.0)
                    .strong()
                    .color(tk.header_text),
            );
            let previewing = self.engine.lock().unwrap().transport.auditioning;
            if previewing
                && ui
                    .button("\u{25A0} Stop preview")
                    .on_hover_text("Stop the sound that is playing")
                    .clicked()
            {
                self.engine.lock().unwrap().transport.pending_audition = Some(None);
            }
            egui::ScrollArea::vertical()
                .id_salt("br_inst")
                .max_height(h)
                .show(ui, |ui| {
                    for it in presets["instrument_presets"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                    {
                        let (n, d) = (
                            it["name"].as_str().unwrap_or(""),
                            it["description"].as_str().unwrap_or(""),
                        );
                        if !matches(&q, n, d) {
                            continue;
                        }
                        ui.horizontal_wrapped(|ui| {
                            // the name column gives way so the three buttons never clip
                            let name_w = (ui.available_width() - 345.0).clamp(70.0, 340.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2(name_w, 34.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    ui.set_min_size(egui::vec2(name_w, 34.0));
                                    ui.label(RichText::new(n).size(14.0).strong().color(tk.text));
                                    if !d.is_empty() {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(d).size(10.5).color(tk.text_dim),
                                            )
                                            .truncate(),
                                        );
                                    }
                                },
                            );
                            if ui
                                .add_enabled(
                                    hear_busy.is_none(),
                                    egui::Button::new("\u{25B6} Hear"),
                                )
                                .on_hover_text("Play a short phrase with this sound")
                                .clicked()
                            {
                                hear = Some(n.to_string());
                            }
                            let use_label = if track.is_empty() {
                                "Use".to_string()
                            } else {
                                format!("Use on {track}")
                            };
                            if ui
                                .button(use_label)
                                .on_hover_text("Replace the sound of the selected channel")
                                .clicked()
                                && !track.is_empty()
                            {
                                call = Some((
                                    "set_instrument".into(),
                                    json!({"track": track, "preset": n}),
                                ));
                            }
                            if ui
                                .button("+ New channel")
                                .on_hover_text(
                                    "Add this sound as its own channel in the Channel rack",
                                )
                                .clicked()
                            {
                                call = Some(("add_track".into(), json!({"name": n, "preset": n})));
                            }
                        });
                        ui.add_space(3.0);
                    }
                });
        }
        if section == 2 {
            // ---------- fx chains and MIDI
            let (port, rec, learning, links, keys) = {
                let e = self.engine.lock().unwrap();
                (
                    e.midi.port.clone(),
                    e.midi.record,
                    e.midi.learn.clone(),
                    e.midi.links.len(),
                    if e.midi.track.is_empty() {
                        p.tracks.first().map(|t| t.name.clone()).unwrap_or_default()
                    } else {
                        e.midi.track.clone()
                    },
                )
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("MIDI IN")
                        .size(11.0)
                        .strong()
                        .color(tk.header_text),
                );
                match &port {
                    Some(n) => {
                        ui.label(RichText::new(n).size(11.0).color(tk.text));
                        if ui.small_button("DISCONNECT").clicked() {
                            call = Some(("midi_input".into(), json!({"action": "disconnect"})));
                        }
                    }
                    None => {
                        if ui
                            .small_button("FIND")
                            .on_hover_text("look for connected controllers")
                            .clicked()
                        {
                            let r = self.engine.lock().unwrap().call_from(
                                "midi_input",
                                &json!({"action": "ports"}),
                                "you",
                            );
                            self.browser.midi_ports = Some(match r {
                                Ok(v) => v["ports"].clone(),
                                Err(err) => json!(format!("{err:#}")),
                            });
                        }
                        match self.browser.midi_ports.clone() {
                            Some(Value::Array(list)) if list.is_empty() => {
                                ui.label(
                                    RichText::new("no controller found")
                                        .size(10.0)
                                        .color(tk.text_dim),
                                );
                            }
                            Some(Value::Array(list)) => {
                                for pt in list {
                                    let a = pt["port"].as_str().unwrap_or("").to_string();
                                    if ui
                                        .small_button(pt["name"].as_str().unwrap_or(&a))
                                        .on_hover_text(format!("connect {a}"))
                                        .clicked()
                                    {
                                        call = Some((
                                            "midi_input".into(),
                                            json!({"action": "connect", "port": a}),
                                        ));
                                    }
                                }
                            }
                            Some(Value::String(why)) => {
                                ui.label(RichText::new(why).size(10.0).color(tk.text_dim));
                            }
                            _ => {}
                        }
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                if ui
                    .small_button(format!("KEYS PLAY {track}"))
                    .on_hover_text("the keyboard plays this track")
                    .clicked()
                    && !track.is_empty()
                {
                    call = Some((
                        "midi_input".into(),
                        json!({"action": "set", "track": track}),
                    ));
                }
                let mut r = rec;
                if ui
                    .checkbox(&mut r, "RECORD")
                    .on_hover_text("write what you play into the pattern under the play head")
                    .changed()
                {
                    call = Some(("midi_input".into(), json!({"action": "set", "record": r})));
                }
                for (lbl, tgt) in [
                    ("LEARN VOL", format!("volume:{track}")),
                    ("LEARN PAN", format!("pan:{track}")),
                ] {
                    if ui
                        .small_button(lbl)
                        .on_hover_text(format!("the next knob you move controls {tgt}"))
                        .clicked()
                        && !track.is_empty()
                    {
                        call = Some((
                            "midi_input".into(),
                            json!({"action": "learn", "target": tgt}),
                        ));
                    }
                }
                if let Some(l) = &learning {
                    ui.label(
                        RichText::new(format!("move a control for {l}"))
                            .size(10.0)
                            .color(tk.text_dim),
                    );
                } else {
                    ui.label(
                        RichText::new(format!("keys: {keys}  \u{00B7}  {links} linked"))
                            .size(10.0)
                            .color(tk.text_dim),
                    )
                    .on_hover_text("links and the keys track are saved with the project");
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("FX CHAINS")
                        .size(11.0)
                        .strong()
                        .color(tk.header_text),
                );
                if ui
                    .small_button("SAVE TRACK'S CHAIN")
                    .on_hover_text(format!(
                        "save_fx_chain {{track: {track}, name: {track}_chain}}"
                    ))
                    .clicked()
                    && !track.is_empty()
                {
                    call = Some((
                        "save_fx_chain".into(),
                        json!({"track": track, "name": format!("{track}_chain")}),
                    ));
                }
            });
            for it in chains["chains"].as_array().cloned().unwrap_or_default() {
                let n = it["name"].as_str().unwrap_or("");
                let d = it["use"].as_str().unwrap_or("saved in this project");
                if !matches(&q, n, d) {
                    continue;
                }
                let fx: Vec<String> = it["effects"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|x| x["type"].as_str().unwrap_or("?").to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    if ui
                        .small_button("APPLY")
                        .on_hover_text(format!("apply_fx_chain {{track: {track}, chain: {n}}}"))
                        .clicked()
                        && !track.is_empty()
                    {
                        call = Some(("apply_fx_chain".into(), json!({"track": track, "chain": n})));
                    }
                    ui.label(RichText::new(n).size(12.0).color(tk.text))
                        .on_hover_text(d);
                    ui.add(
                        egui::Label::new(
                            RichText::new(fx.join(" > ")).size(10.0).color(tk.text_dim),
                        )
                        .truncate(),
                    )
                    .on_hover_text(d);
                });
            }
            ui.add_space(6.0);
            if self.browser.good_amount <= 0.0 {
                self.browser.good_amount = 50.0;
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("SOUNDGOODIZER").size(11.0).strong().color(tk.header_text));
                for (i, m) in ["A", "B", "C", "D"].iter().enumerate() {
                    let tip = ["even lift", "forward mids", "brighter top", "heavy low end"][i];
                    ui.selectable_value(&mut self.browser.good_mode, i, *m).on_hover_text(tip);
                }
                ui.add(egui::Slider::new(&mut self.browser.good_amount, 1.0..=100.0).integer());
                let mode = ["A", "B", "C", "D"][self.browser.good_mode.min(3)];
                if ui.small_button("APPLY").on_hover_text(format!("polish {track} (mode {mode}); again on the same track retunes it")).clicked() && !track.is_empty() {
                    call = Some(("soundgoodizer".into(), json!({"track": track, "mode": mode, "amount": self.browser.good_amount.round()})));
                }
            });
            ui.add_space(6.0);
            ui.add_space(8.0);
            ui.label(
                RichText::new("EFFECTS")
                    .size(11.0)
                    .strong()
                    .color(tk.header_text),
            );
            egui::ScrollArea::vertical()
                .id_salt("br_fx")
                .max_height((ui.available_height() - 8.0).max(120.0))
                .show(ui, |ui| {
                    for it in presets["effects"].as_array().cloned().unwrap_or_default() {
                        let (n, d) = (
                            it["type"].as_str().unwrap_or(""),
                            it["params"].as_str().unwrap_or(""),
                        );
                        if !matches(&q, n, d) {
                            continue;
                        }
                        ui.horizontal(|ui| {
                            if ui
                                .small_button("ADD")
                                .on_hover_text(format!("add_effect {{track: {track}, type: {n}}}"))
                                .clicked()
                                && !track.is_empty()
                            {
                                call =
                                    Some(("add_effect".into(), json!({"track": track, "type": n})));
                            }
                            ui.label(RichText::new(n).size(12.0).color(tk.text))
                                .on_hover_text(d);
                        });
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("    {d}"))
                                    .size(10.0)
                                    .color(tk.text_dim),
                            )
                            .truncate(),
                        )
                        .on_hover_text(d);
                    }
                });
        }
        if section == 3 {
            // ---------- palettes
            ui.label(
                RichText::new("SOUND PALETTES")
                    .size(11.0)
                    .strong()
                    .color(tk.header_text),
            );
            for it in palettes["palettes"].as_array().cloned().unwrap_or_default() {
                let n = it["name"].as_str().unwrap_or("");
                let d = it["description"].as_str().unwrap_or("");
                if !matches(&q, n, d) {
                    continue;
                }
                ui.horizontal(|ui| {
                    if ui
                        .small_button("APPLY")
                        .on_hover_text(format!("apply_palette {{palette: {n}}}"))
                        .clicked()
                    {
                        call = Some(("apply_palette".into(), json!({"palette": n})));
                    }
                    ui.label(RichText::new(n).size(12.0).color(tk.text))
                        .on_hover_text(d);
                });
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("    {d}"))
                            .size(10.0)
                            .color(tk.text_dim),
                    )
                    .truncate(),
                )
                .on_hover_text(d);
            }
        }
        if let Some((tool, args)) = call {
            // A channel added from the browser becomes the selected one, so
            // the piano roll edits it right away.
            let added =
                (tool == "add_track" || tool == "add_sample_track").then_some(p.tracks.len());
            self.call(&tool, args);
            if let Some(i) = added {
                self.selected = i;
            }
            self.browser.chains = None;
        }
        if let Some(n) = hear {
            self.library_job(
                format!("hear {n}"),
                ui.ctx().clone(),
                "preview_instrument",
                json!({"preset": n, "play": true}),
            );
        }
    }
}

impl Studio {
    /// Run a library action off the UI thread: downloads happen without the
    /// engine lock, then the same MCP tool runs on the engine.
    fn library_job(&mut self, label: String, ctx: egui::Context, tool: &'static str, args: Value) {
        if self.browser.job.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let eng = self.engine.clone();
        std::thread::spawn(move || {
            let pre: Result<(), String> = match tool {
                "import_from_url" => crate::web_import::prefetch(
                    args["url"].as_str().unwrap_or(""),
                    args.get("max_mb").and_then(|v| v.as_u64()).unwrap_or(200),
                )
                .map_err(|e| format!("{e:#}")),
                "load_instrument" | "audition_instrument" => {
                    match crate::sound_catalog::entry(args["catalog_id"].as_str().unwrap_or("")) {
                        Some(ent) if ent.format == "sfz_git" => {
                            crate::sound_catalog::fetch_sfz_git(&ent, 400, 2)
                                .map(|_| ())
                                .map_err(|e| format!("{e:#}"))
                        }
                        _ => Ok(()),
                    }
                }
                _ => Ok(()),
            };
            let r = pre.and_then(|_| {
                eng.lock()
                    .unwrap()
                    .call_from(tool, &args, "you")
                    .map_err(|e| format!("{e:#}"))
            });
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.browser.job = Some((label, rx));
        self.browser.job_result.clear();
    }

    /// Pick up the result of a finished library job (a sound preview, a
    /// download). Runs from every Browser section, so a finished preview frees
    /// the Hear buttons whichever list is showing.
    fn poll_library_job(&mut self) {
        if let Some((label, rx)) = &self.browser.job {
            if let Ok(r) = rx.try_recv() {
                self.browser.job_result = match r {
                    Ok(v) if v["played_as"].is_string() && v["playing"].is_boolean() => {
                        let name = v["name"].as_str().or(v["preset"].as_str()).unwrap_or("");
                        if v["playing"] == true {
                            format!(
                                "{label}: playing {name} ({:.1} s, peak {:.1} dBFS)",
                                v["play_seconds"].as_f64().unwrap_or(0.0),
                                v["peak_dbfs"].as_f64().unwrap_or(0.0)
                            )
                        } else {
                            format!(
                                "{label}: no audio output here \u{00B7} saved {}",
                                v["path"].as_str().unwrap_or("")
                            )
                        }
                    }
                    Ok(v) if v["played_as"].is_string() => {
                        format!(
                            "{label}: {} \u{00B7} peak {:.1} dBFS \u{00B7} {}",
                            v["name"].as_str().unwrap_or(""),
                            v["peak_dbfs"].as_f64().unwrap_or(0.0),
                            v["path"].as_str().unwrap_or("")
                        )
                    }
                    Ok(v) => {
                        let what = v["loaded"]
                            .as_str()
                            .or(v["name"].as_str())
                            .unwrap_or("done");
                        format!(
                            "{label}: {what} {}",
                            v["license"]
                                .as_str()
                                .map(|l| format!("({l})"))
                                .unwrap_or_default()
                        )
                    }
                    Err(e) => format!("{label} failed: {e}"),
                };
                self.browser.job = None;
                self.browser.imports = None;
                self.browser.lib_hits = None;
            }
        }
    }

    fn sound_library(&mut self, ui: &mut egui::Ui, track: &str) {
        let tk = Tokens::DARK;
        self.poll_library_job();
        let q = self.browser.search.trim().to_string();
        if self
            .browser
            .lib_hits
            .as_ref()
            .map(|h| h.0 != q)
            .unwrap_or(true)
        {
            let r = self
                .engine
                .lock()
                .unwrap()
                .call_from(
                    "search_instruments",
                    &json!({"query": q, "limit": 60}),
                    "you",
                )
                .unwrap_or(Value::Null);
            self.browser.lib_hits = Some((q.clone(), r));
        }
        if self.browser.imports.is_none() {
            let r = self
                .engine
                .lock()
                .unwrap()
                .call_from("list_imports", &json!({}), "you")
                .unwrap_or(Value::Null);
            self.browser.imports = Some(r);
        }
        let hits = self
            .browser
            .lib_hits
            .clone()
            .map(|h| h.1)
            .unwrap_or(Value::Null);
        let imports = self.browser.imports.clone().unwrap_or(Value::Null);
        let busy = self.browser.job.as_ref().map(|j| j.0.clone());
        let mut job: Option<(String, &'static str, Value)> = None;
        egui::CollapsingHeader::new(
            RichText::new(format!(
                "SAMPLE PACKS  \u{00B7}  {} recorded instruments",
                hits["catalog_size"].as_u64().unwrap_or(0)
            ))
            .size(11.0)
            .strong()
            .color(tk.header_text),
        )
        .id_salt("br_library")
        .default_open(true)
        .show(ui, |ui| {
            ui.collapsing(
                RichText::new("Import your own sound from a link")
                    .size(11.0)
                    .color(tk.text_dim),
                |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new("IMPORT FROM URL")
                                .size(11.0)
                                .color(tk.text_dim),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut self.browser.url)
                                .desired_width(360.0)
                                .hint_text("https://... .wav / .sf2 / .zip of samples"),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut self.browser.url_name)
                                .desired_width(120.0)
                                .hint_text("name (optional)"),
                        );
                        let ready =
                            self.browser.url.trim().starts_with("https://") && busy.is_none();
                        let mut args = json!({"url": self.browser.url.trim()});
                        if !self.browser.url_name.trim().is_empty() {
                            args["name"] = json!(self.browser.url_name.trim());
                        }
                        if ui
                            .add_enabled(ready, egui::Button::new("IMPORT").small())
                            .on_hover_text("Import the audio at this link")
                            .clicked()
                        {
                            job = Some((String::from("import"), "import_from_url", args.clone()));
                        }
                        let mut a2 = args.clone();
                        a2["track"] = json!(track);
                        if ui
                            .add_enabled(
                                ready && !track.is_empty(),
                                egui::Button::new("IMPORT TO TARGET TRACK").small(),
                            )
                            .on_hover_text(format!("import_from_url {a2}"))
                            .clicked()
                        {
                            job = Some((String::from("import"), "import_from_url", a2));
                        }
                        if let Some(b) = &busy {
                            ui.spinner();
                            ui.label(
                                RichText::new(format!("{b}..."))
                                    .size(11.0)
                                    .color(tk.text_dim),
                            );
                        } else if !self.browser.job_result.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&self.browser.job_result)
                                        .size(11.0)
                                        .color(tk.text),
                                )
                                .truncate(),
                            );
                        }
                    });
                },
            );
            if busy.is_none() && !self.browser.job_result.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new(&self.browser.job_result)
                            .size(11.0)
                            .color(tk.text),
                    )
                    .truncate(),
                );
            }
            ui.add_space(4.0);
            ui.columns(2, |cols| {
                let ui = &mut cols[0];
                ui.label(
                    RichText::new(format!(
                        "CATALOG ({} match the search box)",
                        hits["matches"].as_u64().unwrap_or(0)
                    ))
                    .size(11.0)
                    .color(tk.text_dim),
                );
                egui::ScrollArea::vertical()
                    .id_salt("br_cat")
                    .max_height(250.0)
                    .show(ui, |ui| {
                        for it in hits["results"].as_array().cloned().unwrap_or_default() {
                            let id = it["id"].as_str().unwrap_or("").to_string();
                            let name = it["name"].as_str().unwrap_or("");
                            let mb = it["mb"]
                                .as_f64()
                                .map(|m| format!("{m:.0} MB"))
                                .unwrap_or_default();
                            let lic = it["license"].as_str().unwrap_or("");
                            let fmt = it["format"].as_str().unwrap_or("");
                            ui.add_space(6.0);
                            let installed = if it["installed"] == true {
                                " (installed)"
                            } else {
                                ""
                            };
                            ui.label(
                                RichText::new(format!("{name}{installed}"))
                                    .size(13.0)
                                    .strong()
                                    .color(tk.text),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "{} \u{00B7} {mb}",
                                    it["family"].as_str().unwrap_or("")
                                ))
                                .size(10.5)
                                .color(tk.text_dim),
                            )
                            .on_hover_text(format!("License: {lic} ({fmt})"));
                            ui.horizontal_wrapped(|ui| {
                                let can = busy.is_none()
                                    && !track.is_empty()
                                    && (fmt == "sfz_git" || fmt == "vsco_pack");
                                if ui
                                    .add_enabled(can, egui::Button::new("Use on channel"))
                                    .on_hover_text("Load this instrument onto the selected channel")
                                    .clicked()
                                {
                                    job = Some((
                                        format!("loading {id}"),
                                        "load_instrument",
                                        json!({"track": track, "catalog_id": id}),
                                    ));
                                }
                                if ui
                                    .add_enabled(
                                        busy.is_none() && (fmt == "sfz_git" || fmt == "vsco_pack"),
                                        egui::Button::new("+ New channel"),
                                    )
                                    .on_hover_text("Add this instrument as its own channel")
                                    .clicked()
                                {
                                    job = Some((
                                        format!("loading {id}"),
                                        "load_instrument",
                                        json!({"track": id, "catalog_id": id}),
                                    ));
                                }
                                if ui
                                    .add_enabled(
                                        busy.is_none() && fmt == "sfz_git",
                                        egui::Button::new("\u{25B6} Hear"),
                                    )
                                    .on_hover_text("Plays a 4 second preview")
                                    .clicked()
                                {
                                    job = Some((
                                        format!("audition {id}"),
                                        "audition_instrument",
                                        json!({"catalog_id": id, "play": true}),
                                    ));
                                }
                            });
                        }
                    });
                let ui = &mut cols[1];
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "IMPORTED ({})",
                            imports["count"].as_u64().unwrap_or(0)
                        ))
                        .size(11.0)
                        .color(tk.text_dim),
                    );
                    if ui
                        .small_button("CREDITS")
                        .on_hover_text("export_credits {}")
                        .clicked()
                    {
                        let r = self
                            .engine
                            .lock()
                            .unwrap()
                            .call_from("export_credits", &json!({}), "you")
                            .unwrap_or(Value::Null);
                        self.browser.credits = r["credits"].as_str().unwrap_or("").to_string();
                    }
                });
                egui::ScrollArea::vertical()
                    .id_salt("br_imp")
                    .max_height(150.0)
                    .show(ui, |ui| {
                        for it in imports["imports"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default()
                            .iter()
                            .rev()
                        {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(it["slug"].as_str().unwrap_or(""))
                                        .size(12.0)
                                        .color(tk.text),
                                );
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "{} \u{00B7} {}",
                                            it["loaded"].as_str().unwrap_or(""),
                                            it["license"].as_str().unwrap_or("")
                                        ))
                                        .size(10.0)
                                        .color(tk.text_dim),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(it["url"].as_str().unwrap_or(""));
                            });
                        }
                        if !self.browser.credits.is_empty() {
                            ui.separator();
                            ui.label(
                                RichText::new(&self.browser.credits)
                                    .size(10.0)
                                    .color(tk.text_dim),
                            );
                        }
                    });
            });
        });
        if let Some((label, tool, args)) = job {
            self.library_job(label, ui.ctx().clone(), tool, args);
        }
    }
}

/// The audio editor (FL Edison): region, band and the edits that make a new sample.
fn audio_editor(
    ui: &mut egui::Ui,
    tk: &Tokens,
    ed: &mut SampleEdit,
    dur: f32,
) -> Option<(String, Value)> {
    let mut call: Option<(String, Value)> = None;
    let name = ed.sample.clone();
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("AUDIO EDITOR \u{00B7} {name}"))
                .size(11.0)
                .strong()
                .color(tk.header_text),
        );
        if ui.small_button("CLOSE").clicked() {
            ed.sample.clear();
        }
    });
    let top = if dur > 0.0 { dur } else { 600.0 };
    if let Some(sp) = ed.spec.clone() {
        spectrogram_view(ui, tk, ed, &sp, top);
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new("region s").size(10.0).color(tk.text_dim));
        ui.add(
            egui::DragValue::new(&mut ed.start)
                .speed(0.01)
                .range(0.0..=top)
                .max_decimals(3),
        );
        ui.label(RichText::new("to").size(10.0).color(tk.text_dim));
        ui.add(
            egui::DragValue::new(&mut ed.end)
                .speed(0.01)
                .range(0.0..=top)
                .max_decimals(3),
        );
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new("band Hz").size(10.0).color(tk.text_dim));
        ui.add(
            egui::DragValue::new(&mut ed.lo)
                .speed(10.0)
                .range(1.0..=24000.0),
        );
        ui.label(RichText::new("to").size(10.0).color(tk.text_dim));
        ui.add(
            egui::DragValue::new(&mut ed.hi)
                .speed(10.0)
                .range(1.0..=24000.0),
        );
        ui.label(RichText::new("gain dB").size(10.0).color(tk.text_dim));
        ui.add(
            egui::DragValue::new(&mut ed.gain)
                .speed(0.5)
                .range(-120.0..=24.0),
        );
        ui.checkbox(&mut ed.outside, "keep only the band");
    });
    let end = if ed.end > ed.start { ed.end } else { top };
    ui.horizontal_wrapped(|ui| {
        if ui.small_button("CUT BAND").on_hover_text("change only that band inside the region (gain -60 removes it)").clicked() {
            call = Some(("spectral_edit_sample".into(), json!({"sample": name, "start_s": ed.start, "end_s": end, "low_hz": ed.lo, "high_hz": ed.hi, "gain_db": ed.gain, "mode": if ed.outside { "outside" } else { "band" }})));
        }
        if ui.small_button("TRIM TO REGION").clicked() {
            call = Some(("edit_sample".into(), json!({"sample": name, "start_s": ed.start, "length_s": (end - ed.start).max(0.001)})));
        }
        if ui.small_button("REVERSE").clicked() {
            call = Some(("edit_sample".into(), json!({"sample": name, "reverse": true})));
        }
        ui.add(egui::DragValue::new(&mut ed.fade_ms).speed(1.0).range(0.0..=5000.0).suffix(" ms"));
        if ui.small_button("FADE IN/OUT").clicked() {
            call = Some(("edit_sample".into(), json!({"sample": name, "fade_in_ms": ed.fade_ms, "fade_out_ms": ed.fade_ms})));
        }
        if ui.small_button("NORMALIZE").on_hover_text("peak to -1 dB").clicked() {
            call = Some(("edit_sample".into(), json!({"sample": name, "normalize_db": -1.0})));
        }
        if ui.small_button("DENOISE").clicked() {
            call = Some(("denoise_sample".into(), json!({"sample": name})));
        }
        if ui.small_button("SLICE").on_hover_text("cut at each hit").clicked() {
            call = Some(("slice_sample".into(), json!({"sample": name})));
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui
            .small_button("REPAIR ALL")
            .on_hover_text("one-pass fix: de-clip, de-pop, de-click, de-noise, de-breath, gate")
            .clicked()
        {
            call = Some(("repair_take".into(), json!({"sample": name})));
        }
        if ui
            .small_button("DE-CLIP")
            .on_hover_text("redraw squared-off loud peaks")
            .clicked()
        {
            call = Some(("declip_sample".into(), json!({"sample": name})));
        }
        if ui
            .small_button("DE-POP")
            .on_hover_text("tame p/b mic pops")
            .clicked()
        {
            call = Some(("deplosive_sample".into(), json!({"sample": name})));
        }
        if ui
            .small_button("DE-BREATH")
            .on_hover_text("turn down breaths between phrases")
            .clicked()
        {
            call = Some(("reduce_breaths".into(), json!({"sample": name})));
        }
        if ui
            .small_button("RIDE LEVEL")
            .on_hover_text("even out loud and quiet phrases toward the take's own level")
            .clicked()
        {
            call = Some(("ride_take".into(), json!({"sample": name})));
        }
        if ui
            .small_button("PATCH REGION")
            .on_hover_text(
                "replace the selected region with the best-matching stretch elsewhere in the take",
            )
            .clicked()
        {
            call = Some((
                "patch_take".into(),
                json!({"sample": name, "from_s": ed.start, "to_s": end}),
            ));
        }
        if ui
            .small_button("QUANTIZE HITS")
            .on_hover_text("snap hits to the grid at the project tempo")
            .clicked()
        {
            call = Some(("quantize_sample".into(), json!({"sample": name})));
        }
        if ui
            .small_button("LINE UP")
            .on_hover_text("shift the take so it starts exactly with the song")
            .clicked()
        {
            call = Some((
                "shift_take".into(),
                json!({"sample": name, "reference": "mix"}),
            ));
        }
        if ui
            .small_button("DOUBLE")
            .on_hover_text("add a slightly detuned, slightly late copy of the voice (ADT)")
            .clicked()
        {
            call = Some(("double_vocal".into(), json!({"sample": name})));
        }
        if ui
            .small_button("FORMANT DEEPER")
            .on_hover_text("bigger, deeper voice body; pitch and words unchanged")
            .clicked()
        {
            call = Some((
                "formant_sample".into(),
                json!({"sample": name, "shift": 0.85}),
            ));
        }
        if ui
            .small_button("FORMANT THINNER")
            .on_hover_text("smaller, thinner voice body; pitch and words unchanged")
            .clicked()
        {
            call = Some((
                "formant_sample".into(),
                json!({"sample": name, "shift": 1.2}),
            ));
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui
            .small_button("RUMBLE CUT")
            .on_hover_text("remove rumble below 90 Hz")
            .clicked()
        {
            call = Some((
                "eq_1band".into(),
                json!({"sample": name, "type": "high_pass", "freq": 90.0}),
            ));
        }
        if ui
            .small_button("MUD CUT")
            .on_hover_text("scoop the boxy sound around 300 Hz")
            .clicked()
        {
            call = Some((
                "eq_1band".into(),
                json!({"sample": name, "type": "peak", "freq": 300.0, "gain": -4.0, "q": 1.0}),
            ));
        }
        if ui
            .small_button("PRESENCE")
            .on_hover_text("bring the sound forward around 4 kHz")
            .clicked()
        {
            call = Some((
                "eq_1band".into(),
                json!({"sample": name, "type": "peak", "freq": 4000.0, "gain": 3.0, "q": 1.0}),
            ));
        }
        if ui
            .small_button("AIR")
            .on_hover_text("add brightness above 10 kHz")
            .clicked()
        {
            call = Some((
                "eq_1band".into(),
                json!({"sample": name, "type": "high_shelf", "freq": 10000.0, "gain": 3.0}),
            ));
        }
        if ui
            .small_button("SMILE")
            .on_hover_text("more lows and highs, fewer mids")
            .clicked()
        {
            call = Some((
                "eq_7band".into(),
                json!({"sample": name, "low_shelf_freq": 120.0, "low_shelf_gain": 2.0, "mid_freq": 1000.0, "mid_gain": -2.0, "high_shelf_freq": 8000.0, "high_shelf_gain": 2.0}),
            ));
        }
        if ui
            .small_button("GATE")
            .on_hover_text("silence the quiet parts between hits")
            .clicked()
        {
            call = Some((
                "expander_gate".into(),
                json!({"sample": name, "ratio": 100.0}),
            ));
        }
        if ui
            .small_button("EXPAND")
            .on_hover_text("gently turn down the quiet parts")
            .clicked()
        {
            call = Some((
                "expander_gate".into(),
                json!({"sample": name, "ratio": 3.0}),
            ));
        }
    });
    ui.label(
        RichText::new("each edit keeps the original and adds a new sample")
            .size(10.0)
            .color(tk.text_dim),
    );
    call
}

fn mix_color(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    egui::Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// Spectrogram of the sample being edited; drag a box on it to set the
/// region (time) and the band (frequency) the edits use.
fn spectrogram_view(ui: &mut egui::Ui, tk: &Tokens, ed: &mut SampleEdit, sp: &Value, dur: f32) {
    let rows = sp["db"].as_array().cloned().unwrap_or_default();
    let edges: Vec<f32> = sp["band_edges_hz"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_f64())
                .map(|v| v as f32)
                .collect()
        })
        .unwrap_or_default();
    if rows.is_empty() || edges.len() < 2 {
        return;
    }
    let (lo_e, hi_e) = (edges[0].max(1.0), *edges.last().unwrap());
    let w = ui.available_width().clamp(160.0, 560.0);
    let h = 96.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, tk.panel_bg2);
    let nf = rows.len();
    let nb = edges.len() - 1;
    let (cw, ch) = (w / nf as f32, h / nb as f32);
    for (fi, row) in rows.iter().enumerate() {
        for (bi, v) in row
            .as_array()
            .map(|r| r.as_slice())
            .unwrap_or(&[])
            .iter()
            .enumerate()
        {
            let t = ((v.as_f64().unwrap_or(-90.0) as f32 + 60.0) / 60.0).clamp(0.0, 1.0);
            if t <= 0.02 {
                continue;
            }
            let x = rect.left() + fi as f32 * cw;
            let y = rect.bottom() - (bi + 1) as f32 * ch;
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(cw + 0.5, ch + 0.5)),
                0.0,
                mix_color(tk.panel_bg2, tk.accent, t),
            );
        }
    }
    let span = (hi_e / lo_e).ln().max(1e-3);
    let t_of = |x: f32| ((x - rect.left()) / w).clamp(0.0, 1.0) * dur;
    let f_of = |y: f32| lo_e * (((rect.bottom() - y) / h).clamp(0.0, 1.0) * span).exp();
    let x_of = |t: f32| rect.left() + (t / dur).clamp(0.0, 1.0) * w;
    let y_of = |f: f32| rect.bottom() - ((f.max(lo_e) / lo_e).ln() / span).clamp(0.0, 1.0) * h;
    if resp.drag_started() {
        ed.anchor = resp.interact_pointer_pos();
    }
    if resp.dragged() {
        if let (Some(a), Some(q)) = (ed.anchor, resp.interact_pointer_pos()) {
            ed.start = (t_of(a.x.min(q.x)) * 1000.0).round() / 1000.0;
            ed.end = (t_of(a.x.max(q.x)) * 1000.0).round() / 1000.0;
            ed.lo = f_of(a.y.max(q.y)).round();
            ed.hi = f_of(a.y.min(q.y)).round();
        }
    }
    if resp.drag_stopped() {
        ed.anchor = None;
    }
    let end = if ed.end > ed.start { ed.end } else { dur };
    let sel = egui::Rect::from_min_max(
        egui::pos2(x_of(ed.start), y_of(ed.hi)),
        egui::pos2(x_of(end), y_of(ed.lo)),
    );
    painter.rect_stroke(sel, 0.0, egui::Stroke::new(1.5_f32, tk.text));
    resp.on_hover_text("drag a box to choose the region and the band");
}

#[cfg(test)]
mod drop_tests {
    use super::*;

    #[test]
    fn dropped_audio_files_become_channels_and_other_files_are_refused() {
        let d = std::env::temp_dir().join(format!("drop_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let tone: Vec<f32> = (0..4410).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        let wav = d.join("My Snare.wav");
        crate::render::write_wav(&wav, &tone, &tone).unwrap();
        let note = d.join("notes.txt");
        std::fs::write(&note, "hello").unwrap();
        let mut e = crate::engine::Engine::new(d.clone());
        let (added, failed) = import_dropped(&mut e, &[wav, note]);
        assert_eq!(added.len(), 1, "added {added:?}, failed {failed:?}");
        assert_eq!(failed.len(), 1, "added {added:?}, failed {failed:?}");
        assert!(failed[0].starts_with("notes.txt"), "{failed:?}");
        let again = d.join("My Snare.wav");
        let (added2, failed2) = import_dropped(&mut e, &[again]);
        assert_eq!(added2.len(), 1, "second drop: {added2:?} {failed2:?}");
        assert_ne!(
            added2[0], added[0],
            "the second file must not reuse the first sound"
        );
        assert_eq!(e.project.samples.len(), 2);
        let track = format!("{}_s", added[0]);
        assert!(
            e.project.tracks.iter().any(|t| t.name == track),
            "no channel {track}: {:?}",
            e.project.tracks.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
    }
}
