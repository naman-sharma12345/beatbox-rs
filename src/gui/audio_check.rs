//! Manual whole-mix diagnostics. Rendering is delegated to shared tools on a
//! private Engine; only explicit Play may change the live transport.
use crate::{engine::Engine, project::Project, samples::SampleBank};
use eframe::egui::{self, RichText};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

static REQUEST: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq)]
struct Bounds {
    start: f32,
    end: f32,
}
impl Bounds {
    fn checked(start: f32, end: f32) -> Result<Self, String> {
        if !start.is_finite() || !end.is_finite() || start < 0.0 || end > 86400.0 || end <= start {
            return Err("Use elapsed seconds: 0 <= start < end <= 86400.".into());
        }
        Ok(Self { start, end })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Parameters {
    whole: bool,
    start: f32,
    end: f32,
}
impl Default for Parameters {
    fn default() -> Self {
        Self {
            whole: true,
            start: 0.0,
            end: 10.0,
        }
    }
}
impl Parameters {
    fn bounds(self) -> Result<Option<Bounds>, String> {
        if self.whole {
            Ok(None)
        } else {
            Bounds::checked(self.start, self.end).map(Some)
        }
    }
    fn args(self) -> Result<Value, String> {
        let mut args = json!({"max_events": 128});
        if let Some(b) = self.bounds()? {
            args["start_s"] = json!(b.start);
            args["end_s"] = json!(b.end);
        }
        // tail_s is the detector's analysis horizon, not preview ring-out.
        Ok(args)
    }
}

#[derive(Clone)]
struct Key {
    request: u64,
    epoch: u64,
    revision: u64,
    generation: u64,
    params: Parameters,
    project: Project,
    // Retaining the bank keeps buffer identities alive. Project equality also
    // catches direct shared-engine edits that did not advance the revision.
    bank: SampleBank,
}
impl Key {
    fn capture(e: &Engine, epoch: u64, params: Parameters) -> Self {
        Self {
            request: REQUEST.fetch_add(1, Ordering::Relaxed),
            epoch,
            revision: e.revision,
            generation: e.project_load_generation,
            params,
            project: e.project.clone(),
            bank: e.bank.clone(),
        }
    }
    fn matches(&self, e: &Engine, epoch: u64, params: Parameters) -> bool {
        self.epoch == epoch
            && self.params == params
            && self.revision == e.revision
            && self.generation == e.project_load_generation
            && self.project == e.project
            && self.project.samples.iter().all(|s| {
                match (self.bank.get(&s.name), e.bank.get(&s.name)) {
                    (Some(a), Some(b)) => std::ptr::eq(a, b),
                    (None, None) => true,
                    _ => false,
                }
            })
    }
    fn engine(&self, workdir: PathBuf) -> Engine {
        let mut own = Engine::new(workdir);
        own.project = self.project.clone();
        own.bank = self.bank.clone();
        own
    }
}

struct Event {
    kind: String,
    severity: String,
    time: f32,
    end: Option<f32>,
    detail: String,
    track: Option<String>,
}
struct Report {
    raw: Value,
    events: Vec<Event>,
    window: Option<Bounds>,
    source_end: Option<f32>,
    ending_checked: bool,
}
fn finite(v: &Value) -> Option<f32> {
    v.as_f64()
        .map(|x| x as f32)
        .filter(|x| x.is_finite() && *x >= 0.0)
}
impl Report {
    fn parse(raw: Value) -> Result<Self, String> {
        let rows = raw["events"]
            .as_array()
            .ok_or("Audio check returned no event list.")?;
        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            events.push(Event {
                kind: row["kind"]
                    .as_str()
                    .ok_or("Missing event kind.")?
                    .replace('_', " "),
                severity: row["severity"]
                    .as_str()
                    .ok_or("Missing event severity.")?
                    .into(),
                time: finite(&row["time_s"]).ok_or("Invalid event time.")?,
                end: finite(&row["end_s"]),
                detail: row["detail"]
                    .as_str()
                    .ok_or("Missing event detail.")?
                    .into(),
                track: row["track"].as_str().map(str::to_owned),
            });
        }
        let w = if raw["window"].is_object() {
            &raw["window"]
        } else {
            &raw["window_bounds"]
        };
        let window = finite(&w["start_s"])
            .zip(finite(&w["end_s"]))
            .and_then(|(a, b)| Bounds::checked(a, b).ok());
        let source_end = finite(&w["source_end_s"]);
        // Old command versions return a label instead of metadata. Never infer
        // endpoint applicability from a label or a requested full-mix scan.
        let ending_checked = w["source_end_checked"].as_bool() == Some(true)
            && window
                .zip(source_end)
                .is_some_and(|(b, end)| (b.end - end).abs() < 0.001);
        Ok(Self {
            raw,
            events,
            window,
            source_end,
            ending_checked,
        })
    }
    fn excerpt(&self, index: usize) -> Result<Bounds, String> {
        let event = self.events.get(index).ok_or("Event no longer available.")?;
        let end = event.end.unwrap_or(event.time).max(event.time);
        let start = (event.time - 2.0).max(0.0);
        let end = (end + 2.0).min(start + 12.0);
        let end = self.source_end.map_or(end, |limit| end.min(limit));
        Bounds::checked(start, end)
    }
}

struct Prepared {
    report_request: u64,
    bounds: Bounds,
    path: PathBuf,
    audio: super::PreviewAudio,
    requested_bounds: Bounds,
    raw: Value,
}
enum Output {
    Report(Report),
    Prepared(Prepared),
}
struct Job {
    key: Arc<Key>,
    receiver: Receiver<Result<Output, String>>,
}

/// Actual production hit rectangles, useful to pointer tests and native drivers.
/// They are geometry, never a way to inject a completed report.
#[derive(Clone)]
pub(super) struct Targets {
    check: egui::Rect,
    whole: egui::Rect,
    #[cfg(test)]
    whole_id: Option<egui::Id>,
    play: egui::Rect,
    stop: egui::Rect,
    last_prepare: egui::Rect,
    last: egui::Rect,
    clip: egui::Rect,
    max_text_right: f32,
    play_enabled: bool,
    report_heading: egui::Rect,
    notice: egui::Rect,
    first_event: egui::Rect,
    last_event: egui::Rect,
    last_detail: egui::Rect,
    prepared: egui::Rect,
    stale: egui::Rect,
    error: egui::Rect,
}
impl Default for Targets {
    fn default() -> Self {
        Self {
            check: egui::Rect::NOTHING,
            whole: egui::Rect::NOTHING,
            #[cfg(test)]
            whole_id: None,
            play: egui::Rect::NOTHING,
            stop: egui::Rect::NOTHING,
            last_prepare: egui::Rect::NOTHING,
            last: egui::Rect::NOTHING,
            clip: egui::Rect::NOTHING,
            max_text_right: 0.0,
            play_enabled: false,
            report_heading: egui::Rect::NOTHING,
            notice: egui::Rect::NOTHING,
            first_event: egui::Rect::NOTHING,
            last_event: egui::Rect::NOTHING,
            last_detail: egui::Rect::NOTHING,
            prepared: egui::Rect::NOTHING,
            stale: egui::Rect::NOTHING,
            error: egui::Rect::NOTHING,
        }
    }
}

#[derive(Default)]
pub(super) struct State {
    params: Parameters,
    epoch: u64,
    report: Option<(Arc<Key>, Report)>,
    prepared: Option<Prepared>,
    job: Option<Job>,
    expected_request: Option<u64>,
    error: Option<String>,
    stale: bool,
    native_targets: Option<Targets>,
}
impl State {
    pub(super) fn begin_native_frame(&mut self) {
        self.native_targets = None;
    }
    fn invalidate(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.stale = true;
        self.prepared = None;
    }
    fn current(&self, key: &Key, e: &Engine) -> bool {
        !self.stale && key.matches(e, self.epoch, self.params)
    }
    fn accept(&mut self, key: Arc<Key>, output: Result<Output, String>, e: &Engine) {
        if self.expected_request != Some(key.request) {
            return;
        }
        self.expected_request = None;
        if !key.matches(e, self.epoch, self.params) {
            self.error = Some("Project or range changed while working. Check audio again.".into());
            return;
        }
        match output {
            Ok(Output::Report(report)) => {
                self.stale = false;
                self.report = Some((key, report));
                self.prepared = None;
                self.error = None;
            }
            Ok(Output::Prepared(prepared)) => {
                if self
                    .report
                    .as_ref()
                    .is_some_and(|(k, _)| k.request == prepared.report_request)
                    && !self.stale
                {
                    self.prepared = Some(prepared);
                    self.error = None;
                }
            }
            Err(error) => self.error = Some(error),
        }
    }
    pub(super) fn pump(&mut self, e: &Engine, ctx: &egui::Context) {
        if !self.stale
            && (self
                .report
                .as_ref()
                .is_some_and(|(k, _)| !k.matches(e, self.epoch, self.params))
                || self
                    .job
                    .as_ref()
                    .is_some_and(|j| !j.key.matches(e, self.epoch, self.params)))
        {
            self.invalidate();
        }
        if let Some(job) = self.job.take() {
            match job.receiver.try_recv() {
                Ok(output) => self.accept(job.key, output, e),
                Err(TryRecvError::Empty) => {
                    self.job = Some(job);
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
                Err(TryRecvError::Disconnected) => {
                    self.expected_request = None;
                    self.error = Some(
                        "Audio check worker stopped. Retry Check audio or Prepare excerpt.".into(),
                    )
                }
            }
        }
    }
    fn launch(
        &mut self,
        key: Arc<Key>,
        ctx: &egui::Context,
        work: impl FnOnce() -> Result<Output, String> + Send + 'static,
    ) {
        let (tx, receiver) = mpsc::channel();
        self.expected_request = Some(key.request);
        self.job = Some(Job { key, receiver });
        self.error = None;
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("audio-check".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                    .unwrap_or_else(|_| {
                        Err("Audio check worker stopped unexpectedly. Retry when ready.".into())
                    });
                let _ = tx.send(result);
                ctx.request_repaint();
            });
        if let Err(error) = spawned {
            self.job = None;
            self.expected_request = None;
            self.error = Some(format!(
                "Could not start audio check: {error}. Retry when ready."
            ));
        }
    }
    // Production entry points for later native integration. These are the
    // handlers used by the actual buttons, not a fabricated ready-state hook.
    fn request_check(&mut self, e: &Engine, ctx: &egui::Context) {
        if self.job.is_some() {
            return;
        }
        let args = match self.params.args() {
            Ok(v) => v,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        self.invalidate();
        let key = Arc::new(Key::capture(e, self.epoch, self.params));
        let mut own = key.engine(e.workdir.clone());
        self.launch(key, ctx, move || {
            let raw = own
                .call_from("detect_artifacts", &args, "you")
                .map_err(|e| format!("{e:#}"))?;
            Report::parse(raw).map(Output::Report)
        });
    }
    fn request_prepare(&mut self, index: usize, e: &Engine, ctx: &egui::Context) {
        if self.job.is_some() {
            return;
        }
        let Some((key, report)) = &self.report else {
            return;
        };
        if !self.current(key, e) {
            self.invalidate();
            return;
        }
        let bounds = match report.excerpt(index) {
            Ok(b) => b,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        let report_request = key.request;
        let mut preparation_key = (**key).clone();
        preparation_key.request = REQUEST.fetch_add(1, Ordering::Relaxed);
        let key = Arc::new(preparation_key);
        let mut own = key.engine(e.workdir.clone());
        self.prepared = None;
        self.launch(key, ctx, move || {
            // Exclusive reservation, even across multiple Studio processes.
            let path = reserve_preview(&own)?;
            let mut cleanup = PreviewCleanup(Some(path.clone()));
            let result = (|| {
                let value = own
                    .call_from(
                        "render_preview",
                        &json!({"start_s":bounds.start,
                    "end_s":bounds.end, "max_seconds":60, "tail_s":0,
                    "out":path, "spectrogram":false, "inline_audio":false}),
                        "you",
                    )
                    .map_err(|e| format!("{e:#}"))?;
                let actual_start = finite(&value["start_s"]).ok_or("Preview returned no start.")?;
                let seconds = finite(&value["seconds"]).ok_or("Preview returned no duration.")?;
                let actual = Bounds::checked(actual_start, actual_start + seconds)?;
                let audio = crate::samples::decode_stereo(&path).map_err(|e| format!("{e:#}"))?;
                if audio.0.is_empty() {
                    return Err("Preview contains no audio.".into());
                }
                Ok(Output::Prepared(Prepared {
                    report_request,
                    bounds: actual,
                    path: path.clone(),
                    audio: Arc::new(audio),
                    requested_bounds: bounds,
                    raw: value,
                }))
            })();
            if result.is_ok() {
                cleanup.0 = None;
            }
            result
        });
    }
    fn play(&mut self, e: &mut Engine) -> bool {
        if self.job.is_some() {
            return false;
        }
        let Some((key, _)) = &self.report else {
            return false;
        };
        // Called with the live engine locked, immediately before queueing.
        if !self.current(key, e) {
            self.invalidate();
            return false;
        }
        let Some(p) = &self.prepared else {
            return false;
        };
        if p.report_request != key.request {
            return false;
        }
        e.transport.pending.push(crate::engine::TransportCmd::Stop);
        e.transport.pending_audition = Some(Some(p.audio.clone()));
        true
    }
    pub(super) fn show(&mut self, ui: &mut egui::Ui, engine: &Arc<Mutex<Engine>>) -> Targets {
        let e = engine.lock().unwrap();
        self.pump(&e, ui.ctx());
        drop(e);
        let mut targets = Targets::default();
        let mut check = false;
        let mut prepare = None;
        let mut play = false;
        let mut stop = false;
        let previous = self.params;
        // Bound the entire pane, including controls, so long errors/events can
        // always be scrolled past to the last row without expanding the mixer.
        egui::ScrollArea::vertical().id_salt("audio_check_pane")
            .max_height((ui.available_height() * 0.45).clamp(140.0, 310.0))
            .auto_shrink([false, true]).show(ui, |ui| {
                targets.clip = ui.clip_rect();
                ui.label(RichText::new("Check audio").strong());
                ui.horizontal_wrapped(|ui| {
                    ui.label("Source: whole mix");
                    let response = ui.checkbox(&mut self.params.whole, "Full duration");
                    targets.whole = response.rect;
                    #[cfg(test)]
                    { targets.whole_id = Some(response.id); }
                    if !self.params.whole {
                        ui.label("Elapsed seconds");
                        ui.add(egui::DragValue::new(&mut self.params.start).speed(0.1).prefix("Start "));
                        ui.add(egui::DragValue::new(&mut self.params.end).speed(0.1).prefix("End "));
                    }
                    let response = ui.add_enabled(self.job.is_none(), egui::Button::new("Check audio"));
                    targets.check = response.rect;
                    check = response.clicked();
                });
                if previous != self.params { self.invalidate(); }
                ui.add(egui::Label::new("Manual report only. Prepare excerpt writes a uniquely reserved WAV in renders. Preparation does not play or stop music.").wrap());
                ui.horizontal_wrapped(|ui| {
                    let ready = self.report.as_ref().is_some_and(|(key,_)| self.current(key, &engine.lock().unwrap()))
                        && self.prepared.is_some() && self.job.is_none();
                    let response = ui.add_enabled(ready, egui::Button::new("Play excerpt"));
                    targets.play = response.rect;
                    targets.play_enabled = response.enabled();
                    if native_focus(ui.ctx(), "transport") { response.scroll_to_me(Some(egui::Align::TOP)); }
                    play = response.clicked();
                    let response = ui.button("Stop excerpt");
                    targets.stop = response.rect;
                    stop = response.clicked();
                    ui.add(egui::Label::new("Play stops song playback. Stop does not resume it.").wrap());
                });
                if self.job.is_some() { ui.horizontal_wrapped(|ui| { ui.spinner(); ui.label("Working on captured mix. You can keep editing."); }); }
                if let Some(error) = &self.error {
                    let response = ui.add(egui::Label::new(format!("Audio check: {error} Retry when ready.")).wrap());
                    targets.max_text_right = targets.max_text_right.max(response.rect.right());
                    targets.error = response.rect;
                    if native_focus(ui.ctx(), "error") { response.scroll_to_me(Some(egui::Align::TOP)); }
                }
                if self.stale && self.report.is_some() { targets.stale = ui.add(egui::Label::new("Previous report is stale. Check audio again. Play disabled.").wrap()).rect; }
                if let Some(p) = &self.prepared {
                    targets.prepared = ui.add(egui::Label::new(format!("Prepared {:.2} to {:.2} s: {}", p.bounds.start,p.bounds.end,p.path.display())).wrap()).rect;
                }
                if let Some((key, report)) = &self.report {
                    let current = self.current(key, &engine.lock().unwrap());
                    let captured = if key.params.whole { "Captured request: full mix".into() }
                        else { format!("Captured request: {:.2} to {:.2} s",key.params.start,key.params.end) };
                    ui.add(egui::Label::new(captured).wrap());
                    show_report(ui, report, current && self.job.is_none(), &mut prepare, &mut targets);
                }
            });
        if check {
            self.request_check(&engine.lock().unwrap(), ui.ctx());
        }
        if let Some(index) = prepare {
            self.request_prepare(index, &engine.lock().unwrap(), ui.ctx());
        }
        if play {
            self.play(&mut engine.lock().unwrap());
        }
        if stop {
            super::stop_comparison_preview(&mut engine.lock().unwrap());
        }
        if ui
            .ctx()
            .data(|d| d.get_temp::<bool>(egui::Id::new("audio_native_active")))
            .unwrap_or(false)
        {
            self.native_targets = Some(targets.clone());
        }
        targets
    }
}
// A failed or panicking worker must not leave its empty reservation behind.
struct PreviewCleanup(Option<PathBuf>);
impl Drop for PreviewCleanup {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}
fn reserve_preview(e: &Engine) -> Result<PathBuf, String> {
    let dir = e.renders_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dir = dir.canonicalize().map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    for _ in 0..16 {
        let id = REQUEST.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(
            "audio_check_{}_{}_{}.wav",
            std::process::id(),
            stamp,
            id
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("Could not reserve a unique excerpt file. Retry when ready.".into())
}
fn show_report(
    ui: &mut egui::Ui,
    report: &Report,
    current: bool,
    prepare: &mut Option<usize>,
    targets: &mut Targets,
) {
    let source = report.raw["source"].as_str().unwrap_or("Whole mix");
    let summary = ui.add(
        egui::Label::new(format!(
            "Source: {source}. {} events returned; {} warnings, {} failures in checked window.",
            report.events.len(),
            report.raw["warn"],
            report.raw["fail"]
        ))
        .wrap(),
    );
    targets.report_heading = summary.rect;
    if native_focus(ui.ctx(), "report") {
        summary.scroll_to_me(Some(egui::Align::TOP));
    }
    if let Some(b) = report.window {
        ui.label(format!(
            "Checked {:.2} to {:.2} s (absolute elapsed time)",
            b.start, b.end
        ));
    } else {
        ui.add(egui::Label::new("Effective window metadata unavailable. Ending applicability unverified; event times are absolute elapsed seconds.").wrap());
    }
    targets.notice = ui
        .add(
            egui::Label::new(if report.ending_checked {
                "Source ending checked in this window"
            } else {
                "Excerpt boundary, song ending not checked"
            })
            .wrap(),
        )
        .rect;
    if report.events.is_empty() {
        ui.add(
            egui::Label::new(
                "No findings returned for this window. This is not a verdict on the whole song.",
            )
            .wrap(),
        );
    }
    for (index, event) in report.events.iter().enumerate() {
        ui.separator();
        let range = event
            .end
            .map(|end| format!(" to {end:.2}"))
            .unwrap_or_default();
        let heading = ui.add(
            egui::Label::new(format!(
                "{} | {} | {:.2}{range} s",
                event.kind, event.severity, event.time
            ))
            .wrap(),
        );
        if index == 0 {
            targets.first_event = heading.rect;
        }
        targets.last_event = heading.rect;
        if let Some(track) = &event.track {
            let response = ui.add(egui::Label::new(format!("Track: {track}")).wrap());
            targets.max_text_right = targets.max_text_right.max(response.rect.right());
        }
        let response = ui.add(egui::Label::new(&event.detail).wrap());
        targets.last_detail = response.rect;
        targets.max_text_right = targets.max_text_right.max(response.rect.right());
        if let Ok(b) = report.excerpt(index) {
            ui.horizontal_wrapped(|ui| {
                let response = ui.add_enabled(current, egui::Button::new("Prepare excerpt"));
                targets.last_prepare = response.rect;
                if response.clicked() {
                    *prepare = Some(index);
                }
                ui.label(format!("Context {:.2} to {:.2} s", b.start, b.end));
            });
        }
    }
    let last = ui.label("End of returned event report");
    if ui
        .ctx()
        .data(|d| d.get_temp::<bool>(egui::Id::new("audio_native_last")))
        .unwrap_or(false)
    {
        last.scroll_to_me(Some(egui::Align::BOTTOM));
    }
    targets.last = last.rect;
}

fn native_terminal_visible(scenario: NativeScenario, t: &Targets, state: &State) -> bool {
    let seen = |r: egui::Rect| r.is_positive() && t.clip.contains_rect(r);
    if state.job.is_some() {
        return false;
    }
    match scenario {
        NativeScenario::Long => {
            state
                .report
                .as_ref()
                .is_some_and(|(_, r)| r.events.len() >= 21)
                && seen(t.last_event)
                && seen(t.last_detail)
                && seen(t.last)
        }
        NativeScenario::Stale => {
            state.stale
                && state.prepared.is_none()
                && !t.play_enabled
                && seen(t.play)
                && seen(t.stale)
        }
        NativeScenario::Prepared => {
            state.prepared.is_some() && t.play_enabled && seen(t.play) && seen(t.prepared)
        }
        NativeScenario::InvalidRange => state.error.is_some() && !t.play_enabled && seen(t.error),
        _ => {
            state.report.is_some()
                && !state.stale
                && seen(t.report_heading)
                && seen(t.notice)
                && seen(t.first_event)
        }
    }
}

fn native_focus(ctx: &egui::Context, focus: &str) -> bool {
    ctx.data(|d| d.get_temp::<String>(egui::Id::new("audio_native_focus")))
        .is_some_and(|value| value == focus)
}

/// Screenshot-only selectors. Not CLI/MCP actions and never active in ordinary Studio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NativeScenario {
    Populated,
    Interior,
    InvalidRange,
    Retry,
    Prepared,
    Stale,
    Long,
}
impl NativeScenario {
    fn select(
        value: Option<&str>,
        screenshot: bool,
        no_listen: bool,
    ) -> Result<Option<Self>, String> {
        let Some(value) = value else {
            return Ok(None);
        };
        if !screenshot || !no_listen {
            return Err("Audio scenarios require --screenshot and BEATBOX_NO_LISTEN=1.".into());
        }
        Self::parse(value).map(Some)
    }
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "populated" => Ok(Self::Populated),
            "interior" => Ok(Self::Interior),
            "invalid-range" => Ok(Self::InvalidRange),
            "retry" => Ok(Self::Retry),
            "prepared" => Ok(Self::Prepared),
            "stale" => Ok(Self::Stale),
            "long" => Ok(Self::Long),
            _ => Err(format!("Unknown audio screenshot scenario: {value}")),
        }
    }
}

/// Bounded adapter over the SAME handlers used by pane buttons. The only
/// accepted reports/errors/previews come through request_check/request_prepare
/// and pump. No ReadyReport/Output injection, sleeps, playback or listener.
pub(super) struct NativeCapture {
    scenario: NativeScenario,
    name: String,
    pub(super) scale: f32,
    started: std::time::Instant,
    phase: u8,
    settled: u8,
    actions: Vec<Value>,
}
impl NativeCapture {
    pub(super) fn from_environment(screenshot: bool) -> Result<Option<Self>, String> {
        let Some(name) = std::env::var_os("BEATBOX_AUDIO_SCENARIO") else {
            return Ok(None);
        };
        let name = name.into_string().map_err(|_| "Scenario must be UTF-8.")?;
        let scenario = NativeScenario::select(
            Some(&name),
            screenshot,
            std::env::var("BEATBOX_NO_LISTEN").as_deref() == Ok("1"),
        )?
        .ok_or("Missing scenario.")?;
        let scale = match std::env::var("BEATBOX_AUDIO_SCALE").as_deref() {
            Ok("2") => 2.0,
            Ok("1") | Err(_) => 1.0,
            _ => return Err("Audio capture scale must be 1 or 2.".into()),
        };
        Ok(Some(Self {
            scenario,
            name,
            scale,
            started: std::time::Instant::now(),
            phase: 0,
            settled: 0,
            actions: Vec::new(),
        }))
    }
    pub(super) fn activate(&self, ctx: &egui::Context) {
        ctx.set_pixels_per_point(self.scale);
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new("audio_native_active"), true);
            d.insert_temp(
                egui::Id::new("audio_native_last"),
                self.scenario == NativeScenario::Long,
            );
            let focus = match self.scenario {
                NativeScenario::Prepared | NativeScenario::Stale => "transport",
                NativeScenario::InvalidRange => "error",
                NativeScenario::Long => "last",
                _ => "report",
            };
            d.insert_temp(egui::Id::new("audio_native_focus"), focus.to_string());
        });
    }
    fn log(&mut self, action: &str, result: Value) {
        let row = json!({"elapsed_ms":self.started.elapsed().as_millis(), "action":action,"result":result});
        eprintln!("audio-native {row}");
        self.actions.push(row);
    }
    fn request(
        &mut self,
        state: &mut State,
        e: &Engine,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        self.log("request_check", state.params.args()?);
        state.request_check(e, ctx);
        if state.job.is_none() {
            return Err("Check worker was not launched.".into());
        }
        Ok(())
    }
    fn proof(&self, state: &State) -> Value {
        json!({"report":state.report.as_ref().map(|(_,r)|&r.raw), "error":state.error,
            "worker_pending":state.job.is_some(), "stale":state.stale,
            "play_widget_enabled":state.native_targets.as_ref().map(|t|t.play_enabled),
            "prepared":state.prepared.as_ref().map(|p|json!({"path":p.path,
                "start_s":p.bounds.start,"end_s":p.bounds.end,
                "decoded_frames":p.audio.0.len(),"decoded_seconds":p.audio.0.len() as f64 / crate::dsp::SR as f64,
                "request":{"start_s":p.requested_bounds.start,"end_s":p.requested_bounds.end,
                    "out":p.path,"max_seconds":60,"tail_s":0,"spectrogram":false,"inline_audio":false},
                "raw":p.raw}))})
    }
    /// Called AFTER painting the real workspace. True means a genuine terminal
    /// state has remained paintable for several frames, not just a click sent.
    pub(super) fn advance(
        &mut self,
        state: &mut State,
        e: &mut Engine,
        ctx: &egui::Context,
        song_ready: bool,
    ) -> Result<bool, String> {
        if self.started.elapsed() > std::time::Duration::from_secs(90) {
            return Err(format!(
                "Audio scenario {} exceeded 90 s in phase {}",
                self.name, self.phase
            ));
        }
        // Native capture never queues playback, including the baseline shot autoplay.
        if e.transport.playing
            || e.transport
                .pending_audition
                .as_ref()
                .is_some_and(|p| p.is_some())
        {
            return Err("Unexpected playback during audio capture.".into());
        }
        if !song_ready {
            return Ok(false);
        }
        if self.phase == 0 {
            self.log(
                "song_render_ready",
                json!({"revision":e.revision,"generation":e.project_load_generation}),
            );
            state.params = match self.scenario {
                NativeScenario::Interior => Parameters {
                    whole: false,
                    start: 0.0,
                    end: 1.0,
                },
                NativeScenario::InvalidRange | NativeScenario::Retry => Parameters {
                    whole: false,
                    start: 60000.0,
                    end: 60001.0,
                },
                _ => Parameters::default(),
            };
            self.request(state, e, ctx)?;
            self.phase = 1;
            return Ok(false);
        }
        if state.job.is_some() {
            return Ok(false);
        }
        if self.phase == 1
            && matches!(
                self.scenario,
                NativeScenario::InvalidRange | NativeScenario::Retry
            )
        {
            let error = state
                .error
                .as_ref()
                .ok_or("Expected a real invalid-range worker error.")?;
            if state.report.is_some()
                || !error.contains("artifact window starts outside the available source")
                || error.contains("internal error")
            {
                return Err(
                    "Invalid-range scenario did not produce an ordinary analysis error.".into(),
                );
            }
            if self.scenario == NativeScenario::Retry {
                self.log("invalid_range_error", json!(error));
                state.params = Parameters::default();
                self.request(state, e, ctx)?;
                self.phase = 2;
                return Ok(false);
            }
        } else {
            if let Some(error) = &state.error {
                return Err(format!("Audio worker failed: {error}"));
            }
            let (key, report) = state
                .report
                .as_ref()
                .ok_or("Worker finished without report.")?;
            if (self.scenario != NativeScenario::Stale || self.phase < 3) && !state.current(key, e)
            {
                return Err("Expected a current real report.".into());
            }
            if report.events.is_empty() {
                return Err(
                    "Authored project yielded no findings; populated evidence missing.".into(),
                );
            }
            if self.scenario == NativeScenario::Interior
                && (report.ending_checked
                    || report.window.is_none_or(|b| {
                        b.start != 0.0 || (b.end - 1.0).abs() > 1.0 / crate::dsp::SR
                    })
                    || report.source_end.is_none_or(|end| end <= 1.0))
            {
                return Err("Interior window applicability was not verified.".into());
            }
            if self.scenario == NativeScenario::Long && report.events.len() < 21 {
                return Err(
                    "Actual detector returned fewer than 21 findings; long evidence missing."
                        .into(),
                );
            }
            if matches!(
                self.scenario,
                NativeScenario::Prepared | NativeScenario::Stale
            ) && self.phase == 1
            {
                self.log("real_check_complete", self.proof(state));
                state.request_prepare(0, e, ctx);
                if state.job.is_none() {
                    return Err("Prepare worker was not launched.".into());
                }
                self.log(
                    "request_prepare",
                    json!({"event_index":0,"tail_s":0,"spectrogram":false,"inline_audio":false}),
                );
                self.phase = 2;
                return Ok(false);
            }
            if matches!(
                self.scenario,
                NativeScenario::Prepared | NativeScenario::Stale
            ) && self.phase == 2
            {
                if state.prepared.is_none() {
                    return Err("Prepare completed without decoded audio.".into());
                }
                if self.scenario == NativeScenario::Stale {
                    self.log("real_prepare_complete", self.proof(state));
                    let before = e.revision;
                    let result = e
                        .call_from(
                            "set_tempo",
                            &json!({"bpm":if e.project.bpm == 123.0 {124.0} else {123.0}}),
                            "mcp",
                        )
                        .map_err(|err| format!("Shared Engine edit failed: {err:#}"))?;
                    if e.revision == before {
                        return Err("Shared Engine edit did not advance revision.".into());
                    }
                    self.log("same_shared_engine_call_from_not_external_mcp",json!({"before_revision":before,"after_revision":e.revision,"result":result}));
                    state.pump(e, ctx);
                    self.phase = 3;
                    return Ok(false);
                }
            }
        }
        let Some(targets) = &state.native_targets else {
            return Ok(false);
        };
        let valid = native_terminal_visible(self.scenario, targets, state);
        self.settled = if valid {
            self.settled.saturating_add(1)
        } else {
            0
        };
        Ok(self.settled >= 5)
    }
    pub(super) fn manifest(
        &self,
        path: &std::path::Path,
        state: &State,
        ctx: &egui::Context,
        dimensions: [usize; 2],
    ) -> Result<(), String> {
        let rect = |r: egui::Rect| [r.min.x, r.min.y, r.max.x, r.max.y];
        let geometry = state.native_targets.as_ref().map(|t| {
            json!({
            "clip":rect(t.clip),"report_heading":rect(t.report_heading),
            "ending_notice":rect(t.notice),"first_event":rect(t.first_event),
            "last_event":rect(t.last_event),"last_detail":rect(t.last_detail),
            "last_marker":rect(t.last),"play":rect(t.play),"prepared":rect(t.prepared),
            "stale":rect(t.stale),"error":rect(t.error),
            "terminal_visible":native_terminal_visible(self.scenario,t,state)})
        });
        let doc = json!({"schema":1,"scenario":self.name,"scale":self.scale,
            "build_sha":option_env!("BEATBOX_BUILD_SHA"),
            "elapsed_ms":self.started.elapsed().as_millis(),"geometry":geometry,
            "captured_project":state.report.as_ref().map(|(key,_)|&key.project),
            "viewport_pixels":dimensions,"logical_viewport":[ctx.screen_rect().width(),ctx.screen_rect().height()],
            "pixels_per_point":ctx.pixels_per_point(),"actions":self.actions,"result":self.proof(state),
            "capture":"native viewport; production handlers; genuine worker completions",
            "autoplay":false,"external_mcp_proof":false,"pending_capture":"not attempted"});
        std::fs::write(
            path.with_extension("json"),
            serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
}

// All production handlers and UI must precede this first test gate so the
// manual-label scanner covers every new production label.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::TransportCmd;
    use egui::{Event as InputEvent, Modifiers, PointerButton, Pos2, Rect, Vec2};
    use std::time::{Duration, Instant};

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let id = REQUEST.fetch_add(1, Ordering::Relaxed);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "beatbox-audio-check-test-{}-{stamp}-{id}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn engine(&self) -> Engine {
            let mut e = Engine::new(self.0.clone());
            e.project.patterns[0].bars = 1;
            e.project.bpm = 240.0;
            e.project.master_effects.clear();
            e
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    // Synthetic reports are ONLY unit fixtures. No native scenario uses these.
    fn fixture(count: usize) -> Value {
        json!({"source":"mix", "fail":0, "warn":count,
            "window":{"start_s":0.0,"end_s":40.0,"source_end_s":60.0,"source_end_checked":false},
            "events": (0..count).map(|i| json!({"kind":"click","severity":"warn",
                "time_s":i as f32 + 0.25,"end_s":i as f32 + 0.5,
                "detail":format!("Event {i}: {}", "Long wrapping detail with 音楽. ".repeat(18)),
                "track": "An intentionally long track name for wrapping coverage ".repeat(4)})).collect::<Vec<_>>()})
    }
    fn ready(e: &Engine, count: usize) -> State {
        let mut state = State::default();
        let key = Arc::new(Key::capture(e, state.epoch, state.params));
        state.expected_request = Some(key.request);
        state.accept(
            key,
            Ok(Output::Report(Report::parse(fixture(count)).unwrap())),
            e,
        );
        state
    }
    fn prepared(state: &mut State) {
        let request = state.report.as_ref().unwrap().0.request;
        state.prepared = Some(Prepared {
            report_request: request,
            bounds: Bounds::checked(0.0, 1.0).unwrap(),
            path: PathBuf::from("unit-fixture-only.wav"),
            audio: Arc::new((vec![0.0; 8], vec![0.0; 8])),
            requested_bounds: Bounds::checked(0.0, 1.0).unwrap(),
            raw: json!({"unit_fixture_only":true}),
        });
    }
    fn wait(state: &mut State, e: &Engine, ctx: &egui::Context) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.job.is_some() && Instant::now() < deadline {
            state.pump(e, ctx);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(state.job.is_none(), "bounded worker wait expired");
    }

    #[test]
    fn native_selection_requires_screenshot_and_no_listener_and_rejects_typos() {
        assert_eq!(NativeScenario::select(None, false, false).unwrap(), None);
        for name in [
            "populated",
            "interior",
            "invalid-range",
            "retry",
            "prepared",
            "stale",
            "long",
        ] {
            assert!(NativeScenario::select(Some(name), true, true)
                .unwrap()
                .is_some());
            assert!(NativeScenario::select(Some(name), false, true).is_err());
            assert!(NativeScenario::select(Some(name), true, false).is_err());
        }
        for name in ["", "pending", "Populated", "ready-injection", "long "] {
            assert!(NativeScenario::select(Some(name), true, true).is_err());
        }
    }

    #[test]
    fn native_terminal_gates_require_visible_real_state_not_just_old_rectangles() {
        let dir = TestDir::new();
        let e = dir.engine();
        let mut state = ready(&e, 37); // unit-only report; never passed to a native window
        let visible = Rect::from_min_max(Pos2::new(10.0, 10.0), Pos2::new(90.0, 30.0));
        let mut t = Targets {
            clip: Rect::from_min_max(Pos2::ZERO, Pos2::new(100.0, 100.0)),
            report_heading: visible,
            notice: visible,
            first_event: visible,
            last_event: visible,
            last_detail: visible,
            last: visible,
            play: visible,
            prepared: visible,
            stale: visible,
            error: visible,
            ..Default::default()
        };
        assert!(native_terminal_visible(
            NativeScenario::Populated,
            &t,
            &state
        ));
        assert!(native_terminal_visible(NativeScenario::Long, &t, &state));
        t.last = Rect::from_min_max(Pos2::new(10.0, 110.0), Pos2::new(90.0, 130.0));
        assert!(!native_terminal_visible(NativeScenario::Long, &t, &state));
        assert!(!native_terminal_visible(
            NativeScenario::Prepared,
            &t,
            &state
        ));
        prepared(&mut state);
        assert!(!native_terminal_visible(
            NativeScenario::Prepared,
            &t,
            &state
        ));
        t.play_enabled = true;
        assert!(native_terminal_visible(
            NativeScenario::Prepared,
            &t,
            &state
        ));
        state.invalidate();
        assert!(!native_terminal_visible(NativeScenario::Stale, &t, &state));
        t.play_enabled = false;
        assert!(native_terminal_visible(NativeScenario::Stale, &t, &state));
        state.begin_native_frame();
        assert!(state.native_targets.is_none());
        state = ready(&e, 1);
        assert!(!native_terminal_visible(NativeScenario::Long, &t, &state));
        state.error = Some("ordinary invalid range".into());
        assert!(native_terminal_visible(
            NativeScenario::InvalidRange,
            &t,
            &state
        ));
        let (_tx, rx) = mpsc::channel();
        state.job = Some(Job {
            key: state.report.as_ref().unwrap().0.clone(),
            receiver: rx,
        });
        assert!(!native_terminal_visible(
            NativeScenario::InvalidRange,
            &t,
            &state
        ));
    }

    #[test]
    fn actual_invalid_range_worker_can_be_corrected_and_retried() {
        let dir = TestDir::new();
        let e = dir.engine();
        let ctx = egui::Context::default();
        let mut state = State {
            params: Parameters {
                whole: false,
                start: 60000.0,
                end: 60001.0,
            },
            ..Default::default()
        };
        state.request_check(&e, &ctx);
        assert!(state.job.is_some(), "not a local bounds-validation error");
        wait(&mut state, &e, &ctx);
        assert!(state.error.is_some());
        assert!(state
            .error
            .as_ref()
            .unwrap()
            .contains("artifact window starts outside the available source"));
        assert!(!state.error.as_ref().unwrap().contains("internal error"));
        assert!(state.report.is_none());
        state.params = Parameters::default();
        state.request_check(&e, &ctx);
        wait(&mut state, &e, &ctx);
        assert!(state.error.is_none(), "{:?}", state.error);
        assert!(state.report.is_some());
        assert!(e.transport.pending.is_empty());
        assert!(e.transport.pending_audition.is_none());
    }

    #[test]
    fn authored_impulse_project_real_detector_produces_long_report_without_injection() {
        let dir = TestDir::new();
        let mut e = dir.engine();
        e.project.patterns[0].bars = 4;
        let mut samples: Vec<f32> = (0..4 * 44100)
            .map(|i| 0.025 * (std::f32::consts::TAU * 220.0 * i as f32 / 44100.0).sin())
            .collect();
        for j in 0..32 {
            samples[((0.10 + 0.11 * j as f64) * 44100.0).round() as usize] += 0.7;
        }
        let path = dir.0.join("impulses.wav");
        std::fs::write(&path, crate::media::wav_bytes(&samples, &samples, 44100)).unwrap();
        e.call("import_sample", &json!({"path":path,"name":"impulses"}))
            .unwrap();
        e.call(
            "add_track",
            &json!({"name":"Diagnostic signal","preset":"pad","volume_db":0}),
        )
        .unwrap();
        e.call(
            "add_audio_clip",
            &json!({"track":"Diagnostic signal","sample":"impulses","start_beat":0,"length_s":4}),
        )
        .unwrap();
        let ctx = egui::Context::default();
        let mut state = State::default();
        state.request_check(&e, &ctx);
        wait(&mut state, &e, &ctx);
        assert!(state.error.is_none(), "{:?}", state.error);
        assert!(state.report.as_ref().unwrap().1.events.len() >= 21);
        assert!(e.transport.pending.is_empty());
        assert!(e.transport.pending_audition.is_none());
    }

    #[test]
    fn typed_elapsed_bounds_and_shared_arguments() {
        for (a, b) in [
            (0.0, 0.0),
            (2.0, 1.0),
            (-1.0, 2.0),
            (0.0, 86401.0),
            (f32::NAN, 2.0),
            (0.0, f32::INFINITY),
        ] {
            assert!(Bounds::checked(a, b).is_err());
        }
        let args = Parameters {
            whole: false,
            start: 1.0,
            end: 2.0,
        }
        .args()
        .unwrap();
        assert_eq!(args, json!({"start_s":1.0,"end_s":2.0,"max_events":128}));
        assert!(Parameters::default()
            .args()
            .unwrap()
            .get("tail_s")
            .is_none());
        assert!(Parameters::default()
            .args()
            .unwrap()
            .get("start_s")
            .is_none());
    }

    #[test]
    fn metadata_never_infers_song_ending_from_missing_or_legacy_label() {
        let mut raw = fixture(0);
        raw["window"] = json!("whole");
        let legacy = Report::parse(raw.clone()).unwrap();
        assert!(!legacy.ending_checked);
        assert!(legacy.window.is_none());
        raw["window"] =
            json!({"start_s":0.0,"end_s":60.0,"source_end_s":60.0,"source_end_checked":true});
        assert!(Report::parse(raw.clone()).unwrap().ending_checked);
        raw["window"]["end_s"] = json!(40.0);
        assert!(!Report::parse(raw.clone()).unwrap().ending_checked);
        raw["window"]["end_s"] = json!(60.0);
        raw["window_bounds"] = raw["window"].take();
        raw["window"] = json!("whole");
        assert!(
            Report::parse(raw).unwrap().ending_checked,
            "additive alias is supported too"
        );
    }

    #[test]
    fn complete_events_survive_long_utf8_details_without_byte_clipping() {
        let raw = fixture(37);
        assert!(raw.to_string().len() > 4000);
        let report = Report::parse(raw.clone()).unwrap();
        assert_eq!(report.raw, raw);
        assert_eq!(report.events.len(), 37);
        assert!(report.events[36].detail.starts_with("Event 36:"));
        assert!(report.events[36].detail.ends_with("音楽. "));
        let b = report.excerpt(36).unwrap();
        assert_eq!(b.start, 34.25);
        assert_eq!(b.end, 38.5);
        assert!(!report.ending_checked);
    }

    #[test]
    fn range_away_back_and_old_request_completions_stay_stale() {
        let dir = TestDir::new();
        let e = dir.engine();
        let mut state = ready(&e, 1);
        prepared(&mut state);
        let original = state.report.as_ref().unwrap().0.clone();
        state.expected_request = Some(original.request);
        let initial = state.params;
        state.params.whole = false;
        state.invalidate();
        state.params = initial;
        state.invalidate();
        state.accept(
            original.clone(),
            Ok(Output::Report(Report::parse(fixture(2)).unwrap())),
            &e,
        );
        assert_eq!(state.report.as_ref().unwrap().1.events.len(), 1);
        assert!(state.stale);
        assert!(state.prepared.is_none());
        let newer = Arc::new(Key::capture(&e, state.epoch, state.params));
        assert_ne!(newer.request, original.request);
        state.expected_request = Some(newer.request);
        state.accept(
            newer.clone(),
            Ok(Output::Report(Report::parse(fixture(3)).unwrap())),
            &e,
        );
        assert!(!state.stale);
        state.accept(
            original,
            Ok(Output::Report(Report::parse(fixture(4)).unwrap())),
            &e,
        );
        assert_eq!(state.report.as_ref().unwrap().0.request, newer.request);
        assert_eq!(state.report.as_ref().unwrap().1.events.len(), 3);
    }

    #[test]
    fn play_rechecks_mcp_revision_generation_and_unrevisioned_content_under_lock() {
        let dir = TestDir::new();
        let mut e = dir.engine();
        let mut state = ready(&e, 1);
        prepared(&mut state);
        e.call_from("set_tempo", &json!({"bpm":123}), "mcp")
            .unwrap();
        assert!(!state.play(&mut e));
        assert!(e.transport.pending.is_empty());
        let mut state = ready(&e, 1);
        prepared(&mut state);
        e.project_load_generation += 1; // identical-content reload is still a new session
        assert!(!state.play(&mut e));
        let mut state = ready(&e, 1);
        prepared(&mut state);
        e.project.name.push_str(" direct shared-engine edit");
        assert!(!state.play(&mut e));
        assert!(e.transport.pending_audition.is_none());
        assert!(e.transport.pending.is_empty());
    }

    #[test]
    fn replaced_sample_buffer_invalidates_without_relying_on_revision() {
        let dir = TestDir::new();
        let mut e = dir.engine();
        e.project.samples.push(crate::samples::SampleInfo {
            name: "fixture".into(),
            ..Default::default()
        });
        e.bank.insert("fixture", vec![0.0; 16]);
        let mut state = ready(&e, 1);
        prepared(&mut state);
        e.bank.insert("fixture", vec![0.0; 16]);
        assert!(!state.play(&mut e));
    }

    #[test]
    fn pending_edit_error_disconnect_and_panic_release_busy_without_loops() {
        let dir = TestDir::new();
        let mut e = dir.engine();
        let ctx = egui::Context::default();
        let mut state = ready(&e, 1);
        let old = state.report.as_ref().unwrap().0.clone();
        let (tx, receiver) = mpsc::channel();
        state.expected_request = Some(old.request);
        state.job = Some(Job {
            key: old.clone(),
            receiver,
        });
        e.project_load_generation += 1;
        state.pump(&e, &ctx);
        assert!(state.stale);
        tx.send(Ok(Output::Report(Report::parse(fixture(7)).unwrap())))
            .ok()
            .unwrap();
        state.pump(&e, &ctx);
        assert!(state.job.is_none());
        assert_eq!(state.report.as_ref().unwrap().1.events.len(), 1);
        let key = Arc::new(Key::capture(&e, state.epoch, state.params));
        let (tx, receiver) = mpsc::channel();
        state.expected_request = Some(key.request);
        state.job = Some(Job {
            key: key.clone(),
            receiver,
        });
        drop(tx);
        state.pump(&e, &ctx);
        assert!(state.job.is_none());
        assert!(state.error.as_deref().unwrap().contains("worker stopped"));
        state.launch(key.clone(), &ctx, || panic!("synthetic worker failure"));
        wait(&mut state, &e, &ctx);
        assert!(state.error.as_deref().unwrap().contains("unexpectedly"));
        state.launch(key, &ctx, || Err("retryable fixture failure".into()));
        wait(&mut state, &e, &ctx);
        assert_eq!(state.error.as_deref(), Some("retryable fixture failure"));
        state.pump(&e, &ctx);
        assert!(state.job.is_none(), "no automatic reanalysis loop");
    }

    #[test]
    fn real_prepare_is_file_only_then_play_explicitly_stops_and_auditions() {
        let dir = TestDir::new();
        let mut e = dir.engine();
        e.transport.playing = true;
        e.transport.pending = vec![TransportCmd::Play, TransportCmd::Seek(0.5)];
        let existing = Arc::new((vec![0.0; 4], vec![0.0; 4]));
        e.transport.pending_audition = Some(Some(existing.clone()));
        let project = e.project.clone();
        let history = e.history();
        let revision = e.revision;
        let generation = e.project_load_generation;
        let sentinel = dir.0.join("sentinel.wav");
        std::fs::write(&sentinel, b"do not change").unwrap();
        let mut state = ready(&e, 1);
        let ctx = egui::Context::default();
        state.request_prepare(0, &e, &ctx);
        assert!(state.job.is_some());
        wait(&mut state, &e, &ctx);
        assert!(state.prepared.is_some(), "{:?}", state.error);
        assert_eq!(e.project, project);
        assert_eq!(e.history(), history);
        assert_eq!(e.revision, revision);
        assert_eq!(e.project_load_generation, generation);
        assert!(e.transport.playing);
        assert_eq!(
            e.transport.pending,
            vec![TransportCmd::Play, TransportCmd::Seek(0.5)]
        );
        assert!(Arc::ptr_eq(
            e.transport
                .pending_audition
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap(),
            &existing
        ));
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"do not change");
        let paths: Vec<_> = std::fs::read_dir(e.renders_dir())
            .unwrap()
            .map(|p| p.unwrap().path())
            .collect();
        assert_eq!(
            paths.len(),
            1,
            "no spectrogram or undeclared companion files"
        );
        assert_eq!(paths[0].extension().unwrap(), "wav");
        assert!(!state.prepared.as_ref().unwrap().audio.0.is_empty());
        assert!(state.play(&mut e));
        assert_eq!(e.transport.pending.last(), Some(&TransportCmd::Stop));
        assert!(Arc::ptr_eq(
            e.transport
                .pending_audition
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap(),
            &state.prepared.as_ref().unwrap().audio
        ));
        let pending = e.transport.pending.clone();
        super::super::stop_comparison_preview(&mut e);
        assert!(matches!(e.transport.pending_audition, Some(None)));
        assert_eq!(
            e.transport.pending, pending,
            "Stop preview must not queue resume"
        );
    }

    #[test]
    fn actual_check_calls_shared_command_and_keeps_live_state() {
        let dir = TestDir::new();
        let e = dir.engine();
        let mut state = State::default();
        let mut own = Key::capture(&e, 0, state.params).engine(e.workdir.clone());
        let expected = own
            .call("detect_artifacts", &state.params.args().unwrap())
            .unwrap();
        let ctx = egui::Context::default();
        let project = e.project.clone();
        state.request_check(&e, &ctx);
        wait(&mut state, &e, &ctx);
        assert!(state.report.is_some(), "{:?}", state.error);
        assert_eq!(state.report.as_ref().unwrap().1.raw, expected);
        assert_eq!(e.project, project);
        assert!(e.transport.pending.is_empty());
        assert!(e.transport.pending_audition.is_none());
        assert!(
            !e.renders_dir().exists(),
            "detector is not a file-writing action"
        );
    }

    #[test]
    fn reserved_preview_paths_do_not_overwrite_existing_files() {
        let dir = TestDir::new();
        let e = dir.engine();
        let first = reserve_preview(&e).unwrap();
        std::fs::write(&first, b"reserved sentinel").unwrap();
        let second = reserve_preview(&e).unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&first).unwrap(), b"reserved sentinel");
    }

    struct Harness {
        ctx: egui::Context,
        frame: u64,
        size: Vec2,
    }
    impl Harness {
        fn new(width: f32, height: f32, scale: f32) -> Self {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            // The dimensions here are native pixels, not points. The 2x case
            // therefore exercises half the logical width and height.
            Self {
                ctx,
                frame: 0,
                size: egui::vec2(width / scale, height / scale),
            }
        }
        fn frame(
            &mut self,
            state: &mut State,
            engine: &Arc<Mutex<Engine>>,
            events: Vec<InputEvent>,
        ) -> Targets {
            let mut targets = Targets::default();
            let _ = self.ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
                    time: Some(self.frame as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        targets = state.show(ui, engine);
                    });
                },
            );
            self.frame += 1;
            targets
        }
        fn click(&mut self, state: &mut State, engine: &Arc<Mutex<Engine>>, at: Pos2) -> Targets {
            self.frame(
                state,
                engine,
                vec![
                    InputEvent::PointerMoved(at),
                    InputEvent::PointerButton {
                        pos: at,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: Modifiers::NONE,
                    },
                ],
            );
            self.frame(
                state,
                engine,
                vec![InputEvent::PointerButton {
                    pos: at,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                }],
            )
        }
        fn wheel(
            &mut self,
            state: &mut State,
            engine: &Arc<Mutex<Engine>>,
            at: Pos2,
            delta: f32,
        ) -> Targets {
            let mut targets = self.frame(
                state,
                engine,
                vec![
                    InputEvent::PointerMoved(at),
                    InputEvent::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, delta),
                        modifiers: Modifiers::NONE,
                    },
                ],
            );
            for _ in 0..12 {
                targets = self.frame(state, engine, vec![]);
            }
            targets
        }
    }

    #[test]
    fn real_egui_pointer_scroll_last_event_and_controls_at_native_sizes_and_2x() {
        for (width, height, scale) in [
            (1100.0, 700.0, 1.0),
            (1480.0, 920.0, 1.0),
            (1100.0, 700.0, 2.0),
            (1480.0, 920.0, 2.0),
        ] {
            let dir = TestDir::new();
            let engine = Arc::new(Mutex::new(dir.engine()));
            let mut state = ready(&engine.lock().unwrap(), 37);
            let mut h = Harness::new(width, height, scale);
            h.frame(&mut state, &engine, vec![]);
            let mut targets = h.frame(&mut state, &engine, vec![]);
            assert!(!targets.play.intersects(targets.stop));
            assert!(
                targets.max_text_right <= targets.clip.right() + 1.0,
                "long event/track labels wrap within pane"
            );
            assert!(targets.clip.contains(targets.check.center()));
            // Actual pointer checkbox changes away and back must latch stale.
            h.click(&mut state, &engine, targets.whole.center());
            assert!(!state.params.whole);
            targets = h.frame(&mut state, &engine, vec![]);
            h.click(&mut state, &engine, targets.whole.center());
            assert!(state.params.whole);
            assert!(state.stale);
            let mut reached = false;
            for _ in 0..80 {
                targets = h.wheel(&mut state, &engine, targets.clip.center(), -900.0);
                if targets.clip.contains(targets.last.center()) {
                    reached = true;
                    break;
                }
            }
            assert!(
                reached,
                "last of >20 long rows reachable at {width}x{height}/{scale}"
            );
            assert!(
                targets.last.right() <= targets.clip.right() + 1.0,
                "last label stays wrapped"
            );
            assert!(targets.clip.intersects(targets.last_prepare));
            // Scrolling back reaches transport and retry controls too.
            for _ in 0..80 {
                targets = h.wheel(&mut state, &engine, targets.clip.center(), 900.0);
                if targets.clip.contains(targets.check.center()) {
                    break;
                }
            }
            assert!(targets.clip.contains(targets.play.center()));
            h.click(&mut state, &engine, targets.play.center());
            assert!(
                engine.lock().unwrap().transport.pending.is_empty(),
                "stale Play is disabled"
            );
        }
    }

    #[test]
    fn real_keyboard_focus_and_space_change_range_and_latch_stale() {
        let dir = TestDir::new();
        let engine = Arc::new(Mutex::new(dir.engine()));
        let mut state = ready(&engine.lock().unwrap(), 1);
        prepared(&mut state);
        let mut h = Harness::new(1480.0, 920.0, 2.0);
        h.frame(&mut state, &engine, vec![]);
        let targets = h.frame(&mut state, &engine, vec![]);
        let whole_id = targets
            .whole_id
            .expect("production Full duration response ID");
        let key = |key, pressed| InputEvent::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        // The scroll area's drag interaction is also focusable. Traverse the
        // actual focus order, rather than assuming the first Tab is a checkbox.
        let mut focused = Vec::new();
        for _ in 0..32 {
            h.frame(&mut state, &engine, vec![key(egui::Key::Tab, true)]);
            let targets = h.frame(&mut state, &engine, vec![key(egui::Key::Tab, false)]);
            assert_eq!(
                targets.whole_id,
                Some(whole_id),
                "checkbox ID stays stable during Tab traversal"
            );
            let current = h.ctx.memory(|m| m.focused());
            focused.push(current);
            if current == Some(whole_id) {
                break;
            }
        }
        assert_eq!(
            h.ctx.memory(|m| m.focused()),
            Some(whole_id),
            "Tab must reach the exact Full duration checkbox before Space; focus trace: {focused:?}"
        );
        assert!(state.params.whole, "Tab alone must not change the range");
        assert!(!state.stale, "Tab alone must not invalidate the report");
        assert!(
            state.prepared.is_some(),
            "Tab alone must retain the excerpt"
        );
        assert!(engine.lock().unwrap().transport.pending.is_empty());
        h.frame(&mut state, &engine, vec![key(egui::Key::Space, true)]);
        h.frame(&mut state, &engine, vec![key(egui::Key::Space, false)]);
        assert!(
            !state.params.whole,
            "Space activates the exact focused Full duration checkbox"
        );
        assert!(state.stale);
        assert!(state.prepared.is_none());
        assert!(engine.lock().unwrap().transport.pending.is_empty());
    }

    #[test]
    fn real_egui_pending_error_retry_invalid_bounds_and_explicit_play() {
        let dir = TestDir::new();
        let engine = Arc::new(Mutex::new(dir.engine()));
        let mut state = ready(&engine.lock().unwrap(), 0);
        prepared(&mut state);
        let mut h = Harness::new(1100.0, 700.0, 1.0);
        h.frame(&mut state, &engine, vec![]);
        let mut targets = h.frame(&mut state, &engine, vec![]);
        h.click(&mut state, &engine, targets.play.center());
        assert_eq!(
            engine.lock().unwrap().transport.pending,
            vec![TransportCmd::Stop]
        );
        targets = h.frame(&mut state, &engine, vec![]);
        h.click(&mut state, &engine, targets.stop.center());
        assert!(matches!(
            engine.lock().unwrap().transport.pending_audition,
            Some(None)
        ));
        state.params = Parameters {
            whole: false,
            start: 2.0,
            end: 1.0,
        };
        state.invalidate();
        targets = h.frame(&mut state, &engine, vec![]);
        h.click(&mut state, &engine, targets.check.center());
        assert!(state.job.is_none());
        assert!(state.error.as_deref().unwrap().contains("elapsed seconds"));
        state.error = Some("Long retryable error with wrapping ".repeat(200));
        targets = h.frame(&mut state, &engine, vec![]);
        assert!(
            targets.max_text_right <= targets.clip.right() + 1.0,
            "long error stays wrapped"
        );
        assert!(
            targets.clip.contains(targets.check.center()),
            "retry controls precede long error"
        );
        let key = Arc::new(Key::capture(
            &engine.lock().unwrap(),
            state.epoch,
            state.params,
        ));
        let (tx, receiver) = mpsc::channel();
        state.expected_request = Some(key.request);
        state.job = Some(Job { key, receiver });
        targets = h.frame(&mut state, &engine, vec![]);
        h.click(&mut state, &engine, targets.check.center());
        assert!(state.job.is_some(), "pending Check is disabled");
        h.click(&mut state, &engine, targets.play.center());
        assert_eq!(
            engine.lock().unwrap().transport.pending,
            vec![TransportCmd::Stop],
            "pending Play disabled"
        );
        drop(tx);
        h.frame(&mut state, &engine, vec![]);
        assert!(state.job.is_none());
    }
}
