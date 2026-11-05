//! Bounded screenshot-only presentation fixture; never an input driver.
use super::*;

const SIZE: &str = "BEATBOX_CAPTURE_SIZE";
const SCALE: &str = "BEATBOX_CAPTURE_UI_SCALE";
const INSPECTOR: &str = "BEATBOX_CAPTURE_INSPECTOR";
const LEGACY: [&str; 6] = [
    "BEATBOX_NARROW_SHOT",
    "BEATBOX_AUDIO_SCENARIO",
    "BEATBOX_AUDIO_SCALE",
    "BEATBOX_ARRANGEMENT_CAPTURE",
    "BEATBOX_ARRANGEMENT_CLIP_ID",
    "BEATBOX_ARRANGEMENT_ACTION_HASH",
];

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Config {
    pub size: [u32; 2],
    pub scale: u32,
    pub inspector: bool,
    pub view: usize,
}
impl Config {
    fn parse(
        screenshot: bool,
        view: Option<&str>,
        get: impl Fn(&str) -> Option<String>,
    ) -> anyhow::Result<Option<Self>> {
        if !screenshot {
            return Ok(None);
        }
        let Some(size) = get(SIZE) else {
            anyhow::ensure!(
                get(SCALE).is_none() && get(INSPECTOR).is_none(),
                "partial P0A selectors"
            );
            return Ok(None);
        };
        anyhow::ensure!(
            LEGACY.iter().all(|k| get(k).is_none()),
            "conflicting legacy capture selector"
        );
        anyhow::ensure!(
            get("BEATBOX_NO_LISTEN").as_deref() == Some("1"),
            "P0A requires BEATBOX_NO_LISTEN=1"
        );
        let size = match size.as_str() {
            "1100x700" => [1100, 700],
            "1440x900" => [1440, 900],
            _ => anyhow::bail!("invalid P0A size"),
        };
        let scale = match get(SCALE).as_deref() {
            Some("1") => 1,
            Some("2") => 2,
            _ => anyhow::bail!("P0A scale must be 1 or 2"),
        };
        let inspector = match get(INSPECTOR).as_deref() {
            None | Some("0") => false,
            Some("1") => true,
            _ => anyhow::bail!("invalid P0A Inspector"),
        };
        let view = match view {
            Some("sequencer") => 0,
            Some("playlist") => 3,
            Some("mixer") => 1,
            _ => anyhow::bail!("P0A requires an exact raw view"),
        };
        Ok(Some(Self {
            size,
            scale,
            inspector,
            view,
        }))
    }
    fn validate_geometry(&self, pixels: [usize; 2], ppp: f32, logical: Vec2) -> anyhow::Result<()> {
        anyhow::ensure!(
            pixels
                == [
                    (self.size[0] * self.scale) as usize,
                    (self.size[1] * self.scale) as usize
                ],
            "P0A pixel geometry"
        );
        anyhow::ensure!(
            (ppp - self.scale as f32).abs() < 0.01
                && (logical.x - self.size[0] as f32).abs() < 1.1
                && (logical.y - self.size[1] as f32).abs() < 1.1,
            "P0A logical geometry"
        );
        Ok(())
    }
    pub fn environment(screenshot: bool, view: Option<&str>) -> anyhow::Result<Option<Self>> {
        // Non-Unicode values must also fail strict tokens, rather than look absent.
        Self::parse(screenshot, view, |k| {
            std::env::var_os(k).map(|v| v.to_string_lossy().into_owned())
        })
    }
}

#[derive(PartialEq)]
struct Observation {
    project: Vec<u8>,
    ledger: Value,
}
impl Observation {
    fn read(e: &Engine) -> anyhow::Result<Self> {
        Ok(Self {
            project: serde_json::to_vec(&e.project)?,
            ledger: json!({"revision":e.revision,"load_generation":e.project_load_generation,
                "depths":e.undo_depth(),"labels":e.history(),
                "log":e.log.iter().map(|l| json!({"tool":l.tool,"summary":l.summary,
                    "ok":l.ok,"source":l.source})).collect::<Vec<_>>(),
                "snapshots":e.snapshots.iter().map(|(name,s)| json!({"name":name,
                    "project":s.project,"note":s.note,"revision":s.revision})).collect::<Vec<_>>() }),
        })
    }
    fn artifact(&self) -> Value {
        json!({"project_serialized_sha256":crate::palette::sha256_hex(&self.project),
            "project_serialized_bytes":self.project.len(),"public_ledger":self.ledger})
    }
}

