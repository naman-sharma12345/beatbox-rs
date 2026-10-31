//! Beatbox Studio — the native desktop GUI (egui).
//!
//! Every click goes through `Engine::call`, exactly like an MCP client would, so the
//! studio and MCP clients always see the same project. A local control server
//! lets `beatbox mcp --connect` drive this window live.

mod analyze_view;
mod audio_check;
mod browser_view;
mod channel_rack;
mod console;
mod export_view;
/// Resolve a whole-section export interval without changing the project.
pub use export_view::apply_section_range as section_export_range;
/// Describe and validate an export range without creating files.
pub use export_view::range_preview as export_range_preview;
mod fl_automation;
mod fl_playlist;
mod piano;
mod player;
mod playlist;
mod rack_view;
mod render_job;
mod rhythm_trial;
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
use render_job::{Completion, Key as RenderKey, Message as RenderMessage, Source as RenderSource};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use widgets::*;

struct Rendered {
    rev: u64,
    generation: u64,
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

/// A live-launch recording in progress. Transition timestamps come from the
/// audio callback (or virtual-player poll), not GUI observation time.
pub(crate) struct PerfRec {
    /// (bar, pattern) segments; an empty pattern marks a return to the song,
    /// which only closes the previous segment.
    segs: Vec<(u32, String)>,
    /// Take length in player seconds up to the last landing.
    take_last: f32,
    /// Player absolute time at the last event.
    abs_last: f64,
    /// Absolute landing time of the queued launch, consumed when it lands.
    pending: Option<f64>,
    /// Song-loop position when Rec was pressed; anchors the take to song bars.
    start_pos: f32,
    /// Set when the transport stopped mid-take: the take length at that
    /// moment. Timing is no longer guaranteed, so nothing further records.
    frozen: Option<f32>,
    bpm: f32,
    invalid: bool,
    seek_revision: u64,
    project_revision: u64,
}

impl PerfRec {
    /// Current take position in song bars.
    fn bar_now(&self, bpm: f32, abs_now: f64) -> f32 {
        let end = self
            .frozen
            .unwrap_or(self.take_last + (abs_now - self.abs_last).max(0.0) as f32);
        (self.start_pos + end) * bpm / 240.0
    }
}

/// Fileless projects need protection even before their first edit.
fn needs_leave_warning(has_file: bool, revision: u64, saved_revision: u64) -> bool {
    !has_file || revision != saved_revision
}

/// A matching revision alone cannot turn a failed save into permission to leave.
fn save_allows_leave(saved: bool, has_file: bool, revision: u64, saved_revision: u64) -> bool {
    saved && !needs_leave_warning(has_file, revision, saved_revision)
}

/// Pending confirmation is a veto, not permission to close the root window.
#[derive(Default)]
struct QuitCloseState {
    pending: bool,
    approved_once: bool,
}

// Dialog choices grant approval; completed screenshot capture has its own receipt.
// Ordinary saves and capture environment settings are not approval.
enum QuitChoice {
    Save(bool),
    Discard,
    Cancel,
}

impl QuitCloseState {
    /// Return whether this incoming root close must be cancelled.
    fn incoming_close(&mut self, requested: bool, needs_warning: bool) -> bool {
        if !requested {
            return false;
        }
        if std::mem::take(&mut self.approved_once) {
            return false;
        }
        if self.pending || needs_warning {
            self.pending = true;
            return true;
        }
        false
    }

    /// Only opted-in capture with all required writes completed may close unsaved.
    fn approve_completed_capture(
        &mut self,
        screenshot_mode: bool,
        receipt: Option<ScreenshotPersisted>,
    ) -> bool {
        if !screenshot_mode || receipt.is_none() {
            return false;
        }
        self.pending = false;
        self.approved_once = true;
        true
    }

    /// Return whether to emit Close, with a permit for its resulting event.
    fn resolve(&mut self, choice: QuitChoice) -> bool {
        self.pending = false;
        self.approved_once = matches!(choice, QuitChoice::Save(true) | QuitChoice::Discard);
        self.approved_once
    }
}

/// Created only after the PNG and any required companion manifest were written.
struct ScreenshotPersisted;

fn persist_screenshot(
    write_png: impl FnOnce() -> Result<(), String>,
    write_manifest: impl FnOnce() -> Result<(), String>,
) -> Result<ScreenshotPersisted, String> {
    write_png()?;
    write_manifest()?;
    Ok(ScreenshotPersisted)
}

/// Capture failures must not request a guarded Close or report successful exit.
fn fail_screenshot(error: &str) -> ! {
    eprintln!("screenshot FAILED: {error}");
    std::process::exit(1)
}

/// Called while the caller holds the engine lock, including revision capture.
fn save_project_revision(engine: &mut Engine, path: &std::path::Path) -> anyhow::Result<u64> {
    engine.call_from("save_project", &json!({"path": path}), "you")?;
    Ok(engine.revision)
}

/// Describe the same file/revision state used by New/Open/Quit prompts.
fn save_state(has_file: bool, revision: u64, saved_revision: u64) -> &'static str {
    if !has_file {
        "Not saved"
    } else if needs_leave_warning(has_file, revision, saved_revision) {
        "Unsaved changes"
    } else {
        "Saved"
    }
}

/// Capture a fresh checkpoint without overwriting any snapshot.
/// Existing snapshot/restore_snapshot MCP actions remain the source of truth.
pub fn capture_mix_checkpoint(engine: &mut Engine) -> anyhow::Result<String> {
    let mut i = 1u64;
    loop {
        let name = format!("mix_checkpoint_{i}");
        if !engine
            .snapshots
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case(&name))
        {
            engine.call_from("snapshot", &json!({"name":name,"note":"Mixer checkpoint; Save writes the snapshots companion file"}), "you")?;
            return Ok(name);
        }
        i = i
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("No checkpoint names available"))?;
    }
}

/// A notice from an earlier loaded project must not label the new song.
pub fn project_notice_is_stale(seen: u64, current: u64) -> bool {
    seen != current
}

type PreviewAudio = Arc<(Vec<f32>, Vec<f32>)>;
type MixComparisonResult = Result<(Value, PreviewAudio), String>;
struct MixComparison {
    revision: u64,
    generation: u64,
    checkpoint: String,
    start_s: f32,
    length_s: f32,
    value: Value,
    audio: PreviewAudio,
}
struct MixComparisonJob {
    revision: u64,
    generation: u64,
    checkpoint: String,
    start_s: f32,
    length_s: f32,
    receiver: Receiver<MixComparisonResult>,
}

/// Capture both versions without restoring the project or touching transport.
fn checkpoint_comparison_engine(engine: &Engine, checkpoint: &str) -> anyhow::Result<Engine> {
    let snapshot = engine
        .snapshots
        .iter()
        .find(|(name, _)| name == checkpoint)
        .ok_or_else(|| anyhow::anyhow!("Choose an existing checkpoint"))?
        .clone();
    let mut own = Engine::new(engine.workdir.clone());
    own.project = engine.project.clone();
    own.bank = engine.bank.clone();
    own.snapshots = vec![snapshot];
    Ok(own)
}
fn render_checkpoint_comparison(
    mut own: Engine,
    checkpoint: &str,
    start_s: f32,
    length_s: f32,
) -> anyhow::Result<(Value, PreviewAudio)> {
    if !start_s.is_finite()
        || !(0.0..=86400.0).contains(&start_s)
        || !length_s.is_finite()
        || !(1.0..=120.0).contains(&length_s)
    {
        anyhow::bail!("Start must be 0..86400 seconds and length 1..120 seconds");
    }
    // ab_listen also accepts files, which take priority over snapshot names.
    // Use a private non-file alias so an unrelated file cannot replace A.
    let mut alias = "__mixer_checkpoint_comparison".to_string();
    while own.resolve(&alias).is_file() {
        alias.push('_');
    }
    let snapshot = own
        .snapshots
        .iter_mut()
        .find(|(name, _)| name == checkpoint)
        .ok_or_else(|| anyhow::anyhow!("Checkpoint is no longer available"))?;
    snapshot.0 = alias.clone();
    let mut value = own.call(
        "ab_listen",
        &json!({"a": alias, "b":"current", "start_s":start_s, "length_s":length_s, "play":true}),
    )?;
    value["A"] = json!(checkpoint);
    let audio = own
        .transport
        .pending_audition
        .take()
        .flatten()
        .ok_or_else(|| anyhow::anyhow!("Comparison produced no preview"))?;
    Ok((value, audio))
}
fn comparison_note(value: &Value) -> &'static str {
    if value["facts"]["A"]["seconds"] != value["facts"]["B"]["seconds"] {
        "Different rendered lengths; not a like-for-like excerpt. Choose a shorter interval available in both versions."
    } else if value["loudness_matched"].as_bool() == Some(true) {
        "Level matched without boosting. Choose by listening, not loudness."
    } else {
        "A side is silent or below the loudness gate; not level matched."
    }
}

fn comparison_is_current(revision: u64, generation: u64, engine: &Engine) -> bool {
    revision == engine.revision && generation == engine.project_load_generation
}

