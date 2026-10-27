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

type TranslationOutput = Result<(Value, super::PreviewAudio), String>;
struct TranslationJob {
    revision: u64,
    generation: u64,
    start: f32,
    length: f32,
    receiver: std::sync::mpsc::Receiver<TranslationOutput>,
}
struct TranslationReady {
    revision: u64,
    generation: u64,
    start: f32,
    length: f32,
    value: Value,
    audio: super::PreviewAudio,
}
fn translation_current(
    revision: u64,
    generation: u64,
    start: f32,
    length: f32,
    engine: &crate::Engine,
    current: (f32, f32),
) -> bool {
    super::comparison_is_current(revision, generation, engine) && (start, length) == current
}
fn prepare_translation(
    mut own: crate::Engine,
    mode: &str,
    start: f32,
    length: f32,
) -> TranslationOutput {
    let value = own
        .call(
            "preview_mix_translation",
            &json!({"mode":mode,"start_s":start,"length_s":length,"play":true}),
        )
        .map_err(|e| format!("{e:#}"))?;
    let audio = own
        .transport
        .pending_audition
        .take()
        .flatten()
        .ok_or("No preview audio prepared")?;
    Ok((value, audio))
}

#[derive(Default)]
pub struct BrowserState {
    pub pattern_rename: Option<(String, String)>,
    pub search: String,
    pub section: usize,
    /// first open starts on the friendly Sounds list
    pub opened: bool,
    pub track: String,
    /// list_presets + list_palettes, fetched once
    pub catalog: Option<(Value, Value)>,
    /// list_fx_chains, refetched after any browser call (saved chains change)
    pub chains: Option<Value>,
    /// Explicit opt-in: replacing a chain removes its existing effects.
    pub replace_chain: bool,
    pub ab_snapshot: String,
    pub ab_start_s: f32,
    pub ab_length_s: f32,
    ab_job: Option<super::MixComparisonJob>,
    ab_ready: Option<super::MixComparison>,
    ab_checkpoint_project: Option<Project>,
    pub ab_result: String,
    pub translation_start_s: f32,
    pub translation_length_s: f32,
    pub translation_result: String,
    translation_job: Option<TranslationJob>,
    translation_ready: Option<TranslationReady>,
    pub sample_perf: Option<(String, f32, f32, f32, bool, bool, bool, bool, f32, f32)>,
    pub sample_perf_source: Option<(String, f32, f32, f32, bool, bool, bool, bool, f32, f32)>,
    pub kit_level_key: usize,
    pub kit_attack_ms: f32,
    pub kit_decay_ms: f32,
    pub kit_lowpass_hz: f32,
    pub kit_highpass_hz: f32,
    pub kit_velocity_curve: f32,
    pub kit_pan: f32,
    pub kit_level_db: f32,
    pub kit_tune_cents: f32,
    pub kit_accents: Option<(f32, f32, usize, usize)>,
    pub kit_roll_division: usize,
    pub kit_roll: Option<(String, f32, f32, f32, f32, f32, f32)>,
    pub kit_ramp_range: Option<(bool, f32, f32, f32)>,
    pub kit_ramp: Option<(String, f32, f32)>,
    pub kit_flams: Option<(String, f32, f32)>,
    pub kit_swing: Option<f32>,
    pub kit_pocket_ms: f32,
    pub kit_length_ms: f32,
    pub kit_preview_velocity: Option<f32>,
    pub kit_preview_take: u32,
    pub kit_edit_source: Option<(String, u8, [f32; 3])>,
    pub drum_map_key: usize,
    pub drum_map_pattern: String,
    pub production_read_status: String,
    pub echo_send: Option<(usize, f32, f32, f32)>,
    pub parallel_send: Option<(f32, f32, f32, f32, f32)>,
    pub room_send: Option<(f32, f32, f32, f32)>,
    pub aux_bus: String,
    pub aux_db: f32,
    pub aux_pre: bool,
    pub return_filter: Option<(f32, f32)>,
    pub return_duck: Option<(f32, f32, f32)>,
    pub aux_source: Option<(String, String, Option<(f32, bool)>)>,
    pub glue_ready: bool,
    pub glue_threshold: f32,
    pub glue_ratio: f32,
    pub glue_attack: f32,
    pub glue_release: f32,
    pub glue_mix: f32,
    pub glue_hpf: f32,
    pub colour_ready: bool,
    pub colour_mode: usize,
    pub colour_drive: f32,
    pub colour_mix: f32,
    pub colour_output: f32,
    pub colour_tone: f32,
    pub punch_ready: bool,
    pub punch_attack: f32,
    pub punch_sustain: f32,
    pub delay_ready: bool,
    pub delay_division: usize,
    pub delay_mix: f32,
    pub delay_feedback: f32,
    pub delay_tone: f32,
    pub delay_ping: bool,
    pub width_ready: bool,
    pub safe_width: f32,
    pub mono_cutoff: f32,
    /// sound library: import field, catalog search, background job
    pub url: String,
    pub url_name: String,
    /// a sound file on this computer, typed or pasted (drag and drop is not available everywhere)
    pub file_path: String,
    pub lib_hits: Option<(String, Value)>,
    pub imports: Option<Value>,
    pub credits: String,
    pub job: Option<(String, std::sync::mpsc::Receiver<Result<Value, String>>)>,
    pub job_result: String,
    /// the audio editor's sample, region and band
    pub edit: SampleEdit,
    /// soundgoodizer row: mode 0-3 (A-D) and amount 1-100 (0 = not set yet)
    pub duck_source: String,
    pub duck_depth: f32,
    pub duck_beats: f32,
    pub duck_attack: f32,
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
        if !path.exists() {
            failed.push(format!("{file}: there is no file at that path"));
            continue;
        }
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

impl BrowserState {
    /// Read-only UI load, all-or-nothing for the selected processor.
    fn read_production(&mut self, p: &Project, track: &str, kind: &str) -> Result<String, String> {
        let chain = p
            .chain_of(track)
            .ok_or_else(|| "Channel not found".to_string())?;
        let fx = chain
            .iter()
            .find(|f| {
                matches!(
                    (kind, *f),
                    ("glue", crate::fx::Effect::Compressor(_))
                        | ("colour", crate::fx::Effect::Saturator(_))
                        | ("punch", crate::fx::Effect::Transient(_))
                        | ("delay", crate::fx::Effect::Delay(_))
                        | ("width", crate::fx::Effect::Width(_))
                )
            })
            .ok_or_else(|| format!("No {kind} effect on {track}; controls unchanged"))?;
        let valid = |x: f32, lo: f32, hi: f32| x.is_finite() && x >= lo && x <= hi;
        match fx {
            crate::fx::Effect::Compressor(v) => {
                if !valid(v.threshold_db, -40.0, 0.0)
                    || !valid(v.ratio, 1.0, 8.0)
                    || !valid(v.attack_ms, 0.1, 100.0)
                    || !valid(v.release_ms, 10.0, 1000.0)
                    || !valid(v.mix, 0.0, 1.0)
                    || !valid(v.sc_hpf_hz, 0.0, 300.0)
                    || v.knee_db != 6.0
                    || v.makeup_db != 0.0
                    || v.lookahead_ms != 0.0
                {
                    return Err("Compressor outside glue settings (including knee/makeup/lookahead); use rack controls. Nothing loaded".into());
                }
                self.glue_ready = true;
                self.glue_threshold = v.threshold_db;
                self.glue_ratio = v.ratio;
                self.glue_attack = v.attack_ms;
                self.glue_release = v.release_ms;
                self.glue_mix = v.mix;
                self.glue_hpf = v.sc_hpf_hz;
            }
            crate::fx::Effect::Saturator(v) => {
                let mode =
                    match v.mode {
                        crate::fx::SatMode::Tape => 0,
                        crate::fx::SatMode::Tube => 1,
                        crate::fx::SatMode::Transistor => 2,
                        _ => return Err(
                            "Saturator mode not supported here; use rack controls. Nothing loaded"
                                .into(),
                        ),
                    };
                if !valid(v.drive_db, 0.0, 24.0)
                    || !valid(v.mix, 0.0, 1.0)
                    || !valid(v.output_db, -24.0, 0.0)
                    || !valid(v.tone_hz, 500.0, 16000.0)
                    || v.bias != 0.0
                    || !v.oversample
                {
                    return Err("Saturator outside colour settings (including bias/oversampling); use rack controls. Nothing loaded".into());
                }
                self.colour_ready = true;
                self.colour_mode = mode;
                self.colour_drive = v.drive_db;
                self.colour_mix = v.mix;
                self.colour_output = v.output_db;
                self.colour_tone = v.tone_hz;
            }
            crate::fx::Effect::Transient(v) => {
                if !valid(v.attack, -1.0, 1.0) || !valid(v.sustain, -1.0, 1.0) {
                    return Err("Transient outside punch range. Nothing loaded".into());
                }
                self.punch_ready = true;
                self.punch_attack = v.attack;
                self.punch_sustain = v.sustain;
            }
            crate::fx::Effect::Delay(v) => {
                let div = [2.0f32, 3.0, 4.0, 4.0 / 3.0]
                    .iter()
                    .position(|x| (*x - v.steps).abs() < 1e-5)
                    .ok_or_else(|| {
                        "Delay timing not supported here; use rack controls. Nothing loaded"
                            .to_string()
                    })?;
                if !valid(v.mix, 0.0, 0.6)
                    || !valid(v.feedback, 0.0, 0.75)
                    || !valid(v.tone, 500.0, 16000.0)
                {
                    return Err("Delay outside musical controls range. Nothing loaded".into());
                }
                self.delay_ready = true;
                self.delay_division = div;
                self.delay_mix = v.mix;
                self.delay_feedback = v.feedback;
                self.delay_tone = v.tone;
                self.delay_ping = v.ping_pong;
            }
            crate::fx::Effect::Width(v) => {
                if !valid(v.amount, 0.0, 2.0)
                    || !(v.low_mono_hz == 0.0 || valid(v.low_mono_hz, 40.0, 500.0))
                {
                    return Err("Width outside bass-safe range. Nothing loaded".into());
                }
                self.width_ready = true;
                self.safe_width = v.amount;
                self.mono_cutoff = v.low_mono_hz;
            }
            _ => return Err("Unsupported effect".into()),
        }
        Ok(format!(
            "Loaded {kind} from {track} / {}. Read only; Apply enables it and writes settings",
            fx.id()
        ))
    }
}

impl Studio {
    pub(super) fn browser_view(&mut self, ui: &mut egui::Ui, p: &Project) {
        let tk = Tokens::DARK;
        ui.set_width((ui.clip_rect().width() - 32.0).max(180.0));
        self.poll_library_job();
        self.poll_translation_job(ui.ctx());
        self.poll_browser_comparison(ui.ctx());
        if !self.browser.opened {
            self.browser.opened = true;
            self.browser.section = 5;
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
                    .desired_width((ui.available_width() - 80.0).clamp(110.0, 170.0))
                    .hint_text("808, bell, reverb, dark..."),
            );
            ui.end_row();
            ui.label(RichText::new("CHANNEL").size(11.0).color(tk.text_dim));
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
                (5usize, "Recorded sounds"),
                (1, "Modeled synths"),
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
        if section == 5 {
            self.recorded_starters(ui, &track);
        }
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
        if section <= 1 {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("YOUR OWN FILE").size(11.0).color(tk.text_dim));
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.browser.file_path)
                        .desired_width((ui.available_width() - 150.0).clamp(110.0, 300.0))
                        .hint_text("path to a sound file"),
                )
                .on_hover_text("Paste the path of a wav, mp3, flac or ogg file");
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let ready = !self.browser.file_path.trim().is_empty();
                let go = ui
                    .add_enabled(ready, egui::Button::new("+ Add as channel"))
                    .on_hover_text("Bring this file into the project on its own channel. You can also drop a file on the window.")
                    .clicked();
                if (go || enter) && ready {
                    let raw = self.browser.file_path.trim().trim_matches(|c| c == '"' || c == '\'');
                    let raw = raw.strip_prefix("file://").unwrap_or(raw);
                    let path = match (raw.strip_prefix("~/"), std::env::var_os("HOME")) {
                        (Some(rest), Some(home)) => std::path::PathBuf::from(home).join(rest),
                        _ => std::path::PathBuf::from(raw),
                    };
                    let (added, failed) = {
                        let mut e = self.engine.lock().unwrap();
                        import_dropped(&mut e, &[path])
                    };
                    self.toast = match (added.first(), failed.first()) {
                        (Some(a), _) => {
                            self.browser.file_path.clear();
                            // like any channel added here, it is the selected one right away
                            self.selected = p.tracks.len();
                            self.browser.track = format!("{a}_s");
                            Some((format!("Added {a} as a new channel"), 0.0, true))
                        }
                        (None, Some(f)) => Some((f.clone(), 0.0, false)),
                        _ => None,
                    };
                }
            });
            ui.add_space(4.0);
        }
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
                call = Some(("preview_mix_translation".into(), json!({"stop":true})));
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
            ui.checkbox(&mut self.browser.replace_chain, "Replace existing chain")
                .on_hover_text("Off: append effects. On: remove this channel's existing effects first. Undo restores them.");
            ui.label(RichText::new("Presets are starting points. Compare at matched loudness; appending twice stacks effects.").size(10.0).color(tk.text_dim));
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
                        .on_hover_text(format!("{d}\nTarget: {track}. Replace existing: {}", self.browser.replace_chain))
                        .clicked()
                        && !track.is_empty()
                    {
                        call = Some(("apply_fx_chain".into(), json!({"track": track, "chain": n, "replace": self.browser.replace_chain})));
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
            ui.add_space(8.0);
            if !self.browser.production_read_status.is_empty() {
                ui.label(
                    RichText::new(&self.browser.production_read_status)
                        .size(11.0)
                        .color(tk.text),
                );
            }
            if !self.browser.glue_ready {
                self.browser.glue_ready = true;
                self.browser.glue_threshold = -18.0;
                self.browser.glue_ratio = 2.0;
                self.browser.glue_attack = 25.0;
                self.browser.glue_release = 120.0;
                self.browser.glue_mix = 0.5;
                self.browser.glue_hpf = 90.0;
            }
            ui.horizontal_wrapped(|ui|{
                ui.label(RichText::new("BUS / DRUM GLUE").size(11.0).strong().color(tk.header_text));
                if ui.small_button("READ CURRENT").on_hover_text("Load first matching effect settings without changing audio. Unsupported settings are not rounded or reset.").clicked(){self.browser.production_read_status=self.browser.read_production(p,&track,"glue").unwrap_or_else(|e|e);}
                ui.add(egui::DragValue::new(&mut self.browser.glue_threshold).range(-40.0..=0.0).speed(0.1).suffix(" dB threshold"));
                ui.add(egui::DragValue::new(&mut self.browser.glue_ratio).range(1.0..=8.0).speed(0.1).suffix(":1"));
                ui.add(egui::DragValue::new(&mut self.browser.glue_attack).range(0.1..=100.0).speed(0.1).suffix(" ms attack"));
                ui.add(egui::DragValue::new(&mut self.browser.glue_release).range(10.0..=1000.0).speed(1.0).suffix(" ms release"));
                ui.add(egui::DragValue::new(&mut self.browser.glue_mix).range(0.0..=1.0).speed(0.01).prefix("Wet "));
                ui.add(egui::DragValue::new(&mut self.browser.glue_hpf).range(0.0..=300.0).speed(1.0).suffix(" Hz detector cut"));
                if ui.small_button("APPLY GLUE").clicked(){call=Some(("set_bus_glue".into(),json!({"track":track,"threshold_db":self.browser.glue_threshold,"ratio":self.browser.glue_ratio,"attack_ms":self.browser.glue_attack,"release_ms":self.browser.glue_release,"mix":self.browser.glue_mix,"sc_hpf_hz":self.browser.glue_hpf})));}
            });
            ui.label(RichText::new("Retunes/enables first compressor. Soft knee, no lookahead, makeup or limiter. Detector cut reduces bass-triggered compression, not output bass. Compare matched A/B.").size(10.0).color(tk.text_dim));
            ui.add_space(8.0);
            ui.label(
                RichText::new("SHARED ROOM RETURN")
                    .size(11.0)
                    .strong()
                    .color(tk.header_text),
            );
            let room = self
                .browser
                .room_send
                .get_or_insert((-12.0, 0.65, 12.0, 180.0));
            egui::Grid::new("shared_room_controls")
                .num_columns(2)
                .min_col_width(130.0)
                .show(ui, |ui| {
                    ui.label("Send level");
                    ui.add(
                        egui::DragValue::new(&mut room.0)
                            .range(-36.0..=0.0)
                            .speed(0.5)
                            .suffix(" dB"),
                    );
                    ui.end_row();
                    ui.label("Room decay");
                    ui.add(
                        egui::DragValue::new(&mut room.1)
                            .range(0.15..=3.0)
                            .speed(0.05)
                            .suffix(" s"),
                    );
                    ui.end_row();
                    ui.label("Predelay");
                    ui.add(
                        egui::DragValue::new(&mut room.2)
                            .range(0.0..=80.0)
                            .speed(1.0)
                            .suffix(" ms"),
                    );
                    ui.end_row();
                    ui.label("Wet low cut");
                    ui.add(
                        egui::DragValue::new(&mut room.3)
                            .range(20.0..=1000.0)
                            .speed(10.0)
                            .suffix(" Hz"),
                    );
                    ui.end_row();
                });
            if ui
                .small_button(format!("CREATE {track}_room + SEND"))
                .clicked()
            {
                call = Some((
                    "create_room_send".into(),
                    json!({"track":track,"send_db":room.0,"decay_s":room.1,"predelay_ms":room.2,"low_cut_hz":room.3}),
                ));
            }
            ui.label(RichText::new("New 100% wet return to master + post-fader send; dry route stays intact. Existing bus names are rejected. Share with other channels using sends; edit return FX normally. No limiter or level match. Undo removes the setup.").size(10.0).color(tk.text_dim));
            ui.add_space(8.0);
            ui.label(
                RichText::new("PARALLEL DRUM COMPRESSION")
                    .size(11.0)
                    .strong()
                    .color(tk.header_text),
            );
            let pc = self
                .browser
                .parallel_send
                .get_or_insert((-12.0, -24.0, 8.0, 20.0, 120.0));
            egui::Grid::new("parallel_return_controls")
                .num_columns(2)
                .min_col_width(130.0)
                .show(ui, |ui| {
                    ui.label("Send level");
                    ui.add(
                        egui::DragValue::new(&mut pc.0)
                            .range(-36.0..=0.0)
                            .speed(0.5)
                            .suffix(" dB"),
                    );
                    ui.end_row();
                    ui.label("Threshold");
                    ui.add(
                        egui::DragValue::new(&mut pc.1)
                            .range(-40.0..=0.0)
                            .speed(0.5)
                            .suffix(" dB"),
                    );
                    ui.end_row();
                    ui.label("Ratio");
                    ui.add(
                        egui::DragValue::new(&mut pc.2)
                            .range(1.0..=12.0)
                            .speed(0.1)
                            .suffix(":1"),
                    );
                    ui.end_row();
                    ui.label("Attack");
                    ui.add(
                        egui::DragValue::new(&mut pc.3)
                            .range(0.1..=100.0)
                            .speed(1.0)
                            .suffix(" ms"),
                    );
                    ui.end_row();
                    ui.label("Recovery");
                    ui.add(
                        egui::DragValue::new(&mut pc.4)
                            .range(10.0..=1000.0)
                            .speed(5.0)
                            .suffix(" ms"),
                    );
                    ui.end_row();
                });
            if ui
                .small_button(format!("CREATE {track}_parallel + SEND"))
                .clicked()
            {
                call = Some((
                    "create_parallel_send".into(),
                    json!({"track":track,"send_db":pc.0,"threshold_db":pc.1,"ratio":pc.2,"attack_ms":pc.3,"release_ms":pc.4}),
                ));
            }
            ui.label(RichText::new("New processed-only compressor return, original dry route intact. 0 dB makeup, 6 dB knee, 80 Hz detector low cut. Adds signal and may raise peaks; no limiter/level match. Existing names fail. Aux Sends adjusts/shares it; Undo removes setup.").size(10.0).color(tk.text_dim));
            ui.add_space(8.0);
            ui.label(
                RichText::new("SHARED ECHO RETURN")
                    .size(11.0)
                    .strong()
                    .color(tk.header_text),
            );
            let echo = self
                .browser
                .echo_send
                .get_or_insert((1, -12.0, 0.3, 4500.0));
            let divisions = ["eighth", "dotted_eighth", "quarter"];
            let labels = ["1/8", "Dotted 1/8", "1/4"];
            egui::Grid::new("echo_return_controls")
                .num_columns(2)
                .min_col_width(130.0)
                .show(ui, |ui| {
                    ui.label("Echo timing");
                    egui::ComboBox::from_id_salt("echo_return_division")
                        .selected_text(labels[echo.0.min(2)])
                        .show_ui(ui, |ui| {
                            for (i, label) in labels.iter().enumerate() {
                                ui.selectable_value(&mut echo.0, i, *label);
                            }
                        });
                    ui.end_row();
                    ui.label("Send level");
                    ui.add(
                        egui::DragValue::new(&mut echo.1)
                            .range(-36.0..=0.0)
                            .speed(0.5)
                            .suffix(" dB"),
                    );
                    ui.end_row();
                    ui.label("Feedback");
                    ui.add(
                        egui::DragValue::new(&mut echo.2)
                            .range(0.0..=0.7)
                            .speed(0.01),
                    );
                    ui.end_row();
                    ui.label("Echo tone");
                    ui.add(
                        egui::DragValue::new(&mut echo.3)
                            .range(500.0..=12000.0)
                            .speed(50.0)
                            .suffix(" Hz"),
                    );
                    ui.end_row();
                });
            if ui
                .small_button(format!("CREATE {track}_echo + SEND"))
                .clicked()
            {
                call = Some((
                    "create_echo_send".into(),
                    json!({"track":track,"division":divisions[echo.0.min(2)],"send_db":echo.1,"feedback":echo.2,"tone_hz":echo.3}),
                ));
            }
            ui.label(RichText::new("New 100% wet ping-pong return + post-fader send; dry channel unchanged. Syncs to base BPM, not tempo automation. Existing names fail. Feedback can build level; no limiter/gain match. Use Aux Sends below to share, adjust or remove sends. Undo removes setup.").size(10.0).color(tk.text_dim));
            ui.add_space(8.0);
            ui.label(
                RichText::new("AUX SENDS / SHARE A RETURN")
                    .size(11.0)
                    .strong()
                    .color(tk.header_text),
            );
            if !p.buses.iter().any(|b| b.name == self.browser.aux_bus) {
                self.browser.aux_bus = p.buses.first().map(|b| b.name.clone()).unwrap_or_default();
            }
            egui::ComboBox::from_id_salt("aux_return_bus")
                .selected_text(if self.browser.aux_bus.is_empty() {
                    "No return buses"
                } else {
                    &self.browser.aux_bus
                })
                .show_ui(ui, |ui| {
                    for b in &p.buses {
                        ui.selectable_value(&mut self.browser.aux_bus, b.name.clone(), &b.name);
                    }
                });
            let source = p.tracks.iter().find(|t| t.name == track);
            let send = source.and_then(|t| {
                t.sends
                    .iter()
                    .find(|s| s.bus.eq_ignore_ascii_case(&self.browser.aux_bus))
            });
            let values = send.map(|s| (s.db, s.pre_fader));
            let signature = (track.clone(), self.browser.aux_bus.clone(), values);
            if self.browser.aux_source.as_ref() != Some(&signature) {
                (self.browser.aux_db, self.browser.aux_pre) = values.unwrap_or((-12.0, false));
                self.browser.aux_source = Some(signature);
            }
            let routed = source.is_some_and(|t| {
                t.output
                    .as_ref()
                    .is_some_and(|b| b.eq_ignore_ascii_case(&self.browser.aux_bus))
            });
            let automated = p.automation.iter().any(|l| {
                (l.target.eq_ignore_ascii_case(&track)
                    || source.is_some_and(|t| !t.id.is_empty() && l.target == t.id))
                    && match crate::automation::parse_target(&l.param) {
                        Some(crate::automation::Target::Send(b)) => {
                            b.eq_ignore_ascii_case(&self.browser.aux_bus)
                                || send.is_some_and(|s| {
                                    !s.id.is_empty() && b.eq_ignore_ascii_case(&s.id)
                                })
                        }
                        _ => false,
                    }
            });
            ui.label(match values {
                Some((db, pre)) => format!(
                    "Current send: {db:+.1} dB, {}-fader",
                    if pre { "pre" } else { "post" }
                ),
                None => "No send from this channel to the selected bus".into(),
            });
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut self.browser.aux_db)
                        .range(-60.0..=12.0)
                        .speed(0.5)
                        .suffix(" dB send"),
                );
                ui.checkbox(&mut self.browser.aux_pre, "Pre-fader");
            });
            let usable =
                source.is_some() && !self.browser.aux_bus.is_empty() && !routed && !automated;
            ui.horizontal(|ui| {
                if ui.add_enabled(usable,egui::Button::new("SET SEND").small()).clicked(){call=Some(("set_send".into(),json!({"track":track,"bus":self.browser.aux_bus,"db":self.browser.aux_db,"pre_fader":self.browser.aux_pre})));}
                if ui.add_enabled(usable&&send.is_some(),egui::Button::new("REMOVE SEND").small()).clicked(){call=Some(("set_send".into(),json!({"track":track,"bus":self.browser.aux_bus,"remove":true})));}
            });
            if automated {
                ui.label(RichText::new("This send has automation. Edit or remove its lane first; these buttons do not override or delete automation.").color(tk.text_dim));
            }
            if routed {
                ui.label(RichText::new("This channel already outputs to that bus. A send would double the route; choose another return.").color(tk.text_dim));
            }
            ui.label(RichText::new("Any bus can receive a send. For room returns keep reverb 100% wet, or dry audio can be doubled. Post-fader follows channel level; pre-fader does not. Bus mute/FX/output affect the return. Source changes refresh fields and discard unapplied drafts. Remove deletes this send only, not the bus. Undo restores.").size(10.0).color(tk.text_dim));
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        !self.browser.aux_bus.is_empty(),
                        egui::Button::new(format!("HEAR {} ONLY", self.browser.aux_bus)).small(),
                    )
                    .clicked()
                {
                    call = Some((
                        "preview_return".into(),
                        json!({"bus":self.browser.aux_bus,"bars":4,"play":true}),
                    ));
                }
                if ui
                    .add_enabled(
                        !self.browser.aux_bus.is_empty(),
                        egui::Button::new("MIX WITHOUT / WITH RETURN").small(),
                    )
                    .clicked()
                {
                    call = Some((
                        "preview_return".into(),
                        json!({"bus":self.browser.aux_bus,"mode":"mix_ab","bars":4,"play":true}),
                    ));
                }
                if ui.small_button("STOP RETURN PREVIEW").clicked() {
                    call = Some(("preview_return".into(), json!({"stop":true})));
                }
            });
            ui.label(RichText::new("Bus-only audition, first 4 bars up to 20s. Full sources/triggers retained; no mute/solo/project edits. Post-bus FX/fader, excludes master/downstream processing. Muted bus must be unmuted explicitly. Preview may reduce peaks, never boosts; not an in-mix solo.").size(10.0).color(tk.text_dim));
            ui.add_space(8.0);
            ui.label(RichText::new("Mix comparison: WITHOUT selected bus, 0.6s gap, WITH it. Includes downstream/master processing. Same peak reduction both halves; no loudness match. Temporary rendered copy only, no project edits. Up to 20s each; selected bus must be unmuted.").size(10.0).color(tk.text_dim));
            ui.label(
                RichText::new("FILTER SELECTED RETURN")
                    .size(11.0)
                    .strong()
                    .color(tk.header_text),
            );
            let fb = p.buses.iter().find(|b| b.name == self.browser.aux_bus);
            let fc = fb.and_then(|b| {
                b.effects.iter().find_map(|f| match f {
                    crate::fx::Effect::ParametricEq(q) if q.id.starts_with("returnfilter") => {
                        Some(q)
                    }
                    _ => None,
                })
            });
            let filter = self.browser.return_filter.get_or_insert((180.0, 6000.0));
            if let Some(q) = fc {
                if q.bands.len() == 2 {
                    ui.label(format!(
                        "Current shortcut: {:.0} Hz low cut / {:.0} Hz high cut{}",
                        q.bands[0].freq,
                        q.bands[1].freq,
                        if q.bypass { " (bypassed)" } else { "" }
                    ));
                    if ui.small_button("READ RETURN FILTER").clicked() {
                        *filter = (q.bands[0].freq, q.bands[1].freq);
                    }
                }
            } else {
                ui.label("No dedicated return filter yet");
            }
            let fa = fb.is_some_and(|b| {
                p.automation
                    .iter()
                    .any(|l| l.target.eq_ignore_ascii_case(&b.name) || l.target == b.id)
            });
            ui.horizontal_wrapped(|ui|{
                ui.add(egui::DragValue::new(&mut filter.0).range(20.0..=1000.0).speed(10.0).suffix(" Hz low cut"));
                ui.add(egui::DragValue::new(&mut filter.1).range(1000.0..=18000.0).speed(100.0).suffix(" Hz high cut"));
                if ui.add_enabled(fb.is_some()&&!fa,egui::Button::new(format!("FILTER {}",self.browser.aux_bus)).small()).clicked(){call=Some(("set_return_filter".into(),json!({"bus":self.browser.aux_bus,"low_cut_hz":filter.0,"high_cut_hz":filter.1})));}
            });
            if fa {
                ui.label(
                    "Bus has automation: edit/remove its lanes before using this filter shortcut.",
                );
            }
            ui.label(RichText::new("Filters only the selected bus. First add goes after current effects; retunes keep its position. Dry channel/send intact. Reapply retunes/enables this dedicated EQ, not unrelated EQ. All shared contributors affected. Not brickwall; no gain match/limiter. Undo restores.").size(10.0).color(tk.text_dim));
            ui.add_space(8.0);
            ui.label(
                RichText::new("DUCK SELECTED RETURN")
                    .size(11.0)
                    .strong()
                    .color(tk.header_text),
            );
            let current_duck = p
                .buses
                .iter()
                .find(|b| b.name.eq_ignore_ascii_case(&self.browser.aux_bus))
                .and_then(|b| {
                    b.effects.iter().find_map(|fx| match fx {
                        crate::fx::Effect::Sidechain(d)
                            if d.source.eq_ignore_ascii_case(&track) =>
                        {
                            Some(d)
                        }
                        _ => None,
                    })
                });
            if let Some(d) = current_duck {
                ui.label(format!(
                    "Current duck: {:.1} dB, {:.1} ms recovery, {:.1} ms attack{}",
                    -20.0 * (1.0 - d.amount.clamp(0.0, 1.0)).max(1e-6).log10(),
                    d.release_ms,
                    d.attack_ms,
                    if d.bypass { " (bypassed)" } else { "" }
                ));
            } else {
                ui.label("No ducker from this channel on the selected bus");
            }
            let duck = self.browser.return_duck.get_or_insert((9.0, 0.5, 3.0));
            if let Some(d) = current_duck {
                if ui.small_button("READ CURRENT DUCK").clicked() {
                    *duck = (
                        -20.0 * (1.0 - d.amount.clamp(0.0, 1.0)).max(1e-6).log10(),
                        d.release_ms * p.bpm / 60000.0,
                        d.attack_ms,
                    );
                }
            }

            egui::Grid::new("return_duck_controls")
                .num_columns(2)
                .min_col_width(130.0)
                .show(ui, |ui| {
                    ui.label("Duck depth");
                    ui.add(
                        egui::DragValue::new(&mut duck.0)
                            .range(0.0..=24.0)
                            .speed(0.5)
                            .suffix(" dB"),
                    );
                    ui.end_row();
                    ui.label("Recovery");
                    ui.add(
                        egui::DragValue::new(&mut duck.1)
                            .range(0.0625..=4.0)
                            .speed(0.0625)
                            .suffix(" beats"),
                    );
                    ui.end_row();
                    ui.label("Attack");
                    ui.add(
                        egui::DragValue::new(&mut duck.2)
                            .range(0.1..=30.0)
                            .speed(0.1)
                            .suffix(" ms"),
                    );
                    ui.end_row();
                });
            if ui
                .add_enabled(
                    source.is_some() && !self.browser.aux_bus.is_empty(),
                    egui::Button::new(format!("DUCK {} FROM {track}", self.browser.aux_bus))
                        .small(),
                )
                .clicked()
            {
                call = Some((
                    "duck_to_track".into(),
                    json!({"track":self.browser.aux_bus,"source":track,"depth_db":duck.0,"release_beats":duck.1,"attack_ms":duck.2}),
                ));
            }
            ui.label(RichText::new("Ducks only the selected bus from this channel's note/clip onsets, then recovers. Dry channel/send stay intact. Shared bus: every contributor is ducked. Note/clip-onset triggers, not continuous audio-level detection. Recovery converts at Apply BPM, not a live tempo link. Same source retunes/enables its existing ducker; other effects stay. Undo restores.").size(10.0).color(tk.text_dim));
            ui.add_space(8.0);
            if !self.browser.colour_ready {
                self.browser.colour_ready = true;
                self.browser.colour_drive = 6.0;
                self.browser.colour_mix = 0.3;
                self.browser.colour_output = -3.0;
                self.browser.colour_tone = 12000.0;
            }
            ui.horizontal_wrapped(|ui|{
                ui.label(RichText::new("TONE COLOUR").size(11.0).strong().color(tk.header_text));
                if ui.small_button("READ CURRENT").on_hover_text("Load first matching effect settings without changing audio. Unsupported settings are not rounded or reset.").clicked(){self.browser.production_read_status=self.browser.read_production(p,&track,"colour").unwrap_or_else(|e|e);}
                let modes=["tape","tube","transistor"];
                egui::ComboBox::from_id_salt("track_colour_mode").selected_text(modes[self.browser.colour_mode.min(2)]).show_ui(ui,|ui|{for(i,mode)in modes.iter().enumerate(){ui.selectable_value(&mut self.browser.colour_mode,i,*mode);}});
                ui.add(egui::DragValue::new(&mut self.browser.colour_drive).range(0.0..=24.0).speed(0.1).suffix(" dB drive"));
                ui.add(egui::DragValue::new(&mut self.browser.colour_mix).range(0.0..=1.0).speed(0.01).prefix("Wet "));
                ui.add(egui::DragValue::new(&mut self.browser.colour_output).range(-24.0..=0.0).speed(0.1).suffix(" dB output"));
                ui.add(egui::DragValue::new(&mut self.browser.colour_tone).range(500.0..=16000.0).speed(50.0).suffix(" Hz tone"));
                if ui.small_button("APPLY COLOUR").clicked(){call=Some(("set_track_colour".into(),json!({"track":track,"mode":modes[self.browser.colour_mode.min(2)],"drive_db":self.browser.colour_drive,"mix":self.browser.colour_mix,"output_db":self.browser.colour_output,"tone_hz":self.browser.colour_tone})));}
            });
            ui.label(RichText::new("Retunes/enables first saturator, oversampling on. Output trim affects dry+wet. No automatic level match or limiter; compare with matched A/B below.").size(10.0).color(tk.text_dim));
            ui.add_space(8.0);
            if !self.browser.punch_ready {
                self.browser.punch_ready = true;
                self.browser.punch_attack = 0.25;
                self.browser.punch_sustain = -0.15;
            }
            ui.horizontal_wrapped(|ui|{
                ui.label(RichText::new("DRUM ATTACK / TAIL").size(11.0).strong().color(tk.header_text));
                if ui.small_button("READ CURRENT").on_hover_text("Load first matching effect settings without changing audio. Unsupported settings are not rounded or reset.").clicked(){self.browser.production_read_status=self.browser.read_production(p,&track,"punch").unwrap_or_else(|e|e);}
                ui.add(egui::DragValue::new(&mut self.browser.punch_attack).range(-1.0..=1.0).speed(0.01).prefix("Attack "));
                ui.add(egui::DragValue::new(&mut self.browser.punch_sustain).range(-1.0..=1.0).speed(0.01).prefix("Sustain "));
                if ui.small_button("APPLY PUNCH").clicked(){call=Some(("set_drum_punch".into(),json!({"track":track,"attack":self.browser.punch_attack,"sustain":self.browser.punch_sustain})));}
            });
            ui.label(RichText::new("Attack: soften(-) / sharpen(+). Sustain: reduce(-) / lift(+). Reapply retunes first transient effect. No gain matching or peak limit; check output level.").size(10.0).color(tk.text_dim));
            ui.add_space(8.0);
            if !self.browser.delay_ready {
                self.browser.delay_ready = true;
                self.browser.delay_division = 1;
                self.browser.delay_mix = 0.18;
                self.browser.delay_feedback = 0.25;
                self.browser.delay_tone = 4500.0;
                self.browser.delay_ping = true;
            }
            ui.horizontal_wrapped(|ui|{
                ui.label(RichText::new("MUSICAL DELAY").size(11.0).strong().color(tk.header_text));
                if ui.small_button("READ CURRENT").on_hover_text("Load first matching effect settings without changing audio. Unsupported settings are not rounded or reset.").clicked(){self.browser.production_read_status=self.browser.read_production(p,&track,"delay").unwrap_or_else(|e|e);}
                let labels=["1/8","Dotted1/8","1/4","1/8triplet"];
                egui::ComboBox::from_id_salt("musical_delay_division").selected_text(labels[self.browser.delay_division.min(3)]).show_ui(ui,|ui|{for(i,label)in labels.iter().enumerate(){ui.selectable_value(&mut self.browser.delay_division,i,*label);}});
                ui.add(egui::DragValue::new(&mut self.browser.delay_mix).range(0.0..=0.6).speed(0.01).prefix("Wet "));
                ui.add(egui::DragValue::new(&mut self.browser.delay_feedback).range(0.0..=0.75).speed(0.01).prefix("Feedback "));
                ui.add(egui::DragValue::new(&mut self.browser.delay_tone).range(500.0..=16000.0).speed(50.0).suffix(" Hz tone"));
                ui.checkbox(&mut self.browser.delay_ping,"Ping-pong");
                if ui.small_button("APPLY DELAY").clicked(){let division=["eighth","dotted_eighth","quarter","eighth_triplet"][self.browser.delay_division.min(3)];call=Some(("set_musical_delay".into(),json!({"track":track,"division":division,"mix":self.browser.delay_mix,"feedback":self.browser.delay_feedback,"tone_hz":self.browser.delay_tone,"ping_pong":self.browser.delay_ping})));}
            });
            ui.label(RichText::new("Echo timing uses project base BPM (not tempo maps). Reapply retunes/enables the first delay, preserving other effects. Wet0 is dry. Compare before adding more feedback.").size(10.0).color(tk.text_dim));
            ui.add_space(8.0);
            if !self.browser.width_ready {
                self.browser.width_ready = true;
                self.browser.safe_width = 1.0;
                self.browser.mono_cutoff = 120.0;
            }
            ui.horizontal_wrapped(|ui|{
                ui.label(RichText::new("BASS-SAFE WIDTH").size(11.0).strong().color(tk.header_text));
                if ui.small_button("READ CURRENT").on_hover_text("Load first matching effect settings without changing audio. Unsupported settings are not rounded or reset.").clicked(){self.browser.production_read_status=self.browser.read_production(p,&track,"width").unwrap_or_else(|e|e);}
                ui.add(egui::DragValue::new(&mut self.browser.safe_width).range(0.0..=2.0).speed(0.01).prefix("Width "));
                let mut cut=self.browser.mono_cutoff!=0.0;if ui.checkbox(&mut cut,"Low-side cut").changed(){self.browser.mono_cutoff=if cut{120.0}else{0.0};}
                if cut{ui.add(egui::DragValue::new(&mut self.browser.mono_cutoff).range(40.0..=500.0).speed(1.0).suffix(" Hz"));}
                if ui.small_button("APPLY WIDTH").clicked(){call=Some(("set_bass_safe_width".into(),json!({"track":track,"width":self.browser.safe_width,"mono_below_hz":self.browser.mono_cutoff})));}
            });
            ui.label(RichText::new("Width0=mono,1=unchanged highs,2=wider. Cut reduces low stereo side, not a brick wall; cannot restore already-cancelled bass. Reapply retunes first width effect.").size(10.0).color(tk.text_dim));
            ui.add_space(8.0);
            if self.browser.ab_length_s == 0.0 {
                self.browser.ab_length_s = 6.0;
            }
            let ab_before = (
                self.browser.ab_snapshot.clone(),
                self.browser.ab_start_s,
                self.browser.ab_length_s,
            );
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("MATCHED A/B")
                        .size(11.0)
                        .strong()
                        .color(tk.header_text),
                );
                if ui.small_button("CAPTURE BEFORE").clicked() {
                    match super::capture_mix_checkpoint(&mut self.engine.lock().unwrap()) {
                        Ok(name) => self.browser.ab_snapshot = name,
                        Err(e) => self.browser.ab_result = format!("Capture failed: {e:#}"),
                    }
                }
                let names: Vec<String> = self
                    .engine
                    .lock()
                    .unwrap()
                    .snapshots
                    .iter()
                    .map(|(n, _)| n.clone())
                    .collect();
                if !names.contains(&self.browser.ab_snapshot) {
                    self.browser.ab_snapshot.clear();
                }
                egui::ComboBox::from_id_salt("ab_snapshot")
                    .selected_text(if self.browser.ab_snapshot.is_empty() {
                        "Choose before"
                    } else {
                        &self.browser.ab_snapshot
                    })
                    .show_ui(ui, |ui| {
                        for n in &names {
                            ui.selectable_value(&mut self.browser.ab_snapshot, n.clone(), n);
                        }
                    });
                ui.add(
                    egui::DragValue::new(&mut self.browser.ab_start_s)
                        .range(0.0..=86400.0)
                        .speed(0.5)
                        .prefix("Start ")
                        .suffix(" s"),
                );
                ui.add(
                    egui::DragValue::new(&mut self.browser.ab_length_s)
                        .range(1.0..=20.0)
                        .speed(0.5)
                        .prefix("Length ")
                        .suffix(" s"),
                );
                if ui
                    .add_enabled(
                        self.browser.ab_job.is_none() && names.contains(&self.browser.ab_snapshot),
                        egui::Button::new("PREPARE BEFORE / NOW").small(),
                    )
                    .clicked()
                {
                    let e = self.engine.lock().unwrap();
                    match super::checkpoint_comparison_engine(&e, &self.browser.ab_snapshot) {
                        Ok(own) => {
                            self.browser.ab_checkpoint_project =
                                Some(own.version(&self.browser.ab_snapshot).unwrap());
                            let checkpoint = self.browser.ab_snapshot.clone();
                            let start_s = self.browser.ab_start_s;
                            let length_s = self.browser.ab_length_s;
                            let (tx, rx) = std::sync::mpsc::channel();
                            let ctx = ui.ctx().clone();
                            self.browser.ab_ready = None;
                            self.browser.ab_result =
                                "Preparing comparison. You can keep editing.".into();
                            self.browser.ab_job = Some(super::MixComparisonJob {
                                revision: e.revision,
                                generation: e.project_load_generation,
                                checkpoint: checkpoint.clone(),
                                start_s,
                                length_s,
                                receiver: rx,
                            });
                            std::thread::spawn(move || {
                                let _ = tx.send(
                                    super::render_checkpoint_comparison(
                                        own,
                                        &checkpoint,
                                        start_s,
                                        length_s,
                                    )
                                    .map_err(|e| format!("{e:#}")),
                                );
                                ctx.request_repaint();
                            });
                        }
                        Err(e) => self.browser.ab_result = format!("Comparison failed: {e:#}"),
                    }
                }
            });
            if ab_before
                != (
                    self.browser.ab_snapshot.clone(),
                    self.browser.ab_start_s,
                    self.browser.ab_length_s,
                )
            {
                // A new preparation in this frame already captures the new selection.
                self.poll_browser_comparison(ui.ctx());
            }
            if self.browser.ab_job.is_some() {
                ui.spinner();
            }
            if let Some(result) = &self.browser.ab_ready {
                if ui.small_button("PLAY BEFORE / NOW").clicked() {
                    let mut e = self.engine.lock().unwrap();
                    if browser_comparison_current(
                        result.revision,
                        result.generation,
                        &result.checkpoint,
                        result.start_s,
                        result.length_s,
                        &self.browser,
                        &e,
                    ) && super::play_checkpoint_comparison(
                        result,
                        &self.browser.ab_snapshot,
                        self.browser.ab_start_s,
                        self.browser.ab_length_s,
                        &mut e,
                    ) {
                        self.browser.ab_result = self
                            .browser
                            .ab_result
                            .replace("Prepared;", "Queued for playback;");
                    }
                }
            }
            if ui.small_button("STOP A/B PREVIEW").clicked() {
                self.browser.ab_job = None;
                self.browser.ab_ready = None;
                self.browser.ab_checkpoint_project = None;
                self.browser.ab_result = "Comparison stopped".into();
                super::stop_comparison_preview(&mut self.engine.lock().unwrap());
            }
            if !self.browser.ab_result.is_empty() {
                ui.label(&self.browser.ab_result);
            }
            ui.label(RichText::new("Capture, change the mix, Prepare, then Play: before, 1s gap, now. Background full-song renders may take time. Project, checkpoint or range changes discard prepared audio. Louder side turns down; shared peak safety may lower both. Silence is not loudness-matched. No automatic winner. Save keeps the checkpoint companion file.").size(10.0).color(tk.text_dim));
            ui.add_space(8.0);
            let translation_range_before = (
                self.browser.translation_start_s,
                self.browser.translation_length_s,
            );
            if self.browser.translation_length_s == 0.0 {
                self.browser.translation_length_s = 8.0;
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("MIX TRANSLATION")
                        .size(11.0)
                        .strong()
                        .color(tk.header_text),
                );
                ui.add(
                    egui::DragValue::new(&mut self.browser.translation_start_s)
                        .range(0.0..=86400.0)
                        .speed(0.5)
                        .prefix("Start ")
                        .suffix(" s"),
                );
                ui.add(
                    egui::DragValue::new(&mut self.browser.translation_length_s)
                        .range(1.0..=20.0)
                        .speed(0.5)
                        .prefix("Length ")
                        .suffix(" s"),
                );
                for (mode, label) in [
                    ("stereo", "HEAR STEREO"),
                    ("mono", "HEAR MONO"),
                    ("phone", "HEAR PHONE"),
                ] {
                    if ui
                        .add_enabled(
                            self.browser.translation_job.is_none(),
                            egui::Button::new(label.replace("HEAR", "PREPARE")).small(),
                        )
                        .clicked()
                    {
                        let (own, revision, generation) = {
                            let e = self.engine.lock().unwrap();
                            let mut own = crate::Engine::new(e.workdir.clone());
                            own.project = e.project.clone();
                            own.bank = e.bank.clone();
                            (own, e.revision, e.project_load_generation)
                        };
                        let start = self.browser.translation_start_s;
                        let length = self.browser.translation_length_s;
                        let (tx, rx) = std::sync::mpsc::channel();
                        let ctx = ui.ctx().clone();
                        self.browser.translation_ready = None;
                        self.browser.translation_result =
                            "Preparing project snapshot. You can keep editing.".into();
                        self.browser.translation_job = Some(TranslationJob {
                            revision,
                            generation,
                            start,
                            length,
                            receiver: rx,
                        });
                        std::thread::spawn(move || {
                            let _ = tx.send(prepare_translation(own, mode, start, length));
                            ctx.request_repaint();
                        });
                    }
                }
                if let Some(ready) = &self.browser.translation_ready {
                    if ui.small_button("PLAY PREPARED").clicked() {
                        let mut e = self.engine.lock().unwrap();
                        if translation_current(
                            ready.revision,
                            ready.generation,
                            ready.start,
                            ready.length,
                            &e,
                            (
                                self.browser.translation_start_s,
                                self.browser.translation_length_s,
                            ),
                        ) {
                            e.transport.pending.push(crate::engine::TransportCmd::Stop);
                            e.transport.pending_audition = Some(Some(ready.audio.clone()));
                            self.browser.translation_result = translation_readout(&ready.value)
                                .replace(
                                    "Prepared; project unchanged.",
                                    "Queued for playback; project unchanged.",
                                );
                        }
                    }
                }
                if ui.small_button("STOP PREVIEW").clicked() {
                    self.browser.translation_job = None;
                    self.browser.translation_ready = None;
                    call = Some(("preview_mix_translation".into(), json!({"stop":true})));
                }
            });
            if translation_range_before
                != (
                    self.browser.translation_start_s,
                    self.browser.translation_length_s,
                )
            {
                self.browser.translation_result.clear();
                self.browser.translation_job = None;
                self.browser.translation_ready = None;
            }
            if !self.browser.translation_result.is_empty() {
                ui.label(&self.browser.translation_result);
            }
            ui.label(RichText::new("Chosen elapsed-time excerpt, not bar-aligned. Background full-song render preserves preceding effects and may take time. Prepare, then Play. Edits/range changes discard old previews. Short ending is reported. Mono fold-down / phone band-limit are monitoring checks, not mix edits or an exact phone model. No loudness boost.").size(10.0).color(tk.text_dim));
            ui.add_space(8.0);
            ui.label(
                RichText::new("KICK / BASS DUCKING")
                    .size(11.0)
                    .strong()
                    .color(tk.header_text),
            );
            if self.browser.duck_beats == 0.0 {
                self.browser.duck_beats = 0.5;
                self.browser.duck_depth = 6.0;
                self.browser.duck_attack = 3.0;
            }
            if !p
                .tracks
                .iter()
                .any(|t| t.name == self.browser.duck_source && t.name != track)
            {
                self.browser.duck_source = p
                    .tracks
                    .iter()
                    .find(|t| t.name != track)
                    .map(|t| t.name.clone())
                    .unwrap_or_default();
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("Source");
                egui::ComboBox::from_id_salt("duck_source").selected_text(&self.browser.duck_source).show_ui(ui,|ui| {
                    for t in p.tracks.iter().filter(|t|t.name!=track) { ui.selectable_value(&mut self.browser.duck_source,t.name.clone(),&t.name); }
                });
                ui.label(format!("Target: {track}"));
                ui.add(egui::DragValue::new(&mut self.browser.duck_depth).range(0.0..=24.0).speed(0.1).suffix(" dB depth"));
                ui.add(egui::DragValue::new(&mut self.browser.duck_beats).range(0.0625..=4.0).speed(0.0625).suffix(" beats release"));
                ui.add(egui::DragValue::new(&mut self.browser.duck_attack).range(0.1..=30.0).speed(0.1).suffix(" ms attack"));
                if ui.add_enabled(!self.browser.duck_source.is_empty(),egui::Button::new("APPLY DUCK").small()).clicked() {
                    call=Some(("duck_to_track".into(),json!({"track":track,"source":self.browser.duck_source,"depth_db":self.browser.duck_depth,"release_beats":self.browser.duck_beats,"attack_ms":self.browser.duck_attack})));
                }
            });
            ui.label(RichText::new("Note-trigger ducking, not audio-level detection. Release uses tempo at Apply; reapply after tempo changes. Same source retunes, does not stack.").size(10.0).color(tk.text_dim));
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
            // silent swaps read as dead buttons; confirm what happened
            let feedback = match tool.as_str() {
                "set_instrument" => Some(format!(
                    "{} now plays {}",
                    args["track"].as_str().unwrap_or("the channel"),
                    args["preset"].as_str().unwrap_or("the new sound")
                )),
                "add_track" | "add_sample_track" => Some(format!(
                    "Added {} as a new channel",
                    args["name"].as_str().unwrap_or("the sound")
                )),
                _ => None,
            };
            match self.engine.lock().unwrap().call_from(&tool, &args, "you") {
                Ok(v) => {
                    if tool == "preview_mix_translation" {
                        self.browser.translation_result = translation_readout(&v);
                    }
                    if tool == "preview_return" && v["stopped"] != true {
                        self.toast = Some((
                            format!(
                                "Return audition: {} s, preview gain {:.1} dB. Project unchanged.",
                                v["seconds"],
                                v["preview_gain_db"].as_f64().unwrap_or(0.0)
                            ),
                            0.0,
                            true,
                        ));
                    }
                    if let Some(msg) = feedback {
                        self.toast = Some((msg, 0.0, true));
                    }
                }
                Err(e) => {
                    if tool == "preview_mix_translation" {
                        self.browser.translation_result = format!("Preview failed: {e:#}");
                    }
                    self.toast = Some((format!("{e:#}"), 0.0, false));
                }
            }
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

