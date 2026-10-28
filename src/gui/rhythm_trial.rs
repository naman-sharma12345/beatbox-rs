//! A manual piano-to-browser bridge, not another comparison renderer or player.
//! Checkpoints and edits use the existing commands. Browser owns preparation,
//! checkpoint-content freshness, background completion, matching and playback.

use super::{piano::EuclideanControls, Studio};
use crate::{engine::Engine, project::Project};
use eframe::egui;
use serde_json::json;

// Pure production feedback transitions, shared by both load acknowledgment paths.
// No Trial is required: a failed Try can leave feedback without live trial metadata.
pub(super) mod feedback {
    pub(crate) fn attempted(status: &mut String, result: Result<bool, String>) {
        *status = match result {
            Ok(true) => "Rhythm written as one edit. Prepare and Play are separate clicks.".into(),
            Ok(false) => {
                "No notes changed. Undo trial is unavailable; the before checkpoint remains.".into()
            }
            Err(error) => error,
        };
    }

    // Keep and confirmed Undo dismiss historical feedback; failed Undo retains it.
    pub(crate) fn finished(status: &mut String, result: Result<(), String>) -> bool {
        match result {
            Ok(()) => {
                status.clear();
                true
            }
            Err(error) => {
                *status = error;
                false
            }
        }
    }

    // Called only after a successful local load or an observed generation change.
    // Do not compare Project content: even identical-content reloads are new sessions.
    pub(crate) fn loaded(status: &mut String, seen: &mut u64, generation: u64) {
        status.clear();
        *seen = generation;
    }

