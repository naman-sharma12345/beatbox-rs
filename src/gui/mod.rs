//! Beatbox Studio — the native desktop GUI (egui).
//!
//! Every click goes through `Engine::call`, exactly like an MCP client would, so the
//! studio and MCP clients always see the same project. A local control server
//! lets `beatbox mcp --connect` drive this window live.

mod analyze_view;
mod browser_view;
mod channel_rack;
mod console;
mod export_view;
mod fl_automation;
mod fl_playlist;
mod piano;
mod player;
mod playlist;
mod rack_view;
mod shortcuts;
mod theme;
mod views;
mod vocal_view;
mod widgets;
mod workspace;

use crate::analysis::{self, Report};
use crate::engine::Engine;
use crate::instruments;
use crate::project::Project;
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
    /// The project this render is of (the base a live edit splices into).
    project: Arc<Project>,
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
    show_about: bool,
    /// engine revision at the last Save (or when the app opened)
    saved_rev: u64,
    /// "unsaved changes" prompt before File > New project
    confirm_new: bool,
    confirm_quit: bool,
    open_list: bool,
    pending_open: Option<PathBuf>,
    export: export_view::ExportState,
    // view state
    pattern: usize,
    selected: usize,
    // render pipeline
    rendered: Option<Rendered>,
    rendering: Option<(u64, Receiver<Rendered>)>,
    /// Live edit while playing: the affected bars' splice, ahead of the
    /// full render (crate::live_splice).
    live: Option<Receiver<Arc<Mix>>>,
    inspector: Option<InspectorCache>,
    dragging: bool,
    toast: Option<(String, f64, bool)>,
    /// Render WAV runs off the UI thread, like Export.
    render_job: Option<Receiver<Result<Value, String>>>,
    /// Last mix-helper result, shown under the mixer helper row.
    mix_readout: Option<String>,
    mix_job: Option<(u64, Receiver<Result<String, String>>)>,
    mix_readout_revision: Option<u64>,
    // screenshot mode
    screenshot: Option<PathBuf>,
    frames: u64,
    shot_requested: bool,
    /// Performance mode: the pattern looping live, and a launch waiting for
    /// its boundary (Some(None) = back to the song).
    perf: Option<String>,
    perf_next: Option<Option<String>>,
    // central view: 0 sequencer, 1 mixer, 2 automation, 3 playlist
    view: usize,
    auto_sel: Option<(String, String)>,
    piano: piano::PianoState,
    /// vocal view: (sample, waveform peaks, seconds)
    vocal_wave: Option<(String, Vec<(f32, f32)>, f32)>,
    /// browser view: presets, effects, palettes, samples
    browser: browser_view::BrowserState,
    /// analysis panel: file, grid source, background job
    analyze: analyze_view::AnalyzeState,
    /// playlist view: the selected audio clip's stable id
    clip_sel: Option<String>,
    rail_search: String,
    brand: Option<egui::TextureHandle>,
    note_editor: bool,
    workspace: workspace::Workspace,
}

/// The studio is a hand-operated DAW: one view per manual workspace. The
/// one-call production tools (make_beat, produce_song, critique_mix, ...)
/// are MCP tools only and have no place in the GUI.
pub const VIEWS: [&str; 7] = [
    "SEQUENCER",
    "MIXER",
    "AUTOMATION",
    "PLAYLIST",
    "VOCAL",
    "BROWSER",
    "RACK",
];