pub fn translation_readout(v: &Value) -> String {
    if v["stopped"] == true {
        return "Preview stopped".into();
    }
    let mono=match v["mono_check"]["status"].as_str(){
        Some("source_below_floor")=>"Source below -90 dBFS RMS measurement floor; mono change unavailable.".into(),
        Some("mono_below_floor")=>"Source mono sum below -90 dBFS RMS; exact change unavailable. Listen before changing width.".into(),
        Some("measured")=>format!("Source mono RMS change {:.1} dB (before filters/gain). Level change, not a quality score.",v["mono_check"]["mono_change_db"].as_f64().unwrap_or(0.0)),
        _=>"Source mono measurement unavailable.".into(),
    };
    format!("{}: start {:.2}s | {:.2}s rendered{} | safety gain {:.1} dB. {} Prepared; project unchanged.",
        v["mode"].as_str().unwrap_or("preview"),v["start_s"].as_f64().unwrap_or(0.0),v["seconds"].as_f64().unwrap_or(0.0),
        if v["truncated"]==true{" (shorter than requested)"}else{""},v["safety_gain_db"].as_f64().unwrap_or(0.0),mono)
}

fn new_library_channel_name(project: &Project, base: &str) -> String {
    let available = |name: &str| {
        !name.eq_ignore_ascii_case("master")
            && !project
                .tracks
                .iter()
                .any(|t| t.name.eq_ignore_ascii_case(name))
            && !project
                .buses
                .iter()
                .any(|b| b.name.eq_ignore_ascii_case(name))
    };
    if available(base) {
        return base.to_string();
    }
    for n in 2.. {
        let name = format!("{base} #{n}");
        if available(&name) {
            return name;
        }
    }
    unreachable!()
}

