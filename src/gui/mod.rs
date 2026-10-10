//! Beatbox Studio — the native desktop GUI (egui).
//!
//! Every click goes through `Engine::call`, exactly like an AI would, so the
//! studio and MCP clients always see the same project. A local control server
//! lets `beatbox mcp --connect` drive this window live.

mod console;
mod piano;
mod player;
mod playlist;
mod shortcuts;
mod theme;
mod views;
mod vocal_view;
mod widgets;

use crate::analysis::{self, Report};
use crate::engine::{Engine, LogEntry};
use crate::instruments;
use crate::project::{Project, STEPS_PER_BAR};
use crate::render::{self, Mix, RenderOptions};
use crate::{fx, theory};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use player::Player;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use widgets::*;

struct Rendered {
    rev: u64,
    mix: Arc<Mix>,
    peaks: Vec<(f32, f32)>,
    spectrum: Vec<f32>,
    report: Report,
    loop_secs: f32,
    strips: Vec<views::StripMeter>,
}

#[derive(Clone)]
struct InspectorCache {
    track: usize,
    rev: u64,
    inst: Value,
    effects: Vec<Value>,
}

pub struct Studio {
    engine: Arc<Mutex<Engine>>,
    player: Player,
    project_path: Option<PathBuf>,
    listen: String,
    server_ok: bool,
    // view state
    pattern: usize,
    selected: usize,
    // render pipeline
    rendered: Option<Rendered>,
    rendering: Option<(u64, Receiver<Rendered>)>,
    inspector: Option<InspectorCache>,
    dragging: bool,
    toast: Option<(String, f64, bool)>,
    // screenshot mode
    screenshot: Option<PathBuf>,
    frames: u64,
    shot_requested: bool,
    // central view: 0 sequencer, 1 mixer, 2 automation, 3 playlist
    view: usize,
    auto_sel: Option<(String, String)>,
    piano: piano::PianoState,
    /// vocal view: (sample, waveform peaks, seconds)
    vocal_wave: Option<(String, Vec<(f32, f32)>, f32)>,
}

const VIEWS: [&str; 5] = ["SEQUENCER", "MIXER", "AUTOMATION", "PLAYLIST", "VOCAL"];

