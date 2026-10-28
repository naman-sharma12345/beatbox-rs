use beatbox::{render, Engine};
use serde_json::json;
#[test]
fn analyzer_detects_opposed_stereo_attacks() {
    let root = std::env::temp_dir().join(format!("bb_detect_{}", beatbox::creative::fresh_seed()));
    std::fs::create_dir_all(&root).unwrap();
    let mut l = vec![0.; 176400];
    for start in [8000, 30000, 57000, 83000, 119000, 148000] {
        for j in 0..1800 {
            l[start + j] = (j as f32 * 0.13).sin() * (-(j as f32) / 250.).exp() * 0.6;
        }
    }
    let r: Vec<f32> = l.iter().map(|v| -v).collect();
    let path = root.join("opposed.wav");
    render::write_wav(&path, &l, &r).unwrap();
    let mut e = Engine::new(root.clone());
    let a = e.call("analyze_audio", &json!({"path":path})).unwrap();
    assert_eq!(a["onsets"], 6, "{a}");
    std::fs::remove_dir_all(root).unwrap();
}
fn hits(starts: &[usize], n: usize) -> Vec<f32> {
    let mut x = vec![0.; n];
    for &s in starts {
        for j in 0..1800 {
            x[s + j] = (j as f32 * 0.13).sin() * (-(j as f32) / 250.).exp() * 0.6;
        }
    }
    x
}
#[test]
fn duplicated_or_inverted_channels_do_not_inflate_onsets() {
    use beatbox::audio_edit::{onsets, onsets_stereo};
    let x = hits(&[8000, 30000, 57000, 83000, 119000, 148000], 176400);
    let r: Vec<f32> = x.iter().map(|v| -v).collect();
    let a = onsets(&x, 0.5, 60.);
    assert_eq!(a.len(), 6);
    assert_eq!(a, onsets_stereo(&x, &x, 0.5, 60.));
    assert_eq!(a, onsets_stereo(&x, &r, 0.5, 60.));
}
#[test]
fn alternating_channels_are_combined_and_swap_invariant() {
    use beatbox::audio_edit::onsets_stereo;
    let l = hits(&[8000, 57000, 119000], 176400);
    let r = hits(&[30000, 83000, 148000], 176400);
    let a = onsets_stereo(&l, &r, 0.5, 60.);
    assert_eq!(a.len(), 6);
    assert_eq!(a, onsets_stereo(&r, &l, 0.5, 60.));
}
#[test]
fn right_only_silent_short_empty_and_common_length_are_safe() {
    use beatbox::audio_edit::{onsets, onsets_stereo};
    let x = hits(&[8000, 30000], 44100);
    let z = vec![0.; x.len()];
    assert_eq!(onsets(&x, 0.5, 60.), onsets_stereo(&z, &x, 0.5, 60.));
    assert!(onsets_stereo(&z, &z, 0.5, 60.).is_empty());
    assert!(onsets_stereo(&[], &[], 0.5, 60.).is_empty());
    assert!(onsets_stereo(&[0.; 32], &[0.; 32], 0.5, 60.).is_empty());
    assert_eq!(onsets_stereo(&[0.; 32], &[0.5; 32], 0.5, 60.), vec![0]);
    let mut tail = x.clone();
    tail.extend(hits(&[8000], 44100));
    assert_eq!(onsets_stereo(&tail, &z, 0.5, 60.), onsets(&x, 0.5, 60.));
}
#[test]
fn sample_and_file_onsets_match_without_mutation_or_source_change() {
    let root = std::env::temp_dir().join(format!(
        "bb_detect_paths_{}",
        beatbox::creative::fresh_seed()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let l = hits(&[8000, 57000, 119000], 176400);
    let r = hits(&[30000, 83000, 148000], 176400);
    let path = root.join("hits.wav");
    render::write_wav(&path, &l, &r).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut e = Engine::new(root.clone());
    e.call("import_sample", &json!({"path":path,"name":"hits"}))
        .unwrap();
    e.project.samples[0].path = "hits.wav".into();
    let before = e.project.clone();
    let rev = e.revision;
    let depth = e.undo_depth();
    let a = e
        .call("analyze_audio", &json!({"sample":"HITS","max_onsets":2}))
        .unwrap();
    let b = e
        .call("analyze_audio", &json!({"path":path,"max_onsets":2}))
        .unwrap();
    assert_eq!(a["onsets"], 6);
    assert_eq!(a["onset_times_s"].as_array().unwrap().len(), 2);
    assert_eq!(a["onset_times_s"], b["onset_times_s"]);
    let c = e
        .call("analyze_audio", &json!({"sample":"hits","max_onsets":0}))
        .unwrap();
    assert_eq!(c["onsets"], 6);
    assert!(c["onset_times_s"].as_array().unwrap().is_empty());
    assert_eq!(e.project, before);
    assert_eq!(e.revision, rev);
    assert_eq!(e.undo_depth(), depth);
    assert_eq!(bytes, std::fs::read(&path).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}