    pub(crate) fn observe_load(status: &mut String, seen: &mut u64, generation: u64) -> bool {
        if *seen == generation {
            return false;
        }
        loaded(status, seen, generation);
        true
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn try_keep_and_successful_undo_dismiss_status() {
            let mut status = String::new();
            for changed in [true, false] {
                attempted(&mut status, Ok(changed));
                assert!(!status.is_empty());
                assert!(finished(&mut status, Ok(())));
                assert!(status.is_empty());
            }
        }

        #[test]
        fn local_load_acknowledges_generation_and_clears_even_without_trial() {
            let mut status = String::new();
            let mut seen = 7;
            for result in [Ok(true), Ok(false), Err("Before checkpoint remains".into())] {
                attempted(&mut status, result);
                loaded(&mut status, &mut seen, 8);
                assert!(status.is_empty());
                assert_eq!(seen, 8);
            }
        }

        #[test]
        fn failed_undo_preserves_feedback_until_external_load() {
            let mut status = String::new();
            assert!(!finished(&mut status, Err("Use History".into())));
            assert_eq!(status, "Use History");
            let mut seen = u64::MAX;
            assert!(!observe_load(&mut status, &mut seen, u64::MAX));
            assert_eq!(status, "Use History");
            assert!(observe_load(&mut status, &mut seen, 0));
            assert!(status.is_empty());
            assert_eq!(seen, 0);
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Trial {
    checkpoint: String,
    track_id: String,
    track_name: String,
    pattern_id: String,
    pattern_name: String,
    pulses: u32,
    slots: u32,
    rotation: i32,
    replace: bool,
    generation: u64,
    revision: u64,
    before: Project,
    after: Project,
    undo_available: bool,
}

impl Trial {
    fn context_matches(&self, e: &Engine) -> bool {
        e.project_load_generation == self.generation
            && e.project
                .tracks
                .iter()
                .any(|t| t.id == self.track_id && t.name == self.track_name)
            && e.project
                .patterns
                .iter()
                .any(|p| p.id == self.pattern_id && p.name == self.pattern_name)
    }

    fn can_undo(&self, e: &Engine) -> bool {
        self.undo_available
            && self.context_matches(e)
            && e.revision == self.revision
            && e.project == self.after
    }

    fn checkpoint_intact(&self, e: &Engine) -> bool {
        e.project_load_generation == self.generation
            && e.snapshots
                .iter()
                .any(|(name, snapshot)| name == &self.checkpoint && snapshot.project == self.before)
    }

    fn observe(&mut self, e: &Engine) {
        // Latch off even if a later edit/Undo happens to recreate the content.
        self.undo_available = self.can_undo(e);
    }

    fn undo(&mut self, e: &mut Engine) -> anyhow::Result<()> {
        self.observe(e);
        anyhow::ensure!(
            self.can_undo(e),
            "Trial is no longer the latest edit. Use History; no checkpoint was restored."
        );
        let result = e.call_from("undo", &json!({"steps":1}), "you")?;
        self.undo_available = false;
        anyhow::ensure!(
            result["steps"] == 1 && e.project == self.before,
            "Undo did not confirm the expected trial. Check History; no checkpoint was restored."
        );
        Ok(())
    }
}

/// Validate the fixed manual controls and frame identity before capturing anything.
/// Do not run the edit on a scratch engine as a form of validation.
fn begin(
    e: &mut Engine,
    expected: &Project,
    generation: u64,
    track: usize,
    pattern: usize,
    controls: &EuclideanControls,
) -> anyhow::Result<Trial> {
    anyhow::ensure!(
        e.project_load_generation == generation && &e.project == expected,
        "Project changed. Reopen Compose and try again; nothing captured or written."
    );
    anyhow::ensure!(
        (1..=128).contains(&controls.slots) && controls.pulses <= controls.slots,
        "Use 1 to 128 subdivisions and no more hits than subdivisions."
    );
    let t = expected
        .tracks
        .get(track)
        .ok_or_else(|| anyhow::anyhow!("Choose an existing track."))?;
    let p = expected
        .patterns
        .get(pattern)
        .ok_or_else(|| anyhow::anyhow!("Choose an existing pattern."))?;
    // A source pattern is editable. Compiled playlist copies are not an honest
    // persistent target for this bridge.
    anyhow::ensure!(
        !p.name.to_lowercase().starts_with("pl:"),
        "Choose a source pattern, not a playlist copy."
    );
    anyhow::ensure!(
        !t.id.is_empty() && !p.id.is_empty(),
        "Reopen the project to assign track and pattern identities."
    );
    // Resolve exactly the identifiers that the real command will receive.
    anyhow::ensure!(
        e.project.track_index(&t.id)? == track && e.project.pattern_index(&p.id)? == pattern,
        "Track or pattern identity changed. Reopen Compose."
    );
    let args = json!({"track":t.id,"pattern":p.id,"pulses":controls.pulses,
        "slots":controls.slots,"rotation":controls.rotation,"replace":controls.replace});
    let before = e.project.clone();
    let revision = e.revision;
    let checkpoint = super::capture_mix_checkpoint(e)?;
    if let Err(error) = e.call_from("generate_euclidean", &args, "you") {
        anyhow::bail!("Rhythm edit failed: {error:#}. Before checkpoint {checkpoint} remains. Comparison was not opened.");
    }
    Ok(Trial {
        checkpoint,
        track_id: t.id.clone(),
        track_name: t.name.clone(),
        pattern_id: p.id.clone(),
        pattern_name: p.name.clone(),
        pulses: controls.pulses,
        slots: controls.slots,
        rotation: controls.rotation,
        replace: controls.replace,
        generation,
        revision: e.revision,
        before,
        after: e.project.clone(),
        undo_available: revision.checked_add(1) == Some(e.revision),
    })
}

fn excerpt_start(position: f32) -> f32 {
    if position.is_finite() {
        position.clamp(0.0, 86400.0)
    } else {
        0.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Action {
    Compare,
    Keep,
    Undo,
}

/// The exact controls used on screen, also driven with pointer events in tests.
fn actions(
    ui: &mut egui::Ui,
    can_compare: bool,
    can_undo: bool,
) -> (Option<Action>, [egui::Rect; 3]) {
    let mut action = None;
    let mut rects = [egui::Rect::NOTHING; 3];
    ui.horizontal_wrapped(|ui| {
        let compare = ui.add_enabled(can_compare, egui::Button::new("Open matched A/B"));
        let keep = ui.button("Keep change");
        let undo = ui.add_enabled(can_undo, egui::Button::new("Undo trial"));
        rects = [compare.rect, keep.rect, undo.rect];
        if compare.clicked() {
            action = Some(Action::Compare);
        }
        if keep.clicked() {
            action = Some(Action::Keep);
        }
        if undo.clicked() {
            action = Some(Action::Undo);
        }
    });
    (action, rects)
}

impl Studio {
    pub(super) fn start_rhythm_trial(&mut self, expected: &Project) {
        if self.piano.rhythm_trial.is_some() {
            return;
        }
        let result = begin(
            &mut self.engine.lock().unwrap(),
            expected,
            self.seen_project_load,
            self.selected,
            self.pattern,
            &self.piano.euclidean,
        );
        feedback::attempted(
            &mut self.piano.rhythm_trial_status,
            result
                .as_ref()
                .map(|trial| trial.undo_available)
                .map_err(|error| format!("{error:#}")),
        );
        match result {
            Ok(trial) => {
                // Only selection/range is handed to the existing browser. No
                // render, transport command or audition is started here.
                self.browser.ab_snapshot = trial.checkpoint.clone();
                self.browser.ab_start_s = excerpt_start(self.player.position_secs());
                self.browser.ab_length_s = 6.0;
                self.browser.ab_result = "Before selected. Choose an audible elapsed-time excerpt, then Prepare and Play.".into();
                self.browser.opened = true;
                self.browser.section = 2;
                self.workspace.request_view(5);
                self.piano.rhythm_trial = Some(trial);
            }
            Err(_) => {
                self.toast = Some((self.piano.rhythm_trial_status.clone(), 0.0, false));
            }
        }
    }

    pub(super) fn rhythm_trial_ui(&mut self, ctx: &egui::Context) {
        let Some(mut trial) = self.piano.rhythm_trial.take() else {
            return;
        };
        let (can_undo, can_compare) = {
            let e = self.engine.lock().unwrap();
            trial.observe(&e);
            (trial.can_undo(&e), trial.checkpoint_intact(&e))
        };
        if !can_compare && self.browser.ab_snapshot == trial.checkpoint {
            // A checkpoint can be overwritten without advancing revision. Do
            // not let the trial label an overwritten snapshot as its before.
            self.browser.ab_snapshot.clear();
            self.browser.ab_result =
                "Trial checkpoint changed or project was loaded. Choose a checkpoint explicitly."
                    .into();
        }
        let mut action = None;
        egui::Window::new("Rhythm trial")
            .default_width(420.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                egui::ScrollArea::vertical().id_salt("rhythm_trial_details").max_height(460.0).show(ui, |ui| {
                    ui.label(format!("Track: {} [{}]", trial.track_name, trial.track_id));
                    ui.label(format!("Pattern: {} [{}]", trial.pattern_name, trial.pattern_id));
                    ui.label(format!("{} hits / {} slots per bar, rotation {}. {} whole track in this pattern.",
                        trial.pulses, trial.slots, trial.rotation,
                        if trial.replace { "Replace" } else { "Layer onto" }));
                    ui.label(format!("Before: {} (whole project)", trial.checkpoint));
                    ui.label(&self.piano.rhythm_trial_status);
                    ui.label(format!("Excerpt: start {:.2}s, length {:.2}s. Adjust in MATCHED A/B before Prepare.",
                        self.browser.ab_start_s, self.browser.ab_length_s));
                    ui.label("Pattern edits affect every arranged occurrence. No song location is inferred. Choose an excerpt where this pattern is audible; an unarranged pattern may not be heard.");
                    ui.label("Open Browser > Effects & MIDI, then scroll to MATCHED A/B. Start and length are elapsed seconds, prefilled from transport, not a pattern position.");
                    ui.label("Comparison uses the current whole project at preparation, including any later edits.");
                    ui.label("Prepare writes comparison audio files and renders the full mix in the background. Click Play to hear before, a 1s gap, then now. Matching only turns levels down; silence is not matched. Play stops song transport. Stop preview does not resume it.");
                    ui.label("Keep only dismisses this panel. Undo trial uses one ordinary Undo, never whole-project checkpoint restore. Save writes the checkpoint companion file.");
                    if !can_undo {
                        ui.label("Undo trial unavailable: no change, intervening edit, or project/context changed. Use History for other edits.");
                    }
                    if !can_compare {
                        ui.label("Before checkpoint changed or is unavailable in this loaded project. Trial comparison is disabled.");
                    }
                });
                action = actions(ui, can_compare, can_undo).0;
            });
        match action {
            Some(Action::Compare) => {
                self.browser.ab_snapshot = trial.checkpoint.clone();
                self.browser.opened = true;
                self.browser.section = 2;
                self.workspace.request_view(5);
            }
            Some(Action::Keep) => {
                feedback::finished(&mut self.piano.rhythm_trial_status, Ok(()));
                return;
            }
            Some(Action::Undo) => {
                let result = trial
                    .undo(&mut self.engine.lock().unwrap())
                    .map_err(|error| format!("{error:#}"));
                if feedback::finished(&mut self.piano.rhythm_trial_status, result) {
                    return;
                }
            }
            None => {}
        }
        self.piano.rhythm_trial = Some(trial);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // No sample files, device, audio render, comparison files or GUI shell.
    fn fixture() -> Engine {
        let mut e = Engine::new(std::env::temp_dir().join("rhythm-trial-state-only"));
        e.call(
            "new_project",
            &json!({"bpm":120,"patterns":[
                {"name":"verse","bars":1}, {"name":"other","bars":1}
            ]}),
        )
        .unwrap();
        e.call("add_track", &json!({"name":"drums","preset":"kick"}))
            .unwrap();
        e.call("add_track", &json!({"name":"hat","preset":"hat"}))
            .unwrap();
        e.call(
            "add_notes",
            &json!({"track":"drums","pattern":"verse","notes":[
                {"start":3,"len":1,"pitch":60,"velocity":0.4}
            ]}),
        )
        .unwrap();
        e.call(
            "add_notes",
            &json!({"track":"hat","pattern":"verse","notes":[
                {"start":1,"len":1,"pitch":60}
            ]}),
        )
        .unwrap();
        e
    }

    fn start(e: &mut Engine, controls: &EuclideanControls) -> Trial {
        let p = e.project.clone();
        let generation = e.project_load_generation;
        begin(e, &p, generation, 0, 0, controls).unwrap()
    }

    #[test]
    fn exact_scope_one_undo_and_unique_persistent_before() {
        for replace in [true, false] {
            let mut e = fixture();
            e.call(
                "snapshot",
                &json!({"name":"MIX_CHECKPOINT_1","note":"keep"}),
            )
            .unwrap();
            let before = e.project.clone();
            let rev = e.revision;
            let history = e.history();
            let mut trial = start(
                &mut e,
                &EuclideanControls {
                    pulses: 3,
                    slots: 8,
                    rotation: -1,
                    replace,
                },
            );
            assert_eq!(trial.checkpoint, "mix_checkpoint_2");
            assert_eq!(trial.track_id, before.tracks[0].id);
            assert_eq!(trial.pattern_id, before.patterns[0].id);
            assert_eq!(e.revision, rev + 1);
            assert_eq!(e.history().0.len(), history.0.len() + 1);
            assert_eq!(e.project.tracks, before.tracks);
            assert_eq!(e.project.patterns[1], before.patterns[1]);
            assert_eq!(
                e.project.patterns[0].notes("hat"),
                before.patterns[0].notes("hat")
            );
            assert_eq!(
                e.project.patterns[0].notes("drums").len(),
                if replace { 3 } else { 4 }
            );
            assert_eq!(e.snapshots[0].1.note, "keep");
            assert!(trial.checkpoint_intact(&e));
            assert!(trial.can_undo(&e));
            assert!(e.transport.pending.is_empty());
            assert!(e.transport.pending_audition.is_none());
            let mut status = String::new();
            feedback::attempted(&mut status, Ok(trial.undo_available));
            let result = trial.undo(&mut e).map_err(|error| format!("{error:#}"));
            assert!(feedback::finished(&mut status, result));
            assert!(status.is_empty());
            assert_eq!(e.project, before);
            assert_eq!(e.snapshots.len(), 2);
            assert!(trial.checkpoint_intact(&e));
            assert!(!trial.can_undo(&e));
        }
    }

    #[test]
    fn validation_and_stale_frame_never_capture_or_edit() {
        let mut e = fixture();
        let p = e.project.clone();
        let generation = e.project_load_generation;
        let revision = e.revision;
        for (pulses, slots) in [(5, 0), (129, 129), (17, 16)] {
            let c = EuclideanControls {
                pulses,
                slots,
                ..Default::default()
            };
            assert!(begin(&mut e, &p, generation, 0, 0, &c).is_err());
        }
        assert!(begin(&mut e, &p, generation, 999, 0, &Default::default()).is_err());
        assert!(begin(&mut e, &p, generation, 0, 999, &Default::default()).is_err());
        assert!(begin(&mut e, &p, generation + 1, 0, 0, &Default::default()).is_err());
        assert_eq!(e.revision, revision);
        assert_eq!(e.project, p);
        assert!(e.snapshots.is_empty());
        e.call(
            "rename_pattern",
            &json!({"pattern":"verse","new_name":"renamed"}),
        )
        .unwrap();
        let renamed = e.project.clone();
        assert!(begin(&mut e, &p, generation, 0, 0, &Default::default()).is_err());
        assert_eq!(e.project, renamed);
        assert!(e.snapshots.is_empty());
        e.project.tracks.remove(0);
        let deleted = e.project.clone();
        assert!(begin(&mut e, &renamed, generation, 0, 0, &Default::default()).is_err());
        assert_eq!(e.project, deleted);
        assert!(e.snapshots.is_empty());
    }

    #[test]
    fn undo_shortcut_cannot_cross_edits_loads_or_unversioned_content_changes() {
        let mut e = fixture();
        let mut trial = start(&mut e, &Default::default());
        e.call("set_tempo", &json!({"bpm":137})).unwrap();
        let edited = e.project.clone();
        assert!(trial.undo(&mut e).is_err());
        assert_eq!(e.project, edited);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(e.project, trial.after);
        assert!(!trial.can_undo(&e));

        let mut e = fixture();
        let mut trial = start(&mut e, &Default::default());
        e.project.bpm += 1.0; // Content guard is stronger than revision alone.
        assert!(!trial.can_undo(&e));
        trial.observe(&e);
        e.project = trial.after.clone();
        assert!(!trial.can_undo(&e));

        let mut e = fixture();
        let mut trial = start(&mut e, &Default::default());
        e.project_load_generation += 1; // Even an identical reloaded song.
        assert!(trial.undo(&mut e).is_err());
        assert!(!trial.checkpoint_intact(&e));
    }

    #[test]
    fn checkpoint_contents_not_revision_and_noop_does_not_undo_older_work() {
        let mut e = fixture();
        let trial = start(&mut e, &Default::default());
        let revision = e.revision;
        // Snapshot overwrite does not increment the project revision.
        e.call("snapshot", &json!({"name":trial.checkpoint}))
            .unwrap();
        assert_eq!(e.revision, revision);
        assert!(!trial.checkpoint_intact(&e));
        assert!(trial.can_undo(&e)); // Undo is history, not checkpoint restore.

        let mut e = fixture();
        let c = EuclideanControls {
            pulses: 0,
            replace: false,
            ..Default::default()
        };
        let before = e.project.clone();
        let history = e.history();
        let mut trial = start(&mut e, &c);
        assert_eq!(e.project, before);
        assert_eq!(e.history(), history);
        assert!(!trial.can_undo(&e));
        assert!(trial.undo(&mut e).is_err());
        assert_eq!(e.project, before);
        assert_eq!(e.snapshots.len(), 1);
    }

    #[test]
    fn production_feedback_try_keep_and_load_same_or_different_content() {
        // Exercise real Engine edits/save/load plus the same feedback transitions
        // used by Studio. No window, player, renderer or device is constructed.
        for different in [false, true] {
            for keep in [false, true] {
                let mut e = fixture();
                let dir = std::env::temp_dir().join(format!(
                    "rhythm-feedback-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
                std::fs::create_dir_all(&dir).unwrap();
                e.workdir = dir.clone();
                let trial = start(&mut e, &Default::default());
                let mut status = String::new();
                feedback::attempted(&mut status, Ok(trial.undo_available));
                assert!(!status.is_empty());
                if keep {
                    assert!(feedback::finished(&mut status, Ok(())));
                    assert!(status.is_empty());
                }
                let path = dir.join("reload.beatbox.json");
                e.call("save_project", &json!({"path":path})).unwrap();
                let saved = e.project.clone();
                if different {
                    e.call("set_tempo", &json!({"bpm":137})).unwrap();
                    assert_ne!(e.project, saved);
                }
                let mut seen = e.project_load_generation;
                e.call("load_project", &json!({"path":path})).unwrap();
                assert_ne!(seen, e.project_load_generation);
                assert_eq!(e.project, saved);
                // Successful local Open acknowledges immediately; external
                // observation reaches the same reset only on generation change.
                feedback::loaded(&mut status, &mut seen, e.project_load_generation);
                assert!(status.is_empty());
                assert_eq!(seen, e.project_load_generation);
                assert!(!trial.can_undo(&e));
                assert!(!trial.checkpoint_intact(&e));
                std::fs::remove_dir_all(dir).unwrap();
            }
        }
    }

    #[test]
    fn production_failed_try_and_failed_load_preserve_feedback_until_successful_load() {
        let mut e = fixture();
        let dir = std::env::temp_dir().join(format!(
            "rhythm-failed-feedback-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        e.workdir = dir.clone();
        let path = dir.join("reload.beatbox.json");
        e.call("save_project", &json!({"path":path})).unwrap();
        let expected = e.project.clone();
        let mut seen = e.project_load_generation;
        let result = begin(
            &mut e,
            &expected,
            seen,
            0,
            0,
            &EuclideanControls {
                slots: 0,
                ..Default::default()
            },
        );
        assert!(result.is_err());
        let mut status = String::new();
        feedback::attempted(
            &mut status,
            result
                .as_ref()
                .map(|trial| trial.undo_available)
                .map_err(|error| format!("{error:#}")),
        );
        let failure = status.clone();
        assert!(!failure.is_empty());
        assert!(e
            .call("load_project", &json!({"path":dir.join("missing.json")}))
            .is_err());
        assert_eq!(e.project_load_generation, seen);
        assert_eq!(e.project, expected);
        // Same condition as Studio's external generation observer: a failed
        // load must not acknowledge/reset the current project's failure.
        assert!(!feedback::observe_load(
            &mut status,
            &mut seen,
            e.project_load_generation
        ));
        assert_eq!(status, failure);
        e.call("load_project", &json!({"path":path})).unwrap();
        assert!(feedback::observe_load(
            &mut status,
            &mut seen,
            e.project_load_generation
        ));
        assert!(status.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn excerpt_prefill_is_bounded_elapsed_seconds() {
        for (input, expected) in [
            (f32::NAN, 0.0),
            (f32::INFINITY, 0.0),
            (-5.0, 0.0),
            (2.25, 2.25),
            (100000.0, 86400.0),
        ] {
            assert_eq!(excerpt_start(input), expected);
        }
    }

    fn click_action_at_scale(
        index: usize,
        can_compare: bool,
        can_undo: bool,
        scale: f32,
    ) -> Option<Action> {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(scale);
        let mut rects = [egui::Rect::NOTHING; 3];
        let mut clicked = None;
        for frame in 0..4 {
            let pos = rects[index].center();
            let events = if frame < 2 {
                vec![]
            } else {
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: frame == 2,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]
            };
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(700.0, 300.0),
                    )),
                    time: Some(frame as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let (action, bounds) = actions(ui, can_compare, can_undo);
                        rects = bounds;
                        if action.is_some() {
                            clicked = action;
                        }
                    });
                },
            );
        }
        clicked
    }

    fn click_action(index: usize, can_compare: bool, can_undo: bool) -> Option<Action> {
        click_action_at_scale(index, can_compare, can_undo, 1.0)
    }

    #[test]
    fn real_pointer_events_obey_enabled_actions() {
        for scale in [1.0, 2.0] {
            assert_eq!(
                click_action_at_scale(0, true, true, scale),
                Some(Action::Compare)
            );
            assert_eq!(
                click_action_at_scale(1, false, false, scale),
                Some(Action::Keep)
            );
            assert_eq!(
                click_action_at_scale(2, true, true, scale),
                Some(Action::Undo)
            );
            assert_eq!(click_action_at_scale(0, false, true, scale), None);
            assert_eq!(click_action_at_scale(2, true, false, scale), None);
        }
    }

    #[test]
    fn pointer_undo_uses_ordinary_history_and_keep_is_only_dismissal() {
        let mut e = fixture();
        let mut trial = start(&mut e, &Default::default());
        let after = e.project.clone();
        let history = e.history();
        assert_eq!(
            click_action(1, true, trial.can_undo(&e)),
            Some(Action::Keep)
        );
        assert_eq!(e.project, after);
        assert_eq!(e.history(), history);
        // Same dispatch as the panel's Undo arm; no Studio/device construction.
        assert_eq!(
            click_action(2, true, trial.can_undo(&e)),
            Some(Action::Undo)
        );
        trial.undo(&mut e).unwrap();
        assert_eq!(e.project, trial.before);
        assert!(e.transport.pending.is_empty());
        assert!(e.transport.pending_audition.is_none());
    }
}