pub fn run(
    engine: Engine,
    listen: &str,
    project: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    view: Option<String>,
) -> anyhow::Result<()> {
    let view = view
        .and_then(|v| VIEWS.iter().position(|n| n.eq_ignore_ascii_case(&v)))
        .unwrap_or(3);
    let engine = Arc::new(Mutex::new(engine));
    let server_ok = crate::server::spawn(engine.clone(), listen).is_ok();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Beatbox Studio")
            .with_inner_size(
                if screenshot.is_some() && std::env::var_os("BEATBOX_NARROW_SHOT").is_some() {
                    [1100.0, 700.0]
                } else {
                    [1480.0, 920.0]
                },
            )
            .with_min_inner_size([1100.0, 700.0]),
        ..Default::default()
    };
    let listen = listen.to_string();
    eframe::run_native(
        "Beatbox Studio",
        options,
        Box::new(move |cc| {
            apply_theme(&cc.egui_ctx);
            let saved_rev = engine.lock().unwrap().revision;
            Ok(Box::new(Studio {
                saved_rev,
                confirm_new: false,
                confirm_quit: false,
                open_list: false,
                pending_open: None,
                engine,
                player: Player::new(),
                project_path: project,
                listen,
                server_ok,
                show_about: false,
                export: Default::default(),
                pattern: 0,
                selected: 0,
                rendered: None,
                rendering: None,
                live: None,
                inspector: None,
                dragging: false,
                toast: None,
                render_job: None,
                mix_readout: None,
                mix_job: None,
                mix_readout_revision: None,
                screenshot,
                frames: 0,
                shot_requested: false,
                perf: None,
                perf_next: None,
                view,
                auto_sel: None,
                piano: piano::PianoState {
                    snap: 2,
                    ..Default::default()
                },
                vocal_wave: None,
                browser: Default::default(),
                analyze: Default::default(),
                clip_sel: None,
                rail_search: String::new(),
                brand: None,
                note_editor: false,
                workspace: workspace::Workspace::load(&cc.egui_ctx, view),
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

    fn snapshot(&self) -> (Project, u64, (Vec<String>, Vec<String>), (usize, usize)) {
        let e = self.engine.lock().unwrap();
        (e.project.clone(), e.revision, e.history(), e.undo_depth())
    }

    fn pump_render(&mut self, project: &Project, rev: u64, ctx: &egui::Context) {
        // an edit made while playing: its bars are heard as soon as they are
        // spliced in (the player swaps at its next block, crossfaded)
        if let Some(rx) = &self.live {
            if let Ok(m) = rx.try_recv() {
                if self.perf.is_none() && self.perf_next.is_none() {
                    if let Some(r) = &self.rendered {
                        self.player.set_mix(m, r.loop_secs);
                    }
                }
                self.live = None;
            }
        }
        if let Some((want, rx)) = &self.rendering {
            if let Ok(r) = rx.try_recv() {
                let _ = want;
                self.live = None;
                // performance mode keeps looping its pattern; the song comes
                // back on exit
                if self.perf.is_none() && self.perf_next.is_none() {
                    self.player.set_mix(r.mix.clone(), r.loop_secs);
                }
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
            // while the song plays, splice the edited bars into the playing
            // buffer first; the full render below still follows
            let prev =
                (self.player.is_playing() && self.perf.is_none() && self.perf_next.is_none())
                    .then(|| {
                        self.rendered
                            .as_ref()
                            .map(|r| (r.project.clone(), r.mix.clone()))
                    })
                    .flatten();
            let (ltx, lrx) = channel();
            self.live = prev.is_some().then_some(lrx);
            std::thread::spawn(move || {
                if let Some((op, om)) = prev {
                    if let crate::live_splice::Dirty::Steps(a, b) =
                        crate::live_splice::dirty_steps(&op, &p)
                    {
                        if !crate::live_splice::worth_splicing(&op, &p, a, b) {
                            // the full render below is as quick
                        } else if let Ok((m, _)) =
                            crate::live_splice::splice(&op, &p, &bank, &om, a, b)
                        {
                            let _ = ltx.send(Arc::new(m));
                            ctx.request_repaint();
                        }
                    }
                }
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
                        project: Arc::new(p.clone()),
                    });
                    ctx.request_repaint();
                }
            });
            self.rendering = Some((rev, rx));
        }
    }

    fn save(&mut self) {
        // A project without a file gets a name nobody is using yet, so the first
        // Save can never overwrite another beat in the folder.
        let eng = self.engine.clone();
        let path = self.project_path.clone().unwrap_or_else(|| {
            let mut p = PathBuf::from("beat.beatbox.json");
            let mut n = 2;
            while eng.lock().unwrap().resolve(&p.to_string_lossy()).exists() {
                p = PathBuf::from(format!("beat-{n}.beatbox.json"));
                n += 1;
            }
            p
        });
        let result =
            self.engine
                .lock()
                .unwrap()
                .call_from("save_project", &json!({"path": path}), "you");
        match result {
            Ok(_) => {
                self.project_path = Some(path.clone());
                self.saved_rev = self.engine.lock().unwrap().revision;
                self.toast = Some((format!("Saved {}", path.display()), 0.0, true));
            }
            Err(e) => {
                self.toast = Some((
                    format!(
                        "Could not save {}: {e:#}. Your project is still open with your edits; check the folder is writable and try again",
                        path.display()
                    ),
                    0.0,
                    false,
                ))
            }
        }
    }

    fn open_project_file(&mut self, path: PathBuf) {
        let r =
            self.engine
                .lock()
                .unwrap()
                .call_from("load_project", &json!({"path": path}), "you");
        match r {
            Ok(_) => {
                self.project_path = Some(path.clone());
                self.saved_rev = self.engine.lock().unwrap().revision;
                self.toast = Some((format!("Opened {}", path.display()), 0.0, true));
            }
            Err(e) => {
                self.toast = Some((
                    format!(
                        "Could not open {}: {e:#}. Your current project is unchanged",
                        path.display()
                    ),
                    0.0,
                    false,
                ))
            }
        }
    }

    fn saved_projects(&self) -> Vec<PathBuf> {
        let dir = self.engine.lock().unwrap().workdir.clone();
        let mut v: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".beatbox.json"))
            .map(|e| {
                let t = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                (t, PathBuf::from(e.file_name()))
            })
            .collect();
        v.sort_by(|a, b| b.0.cmp(&a.0));
        v.into_iter().map(|x| x.1).collect()
    }

    fn start_new_project(&mut self) {
        self.call(
            "new_project",
            json!({"name": "my beat", "bpm": 120, "patterns": [{"name": "A", "bars": 1}]}),
        );
        // never let Save overwrite the file of the project we just left
        self.project_path = None;
        self.saved_rev = self.engine.lock().unwrap().revision;
        self.toast = Some((
            "New empty project. Add sounds from the Browser (Alt+F8), then click steps in the Channel rack".into(),
            0.0,
            true,
        ));
    }

    fn render_wav(&mut self) {
        if self.render_job.is_some() {
            self.toast = Some(("A render is already running".into(), 0.0, false));
            return;
        }
        let (project, bank, workdir) = {
            let e = self.engine.lock().unwrap();
            (e.project.clone(), e.bank.clone(), e.workdir.clone())
        };
        if project_has_nothing_to_render(&project) {
            self.toast = Some((
                "Nothing to render: add a pattern or clip first".into(),
                0.0,
                false,
            ));
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.render_job = Some(rx);
        self.toast = Some(("Rendering WAV...".into(), 0.0, true));
        std::thread::spawn(move || {
            let mut own = Engine::new(workdir);
            own.project = project;
            own.bank = bank;
            let r = own
                .call_from("render", &json!({}), "you")
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(r);
        });
    }

    fn poll_render_job(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.render_job else { return };
        match rx.try_recv() {
            Ok(r) => {
                self.render_job = None;
                self.toast = Some(match r {
                    Ok(v) => (
                        format!("Rendered {}", v["path"].as_str().unwrap_or("")),
                        0.0,
                        true,
                    ),
                    Err(e) => (e, 0.0, false),
                });
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.render_job = None;
                self.toast = Some(("Render stopped before finishing".into(), 0.0, false));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
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
                    self.workspace.request_view(v);
                }
            }
            Action::Piano => self.workspace.request_pane(workspace::Pane::Notes),
            Action::ResetLayout => self.workspace.request_reset(),
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
        ui.horizontal(|ui| {
            // Narrow windows: shrink the swing slider and key/scale boxes so
            // the whole row fits without scrolling.
            let narrow = ui.ctx().screen_rect().width() < 900.0;
            ui.spacing_mut().slider_width = if narrow { 30.0 } else { 66.0 };
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
            // record: start/stop a take from the default input; the take is
            // placed on the "rec" track where recording started
            let rec = crate::recorder::recording_seconds();
            let tip = match rec {
                Some(s) => format!("Recording ({s:.0} s). Click to stop; the take goes on the 'rec' track."),
                None => "Record from the default audio input. Click again to stop; the take goes on the 'rec' track where recording started.".to_string(),
            };
            if record_button(ui, rec.is_some()).on_hover_text(tip).clicked() {
                match rec {
                    None => self.call("start_recording", json!({})),
                    Some(s) => {
                        let start_beat = if self.player.is_playing() {
                            (self.player.position_secs() - s).max(0.0) * p.bpm / 60.0
                        } else {
                            0.0
                        };
                        self.call("stop_recording", json!({"track": "rec", "start_beat": start_beat}));
                    }
                }
            }
            if rec.is_some() {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
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
                let _len = format_position(
                    sr.samples(f64::from(total)),
                    TimeFormat::MinSecs,
                    sr,
                    &map,
                    FrameRate::Fps30,
                    0,
                );
                let (r, _) = ui.allocate_exact_size(Vec2::new(244.0, 34.0), Sense::hover());
                let pt = ui.painter();
                let main = Rect::from_min_size(r.min, Vec2::new(122.0, 34.0));
                console::counter_box(pt, main, &bb, 17.0);
                pt.text(
                    main.left_top() + Vec2::new(5.0, 2.0),
                    Align2::LEFT_TOP,
                    "BARS|BEATS",
                    FontId::monospace(9.0),
                    DIM,
                );
                let sub = Rect::from_min_size(
                    Pos2::new(main.right() + 4.0, r.top()),
                    Vec2::new(118.0, 34.0),
                );
                console::counter_box(pt, sub, &ms, 13.0);
                pt.text(
                    sub.left_top() + Vec2::new(5.0, 2.0),
                    Align2::LEFT_TOP,
                    "MIN:SECS",
                    FontId::monospace(9.0),
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
                .width(if narrow { 44.0 } else { 56.0 })
                .selected_text(RichText::new(&root).strong())
                .show_ui(ui, |ui| {
                    for n in theory::NOTE_NAMES {
                        ui.selectable_value(&mut root, n.to_string(), n);
                    }
                });
            egui::ComboBox::from_id_salt("scale")
                .width(if narrow { 84.0 } else { 120.0 })
                .selected_text(&scale)
                .show_ui(ui, |ui| {
                    for (n, _) in theory::SCALES {
                        ui.selectable_value(&mut scale, n.to_string(), *n);
                    }
                });
            if root != p.key_root || scale != p.scale {
                self.call("set_key", json!({"root": root, "scale": scale}));
            }
            // Undo and Redo stay in reach; everything else lives under File.
            let mut tail = |ui: &mut egui::Ui, right_to_left: bool| {
                macro_rules! undo_redo {
                    ($ui:expr) => {{
                        if $ui
                            .add_enabled(undo.0 > 0, egui::Button::new("Undo"))
                            .on_hover_text("Undo")
                            .clicked()
                        {
                            self.call("undo", json!({}));
                        }
                        if $ui
                            .add_enabled(undo.1 > 0, egui::Button::new("Redo"))
                            .on_hover_text("Redo")
                            .clicked()
                        {
                            self.call("redo", json!({}));
                        }
                    }};
                }
                macro_rules! file_menu {
                    ($ui:expr) => {{
                        $ui.menu_button("File", |ui| {
                            if ui
                                .button("New project")
                                .on_hover_text("Start an empty beat. Undo brings the previous one back.")
                                .clicked()
                            {
                                if self.engine.lock().unwrap().revision != self.saved_rev {
                                    self.confirm_new = true;
                                } else {
                                    self.start_new_project();
                                }
                                ui.close_menu();
                            }
                            if ui
                                .button("Open project...")
                                .on_hover_text("Reopen a saved beat from this folder")
                                .clicked()
                            {
                                self.open_list = true;
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("Export...").clicked() {
                                self.export.open = true;
                                ui.close_menu();
                            }
                            if ui.button("Render WAV").on_hover_text("Cmd+R").clicked() {
                                self.render_wav();
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("About").clicked() {
                                self.show_about = !self.show_about;
                                ui.close_menu();
                            }
                        });
                    }};
                }
                if right_to_left {
                    file_menu!(ui);
                    if ui.button("Save").on_hover_text("Cmd+S").clicked() {
                        self.save();
                    }

                    // right_to_left adds the rightmost item first
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
                } else {
                    undo_redo!(ui);
                    if ui.button("Save").on_hover_text("Cmd+S").clicked() {
                        self.save();
                    }
                    file_menu!(ui);
                }
            };
            if ui.ctx().screen_rect().width() < 1300.0 {
                tail(ui, false);
            } else {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    tail(ui, true)
                });
            }
        });
    }

    // ---------------- browser ----------------
    fn browser(&mut self, ui: &mut egui::Ui, p: &Project) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                section_label(ui, "PATTERNS");
                for (i, pat) in p.patterns.iter().enumerate() {
                    let sel = i == self.pattern;
                    let r = ui.add(
                        egui::Button::new(
                            RichText::new(format!("{}   {} bars", pat.name, pat.bars)).size(12.5),
                        )
                        .fill(if sel {
                            ACCENT.gamma_multiply(0.35)
                        } else {
                            PANEL2
                        })
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
                ui.add(
                    egui::TextEdit::singleline(&mut self.rail_search)
                        .hint_text("Search instruments")
                        .desired_width(ui.available_width()),
                );
                for family in ["DRUMS", "PERCUSSION", "BASS", "MELODIC"] {
                    egui::CollapsingHeader::new(
                        RichText::new(family).monospace().size(11.0).color(DIM),
                    )
                    .default_open(true)
                    .show(ui, |ui| {
                        for (name, desc) in instruments::PRESETS {
                            let kind = instruments::preset(name)
                                .map(|i| i.kind_name())
                                .unwrap_or("synth");
                            let group = if name.contains("bass") || *name == "808" {
                                "BASS"
                            } else if ["kick", "snare", "clap", "hat", "open_hat"].contains(name) {
                                "DRUMS"
                            } else if kind == "drum" {
                                "PERCUSSION"
                            } else {
                                "MELODIC"
                            };
                            if group != family {
                                continue;
                            }
                            if !name
                                .replace('_', " ")
                                .contains(&self.rail_search.to_lowercase())
                            {
                                continue;
                            }

                            let col = track_color(name, kind);
                            let r = ui
                                .horizontal(|ui| {
                                    let (dot, _) =
                                        ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                                    ui.painter().circle_filled(dot.center(), 4.0, col);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(name.replace('_', " ")).size(12.5),
                                        )
                                        .sense(Sense::click()),
                                    )
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
        // the pattern picker: select / add a pattern, send it to the playlist
        let mut pat = self.pattern;
        let mut pgeo = channel_rack::PatternBarGeo::default();
        // drop target: the selected playlist clip's track and bar (a collision
        // there routes the new clip to the next free track)
        let target = self
            .clip_sel
            .as_ref()
            .and_then(|id| p.playlist.iter().find(|c| &c.id == id))
            .map(|c| (c.lane, c.start_bar));
        let picks = channel_rack::pattern_bar(ui, p, &mut pat, target, &mut pgeo);
        self.pattern = pat;
        for (tool, args) in picks {
            self.call(tool, args);
        }
        if p.patterns.get(self.pattern).is_none() {
            return;
        }
        let playing_here = locate(p, self.player.position_secs())
            .filter(|(pi, _)| *pi == self.pattern)
            .map(|(_, s)| s);
        let seq_top = ui.next_widget_position().y;
        egui::ScrollArea::vertical()
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .auto_shrink([false, false])
            .id_salt("seq")
            .show(ui, |ui| {
                let mut clip = ui.clip_rect();
                clip.min.y = clip.min.y.max(seq_top + 2.0);
                ui.set_clip_rect(clip);
                // the FL-style channel rack (channel_rack.rs): every gesture
                // comes back as one tool call
                let mut sel = channel_rack::RackSel {
                    selected: self.selected,
                    rows: Vec::new(),
                    open_editor: false,
                };
                let actions =
                    channel_rack::channel_rack(ui, p, self.pattern, &mut sel, playing_here);
                self.selected = sel.selected;
                if sel.open_editor {
                    self.note_editor = true;
                }
                for (tool, args) in actions {
                    self.call(tool, args);
                }
                if p.tracks.is_empty() {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("Add a track from Panels > Library, or browse Sounds.")
                                .size(16.0)
                                .color(DIM),
                        );
                    });
                }
            });
    }

    // ---------------- master section ----------------
    fn master(&mut self, ui: &mut egui::Ui, p: &Project) {
        let Some(r) = self
            .rendered
            .as_ref()
            .filter(|r| r.rev == self.engine.lock().unwrap().revision)
        else {
            ui.label(RichText::new("Updating analysis for current project...").color(DIM));
            return;
        };
        if p.tracks.is_empty() || r.report.master.peak_dbfs < -69.0 {
            ui.label(RichText::new("SILENCE - no audio to measure").color(DIM));
            return;
        }
        ui.horizontal(|ui| {
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
        let _ = p;
    }

    // ---------------- inspector ----------------
    fn inspector(
        &mut self,
        ui: &mut egui::Ui,
        p: &Project,
        rev: u64,
        _log: &(Vec<String>, Vec<String>),
    ) {
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
                    ui.horizontal_wrapped(|ui| {
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
                    });
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
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("EFFECTS").size(10.5).color(DIM).strong());
                        ui.vertical(|ui| {
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
        });
    }
}

impl eframe::App for Studio {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.workspace.flush();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frames += 1;
        let midi_live;
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
            match e.transport.pending_audition.take() {
                Some(Some(buf)) => self.player.audition(buf),
                Some(None) => self.player.stop_audition(),
                None => {}
            }
            if let Some(l) = e.transport.pending_launch.take() {
                let song = self.rendered.as_ref().map(|r| (r.mix.clone(), r.loop_secs));
                match (l.mix, song) {
                    (Some(m), _) => {
                        self.player.launch(m, l.loop_secs, l.quantum_secs);
                        self.perf_next = Some(l.pattern);
                    }
                    (None, Some((m, secs))) => {
                        self.player.launch(m, secs, l.quantum_secs);
                        self.perf_next = Some(None);
                    }
                    (None, None) => {
                        self.perf = None;
                        self.perf_next = None;
                    }
                }
            }
            self.player.poll();
            if self.perf_next.is_some() && !self.player.launch_pending() {
                self.perf = self.perf_next.take().unwrap();
                if self.perf.is_none() {
                    // the song render may be newer than the one queued on exit
                    if let Some(r) = &self.rendered {
                        self.player.set_mix(r.mix.clone(), r.loop_secs);
                    }
                }
            }
            e.transport.perf_playing = self.perf.clone();
            e.transport.perf_queued = self
                .perf_next
                .clone()
                .map(|n| n.unwrap_or_else(|| "song".into()));
            // MIDI controller: apply what it sent, then sound the live notes
            crate::midi_in::pump(&mut e);
            for buf in std::mem::take(&mut e.transport.pending_notes) {
                self.player.layer(buf);
            }
            midi_live = e.midi.port.is_some();
            e.transport.auditioning = self.player.auditioning();
            e.transport.playing = self.player.is_playing();
            e.transport.position_s = self.player.position_secs();
        }
        // a connected controller is polled every few ms so playing feels live
        ctx.request_repaint_after(std::time::Duration::from_millis(if midi_live {
            5
        } else {
            250
        }));
        let (p, rev, log, undo) = self.snapshot();
        // closing the window with unsaved edits asks first
        if ctx.input(|i| i.viewport().close_requested())
            && !self.confirm_quit
            && self.engine.lock().unwrap().revision != self.saved_rev
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_quit = true;
        }
        if self.confirm_quit {
            let mut action = 0; // 1 save and quit, 2 quit anyway, 3 stay
            egui::Window::new("Quit without saving?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("This project has changes that are not saved.");
                    ui.label("Undo history is lost when the app closes.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save, then quit").clicked() {
                            action = 1;
                        }
                        if ui.button("Quit without saving").clicked() {
                            action = 2;
                        }
                        if ui.button("Cancel").clicked() {
                            action = 3;
                        }
                    });
                });
            match action {
                1 => {
                    self.save();
                    if self.engine.lock().unwrap().revision == self.saved_rev {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    } else {
                        self.confirm_quit = false;
                    }
                }
                2 => {
                    self.saved_rev = self.engine.lock().unwrap().revision;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                3 => self.confirm_quit = false,
                _ => {}
            }
        }
        if self.open_list {
            let files = self.saved_projects();
            let mut open = true;
            let mut pick: Option<PathBuf> = None;
            egui::Window::new("Open a project")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    if files.is_empty() {
                        ui.label("No saved beats in this folder yet. Press Save first.");
                    }
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .show(ui, |ui| {
                            for f in &files {
                                let name = f.to_string_lossy().replace(".beatbox.json", "");
                                if ui.button(name).clicked() {
                                    pick = Some(f.clone());
                                }
                            }
                        });
                });
            if !open {
                self.open_list = false;
            }
            if let Some(f) = pick {
                self.open_list = false;
                if self.engine.lock().unwrap().revision != self.saved_rev {
                    self.pending_open = Some(f);
                } else {
                    self.open_project_file(f);
                }
            }
        }
        if let Some(target) = self.pending_open.clone() {
            let mut action = 0; // 1 save first, 2 discard, 3 cancel
            egui::Window::new("Open another project?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("This project has changes that are not saved.");
                    ui.label("Undo history is lost when you open another project.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save, then open").clicked() {
                            action = 1;
                        }
                        if ui.button("Discard changes").clicked() {
                            action = 2;
                        }
                        if ui.button("Cancel").clicked() {
                            action = 3;
                        }
                    });
                });
            match action {
                1 => {
                    self.save();
                    self.pending_open = None;
                    if self.engine.lock().unwrap().revision == self.saved_rev {
                        self.open_project_file(target);
                    }
                }
                2 => {
                    self.pending_open = None;
                    self.open_project_file(target);
                }
                3 => self.pending_open = None,
                _ => {}
            }
        }
        if self.confirm_new {
            let mut action = 0; // 1 save first, 2 discard, 3 cancel
            egui::Window::new("Start a new project?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("This project has changes that are not saved.");
                    ui.label("Undo only works until you close the app.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save, then start new").clicked() {
                            action = 1;
                        }
                        if ui.button("Discard changes").clicked() {
                            action = 2;
                        }
                        if ui.button("Cancel").clicked() {
                            action = 3;
                        }
                    });
                });
            match action {
                1 => {
                    self.save();
                    // only continue if the save went through
                    if self.engine.lock().unwrap().revision == self.saved_rev {
                        self.confirm_new = false;
                        self.start_new_project();
                    } else {
                        self.confirm_new = false;
                    }
                }
                2 => {
                    self.confirm_new = false;
                    self.start_new_project();
                }
                3 => self.confirm_new = false,
                _ => {}
            }
        }
        if self.show_about {
            let mut open = true;
            egui::Window::new("About Beatbox Studio")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!("Beatbox Studio {}", env!("CARGO_PKG_VERSION")));
                    let port = self.listen.rsplit(':').next().unwrap_or("").to_string();
                    ui.label(if self.server_ok {
                        format!("Remote control on port {port}")
                    } else {
                        "Remote control is off".to_string()
                    });
                });
            self.show_about = open;
        }
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
        self.poll_render_job(ctx);

        // keyboard shortcuts (ported table: shortcuts.rs)
        for a in shortcuts::pressed(ctx) {
            self.shortcut(a, &p);
        }

        self.workspace_ui(ctx, &p, rev, &log, undo);

        self.export_window(ctx);

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

#[cfg(test)]
mod manual_ui_tests;

/// True only when no pattern, arrangement section, playlist clip, audio clip
/// or vocal map exists, so a render could only produce silence.
fn project_has_nothing_to_render(p: &crate::project::Project) -> bool {
    p.patterns.is_empty()
        && p.arrangement.is_empty()
        && p.playlist.is_empty()
        && p.audio_clips.is_empty()
        && p.vocal_map.is_none()
}
