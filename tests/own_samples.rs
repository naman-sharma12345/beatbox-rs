use beatbox::Engine;
use serde_json::json;
use std::fs;

#[test]
fn arbitrary_local_sample_slices_survive_reopen_without_changing_the_source() {
    let root = std::env::temp_dir().join(format!(
        "bb_own_samples_{}",
        beatbox::creative::fresh_seed()
    ));
    fs::create_dir_all(&root).unwrap();
    let source = root.join("a sound outside any pack.wav");
    let mut audio = vec![0.0f32; 44100];
    for k in 0..4 {
        for j in 0..3000 {
            audio[k * 11025 + j] =
                (j as f32 * 0.17).sin() * (-(j as f32) / 400.0).exp() * (0.3 + k as f32 * 0.1);
        }
    }
    beatbox::render::write_wav(&source, &audio, &audio).unwrap();
    let original_bytes = fs::read(&source).unwrap();
    let mut e = Engine::new(root.clone());
    e.call("new_project", &json!({"name":"own sample lifecycle"}))
        .unwrap();
    e.call("set_pattern_length", &json!({"bars":1})).unwrap();
    e.call("import_sample", &json!({"path":source,"name":"my sound"}))
        .unwrap();
    let sample = e.project.samples[0].name.clone();
    e.call(
        "add_sample_track",
        &json!({"name":"Original","sample":sample}),
    )
    .unwrap();
    let original_track = e.project.tracks[0].clone();
    let sliced = e.call("slice_sample", &json!({"sample":sample})).unwrap();
    assert_eq!(sliced["slices"], 4, "{sliced}");
    assert_eq!(sliced["notes_written"], 4, "{sliced}");
    assert_eq!(e.project.tracks[0], original_track);
    assert_eq!(fs::read(&source).unwrap(), original_bytes);
    let project = e.project.clone();
    let rendered = e.render_stems_all(&project).unwrap();
    let stem = rendered.stems.iter().find(|s| s.name == sliced["track"]);
    assert!(stem.is_some(), "missing slice channel");
    assert!(stem.unwrap().left.iter().any(|v| v.abs() > 0.01));
    let saved = root.join("reopen.json");
    e.call("save_project", &json!({"path":saved})).unwrap();
    let mut reopened = Engine::new(root.clone());
    reopened
        .call("load_project", &json!({"path":saved}))
        .unwrap();
    assert_eq!(reopened.project, project);
    let again = reopened.render_stems_all(&project).unwrap();
    assert_eq!(rendered.left, again.left);
    assert_eq!(rendered.right, again.right);
    assert_eq!(fs::read(&source).unwrap(), original_bytes);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn source_preview_preserves_stereo_caps_duration_and_does_not_edit_project() {
    let root = std::env::temp_dir().join(format!(
        "bb_source_preview_{}",
        beatbox::creative::fresh_seed()
    ));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("stereo.wav");
    let left: Vec<f32> = (0..44100 * 13)
        .map(|i| (i as f32 * 0.03).sin() * 0.5)
        .collect();
    let right: Vec<f32> = (0..left.len())
        .map(|i| (i as f32 * 0.07).cos() * 0.2)
        .collect();
    beatbox::render::write_wav(&path, &left, &right).unwrap();
    let bytes = fs::read(&path).unwrap();
    let decoded = beatbox::samples::decode_stereo(&path).unwrap();
    let mut e = Engine::new(root.clone());
    e.call("import_sample", &json!({"path":path,"name":"stereo"}))
        .unwrap();
    let project = e.project.clone();
    let revision = e.revision;
    let r = e
        .call(
            "play_audio",
            &json!({"sample":"stereo","gain_db":-6.0,"max_seconds":12.0}),
        )
        .unwrap();
    assert_eq!(r["playing"], false, "no device attachment is claimed");
    assert_eq!(r["play_seconds"], 12.0);
    let queued = e.transport.pending_audition.clone().unwrap().unwrap();
    assert_eq!(queued.0.len(), 44100 * 12);
    let gain = 10f32.powf(-6.0 / 20.0);
    for (actual, source) in [(&queued.0, &decoded.0), (&queued.1, &decoded.1)] {
        assert!(actual
            .iter()
            .zip(source)
            .all(|(a, b)| a.to_bits() == (b * gain).to_bits()));
    }
    assert_ne!(queued.0, queued.1, "stereo must not be folded to mono");
    e.call("play_audio", &json!({"stop":true})).unwrap();
    assert!(matches!(e.transport.pending_audition, Some(None)));
    assert_eq!(e.project, project);
    assert_eq!(e.revision, revision);
    assert_eq!(fs::read(path).unwrap(), bytes);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn explicit_fresh_slice_channel_keeps_prior_arranged_slices() {
    let root =
        std::env::temp_dir().join(format!("bb_slice_keep_{}", beatbox::creative::fresh_seed()));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("loop.wav");
    let audio: Vec<f32> = (0..44100).map(|i| (i as f32 * 0.06).sin() * 0.3).collect();
    beatbox::render::write_wav(&path, &audio, &audio).unwrap();
    let mut e = Engine::new(root.clone());
    e.call("import_sample", &json!({"path":path,"name":"loop"}))
        .unwrap();
    e.call("slice_sample", &json!({"sample":"loop","equal":4}))
        .unwrap();
    let prior = e.project.clone();
    let prior_audio = e.render_stems_all(&prior).unwrap();
    let result = e
        .call(
            "slice_sample",
            &json!({"sample":"loop","track":"loop_slices #2","equal":2}),
        )
        .unwrap();
    assert_eq!(result["slices"], 2);
    assert_eq!(e.project.tracks[..prior.tracks.len()], prior.tracks);
    assert_eq!(
        e.project.patterns[0].notes("loop_slices"),
        prior.patterns[0].notes("loop_slices")
    );
    assert_eq!(e.project.patterns[0].notes("loop_slices #2").len(), 2);
    let completed = e.project.clone();
    e.call("undo", &json!({})).unwrap();
    assert_eq!(e.project, prior);
    let undone_audio = e.render_stems_all(&prior).unwrap();
    assert_eq!(undone_audio.left, prior_audio.left);
    assert_eq!(undone_audio.right, prior_audio.right);
    e.call("redo", &json!({})).unwrap();
    assert_eq!(e.project, completed);
    let saved = root.join("slices.json");
    e.call("save_project", &json!({"path":saved})).unwrap();
    let mut reopened = Engine::new(root.clone());
    reopened
        .call("load_project", &json!({"path":saved}))
        .unwrap();
    assert_eq!(reopened.project, completed);
    fs::remove_dir_all(root).unwrap();
}