fn library_job_result(
    rx: &std::sync::mpsc::Receiver<Result<Value, String>>,
) -> Option<Result<Value, String>> {
    match rx.try_recv() {
        Ok(result) => Some(result),
        Err(std::sync::mpsc::TryRecvError::Empty) => None,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(
            "background sound job stopped unexpectedly; try again".into(),
        )),
    }
}

fn prepare_library_load(project: &Project, args: &mut Value) {
    if args["new_channel"] == true {
        let base = args["track"].as_str().unwrap_or("Recorded instrument");
        args["track"] = json!(new_library_channel_name(project, base));
    }
}

fn browser_comparison_current(
    revision: u64,
    generation: u64,
    checkpoint: &str,
    start: f32,
    length: f32,
    browser: &BrowserState,
    engine: &crate::Engine,
) -> bool {
    super::comparison_is_current(revision, generation, engine)
        && checkpoint == browser.ab_snapshot
        && (start, length) == (browser.ab_start_s, browser.ab_length_s)
        && browser
            .ab_checkpoint_project
            .as_ref()
            .is_some_and(|captured| {
                engine
                    .snapshots
                    .iter()
                    .any(|(n, s)| n == checkpoint && &s.project == captured)
            })
}
fn browser_comparison_readout(checkpoint: &str, start: f32, length: f32, value: &Value) -> String {
    format!("A: {checkpoint} | B: current at preparation | start {start:.2}s, requested {length:.2}s | A {}s, B {}s. {} Prepared; project unchanged.",value["facts"]["A"]["seconds"],value["facts"]["B"]["seconds"],super::comparison_note(value))
}
impl Studio {
    fn poll_browser_comparison(&mut self, ctx: &egui::Context) {
        let e = self.engine.lock().unwrap();
        let current =
            |r, g, c: &str, s, l| browser_comparison_current(r, g, c, s, l, &self.browser, &e);
        let stale = self.browser.ab_job.as_ref().is_some_and(|j| {
            !current(
                j.revision,
                j.generation,
                &j.checkpoint,
                j.start_s,
                j.length_s,
            )
        }) || self.browser.ab_ready.as_ref().is_some_and(|r| {
            !current(
                r.revision,
                r.generation,
                &r.checkpoint,
                r.start_s,
                r.length_s,
            )
        });
        drop(e);
        if stale {
            self.browser.ab_job = None;
            self.browser.ab_ready = None;
            self.browser.ab_checkpoint_project = None;
            self.browser.ab_result = "Project, checkpoint or range changed. Prepare again.".into();
            return;
        }
        if let Some(job) = self.browser.ab_job.take() {
            match job.receiver.try_recv() {
                Ok(Ok((value, audio))) => {
                    self.browser.ab_result = browser_comparison_readout(
                        &job.checkpoint,
                        job.start_s,
                        job.length_s,
                        &value,
                    );
                    self.browser.ab_ready = Some(super::MixComparison {
                        revision: job.revision,
                        generation: job.generation,
                        checkpoint: job.checkpoint,
                        start_s: job.start_s,
                        length_s: job.length_s,
                        value,
                        audio,
                    });
                }
                Ok(Err(e)) => self.browser.ab_result = format!("Comparison failed: {e}"),
                Err(std::sync::mpsc::TryRecvError::Empty) => self.browser.ab_job = Some(job),
                Err(_) => self.browser.ab_result = "Comparison preparation stopped".into(),
            }
        }
        if self.browser.ab_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

impl Studio {
    fn poll_translation_job(&mut self, ctx: &egui::Context) {
        let range = (
            self.browser.translation_start_s,
            self.browser.translation_length_s,
        );
        let e = self.engine.lock().unwrap();
        let stale = self.browser.translation_job.as_ref().is_some_and(|j| {
            !translation_current(j.revision, j.generation, j.start, j.length, &e, range)
        }) || self.browser.translation_ready.as_ref().is_some_and(|r| {
            !translation_current(r.revision, r.generation, r.start, r.length, &e, range)
        });
        drop(e);
        if stale {
            self.browser.translation_job = None;
            self.browser.translation_ready = None;
            self.browser.translation_result = "Project or range changed. Prepare again.".into();
            return;
        }
        let result =
            self.browser
                .translation_job
                .as_ref()
                .and_then(|j| match j.receiver.try_recv() {
                    Ok(v) => Some(v),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    Err(_) => Some(Err("Preview preparation stopped".into())),
                });
        if let Some(result) = result {
            let j = self.browser.translation_job.take().unwrap();
            match result {
                Ok((value, audio)) => {
                    self.browser.translation_result = translation_readout(&value);
                    self.browser.translation_ready = Some(TranslationReady {
                        revision: j.revision,
                        generation: j.generation,
                        start: j.start,
                        length: j.length,
                        value,
                        audio,
                    });
                }
                Err(e) => self.browser.translation_result = format!("Preview failed: {e}"),
            }
        }
        if self.browser.translation_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

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
                            crate::sound_catalog::fetch_sfz_git(
                                &ent,
                                args.get("max_mb").and_then(|v| v.as_u64()).unwrap_or(400),
                                2,
                            )
                            .map(|_| ())
                            .map_err(|e| format!("{e:#}"))
                        }
                        _ => Ok(()),
                    }
                }
                _ => Ok(()),
            };
            let r = pre.and_then(|_| {
                let mut engine = eng.lock().unwrap();
                let mut args = args;
                prepare_library_load(&engine.project, &mut args);
                engine
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
            if let Some(r) = library_job_result(rx) {
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

    fn recorded_starters(&mut self, ui: &mut egui::Ui, track: &str) {
        self.poll_library_job();
        ui.set_width((ui.clip_rect().width() - 40.0).max(150.0));
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
        ui.label(RichText::new("REAL RECORDED INSTRUMENTS").strong());
        ui.label("Recordings of physical instruments, not oscillator presets. Downloads only when you press Hear or Use. Free CC0 samples from Versilian Studios.");
        ui.label(RichText::new("Use changes the channel sound, not its MIDI. Drum kits need GM drum notes. Existing notes are never remapped automatically.").small());
        let sampler = self
            .engine
            .lock()
            .unwrap()
            .project
            .tracks
            .iter()
            .find(|t| t.name == track)
            .and_then(|t| match &t.instrument {
                crate::instruments::Instrument::Multisample(p) => Some((
                    p.attack * 1000.0,
                    p.release * 1000.0,
                    p.vel_to_gain,
                    p.one_shot,
                    p.round_robin,
                    p.vel_crossfade,
                    p.gm_hat_choke,
                    p.velocity_curve,
                    p.tune_cents,
                )),
                _ => None,
            });
        if let Some((attack, release, velocity, shot, rr, blend, choke, curve, tune)) = sampler {
            self.browser.sync_sample_performance((
                track.to_string(),
                attack,
                release,
                velocity,
                shot,
                rr,
                blend,
                choke,
                curve,
                tune,
            ));
            ui.horizontal(|ui| {
                ui.label("SAMPLE PERFORMANCE");
                if ui.small_button("READ CURRENT").clicked() {
                    self.browser.sample_perf = Some((
                        track.to_string(),
                        attack,
                        release,
                        velocity,
                        shot,
                        rr,
                        blend,
                        choke,
                        curve,
                        tune,
                    ));
                }
            });
            let p = self.browser.sample_perf.as_mut().unwrap();
            egui::Grid::new("sample_performance_fields")
                .num_columns(2)
                .min_col_width(130.0)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Attack");
                    ui.add(
                        egui::DragValue::new(&mut p.1)
                            .range(0.5..=200.0)
                            .speed(0.5)
                            .suffix(" ms"),
                    );
                    ui.end_row();
                    ui.label("Release");
                    ui.add(
                        egui::DragValue::new(&mut p.2)
                            .range(5.0..=3000.0)
                            .speed(5.0)
                            .suffix(" ms"),
                    );
                    ui.end_row();
                    ui.label("Velocity level");
                    ui.add(egui::DragValue::new(&mut p.3).range(0.0..=1.0).speed(0.01));
                    ui.end_row();
                    ui.label("Touch curve");
                    ui.add(egui::DragValue::new(&mut p.8).range(0.25..=4.0).speed(0.01));
                    ui.end_row();
                    ui.label("Fine tune");
                    ui.add(
                        egui::DragValue::new(&mut p.9)
                            .range(-100.0..=100.0)
                            .speed(0.5)
                            .suffix(" cents"),
                    );
                    ui.end_row();
                    ui.checkbox(&mut p.4, "One-shot");
                    ui.checkbox(&mut p.5, "Alternate takes");
                    ui.end_row();
                    ui.checkbox(&mut p.6, "Blend layers");
                    ui.checkbox(&mut p.7, "GM hat choke");
                    ui.end_row();
                });
            if ui.small_button(format!("APPLY TO {track}")).clicked() {
                let r = self.engine.lock().unwrap().call_from("set_sample_performance", &json!({"track":track,"attack_ms":p.1,"release_ms":p.2,"velocity_amount":p.3,"one_shot":p.4,"round_robin":p.5,"velocity_blend":p.6,"gm_hat_choke":p.7,"velocity_curve":p.8,"tune_cents":p.9}), "you");
                self.browser.job_result = match r {
                    Ok(_) => format!("Updated sample performance on {track}"),
                    Err(e) => format!("No change: {e:#}"),
                };
            }
            ui.label(RichText::new("Fields refresh on channel/current-setting changes; un-applied drafts are discarded then. One-shot plays whole recording, ignoring note gate/release. Off: note length plus release. Fine tune changes pitch and recorded duration together, not time-stretch; mapping and MIDI unchanged. Touch curve: 1 unchanged, below 1 stronger, above 1 softer; affects existing recorded layers and level, not MIDI. Velocity level controls how much touch changes level. Alternate takes and blending use only existing recorded takes/layers; no new layers. GM hat choke:46 closes on later42/44 on this channel only. No gain matching.").small());
        }
        if sampler.is_none() {
            self.browser.sample_perf = None;
            self.browser.sample_perf_source = None;
        }
        let melodic_sampler=self.engine.lock().unwrap().project.tracks.iter().find(|t|t.name==track).is_some_and(|t|matches!(&t.instrument,crate::instruments::Instrument::Multisample(p) if !p.zones.is_empty()&&!p.zones.iter().all(|z|z.lo_note==z.hi_note&&z.root==z.lo_note&&(35..=81).contains(&z.root))));
        if melodic_sampler {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .small_button(format!("HEAR CURRENT PHRASE / {track}"))
                    .clicked()
                {
                    let r = self.engine.lock().unwrap().call_from(
                        "preview_instrument",
                        &json!({"track":track,"play":true}),
                        "you",
                    );
                    self.browser.job_result = match r {
                        Ok(v) => format!(
                            "Dry current phrase on {track}, root {}, listening trim -6 dB: {}",
                            v["root"],
                            v["path"].as_str().unwrap_or("")
                        ),
                        Err(e) => format!("No preview: {e:#}"),
                    };
                }
                if ui.small_button("STOP PHRASE").clicked() {
                    let _ = self.engine.lock().unwrap().call_from(
                        "play_audio",
                        &json!({"stop":true}),
                        "you",
                    );
                }
            });
            ui.label(RichText::new("5s fixed major phrase, not the song or key detection. Uses applied instrument settings; dry before effects/faders. Listening trim -6dB; WAV untrimmed. Long tails may be cut at 5s; song audio can play underneath.").small());
        }
        let kit_trims = self
            .engine
            .lock()
            .unwrap()
            .project
            .tracks
            .iter()
            .find(|t| t.name == track)
            .and_then(|t| match &t.instrument {
                crate::instruments::Instrument::Multisample(p)
                    if !p.zones.is_empty()
                        && p.zones.iter().all(|z| {
                            z.lo_note == z.hi_note
                                && z.root == z.lo_note
                                && (35..=81).contains(&z.root)
                        }) =>
                {
                    Some((
                        p.key_trim_db.clone(),
                        p.key_tune_cents.clone(),
                        p.key_length_ms.clone(),
                        p.key_pan.clone(),
                        p.key_attack_ms.clone(),
                        p.attack * 1000.,
                        p.reverse_keys.clone(),
                        p.key_decay_ms.clone(),
                        p.key_lowpass_hz.clone(),
                        p.key_highpass_hz.clone(),
                        p.key_velocity_curve.clone(),
                        p.velocity_curve,
                    ))
                }
                _ => None,
            });
        if let Some((
            trims,
            tuning,
            lengths,
            pans,
            attacks,
            kit_attack,
            reverse_keys,
            decays,
            lowpasses,
            highpasses,
            velocity_curves,
            kit_velocity_curve,
        )) = kit_trims
        {
            ui.horizontal_wrapped(|ui| {
                ui.label("KIT PIECE LEVEL");
                let labels = [
                    "Kick36",
                    "Snare38",
                    "Clap39",
                    "Closedhat42",
                    "Pedalhat44",
                    "Openhat46",
                    "Lowtom45",
                    "Hightom48",
                    "Crash49",
                ];
                let keys = [36u8, 38, 39, 42, 44, 46, 45, 48, 49];
                egui::ComboBox::from_id_salt("kit_level_key")
                    .selected_text(labels[self.browser.kit_level_key.min(8)])
                    .show_ui(ui, |ui| {
                        for (i, label) in labels.iter().enumerate() {
                            ui.selectable_value(&mut self.browser.kit_level_key, i, *label);
                        }
                    });
                let key = keys[self.browser.kit_level_key.min(8)];
                let values = [
                    trims.get(&key).copied().unwrap_or(0.0),
                    tuning.get(&key).copied().unwrap_or(0.0),
                    lengths.get(&key).copied().unwrap_or(0.0),
                ];
                let source = (track.to_string(), key, values);
                if self.browser.kit_edit_source.as_ref() != Some(&source) {
                    self.browser.kit_level_db = values[0];
                    self.browser.kit_tune_cents = values[1];
                    self.browser.kit_length_ms = values[2];
                    self.browser.kit_edit_source = Some(source);
                }
                let current = values[0];
                ui.label(format!("Current {current:.1} dB"));
                if ui.small_button("READ").clicked() {
                    self.browser.kit_level_db = current;
                }
                ui.add(
                    egui::DragValue::new(&mut self.browser.kit_level_db)
                        .range(-24.0..=6.0)
                        .speed(0.1)
                        .suffix(" dB trim"),
                );
                if ui
                    .small_button(format!(
                        "SET {track} / {}",
                        labels[self.browser.kit_level_key.min(8)]
                    ))
                    .clicked()
                {
                    let r = self.engine.lock().unwrap().call_from(
                        "set_kit_piece_level",
                        &json!({"track":track,"key":key,"gain_db":self.browser.kit_level_db}),
                        "you",
                    );
                    self.browser.job_result = match r {
                        Ok(_) => format!(
                            "Set key{key} trim on {track} to {}dB",
                            self.browser.kit_level_db
                        ),
                        Err(e) => format!("No change: {e:#}"),
                    };
                }
            });
            ui.label(RichText::new("Absolute trim, not stacked. 0dB resets this key. Preserves original recorded-layer gains and MIDI. Boost can clip; no limiter or automatic matching.").small());
            ui.horizontal_wrapped(|ui| {
                ui.label("KIT PIECE TUNING");
                let keys = [36u8, 38, 39, 42, 44, 46, 45, 48, 49];
                let key = keys[self.browser.kit_level_key.min(8)];
                let current = tuning.get(&key).copied().unwrap_or(0.0);
                ui.label(format!("Selected key{key}: {current:.0} cents"));
                if ui.small_button("READ TUNING").clicked() {
                    self.browser.kit_tune_cents = current;
                }
                ui.add(
                    egui::DragValue::new(&mut self.browser.kit_tune_cents)
                        .range(-1200.0..=1200.0)
                        .speed(5.0)
                        .suffix(" cents"),
                );
                if ui
                    .small_button(format!("TUNE {track} / key{key}"))
                    .clicked()
                {
                    let r = self.engine.lock().unwrap().call_from(
                        "set_kit_piece_tuning",
                        &json!({"track":track,"key":key,"cents":self.browser.kit_tune_cents}),
                        "you",
                    );
                    self.browser.job_result = match r {
                        Ok(_) => format!(
                            "Tuned key{key} on {track} to {} cents",
                            self.browser.kit_tune_cents
                        ),
                        Err(e) => format!("No change: {e:#}"),
                    };
                }
            });
            ui.label(RichText::new("Uses the piece selected above. Absolute cents; 0 resets. Changes pitch and duration together, not time stretch or automatic key detection. MIDI and recorded mapping stay unchanged.").small());
            ui.horizontal_wrapped(|ui| {
                ui.label("KIT PIECE LENGTH");
                let keys = [36u8, 38, 39, 42, 44, 46, 45, 48, 49];
                let key = keys[self.browser.kit_level_key.min(8)];
                let current = lengths.get(&key).copied().unwrap_or(0.0);
                ui.label(format!("Selected key{key}: {current:.0} ms (0=off)"));
                if ui.small_button("READ LENGTH").clicked() {
                    self.browser.kit_length_ms = current;
                }
                ui.add(
                    egui::DragValue::new(&mut self.browser.kit_length_ms)
                        .range(0.0..=5000.0)
                        .speed(5.0)
                        .suffix(" ms cap"),
                );
                if ui
                    .small_button(format!("LENGTH {track} / key{key}"))
                    .clicked()
                {
                    let r = self.engine.lock().unwrap().call_from(
                        "set_kit_piece_length",
                        &json!({"track":track,"key":key,"length_ms":self.browser.kit_length_ms}),
                        "you",
                    );
                    self.browser.job_result = match r {
                        Ok(_) => format!(
                            "Set key{key} maximum playback on {track} to {}ms (0=off)",
                            self.browser.kit_length_ms
                        ),
                        Err(e) => format!("No change: {e:#}"),
                    };
                }
            });
            ui.label(RichText::new("Selected piece only. 0=off; otherwise 10..5000ms. Caps playback after tuning/gate, with 5ms end fade. Never extends recording; MIDI unchanged. Not time stretch.").small());
            ui.horizontal_wrapped(|ui| {
                let key = [36u8, 38, 39, 42, 44, 46, 45, 48, 49][self.browser.kit_level_key.min(8)];
                let current = pans.get(&key).copied().unwrap_or(0.);
                ui.label(format!("KIT PIECE BALANCE key{key}: {current:+.2}"));
                if ui.small_button("READ BALANCE").clicked() {
                    self.browser.kit_pan = current;
                }
                ui.add(
                    egui::DragValue::new(&mut self.browser.kit_pan)
                        .range(-1.0..=1.0)
                        .speed(0.01),
                );
                if ui
                    .small_button(format!("BALANCE {track} / key{key}"))
                    .clicked()
                {
                    let r = self.engine.lock().unwrap().call_from(
                        "set_kit_piece_pan",
                        &json!({"track":track,"key":key,"pan":self.browser.kit_pan}),
                        "you",
                    );
                    self.browser.job_result = match r {
                        Ok(_) => "Updated selected piece balance; Undo restores.".into(),
                        Err(e) => format!("No balance change: {e:#}"),
                    };
                }
            });
            ui.label(RichText::new("-1=left, +1=right, 0=unchanged/reset. Attenuates opposite side, no boost. All patterns, chosen key only, before channel FX/pan. Stereo FX can spread it again. Mono instrument audition does not reflect mix placement. Undo restores.").small());
            ui.horizontal_wrapped(|ui| {
                let key = [36u8, 38, 39, 42, 44, 46, 45, 48, 49][self.browser.kit_level_key.min(8)];
                let current = attacks.get(&key).copied().unwrap_or(0.);
                ui.label(format!(
                    "KIT PIECE ATTACK key{key}: {current:.1} ms (0=kit {kit_attack:.1} ms)"
                ));
                if ui.small_button("READ ATTACK").clicked() {
                    self.browser.kit_attack_ms = current;
                }
                ui.add(
                    egui::DragValue::new(&mut self.browser.kit_attack_ms)
                        .range(0.0..=200.0)
                        .speed(0.5)
                        .suffix(" ms"),
                );
                if ui
                    .small_button(format!("ATTACK {track} / key{key}"))
                    .clicked()
                {
                    let r = self.engine.lock().unwrap().call_from(
                        "set_kit_piece_attack",
                        &json!({"track":track,"key":key,"attack_ms":self.browser.kit_attack_ms}),
                        "you",
                    );
                    self.browser.job_result = match r {
                        Ok(_) => "Updated piece attack; Undo restores.".into(),
                        Err(e) => format!("No attack change: {e:#}"),
                    };
                }
            });
            ui.label(RichText::new("0=use kit attack; otherwise0.5..200ms linear fade-in. Softens chosen piece only, all patterns. Does not move note start or extend the recording. Other keys/kit-wide envelope intact. No gain match. Undo restores.").small());
            ui.horizontal_wrapped(|ui| {
                let key = [36u8, 38, 39, 42, 44, 46, 45, 48, 49][self.browser.kit_level_key.min(8)];
                let on = reverse_keys.contains(&key);
                ui.label(format!(
                    "KIT PIECE REVERSE key{key}: {}",
                    if on { "ON" } else { "OFF" }
                ));
                if ui
                    .small_button(if on {
                        "PLAY PIECE FORWARD"
                    } else {
                        "REVERSE SELECTED PIECE"
                    })
                    .clicked()
                {
                    let r = self.engine.lock().unwrap().call_from(
                        "set_kit_piece_reverse",
                        &json!({"track":track,"key":key,"enabled":!on}),
                        "you",
                    );
                    self.browser.job_result = match r {
                        Ok(_) => "Changed piece direction; Undo restores.".into(),
                        Err(e) => format!("No direction change: {e:#}"),
                    };
                }
            });
            ui.label(RichText::new("Reverses each full source recording before tuning/envelope/length; other pieces intact. Note start stays fixed, reverse transient arrives later: no automatic lead-in. Length caps/gated mode can cut off swell; one-shot plays full source. No gain match. Undo restores.").small());
            ui.horizontal_wrapped(|ui| {
                let key = [36u8, 38, 39, 42, 44, 46, 45, 48, 49][self.browser.kit_level_key.min(8)];
                let current = decays.get(&key).copied().unwrap_or(0.);
                ui.label(format!(
                    "KIT PIECE DECAY key{key}: {current:.1} ms half-life"
                ));
                if ui.small_button("READ DECAY").clicked() {
                    self.browser.kit_decay_ms = current;
                }
                ui.add(
                    egui::DragValue::new(&mut self.browser.kit_decay_ms)
                        .range(0.0..=2000.0)
                        .speed(5.0)
                        .suffix(" ms"),
                );
                if ui
                    .small_button(format!("DECAY {track} / key{key}"))
                    .clicked()
                {
                    let r = self.engine.lock().unwrap().call_from(
                        "set_kit_piece_decay",
                        &json!({"track":track,"key":key,"half_life_ms":self.browser.kit_decay_ms}),
                        "you",
                    );
                    self.browser.job_result = match r {
                        Ok(_) => "Changed piece decay; Undo restores.".into(),
                        Err(e) => format!("No decay change: {e:#}"),
                    };
                }
            });
            ui.label(RichText::new("0=off; otherwise10..2000ms half-life: amplitude halves each interval from note start, also during attack. Quiets the selected piece tail without cutting length or moving notes. Other pieces unchanged; no gain match. Reverse swell may become quieter. Undo restores.").small());
            ui.horizontal_wrapped(|ui| {
                let key = [36u8, 38, 39, 42, 44, 46, 45, 48, 49][self.browser.kit_level_key.min(8)];
                let current = lowpasses.get(&key).copied().unwrap_or(0.);
                ui.label(format!(
                    "KIT PIECE LOW-PASS key{key}: {current:.0} Hz (0=bypass)"
                ));
                if ui.small_button("READ LOW-PASS").clicked() {
                    self.browser.kit_lowpass_hz = current;
                }
                ui.add(
                    egui::DragValue::new(&mut self.browser.kit_lowpass_hz)
                        .range(0.0..=18000.0)
                        .speed(100.0)
                        .suffix(" Hz"),
                );
                if ui
                    .small_button(format!("LOW-PASS {track} / key{key}"))
                    .clicked()
                {
                    let r = self.engine.lock().unwrap().call_from(
                        "set_kit_piece_lowpass",
                        &json!({"track":track,"key":key,"cutoff_hz":self.browser.kit_lowpass_hz}),
                        "you",
                    );
                    self.browser.job_result = match r {
                        Ok(_) => "Changed piece tone; Undo restores.".into(),
                        Err(e) => format!("No tone change: {e:#}"),
                    };
                }
            });
            ui.label(RichText::new("0=bypass; otherwise100..18000Hz per-stage cutoff, higher=brighter. Two cascaded one-pole stages, before envelope/channel FX. No resonance, no gain match; combined response is not a calibrated -3dB cutoff. Selected piece only, all patterns. Length unchanged, filter tail not extended. Undo restores.").small());
            ui.horizontal_wrapped(|ui| {
                let key = [36u8, 38, 39, 42, 44, 46, 45, 48, 49][self.browser.kit_level_key.min(8)];
                let current = highpasses.get(&key).copied().unwrap_or(0.);
                ui.label(format!(
                    "KIT PIECE HIGH-PASS key{key}: {current:.0} Hz (0=bypass)"
                ));
                if ui.small_button("READ HIGH-PASS").clicked() {
                    self.browser.kit_highpass_hz = current;
                }
                ui.add(
                    egui::DragValue::new(&mut self.browser.kit_highpass_hz)
                        .range(0.0..=4000.0)
                        .speed(20.0)
                        .suffix(" Hz"),
                );
                if ui
                    .small_button(format!("HIGH-PASS {track} / key{key}"))
                    .clicked()
                {
                    let r = self.engine.lock().unwrap().call_from(
                        "set_kit_piece_highpass",
                        &json!({"track":track,"key":key,"cutoff_hz":self.browser.kit_highpass_hz}),
                        "you",
                    );
                    self.browser.job_result = match r {
                        Ok(_) => "Changed piece tone; Undo restores.".into(),
                        Err(e) => format!("No tone change: {e:#}"),
                    };
                }
            });
            ui.label(RichText::new("0=bypass; otherwise20..4000Hz per-stage pole frequency, higher=less body. Two DC-blocking stages after low-pass, before envelope/channel FX. No resonance or gain match; not a calibrated combined -3dB cutoff. Chosen piece only, all patterns; length unchanged. Peaks can change. Undo restores.").small());
            ui.horizontal_wrapped(|ui| {
                let key = [36u8, 38, 39, 42, 44, 46, 45, 48, 49][self.browser.kit_level_key.min(8)];
                let current = velocity_curves.get(&key).copied().unwrap_or(0.);
                ui.label(format!(
                    "KIT PIECE RESPONSE key{key}: {current:.2} (0=kit {kit_velocity_curve:.2})"
                ));
                if ui.small_button("READ RESPONSE").clicked() {
                    self.browser.kit_velocity_curve = current;
                }
                ui.add(
                    egui::DragValue::new(&mut self.browser.kit_velocity_curve)
                        .range(0.0..=4.0)
                        .speed(0.05),
                );
                if ui
                    .small_button(format!("RESPONSE {track} / key{key}"))
                    .clicked()
                {
                    let r = self.engine.lock().unwrap().call_from(
                        "set_kit_piece_velocity_curve",
                        &json!({"track":track,"key":key,"curve":self.browser.kit_velocity_curve}),
                        "you",
                    );
                    self.browser.job_result = match r {
                        Ok(_) => "Changed piece response; Undo restores.".into(),
                        Err(e) => format!("No response change: {e:#}"),
                    };
                }
            });
            ui.label(RichText::new("0=use kit curve; otherwise0.25..4. Below1brings quiet hits forward,above1softens them;1linear overrides kit curve,not reset. Replaces kit response for recorded layers and level,never multiplies curves. MIDI velocities/other pieces intact,all patterns. No new layers or gain match. Undo restores.").small());
            ui.label(RichText::new("ALTERNATING PIECE DYNAMICS").strong());
            let ak = [36u8, 38, 39, 42, 44, 46, 45, 48, 49][self.browser.kit_level_key.min(8)];
            let ac = self.browser.kit_accents.get_or_insert((0.85, 0.45, 2, 0));
            ui.horizontal_wrapped(|ui|{
                ui.add(egui::DragValue::new(&mut ac.0).range(0.02..=1.0).speed(0.01).prefix("Accent "));
                ui.add(egui::DragValue::new(&mut ac.1).range(0.02..=1.0).speed(0.01).prefix("Soft "));
                egui::ComboBox::from_id_salt("piece_accent_every").selected_text(format!("Every {} hits",ac.2)).show_ui(ui,|ui|{ui.selectable_value(&mut ac.2,2,"Every 2 hits");ui.selectable_value(&mut ac.2,4,"Every 4 hits");});
                ac.3=ac.3.min(ac.2-1);ui.add(egui::DragValue::new(&mut ac.3).range(0..=ac.2-1).prefix("Phase "));
                if ui.small_button(format!("ACCENT {track} / key{ak}")).clicked(){let r=self.engine.lock().unwrap().call_from("set_kit_piece_accents",&json!({"track":track,"key":ak,"accent":ac.0,"soft":ac.1,"every":ac.2,"phase":ac.3}),"you");self.browser.job_result=match r{Ok(v)=>format!("Replaced velocities on {} key{ak} notes. Undo restores.",v["notes"]),Err(e)=>format!("No accents: {e:#}")};}
            });
            ui.label(RichText::new("Selected piece above, ALL patterns. Replaces prior velocities, not a multiplier. Accent every 2/4 distinct grid starts; phase 0=first, rest soft. Simultaneous hits stay together; count restarts per pattern. Rhythm, other keys and IDs intact. Existing recorded layers only; Undo restores dynamics.").small());
            ui.label(RichText::new("VELOCITY BUILD / FADE / SELECTED PIECE").strong());
            let rk = [36u8, 38, 39, 42, 44, 46, 45, 48, 49][self.browser.kit_level_key.min(8)];
            let rnames: Vec<String> = self
                .engine
                .lock()
                .unwrap()
                .project
                .patterns
                .iter()
                .map(|p| p.name.clone())
                .collect();
            let ramp = self.browser.kit_ramp.get_or_insert((
                rnames.first().cloned().unwrap_or_default(),
                0.25,
                0.95,
            ));
            if !rnames.contains(&ramp.0) {
                ramp.0 = rnames.first().cloned().unwrap_or_default();
            }
            let max_step = self
                .engine
                .lock()
                .unwrap()
                .project
                .patterns
                .iter()
                .find(|p| p.name == ramp.0)
                .map(|p| p.steps() as f32)
                .unwrap_or(16.);
            let rr = self
                .browser
                .kit_ramp_range
                .get_or_insert((false, 0., max_step, 1.));
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut rr.0, "Only step range");
                ui.add_enabled(
                    rr.0,
                    egui::DragValue::new(&mut rr.1)
                        .range(0.0..=max_step)
                        .speed(1.)
                        .prefix("From "),
                );
                ui.add_enabled(
                    rr.0,
                    egui::DragValue::new(&mut rr.2)
                        .range(0.0..=max_step)
                        .speed(1.)
                        .prefix("To (excluded) "),
                );
                ui.add(
                    egui::DragValue::new(&mut rr.3)
                        .range(0.25..=4.)
                        .speed(0.05)
                        .prefix("Curve "),
                );
            });
            ui.horizontal_wrapped(|ui|{
                egui::ComboBox::from_id_salt("kit_ramp_pattern").selected_text(&ramp.0).show_ui(ui,|ui|{for n in &rnames{ui.selectable_value(&mut ramp.0,n.clone(),n);}});
                ui.add(egui::DragValue::new(&mut ramp.1).range(0.02..=1.0).speed(0.01).prefix("Start "));ui.add(egui::DragValue::new(&mut ramp.2).range(0.02..=1.0).speed(0.01).prefix("End "));
                if ui.small_button("REVERSE BUILD/FADE").clicked(){std::mem::swap(&mut ramp.1,&mut ramp.2);}
                if ui.small_button(format!("RAMP {track} / key{rk}")).clicked(){let mut args=json!({"track":track,"key":rk,"pattern":ramp.0,"start_velocity":ramp.1,"end_velocity":ramp.2,"curve":rr.3});if rr.0{args["from_step"]=json!(rr.1);args["to_step"]=json!(rr.2);}let r=self.engine.lock().unwrap().call_from("set_kit_piece_ramp",&args,"you");self.browser.job_result=match r{Ok(v)=>format!("Replaced {} key{rk} velocities in {}. Undo restores.",v["notes"],ramp.0),Err(e)=>format!("No ramp: {e:#}")};}
            });
            ui.label(RichText::new("Selected piece, ONE pattern, including grace hits. Range uses grid starts [From,To), zero-based sixteenth steps. Unchecked=whole pattern. Curve1=linear; >1=late change, <1=early change. Replaces velocities by distinct start order, not time distance; simultaneous hits share level. Needs 2 distinct starts. End lower than Start fades. Rhythm/other pieces untouched; recorded layers can change. No level match. Undo restores.").small());
            ui.label(RichText::new("ADD ROLL / EMPTY SELECTED-PIECE RANGE").strong());
            let roll = self.browser.kit_roll.get_or_insert((
                rnames.first().cloned().unwrap_or_default(),
                48.,
                64.,
                1.,
                0.25,
                0.95,
                1.,
            ));
            if !rnames.contains(&roll.0) {
                roll.0 = rnames.first().cloned().unwrap_or_default();
            }
            ui.horizontal_wrapped(|ui|{
                egui::ComboBox::from_id_salt("kit_roll_pattern").selected_text(&roll.0).show_ui(ui,|ui|{for n in &rnames{ui.selectable_value(&mut roll.0,n.clone(),n);}});
                ui.add(egui::DragValue::new(&mut roll.1).range(0.0..=1024.).speed(1.).prefix("From "));ui.add(egui::DragValue::new(&mut roll.2).range(0.0..=1024.).speed(1.).prefix("To (excluded) "));
                let divs=[("sixteenth","Sixteenth"),("eighth","Eighth"),("thirty_second","32nd"),("eighth_triplet","Eighth triplet"),("sixteenth_triplet","Sixteenth triplet")];
                self.browser.kit_roll_division=self.browser.kit_roll_division.min(4);
                egui::ComboBox::from_id_salt("roll_spacing").selected_text(divs[self.browser.kit_roll_division].1).show_ui(ui,|ui|{for(i,(_,label))in divs.iter().enumerate(){ui.selectable_value(&mut self.browser.kit_roll_division,i,*label);}});
                ui.add(egui::DragValue::new(&mut roll.4).range(0.02..=1.).speed(0.01).prefix("Start "));ui.add(egui::DragValue::new(&mut roll.5).range(0.02..=1.).speed(0.01).prefix("End "));ui.add(egui::DragValue::new(&mut roll.6).range(0.25..=4.).speed(0.05).prefix("Curve "));
                if ui.small_button(format!("ADD ROLL {track} / key{rk}")).clicked(){let r=self.engine.lock().unwrap().call_from("add_kit_piece_roll",&json!({"track":track,"pattern":roll.0,"key":rk,"from_step":roll.1,"to_step":roll.2,"subdivision":divs[self.browser.kit_roll_division].0,"start_velocity":roll.4,"end_velocity":roll.5,"curve":roll.6}),"you");self.browser.job_result=match r{Ok(v)=>format!("Added {} roll notes. Undo restores.",v["added"]),Err(e)=>format!("No roll: {e:#}")};}
            });
            ui.label(RichText::new("NEW notes, unlike ramp above. Chosen piece/range must be empty; rejects collisions, never replaces notes. Zero-based sixteenth steps [From,To), within pattern; exact2..128hits. Triplets use 3 equal hits per half/whole beat, not swing. Requires global swing0. Existing kit layers, no new recordings. Short gate; one-shot can overlap hits. Curve1=linear, >1=late change. Undo removes roll.").small());
            ui.label(RichText::new("GRACE-HIT FLAMS / SELECTED PIECE").strong());
            let fk = [36u8, 38, 39, 42, 44, 46, 45, 48, 49][self.browser.kit_level_key.min(8)];
            let pattern_names: Vec<String> = self
                .engine
                .lock()
                .unwrap()
                .project
                .patterns
                .iter()
                .map(|p| p.name.clone())
                .collect();
            let flam = self.browser.kit_flams.get_or_insert((
                pattern_names.first().cloned().unwrap_or_default(),
                18.0,
                0.45,
            ));
            if !pattern_names.contains(&flam.0) {
                flam.0 = pattern_names.first().cloned().unwrap_or_default();
            }
            ui.horizontal_wrapped(|ui|{
                egui::ComboBox::from_id_salt("kit_flam_pattern").selected_text(&flam.0).show_ui(ui,|ui|{for n in &pattern_names{ui.selectable_value(&mut flam.0,n.clone(),n);}});
                ui.add(egui::DragValue::new(&mut flam.1).range(5.0..=40.0).speed(1.0).suffix(" ms before"));ui.add(egui::DragValue::new(&mut flam.2).range(0.1..=0.8).speed(0.01).prefix("Strength "));
                if ui.small_button(format!("FLAM {track} / key{fk}")).clicked(){let r=self.engine.lock().unwrap().call_from("add_kit_piece_flams",&json!({"track":track,"key":fk,"pattern":flam.0,"spacing_ms":flam.1,"strength":flam.2}),"you");self.browser.job_result=match r{Ok(v)=>format!("Added {} grace hits, skipped {}. Main hits intact; Undo restores.",v["added"],v["skipped"]),Err(e)=>format!("No flams: {e:#}")};}
            });
            ui.label(RichText::new("Selected piece, ONE pattern. Adds quieter grace hits; main notes intact. Same setup skips nearby hits, not repeated layering. Changing spacing may add a second layer: Undo first. Before-start/colliding hits skipped. Requires global swing0. Apply BPM spacing, not live ms; no new recordings. Undo restores.").small());
            ui.label(RichText::new("SIXTEENTH SWING / SELECTED PIECE").strong());
            let sk = [36u8, 38, 39, 42, 44, 46, 45, 48, 49][self.browser.kit_level_key.min(8)];
            let sw = self.browser.kit_swing.get_or_insert(0.35);
            let global_swing = self.engine.lock().unwrap().project.swing;
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::DragValue::new(sw)
                        .range(0.0..=1.0)
                        .speed(0.01)
                        .prefix("Swing "),
                );
                if ui
                    .small_button(format!("SWING {track} / key{sk}"))
                    .clicked()
                {
                    let r = self.engine.lock().unwrap().call_from(
                        "set_kit_piece_swing",
                        &json!({"track":track,"key":sk,"amount":sw}),
                        "you",
                    );
                    self.browser.job_result = match r {
                        Ok(v) => format!(
                            "Set swing on {} key{sk} grid notes. Undo restores.",
                            v["notes"]
                        ),
                        Err(e) => format!("No swing: {e:#}"),
                    };
                }
                ui.label(format!("Global swing adds: {global_swing:.2}"));
            });
            ui.label(RichText::new("ALL patterns, selected piece above. Odd integer sixteenths late by Swing x half-step; even grid hits reset to zero offset. Fractional starts untouched. Replaces prior grid timing, not stacked. Global swing adds: use 0 globally for piece-only shuffle. 0 resets selected grid offsets. Velocity/IDs intact; Undo restores.").small());
            ui.label(RichText::new("DRUM POCKET / SELECTED PIECE").strong());
            let pocket_key =
                [36u8, 38, 39, 42, 44, 46, 45, 48, 49][self.browser.kit_level_key.min(8)];
            let (pocket_bpm, pocket_offsets) = {
                let e = self.engine.lock().unwrap();
                (
                    e.project.bpm,
                    e.project
                        .patterns
                        .iter()
                        .flat_map(|pat| pat.notes(track).iter())
                        .filter(|n| n.pitch == pocket_key)
                        .map(|n| n.offset)
                        .collect::<Vec<f32>>(),
                )
            };
            let same = pocket_offsets.first().copied().filter(|first| {
                pocket_offsets
                    .iter()
                    .all(|v| v.to_bits() == first.to_bits())
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(match same {
                    Some(v) => format!(
                        "key{pocket_key}: {:.1} ms at current BPM ({} notes)",
                        v * 15000.0 / pocket_bpm,
                        pocket_offsets.len()
                    ),
                    None if pocket_offsets.is_empty() => "No notes for this piece".into(),
                    None => format!("Mixed timing on {} notes", pocket_offsets.len()),
                });
                if ui
                    .add_enabled(same.is_some(), egui::Button::new("READ POCKET").small())
                    .clicked()
                {
                    self.browser.kit_pocket_ms = same.unwrap() * 15000.0 / pocket_bpm;
                }
            });
            ui.horizontal_wrapped(|ui|{
                ui.label("Push (-) / lay back (+)");ui.add(egui::DragValue::new(&mut self.browser.kit_pocket_ms).range(-50.0..=50.0).speed(1.0).suffix(" ms"));
                if ui.add_enabled(!pocket_offsets.is_empty(),egui::Button::new(format!("POCKET {track} / key{pocket_key}")).small()).clicked(){let r=self.engine.lock().unwrap().call_from("set_kit_piece_pocket",&json!({"track":track,"key":pocket_key,"offset_ms":self.browser.kit_pocket_ms}),"you");self.browser.job_result=match r{Ok(v)=>format!("Set {} key{pocket_key} notes to {} ms. Undo restores.",v["notes"],self.browser.kit_pocket_ms),Err(e)=>format!("No pocket change: {e:#}")};}
            });
            ui.label(RichText::new("Replaces this piece's offsets across ALL patterns, not additive. Zero resets. Grid, velocity and other pieces stay intact. Converts at Apply BPM; tempo changes scale milliseconds. Half-step maximum; early hits may clamp at song start. Undo restores prior timing.").small());
            ui.horizontal_wrapped(|ui| {
                let keys = [36u8, 38, 39, 42, 44, 46, 45, 48, 49];
                let key = keys[self.browser.kit_level_key.min(8)];
                if ui
                    .small_button(format!("EXTRACT key{key} TO OWN CHANNEL"))
                    .clicked()
                {
                    let r = self.engine.lock().unwrap().call_from(
                        "extract_kit_piece",
                        &json!({"track":track,"key":key}),
                        "you",
                    );
                    self.browser.job_result = match r {
                        Ok(v) => format!(
                            "Moved {} notes to {}. Undo restores the kit.",
                            v["notes_moved"],
                            v["track"].as_str().unwrap_or("channel")
                        ),
                        Err(e) => format!("No extraction: {e:#}"),
                    };
                }
            });
            ui.label(RichText::new("Moves this key's notes across all patterns, not a copy. New channel starts with this sound, FX, fader and routing. Shared compression/saturation can sound different after splitting. Source automation blocks extraction; enabled hat choke blocks hat splits. Audio clips stay here. Undo restores.").small());
            ui.horizontal_wrapped(|ui| {
                let keys = [36u8, 38, 39, 42, 44, 46, 45, 48, 49];
                let key = keys[self.browser.kit_level_key.min(8)];
                ui.add(egui::DragValue::new(self.browser.kit_preview_velocity.get_or_insert(0.8)).range(0.01..=1.0).speed(0.01).prefix("Preview velocity "));
                ui.add(egui::DragValue::new(&mut self.browser.kit_preview_take).range(0..=127).speed(1.0).prefix("Take counter "));
                if ui
                    .small_button(format!("HEAR CURRENT {track} / key{key}"))
                    .clicked()
                {
                    let r = self.engine.lock().unwrap().call_from(
                        "preview_kit_piece",
                        &json!({"track":track,"key":key,"play":true,"velocity":self.browser.kit_preview_velocity.unwrap_or(0.8),"take":self.browser.kit_preview_take}),
                        "you",
                    );
                    self.browser.job_result = match r {
                        Ok(v) => format!(
                            "Dry key{key}, take counter{}, velocity{}, listeningtrim-6dB: {}",
                            self.browser.kit_preview_take,self.browser.kit_preview_velocity.unwrap_or(0.8),
                            v["path"].as_str().unwrap_or("")
                        ),
                        Err(e) => format!("No preview: {e:#}"),
                    };
                }
                if ui.small_button("STOP PREVIEW").clicked() {
                    let _ = self.engine.lock().unwrap().call_from(
                        "play_audio",
                        &json!({"stop":true}),
                        "you",
                    );
                }
            });
            ui.label(RichText::new("One dry hit at preview velocity/take counter (0=first, wraps available takes; ignored if Alternate takes off). Preview controls never edit MIDI. Uses applied piece settings; before track/master effects and faders. Preview listening trim -6dB; WAV untrimmed. Song audio can still play.").small());
        }
        let pattern_names: Vec<String> = self
            .engine
            .lock()
            .unwrap()
            .project
            .patterns
            .iter()
            .map(|p| p.name.clone())
            .collect();
        if !pattern_names.contains(&self.browser.drum_map_pattern) {
            self.browser.drum_map_pattern = pattern_names.first().cloned().unwrap_or_default();
        }
        ui.horizontal_wrapped(|ui|{
            ui.label("ONE-DRUM NOTE MAPPING");let labels=["Kick36","Snare38","Clap39","Closedhat42","Openhat46","Lowtom45","Hightom48","Crash49"];
            egui::ComboBox::from_id_salt("starter_drum_map").selected_text(labels[self.browser.drum_map_key.min(7)]).show_ui(ui,|ui|{for(i,label)in labels.iter().enumerate(){ui.selectable_value(&mut self.browser.drum_map_key,i,*label);}});
            egui::ComboBox::from_id_salt("starter_drum_map_pattern").selected_text(if self.browser.drum_map_pattern.is_empty(){"No pattern"}else{&self.browser.drum_map_pattern}).show_ui(ui,|ui|{for name in &pattern_names{ui.selectable_value(&mut self.browser.drum_map_pattern,name.clone(),name);}});
            if ui.add_enabled(self.browser.job.is_none()&&!track.is_empty()&&!self.browser.drum_map_pattern.is_empty(),egui::Button::new(format!("MAP {track} / {}",self.browser.drum_map_pattern))).on_hover_text("Explicit pitch edit of this track in the selected pattern. Rejects mixed-pitch parts and slides. Undo restores original notes.").clicked(){let r=self.engine.lock().unwrap().call_from("map_drum_notes",&json!({"track":track,"pattern":self.browser.drum_map_pattern,"key":([36,38,39,42,46,45,48,49][self.browser.drum_map_key.min(7)])}),"you");self.browser.job_result=match r{Ok(v)=>format!("Mapped {} notes on {track} in {} to key{}",v["changed"],v["pattern"],v["key"]),Err(e)=>format!("No mapping: {e:#}")};}
        });
        ui.label(RichText::new("Opt-in only: changes all pitches of a single-pitch drum part in the selected pattern. No melody/whole-kit remapping. Load the kit first, then map if needed.").small());

        if !self.browser.job_result.is_empty() {
            ui.label(&self.browser.job_result);
        }
        let busy = self.browser.job.as_ref().map(|j| j.0.clone());
        if let Some(label) = &busy {
            ui.label(format!("Working: {label}"));
            ui.spinner();
        }
        let mut job = None;
        ui.vertical(|ui| {
            ui.set_width((ui.clip_rect().width()-48.0).max(140.0));
            for (id,label,detail) in [
                ("vcsl_acoustic_drum_kit","Acoustic drums","Recorded kick, snare, hats, toms and crash. GM keys36/38/42/46/45/48/49."),
                ("vcsl_upright_piano_yamaha_lite","Yamaha upright piano","Real piano, one recorded velocity layer. Light version; not a full dynamic concert library."),
                ("vcsl_balafon","Balafon","Recorded wooden mallet instrument with velocity layers."),
            ] {
                let Some(ent)=crate::sound_catalog::entry(id) else {continue;};
                ui.separator();ui.label(RichText::new(label).strong());ui.label(detail);
                ui.label(format!("CC0 | up to {:.1} MB | audio stored locally",ent.bytes as f64/1_000_000.));
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(busy.is_none(),egui::Button::new("Hear recording")).clicked(){job=Some((format!("hear {label}"),"audition_instrument",json!({"catalog_id":id,"play":true,"max_mb":100})));}
                    if ui.add_enabled(busy.is_none()&&!track.is_empty(),egui::Button::new(format!("Use on {track}"))).clicked(){job=Some((format!("load {label}"),"load_instrument",json!({"catalog_id":id,"track":track,"max_mb":100})));}
                    if ui.add_enabled(busy.is_none(),egui::Button::new("+ New channel")).clicked(){job=Some((format!("load {label}"),"load_instrument",json!({"catalog_id":id,"track":label,"new_channel":true,"max_mb":100})));}
                });
                ui.horizontal_wrapped(|ui|{ui.hyperlink_to("Source and license",&ent.license_url);});
            }
            ui.separator();ui.label("More recorded sounds are in Sample packs. Modeled synths remain available separately; existing projects keep their sounds.");
        });
        if let Some((label, tool, args)) = job {
            self.library_job(label, ui.ctx().clone(), tool, args);
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
                                        json!({"track": id, "catalog_id": id,"new_channel":true}),
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
        let (none, gone) = import_dropped(&mut e, &[d.join("nowhere.wav")]);
        assert!(none.is_empty(), "{none:?}");
        assert_eq!(
            gone,
            vec!["nowhere.wav: there is no file at that path".to_string()]
        );
        let track = format!("{}_s", added[0]);
        assert!(
            e.project.tracks.iter().any(|t| t.name == track),
            "no channel {track}: {:?}",
            e.project.tracks.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
    }
}

