use beatbox::{audio_edit, dsp::SR};
fn chord() -> Vec<f32> {
    (0..176400)
        .map(|i| {
            [130.8128, 164.8138, 195.9977]
                .iter()
                .map(|hz| (i as f32 * hz * std::f32::consts::TAU / SR).sin() * 0.13)
                .sum()
        })
        .collect()
}
#[test]
fn opposed_stereo_profile_does_not_erase_key_candidates() {
    let x = chord();
    let r: Vec<f32> = x.iter().map(|v| -v).collect();
    let a = audio_edit::profile("mono", &x, &x);
    let b = audio_edit::profile("opposed", &x, &r);
    assert!(!a.key.is_empty());
    assert_eq!(a.key, b.key);
}
#[test]
fn mono_polarity_and_channel_swaps_keep_candidates_and_confidence() {
    let x = chord();
    let r: Vec<f32> = x.iter().map(|v| -v).collect();
    let a = audio_edit::estimate_key(&x);
    assert_eq!(a, audio_edit::estimate_key_stereo(&x, &x));
    assert_eq!(a, audio_edit::estimate_key_stereo(&x, &r));
    assert_eq!(a, audio_edit::estimate_key_stereo(&r, &x));
    assert_eq!(
        audio_edit::key_report(&x),
        audio_edit::key_report_stereo(&x, &r)
    );
}
#[test]
fn silent_short_right_only_and_common_length_are_safe() {
    let x = chord();
    let z = vec![0.; x.len()];
    let mut tail = x.clone();
    tail.extend(chord());
    assert_eq!(
        audio_edit::estimate_key_stereo(&x, &z),
        audio_edit::estimate_key_stereo(&z, &x)
    );
    assert!(!audio_edit::estimate_key_stereo(&z, &x).is_empty());
    assert_eq!(
        audio_edit::estimate_key_stereo(&tail, &x),
        audio_edit::estimate_key(&x)
    );
    for (l, r) in [
        (vec![], vec![]),
        (vec![0.; 32], vec![0.5; 32]),
        (z.clone(), z),
    ] {
        assert!(audio_edit::estimate_key_stereo(&l, &r).is_empty());
        assert_eq!(audio_edit::key_report_stereo(&l, &r)["confidence"], "none");
    }
}
#[test]
fn separated_channel_chroma_is_combined_without_cross_terms() {
    let l = chord();
    let r: Vec<f32> = (0..l.len())
        .map(|i| (i as f32 * 220. * std::f32::consts::TAU / SR).sin() * 0.1)
        .collect();
    let inv: Vec<f32> = r.iter().map(|v| -v).collect();
    let (lf, lb) = audio_edit::chroma_split(&l);
    let (rf, rb) = audio_edit::chroma_split(&r);
    let (f, b) = audio_edit::chroma_split_stereo(&l, &r);
    for i in 0..12 {
        assert_eq!(f[i], 0.5 * (lf[i] + rf[i]));
        assert_eq!(b[i], 0.5 * (lb[i] + rb[i]));
    }
    assert_eq!((f, b), audio_edit::chroma_split_stereo(&r, &l));
    assert_eq!((f, b), audio_edit::chroma_split_stereo(&l, &inv));
}
#[test]
fn analyzer_sample_and_file_key_details_match_without_mutation() {
    use beatbox::{render, Engine};
    use serde_json::json;
    let root =
        std::env::temp_dir().join(format!("bb_stereo_key_{}", beatbox::creative::fresh_seed()));
    std::fs::create_dir_all(&root).unwrap();
    let l = chord();
    let r: Vec<f32> = l.iter().map(|v| -v).collect();
    let path = root.join("chord.wav");
    render::write_wav(&path, &l, &r).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut e = Engine::new(root.clone());
    e.call("import_sample", &json!({"path":path,"name":"chord"}))
        .unwrap();
    e.project.samples[0].path = "chord.wav".into();
    let before = e.project.clone();
    let rev = e.revision;
    let hist = e.undo_depth();
    let a = e.call("analyze_audio", &json!({"sample":"CHORD"})).unwrap();
    let b = e.call("analyze_audio", &json!({"path":path})).unwrap();
    assert!(!a["key_candidates"].as_array().unwrap().is_empty());
    for k in ["key", "key_candidates", "key_detail"] {
        assert_eq!(a[k], b[k]);
    }
    assert_eq!(e.project, before);
    assert_eq!(e.revision, rev);
    assert_eq!(e.undo_depth(), hist);
    assert_eq!(bytes, std::fs::read(&path).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn low_margin_warning_and_candidate_scores_are_not_upgraded() {
    let x: Vec<f32> = (0..176400)
        .map(|i| (i as f32 * 440. * std::f32::consts::TAU / SR).sin() * 0.3)
        .collect();
    let r: Vec<f32> = x.iter().map(|v| -v).collect();
    let a = audio_edit::key_report(&x);
    let b = audio_edit::key_report_stereo(&x, &r);
    assert_eq!(a, b);
    if a["confidence"] == "low" {
        assert!(a["note"].as_str().unwrap().contains("low margin"));
    }
    assert_eq!(b["candidates"].as_array().unwrap().len(), 3);
}