pub fn run(
    engine: Engine,
    listen: &str,
    project: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    view: Option<String>,
) -> anyhow::Result<()> {
    let view = view
        .and_then(|v| VIEWS.iter().position(|n| n.eq_ignore_ascii_case(&v)))
        .unwrap_or(0);
    let engine = Arc::new(Mutex::new(engine));
    let server_ok = crate::server::spawn(engine.clone(), listen).is_ok();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Beatbox Studio")
            .with_inner_size([1480.0, 920.0])
            .with_min_inner_size([1100.0, 700.0]),
        ..Default::default()
    };
    let listen = listen.to_string();
    eframe::run_native(
        "Beatbox Studio",
        options,
        Box::new(move |cc| {
            apply_theme(&cc.egui_ctx);
            Ok(Box::new(Studio {
                engine,
                player: Player::new(),
                project_path: project,
                listen,
                server_ok,
                pattern: 0,
                selected: 0,
                rendered: None,
                rendering: None,
                inspector: None,
                dragging: false,
                toast: None,
                screenshot,
                frames: 0,
                shot_requested: false,
                view,
                auto_sel: None,
                piano: piano::PianoState {
                    snap: 2,
                    ..Default::default()
                },
                vocal_wave: None,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("studio failed: {e}"))
}

/// Compute (pattern index, local step) for a song position in seconds.
fn locate(p: &Project, secs: f32) -> Option<(usize, f32)> {
    let mut step = secs / p.step_secs();
    for s in p.song_sections() {
        let pi = p.pattern_index(&s.pattern).ok()?;
        let len = p.patterns[pi].steps() as f32;
        for _ in 0..s.repeats.max(1) {
            if step < len {
                return Some((pi, step));
            }
            step -= len;
        }
    }
    None
}

fn param_range(kind: &str, name: &str, v: f32) -> (f32, f32, bool) {
    match name {
        "cutoff" | "low_freq" | "high_freq" | "tone" => (20.0, 18000.0, true),
        "attack" if kind == "transient" => (-1.0, 1.0, false),
        "sustain" if kind == "transient" => (-1.0, 1.0, false),
        "attack" | "decay" | "release" if kind == "env" => (0.001, 4.0, true),
        "sustain" => (0.0, 1.0, false),
        "attack" | "release" if kind == "sampler" => (0.0005, 2.0, true),
        "decay" => (0.05, 4.0, true),
        "unison" => (1.0, 9.0, false),
        "osc2_semitones" | "tune" => (-24.0, 24.0, false),
        "osc2_cents" => (-50.0, 50.0, false),
        "unison_spread_cents" => (0.0, 60.0, false),
        "filter_env_amount" => (-6.0, 6.0, false),
        "lfo_rate" => (0.0, 12.0, false),
        "lfo_to_cutoff" => (0.0, 3.0, false),
        "lfo_to_pitch" => (0.0, 2.0, false),
        "pitch_env_semitones" => (-24.0, 48.0, false),
        "pitch_env_time" => (0.001, 1.0, true),
        "ratio" if kind == "fm" => (0.25, 16.0, false),
        "ratio" => (1.0, 20.0, false),
        "index" => (0.0, 12.0, false),
        "ratio2" => (0.0, 20.0, false),
        "index2" => (0.0, 5.0, false),
        "punch" => (0.0, 24.0, false),
        "root" => (24.0, 96.0, false),
        "max_length" => (0.0, 10.0, false),
        "threshold_db" => (-60.0, 0.0, false),
        "attack_ms" => (0.1, 200.0, true),
        "release_ms" => (5.0, 2000.0, true),
        "makeup_db" => (0.0, 24.0, false),
        "db" => (-24.0, 24.0, false),
        "ceiling_db" => (-12.0, 0.0, false),
        "low_db" | "mid_db" | "high_db" => (-18.0, 18.0, false),
        "bits" => (1.0, 16.0, false),
        "downsample" => (1.0, 64.0, false),
        "steps" => (0.5, 16.0, false),
        "predelay_ms" => (0.0, 200.0, false),
        "width" => (0.0, 2.0, false),
        "amount" if kind == "width" => (0.0, 3.0, false),
        "rate_hz" => (0.05, 10.0, true),
        "depth_ms" => (0.0, 15.0, false),
        "feedback" => (0.0, 0.95, false),
        "gain" => (0.0, 1.5, false),
        _ => (0.0, 1.0f32.max(v.abs() * 2.0), false),
    }
}

const ENUMS: &[(&str, &[&str])] = &[
    ("osc1", &["sine", "saw", "square", "triangle", "noise"]),
    ("osc2", &["sine", "saw", "square", "triangle", "noise"]),
    ("filter_mode", &["lowpass", "highpass", "bandpass", "notch"]),
    ("mode", &["lowpass", "highpass", "bandpass", "notch"]),
    (
        "kind",
        &[
            "kick",
            "snare",
            "clap",
            "closed_hat",
            "open_hat",
            "rim",
            "tom",
            "cowbell",
            "shaker",
            "crash",
        ],
    ),
];

/// Generic parameter editor over a JSON object. Returns true when an edit was committed.
fn param_editor(
    ui: &mut egui::Ui,
    id: &str,
    kind: &str,
    v: &mut Value,
    color: Color32,
    dragging: &mut bool,
) -> bool {
    let mut committed = false;
    let Some(obj) = v.as_object_mut() else {
        return false;
    };
    let keys: Vec<String> = obj.keys().cloned().collect();
    // enums & bools first, then knobs, then envelopes
    ui.horizontal_wrapped(|ui| {
        for k in &keys {
            if let Some((_, opts)) = ENUMS.iter().find(|(n, _)| n == k) {
                let cur = obj[k].as_str().unwrap_or("").to_string();
                let mut sel = cur.clone();
                egui::ComboBox::from_id_salt(format!("{id}-{k}"))
                    .width(96.0)
                    .selected_text(
                        RichText::new(format!("{}: {}", k.replace('_', " "), sel)).size(12.0),
                    )
                    .show_ui(ui, |ui| {
                        for o in opts.iter() {
                            ui.selectable_value(&mut sel, o.to_string(), *o);
                        }
                    });
                if sel != cur {
                    obj.insert(k.clone(), json!(sel));
                    committed = true;
                }
            } else if let Some(b) = obj[k].as_bool() {
                let mut bb = b;
                if ui.checkbox(&mut bb, k.replace('_', " ")).changed() {
                    obj.insert(k.clone(), json!(bb));
                    committed = true;
                }
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for k in &keys {
            if k == "type" {
                continue;
            }
            if let Some(x) = obj[k].as_f64() {
                let mut f = x as f32;
                let (lo, hi, log) = param_range(kind, k, f);
                let (_, done) = knob(ui, k, &mut f, lo, hi, log, color);
                if (f as f64 - x).abs() > 1e-9 {
                    *dragging = true;
                    let val = if k == "unison" || k == "downsample" || k == "root" {
                        json!(f.round() as i64)
                    } else {
                        json!(f)
                    };
                    obj.insert(k.clone(), val);
                }
                if done {
                    committed = true;
                }
            }
        }
    });
    for k in &keys {
        if obj[k].is_object() {
            ui.label(
                RichText::new(k.replace('_', " ").to_uppercase())
                    .size(10.0)
                    .color(DIM),
            );
            let mut sub = obj[k].clone();
            if param_editor(ui, &format!("{id}-{k}"), "env", &mut sub, color, dragging) {
                committed = true;
            }
            obj.insert(k.clone(), sub);
        }
    }
    committed
}

impl Studio {
    fn call(&mut self, tool: &str, args: Value) {
        let r = self.engine.lock().unwrap().call_from(tool, &args, "you");
        if let Err(e) = r {
            self.toast = Some((format!("{e:#}"), 0.0, false));
        }
    }

    fn snapshot(&self) -> (Project, u64, Vec<LogEntry>, (usize, usize)) {
        let e = self.engine.lock().unwrap();
        let log = e.log.iter().rev().take(40).cloned().collect();
        (e.project.clone(), e.revision, log, e.undo_depth())
    }

    fn pump_render(&mut self, project: &Project, rev: u64, ctx: &egui::Context) {
        if let Some((want, rx)) = &self.rendering {
            if let Ok(r) = rx.try_recv() {
                let _ = want;
                self.player.set_mix(r.mix.clone(), r.loop_secs);
                self.rendered = Some(r);
                self.rendering = None;
            }
        }
        let have = self.rendered.as_ref().map(|r| r.rev);
        let busy = self.rendering.is_some();
        if have != Some(rev) && !busy && !self.dragging {
            let mut p = project.clone();
            let bank = {
                let mut e = self.engine.lock().unwrap();
                let samples = e.project.samples.clone();
                e.bank.sync(&samples);
                e.bank.clone()
            };
            p.arrangement = project.arrangement.clone();
            let (tx, rx) = channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                if let Ok(mix) = render::render(
                    &p,
                    &bank,
                    &RenderOptions {
                        keep_stems: true,
                        ..Default::default()
                    },
                ) {
                    let mono: Vec<f32> = mix
                        .left
                        .iter()
                        .zip(mix.right.iter())
                        .map(|(a, b)| 0.5 * (a + b))
                        .collect();
                    let peaks = analysis::waveform_peaks(&mono, 1400);
                    let spectrum = analysis::log_spectrum_db(&mono, 72);
                    let report = analysis::analyze(&mix);
                    let loop_secs = p.song_seconds();
                    let mut strips: Vec<views::StripMeter> = mix
                        .stems
                        .iter()
                        .chain(mix.bus_stems.iter())
                        .map(|s| views::strip_meter(&s.name, &s.left, &s.right))
                        .collect();
                    strips.push(views::strip_meter("master", &mix.left, &mix.right));
                    let _ = tx.send(Rendered {
                        strips,
                        rev,
                        mix: Arc::new(mix),
                        peaks,
                        spectrum,
                        report,
                        loop_secs,
                    });
                    ctx.request_repaint();
                }
            });
            self.rendering = Some((rev, rx));
        }
    }

    fn save(&mut self) {
        let path = self
            .project_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("beat.beatbox.json"));
        self.call("save_project", json!({"path": path}));
        self.project_path = Some(path.clone());
        self.toast = Some((format!("Saved {}", path.display()), 0.0, true));
    }

    fn render_wav(&mut self) {
        let r = self
            .engine
            .lock()
            .unwrap()
            .call_from("render", &json!({}), "you");
        match r {
            Ok(v) => {
                self.toast = Some((
                    format!("Rendered {}", v["path"].as_str().unwrap_or("")),
                    0.0,
                    true,
                ))
            }
            Err(e) => self.toast = Some((format!("{e:#}"), 0.0, false)),
        }
    }

    fn shortcut(&mut self, a: shortcuts::Action, p: &Project) {
        use shortcuts::Action;
        match a {
            Action::PlayStop => {
                if self.player.is_playing() {
                    self.player.stop();
                } else {
                    self.player.play();
                }
            }
            Action::Undo => self.call("undo", json!({})),
            Action::Redo => self.call("redo", json!({})),
            Action::Save => self.save(),
            Action::Render => self.render_wav(),
            Action::View(v) => {
                if v < VIEWS.len() {
                    self.view = v;
                }
            }
            Action::PrevPattern => self.pattern = self.pattern.saturating_sub(1),
            Action::NextPattern => {
                self.pattern = (self.pattern + 1).min(p.patterns.len().saturating_sub(1))
            }
            Action::PrevTrack => self.selected = self.selected.saturating_sub(1),
            Action::NextTrack => {
                self.selected = (self.selected + 1).min(p.tracks.len().saturating_sub(1))
            }
            Action::MuteSelected => {
                if let Some(t) = p.tracks.get(self.selected) {
                    self.call("set_mixer", json!({"track": t.name, "mute": !t.mute}));
                }
            }
            Action::SoloSelected => {
                if let Some(t) = p.tracks.get(self.selected) {
                    self.call("set_mixer", json!({"track": t.name, "solo": !t.solo}));
                }
            }
            Action::SeekStart => self.player.seek(0.0),
        }
    }

    // ---------------- top bar ----------------
    fn top_bar(&mut self, ui: &mut egui::Ui, p: &Project, undo: (usize, usize)) {
        ui.horizontal_centered(|ui| {
            // logo
            let (r, _) = ui.allocate_exact_size(Vec2::new(30.0, 30.0), Sense::hover());
            let c = r.center();
            ui.painter().circle_filled(c, 14.0, ACCENT);
            ui.painter().circle_filled(c, 8.0, ACCENT2);
            ui.painter().circle_filled(c, 3.5, BG);
            ui.label(
                RichText::new("BEATBOX")
                    .size(19.0)
                    .strong()
                    .color(TEXT)
                    .extra_letter_spacing(2.0),
            );
            ui.label(RichText::new("STUDIO").size(11.0).color(DIM));
            ui.add_space(18.0);
            let playing = self.player.is_playing();
            if transport_button(ui, playing)
                .on_hover_text(format!(
                    "audio: {}\n\nShortcuts:\n{}",
                    self.player.device_name,
                    shortcuts::help_line()
                ))
                .clicked()
            {
                if playing {
                    self.player.stop();
                } else {
                    self.player.play();
                }
            }
            let pos = self.player.position_secs();
            let total = p.song_seconds();
            ui.add_space(6.0);
            // console counters: Bars|Beats (PPQ timebase) and Min:Secs
            {
                use crate::timebase::{
                    format_position, FrameRate, SampleRate, TempoMap, TimeFormat,
                };
                let sr = SampleRate::default();
                let map = TempoMap::for_project(p);
                let at = sr.samples(f64::from(pos));
                let bb = format_position(at, TimeFormat::BarsBeats, sr, &map, FrameRate::Fps30, 0);
                let ms = format_position(at, TimeFormat::MinSecs, sr, &map, FrameRate::Fps30, 0);
                let len = format_position(
                    sr.samples(f64::from(total)),
                    TimeFormat::MinSecs,
                    sr,
                    &map,
                    FrameRate::Fps30,
                    0,
                );
                let (r, _) = ui.allocate_exact_size(Vec2::new(300.0, 34.0), Sense::hover());
                let pt = ui.painter();
                let main = Rect::from_min_size(r.min, Vec2::new(150.0, 34.0));
                console::counter_box(pt, main, &bb, 17.0);
                pt.text(
                    main.left_top() + Vec2::new(5.0, 2.0),
                    Align2::LEFT_TOP,
                    "BARS|BEATS",
                    FontId::proportional(7.5),
                    DIM,
                );
                let sub = Rect::from_min_size(
                    Pos2::new(main.right() + 4.0, r.top()),
                    Vec2::new(146.0, 34.0),
                );
                console::counter_box(pt, sub, &ms, 13.0);
                pt.text(
                    sub.left_top() + Vec2::new(5.0, 2.0),
                    Align2::LEFT_TOP,
                    "MIN:SECS",
                    FontId::proportional(7.5),
                    DIM,
                );
                pt.text(
                    sub.right_bottom() - Vec2::new(5.0, 2.0),
                    Align2::RIGHT_BOTTOM,
                    format!("/ {len}"),
                    FontId::proportional(8.0),
                    DIM,
                );
            }
            ui.add_space(10.0);
            let mut bpm = p.bpm;
            ui.label(RichText::new("BPM").size(11.0).color(DIM));
            let r = ui.add(
                egui::DragValue::new(&mut bpm)
                    .range(40.0..=300.0)
                    .speed(0.3)
                    .fixed_decimals(0),
            );
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                self.call("set_tempo", json!({"bpm": bpm}));
            }
            let mut swing = p.swing;
            ui.label(RichText::new("SWING").size(11.0).color(DIM));
            let r = ui.add(egui::Slider::new(&mut swing, 0.0..=1.0).show_value(false));
            if r.drag_stopped() {
                self.call("set_tempo", json!({"swing": swing}));
            }
            ui.add_space(8.0);
            let mut root = p.key_root.clone();
            let mut scale = p.scale.clone();
            egui::ComboBox::from_id_salt("key")
                .width(56.0)
                .selected_text(RichText::new(&root).strong())
                .show_ui(ui, |ui| {
                    for n in theory::NOTE_NAMES {
                        ui.selectable_value(&mut root, n.to_string(), n);
                    }
                });
            egui::ComboBox::from_id_salt("scale")
                .width(120.0)
                .selected_text(&scale)
                .show_ui(ui, |ui| {
                    for (n, _) in theory::SCALES {
                        ui.selectable_value(&mut scale, n.to_string(), *n);
                    }
                });
            if root != p.key_root || scale != p.scale {
                self.call("set_key", json!({"root": root, "scale": scale}));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // live AI badge
                let (r, _) = ui.allocate_exact_size(Vec2::new(214.0, 26.0), Sense::hover());
                let t = ui.input(|i| i.time) as f32;
                let pulse = 0.55 + 0.45 * (t * 2.5).sin().abs();
                let col = if self.server_ok { GOOD } else { HOT };
                ui.painter().rect_filled(r, 13.0, col.gamma_multiply(0.12));
                ui.painter().circle_filled(
                    Pos2::new(r.left() + 14.0, r.center().y),
                    4.5,
                    col.gamma_multiply(pulse),
                );
                ui.painter().text(
                    Pos2::new(r.left() + 26.0, r.center().y),
                    Align2::LEFT_CENTER,
                    if self.server_ok {
                        format!("AI LINK {}", self.listen.rsplit(':').next().unwrap_or(""))
                    } else {
                        "AI LINK OFF".into()
                    },
                    FontId::proportional(11.5),
                    col,
                );
                ui.painter().text(
                    Pos2::new(r.right() - 12.0, r.center().y),
                    Align2::RIGHT_CENTER,
                    format!("{} tools", crate::tools::registry().len()),
                    FontId::proportional(10.5),
                    DIM,
                );
                ui.add_space(6.0);
                if ui.button("Render WAV").on_hover_text("Cmd+R").clicked() {
                    self.render_wav();
                }
                if ui.button("Save").on_hover_text("Cmd+S").clicked() {
                    self.save();
                }
                if ui
                    .add_enabled(undo.1 > 0, egui::Button::new("Redo"))
                    .on_hover_text("Redo")
                    .clicked()
                {
                    self.call("redo", json!({}));
                }
                if ui
                    .add_enabled(undo.0 > 0, egui::Button::new("Undo"))
                    .on_hover_text("Undo")
                    .clicked()
                {
                    self.call("undo", json!({}));
                }
            });
        });
    }

    // ---------------- browser ----------------
    fn browser(&mut self, ui: &mut egui::Ui, p: &Project) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            section_label(ui, "INSTANT BEAT");
            let styles: Vec<&str> = theory::GROOVES.iter().map(|g| g.name).collect();
            egui::Grid::new("styles").num_columns(2).spacing([6.0, 6.0]).show(ui, |ui| {
                for (i, s) in styles.iter().enumerate() {
                    let hue = i as f32 / styles.len() as f32;
                    let col = lerp_color(ACCENT, ACCENT2, hue);
                    let b = egui::Button::new(RichText::new(s.replace('_', " ")).size(12.5).color(TEXT))
                        .fill(col.gamma_multiply(0.22))
                        .stroke(Stroke::new(1.0_f32, col.gamma_multiply(0.6)))
                        .min_size(Vec2::new(98.0, 30.0));
                    if ui.add(b).on_hover_text(format!("generate_beat style={s}")).clicked() {
                        self.call("generate_beat", json!({"style": s, "seed": (ui.input(|i| i.time) * 1000.0) as u64 % 9973}));
                        self.pattern = 0;
                        self.selected = 0;
                    }
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
            ui.add_space(8.0);
            section_label(ui, "PATTERNS");
            for (i, pat) in p.patterns.iter().enumerate() {
                let sel = i == self.pattern;
                let r = ui.add(
                    egui::Button::new(RichText::new(format!("{}   {} bars", pat.name, pat.bars)).size(12.5))
                        .fill(if sel { ACCENT.gamma_multiply(0.35) } else { PANEL2 })
                        .min_size(Vec2::new(ui.available_width(), 26.0)),
                );
                if r.clicked() {
                    self.pattern = i;
                }
            }
            ui.horizontal(|ui| {
                if ui.small_button("+ new").clicked() {
                    let name = format!("P{}", p.patterns.len() + 1);
                    self.call("add_pattern", json!({"name": name, "bars": 4}));
                    self.pattern = p.patterns.len();
                }
                if ui.small_button("duplicate").clicked() {
                    if let Some(src) = p.patterns.get(self.pattern) {
                        let name = format!("{}_v{}", src.name, p.patterns.len() + 1);
                        self.call("add_pattern", json!({"name": name, "copy_from": src.name}));
                        self.pattern = p.patterns.len();
                    }
                }
            });
            ui.add_space(8.0);
            section_label(ui, "INSTRUMENTS");
            for (name, desc) in instruments::PRESETS {
                let kind = instruments::preset(name).map(|i| i.kind_name()).unwrap_or("synth");
                let col = track_color(name, kind);
                let r = ui
                    .horizontal(|ui| {
                        let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                        ui.painter().circle_filled(dot.center(), 4.0, col);
                        ui.add(egui::Label::new(RichText::new(name.replace('_', " ")).size(12.5)).sense(Sense::click()))
                    })
                    .inner
                    .on_hover_text(format!("{desc}\nclick to add a track"));
                if r.clicked() {
                    let mut n = name.to_string();
                    let mut k = 2;
                    while p.track_index(&n).is_ok() {
                        n = format!("{name}{k}");
                        k += 1;
                    }
                    self.call("add_track", json!({"name": n, "preset": name}));
                    self.selected = p.tracks.len();
                }
            }
        });
    }

    // ---------------- arrangement strip ----------------
    fn arrangement(&mut self, ui: &mut egui::Ui, p: &Project) {
        let h = 40.0;
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::click());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 8.0, BG);
        let total = p.song_steps().max(1) as f32;
        let mut x = rect.left() + 4.0;
        let w_avail = rect.width() - 8.0;
        for s in p.song_sections() {
            let Ok(pi) = p.pattern_index(&s.pattern) else {
                continue;
            };
            let pat = &p.patterns[pi];
            for _ in 0..s.repeats.max(1) {
                let w = w_avail * pat.steps() as f32 / total;
                let r = Rect::from_min_size(
                    Pos2::new(x, rect.top() + 5.0),
                    Vec2::new(w - 3.0, h - 10.0),
                );
                let col = lerp_color(ACCENT, ACCENT2, (pi as f32 * 0.37) % 1.0);
                painter.rect_filled(
                    r,
                    6.0,
                    col.gamma_multiply(if pi == self.pattern { 0.55 } else { 0.25 }),
                );
                painter.rect_stroke(r, 6.0, Stroke::new(1.0_f32, col.gamma_multiply(0.8)));
                painter.text(
                    r.left_center() + Vec2::new(8.0, 0.0),
                    Align2::LEFT_CENTER,
                    &pat.name,
                    FontId::proportional(12.0),
                    TEXT,
                );
                x += w;
            }
        }
        let pos = self.player.position_secs();
        let px = rect.left() + 4.0 + w_avail * (pos / p.song_seconds().max(0.01)).min(1.0);
        painter.line_segment(
            [Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())],
            Stroke::new(2.0_f32, Color32::WHITE),
        );
        if resp.clicked() {
            if let Some(m) = resp.interact_pointer_pos() {
                let t = ((m.x - rect.left() - 4.0) / w_avail).clamp(0.0, 1.0);
                self.player.seek(t * p.song_seconds());
                if let Some((pi, _)) = locate(p, t * p.song_seconds()) {
                    self.pattern = pi;
                }
            }
        }
    }

    // ---------------- sequencer ----------------
    fn sequencer(&mut self, ui: &mut egui::Ui, p: &Project) {
        let Some(pat) = p.patterns.get(self.pattern) else {
            return;
        };
        let steps = pat.steps() as usize;
        let header_w = 210.0;
        let playing_here = locate(p, self.player.position_secs())
            .filter(|(pi, _)| *pi == self.pattern)
            .map(|(_, s)| s);
        egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("seq").show(ui, |ui| {
            // ruler
            let (ruler, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 18.0), Sense::hover());
            let grid_left = ruler.left() + header_w;
            let cell = ((ruler.width() - header_w - 6.0) / steps as f32).max(4.0);
            for b in 0..pat.bars {
                let x = grid_left + (b * STEPS_PER_BAR) as f32 * cell;
                ui.painter().text(Pos2::new(x + 3.0, ruler.center().y), Align2::LEFT_CENTER, format!("{}", b + 1), FontId::monospace(11.0), DIM);
            }
            for (ti, t) in p.tracks.iter().enumerate() {
                let kind = t.instrument.kind_name();
                let color = track_color(&t.name, kind);
                let is_drum = t.instrument.is_drum() || matches!(&t.instrument, instruments::Instrument::Sampler(s) if s.one_shot);
                let row_h = if is_drum { 30.0 } else { 46.0 };
                let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::hover());
                let painter = ui.painter_at(row);
                let selected = ti == self.selected;
                // header
                let head = Rect::from_min_size(row.min, Vec2::new(header_w - 8.0, row_h - 4.0));
                painter.rect_filled(head, 7.0, if selected { Color32::from_rgb(54, 58, 64) } else { PANEL2 });
                painter.rect_filled(Rect::from_min_size(head.min, Vec2::new(4.0, head.height())), 2.0, color);
                painter.text(head.left_center() + Vec2::new(14.0, -6.0), Align2::LEFT_CENTER, &t.name, FontId::proportional(13.0), if t.mute { DIM } else { TEXT });
                painter.text(head.left_center() + Vec2::new(14.0, 8.0), Align2::LEFT_CENTER, kind, FontId::proportional(10.0), DIM);
                let head_resp = ui.interact(head, ui.id().with(("head", ti)), Sense::click());
                if head_resp.clicked() {
                    self.selected = ti;
                }
                // M / S pills
                let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(Pos2::new(head.right() - 54.0, head.center().y - 9.0), Vec2::new(54.0, 18.0))));
                cui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if pill(ui, "M", t.mute, WARN).clicked() {
                        self.call("set_mixer", json!({"track": t.name, "mute": !t.mute}));
                    }
                    if pill(ui, "S", t.solo, SOLO).clicked() {
                        self.call("set_mixer", json!({"track": t.name, "solo": !t.solo}));
                    }
                });
                // grid
                let grid = Rect::from_min_max(Pos2::new(row.left() + header_w, row.top()), Pos2::new(row.left() + header_w + cell * steps as f32, row.bottom() - 4.0));
                painter.rect_filled(grid, 6.0, BG);
                let notes = pat.notes(&t.name);
                if is_drum {
                    for s in 0..steps {
                        let r = Rect::from_min_size(Pos2::new(grid.left() + s as f32 * cell + 1.5, grid.top() + 3.0), Vec2::new(cell - 3.0, grid.height() - 6.0));
                        let hit = notes.iter().find(|n| n.start >= s as f32 && n.start < s as f32 + 1.0);
                        let beat = (s / 4) % 2 == 0;
                        let base = if beat { Color32::from_rgb(54, 54, 57) } else { Color32::from_rgb(46, 46, 48) };
                        let fill = match hit {
                            Some(n) => color.gamma_multiply(0.35 + 0.65 * n.vel),
                            None => base,
                        };
                        painter.rect_filled(r, 3.5, fill);
                        if playing_here.map(|ps| ps as usize == s).unwrap_or(false) {
                            painter.rect_stroke(r, 3.5, Stroke::new(1.5_f32, Color32::WHITE));
                        }
                        let resp = ui.interact(r, ui.id().with(("pad", ti, s)), Sense::click());
                        if resp.clicked() {
                            self.selected = ti;
                            self.call("toggle_step", json!({"track": t.name, "step": s, "pattern": pat.name}));
                        }
                    }
                } else {
                    // mini piano roll lane
                    for b in 0..=pat.bars {
                        let x = grid.left() + (b * STEPS_PER_BAR) as f32 * cell;
                        painter.line_segment([Pos2::new(x, grid.top()), Pos2::new(x, grid.bottom())], Stroke::new(1.0_f32, LINE));
                    }
                    let (lo, hi) = notes.iter().fold((127u8, 0u8), |(l, h), n| (l.min(n.pitch), h.max(n.pitch)));
                    let span = (hi.saturating_sub(lo)).max(12) as f32;
                    for n in notes {
                        let y = grid.bottom() - 5.0 - (n.pitch.saturating_sub(lo)) as f32 / span * (grid.height() - 12.0);
                        let r = Rect::from_min_size(Pos2::new(grid.left() + n.start * cell, y - 2.5), Vec2::new((n.len * cell - 1.0).max(2.0), 5.0));
                        painter.rect_filled(r, 2.0, color.gamma_multiply(0.5 + 0.5 * n.vel));
                    }
                    let lane = ui.interact(grid, ui.id().with(("lane", ti)), Sense::click());
                    if lane.clicked() {
                        self.selected = ti;
                    }
                }
                if let Some(ps) = playing_here {
                    let x = grid.left() + ps * cell;
                    painter.line_segment([Pos2::new(x, grid.top()), Pos2::new(x, grid.bottom())], Stroke::new(1.5_f32, Color32::WHITE.gamma_multiply(0.8)));
                }
            }
            if p.tracks.is_empty() {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("Pick a style on the left or ask your AI to start a beat").size(16.0).color(DIM));
                });
            }
        });
    }

    // ---------------- master section ----------------
    fn master(&mut self, ui: &mut egui::Ui, p: &Project) {
        let Some(r) = &self.rendered else {
            ui.centered_and_justified(|ui| ui.label(RichText::new("rendering…").color(DIM)));
            return;
        };
        ui.horizontal(|ui| {
            score_ring(ui, r.report.score);
            ui.vertical(|ui| {
                ui.label(RichText::new("MASTER").size(11.0).color(DIM).strong());
                ui.label(
                    RichText::new(format!(
                        "{:.1} dBFS peak · {:.1} dBFS RMS",
                        r.report.master.peak_dbfs, r.report.master.rms_dbfs
                    ))
                    .size(12.5),
                );
                ui.label(
                    RichText::new(format!(
                        "crest {:.1} dB · stereo {:.2}",
                        r.report.master.crest_db, r.report.stereo_correlation
                    ))
                    .size(12.0)
                    .color(DIM),
                );
                let l = &r.report.loudness;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let lufs_ok = (l.integrated_lufs + 14.0).abs() <= 2.0;
                    stat_chip(
                        ui,
                        "LUFS",
                        &format!("{:.1}", l.integrated_lufs),
                        if lufs_ok { GOOD } else { WARN },
                    );
                    let tp = l.true_peak_dbtp;
                    stat_chip(
                        ui,
                        "TRUE PEAK",
                        &format!("{tp:.1} dBTP"),
                        if tp <= -1.0 {
                            GOOD
                        } else if tp <= 0.0 {
                            WARN
                        } else {
                            HOT
                        },
                    );
                    stat_chip(ui, "LRA", &format!("{:.1}", l.loudness_range_lu), TEXT);
                });
            });
        });
        // waveform
        let (wr, wresp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 66.0), Sense::click());
        let painter = ui.painter_at(wr);
        painter.rect_filled(wr, 8.0, BG);
        let n = r.peaks.len().max(1);
        let pos = self.player.position_secs();
        let frac = (pos / r.mix.seconds.max(0.01)).min(1.0);
        for (i, (lo, hi)) in r.peaks.iter().enumerate() {
            let x = wr.left() + 4.0 + (wr.width() - 8.0) * i as f32 / n as f32;
            let t = i as f32 / n as f32;
            let col = lerp_color(ACCENT, ACCENT2, t);
            let col = if t <= frac {
                col
            } else {
                col.gamma_multiply(0.45)
            };
            let cy = wr.center().y;
            painter.line_segment(
                [Pos2::new(x, cy - hi * 29.0), Pos2::new(x, cy - lo * 29.0)],
                Stroke::new(1.0_f32, col),
            );
        }
        let px = wr.left() + 4.0 + (wr.width() - 8.0) * frac;
        painter.line_segment(
            [Pos2::new(px, wr.top()), Pos2::new(px, wr.bottom())],
            Stroke::new(2.0_f32, Color32::WHITE),
        );
        if wresp.clicked() {
            if let Some(m) = wresp.interact_pointer_pos() {
                self.player
                    .seek((m.x - wr.left()) / wr.width() * r.mix.seconds);
            }
        }
        // spectrum
        let (sr, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 54.0), Sense::hover());
        let painter = ui.painter_at(sr);
        painter.rect_filled(sr, 8.0, BG);
        let bins = r.spectrum.len().max(1);
        let maxdb = r.spectrum.iter().cloned().fold(-200.0f32, f32::max);
        let bw = (sr.width() - 8.0) / bins as f32;
        for (i, db) in r.spectrum.iter().enumerate() {
            let h = ((db - (maxdb - 60.0)) / 60.0).clamp(0.02, 1.0) * (sr.height() - 10.0);
            let x = sr.left() + 4.0 + i as f32 * bw;
            let bar = Rect::from_min_max(
                Pos2::new(x + 0.5, sr.bottom() - 4.0 - h),
                Pos2::new(x + bw - 0.5, sr.bottom() - 4.0),
            );
            vgradient_bar(&painter, bar, ACCENT.gamma_multiply(0.8), ACCENT2);
        }
        for (label, f) in [
            ("60", 60.0f32),
            ("250", 250.0),
            ("1k", 1000.0),
            ("4k", 4000.0),
            ("10k", 10000.0),
        ] {
            let t = (f / 20.0).ln() / 1000f32.ln();
            painter.text(
                Pos2::new(sr.left() + 4.0 + t * (sr.width() - 8.0), sr.top() + 8.0),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(9.5),
                DIM,
            );
        }
        ui.add_space(4.0);
        ui.label(RichText::new("AI MIX NOTES").size(10.5).color(DIM).strong());
        for s in r.report.suggestions.iter().take(2) {
            if ui.available_height() < 30.0 {
                break;
            }
            ui.label(RichText::new(format!("- {s}")).size(11.5).color(WARN));
        }
        let _ = p;
    }

    // ---------------- inspector ----------------
    fn inspector(&mut self, ui: &mut egui::Ui, p: &Project, rev: u64, log: &[LogEntry]) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if let Some(t) = p.tracks.get(self.selected) {
                let color = track_color(&t.name, t.instrument.kind_name());
                let need = self.inspector.as_ref().map(|c| c.track != self.selected || (c.rev != rev && !self.dragging)).unwrap_or(true);
                if need {
                    self.inspector = Some(InspectorCache {
                        track: self.selected,
                        rev,
                        inst: serde_json::to_value(&t.instrument).unwrap_or(json!({})),
                        effects: t.effects.iter().map(|e| serde_json::to_value(e).unwrap_or(json!({}))).collect(),
                    });
                }
                let mut cache = self.inspector.clone().unwrap();
                ui.horizontal(|ui| {
                    let (d, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                    ui.painter().circle_filled(d.center(), 6.0, color);
                    ui.label(RichText::new(&t.name).size(17.0).strong());
                    ui.label(RichText::new(t.instrument.kind_name().to_uppercase()).size(10.5).color(color));
                });
                // mixer row
                card().show(ui, |ui| {
                    ui.label(RichText::new("MIXER").size(10.5).color(DIM).strong());
                    ui.horizontal(|ui| {
                        let mut vol = t.volume_db;
                        let (_, d1) = knob(ui, "volume_db", &mut vol, -40.0, 12.0, false, color);
                        let mut pan = t.pan;
                        let (_, d2) = knob(ui, "pan", &mut pan, -1.0, 1.0, false, color);
                        if (vol - t.volume_db).abs() > 1e-6 || (pan - t.pan).abs() > 1e-6 {
                            self.dragging = true;
                            // live preview of mixer values on the cached project is skipped; commit on release
                            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("mix_live"), (vol, pan)));
                        }
                        if d1 || d2 {
                            let (v, pn) = ui.ctx().data(|d| d.get_temp::<(f32, f32)>(egui::Id::new("mix_live"))).unwrap_or((vol, pan));
                            self.call("set_mixer", json!({"track": t.name, "volume_db": v, "pan": pn}));
                            self.dragging = false;
                        }
                        ui.vertical(|ui| {
                            let mut preset = String::new();
                            egui::ComboBox::from_id_salt("preset_pick").width(130.0).selected_text("load preset…").show_ui(ui, |ui| {
                                for (n, _) in instruments::PRESETS {
                                    ui.selectable_value(&mut preset, n.to_string(), n.replace('_', " "));
                                }
                            });
                            if !preset.is_empty() {
                                self.call("set_instrument", json!({"track": t.name, "preset": preset}));
                            }
                            if ui.button(RichText::new("remove track").size(11.5)).clicked() {
                                self.call("remove_track", json!({"track": t.name}));
                                self.selected = self.selected.saturating_sub(1);
                            }
                        });
                    });
                });
                ui.add_space(6.0);
                card().show(ui, |ui| {
                    ui.label(RichText::new("INSTRUMENT").size(10.5).color(DIM).strong());
                    let kind = t.instrument.kind_name();
                    let mut dragging = false;
                    if param_editor(ui, "inst", kind, &mut cache.inst, color, &mut dragging) {
                        self.call("tweak_instrument", json!({"track": t.name, "params": cache.inst.clone()}));
                        self.dragging = false;
                    } else if dragging {
                        self.dragging = true;
                    }
                });
                ui.add_space(6.0);
                card().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("EFFECTS").size(10.5).color(DIM).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let mut add = String::new();
                            egui::ComboBox::from_id_salt("add_fx").width(110.0).selected_text("+ add effect").show_ui(ui, |ui| {
                                for (n, _) in fx::EFFECT_TYPES {
                                    ui.selectable_value(&mut add, n.to_string(), *n);
                                }
                            });
                            if !add.is_empty() {
                                let mut args = json!({"track": t.name, "type": add});
                                if add == "sidechain" {
                                    let src = p.tracks.iter().find(|x| x.name.contains("kick")).map(|x| x.name.clone()).unwrap_or_else(|| p.tracks[0].name.clone());
                                    args["params"] = json!({"source": src});
                                }
                                self.call("add_effect", args);
                            }
                        });
                    });
                    let mut remove: Option<usize> = None;
                    for (i, fxv) in cache.effects.iter_mut().enumerate() {
                        let ty = fxv["type"].as_str().unwrap_or("fx").to_string();
                        egui::Frame::none().fill(BG).rounding(8.0).inner_margin(8.0).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(ty.to_uppercase()).size(11.5).strong().color(ACCENT2));
                                if let Some(m) = fxv.get("mode").and_then(|s| s.as_str()).filter(|m| *m != "freeverb") {
                                    ui.label(RichText::new(m.replace('_', " ").to_uppercase()).size(10.5).color(ACCENT));
                                }
                                if let Some(id) = fxv.get("id").and_then(|s| s.as_str()) {
                                    ui.label(RichText::new(format!("#{id}")).size(10.0).color(DIM));
                                }
                                if let Some(src) = fxv.get("source").and_then(|s| s.as_str()) {
                                    ui.label(RichText::new(format!("← {src}")).size(11.0).color(DIM));
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("✕").clicked() {
                                        remove = Some(i);
                                    }
                                });
                            });
                            let mut dragging = false;
                            if param_editor(ui, &format!("fx{i}"), &ty, fxv, ACCENT2, &mut dragging) {
                                self.call("tweak_effect", json!({"track": t.name, "index": i, "params": fxv.clone()}));
                                self.dragging = false;
                            } else if dragging {
                                self.dragging = true;
                            }
                        });
                        ui.add_space(4.0);
                    }
                    if let Some(i) = remove {
                        self.call("remove_effect", json!({"track": t.name, "index": i}));
                    }
                    if cache.effects.is_empty() {
                        ui.label(RichText::new("No effects yet").size(11.5).color(DIM));
                    }
                });
                self.inspector = Some(cache);
            }
            ui.add_space(8.0);
            card().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("ACTIVITY").size(10.5).color(DIM).strong());
                    ui.label(RichText::new("every action is a tool call").size(10.0).color(DIM));
                });
                for e in log.iter().take(9) {
                    ui.horizontal(|ui| {
                        let (col, tag) = match e.source.as_str() {
                            "ai" | "mcp" => (ACCENT, "AI"),
                            "you" => (ACCENT2, "YOU"),
                            _ => (DIM, "SYS"),
                        };
                        let (r, _) = ui.allocate_exact_size(Vec2::new(30.0, 16.0), Sense::hover());
                        ui.painter().rect_filled(r, 4.0, col.gamma_multiply(0.25));
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, tag, FontId::proportional(9.5), col);
                        ui.label(RichText::new(&e.tool).monospace().size(11.5).color(if e.ok { TEXT } else { HOT }));
                        let budget = ((236.0 - e.tool.len() as f32 * 7.2) / 5.6).max(6.0) as usize;
                        let mut s: String = e.summary.chars().take(budget).collect();
                        if e.summary.chars().count() > budget {
                            s.push('…');
                        }
                        ui.label(RichText::new(s).size(10.5).color(DIM));
                    });
                }
            });
        });
    }
}