#[cfg(test)]
mod new_channel_tests {
    use super::*;
    #[test]
    fn new_recorded_channel_never_reuses_a_track_or_bus_name() {
        let mut p = Project::default();
        assert_eq!(
            new_library_channel_name(&p, "Acoustic drums"),
            "Acoustic drums"
        );
        p.tracks.push(crate::project::Track::new(
            "acoustic DRUMS",
            crate::instruments::Instrument::Multisample(Default::default()),
        ));
        p.tracks.push(crate::project::Track::new(
            "Acoustic drums #2",
            crate::instruments::Instrument::Multisample(Default::default()),
        ));
        assert_eq!(
            new_library_channel_name(&p, "Acoustic drums"),
            "Acoustic drums #3"
        );
        p.buses.push(crate::project::Bus::new("Acoustic drums #3"));
        assert_eq!(
            new_library_channel_name(&p, "Acoustic drums"),
            "Acoustic drums #4"
        );
        assert_ne!(
            new_library_channel_name(&p, "MASTER").to_lowercase(),
            "master"
        );
    }
    #[test]
    fn only_new_channel_requests_get_a_fresh_target() {
        let mut p = Project::default();
        p.tracks.push(crate::project::Track::new(
            "Balafon",
            crate::instruments::Instrument::Multisample(Default::default()),
        ));
        let before = serde_json::to_value(&p).unwrap();
        let mut new =
            json!({"track":"Balafon","catalog_id":"vcsl_balafon","max_mb":100,"new_channel":true});
        let mut expected = new.clone();
        expected["track"] = json!("Balafon #2");
        prepare_library_load(&p, &mut new);
        assert_eq!(new, expected);
        let mut use_existing = json!({"track":"Balafon","catalog_id":"vcsl_balafon","max_mb":100});
        let original = use_existing.clone();
        prepare_library_load(&p, &mut use_existing);
        assert_eq!(use_existing, original);
        assert_eq!(serde_json::to_value(p).unwrap(), before);
    }
    #[test]
    fn disconnected_sound_jobs_return_failure_instead_of_staying_busy() {
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(library_job_result(&rx).is_none());
        tx.send(Ok(json!({"name":"ready"}))).unwrap();
        assert_eq!(library_job_result(&rx).unwrap().unwrap()["name"], "ready");
        tx.send(Err("download failed".into())).unwrap();
        assert_eq!(
            library_job_result(&rx).unwrap().unwrap_err(),
            "download failed"
        );
        drop(tx);
        assert!(library_job_result(&rx)
            .unwrap()
            .unwrap_err()
            .contains("stopped unexpectedly"));
    }
}

