//! Playback-render ownership, independent of rendering, audio devices and egui.
//!
//! Engine mutations must advance revision; whole-project loads must advance
//! load_generation even when revisions collide. Direct unversioned project or
//! sample-file replacement is not observable here. A stuck (not disconnected)
//! worker cannot safely be cancelled by dropping its receiver: keep one worker
//! in flight rather than starting unbounded replacement renders.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Source {
    pub revision: u64,
    pub load_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Key {
    pub source: Source,
    sequence: u64,
}

pub(super) struct Message<T> {
    pub key: Key,
    pub result: Result<T, String>,
}

/// Empty means still running, disconnection is a terminal result.
pub(super) fn poll<T>(
    key: Key,
    receiver: &std::sync::mpsc::Receiver<Message<T>>,
) -> Option<Message<T>> {
    match receiver.try_recv() {
        Ok(message) => Some(message),
        Err(std::sync::mpsc::TryRecvError::Empty) => None,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Message {
            key,
            result: Err("Sound preparation stopped unexpectedly. Try again.".into()),
        }),
    }
}

/// Contain unwinding panics in all preparation stages, not just the renderer.
/// Process aborts and allocation aborts cannot be recovered by this boundary.
pub(super) fn run<T>(key: Key, work: impl FnOnce() -> Result<T, String>) -> Message<T> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
        .unwrap_or_else(|_| Err("Sound preparation stopped unexpectedly. Try again.".into()));
    Message { key, result }
}

pub(super) enum Completion<T> {
    Install(T),
    Failed,
    Discard,
}

#[derive(Default)]
pub(super) struct State {
    active: Option<Key>,
    installed: Option<Source>,
    failure: Option<(Source, String)>,
    sequence: u64,
}

impl State {
    pub fn can_start(&self, current: Source) -> bool {
        self.active.is_none()
            && self.installed != Some(current)
            && self.failure.as_ref().map(|(source, _)| *source) != Some(current)
    }

    pub fn begin(&mut self, current: Source) -> Key {
        assert!(self.can_start(current));
        self.sequence = self.sequence.wrapping_add(1);
        let key = Key {
            source: current,
            sequence: self.sequence,
        };
        self.active = Some(key);
        self.failure = None;
        key
    }

    /// Also used for provisional live splices; old jobs cannot replace audio.
    pub fn accepts(&self, key: Key, current: Source) -> bool {
        self.active == Some(key) && key.source == current
    }

    pub fn finish<T>(&mut self, message: Message<T>, current: Source) -> Completion<T> {
        // A late message must not release a newer job's busy state.
        if self.active != Some(message.key) {
            return Completion::Discard;
        }
        self.active = None;
        if message.key.source != current {
            return Completion::Discard;
        }
        match message.result {
            Ok(value) => {
                self.installed = Some(current);
                self.failure = None;
                Completion::Install(value)
            }
            Err(error) => {
                self.failure = Some((current, error));
                Completion::Failed
            }
        }
    }

    pub fn error(&self, current: Source) -> Option<&str> {
        self.failure
            .as_ref()
            .filter(|(source, _)| *source == current)
            .map(|(_, error)| error.as_str())
    }