impl eframe::App for Studio {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frames += 1;
        // live link: apply transport commands from MCP and publish the state
        {
            let mut e = self.engine.lock().unwrap();
            e.transport.attached = true;
            let cmds: Vec<crate::engine::TransportCmd> = std::mem::take(&mut e.transport.pending);
            for c in cmds {
                match c {
                    crate::engine::TransportCmd::Play => self.player.play(),
                    crate::engine::TransportCmd::Stop => self.player.stop(),
                    crate::engine::TransportCmd::Seek(s) => self.player.seek(s.max(0.0)),
                }
            }
            e.transport.playing = self.player.is_playing();
            e.transport.position_s = self.player.position_secs();
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        let (p, rev, log, undo) = self.snapshot();
        if self.pattern >= p.patterns.len() {
            self.pattern = 0;
        }
        if self.selected >= p.tracks.len() {
            self.selected = p.tracks.len().saturating_sub(1);
        }
        if !ctx.input(|i| i.pointer.any_down()) {
            self.dragging = false;
        }
        self.pump_render(&p, rev, ctx);

        // keyboard shortcuts (ported table: shortcuts.rs)
        for a in shortcuts::pressed(ctx) {
            self.shortcut(a, &p);
        }

        egui::TopBottomPanel::top("top")
            .exact_height(58.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::Tokens::DARK.toolbar_bg)
                    .inner_margin(egui::Margin::symmetric(14.0, 8.0))
                    .stroke(Stroke::new(1.0_f32, LINE)),
            )
            .show(ctx, |ui| self.top_bar(ui, &p, undo));