#[cfg(test)]
mod production_read_tests {
    use super::*;
    #[test]
    fn read_exact_nonmutating_and_reject_unsupported() {
        let mut p = Project::default();
        let mut fx = crate::fx::CompressorFx {
            threshold_db: -25.0,
            ratio: 4.0,
            attack_ms: 12.0,
            release_ms: 100.0,
            mix: 0.8,
            sc_hpf_hz: 90.0,
            knee_db: 6.0,
            ..Default::default()
        };
        fx.id = "compressor1".into();
        fx.bypass = true;
        p.master_effects = vec![crate::fx::Effect::Compressor(fx)];
        let before = serde_json::to_value(&p).unwrap();
        let mut b = BrowserState::default();
        b.read_production(&p, "master", "glue").unwrap();
        assert_eq!(b.glue_threshold, -25.0);
        assert_eq!(b.glue_mix, 0.8);
        assert_eq!(serde_json::to_value(&p).unwrap(), before);
        if let crate::fx::Effect::Compressor(v) = &mut p.master_effects[0] {
            v.makeup_db = 2.0;
        }
        assert!(b.read_production(&p, "master", "glue").is_err());
        assert_eq!(b.glue_threshold, -25.0);
        assert!(b.read_production(&p, "master", "colour").is_err());
        assert!(b.read_production(&p, "missing", "glue").is_err());
        p.master_effects = vec![crate::fx::Effect::Width(crate::fx::WidthFx {
            amount: 1.2,
            low_mono_hz: 0.0,
            ..Default::default()
        })];
        b.read_production(&p, "master", "width").unwrap();
        assert_eq!(b.mono_cutoff, 0.0);
        p.master_effects = vec![crate::fx::Effect::Delay(crate::fx::DelayFx {
            steps: 1.5,
            ..Default::default()
        })];
        assert!(b.read_production(&p, "master", "delay").is_err());
        assert!(!b.delay_ready);
    }
}