    pub fn retry(&mut self) {
        self.failure = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(revision: u64, load_generation: u64) -> Source {
        Source {
            revision,
            load_generation,
        }
    }

    #[test]
    fn success_installs_only_once() {
        let mut state = State::default();
        let current = source(4, 2);
        let key = state.begin(current);
        assert!(!state.can_start(current));
        assert!(matches!(
            state.finish(
                Message {
                    key,
                    result: Ok(42)
                },
                current
            ),
            Completion::Install(42)
        ));
        assert!(!state.can_start(current));
        assert!(matches!(
            state.finish(
                Message {
                    key,
                    result: Ok(99)
                },
                current
            ),
            Completion::Discard
        ));
    }

    #[test]
    fn failure_preserves_audio_and_position_and_retry_can_succeed() {
        let mut state = State::default();
        let old = source(1, 0);
        let key = state.begin(old);
        let mut playing = ("last completed mix", 12.5_f32);
        if let Completion::Install(mix) = state.finish(
            Message {
                key,
                result: Ok(playing.0),
            },
            old,
        ) {
            playing.0 = mix;
        }
        let current = source(2, 0);
        let key = state.begin(current);
        if let Completion::Install(mix) = state.finish(
            Message {
                key,
                result: Err("Missing sample".into()),
            },
            current,
        ) {
            playing.0 = mix;
        }
        assert_eq!(playing, ("last completed mix", 12.5));
        assert_eq!(state.error(current), Some("Missing sample"));
        assert!(state.active.is_none());
        assert!(!state.can_start(current)); // no automatic failure loop
        state.retry();
        assert!(state.can_start(current));
        let retry = state.begin(current);
        assert_ne!(retry, key);
        assert!(matches!(
            state.finish(
                Message {
                    key,
                    result: Ok("late")
                },
                current
            ),
            Completion::Discard
        ));
        assert_eq!(state.active, Some(retry));
        assert!(matches!(
            state.finish(
                Message {
                    key: retry,
                    result: Ok("fixed")
                },
                current
            ),
            Completion::Install("fixed")
        ));
    }

    #[test]
    fn disconnect_is_a_terminal_failure_not_a_busy_wedge() {
        let mut state = State::default();
        let current = source(3, 0);
        let key = state.begin(current);
        let (tx, rx) = std::sync::mpsc::channel::<Message<()>>();
        assert!(poll(key, &rx).is_none()); // empty is still busy
        assert_eq!(state.active, Some(key));
        drop(tx);
        let message = poll(key, &rx).expect("disconnected worker is terminal");
        assert!(matches!(state.finish(message, current), Completion::Failed));
        assert!(state.active.is_none());
        state.retry();
        assert!(state.can_start(current));
    }

    #[test]
    fn unwinding_worker_panic_clears_busy_and_allows_retry() {
        let mut state = State::default();
        let current = source(3, 0);
        let key = state.begin(current);
        let message = run::<()>(key, || panic!("synthetic preparation panic"));
        assert_eq!(message.key, key);
        assert!(message.result.is_err());
        assert!(matches!(state.finish(message, current), Completion::Failed));
        assert!(state.active.is_none());
        state.retry();
        let key = state.begin(current);
        assert!(matches!(
            state.finish(run(key, || Ok(12)), current),
            Completion::Install(12)
        ));
    }

    #[test]
    fn stale_revision_and_new_project_results_are_discarded() {
        for current in [source(8, 1), source(7, 2)] {
            let mut state = State::default();
            let key = state.begin(source(7, 1));
            assert!(!state.accepts(key, current)); // includes live splices
            assert!(matches!(
                state.finish(
                    Message {
                        key,
                        result: Ok("old song")
                    },
                    current
                ),
                Completion::Discard
            ));
            assert!(state.can_start(current));
            let latest = state.begin(current);
            assert!(matches!(
                state.finish(
                    Message {
                        key: latest,
                        result: Ok("latest")
                    },
                    current
                ),
                Completion::Install("latest")
            ));
        }
    }

    #[test]
    fn stale_failure_does_not_block_edits_or_reopened_project() {
        let mut state = State::default();
        let old = source(7, 1);
        let key = state.begin(old);
        let current = source(7, 2); // reload with the same revision
        assert!(matches!(
            state.finish::<()>(
                Message {
                    key,
                    result: Err("old error".into())
                },
                current
            ),
            Completion::Discard
        ));
        assert!(state.error(current).is_none());
        assert!(state.can_start(current));
        let key = state.begin(current);
        let _ = state.finish::<()>(
            Message {
                key,
                result: Err("failed".into()),
            },
            current,
        );
        assert!(state.can_start(source(8, 2))); // an edit can recover without Retry
        assert!(state.can_start(source(7, 3))); // likewise Open
    }

    #[test]
    fn active_job_bounds_work_and_splices_follow_its_identity() {
        let mut state = State::default();
        let current = source(5, 0);
        let key = state.begin(current);
        assert!(state.accepts(key, current));
        assert!(!state.can_start(source(6, 0))); // wait, do not launch a second worker
        assert!(!state.can_start(source(5, 1))); // likewise during Open
        let failed = run::<()>(key, || Err("Missing sample".into()));
        assert!(matches!(state.finish(failed, current), Completion::Failed));
        assert!(!state.accepts(key, current)); // a queued splice cannot outlive failure
        assert!(state.can_start(source(6, 0)));
    }

    #[test]
    fn gui_wiring_preserves_completed_mix_and_does_not_stop_or_seek_on_failure() {
        let source = include_str!("mod.rs");
        let pump = source
            .split("fn pump_render(")
            .nth(1)
            .unwrap()
            .split("fn render_recovery_ui(")
            .next()
            .unwrap();
        let failure = pump
            .split("Completion::Failed => {")
            .nth(1)
            .unwrap()
            .split("Completion::Discard =>")
            .next()
            .unwrap();
        assert!(failure.contains("self.player.set_mix(r.mix.clone(), r.loop_secs)"));
        for forbidden in ["self.rendered =", ".stop(", ".seek(", ".play("] {
            assert!(
                !failure.contains(forbidden),
                "failure must preserve transport: {forbidden}"
            );
        }
        assert!(pump.contains("render_job::poll(*key, rx)"));
        assert!(pump.contains("render_job::run(key, ||"));
        assert!(pump.contains("self.render_state.accepts(*key, current)"));
        assert!(
            pump.find("if let Some(message) = completed").unwrap()
                < pump.find("if let Some((key, rx)) = &self.live").unwrap()
        );
        let recovery = source
            .split("fn render_recovery_ui(")
            .nth(1)
            .unwrap()
            .split("fn save_tooltip(")
            .next()
            .unwrap();
        assert!(recovery.contains("Retry sound"));
        assert!(recovery.contains("self.render_state.retry()"));
        assert!(recovery.contains("ui.collapsing(\"Details\""));
    }

    #[test]
    fn installed_revision_does_not_hide_a_same_revision_project_load() {
        let mut state = State::default();
        let old = source(1, 1);
        let key = state.begin(old);
        let _ = state.finish(
            Message {
                key,
                result: Ok(()),
            },
            old,
        );
        assert!(state.can_start(source(1, 2)));
    }
}