        // the mixer takes the browser's space: strips need the width
        if self.view != 1 {
            egui::SidePanel::left("browser")
                .exact_width(232.0)
                .resizable(false)
                .frame(egui::Frame::none().fill(PANEL).inner_margin(12.0))
                .show(ctx, |ui| self.browser(ui, &p));
        }

        egui::SidePanel::right("inspector")
            .exact_width(352.0)
            .resizable(false)
            .frame(egui::Frame::none().fill(PANEL).inner_margin(12.0))
            .show(ctx, |ui| self.inspector(ui, &p, rev, &log));

        egui::TopBottomPanel::bottom("bottom")
            .exact_height(300.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(12.0)
                    .stroke(Stroke::new(1.0_f32, LINE)),
            )
            .show(ctx, |ui| {
                ui.columns(2, |cols| {
                    self.piano_roll(&mut cols[0], &p);
                    self.master(&mut cols[1], &p);
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(14.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("SONG").size(10.5).color(DIM).strong());
                    let names: Vec<String> = p
                        .song_sections()
                        .iter()
                        .map(|s| format!("{} x{}", s.pattern, s.repeats))
                        .collect();
                    ui.label(RichText::new(names.join("  >  ")).size(11.0).color(DIM));
                });
                self.arrangement(ui, &p);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if let Some(v) = tabs(ui, &VIEWS, self.view) {
                        self.view = v;
                    }
                    ui.add_space(10.0);
                    let name = p
                        .patterns
                        .get(self.pattern)
                        .map(|x| x.name.clone())
                        .unwrap_or_default();
                    let info = match self.view {
                        1 => format!(
                            "{} tracks · {} buses · {} sends",
                            p.tracks.len(),
                            p.buses.len(),
                            p.tracks.iter().map(|t| t.sends.len()).sum::<usize>()
                        ),
                        3 => format!(
                            "{} clips · {} PPQ · {:.0} BPM",
                            playlist::placed_clips(&p).len(),
                            crate::timebase::TICKS_PER_QUARTER,
                            p.bpm
                        ),
                        4 => match &p.vocal_map {
                            Some(m) => format!("{} words · {} sung notes · {} chords", m.words.len(), m.notes.len(), m.chords.len()),
                            None => "vocal_to_song builds a song around a sung take".to_string(),
                        },
                        2 => format!(
                            "{} lanes · {:.0} beats · {:.0} BPM",
                            p.automation.len(),
                            p.song_beats(),
                            p.bpm
                        ),
                        _ => format!(
                            "pattern {name} · {} tracks · {} {} · {:.0} BPM",
                            p.tracks.len(),
                            p.key_root,
                            p.scale,
                            p.bpm
                        ),
                    };
                    ui.label(RichText::new(info).size(11.0).color(DIM));
                    if self.rendering.is_some() {
                        ui.spinner();
                    }
                });
                ui.add_space(6.0);
                match self.view {
                    1 => self.mixer(ui, &p),
                    3 => self.playlist_view(ui, &p),
                    2 => self.automation_view(ui, &p),
                    4 => self.vocal_view(ui, &p),
                    _ => self.sequencer(ui, &p),
                }
            });

        // toast
        if let Some((msg, t0, ok)) = &mut self.toast {
            let now = ctx.input(|i| i.time);
            if *t0 == 0.0 {
                *t0 = now;
            }
            let (msg, ok) = (msg.clone(), *ok);
            if now - *t0 < 4.0 {
                egui::Area::new(egui::Id::new("toast"))
                    .anchor(Align2::CENTER_BOTTOM, [0.0, -320.0])
                    .show(ctx, |ui| {
                        egui::Frame::none()
                            .fill(if ok {
                                GOOD.gamma_multiply(0.9)
                            } else {
                                HOT.gamma_multiply(0.9)
                            })
                            .rounding(10.0)
                            .inner_margin(10.0)
                            .show(ui, |ui| {
                                ui.label(RichText::new(msg).color(BG).strong());
                            });
                    });
            } else {
                self.toast = None;
            }
        }

        // screenshot automation
        if let Some(path) = self.screenshot.clone() {
            if self.frames == 2 {
                self.player.seek(p.song_seconds() * 0.38);
                self.player.play();
            }
            let ready = self
                .rendered
                .as_ref()
                .map(|r| r.rev == rev)
                .unwrap_or(false);
            if ready && self.frames > 30 && !self.shot_requested {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
                self.shot_requested = true;
            }
            let shot = ctx.input(|i| {
                i.raw.events.iter().find_map(|e| {
                    if let egui::Event::Screenshot { image, .. } = e {
                        Some(image.clone())
                    } else {
                        None
                    }
                })
            });
            if let Some(img) = shot {
                let [w, h] = img.size;
                let mut buf = Vec::with_capacity(w * h * 4);
                for px in &img.pixels {
                    buf.extend_from_slice(&px.to_array());
                }
                if let Some(im) = image::RgbaImage::from_raw(w as u32, h as u32, buf) {
                    let _ = im.save(&path);
                    eprintln!("screenshot saved to {}", path.display());
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(
            if self.player.is_playing() { 16 } else { 120 },
        ));
    }
}