impl BrowserState {
    fn sync_sample_performance(
        &mut self,
        source: (String, f32, f32, f32, bool, bool, bool, bool, f32, f32),
    ) {
        if self.sample_perf_source.as_ref() != Some(&source) || self.sample_perf.is_none() {
            self.sample_perf = Some(source.clone());
            self.sample_perf_source = Some(source);
        }
    }
}

#[cfg(test)]
mod sample_performance_sync_tests {
    use super::*;
    #[test]
    fn refresh_changes_and_preserve_drafts_until_source_changes() {
        let mut b = BrowserState::default();
        let original = (
            "piano".into(),
            2.0,
            350.0,
            0.6,
            false,
            true,
            true,
            false,
            1.0,
            0.0,
        );
        b.sync_sample_performance(original.clone());
        b.sample_perf.as_mut().unwrap().2 = 777.0;
        b.sync_sample_performance(original.clone());
        assert_eq!(b.sample_perf.as_ref().unwrap().2, 777.0);
        let mut changed = original.clone();
        changed.2 = 1200.0;
        b.sync_sample_performance(changed.clone());
        assert_eq!(b.sample_perf.as_ref(), Some(&changed));
        b.sync_sample_performance(original.clone());
        assert_eq!(b.sample_perf.as_ref(), Some(&original));
        let mut other = original.clone();
        other.0 = "drums".into();
        other.4 = true;
        b.sync_sample_performance(other.clone());
        assert_eq!(b.sample_perf.as_ref(), Some(&other));
    }
}