fn comparison_ready_for(
    result: &MixComparison,
    checkpoint: &str,
    start_s: f32,
    length_s: f32,
    engine: &Engine,
) -> bool {
    result.checkpoint == checkpoint
        && result.start_s == start_s
        && result.length_s == length_s
        && comparison_is_current(result.revision, result.generation, engine)
}
fn play_checkpoint_comparison(
    result: &MixComparison,
    checkpoint: &str,
    start_s: f32,
    length_s: f32,
    engine: &mut Engine,
) -> bool {
    if !comparison_ready_for(result, checkpoint, start_s, length_s, engine) {
        return false;
    }
    engine
        .transport
        .pending
        .push(crate::engine::TransportCmd::Stop);
    engine.transport.pending_audition = Some(Some(result.audio.clone()));
    true
}
fn stop_comparison_preview(engine: &mut Engine) {
    engine.transport.pending_audition = Some(None);
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
    seen_project_load: u64,
    /// "unsaved changes" prompt before File > New project
    confirm_new: bool,
    quit_close: QuitCloseState,
    mixer_routing: bool,
    open_list: bool,
    pending_open: Option<PathBuf>,
    export: export_view::ExportState,
    // view state
    pattern: usize,
    selected: usize,
    // render pipeline
    rendered: Option<Rendered>,
    rendering: Option<(RenderKey, Receiver<RenderMessage<Rendered>>)>,
    render_state: render_job::State,
    /// Live edit while playing: the affected bars' splice, ahead of the
    /// full render (crate::live_splice).
    live: Option<(RenderKey, Receiver<Arc<Mix>>)>,
    /// A provisional splice may need to fall back to the last completed mix.
    live_applied: bool,
    inspector: Option<InspectorCache>,
    dragging: bool,
    toast: Option<(String, f64, bool)>,
    /// Render WAV runs off the UI thread, like Export.
    render_job: Option<Receiver<Result<Value, String>>>,
    /// Last mix-helper result, shown under the mixer helper row.
    mix_readout: Option<String>,
    mix_job: Option<(u64, Receiver<Result<String, String>>)>,
    mix_readout_revision: Option<u64>,
    audio_check: audio_check::State,
    mix_comparison: Option<MixComparison>,
    mix_comparison_job: Option<MixComparisonJob>,
    comparison_start_s: f32,
    comparison_length_s: f32,
    // screenshot mode
    screenshot: Option<PathBuf>,
    native_capture: Option<audio_check::NativeCapture>,
    arrangement_capture: Option<ArrangementCapture>,
    feel_capture: Option<FeelCapture>,
    frames: u64,
    shot_requested: bool,
    /// Performance mode: the pattern looping live, and a launch waiting for
    /// its boundary (Some(None) = back to the song).
    perf: Option<String>,
    perf_next: Option<Option<String>>,
    /// PERFORM recording in progress.
    perf_rec: Option<PerfRec>,
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
    marker_name: String,
    tempo_edit_bpm: f32,
    tempo_edit_ramp: bool,
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

/// Bounded appearance-only capture. No project/transport command is issued here.
struct ArrangementCapture {
    case: String,
    id: Option<String>,
    action_hash: String,
    project_bytes: Vec<u8>,
    revision: u64,
    generation: u64,
    history: (usize, usize),
    seek_revision: u64,
    state: Option<Value>,
    started: std::time::Instant,
}

impl ArrangementCapture {
    fn from_environment(
        screenshot: bool,
        view: Option<&str>,
        e: &Engine,
    ) -> anyhow::Result<Option<Self>> {
        let Some(case) = std::env::var_os("BEATBOX_ARRANGEMENT_CAPTURE") else {
            return Ok(None);
        };
        let case = case
            .into_string()
            .map_err(|_| anyhow::anyhow!("Non-UTF8 arrangement selector"))?;
        anyhow::ensure!(
            matches!(
                case.as_str(),
                "shared" | "independent" | "long-overview" | "long-reveal" | "tempo"
            ),
            "Unknown arrangement selector"
        );
        anyhow::ensure!(
            screenshot && std::env::var("BEATBOX_NO_LISTEN").as_deref() == Ok("1"),
            "Arrangement capture requires screenshot and NO_LISTEN=1"
        );
        anyhow::ensure!(
            std::env::var_os("BEATBOX_AUDIO_SCENARIO").is_none()
                && std::env::var_os("BEATBOX_AUDIO_SCALE").is_none(),
            "Mixed audio/arrangement capture"
        );
        anyhow::ensure!(
            view.is_none_or(|v| v.eq_ignore_ascii_case("playlist")),
            "Arrangement capture requires playlist view"
        );
        let id = std::env::var_os("BEATBOX_ARRANGEMENT_CLIP_ID")
            .map(|v| {
                v.into_string()
                    .map_err(|_| anyhow::anyhow!("Non-UTF8 clip ID"))
            })
            .transpose()?;
        let action_hash = std::env::var("BEATBOX_ARRANGEMENT_ACTION_HASH")?;
        anyhow::ensure!(
            action_hash.len() == 64 && action_hash.bytes().all(|b| b.is_ascii_hexdigit()),
            "Missing/invalid action hash"
        );
        Ok(Some(Self {
            case,
            id,
            action_hash,
            project_bytes: serde_json::to_vec(&e.project)?,
            revision: e.revision,
            generation: e.project_load_generation,
            history: e.undo_depth(),
            seek_revision: 0,
            state: None,
            started: std::time::Instant::now(),
        }))
    }

    fn check(&self, studio: &Studio, ctx: &egui::Context) -> Result<Value, String> {
        if self.started.elapsed().as_secs() > 90 {
            return Err("Arrangement capture timeout".into());
        }
        let e = studio.engine.lock().unwrap();
        if e.revision != self.revision
            || e.project_load_generation != self.generation
            || e.undo_depth() != self.history
            || serde_json::to_vec(&e.project).map_err(|e| e.to_string())? != self.project_bytes
        {
            return Err("Arrangement project/revision/generation/history drift".into());
        }
        if studio.player.is_playing()
            || studio.player.auditioning()
            || studio.player.launch_pending()
            || studio.perf.is_some()
            || studio.perf_next.is_some()
            || studio.perf_rec.is_some()
            || e.transport.playing
            || e.transport.auditioning
            || !e.transport.pending.is_empty()
            || e.transport.pending_audition.is_some()
            || e.transport.pending_launch.is_some()
            || !e.transport.pending_notes.is_empty()
            || e.transport.perf_playing.is_some()
            || e.transport.perf_queued.is_some()
        {
            return Err("Arrangement transport drift".into());
        }
        // Preserve the pre-existing ordinary screenshot seek at frame 2.
        let sought = studio.frames >= 2;
        let seconds = if sought {
            e.project.song_seconds() * 0.38
        } else {
            0.0
        };
        if studio.player.seek_revision != self.seek_revision + u64::from(sought)
            || !studio.player.position_secs().is_finite()
            || (studio.player.position_secs() - seconds).abs() > 0.01
            || (studio.frames > 2 && (e.transport.position_s - seconds).abs() > 0.01)
            || studio.clip_sel != self.id
        {
            return Err("Arrangement cursor/selection drift".into());
        }
        let state = fl_playlist::capture_view(
            ctx,
            &e.project,
            self.revision,
            self.generation,
            &self.case,
            self.id.as_deref(),
            false,
        )?;
        if self.state.as_ref() != Some(&state) {
            return Err("Arrangement initialized view changed".into());
        }
        Ok(state)
    }

    fn manifest(
        &self,
        studio: &Studio,
        ctx: &egui::Context,
        path: &std::path::Path,
        pixels: [usize; 2],
    ) -> Result<(), String> {
        let state = self.check(studio, ctx)?;
        if !studio.shot_requested
            || studio
                .rendered
                .as_ref()
                .is_none_or(|r| r.rev != self.revision || r.generation != self.generation)
        {
            return Err("Arrangement capture not requested or render identity mismatch".into());
        }
        let narrow = std::env::var_os("BEATBOX_NARROW_SHOT").is_some();
        let expected = if narrow { [1100, 700] } else { [1480, 920] };
        let logical = ctx.screen_rect().size();
        if pixels != expected
            || (ctx.pixels_per_point() - 1.0).abs() > 0.01
            || (logical.x - expected[0] as f32).abs() > 1.1
            || (logical.y - expected[1] as f32).abs() > 1.1
        {
            return Err("Arrangement dimensions/ppp mismatch (only native1x supported)".into());
        }
        let e = studio.engine.lock().unwrap();
        let doc = json!({"schema":1,"selector":self.case,
            "state_origin":["engine_json_prepared","capture_initialized_view","no_native_input"],
            "initial_view_state":self.state,"final_view_state":state,
            "project_serialized_sha256":crate::palette::sha256_hex(&self.project_bytes),
            "revision":self.revision,"load_generation":self.generation,"history_depth":self.history,
            "render_revision":studio.rendered.as_ref().map(|r|r.rev),
            "render_load_generation":studio.rendered.as_ref().map(|r|r.generation),
            "action_log_sha256":self.action_hash,"build_sha":option_env!("BEATBOX_BUILD_SHA").unwrap_or("unknown"),
            "viewport_pixels":pixels,"logical_viewport":[logical.x,logical.y],"pixels_per_point":ctx.pixels_per_point(),
            "playing":false,"autoplay":false,"seek_origin":"existing ordinary frame2 38%-duration seek",
            "seek_revision":studio.player.seek_revision,"seconds":studio.player.position_secs(),
            "beat":playlist::marker_beat(&e.project,studio.player.position_secs()),
            "status":"captured_uninspected","native_input":false,
            "limitations":"Appearance only; toolbar activation, stale draft/drag, Studio focus and ordinary Close UNRUN; bounds require image inspection; native2x UNRUN"});
        std::fs::write(
            path.with_extension("json"),
            serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
}

/// Four fixed appearance shots, sharing the ordinary screenshot receipt lifecycle.
struct FeelCapture {
    case: String,
    action_hash: String,
    fixture_hash: String,
    project_bytes: Vec<u8>,
    revision: u64,
    generation: u64,
    history: (usize, usize),
    full_log: String,
    transport: crate::engine::Transport,
    seek_revision: Option<u64>,
    state: Option<Value>,
    started: std::time::Instant,
}

impl FeelCapture {
    // Exact screenshot-tool contract: typed Project -> serde_json::to_string.
    // The saved Engine JSON is a separate immutable byte identity, not the
    // temporary launcher file. No Value/Python numeric canonicalization here.
    const PROJECT_CONTRACT: &str = "project_json_v1";

    fn verify_project_provenance(
        contract: &str,
        fixture: &[u8],
        fixture_hash: &str,
        launcher: &[u8],
        loaded: &Project,
    ) -> anyhow::Result<Vec<u8>> {
        anyhow::ensure!(
            contract == Self::PROJECT_CONTRACT,
            "Unknown feel project contract"
        );
        anyhow::ensure!(
            fixture_hash.len() == 64 && fixture_hash.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid feel fixture hash"
        );
        anyhow::ensure!(
            crate::palette::sha256_hex(fixture) == fixture_hash,
            "Feel fixture hash mismatch"
        );
        let saved: Project = serde_json::from_slice(fixture)?;
        let expected = serde_json::to_string(&saved)?.into_bytes();
        anyhow::ensure!(launcher == expected, "Feel typed launcher project mismatch");
        // Independently bind the actual loaded Studio Project to those bytes.
        // Reject any load normalization/drift rather than silently rebasing.
        let project_bytes = serde_json::to_vec(loaded)?;
        anyhow::ensure!(project_bytes == launcher, "Feel loaded project mismatch");
        Ok(project_bytes)
    }

    fn validate_mode(
        case: &str,
        screenshot: bool,
        no_listen: bool,
        view: Option<&str>,
        mixed: bool,
    ) -> Result<(), String> {
        if !matches!(case, "selected" | "all-offsets" | "boundary-blocked") {
            return Err("Unknown feel selector".into());
        }
        if !screenshot
            || !no_listen
            || view.is_none_or(|v| !v.eq_ignore_ascii_case("sequencer"))
            || mixed
        {
            return Err("Feel capture requires screenshot, NO_LISTEN=1, explicit sequencer view and no mixed selectors".into());
        }
        Ok(())
    }

    fn from_environment(
        screenshot: bool,
        view: Option<&str>,
        e: &Engine,
        project: Option<&std::path::Path>,
    ) -> anyhow::Result<Option<Self>> {
        let Some(case) = std::env::var_os("BEATBOX_FEEL_CAPTURE") else {
            return Ok(None);
        };
        let case = case
            .into_string()
            .map_err(|_| anyhow::anyhow!("Non-UTF8 feel selector"))?;
        let mixed = [
            "BEATBOX_AUDIO_SCENARIO",
            "BEATBOX_AUDIO_SCALE",
            "BEATBOX_ARRANGEMENT_CAPTURE",
            "BEATBOX_ARRANGEMENT_CLIP_ID",
            "BEATBOX_ARRANGEMENT_ACTION_HASH",
        ]
        .iter()
        .any(|key| std::env::var_os(key).is_some());
        Self::validate_mode(
            &case,
            screenshot,
            std::env::var("BEATBOX_NO_LISTEN").as_deref() == Ok("1"),
            view,
            mixed,
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        let action_hash = std::env::var("BEATBOX_FEEL_ACTION_HASH")?;
        let fixture_hash = std::env::var("BEATBOX_FEEL_FIXTURE_HASH")?;
        for hash in [&action_hash, &fixture_hash] {
            anyhow::ensure!(
                hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                "Invalid feel provenance hash"
            );
        }
        let path =
            project.ok_or_else(|| anyhow::anyhow!("Feel capture requires loaded project file"))?;
        let fixture_path = std::env::var_os("BEATBOX_FEEL_FIXTURE_PATH")
            .ok_or_else(|| anyhow::anyhow!("Feel capture requires original fixture path"))?;
        let contract = std::env::var("BEATBOX_FEEL_PROJECT_CONTRACT")?;
        let project_bytes = Self::verify_project_provenance(
            &contract,
            &std::fs::read(std::path::Path::new(&fixture_path))?,
            &fixture_hash,
            &std::fs::read(path)?,
            &e.project,
        )?;
        anyhow::ensure!(
            !e.transport.attached && !e.transport.playing && e.transport.position_s == 0.0,
            "Non-fresh feel transport"
        );
        Ok(Some(Self {
            case,
            action_hash,
            fixture_hash,
            project_bytes,
            revision: e.revision,
            generation: e.project_load_generation,
            history: e.undo_depth(),
            full_log: format!("{:?}", e.log),
            transport: e.transport.clone(),
            seek_revision: None,
            state: None,
            started: std::time::Instant::now(),
        }))
    }

    fn check_engine(&self, e: &Engine) -> Result<(), String> {
        if self.started.elapsed() >= std::time::Duration::from_secs(90) {
            return Err("Feel capture timeout".into());
        }
        if e.revision != self.revision
            || e.project_load_generation != self.generation
            || e.undo_depth() != self.history
            || format!("{:?}", e.log) != self.full_log
            || serde_json::to_vec(&e.project).map_err(|e| e.to_string())? != self.project_bytes
        {
            return Err("Feel project/revision/generation/history/full-log drift".into());
        }
        Ok(())
    }

    // Called on the first update, before any transport processing or frame2 seek.
    // Never refresh this original stamp when native focus arrives later.
    fn freeze_initial_seek(&mut self, frame: u64, revision: u64) {
        if frame == 1 && self.seek_revision.is_none() {
            self.seek_revision = Some(revision);
        }
    }

    fn initialize_pending(
        &mut self,
        e: &Engine,
        ctx: &egui::Context,
        piano: &mut piano::PianoState,
        frame_stamp: (u64, u64),
    ) -> Result<Option<(usize, usize)>, String> {
        self.check_engine(e)?;
        if self.state.is_some()
            || !ctx.input(|i| i.focused)
            || frame_stamp != (self.revision, self.generation)
        {
            return Ok(None);
        }
        if ctx.input(|i| i.pointer.any_down()) {
            return Err("Feel initialization held pointer".into());
        }
        let owner = piano.initialize_feel_capture(
            &e.project,
            (self.revision, self.generation),
            &self.case,
        )?;
        self.state = Some(piano.observe_feel_capture(
            &e.project,
            (self.revision, self.generation),
            (owner.0, owner.1, 0),
            &self.case,
        )?);
        Ok(Some(owner))
    }

    fn check(&self, studio: &Studio, ctx: &egui::Context) -> Result<Value, String> {
        let e = studio.engine.lock().unwrap();
        self.check_engine(&e)?;
        let sought = studio.frames >= 2;
        let seconds = if sought {
            e.project.song_seconds() * 0.38
        } else {
            0.0
        };
        // Only ordinary GUI attachment and the existing frame2 seek may differ.
        let mut expected_transport = self.transport.clone();
        expected_transport.attached = true;
        expected_transport.position_s = if studio.frames > 2 { seconds } else { 0.0 };
        if format!("{:?}", e.transport) != format!("{:?}", expected_transport)
            || studio.player.is_playing()
            || studio.player.auditioning()
            || studio.player.launch_pending()
            || studio.perf.is_some()
            || studio.perf_next.is_some()
            || studio.perf_rec.is_some()
            || !e.transport.pending.is_empty()
            || e.transport.pending_audition.is_some()
            || e.transport.pending_launch.is_some()
            || !e.transport.pending_notes.is_empty()
            || e.transport.perf_playing.is_some()
            || e.transport.perf_queued.is_some()
            || Some(studio.player.seek_revision)
                != self.seek_revision.map(|r| r + u64::from(sought))
            || !studio.player.position_secs().is_finite()
            || (studio.player.position_secs() - seconds).abs() > 0.01
            || studio.dragging
            || ctx.input(|i| i.pointer.any_down())
            || studio.view != 0
            || (self.state.is_some() && studio.seen_project_load != self.generation)
        {
            return Err("Feel transport/view/gesture drift".into());
        }
        // Pending startup still checks every immutable/transport guard above.
        // Only the draft observer waits for genuine focus and a coherent frame.
        if self.state.is_none() {
            return Ok(Value::Null);
        }
        let state = studio.piano.observe_feel_capture(
            &e.project,
            (self.revision, self.generation),
            (studio.selected, studio.pattern, studio.view),
            &self.case,
        )?;
        if self.state.as_ref() != Some(&state) {
            return Err("Feel frozen draft/view/scope drift".into());
        }
        Ok(state)
    }

    fn manifest(
        &self,
        studio: &Studio,
        ctx: &egui::Context,
        path: &std::path::Path,
        pixels: [usize; 2],
    ) -> Result<(), String> {
        let state = self.check(studio, ctx)?;
        if self.state.is_none()
            || !studio.shot_requested
            || studio.frames <= 30
            || studio
                .rendered
                .as_ref()
                .is_none_or(|r| r.rev != self.revision || r.generation != self.generation)
        {
            return Err("Feel capture not requested/ready with frozen render identity".into());
        }
        let expected = if std::env::var_os("BEATBOX_NARROW_SHOT").is_some() {
            [1100, 700]
        } else {
            [1480, 920]
        };
        let logical = ctx.screen_rect().size();
        if pixels != expected
            || (ctx.pixels_per_point() - 1.0).abs() > 0.01
            || (logical.x - expected[0] as f32).abs() > 1.1
            || (logical.y - expected[1] as f32).abs() > 1.1
        {
            return Err("Feel dimensions/ppp mismatch (native1x only)".into());
        }
        let doc = json!({"schema":1,"selector":self.case,
            "state_origin":["engine_json_prepared","capture_initialized_view","no_native_input"],
            "initial_view_state":self.state,"final_view_state":state,
            "project_serialized_sha256":crate::palette::sha256_hex(&self.project_bytes),
            "revision":self.revision,"load_generation":self.generation,"history_depth":self.history,
            "full_log_sha256":crate::palette::sha256_hex(self.full_log.as_bytes()),
            "transport_baseline":format!("{:?}",self.transport),"transport_final":format!("{:?}",studio.engine.lock().unwrap().transport),
            "render_revision":studio.rendered.as_ref().map(|r|r.rev),"render_load_generation":studio.rendered.as_ref().map(|r|r.generation),
            "request_ready":true,"shot_requested":studio.shot_requested,
            "build_sha":option_env!("BEATBOX_BUILD_SHA").unwrap_or("unknown"),
            "fixture_sha256":self.fixture_hash,"action_log_sha256":self.action_hash,
            "project_contract":Self::PROJECT_CONTRACT,
            "launcher_project_sha256":crate::palette::sha256_hex(&self.project_bytes),
            "viewport_pixels":pixels,"logical_viewport":[logical.x,logical.y],"pixels_per_point":ctx.pixels_per_point(),
            "playing":false,"autoplay":false,"native_input":false,
            "seek_origin":"existing ordinary frame2 38%-duration seek","initial_seek_revision":self.seek_revision,
            "seek_revision":studio.player.seek_revision,"seconds":studio.player.position_secs(),
            "status":"captured_uninspected",
            "limitations":"Appearance only. Native toolbar, selection, controls, Apply/Cancel, Undo/Redo, keyboard/focus, held gestures, docking, ordinary Close, native2x/Wayland/Windows/macOS UNRUN. Bounds and legibility require human pixel inspection."});
        std::fs::write(
            path.with_extension("json"),
            serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
}

pub fn run(
    engine: Engine,
    listen: &str,
    project: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    view: Option<String>,
) -> anyhow::Result<()> {
    let feel_capture = FeelCapture::from_environment(
        screenshot.is_some(),
        view.as_deref(),
        &engine,
        project.as_deref(),
    )?;
    let arrangement_capture =
        ArrangementCapture::from_environment(screenshot.is_some(), view.as_deref(), &engine)?;
    let native_capture = audio_check::NativeCapture::from_environment(screenshot.is_some())
        .map_err(|e| anyhow::anyhow!(e))?;
    let native_scale = native_capture.as_ref().map_or(1.0, |c| c.scale);
    let view = if native_capture.is_some() {
        Some("mixer".to_string())
    } else {
        view
    };
    let view = view
        .and_then(|v| VIEWS.iter().position(|n| n.eq_ignore_ascii_case(&v)))
        .unwrap_or(3);
    let engine = Arc::new(Mutex::new(engine));
    // The existing screenshot tool sets this guard. Honor it here, before any
    // socket is bound; scenario evidence never requires a control listener.
    let server_ok = std::env::var("BEATBOX_NO_LISTEN").as_deref() != Ok("1")
        && crate::server::spawn(engine.clone(), listen).is_ok();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Beatbox Studio")
            .with_inner_size(
                if screenshot.is_some() && std::env::var_os("BEATBOX_NARROW_SHOT").is_some() {
                    [1100.0 * native_scale, 700.0 * native_scale]
                } else {
                    [1480.0 * native_scale, 920.0 * native_scale]
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
            if let Some(capture) = &native_capture {
                capture.activate(&cc.egui_ctx);
            }
            let saved_rev = engine.lock().unwrap().revision;
            let seen_project_load = engine.lock().unwrap().project_load_generation;
            let workspace = if screenshot.is_some() {
                workspace::Workspace::capture(&cc.egui_ctx, view)
            } else {
                workspace::Workspace::load(&cc.egui_ctx, view)
            };
            Ok(Box::new(Studio {
                saved_rev,
                seen_project_load,
                confirm_new: false,
                quit_close: QuitCloseState::default(),
                mixer_routing: false,
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
                render_state: Default::default(),
                live: None,
                live_applied: false,
                inspector: None,
                dragging: false,
                toast: None,
                render_job: None,
                // The existing header uses this flag only for its initial open
                // default. Remove the empty opener next frame; no report is injected.
                mix_readout: native_capture.as_ref().map(|_| String::new()),
                mix_job: None,
                mix_readout_revision: None,
                audio_check: audio_check::State::default(),
                mix_comparison: None,
                mix_comparison_job: None,
                comparison_start_s: 0.0,
                comparison_length_s: 8.0,
                screenshot,
                native_capture,
                arrangement_capture,
                feel_capture,
                frames: 0,
                shot_requested: false,
                perf: None,
                perf_next: None,
                perf_rec: None,
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
                marker_name: String::new(),
                tempo_edit_bpm: 120.0,
                tempo_edit_ramp: false,
                rail_search: String::new(),
                brand: None,
                note_editor: false,
                workspace,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("studio failed: {e}"))
}

/// Compute (pattern index, local step) for a song position in seconds.
fn locate(p: &Project, secs: f32) -> Option<(usize, f32)> {
    let mut step = if p.tempo_points.is_empty() {
        secs / p.step_secs()
    } else {
        crate::tempo_curve::TempoCurve::new(p).beat_at_secs(secs as f64) as f32 * 4.0
    };
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

/// Song body only, not render ring-out. Automated songs loop at their real end.
fn song_loop_seconds(p: &Project) -> f32 {
    if p.tempo_points.is_empty() {
        p.song_seconds()
    } else {
        crate::tempo_curve::TempoCurve::new(p).secs_at_beat(p.song_beats() as f64) as f32
    }
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
    /// Stop a PERFORM recording and write the launches as playlist clips on a
    /// fresh lane, in one atomic batch so a single GUI Undo removes the take.
    /// Nothing is written on an empty run.
    fn place_perf_recording(&mut self) {
        let Some(rec) = self.perf_rec.take() else {
            return;
        };
        if rec.invalid || self.engine.lock().unwrap().revision != rec.project_revision {
            self.toast = Some((
                "Project or timing changed - discard and record again".into(),
                0.0,
                false,
            ));
            self.perf_rec = Some(rec);
            return;
        }
        let end_bar = {
            let e = self.engine.lock().unwrap();
            rec.bar_now(e.project.bpm, self.player.abs_position_secs())
                .round()
                .max(0.0) as u32
        };
        let segs = &rec.segs;
        if !segs.iter().any(|(_, n)| !n.is_empty()) {
            self.toast = Some((
                "Nothing recorded - launch patterns while REC is on".into(),
                0.0,
                false,
            ));
            return;
        }
        let (lane, calls) = {
            let e = self.engine.lock().unwrap();
            let lane = e.project.playlist.iter().map(|c| c.lane).max().unwrap_or(0) + 1;
            let mut calls = Vec::new();
            if e.project.playlist.is_empty() && !e.project.arrangement.is_empty() {
                calls.push(json!({"tool": "arrangement_to_playlist", "args": {}}));
            }
            for (i, (bar, name)) in segs.iter().enumerate() {
                if name.is_empty() {
                    continue; // song-exit marker: only ends the previous segment
                }
                let next = segs.get(i + 1).map(|(b, _)| *b).unwrap_or(end_bar);
                let bars = next.saturating_sub(*bar).max(1);
                calls.push(json!({
                    "tool": "place_pattern",
                    "args": {"pattern": name, "start_bar": bar, "bars": bars, "lane": lane},
                }));
            }
            (lane, calls)
        };
        let n = segs.iter().filter(|(_, name)| !name.is_empty()).count();
        let track = lane + 1;
        let r = self.engine.lock().unwrap().call_from(
            "batch",
            &json!({"calls": calls, "atomic": true}),
            "you",
        );
        self.toast = Some(match r {
            Ok(_) => (
                format!("Recorded {n} launches to Track {track} - one Undo removes the take"),
                0.0,
                true,
            ),
            Err(err) => {
                self.perf_rec = Some(rec);
                (
                    format!("Recording not placed; take kept: {err:#}"),
                    0.0,
                    false,
                )
            }
        });
    }

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

    fn pump_render(&mut self, ctx: &egui::Context) {
        // Capture identity, project and bank under one engine lock. The frame's
        // earlier UI snapshot can already be stale after New/Open or a remote edit.
        let engine = self.engine.clone();
        let mut e = engine.lock().unwrap();
        let current = RenderSource {
            revision: e.revision,
            load_generation: e.project_load_generation,
        };
        let completed = self
            .rendering
            .as_ref()
            .and_then(|(key, rx)| render_job::poll(*key, rx));
        if let Some(message) = completed {
            self.rendering = None;
            self.live = None;
            match self.render_state.finish(message, current) {
                Completion::Install(r) => {
                    // Keep the identity lock until installation: a load/edit
                    // cannot slip between checking the source and swapping audio.
                    if self.perf.is_none() && self.perf_next.is_none() {
                        self.player.set_mix(r.mix.clone(), r.loop_secs);
                    }
                    self.rendered = Some(r);
                    self.live_applied = false;
                }
                Completion::Failed => {
                    // Splices are provisional. A failed full render falls back
                    // to the last completed mix, without stopping or seeking.
                    if self.live_applied && self.perf.is_none() && self.perf_next.is_none() {
                        if let Some(r) = &self.rendered {
                            self.player.set_mix(r.mix.clone(), r.loop_secs);
                        }
                    }
                    self.live_applied = false;
                }
                Completion::Discard => {}
            }
        }
        // Poll the full result first so a completed failure cannot be preceded
        // by a queued provisional splice in the same frame.
        if let Some((key, rx)) = &self.live {
            match rx.try_recv() {
                Ok(m) => {
                    if self.render_state.accepts(*key, current)
                        && self.perf.is_none()
                        && self.perf_next.is_none()
                    {
                        if let Some(r) = &self.rendered {
                            self.player.set_mix(m, r.loop_secs);
                            self.live_applied = true;
                        }
                    }
                    self.live = None;
                }
                Err(TryRecvError::Disconnected) => self.live = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        if !self.render_state.can_start(current) || self.dragging {
            return;
        }
        let p = e.project.clone();
        let samples = p.samples.clone();
        e.bank.sync(&samples);
        let bank = e.bank.clone();
        let key = self.render_state.begin(current);
        drop(e);
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        // Never splice across whole-project loads, including same-revision
        // reloads. New advances revision; dirty_steps also requires equal
        // non-note content before splicing. Ordinary edits keep this fast path.
        let prev = (self.player.is_playing() && self.perf.is_none() && self.perf_next.is_none())
            .then(|| {
                self.rendered
                    .as_ref()
                    .filter(|r| r.generation == current.load_generation)
                    .map(|r| (r.project.clone(), r.mix.clone()))
            })
            .flatten();
        let (ltx, lrx) = channel();
        self.live = prev.is_some().then_some((key, lrx));
        // Thread creation failure drops the sender; the next poll handles it as
        // a disconnect. Panics anywhere in preparation send an explicit failure.
        let _ = std::thread::Builder::new()
            .name("playback-render".into())
            .spawn(move || {
                let message = render_job::run(key, || {
                    if let Some((op, om)) = prev {
                        if let crate::live_splice::Dirty::Steps(a, b) =
                            crate::live_splice::dirty_steps(&op, &p)
                        {
                            if crate::live_splice::worth_splicing(&op, &p, a, b) {
                                if let Ok((m, _)) =
                                    crate::live_splice::splice(&op, &p, &bank, &om, a, b)
                                {
                                    let _ = ltx.send(Arc::new(m));
                                    ctx.request_repaint();
                                }
                            }
                        }
                    }
                    let mix = render::render(
                        &p,
                        &bank,
                        &RenderOptions {
                            keep_stems: true,
                            ..Default::default()
                        },
                    )
                    .map_err(|error| format!("Could not prepare sound: {error:#}"))?;
                    let mono: Vec<f32> = mix
                        .left
                        .iter()
                        .zip(mix.right.iter())
                        .map(|(a, b)| 0.5 * (a + b))
                        .collect();
                    let peaks = analysis::waveform_peaks(&mono, 1400);
                    let spectrum = analysis::log_spectrum_db(&mono, 72);
                    let report = analysis::analyze(&mix);
                    let loop_secs = song_loop_seconds(&p);
                    let mut strips: Vec<views::StripMeter> = mix
                        .stems
                        .iter()
                        .chain(mix.bus_stems.iter())
                        .map(|s| views::strip_meter(&s.name, &s.left, &s.right))
                        .collect();
                    strips.push(views::strip_meter("master", &mix.left, &mix.right));
                    Ok(Rendered {
                        strips,
                        rev: current.revision,
                        generation: current.load_generation,
                        mix: Arc::new(mix),
                        peaks,
                        spectrum,
                        report,
                        loop_secs,
                        project: Arc::new(p),
                    })
                });
                let _ = tx.send(message);
                ctx.request_repaint();
            });
        self.rendering = Some((key, rx));
    }

    fn render_recovery_ui(&mut self, ctx: &egui::Context) {
        let current = {
            let e = self.engine.lock().unwrap();
            RenderSource {
                revision: e.revision,
                load_generation: e.project_load_generation,
            }
        };
        let Some(error) = self.render_state.error(current).map(str::to_owned) else {
            return;
        };
        egui::TopBottomPanel::top("render_recovery").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Sound is not up to date").color(WARN))
                    .on_hover_text(&error);
                if ui.button("Retry sound").clicked() {
                    self.render_state.retry();
                    ctx.request_repaint();
                }
                ui.label(
                    RichText::new(if self.rendered.is_some() {
                        "Last completed mix kept. Open Details for the cause."
                    } else {
                        "No playable mix yet. Open Details for the cause."
                    })
                    .size(11.0)
                    .color(DIM),
                );
            });
            ui.collapsing("Details", |ui| {
                ui.label(error);
            });
        });
    }

    fn save_tooltip(&self) -> String {
        let key = if cfg!(target_os = "macos") {
            "Cmd+S"
        } else {
            "Ctrl+S"
        };
        match &self.project_path {
            Some(path) => format!("Save project ({key})\n{}", self.engine.lock().unwrap().resolve(&path.to_string_lossy()).display()),
            None => format!("Save project ({key})\nNo project file yet. First Save creates a new Beatbox project file without overwriting another beat."),
        }
    }

    fn needs_leave_warning(&self) -> bool {
        let revision = self.engine.lock().unwrap().revision;
        needs_leave_warning(self.project_path.is_some(), revision, self.saved_rev)
    }

    fn save_before_leave(&mut self) -> bool {
        let saved = self.save();
        let revision = self.engine.lock().unwrap().revision;
        save_allows_leave(saved, self.project_path.is_some(), revision, self.saved_rev)
    }

    fn save(&mut self) -> bool {
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
        let result = {
            let mut engine = self.engine.lock().unwrap();
            save_project_revision(&mut engine, &path)
        };
        match result {
            Ok(written_revision) => {
                self.project_path = Some(path.clone());
                self.saved_rev = written_revision;
                self.toast = Some((format!("Saved {}", path.display()), 0.0, true));
                true
            }
            Err(e) => {
                self.toast = Some((
                    format!(
                        "Could not save {}: {e:#}. Your project is still open with your edits; check the folder is writable and try again",
                        path.display()
                    ),
                    0.0,
                    false,
                ));
                false
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
                let generation = self.engine.lock().unwrap().project_load_generation;
                rhythm_trial::feedback::loaded(
                    &mut self.piano.rhythm_trial_status,
                    &mut self.seen_project_load,
                    generation,
                );
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
        v.sort_by_key(|x| std::cmp::Reverse(x.0));
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
            Action::Save => {
                self.save();
            }
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
        ui.horizontal_wrapped(|ui| {
            // Use the dock panel width, not the screen: the transport can float
            // or share a window with another panel. Controls wrap when needed.
            let panel_width = ui.available_width();
            let narrow = panel_width < 1000.0;
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
                let bb = playlist::marker_position(p, pos);
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
            if self.rendering.is_some() && self.player.is_playing() {
                ui.label(RichText::new("Preparing sound...").size(11.0).color(GOOD));
            }
            ui.add_space(8.0);
            let mut root = p.key_root.clone();
            let mut scale = p.scale.clone();
            let key_label=format!("Key: {root} {scale}");
            let mut key_controls=|ui:&mut egui::Ui| {
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
            };
            if panel_width<700.0 {
                ui.menu_button(key_label,key_controls);
            } else {
                key_controls(ui);
            }
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
                                if self.needs_leave_warning() {
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
                    if ui.button("Save").on_hover_text(self.save_tooltip()).clicked() {
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
                    if ui.button("Save").on_hover_text(self.save_tooltip()).clicked() {
                        self.save();
                    }
                    file_menu!(ui);
                }
            };
            if panel_width < 1300.0 {
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
                    fl_playlist::friendly_name(&pat.name),
                    FontId::proportional(12.0),
                    TEXT,
                );
                x += w;
            }
        }
        let curve = crate::tempo_curve::TempoCurve::new(p);
        let pos_step = curve.beat_at_secs(self.player.position_secs() as f64) as f32 * 4.0;
        let px = rect.left() + 4.0 + w_avail * (pos_step / total).clamp(0.0, 1.0);
        painter.line_segment(
            [Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())],
            Stroke::new(2.0_f32, Color32::WHITE),
        );
        if resp.clicked() {
            if let Some(m) = resp.interact_pointer_pos() {
                let t = ((m.x - rect.left() - 4.0) / w_avail).clamp(0.0, 1.0);
                let target_s = curve.secs_at_beat((t * total / 4.0) as f64) as f32;
                self.player.seek(target_s);
                if let Some((pi, _)) = locate(p, target_s) {
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
        if let Some(selected) = p
            .patterns
            .get(self.pattern)
            .filter(|q| !q.name.starts_with(crate::tools_fl::PL))
        {
            let identity = format!("{}:{}", selected.id, selected.name);
            if self
                .browser
                .pattern_rename
                .as_ref()
                .is_none_or(|v| v.0 != identity)
            {
                self.browser.pattern_rename = Some((identity, selected.name.clone()));
            }
            let mut rename = None;
            let mut duplicate = false;
            ui.horizontal_wrapped(|ui| {
                ui.label("Pattern name");
                let draft=&mut self.browser.pattern_rename.as_mut().unwrap().1;
                ui.add(egui::TextEdit::singleline(draft).desired_width(180.0));
                if ui.add_enabled(!draft.trim().is_empty() && draft.trim()!=selected.name,egui::Button::new("Rename")).on_hover_text("Rename this pattern and its song/playlist references. Notes, stable IDs and timing stay unchanged. Undo restores.").clicked() {
                    rename=Some(draft.trim().to_string());
                }
                duplicate = ui.button("Duplicate pattern").on_hover_text("Make an independent copy with the same notes and meter. Selects the copy for editing, without placing it in the song. Undo removes it.").clicked();
            });
            if let Some(name) = rename {
                self.call(
                    "rename_pattern",
                    json!({"pattern":selected.id,"new_name":name}),
                );
            }
            if duplicate {
                self.call("duplicate_pattern", json!({"pattern":selected.id}));
                let count = self.engine.lock().unwrap().project.patterns.len();
                if count > p.patterns.len() {
                    self.pattern = count - 1;
                }
                return; // The project snapshot above predates the new pattern.
            }
        }
        let playing_here = locate(p, self.player.position_secs())
            .filter(|(pi, _)| *pi == self.pattern)
            .map(|(_, s)| s);
        let rack_steps = p.patterns[self.pattern].steps() as usize;
        if channel_rack::rack_content_width(ui.available_width(), rack_steps) > ui.available_width()
        {
            ui.label(
                RichText::new("More steps to the right: scroll horizontally or Shift+wheel.")
                    .size(11.0)
                    .color(DIM),
            );
        }
        let seq_top = ui.next_widget_position().y;
        // a channel added to the rack scrolls into view: the scroll area
        // clamps to last frame's content, so the jump to the end comes one
        // frame after the channel count grows
        let seen_key = ui.id().with("rack-channel-count");
        let pend_key = ui.id().with("rack-scroll-pending");
        let seen = ui.data(|d| d.get_temp::<usize>(seen_key));
        ui.data_mut(|d| d.insert_temp(seen_key, p.tracks.len()));
        let grew = seen.is_some_and(|n| p.tracks.len() > n);
        let pending = ui.data(|d| d.get_temp::<bool>(pend_key)).unwrap_or(false);
        ui.data_mut(|d| d.insert_temp(pend_key, grew));
        let mut seq_scroll = egui::ScrollArea::both()
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .auto_shrink([false, false])
            .id_salt("seq");
        // never taller than what the pane shows, or rows past its bottom edge
        // could not be scrolled to
        let shown = ui.clip_rect().bottom() - ui.next_widget_position().y;
        if shown.is_finite() && shown > 40.0 {
            seq_scroll = seq_scroll.max_height(shown);
        }
        if grew {
            ui.ctx().request_repaint();
        }
        if pending {
            seq_scroll = seq_scroll.vertical_scroll_offset(1.0e6);
        }
        let steps = p.patterns[self.pattern].steps() as usize;
        let content_width = channel_rack::rack_content_width(ui.available_width(), steps);
        seq_scroll.show(ui, |ui| {
            ui.set_min_width(content_width);
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
            let actions = channel_rack::channel_rack(ui, p, self.pattern, &mut sel, playing_here);
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
        if let Some(capture) = &mut self.feel_capture {
            capture.freeze_initial_seek(self.frames, self.player.seek_revision);
        }
        if self.native_capture.is_some() {
            self.audio_check.begin_native_frame();
        }
        if self.native_capture.is_some()
            && self.frames == 2
            && self.mix_readout.as_deref() == Some("")
        {
            self.mix_readout = None;
        }
        self.audio_check.pump(&self.engine.lock().unwrap(), ctx);
        // audio files dropped on the window become channels
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            let (added, failed) = {
                let mut e = self.engine.lock().unwrap();
                browser_view::import_dropped(&mut e, &dropped)
            };
            let (text, good) = match (added.is_empty(), failed.first()) {
                (false, None) => (format!("Added {} as a new channel", added.join(", ")), true),
                (false, Some(f)) => (format!("Added {}. Skipped {f}", added.join(", ")), true),
                (true, Some(f)) => (f.clone(), false),
                (true, None) => (String::new(), false),
            };
            self.toast = Some((text, 0.0, good));
        }
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
                        let land = self.player.launch(m, l.loop_secs, l.quantum_secs);
                        if let Some(rec) = &mut self.perf_rec {
                            rec.pending = Some(land);
                        }
                        self.perf_next = Some(l.pattern);
                    }
                    (None, Some((m, secs))) => {
                        let land = self.player.launch(m, secs, l.quantum_secs);
                        if let Some(rec) = &mut self.perf_rec {
                            rec.pending = Some(land);
                        }
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
                if let Some(rec) = &mut self.perf_rec {
                    let bpm = e.project.bpm;
                    let bar = match rec.pending.take() {
                        Some(_) if rec.frozen.is_none() => {
                            let land = self.player.last_landing_secs();
                            if std::env::var_os("BEATBOX_PERF_TRACE").is_some() {
                                eprintln!("PERF_LANDING pattern={:?} abs={land:.9} start_pos={:.9} take_before={:.9} abs_last={:.9} bpm={bpm}", self.perf, rec.start_pos, rec.take_last, rec.abs_last);
                            }
                            // Read the actual callback/poll transition timestamp,
                            // not the launch request or repaint observation.
                            rec.take_last += (land - rec.abs_last).max(0.0) as f32;
                            rec.abs_last = land;
                            ((rec.start_pos + rec.take_last) * bpm / 240.0)
                                .round()
                                .max(0.0) as u32
                        }
                        _ => {
                            rec.pending = None;
                            rec.bar_now(bpm, self.player.abs_position_secs())
                                .round()
                                .max(0.0) as u32
                        }
                    };
                    if rec.frozen.is_none() {
                        match &self.perf {
                            Some(name) => {
                                // a re-launch in the same bar supersedes the first take
                                if rec.segs.last().map(|(b, _)| *b) == Some(bar) {
                                    rec.segs.pop();
                                }
                                rec.segs.push((bar, name.clone()));
                            }
                            // back to the song: close the open segment but keep
                            // the take open - only Place take writes clips
                            None => rec.segs.push((bar, String::new())),
                        }
                    }
                }
                if self.perf.is_none() {
                    // the song render may be newer than the one queued on exit
                    if let Some(r) = &self.rendered {
                        self.player.set_mix(r.mix.clone(), r.loop_secs);
                    }
                }
            }
            if let Some(rec) = &mut self.perf_rec {
                if (rec.bpm - e.project.bpm).abs() > 0.001
                    || rec.seek_revision != self.player.seek_revision
                    || rec.project_revision != e.revision
                {
                    rec.invalid = true;
                }
                // stopping mid-take makes further timing unsafe: freeze the
                // take at the stop point instead of recording on guesswork
                if rec.frozen.is_none() && !self.player.is_playing() {
                    rec.frozen = Some(
                        rec.take_last
                            + (self.player.abs_position_secs() - rec.abs_last).max(0.0) as f32,
                    );
                    rec.pending = None;
                    self.toast = Some((
                        "Transport stopped - take frozen; Place take or Discard".into(),
                        0.0,
                        false,
                    ));
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
        let generation = self.engine.lock().unwrap().project_load_generation;
        if rhythm_trial::feedback::observe_load(
            &mut self.piano.rhythm_trial_status,
            &mut self.seen_project_load,
            generation,
        ) {
            self.toast = None;
        }
        let (p, rev, log, undo) = self.snapshot();
        if let Some(capture) = &mut self.arrangement_capture {
            if capture.state.is_none() {
                let e = self.engine.lock().unwrap();
                if rev != e.revision || self.seen_project_load != e.project_load_generation {
                    fail_screenshot("Arrangement initialization identity drift");
                }
                let state = fl_playlist::capture_view(
                    ctx,
                    &e.project,
                    capture.revision,
                    capture.generation,
                    &capture.case,
                    capture.id.as_deref(),
                    true,
                )
                .unwrap_or_else(|e| fail_screenshot(&e));
                self.clip_sel = capture.id.clone();
                if let Some(pi) = state["source"]["index"].as_u64() {
                    self.pattern = pi as usize;
                }
                capture.seek_revision = self.player.seek_revision;
                capture.state = Some(state);
            }
        }

        if let Some(capture) = &mut self.feel_capture {
            let e = self.engine.lock().unwrap();
            if self.dragging {
                fail_screenshot("Feel initialization held Studio gesture");
            }
            if let Some((ti, pi)) = capture
                .initialize_pending(&e, ctx, &mut self.piano, (rev, self.seen_project_load))
                .unwrap_or_else(|e| fail_screenshot(&e))
            {
                self.selected = ti;
                self.pattern = pi;
                self.view = 0;
                self.note_editor = true;
            }
        }

        // Every incoming close is vetoed while the quit question is pending.
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        let needs_warning = self.needs_leave_warning();
        if self
            .quit_close
            .incoming_close(close_requested, needs_warning)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if self.quit_close.pending {
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
            let choice = match action {
                1 => Some(QuitChoice::Save(self.save_before_leave())),
                2 => Some(QuitChoice::Discard),
                3 => Some(QuitChoice::Cancel),
                _ => None,
            };
            if let Some(choice) = choice {
                if self.quit_close.resolve(choice) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
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
                if self.needs_leave_warning() {
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
                    let saved = self.save_before_leave();
                    self.pending_open = None;
                    if saved {
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
                    // Only continue after this save succeeds with no later edit.
                    if self.save_before_leave() {
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
        self.pump_render(ctx);
        self.render_recovery_ui(ctx);
        self.poll_render_job(ctx);

        // keyboard shortcuts (ported table: shortcuts.rs)
        for a in shortcuts::pressed(ctx) {
            self.shortcut(a, &p);
        }

        self.workspace_ui(ctx, &p, rev, &log, undo);

        self.export_window(ctx);
        self.rhythm_trial_ui(ctx);

        // toast
        if let Some((msg, t0, ok)) = &mut self.toast {
            let now = ctx.input(|i| i.time);
            if *t0 == 0.0 {
                *t0 = now;
            }
            let (msg, ok) = (msg.clone(), *ok);
            if now - *t0 < 4.0 {
                // painted straight onto a foreground layer: a notice, not a
                // widget, so it can never swallow a click meant for the
                // control under it
                let galley =
                    ctx.fonts(|f| f.layout_no_wrap(msg, egui::FontId::proportional(14.0), BG));
                let size = galley.size() + egui::vec2(20.0, 16.0);
                let screen = ctx.screen_rect();
                let rect = egui::Rect::from_center_size(
                    egui::pos2(screen.center().x, screen.max.y - 12.0 - size.y / 2.0),
                    size,
                );
                let painter = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("toast"),
                ));
                painter.rect_filled(
                    rect,
                    10.0,
                    if ok {
                        GOOD.gamma_multiply(0.9)
                    } else {
                        HOT.gamma_multiply(0.9)
                    },
                );
                painter.galley(rect.min + egui::vec2(10.0, 8.0), galley, BG);
            } else {
                self.toast = None;
            }
        }

        // screenshot automation
        if let Some(path) = self.screenshot.clone() {
            if self.native_capture.is_none() && self.frames == 2 {
                // Keep the ordinary screenshot cursor, but never autoplay for
                // any capture (including the original ten workflow images).
                self.player.seek(p.song_seconds() * 0.38);
            }
            let ready = self
                .rendered
                .as_ref()
                .map(|r| r.rev == rev)
                .unwrap_or(false);
            let scenario_ready = if let Some(capture) = &mut self.native_capture {
                match capture.advance(
                    &mut self.audio_check,
                    &mut self.engine.lock().unwrap(),
                    ctx,
                    ready,
                ) {
                    Ok(ready) => ready,
                    Err(error) => {
                        fail_screenshot(&format!("audio-native: {error}"));
                    }
                }
            } else {
                true
            };
            if let Some(capture) = &self.arrangement_capture {
                capture
                    .check(self, ctx)
                    .unwrap_or_else(|e| fail_screenshot(&e));
                if ready
                    && self
                        .rendered
                        .as_ref()
                        .is_none_or(|r| r.generation != capture.generation)
                {
                    fail_screenshot("Arrangement render generation mismatch");
                }
            }
            if let Some(capture) = &self.feel_capture {
                capture
                    .check(self, ctx)
                    .unwrap_or_else(|e| fail_screenshot(&e));
                if capture.state.is_some()
                    && ready
                    && self.rendered.as_ref().is_none_or(|r| {
                        r.rev != capture.revision || r.generation != capture.generation
                    })
                {
                    fail_screenshot("Feel render identity mismatch");
                }
            }
            let feel_ready = self.feel_capture.as_ref().is_none_or(|c| c.state.is_some());
            if ready && scenario_ready && feel_ready && self.frames > 30 && !self.shot_requested {
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
                if self.native_capture.is_some() && (!scenario_ready || !self.shot_requested) {
                    fail_screenshot("audio-native: terminal state changed before viewport capture");
                }
                if let Some(capture) = &self.arrangement_capture {
                    if !self.shot_requested
                        || !ready
                        || self.rendered.as_ref().is_none_or(|r| {
                            r.rev != capture.revision || r.generation != capture.generation
                        })
                    {
                        fail_screenshot("Arrangement capture not requested or render not ready");
                    }
                }
                if let Some(capture) = &self.feel_capture {
                    capture
                        .check(self, ctx)
                        .unwrap_or_else(|e| fail_screenshot(&e));
                    if capture.state.is_none()
                        || !ready
                        || !self.shot_requested
                        || self.rendered.as_ref().is_none_or(|r| {
                            r.rev != capture.revision || r.generation != capture.generation
                        })
                    {
                        fail_screenshot("Feel screenshot request/readiness drift");
                    }
                }
                let [w, h] = img.size;
                let mut buf = Vec::with_capacity(w * h * 4);
                for px in &img.pixels {
                    buf.extend_from_slice(&px.to_array());
                }
                let saved = persist_screenshot(
                    || {
                        let im = image::RgbaImage::from_raw(w as u32, h as u32, buf)
                            .ok_or_else(|| "Invalid screenshot pixel buffer".to_string())?;
                        im.save(&path).map_err(|e| e.to_string())
                    },
                    || {
                        if let Some(capture) = &self.native_capture {
                            capture.manifest(&path, &self.audio_check, ctx, [w, h])
                        } else if let Some(capture) = &self.feel_capture {
                            capture.manifest(self, ctx, &path, [w, h])
                        } else if let Some(capture) = &self.arrangement_capture {
                            capture.manifest(self, ctx, &path, [w, h])
                        } else {
                            Ok(())
                        }
                    },
                );
                let receipt = match saved {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        let _ = std::fs::remove_file(&path);
                        if self.arrangement_capture.is_some() || self.feel_capture.is_some() {
                            let _ = std::fs::remove_file(path.with_extension("json"));
                        }
                        fail_screenshot(&error);
                    }
                };
                if self
                    .quit_close
                    .approve_completed_capture(self.screenshot.is_some(), Some(receipt))
                {
                    eprintln!(
                        "screenshot saved to {}; capture complete, closing",
                        path.display()
                    );
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(
            if self.player.is_playing() { 16 } else { 120 },
        ));
    }
}

/// True only when no pattern, arrangement section, playlist clip, audio clip
/// or vocal map exists, so a render could only produce silence.
fn project_has_nothing_to_render(p: &crate::project::Project) -> bool {
    p.patterns.is_empty()
        && p.arrangement.is_empty()
        && p.playlist.is_empty()
        && p.audio_clips.is_empty()
        && p.vocal_map.is_none()
}

#[cfg(test)]
mod capture_shutdown_tests {
    use super::{persist_screenshot, QuitCloseState};
    use std::cell::Cell;

    #[test]
    fn completed_capture_permits_one_dirty_close_without_claiming_saved() {
        for pending in [false, true] {
            let mut quit = QuitCloseState {
                pending,
                approved_once: false,
            };
            let receipt = persist_screenshot(|| Ok(()), || Ok(())).unwrap();
            assert!(quit.approve_completed_capture(true, Some(receipt)));
            assert!(!quit.pending);
            assert!(!quit.incoming_close(false, true));
            assert!(quit.approved_once, "no event must retain approval");
            assert!(!quit.incoming_close(true, true));
            assert!(!quit.approved_once);
            assert!(quit.incoming_close(true, true));
        }
    }

    #[test]
    fn ordinary_mode_and_missing_receipt_never_authorize_close() {
        for screenshot_mode in [false, true] {
            let mut quit = QuitCloseState::default();
            assert!(!quit.approve_completed_capture(screenshot_mode, None));
            assert!(quit.incoming_close(true, true));
            assert!(quit.pending);
            assert!(!quit.approved_once);
        }
        let mut quit = QuitCloseState::default();
        let receipt = persist_screenshot(|| Ok(()), || Ok(())).unwrap();
        assert!(!quit.approve_completed_capture(false, Some(receipt)));
        assert!(quit.incoming_close(true, true));
    }

    #[test]
    fn failed_png_or_partial_manifest_cannot_supply_a_receipt() {
        let manifest_called = Cell::new(false);
        let result = persist_screenshot(
            || Err("PNG write failed".into()),
            || {
                manifest_called.set(true);
                Ok(())
            },
        );
        assert!(!manifest_called.get());
        assert_eq!(result.err().as_deref(), Some("PNG write failed"));
        let png_written = Cell::new(false);
        let result = persist_screenshot(
            || {
                png_written.set(true);
                Ok(())
            },
            || Err("JSON write failed after PNG".into()),
        );
        assert!(png_written.get());
        assert!(result.is_err());
        let mut quit = QuitCloseState::default();
        assert!(!quit.approve_completed_capture(true, result.ok()));
        assert!(quit.incoming_close(true, true));
        assert!(!quit.approved_once);
    }
}

#[cfg(test)]
mod quit_close_tests {
    use super::{needs_leave_warning, QuitChoice, QuitCloseState};

    #[test]
    fn edited_and_fileless_first_second_third_close_are_vetoed() {
        for (has_file, revision, saved) in [(true, 10, 9), (false, 9, 9)] {
            let mut state = QuitCloseState::default();
            for _ in 0..3 {
                assert!(state.incoming_close(true, needs_leave_warning(has_file, revision, saved)));
                assert!(state.pending);
                assert!(!state.approved_once);
            }
        }
    }

    #[test]
    fn pending_then_external_save_clean_still_vetoes() {
        let mut state = QuitCloseState::default();
        assert!(state.incoming_close(true, needs_leave_warning(true, 10, 9)));
        for _ in 0..2 {
            assert!(state.incoming_close(true, needs_leave_warning(true, 10, 10)));
        }
        assert!(!state.approved_once);
    }

    #[test]
    fn cancel_in_same_close_update_keeps_veto_and_later_dirty_asks() {
        let mut state = QuitCloseState::default();
        assert!(state.incoming_close(true, true));
        // The production route handles the incoming event before dialog choices.
        let cancel_close = state.incoming_close(true, true);
        assert!(!state.resolve(QuitChoice::Cancel));
        assert!(cancel_close);
        assert!(!state.pending);
        assert!(!state.approved_once);
        assert!(!state.incoming_close(false, true));
        assert!(state.incoming_close(true, true));
        assert!(state.pending);
    }

    #[test]
    fn no_event_is_inert_and_clean_initial_close_passes() {
        let mut state = QuitCloseState::default();
        assert!(!state.incoming_close(false, true));
        assert!(!state.pending);
        assert!(!state.incoming_close(true, needs_leave_warning(true, 9, 9)));
        assert!(!state.pending);
        assert!(state.incoming_close(true, true));
        assert!(!state.incoming_close(false, false));
        assert!(state.pending);
    }

    #[test]
    fn new_and_open_pending_do_not_supply_quit_approval() {
        // New/Open retain independent pending fields. Their actual routing is
        // checked in manual_ui_tests; neither invokes quit resolution.
        for _pending_action in ["New", "Open"] {
            for warning in [
                needs_leave_warning(true, 10, 9),
                needs_leave_warning(false, 9, 9),
            ] {
                let mut state = QuitCloseState::default();
                assert!(state.incoming_close(true, warning));
                assert!(state.incoming_close(true, warning));
                assert!(state.incoming_close(true, warning));
            }
        }
    }

    #[test]
    fn successful_save_and_discard_permit_exactly_one_incoming_close() {
        for choice in [QuitChoice::Save(true), QuitChoice::Discard] {
            let mut state = QuitCloseState::default();
            assert!(state.incoming_close(true, true));
            assert!(state.resolve(choice));
            assert!(!state.pending);
            assert!(state.approved_once);
            // No event must not spend approval for the generated Close event.
            assert!(!state.incoming_close(false, true));
            assert!(state.approved_once);
            // Still-fileless Discard must pass, without changing saved state.
            assert!(!state.incoming_close(true, needs_leave_warning(false, 9, 9)));
            assert!(!state.approved_once);
            assert!(!state.pending);
            assert!(state.incoming_close(true, true));
            assert!(state.pending);
        }
    }

    #[test]
    fn failed_save_never_permits_and_later_close_asks_again() {
        for warning in [true, false] {
            let mut state = QuitCloseState::default();
            assert!(state.incoming_close(true, true));
            assert!(!state.resolve(QuitChoice::Save(false)));
            assert!(!state.pending);
            assert!(!state.approved_once);
            assert!(!state.incoming_close(false, warning));
            assert!(state.incoming_close(true, true));
        }
    }
}

#[cfg(test)]
mod save_state_tests {
    use super::save_state;
    #[test]
    fn file_required_before_claiming_saved() {
        assert_eq!(save_state(false, 0, 0), "Not saved");
        assert_eq!(save_state(false, 9, 2), "Not saved");
    }
    #[test]
    fn edits_follow_existing_revision_guard() {
        assert_eq!(save_state(true, 2, 2), "Saved");
        assert_eq!(save_state(true, 3, 2), "Unsaved changes");
        assert_eq!(save_state(true, 3, 3), "Saved");
        // Undo can also advance revisions: do not promise content equality.
        assert_eq!(save_state(true, 4, 3), "Unsaved changes");
    }
}

#[cfg(test)]
mod unsaved_guard_tests {
    use super::{needs_leave_warning, save_allows_leave};

    #[test]
    fn fileless_equal_revision_needs_warning_for_every_leave_action() {
        // These are the shared predicates called by New, Open and OS Close.
        for revision in [0, 9, u64::MAX] {
            assert!(needs_leave_warning(false, revision, revision));
            assert!(!save_allows_leave(true, false, revision, revision));
            assert!(!needs_leave_warning(true, revision, revision));
        }
        assert!(needs_leave_warning(true, 10, 9));
        assert!(needs_leave_warning(true, 8, 9));
    }

    #[test]
    fn failed_attempt_blocks_leave_even_when_numeric_revisions_match() {
        let failing_save = || -> Result<u64, &'static str> { Err("write failed") };
        let result = failing_save();
        assert!(!save_allows_leave(result.is_ok(), true, 9, 9));
        assert!(!save_allows_leave(result.is_ok(), false, 9, 9));
    }

    #[test]
    fn success_allows_only_the_captured_revision_not_a_later_edit() {
        let written_revision = 9;
        assert!(save_allows_leave(true, true, 9, written_revision));
        // An edit observed after releasing the save lock is still dirty.
        assert!(needs_leave_warning(true, 10, written_revision));
        assert!(!save_allows_leave(true, true, 10, written_revision));
    }
}

// Real-engine temporary-I/O coverage. Requires the normal crate test build;
// the lightweight extracted predicate harness does not execute this module.
#[cfg(test)]
mod save_revision_io_tests {
    use super::*;

    struct TempProject(PathBuf);
    impl TempProject {
        fn new() -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "beatbox-unsaved-guard-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn actual_save_captures_locked_revision_and_later_edit_stays_dirty() {
        let dir = TempProject::new();
        let engine = Mutex::new(Engine::new(dir.0.clone()));
        let (written, project) = {
            let mut held = engine.lock().unwrap();
            held.call_from("set_tempo", &json!({"bpm": 97}), "you")
                .unwrap();
            let expected = held.revision;
            let project = held.project.clone();
            let written =
                save_project_revision(&mut held, std::path::Path::new("saved.beatbox.json"))
                    .unwrap();
            assert_eq!(written, expected);
            assert_eq!(held.revision, expected);
            (written, project)
        };
        let mut reloaded = Engine::new(dir.0.clone());
        reloaded
            .call_from(
                "load_project",
                &json!({"path": "saved.beatbox.json"}),
                "you",
            )
            .unwrap();
        assert_eq!(reloaded.project, project);
        let mut held = engine.lock().unwrap();
        held.call_from("set_tempo", &json!({"bpm": 101}), "you")
            .unwrap();
        assert!(needs_leave_warning(true, held.revision, written));
        assert!(!save_allows_leave(true, true, held.revision, written));
    }

    #[test]
    fn actual_failure_beneath_regular_file_never_allows_equal_revision_leave() {
        let dir = TempProject::new();
        let mut engine = Engine::new(dir.0.clone());
        let saved_revision =
            save_project_revision(&mut engine, std::path::Path::new("saved.beatbox.json")).unwrap();
        let project = engine.project.clone();
        let bytes = std::fs::read(dir.0.join("saved.beatbox.json")).unwrap();
        std::fs::write(dir.0.join("blocked"), b"regular file, not a directory").unwrap();
        let result = save_project_revision(&mut engine, &dir.0.join("blocked/child.beatbox.json"));
        assert!(result.is_err());
        assert_eq!(engine.revision, saved_revision);
        assert_eq!(engine.project, project);
        assert!(!save_allows_leave(
            result.is_ok(),
            true,
            engine.revision,
            saved_revision
        ));
        assert_eq!(
            std::fs::read(dir.0.join("saved.beatbox.json")).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::read(dir.0.join("blocked")).unwrap(),
            b"regular file, not a directory"
        );
    }
}

#[cfg(test)]
mod manual_ui_tests;

#[cfg(test)]
mod playback_cursor_tests {
    use super::*;
    #[test]
    fn rack_cursor_follows_repeats_meters_and_tempo_curve() {
        let mut e = crate::engine::Engine::new(std::env::temp_dir().join("cursor-map-test"));
        for (name, meter) in [("first", "3/4"), ("odd", "7/8")] {
            e.call("add_pattern", &json!({"name":name,"bars":1}))
                .unwrap();
            e.call(
                "set_time_signature",
                &json!({"pattern":name,"time_signature":meter}),
            )
            .unwrap();
        }
        e.call(
            "set_arrangement",
            &json!({"sections":[{"pattern":"first","repeats":2},{"pattern":"odd","repeats":1}]}),
        )
        .unwrap();
        e.call("add_tempo_point", &json!({"beat":2,"bpm":60}))
            .unwrap();
        e.call("add_tempo_point", &json!({"beat":8,"bpm":100,"ramp":true}))
            .unwrap();
        let p = &e.project;
        let c = crate::tempo_curve::TempoCurve::new(p);
        let first = p.pattern_index("first").unwrap();
        let odd = p.pattern_index("odd").unwrap();
        for (beat, pi, step) in [
            (1., first, 4.),
            (4., first, 4.),
            (6.25, odd, 1.),
            (9., odd, 12.),
        ] {
            let r = locate(p, c.secs_at_beat(beat) as f32).unwrap();
            assert_eq!(r.0, pi);
            assert!((r.1 - step).abs() < 0.0001);
        }
        assert!(locate(p, c.secs_at_beat(10.) as f32).is_none());
        let total = p.song_steps() as f32;
        let t = 6.25 * 4. / total;
        let target = c.secs_at_beat((t * total / 4.) as f64) as f32;
        assert_eq!(locate(p, target).unwrap().0, odd);
        let mut flat = p.clone();
        flat.tempo_points.clear();
        for secs in [0., 0.5, 1., 2., 3., 4.] {
            let expected = secs / flat.step_secs();
            if let Some((_, local)) = locate(&flat, secs) {
                assert!(local <= expected);
            }
        }
    }
}

#[cfg(test)]
mod song_loop_tests {
    use super::*;
    #[test]
    fn song_loop_end_follows_tempo_without_render_tail() {
        let mut p = Project::default();
        for bpm in [20., 61., 120., 137., 400.] {
            p.bpm = bpm;
            assert_eq!(song_loop_seconds(&p), p.song_seconds());
        }
        p.bpm = 120.;
        p.tempo_points = vec![crate::project::TempoPoint {
            beat: 4.,
            bpm: 60.,
            ramp: false,
        }];
        assert_eq!(song_loop_seconds(&p), 14.);
        p.tempo_points.push(crate::project::TempoPoint {
            beat: 8.,
            bpm: 100.,
            ramp: true,
        });
        let c = crate::tempo_curve::TempoCurve::new(&p);
        assert_eq!(song_loop_seconds(&p), c.secs_at_beat(16.) as f32);
    }
}

#[cfg(test)]
mod mix_checkpoint_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/mix-checkpoint-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e
    }
    #[test]
    fn checkpoint_unique_readonly_no_overwrite() {
        let mut e = fixture();
        e.call(
            "snapshot",
            &json!({"name":"MIX_CHECKPOINT_1","note":"keep me"}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        let rev = e.revision;
        let h = e.history().0.len();
        assert_eq!(
            beatbox::gui::capture_mix_checkpoint(&mut e).unwrap(),
            "mix_checkpoint_2"
        );
        assert_eq!(
            beatbox::gui::capture_mix_checkpoint(&mut e).unwrap(),
            "mix_checkpoint_3"
        );
        assert_eq!(e.snapshots.len(), 3);
        assert_eq!(e.snapshots[0].1.note, "keep me");
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        assert_eq!(rev, e.revision);
        assert_eq!(h, e.history().0.len());
    }
    #[test]
    fn checkpoint_restore_all_undo_exact() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        let name = beatbox::gui::capture_mix_checkpoint(&mut e).unwrap();
        e.call("set_mixer", &json!({"track":"drums","volume_db":-11}))
            .unwrap();
        e.call(
            "rename_pattern",
            &json!({"pattern":"groove","new_name":"Changed phrase"}),
        )
        .unwrap();
        let edited = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        e.call("restore_snapshot", &json!({"name":name})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        assert_eq!(e.history().0.len(), h + 1);
        assert_eq!(e.snapshots.len(), 1);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), edited);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        let h = e.history().0.len();
        let rev = e.revision;
        e.call("restore_snapshot", &json!({"name":name})).unwrap();
        assert_eq!(h, e.history().0.len());
        assert_eq!(rev, e.revision);
    }
    #[test]
    fn checkpoint_save_companion_reload_and_missing_atomic() {
        let mut e = fixture();
        let name = beatbox::gui::capture_mix_checkpoint(&mut e).unwrap();
        let original = serde_json::to_value(&e.project).unwrap();
        e.call("set_mixer", &json!({"track":"drums","volume_db":-14}))
            .unwrap();
        e.call(
            "save_project",
            &json!({"path":"/tmp/checkpoint-project.json"}),
        )
        .unwrap();
        assert!(std::path::Path::new("/tmp/checkpoint-project.json.snapshots.json").exists());
        let mut f = beatbox::Engine::new("/tmp/checkpoint-reload".into());
        f.call(
            "load_project",
            &json!({"path":"/tmp/checkpoint-project.json"}),
        )
        .unwrap();
        assert_eq!(f.snapshots.len(), e.snapshots.len());
        f.call("restore_snapshot", &json!({"name":name})).unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), original);
        let b = serde_json::to_value(&f.project).unwrap();
        let rev = f.revision;
        assert!(f
            .call("restore_snapshot", &json!({"name":"missing-checkpoint"}))
            .is_err());
        assert_eq!(rev, f.revision);
        assert_eq!(serde_json::to_value(&f.project).unwrap(), b);
    }
}

#[cfg(test)]
mod project_notice_tests {
    use crate as beatbox;
    use serde_json::json;
    #[test]
    fn generation_only_successful_loads() {
        let mut e = beatbox::Engine::new("/tmp/project-notice-test".into());
        assert_eq!(e.project_load_generation, 0);
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        assert_eq!(e.project_load_generation, 1);
        let seen = e.project_load_generation;
        e.call("set_mixer", &json!({"track":"drums","volume_db":-7}))
            .unwrap();
        assert!(!beatbox::gui::project_notice_is_stale(
            seen,
            e.project_load_generation
        ));
        e.call("undo", &json!({})).unwrap();
        assert_eq!(seen, e.project_load_generation);
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        assert!(beatbox::gui::project_notice_is_stale(
            seen,
            e.project_load_generation
        ));
        assert_eq!(e.project_load_generation, 2);
    }
    #[test]
    fn failed_load_does_not_invalidate() {
        let mut e = beatbox::Engine::new("/tmp/project-notice-test".into());
        let seen = e.project_load_generation;
        std::fs::write("/tmp/project-notice-invalid.json", "bad json").unwrap();
        assert!(e
            .call(
                "load_project",
                &json!({"path":"/tmp/project-notice-invalid.json"})
            )
            .is_err());
        assert!(!beatbox::gui::project_notice_is_stale(
            seen,
            e.project_load_generation
        ));
        assert!(e
            .call(
                "load_project",
                &json!({"path":"/tmp/project-notice-missing215.json"})
            )
            .is_err());
        assert_eq!(seen, e.project_load_generation);
    }
    #[test]
    fn generation_not_project_data_or_undo() {
        let mut e = beatbox::Engine::new("/tmp/project-notice-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        let mut expected = e.project.clone();
        expected.name = "Other song".into();
        e.replace_project(expected);
        assert_eq!(e.project_load_generation, 2);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        assert_eq!(e.project_load_generation, 2);
        assert!(!beatbox::gui::project_notice_is_stale(2, 2));
        assert!(beatbox::gui::project_notice_is_stale(u64::MAX, 0));
    }
}

#[cfg(test)]
mod checkpoint_comparison_tests {
    use super::*;
    #[test]
    fn source_unchanged_matching_and_transport() {
        let dir = std::env::temp_dir().join(format!("bb223_{}", std::process::id()));
        let mut e = Engine::new(dir);
        e.call("new_project", &json!({"name":"ab","bpm":120}))
            .unwrap();
        e.call("add_track", &json!({"name":"tone","instrument":"sine"}))
            .unwrap();
        e.call("add_notes",&json!({"track":"tone","pattern":"A","notes":[{"pitch":"C4","start":0,"len":16,"vel":0.8}]})).unwrap();
        e.call("snapshot", &json!({"name":"before"})).unwrap();
        e.call("set_mixer", &json!({"track":"tone","volume_db":-9}))
            .unwrap();
        let original = serde_json::to_value(&e.project).unwrap();
        let rev = e.revision;
        let history = e.history().0.len();
        let (v, audio) = render_checkpoint_comparison(
            checkpoint_comparison_engine(&e, "before").unwrap(),
            "before",
            0.0,
            8.0,
        )
        .unwrap();
        assert_eq!(v["loudness_matched"], true);
        assert_eq!(v["A"], "before");
        assert_eq!(v["B"], "current");
        assert!(v["judgement"].is_null());
        assert_eq!(v["level_change_db"]["B"].as_f64().unwrap(), 0.0);
        assert!(v["level_change_db"]["A"].as_f64().unwrap() < -8.0);
        assert_eq!(audio.0.len(), audio.1.len());
        assert!(audio.0.iter().all(|x| x.is_finite() && x.abs() <= 0.951));
        assert_eq!(serde_json::to_value(&e.project).unwrap(), original);
        assert_eq!(e.revision, rev);
        assert_eq!(e.history().0.len(), history);
        assert!(e.transport.pending.is_empty());
        assert!(e.transport.pending_audition.is_none());
        assert_eq!(e.snapshots.len(), 1);
        assert!(comparison_is_current(rev, e.project_load_generation, &e));
        assert!(!comparison_is_current(
            rev + 1,
            e.project_load_generation,
            &e
        ));
        assert!(!comparison_is_current(
            rev,
            e.project_load_generation + 1,
            &e
        ));
        assert!(checkpoint_comparison_engine(&e, "missing").is_err());
    }
    #[test]
    fn silent_side_is_labeled_unmatched_and_keeps_source() {
        let mut e =
            Engine::new(std::env::temp_dir().join(format!("bb223_silent_{}", std::process::id())));
        e.call("new_project", &json!({"name":"silent"})).unwrap();
        e.call("snapshot", &json!({"name":"silence"})).unwrap();
        let (value, audio) = render_checkpoint_comparison(
            checkpoint_comparison_engine(&e, "silence").unwrap(),
            "silence",
            0.0,
            8.0,
        )
        .unwrap();
        assert_eq!(value["loudness_matched"], false);
        assert!(value["matched_to_lufs"].is_null());
        assert!(audio.0.iter().chain(&audio.1).all(|sample| *sample == 0.));
        assert!(e.transport.pending_audition.is_none());
    }
    #[test]
    fn captured_versions_survive_later_source_edits_without_following_them() {
        let mut e = Engine::new(std::env::temp_dir().join("bb223_isolation"));
        e.call("new_project", &json!({"name":"original"})).unwrap();
        e.call("snapshot", &json!({"name":"before"})).unwrap();
        let captured = checkpoint_comparison_engine(&e, "before").unwrap();
        let rev = e.revision;
        let generation = e.project_load_generation;
        e.call("set_tempo", &json!({"bpm":99})).unwrap();
        assert_ne!(captured.project.bpm, e.project.bpm);
        assert!(!comparison_is_current(rev, generation, &e));
        assert_eq!(captured.snapshots.len(), 1);
        assert_eq!(captured.snapshots[0].0, "before");
    }
    #[test]
    fn selection_and_stale_guards_precede_play_stop_only_changes_preview() {
        let mut e = Engine::new(std::env::temp_dir().join("bb223_play"));
        let result = MixComparison {
            revision: e.revision,
            generation: e.project_load_generation,
            checkpoint: "first".into(),
            start_s: 0.0,
            length_s: 8.0,
            value: json!({}),
            audio: Arc::new((vec![0.; 8], vec![0.; 8])),
        };
        assert!(!play_checkpoint_comparison(
            &result, "second", 0.0, 8.0, &mut e
        ));
        assert!(e.transport.pending.is_empty());
        assert!(e.transport.pending_audition.is_none());
        assert!(!play_checkpoint_comparison(
            &result, "first", 1.0, 8.0, &mut e
        ));
        assert!(!play_checkpoint_comparison(
            &result, "first", 0.0, 4.0, &mut e
        ));
        assert!(play_checkpoint_comparison(
            &result, "first", 0.0, 8.0, &mut e
        ));
        assert_eq!(e.transport.pending, vec![crate::engine::TransportCmd::Stop]);
        let queued = e
            .transport
            .pending_audition
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap();
        assert!(Arc::ptr_eq(queued, &result.audio));
        let rev = e.revision;
        let project = serde_json::to_value(&e.project).unwrap();
        stop_comparison_preview(&mut e);
        assert!(matches!(e.transport.pending_audition, Some(None)));
        assert_eq!(e.transport.pending, vec![crate::engine::TransportCmd::Stop]);
        assert_eq!(e.revision, rev);
        assert_eq!(serde_json::to_value(&e.project).unwrap(), project);
        e.transport.pending.clear();
        e.revision += 1;
        assert!(!play_checkpoint_comparison(
            &result, "first", 0.0, 8.0, &mut e
        ));
        assert!(e.transport.pending.is_empty());
    }
    #[test]
    fn checkpoint_named_like_file_does_not_audition_the_file() {
        let dir = std::env::temp_dir().join(format!("bb223_collision_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        e.call("new_project", &json!({"name":"silent"})).unwrap();
        e.call("snapshot", &json!({"name":"before"})).unwrap();
        std::fs::write(dir.join("before"), b"not audio").unwrap();
        std::fs::write(
            dir.join("__mixer_checkpoint_comparison"),
            b"not audio either",
        )
        .unwrap();
        let (value, audio) = render_checkpoint_comparison(
            checkpoint_comparison_engine(&e, "before").unwrap(),
            "before",
            0.0,
            8.0,
        )
        .unwrap();
        assert_eq!(value["A"], "before");
        assert!(audio.0.iter().chain(&audio.1).all(|sample| *sample == 0.));
        assert_eq!(std::fs::read(dir.join("before")).unwrap(), b"not audio");
        assert_eq!(e.snapshots[0].0, "before");
    }
    #[test]
    fn later_excerpt_uses_requested_audio_and_rejects_bad_ranges() {
        let mut e =
            Engine::new(std::env::temp_dir().join(format!("bb225_region_{}", std::process::id())));
        e.call("new_project", &json!({"name":"later","bpm":120}))
            .unwrap();
        e.call("add_track", &json!({"name":"tone","instrument":"sine"}))
            .unwrap();
        e.call("add_notes",&json!({"track":"tone","pattern":"A","notes":[{"pitch":60,"start":32,"len":16,"vel":0.8}]})).unwrap();
        e.call("snapshot", &json!({"name":"before"})).unwrap();
        let (early, _) = render_checkpoint_comparison(
            checkpoint_comparison_engine(&e, "before").unwrap(),
            "before",
            0.,
            2.,
        )
        .unwrap();
        let (later, audio) = render_checkpoint_comparison(
            checkpoint_comparison_engine(&e, "before").unwrap(),
            "before",
            4.,
            2.,
        )
        .unwrap();
        assert_eq!(early["loudness_matched"], false);
        assert_eq!(later["loudness_matched"], true);
        assert_eq!(later["facts"]["A"]["seconds"], 2.0);
        assert_eq!(later["facts"]["B"]["seconds"], 2.0);
        assert_eq!(audio.0.len(), crate::dsp::SR as usize * 5);
        for (start, length) in [
            (-1., 2.),
            (0., 0.),
            (0., 121.),
            (f32::NAN, 2.),
            (0., f32::INFINITY),
            (99999., 2.),
        ] {
            assert!(render_checkpoint_comparison(
                checkpoint_comparison_engine(&e, "before").unwrap(),
                "before",
                start,
                length
            )
            .is_err());
        }
        assert!(render_checkpoint_comparison(
            checkpoint_comparison_engine(&e, "before").unwrap(),
            "before",
            100.,
            2.
        )
        .is_err());
        assert!(e.transport.pending_audition.is_none());
    }
    #[test]
    fn shorter_side_is_explicit_and_one_missing_side_is_rejected() {
        let mut e =
            Engine::new(std::env::temp_dir().join(format!("bb225_short_{}", std::process::id())));
        e.call("new_project", &json!({"name":"short","bpm":120}))
            .unwrap();
        e.call("add_track", &json!({"name":"tone","instrument":"sine"}))
            .unwrap();
        e.call("add_notes", &json!({"track":"tone","pattern":"A","notes":[{"pitch":60,"start":0,"len":64,"vel":0.8}]})).unwrap();
        e.call("snapshot", &json!({"name":"before"})).unwrap();
        e.project.bpm = 240.;
        let (value, _) = render_checkpoint_comparison(
            checkpoint_comparison_engine(&e, "before").unwrap(),
            "before",
            2.,
            8.,
        )
        .unwrap();
        assert_ne!(
            value["facts"]["A"]["seconds"],
            value["facts"]["B"]["seconds"]
        );
        assert!(comparison_note(&value).starts_with("Different rendered lengths"));
        assert!(render_checkpoint_comparison(
            checkpoint_comparison_engine(&e, "before").unwrap(),
            "before",
            6.,
            2.
        )
        .is_err());
        assert!(comparison_note(
            &json!({"facts":{"A":{"seconds":2.},"B":{"seconds":2.}},"loudness_matched":false})
        )
        .contains("not level matched"));
    }
}

#[cfg(test)]
mod feel_capture_mode_tests {
    use super::FeelCapture;

    #[test]
    fn finite_capture_mode_rejects_unknown_missing_gate_and_mixed_selectors() {
        for case in ["selected", "all-offsets", "boundary-blocked"] {
            assert!(FeelCapture::validate_mode(case, true, true, Some("sequencer"), false).is_ok());
            for (screenshot, no_listen, view, mixed) in [
                (false, true, Some("sequencer"), false),
                (true, false, Some("sequencer"), false),
                (true, true, None, false),
                (true, true, Some("mixer"), false),
                (true, true, Some("playlist"), false),
                (true, true, Some("sequencer"), true),
            ] {
                assert!(
                    FeelCapture::validate_mode(case, screenshot, no_listen, view, mixed).is_err()
                );
            }
        }
        for case in ["", "unknown", "selected,all-offsets", "SELECTED"] {
            assert!(
                FeelCapture::validate_mode(case, true, true, Some("sequencer"), false).is_err()
            );
        }
    }

    #[test]
    fn engine_pretty_and_screenshot_typed_compact_provenance_roundtrip() {
        use crate::{engine::Engine, palette::sha256_hex};
        use serde_json::json;
        let root = std::env::temp_dir().join(format!("feel_provenance_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let saved_path = root.join("engine.json");
        let launcher_path = root.join("launcher.json");
        let mut author = Engine::new(root.clone());
        author
            .call(
                "new_project",
                &json!({"bpm":120,"patterns":[{"name":"A","bars":1}]}),
            )
            .unwrap();
        author.call("add_track", &json!({"name":"Feel keys","volume_db":-24,
            "instrument":{"type":"synth","osc1":"sine","osc2":"sine","osc_mix":0.0,"unison":1,"noise_level":0.0}})).unwrap();
        author
            .call(
                "add_notes",
                &json!({"track":"Feel keys","pattern":"A","notes":[
            {"pitch":60,"start":0.3,"len":1,"vel":0.6,"offset":0.12},
            {"pitch":64,"start":1.3,"len":1,"vel":0.6,"offset":-0.1},
            {"pitch":67,"start":4.3,"len":1,"vel":0.6,"offset":0}]}),
            )
            .unwrap();
        author
            .call("save_project", &json!({"path":saved_path}))
            .unwrap();
        let saved = std::fs::read(&saved_path).unwrap();
        let saved_json: serde_json::Value = serde_json::from_slice(&saved).unwrap();
        let notes = &saved_json["patterns"][0]["clips"]["feel keys"];
        assert_eq!(notes[0]["start"], json!(0.3));
        assert_eq!(notes[0]["vel"], json!(0.6));
        for key in ["offset", "prob", "slide_to"] {
            assert!(notes[2].get(key).is_none());
        }
        let mut tool = Engine::new(root.clone());
        tool.call("load_project", &json!({"path":saved_path}))
            .unwrap();
        // Exact real tools_parity screenshot serialization, not Value JSON.
        let launcher = serde_json::to_string(&tool.project).unwrap().into_bytes();
        assert_ne!(saved, launcher);
        assert_ne!(sha256_hex(&saved), sha256_hex(&launcher));
        std::fs::write(&launcher_path, &launcher).unwrap();
        let mut studio = Engine::new(root.clone());
        studio
            .call("load_project", &json!({"path":launcher_path}))
            .unwrap();
        let baseline = serde_json::to_vec(&studio.project).unwrap();
        for fixture in [&saved, &launcher] {
            assert_eq!(
                FeelCapture::verify_project_provenance(
                    FeelCapture::PROJECT_CONTRACT,
                    fixture,
                    &sha256_hex(fixture),
                    &std::fs::read(&launcher_path).unwrap(),
                    &studio.project
                )
                .unwrap(),
                baseline
            );
        }
        // The verified Project still reaches the actual initializer/observer.
        for case in ["selected", "all-offsets"] {
            let mut piano = super::piano::PianoState::default();
            let stamp = (studio.revision, studio.project_load_generation);
            let (ti, pi) = piano
                .initialize_feel_capture(&studio.project, stamp, case)
                .unwrap();
            let observed = piano
                .observe_feel_capture(&studio.project, stamp, (ti, pi, 0), case)
                .unwrap();
            assert_eq!(observed["source_notes"], *notes);
            assert_eq!(serde_json::to_vec(&studio.project).unwrap(), baseline);
        }
        let hash = sha256_hex(&saved);
        let check = |contract: &str,
                     fixture: &[u8],
                     hash: &str,
                     bytes: &[u8],
                     project: &crate::project::Project| {
            FeelCapture::verify_project_provenance(contract, fixture, hash, bytes, project).is_err()
        };
        for contract in ["", "unknown", "project_json_v2"] {
            assert!(check(contract, &saved, &hash, &launcher, &studio.project));
        }
        for wrong_hash in ["bad".to_string(), "0".repeat(64), sha256_hex(&launcher)] {
            assert!(check(
                FeelCapture::PROJECT_CONTRACT,
                &saved,
                &wrong_hash,
                &launcher,
                &studio.project
            ));
        }
        assert!(check(
            FeelCapture::PROJECT_CONTRACT,
            &saved,
            &hash,
            &saved,
            &studio.project
        ));
        let mut modified = studio.project.clone();
        modified.bpm = 121.0;
        let modified_launcher = serde_json::to_string(&modified).unwrap().into_bytes();
        assert!(check(
            FeelCapture::PROJECT_CONTRACT,
            &saved,
            &hash,
            &modified_launcher,
            &modified
        ));
        assert!(check(
            FeelCapture::PROJECT_CONTRACT,
            &saved,
            &hash,
            &launcher,
            &modified
        ));
        assert!(check(
            FeelCapture::PROJECT_CONTRACT,
            &modified_launcher,
            &hash,
            &modified_launcher,
            &modified
        ));
        assert!(check(
            FeelCapture::PROJECT_CONTRACT,
            b"not json",
            &sha256_hex(b"not json"),
            &launcher,
            &studio.project
        ));
        std::fs::remove_file(saved_path).unwrap();
        std::fs::remove_file(launcher_path).unwrap();
    }
}