pub(super) struct Capture {
    pub config: Config,
    baseline: Observation,
    pub seeded: bool,
}
impl Capture {
    pub fn new(config: Config, engine: &Engine) -> anyhow::Result<Self> {
        Ok(Self {
            config,
            baseline: Observation::read(engine)?,
            seeded: false,
        })
    }
    pub fn manifest(
        &self,
        studio: &Studio,
        ctx: &egui::Context,
        path: &std::path::Path,
        pixels: [usize; 2],
    ) -> Result<(), String> {
        self.write_manifest(studio, ctx, path, pixels)
            .map_err(|e| e.to_string())
    }
    fn write_manifest(
        &self,
        s: &Studio,
        ctx: &egui::Context,
        path: &std::path::Path,
        pixels: [usize; 2],
    ) -> anyhow::Result<()> {
        let e = s.engine.lock().unwrap();
        let final_state = Observation::read(&e)?;
        anyhow::ensure!(
            self.baseline == final_state,
            "P0A project/public ledger drift"
        );
        let c = &self.config;
        let logical = ctx.screen_rect().size();
        c.validate_geometry(pixels, ctx.pixels_per_point(), logical)?;
        let rendered = s
            .rendered
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("P0A missing render"))?;
        anyhow::ensure!(
            s.shot_requested
                && self.seeded
                && s.view == c.view
                && rendered.rev == e.revision
                && rendered.generation == e.project_load_generation,
            "P0A stale render or view"
        );
        let transport = json!({"playing":s.player.is_playing(),"auditioning":s.player.auditioning(),
            "launch_pending":s.player.launch_pending(),"position_s":s.player.position_secs(),
            "engine_playing":e.transport.playing,"engine_auditioning":e.transport.auditioning,
            "commands_pending":e.transport.pending.len(),"notes_pending":e.transport.pending_notes.len(),
            "audition_pending":e.transport.pending_audition.is_some(),"launch_command_pending":e.transport.pending_launch.is_some(),
            "perf_playing":e.transport.perf_playing,"perf_queued":e.transport.perf_queued});
        anyhow::ensure!(
            !s.player.is_playing()
                && !s.player.auditioning()
                && !s.player.launch_pending()
                && !e.transport.playing
                && !e.transport.auditioning
                && e.transport.pending.is_empty()
                && e.transport.pending_notes.is_empty()
                && e.transport.pending_audition.is_none()
                && e.transport.pending_launch.is_none()
                && e.transport.perf_playing.is_none()
                && e.transport.perf_queued.is_none(),
            "P0A autoplay/pending activity"
        );
        let workspace = s.workspace.capture_presentation(c.inspector)?;
        let doc = json!({"schema":1,"build_sha":option_env!("BEATBOX_BUILD_SHA").unwrap_or("unknown"),
            "status":"captured_uninspected","native_input":false,"autoplay":false,
            "state_origin":["project_file_loaded","capture_first_viewport","presentation_seed_only"],
            "view_index":s.view,"view_name":VIEWS[s.view],"workspace":workspace,
            "project_file":s.project_path,"runner_case":path.file_stem().and_then(|v| v.to_str()),
            "fixture_kind_source":"runner manifest cases[].fixture; not an app UI assertion",
            "seeded_presentation":{"inspector":c.inspector},
            "requested":{"logical_size":c.size,"scale":c.scale},
            "viewport_pixels":pixels,"pixels_per_point":ctx.pixels_per_point(),
            "logical_viewport":[logical.x,logical.y],"native_viewport_observations":null,
            "capture_frame":s.frames,"shot_requested":s.shot_requested,
            "render_revision":rendered.rev,"render_load_generation":rendered.generation,
            "baseline":self.baseline.artifact(),"final":final_state.artifact(),
            "invariance":"project serialized bytes + complete public ledger; private history payloads not exposed",
            "transport":transport,"transport_position_policy":"ordinary frame 2 seek to 38 percent retained; not frozen"});
        std::fs::write(
            path.with_extension("json"),
            serde_json::to_vec_pretty(&doc)?,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(extra: &[(&str, &str)], shot: bool, view: &str) -> anyhow::Result<Option<Config>> {
        let mut vars = vec![(SIZE, "1100x700"), (SCALE, "1"), ("BEATBOX_NO_LISTEN", "1")];
        vars.extend_from_slice(extra);
        Config::parse(shot, Some(view), |k| {
            vars.iter()
                .rev()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        })
    }
    #[test]
    fn strict_tokens_conflicts_and_interactive_ignore() {
        for token in ["", " 1", "1 ", "0", "-1", "NaN", "inf", "1e0", "999999999"] {
            assert!(config(&[(SCALE, token)], true, "mixer").is_err());
            if token != "0" {
                assert!(config(&[(INSPECTOR, token)], true, "mixer").is_err());
            }
            assert!(config(&[(SIZE, token)], true, "mixer").is_err());
            assert!(config(&[(SCALE, token)], false, "invalid")
                .unwrap()
                .is_none());
        }
        assert!(!config(&[], true, "mixer").unwrap().unwrap().inspector);
        assert!(
            !config(&[(INSPECTOR, "0")], true, "mixer")
                .unwrap()
                .unwrap()
                .inspector
        );
        for key in LEGACY {
            assert!(config(&[(key, "")], true, "mixer").is_err());
        }
        for view in ["MIXER", "unknown", ""] {
            assert!(config(&[], true, view).is_err());
        }
        assert!(config(&[("BEATBOX_NO_LISTEN", "0")], true, "mixer").is_err());
        for partial in [SCALE, INSPECTOR] {
            assert!(
                Config::parse(true, Some("mixer"), |k| (k == partial).then(|| "1".into())).is_err()
            );
        }
        assert!(Config::parse(true, None, |_| None).unwrap().is_none());
    }
    #[test]
    fn all_sizes_scales_and_raw_view_mapping() {
        for size in ["1100x700", "1440x900"] {
            for scale in ["1", "2"] {
                for (view, index) in [("sequencer", 0), ("playlist", 3), ("mixer", 1)] {
                    let c = config(
                        &[(SIZE, size), (SCALE, scale), (INSPECTOR, "1")],
                        true,
                        view,
                    )
                    .unwrap()
                    .unwrap();
                    assert_eq!(c.view, index);
                    assert!(c.inspector);
                    assert!(c.size[0] * c.scale <= 2880 && c.size[1] * c.scale <= 1800);
                }
            }
        }
    }
    #[test]
    fn geometry_checks_observations_not_requests() {
        let c = config(&[], true, "playlist").unwrap().unwrap();
        assert!(c
            .validate_geometry([1100, 700], 1.0, Vec2::new(1100.0, 700.0))
            .is_ok());
        for (pixels, ppp, logical) in [
            ([1099, 700], 1.0, Vec2::new(1100.0, 700.0)),
            ([1100, 700], 2.0, Vec2::new(1100.0, 700.0)),
            ([1100, 700], 1.0, Vec2::new(550.0, 350.0)),
            ([1100, 700], f32::NAN, Vec2::new(1100.0, 700.0)),
        ] {
            assert!(c.validate_geometry(pixels, ppp, logical).is_err());
        }
    }
    #[test]
    fn observation_rejects_project_and_public_ledger_changes() {
        let mut e = Engine::new(PathBuf::from("."));
        let a = Observation::read(&e).unwrap();
        e.revision += 1;
        assert!(a != Observation::read(&e).unwrap());
        e.revision -= 1;
        e.log.push(crate::engine::LogEntry {
            tool: "fixture".into(),
            summary: "observed".into(),
            ok: true,
            source: "test".into(),
        });
        assert!(a != Observation::read(&e).unwrap());
        e.log.pop();
        e.snapshots.push((
            "observed".into(),
            crate::engine::Snapshot {
                project: e.project.clone(),
                note: "keep".into(),
                revision: e.revision,
            },
        ));
        assert!(a != Observation::read(&e).unwrap());
        e.snapshots.clear();
        e.project.name.push_str("changed");
        assert!(a != Observation::read(&e).unwrap());
    }
}