#[cfg(test)]
mod translation_gui_tests {
    use super::*;
    #[test]
    fn range_and_project_identity_guard_prepared_audio() {
        let mut e = crate::Engine::new(
            std::env::temp_dir().join(format!("bb229_guard_{}", std::process::id())),
        );
        let rev = e.revision;
        let generation = e.project_load_generation;
        assert!(translation_current(
            rev,
            generation,
            1.0,
            2.0,
            &e,
            (1.0, 2.0)
        ));
        assert!(!translation_current(
            rev,
            generation,
            1.0,
            2.0,
            &e,
            (2.0, 2.0)
        ));
        assert!(!translation_current(
            rev,
            generation,
            1.0,
            2.0,
            &e,
            (1.0, 3.0)
        ));
        e.revision += 1;
        assert!(!translation_current(
            rev,
            generation,
            1.0,
            2.0,
            &e,
            (1.0, 2.0)
        ));
        e.revision = rev;
        e.project_load_generation += 1;
        assert!(!translation_current(
            rev,
            generation,
            1.0,
            2.0,
            &e,
            (1.0, 2.0)
        ));
    }
    #[test]
    fn preparation_does_not_autoplay_on_live_transport_and_reports_short_end() {
        let mut own = crate::Engine::new(
            std::env::temp_dir().join(format!("bb229_prepare_{}", std::process::id())),
        );
        own.call("new_project", &json!({"name":"prepare","bpm":120}))
            .unwrap();
        let (v, audio) = prepare_translation(own, "stereo", 8.0, 8.0).unwrap();
        assert_eq!(v["truncated"], true);
        assert!(!audio.0.is_empty());
        let text = translation_readout(&v);
        assert!(text.contains("shorter than requested"));
        assert!(text.contains("Prepared; project unchanged."));
        assert!(!text.contains("Queued"));
    }
}

