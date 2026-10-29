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
            "artifact-source-scope-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        let mut impulse = vec![0.0; 88_200];
        impulse[22_050] = 0.5;
        std::fs::write(
            dir.join("impulse.wav"),
            beatbox::media::wav_bytes(&impulse, &impulse, 44_100),
        )
        .unwrap();
        e.call(
            "import_sample",
            &json!({"path":"impulse.wav","name":"impulse"}),
        )
        .unwrap();
        e.call(
            "add_track",
            &json!({"name":"Lead","preset":"pad","volume_db":0}),
        )
        .unwrap();
        e.call(
            "add_track",
            &json!({"name":"Kick","preset":"kick","volume_db":0}),
        )
        .unwrap();
        e.call(
            "add_audio_clip",
            &json!({"track":"Lead","sample":"impulse","start_beat":0,"length_s":2}),
        )
        .unwrap();
        e.call(
            "add_notes",
            &json!({"track":"Kick","notes":[{"start":4,"len":1,"pitch":60,"vel":0.8}]}),
        )
        .unwrap();
        e.call("set_tempo", &json!({"bpm":130})).unwrap();
        e.call("set_tempo", &json!({"bpm":140})).unwrap();
        e.call("undo", &json!({})).unwrap();
        e.call("snapshot", &json!({"name":"keep"})).unwrap();
        e.call("save_project", &json!({"path":"sentinel.json"}))
            .unwrap();
        e.project.bpm = 120.0;
        e.project.patterns[0].bars = 1;
        e.project.master_effects.clear();
        e.project.master_volume_db = 0.0;
        e.midi.quantize = 0.25;
        e.midi.learn = Some("volume:Lead".into());
        e.midi.recent.push_back("sentinel MIDI state".into());
        e.midi.inbox.lock().unwrap().push([0x90, 60, 100]);
        e.transport.pending.push(TransportCmd::Play);
        e.transport.pending_audition = Some(Some(std::sync::Arc::new((vec![0.1], vec![0.2]))));
        e.transport
            .pending_notes
            .push(std::sync::Arc::new((vec![0.3], vec![0.4])));
        std::fs::create_dir(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested/sentinel"), b"nested untouched").unwrap();
        std::fs::write(dir.join("sentinel"), b"untouched").unwrap();
        Self { dir, e }
    }
    fn files(&self) -> Vec<(PathBuf, Vec<u8>)> {
        fn visit(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(root, &path, out);
                } else {
                    out.push((
                        path.strip_prefix(root).unwrap().into(),
                        std::fs::read(&path).unwrap(),
                    ));
                }
            }
        }
        let mut files = Vec::new();
        visit(&self.dir, &self.dir, &mut files);
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
        "record":e.midi.record,"learn":e.midi.learn,"midi_recent":e.midi.recent,
        "midi_port":e.midi.port,"midi_inbox":e.midi.inbox.lock().unwrap().clone(),"transport":format!("{:?}",e.transport)})
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

fn has_click(events: &Value, at: f64) -> bool {
    events
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "click" && (e["time_s"].as_f64().unwrap() - at).abs() < 0.015)
}
fn check_forms(r: &mut Rig, args: Value) -> Value {
    let before = state(&r.e);
    let files = r.files();
    let sample = r.e.bank.get("impulse").unwrap().to_vec();
    let expected = r.e.call("detect_artifacts", &args).unwrap();
    assert_unchanged(r, &before, &files);
    assert_eq!(r.e.bank.get("impulse").unwrap(), sample.as_slice());
    for (name, a) in forms(&args) {
        assert_eq!(r.e.call(&name, &a).unwrap(), expected);
        assert_unchanged(r, &before, &files);
        assert_eq!(r.e.bank.get("impulse").unwrap(), sample.as_slice());
    }
    expected
}
fn solo_pcm(r: &mut Rig, ti: usize) -> (Vec<f32>, Vec<f32>) {
    let mut p = r.e.project.clone();
    for (i, t) in p.tracks.iter_mut().enumerate() {
        t.solo = i == ti;
        if i == ti {
            t.mute = false;
        }
    }
    r.e.bank.sync(&p.samples);
    let m = beatbox::render::render(&p, &r.e.bank, &Default::default()).unwrap();
    (m.left, m.right)
}

#[test]
fn authored_lead_foreign_aligned_muted_displaced_and_solo_mix() {
    let mut r = Rig::new();
    let reference = solo_pcm(&mut r, 0);
    let (raw, _) = beatbox::ears::detect_artifacts(&reference.0, &reference.1, &Default::default());
    assert!(
        raw.iter()
            .any(|e| e.kind == "click" && (e.time_s - 0.5).abs() < 0.015),
        "fixture must actually render the authored impulse"
    );
    for mode in 0..3 {
        r.e.project.tracks[1].mute = mode == 1;
        r.e.project.patterns[0].notes_mut("Kick")[0].start = if mode == 2 { 8.0 } else { 4.0 };
        assert_eq!(solo_pcm(&mut r, 0), reference);
        let out = check_forms(&mut r, json!({"track":"Lead","start_s":0.25,"end_s":0.75}));
        assert!(has_click(&out["events"], 0.5), "{out}");
        assert_eq!(out["drum_attack_transients_ignored"], 0);
        assert_eq!(out["window_bounds"]["source_end_checked"], false);
        r.e.project.tracks[0].solo = true;
        let mix = check_forms(&mut r, json!({"start_s":0.25,"end_s":0.75}));
        assert!(has_click(&mix["events"], 0.5), "{mix}");
        assert_eq!(mix["drum_attack_transients_ignored"], 0);
        r.e.project.tracks[0].solo = false;
    }
}

