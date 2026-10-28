//! Tiny command-boundary fixtures. No device, GUI, producer or render campaign.
use beatbox::{engine::TransportCmd, tools_categories::category_of, Engine};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Rig {
    dir: PathBuf,
    e: Engine,
}
impl Rig {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "artifact-windows-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        let tone: Vec<f32> = (0..88_200)
            .map(|i| 0.1 * (std::f32::consts::TAU * 220.0 * i as f32 / 44_100.0).sin())
            .collect();
        std::fs::write(
            dir.join("tone.wav"),
            beatbox::media::wav_bytes(&tone, &tone, 44_100),
        )
        .unwrap();
        e.call("import_sample", &json!({"path":"tone.wav","name":"tone"}))
            .unwrap();
        e.call("set_tempo", &json!({"bpm":130})).unwrap();
        e.call("set_tempo", &json!({"bpm":140})).unwrap();
        e.call("undo", &json!({})).unwrap();
        e.call("snapshot", &json!({"name":"keep"})).unwrap();
        e.call("save_project", &json!({"path":"sentinel.json"}))
            .unwrap();
        e.midi.quantize = 0.25;
        e.transport.pending.push(TransportCmd::Play);
        std::fs::write(dir.join("sentinel"), b"untouched").unwrap();
        Self { dir, e }
    }
    fn files(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files: Vec<_> = std::fs::read_dir(&self.dir)
            .unwrap()
            .map(|f| {
                let p = f.unwrap().path();
                (p.file_name().unwrap().into(), std::fs::read(p).unwrap())
            })
            .collect();
        files.sort();
        files
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn state(e: &Engine) -> Value {
    let snapshots: Vec<_> = e
        .snapshots
        .iter()
        .map(|(n, s)| json!({"name":n,"project":s.project,"note":s.note,"revision":s.revision}))
        .collect();
    json!({"project":e.project,"history":e.history(),"depth":e.undo_depth(),"revision":e.revision,
        "generation":e.project_load_generation,"snapshots":snapshots,"midi":beatbox::midi_in::snapshot(e),
        "record":e.midi.record,"learn":e.midi.learn,"transport":format!("{:?}",e.transport)})
}
fn forms(args: &Value) -> Vec<(String, Value)> {
    let home = category_of("detect_artifacts").unwrap().name;
    let mut flat = args.as_object().unwrap().clone();
    flat.insert("action".into(), json!("detect_artifacts"));
    vec![
        ("detect_artifacts".into(), args.clone()),
        (home.into(), Value::Object(flat)),
        (
            home.into(),
            json!({"action":"detect_artifacts","params":args}),
        ),
        (
            "call_tool".into(),
            json!({"name":"detect_artifacts","arguments":args}),
        ),
        (
            "call_tool".into(),
            json!({"name":home,"arguments":{"action":"detect_artifacts","params":args}}),
        ),
    ]
}
fn assert_unchanged(r: &Rig, before: &Value, files: &[(PathBuf, Vec<u8>)]) {
    assert_eq!(&state(&r.e), before);
    assert_eq!(r.files(), files);
}
#[test]
fn direct_category_wrapper_parity_and_live_state() {
    let mut r = Rig::new();
    let before = state(&r.e);
    let files = r.files();
    for source in [json!({"path":"tone.wav"}), json!({"sample":"tone"})] {
        let mut args = source;
        args["start_s"] = json!(0.25);
        args["end_s"] = json!(0.75);
        let expected = r.e.call("detect_artifacts", &args).unwrap();
        assert_eq!(
            expected["window_bounds"],
            json!({"start_s":0.25,"end_s":0.75,"source_end_s":2.0,"source_end_checked":false})
        );
        assert!(expected["window"].is_string());
        assert!(!expected["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["kind"] == "truncated_tail" || x["kind"] == "end_level_jump"));
        for (name, a) in forms(&args) {
            assert_eq!(r.e.call(&name, &a).unwrap(), expected);
        }
        args["tail_s"] = json!(6);
        assert_eq!(r.e.call("detect_artifacts", &args).unwrap(), expected);
        assert_unchanged(&r, &before, &files);
    }
}
#[test]
fn invalid_ranges_and_budgets_are_normal_errors_in_all_forms() {
    let mut r = Rig::new();
    let before = state(&r.e);
    let files = r.files();
    for bad in [
        json!({"start_s":2}),
        json!({"start_s":3}),
        json!({"start_s":0.8,"end_s":0.2}),
        json!({"start_s":0.5,"end_s":0.5}),
        json!({"start_s":-1}),
        json!({"end_s":0}),
        json!({"start_s":null}),
        json!({"start_s":"NaN"}),
        json!({"clicks":null}),
        json!({"per_track":"true"}),
        json!({"end_s":1e300}),
        json!({"start_s":0,"end_s":0.000001}),
        json!({"max_events":0}),
        json!({"max_events":257}),
        json!({"max_events":u64::MAX}),
        json!({"max_events":-1}),
        json!({"max_events":1.5}),
        json!({"tail_s":-1}),
        json!({"tail_s":null}),
        json!({"tail_s":"inf"}),
        json!({"section":0}),
        json!({"start_beat":0}),
        json!({"end_beat":2}),
        json!({"start_s":0,"section":0}),
        json!({"sample":"tone"}),
        json!({"track":"x"}),
    ] {
        let mut args = bad;
        args["path"] = json!("tone.wav");
        for (name, a) in forms(&args) {
            let err = r.e.call(&name, &a).unwrap_err().to_string();
            assert!(!err.contains("internal error"), "{name} {a}: {err}");
            assert_unchanged(&r, &before, &files);
        }
    }
}
#[test]
fn eof_clamping_hot_and_decayed_endings() {
    let mut r = Rig::new();
    for cap in [1, 256] {
        let hot =
            r.e.call(
                "detect_artifacts",
                &json!({"path":"tone.wav","end_s":99,"max_events":cap}),
            )
            .unwrap();
        assert_eq!(hot["window_bounds"]["end_s"], 2.0);
        assert_eq!(hot["window_bounds"]["source_end_checked"], true);
        assert!(hot["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["kind"] == "truncated_tail"));
    }
    let silence = vec![0.0; 88_200];
    std::fs::write(
        r.dir.join("silence.wav"),
        beatbox::media::wav_bytes(&silence, &silence, 44_100),
    )
    .unwrap();
    let clean =
        r.e.call("detect_artifacts", &json!({"path":"silence.wav"}))
            .unwrap();
    assert_eq!(clean["clean"], true);
    let decay: Vec<f32> = (0..88_200)
        .map(|i| {
            if i < 44_100 {
                0.1 * (std::f32::consts::TAU * 220.0 * i as f32 / 44_100.0).sin()
                    * (1.0 - i as f32 / 44_100.0)
            } else {
                0.0
            }
        })
        .collect();
    std::fs::write(
        r.dir.join("decay.wav"),
        beatbox::media::wav_bytes(&decay, &decay, 44_100),
    )
    .unwrap();
    let clean =
        r.e.call("detect_artifacts", &json!({"path":"decay.wav"}))
            .unwrap();
    assert!(!clean["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["kind"] == "truncated_tail"));
}

#[test]
fn mix_and_track_share_literal_seconds_and_routing() {
    let mut r = Rig::new();
    r.e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
        .unwrap();
    r.e.call(
        "add_notes",
        &json!({"track":"keys","notes":[{"start":0,"len":2,"pitch":60,"vel":0.7}]}),
    )
    .unwrap();
    let before = state(&r.e);
    let files = r.files();
    let mut bounds = None;
    for source in [json!({}), json!({"track":"keys"})] {
        let mut args = source;
        args["start_s"] = json!(0.25);
        args["end_s"] = json!(0.75);
        let expected = r.e.call("detect_artifacts", &args).unwrap();
        assert_eq!(expected["window_bounds"]["start_s"], 0.25);
        assert_eq!(expected["window_bounds"]["end_s"], 0.75);
        assert_eq!(expected["window_bounds"]["source_end_checked"], false);
        if let Some(b) = &bounds {
            assert_eq!(&expected["window_bounds"], b);
        }
        bounds = Some(expected["window_bounds"].clone());
        for (name, a) in forms(&args) {
            assert_eq!(r.e.call(&name, &a).unwrap(), expected);
        }
        args["tail_s"] = json!(6);
        assert_eq!(r.e.call("detect_artifacts", &args).unwrap(), expected);
        assert_unchanged(&r, &before, &files);
    }
}

#[test]
fn interior_stereo_clicks_keep_absolute_times_and_dc_findings() {
    let mut r = Rig::new();
    let mut l = vec![0.0; 4 * 44_100];
    let mut rr = l.clone();
    l[44_100] = 0.6;
    rr[2 * 44_100] = 0.6;
    for v in &mut l[110_250..] {
        *v += 0.05;
    }
    for v in &mut rr[110_250..] {
        *v += 0.05;
    }
    std::fs::write(
        r.dir.join("clicks.wav"),
        beatbox::media::wav_bytes(&l, &rr, 44_100),
    )
    .unwrap();
    let before = state(&r.e);
    let files = r.files();
    let a =
        r.e.call(
            "detect_artifacts",
            &json!({"path":"clicks.wav","start_s":0.5,"end_s":3.5}),
        )
        .unwrap();
    let events = a["events"].as_array().unwrap();
    for at in [1.0, 2.0] {
        assert!(
            events
                .iter()
                .any(|e| e["kind"] == "click" && (e["time_s"].as_f64().unwrap() - at).abs() < 0.02),
            "{a}"
        );
    }
    assert!(events.iter().any(|e| e["kind"] == "dc_step"), "{a}");
    assert_eq!(a["clean"], false);
    assert_eq!(a["window_bounds"]["source_end_checked"], false);
    assert_unchanged(&r, &before, &files);
}

#[test]
fn fade_then_lift_is_an_ending_finding_only_at_source_end() {
    let mut r = Rig::new();
    let n = 6 * 44_100;
    let mut audio: Vec<f32> = (0..n)
        .map(|i| {
            let fade = if i >= 3 * 44_100 {
                ((n - i) as f32 / (3.0 * 44_100.0)).powi(3)
            } else {
                1.0
            };
            0.2 * (std::f32::consts::TAU * 220.0 * i as f32 / 44_100.0).sin() * fade
        })
        .collect();
    for (i, v) in audio
        .iter_mut()
        .enumerate()
        .take(n - 10_000)
        .skip(n - 15_000)
    {
        *v += 0.3 * (std::f32::consts::TAU * 200.0 * i as f32 / 44_100.0).sin();
    }
    std::fs::write(
        r.dir.join("ending.wav"),
        beatbox::media::wav_bytes(&audio, &audio, 44_100),
    )
    .unwrap();
    let ending =
        r.e.call("detect_artifacts", &json!({"path":"ending.wav","tail_s":4}))
            .unwrap();
    assert!(
        ending["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "end_level_jump"),
        "{ending}"
    );
    audio.extend(vec![0.0; 44_100]);
    std::fs::write(
        r.dir.join("interior.wav"),
        beatbox::media::wav_bytes(&audio, &audio, 44_100),
    )
    .unwrap();
    let interior =
        r.e.call(
            "detect_artifacts",
            &json!({"path":"interior.wav","end_s":6,"tail_s":4}),
        )
        .unwrap();
    assert_eq!(interior["window_bounds"]["source_end_checked"], false);
    assert!(!interior["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "end_level_jump" || e["kind"] == "truncated_tail"));
}