#[cfg(test)]
mod browser_comparison_tests {
    use super::*;
    fn fixture() -> crate::Engine {
        let mut e = crate::Engine::new(
            std::env::temp_dir().join(format!("bb230_{}", crate::creative::fresh_seed())),
        );
        e.call("new_project", &json!({"name":"comparison","bpm":120}))
            .unwrap();
        e.call("add_track", &json!({"name":"keys","preset":"pad"}))
            .unwrap();
        e.call(
            "add_notes",
            &json!({"track":"keys","notes":[{"start":0,"len":16,"pitch":60}]}),
        )
        .unwrap();
        e.call("snapshot", &json!({"name":"before"})).unwrap();
        e
    }
    fn browser(e: &crate::Engine) -> BrowserState {
        BrowserState {
            ab_snapshot: "before".into(),
            ab_start_s: 0.,
            ab_length_s: 2.,
            ab_checkpoint_project: Some(e.version("before").unwrap()),
            ..Default::default()
        }
    }
    #[test]
    fn comparison_rejects_project_load_range_checkpoint_and_overwrite_changes() {
        let mut e = fixture();
        let mut b = browser(&e);
        let r = e.revision;
        let g = e.project_load_generation;
        let valid = |b: &BrowserState, e: &crate::Engine| {
            browser_comparison_current(r, g, "before", 0., 2., b, e)
        };
        assert!(valid(&b, &e));
        b.ab_start_s = 1.;
        assert!(!valid(&b, &e));
        b.ab_start_s = 0.;
        b.ab_length_s = 3.;
        assert!(!valid(&b, &e));
        b.ab_length_s = 2.;
        b.ab_snapshot = "other".into();
        assert!(!valid(&b, &e));
        b.ab_snapshot = "before".into();
        e.revision += 1;
        assert!(!valid(&b, &e));
        e.revision = r;
        e.project_load_generation += 1;
        assert!(!valid(&b, &e));
        e.project_load_generation = g;
        // Snapshot overwrites do not increment engine revision.
        e.snapshots[0].1.project.bpm += 1.;
        assert!(!valid(&b, &e));
        e.snapshots.clear();
        assert!(!valid(&b, &e));
    }
    #[test]
    fn isolated_comparison_preserves_live_engine_and_uses_checkpoint_not_file() {
        let mut e = fixture();
        e.call("set_mixer", &json!({"track":"keys","volume_db":6}))
            .unwrap();
        let project = e.project.clone();
        let revision = e.revision;
        let history = e.undo_depth().0;
        std::fs::create_dir_all(&e.workdir).unwrap();
        std::fs::write(e.workdir.join("before"), b"not audio").unwrap();
        let own = super::super::checkpoint_comparison_engine(&e, "before").unwrap();
        let job = std::thread::spawn(move || {
            super::super::render_checkpoint_comparison(own, "before", 0., 2.)
        });
        e.call("set_mixer", &json!({"track":"keys","volume_db":-9}))
            .unwrap();
        let (value, audio) = job.join().unwrap().unwrap();
        assert_eq!(value["A"], "before");
        assert_eq!(value["loudness_matched"], true);
        assert!(value["judgement"].is_null());
        assert!(!audio.0.is_empty());
        assert!(value["level_change_db"]["B"].as_f64().unwrap() < 0.);
        assert!(e.transport.pending_audition.is_none());
        assert!(e.transport.pending.is_empty());
        assert!(e.revision > revision);
        assert!(e.undo_depth().0 > history);
        assert_ne!(e.project, project);
        let text = browser_comparison_readout("before", 0., 2., &value);
        assert!(text.contains("Prepared; project unchanged."));
        assert!(!text.contains("Queued"));
    }
    #[test]
    fn explicit_play_queues_once_stop_clears_and_stale_play_fails() {
        let mut e = fixture();
        let b = browser(&e);
        let (value, audio) = super::super::render_checkpoint_comparison(
            super::super::checkpoint_comparison_engine(&e, "before").unwrap(),
            "before",
            0.,
            2.,
        )
        .unwrap();
        let result = super::super::MixComparison {
            revision: e.revision,
            generation: e.project_load_generation,
            checkpoint: "before".into(),
            start_s: 0.,
            length_s: 2.,
            value,
            audio,
        };
        assert!(browser_comparison_current(
            result.revision,
            result.generation,
            "before",
            0.,
            2.,
            &b,
            &e
        ));
        assert!(super::super::play_checkpoint_comparison(
            &result, "before", 0., 2., &mut e
        ));
        assert!(e.transport.pending_audition.as_ref().unwrap().is_some());
        super::super::stop_comparison_preview(&mut e);
        assert!(e.transport.pending_audition.as_ref().unwrap().is_none());
        e.revision += 1;
        assert!(!super::super::play_checkpoint_comparison(
            &result, "before", 0., 2., &mut e
        ));
        assert!(e.transport.pending_audition.as_ref().unwrap().is_none());
    }
    #[test]
    fn readout_keeps_silence_and_different_length_warnings() {
        let v = json!({"facts":{"A":{"seconds":1},"B":{"seconds":2}},"loudness_matched":true});
        assert!(
            browser_comparison_readout("before", 0., 2., &v).contains("Different rendered lengths")
        );
        let v = json!({"facts":{"A":{"seconds":2},"B":{"seconds":2}},"loudness_matched":false});
        assert!(browser_comparison_readout("before", 0., 2., &v).contains("not level matched"));
    }
}

#[cfg(test)]
mod translation_mono_readout_tests {
    use super::*;
    #[test]
    fn silence_cancellation_and_measurement_are_not_quality_scores() {
        let mut v = json!({"mode":"mono","start_s":1.,"seconds":2.,"safety_gain_db":-6.,"mono_check":{"status":"source_below_floor","mono_change_db":null}});
        let s = translation_readout(&v);
        assert!(s.contains("mono change unavailable"));
        assert!(!s.contains("RMS change 0.0"));
        v["mono_check"]["status"] = json!("mono_below_floor");
        assert!(translation_readout(&v).contains("exact change unavailable"));
        v["mono_check"] = json!({"status":"measured","mono_change_db":-3.0103});
        let s = translation_readout(&v);
        assert!(s.contains("-3.0 dB (before filters/gain)"));
        assert!(s.contains("safety gain -6.0 dB"));
        assert!(s.contains("not a quality score"));
        assert!(s.contains("Prepared;"));
    }
}