#[test]
fn own_culprit_mask_file_sample_and_selected_unmute_id() {
    let mut r = Rig::new();
    let out = check_forms(
        &mut r,
        json!({"per_track":true,"start_s":0.25,"end_s":0.75}),
    );
    let lead = out["culprits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["track"] == "Lead")
        .expect("foreign kick cannot hide Lead culprit");
    assert!(
        has_click(&lead["events"], 0.25),
        "retain existing local culprit coordinates: {lead}"
    );
    // A percussion-classified selected track with a scheduled own hit and an
    // authored clip: raw click is established before testing its own mask.
    r.e.project.tracks[0].name = "Own percussion".into();
    r.e.project.tracks[0].id = "t-own".into();
    r.e.project.audio_clips[0].track = "Own percussion".into();
    r.e.project.patterns[0]
        .notes_mut("Own percussion")
        .push(beatbox::project::Note::new(4.0, 1.0, 60, 0.0));
    r.e.project.tracks[0].mute = true;
    let pcm = solo_pcm(&mut r, 0);
    let (raw, _) = beatbox::ears::detect_artifacts(&pcm.0, &pcm.1, &Default::default());
    assert!(raw
        .iter()
        .any(|e| e.kind == "click" && (e.time_s - 0.5).abs() < 0.015));
    let own = check_forms(&mut r, json!({"track":"t-own","start_s":0.25,"end_s":0.75}));
    assert!(!has_click(&own["events"], 0.5), "{own}");
    assert_eq!(own["drum_attack_transients_ignored"], 1);
    r.e.project.patterns[0].notes_mut("Own percussion")[0].start = 8.0;
    let foreign = check_forms(&mut r, json!({"track":"t-own","start_s":0.25,"end_s":0.75}));
    assert!(
        has_click(&foreign["events"], 0.5),
        "foreign Kick at 0.5 must not mask selected percussion: {foreign}"
    );
    assert_eq!(foreign["drum_attack_transients_ignored"], 0);
    for solo in [false, true] {
        r.e.project.tracks[1].solo = solo;
        for source in [json!({"path":"impulse.wav"}), json!({"sample":"impulse"})] {
            let mut a = source;
            a["start_s"] = json!(0.25);
            a["end_s"] = json!(0.75);
            let out = check_forms(&mut r, a);
            assert!(has_click(&out["events"], 0.5), "{out}");
            assert_eq!(out["drum_attack_transients_ignored"], 0);
        }
    }
}

fn decoded_render(r: &mut Rig, selected: Option<usize>) -> (Vec<f32>, Vec<f32>) {
    let (l, rr) = if let Some(ti) = selected {
        solo_pcm(r, ti)
    } else {
        let m = r.e.mix().unwrap();
        (m.left.clone(), m.right.clone())
    };
    let path = r.dir.join("decoded-render.wav");
    std::fs::write(&path, beatbox::media::wav_bytes(&l, &rr, 44_100)).unwrap();
    beatbox::samples::decode_stereo(&path).unwrap()
}
fn decoded_preview(r: &mut Rig) -> (Vec<f32>, Vec<f32>) {
    r.e.call("render_preview", &json!({"track":"Lead","start_s":0.25,"end_s":0.75,"tail_s":0,"spectrogram":false,"inline_audio":false,"out":"preview.wav"})).unwrap();
    beatbox::samples::decode_stereo(&r.dir.join("preview.wav")).unwrap()
}
#[test]
fn diagnostics_do_not_change_decoded_mix_solo_or_preview_pcm() {
    let mut r = Rig::new();
    let mix = decoded_render(&mut r, None);
    let solo = decoded_render(&mut r, Some(0));
    let preview = decoded_preview(&mut r);
    let before = state(&r.e);
    let files = r.files();
    for a in [
        json!({}),
        json!({"track":"Lead"}),
        json!({"per_track":true}),
        json!({"path":"impulse.wav"}),
        json!({"sample":"impulse"}),
    ] {
        check_forms(&mut r, a);
    }
    assert_unchanged(&r, &before, &files);
    assert_eq!(decoded_render(&mut r, None), mix);
    assert_eq!(decoded_render(&mut r, Some(0)), solo);
    assert_eq!(decoded_preview(&mut r), preview);
    // Same-build before/after diagnostic invariance, NOT an independent
    // baseline-code/fixed-code audio oracle or acoustic attribution proof.
}
