use beatbox::{audio_edit, dsp::SR};
fn pulse() -> Vec<f32> {
    let mut x = vec![0.; SR as usize * 12];
    for k in 0..24 {
        let s = 3000 + k * 22050;
        for j in 0..2000 {
            x[s + j] = (j as f32 * 0.15).sin() * (-(j as f32) / 250.).exp() * 0.7;
        }
    }
    x
}
#[test]
fn opposed_stereo_profile_keeps_tempo_evidence() {
    let x = pulse();
    let r: Vec<f32> = x.iter().map(|v| -v).collect();
    let mono = audio_edit::profile("mono", &x, &x);
    let opposed = audio_edit::profile("opposed", &x, &r);
    assert!(mono.bpm_confidence > 0.35, "{mono:?}");
    assert_eq!(mono.bpm, opposed.bpm);
    assert_eq!(mono.bpm_confidence, opposed.bpm_confidence);
}
#[test]
fn mono_math_and_channel_swaps_match() {
    let x = pulse();
    let r: Vec<f32> = x.iter().map(|v| -v).collect();
    assert_eq!(
        audio_edit::estimate_bpm(&x),
        audio_edit::estimate_bpm_stereo(&x, &x)
    );
    assert_eq!(
        audio_edit::estimate_bpm_stereo(&x, &r),
        audio_edit::estimate_bpm_stereo(&r, &x)
    );
}
#[test]
fn right_only_and_alternating_hits_keep_pulse() {
    let x = pulse();
    let zero = vec![0.; x.len()];
    let a = audio_edit::estimate_bpm_stereo(&x, &zero);
    let b = audio_edit::estimate_bpm_stereo(&zero, &x);
    assert_eq!(a, b);
    assert!((a.0 - 120.).abs() < 1.);
    assert!(a.1 > 0.35);
    let mut l = x.clone();
    let mut r = x.clone();
    for i in 0..x.len() {
        if (i.saturating_sub(3000) / 22050) % 2 == 0 {
            r[i] = 0.;
        } else {
            l[i] = 0.;
        }
    }
    let a = audio_edit::estimate_bpm_stereo(&l, &r);
    assert!((a.0 - 120.).abs() < 1., "{a:?}");
    assert!(a.1 > 0.35, "{a:?}");
}
#[test]
fn short_silent_empty_and_extra_tail_are_safe() {
    assert_eq!(audio_edit::estimate_bpm_stereo(&[], &[]), (0., 0.));
    assert_eq!(
        audio_edit::estimate_bpm_stereo(&[0.5; 32], &[0.; 32]),
        (0., 0.)
    );
    let z = vec![0.; 88200];
    assert_eq!(audio_edit::estimate_bpm_stereo(&z, &z).1, 0.);
    let x = pulse();
    let mut t = x.clone();
    t.extend(pulse());
    assert_eq!(
        audio_edit::estimate_bpm_stereo(&t, &x),
        audio_edit::estimate_bpm(&x)
    );
}
#[test]
fn analyzer_sample_file_tempo_matches_without_mutation() {
    use beatbox::{render, Engine};
    use serde_json::json;
    let root = std::env::temp_dir().join(format!("bb_tempo_{}", beatbox::creative::fresh_seed()));
    std::fs::create_dir_all(&root).unwrap();
    let x = pulse();
    let r: Vec<f32> = x.iter().map(|v| -v).collect();
    let path = root.join("pulse.wav");
    render::write_wav(&path, &x, &r).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut e = Engine::new(root.clone());
    e.call("import_sample", &json!({"path":path,"name":"pulse"}))
        .unwrap();
    e.project.samples[0].path = "pulse.wav".into();
    let before = e.project.clone();
    let rev = e.revision;
    let depth = e.undo_depth();
    let a = e.call("analyze_audio", &json!({"sample":"PULSE"})).unwrap();
    let b = e.call("analyze_audio", &json!({"path":path})).unwrap();
    assert_eq!(a["bpm"], 120.);
    assert_eq!(a["bpm_reliable"], true);
    assert_eq!(a["bpm"], b["bpm"]);
    assert_eq!(a["bpm_confidence"], b["bpm_confidence"]);
    assert_eq!(e.project, before);
    assert_eq!(e.revision, rev);
    assert_eq!(e.undo_depth(), depth);
    assert_eq!(bytes, std::fs::read(&path).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn sustained_audio_still_has_low_confidence_warning() {
    use beatbox::{render, Engine};
    use serde_json::json;
    let root =
        std::env::temp_dir().join(format!("bb_tempo_pad_{}", beatbox::creative::fresh_seed()));
    std::fs::create_dir_all(&root).unwrap();
    let x: Vec<f32> = (0..176400)
        .map(|i| (i as f32 * 440. * std::f32::consts::TAU / SR).sin() * 0.3)
        .collect();
    let r: Vec<f32> = x.iter().map(|v| -v).collect();
    let path = root.join("pad.wav");
    render::write_wav(&path, &x, &r).unwrap();
    let mut e = Engine::new(root.clone());
    let a = e.call("analyze_audio", &json!({"path":path})).unwrap();
    assert_eq!(a["bpm_reliable"], false, "{a}");
    assert!(a["bpm_confidence"].as_f64().unwrap() < 0.35);
    assert!(a["bpm_note"].as_str().unwrap().contains("low confidence"));
    std::fs::remove_dir_all(root).unwrap();
}
