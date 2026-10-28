use beatbox::{analysis, dsp::SR};
#[test]
fn spectral_stats_do_not_erase_opposed_stereo() {
    let x: Vec<f32> = (0..88200)
        .map(|i| (i as f32 * 1000. * std::f32::consts::TAU / SR).sin() * 0.5)
        .collect();
    let inv: Vec<f32> = x.iter().map(|v| -v).collect();
    let same = analysis::stats(&x, &x);
    let opposed = analysis::stats(&x, &inv);
    assert!(
        opposed.spectral_centroid_hz > 990. && opposed.spectral_centroid_hz < 1010.,
        "{opposed:?}"
    );
    assert_eq!(same.spectral_centroid_hz, opposed.spectral_centroid_hz);
    for (a, b) in same.bands.iter().zip(&opposed.bands) {
        assert_eq!(a.percent, b.percent);
    }
}

fn tone(hz: f32, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (i as f32 * hz * std::f32::consts::TAU / SR).sin() * 0.5)
        .collect()
}
#[test]
fn spectral_stats_are_channel_swap_and_polarity_invariant() {
    let l = tone(110., 88200);
    let r = tone(5000., 88200);
    let inv: Vec<f32> = r.iter().map(|v| -v).collect();
    let a = analysis::stats(&l, &r);
    let b = analysis::stats(&r, &l);
    let c = analysis::stats(&l, &inv);
    assert!(a.bands.iter().find(|b| b.band == "bass").unwrap().percent > 45.);
    assert!(
        a.bands
            .iter()
            .find(|b| b.band == "presence")
            .unwrap()
            .percent
            > 45.
    );
    assert_eq!(a.spectral_centroid_hz, b.spectral_centroid_hz);
    assert_eq!(a.spectral_centroid_hz, c.spectral_centroid_hz);
    for ((x, y), z) in a.bands.iter().zip(&b.bands).zip(&c.bands) {
        assert_eq!(x.percent, y.percent);
        assert_eq!(x.percent, z.percent);
    }
}
#[test]
fn right_only_and_unequal_lengths_use_common_stereo_frames() {
    let x = tone(110., 88200);
    let zero = vec![0.; x.len()];
    let a = analysis::stats(&x, &zero);
    let b = analysis::stats(&zero, &x);
    assert_eq!(a.spectral_centroid_hz, b.spectral_centroid_hz);
    assert!(a.bands.iter().find(|b| b.band == "bass").unwrap().percent > 99.);
    let mut tail = x.clone();
    tail.extend(tone(5000., 88200));
    let c = analysis::stats(&tail, &zero);
    assert_eq!(a.spectral_centroid_hz, c.spectral_centroid_hz);
}
#[test]
fn phase_and_mono_fold_diagnostics_still_warn() {
    let x = tone(1000., 88200);
    let r: Vec<f32> = x.iter().map(|v| -v).collect();
    assert!(analysis::correlation(&x, &r) < -0.99);
    assert!(analysis::loudness(&x, &r).mono_fold_db < -20.);
    let s = analysis::stats(&[], &[]);
    assert_eq!(s.spectral_centroid_hz, 0.);
    assert!(s.bands.iter().all(|b| b.percent == 0.));
    let short = analysis::stats(&[0.5, -0.5], &[-0.5, 0.5]);
    assert!(short.spectral_centroid_hz.is_finite());
}
#[test]
fn analyze_registered_and_file_paths_report_nonzero_spectrum_without_mutation() {
    use serde_json::json;
    let root = std::env::temp_dir().join(format!(
        "bb_stereo_spectrum_{}",
        beatbox::creative::fresh_seed()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let x = tone(1000., 88200);
    let r: Vec<f32> = x.iter().map(|v| -v).collect();
    let path = root.join("opposed.wav");
    beatbox::render::write_wav(&path, &x, &r).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut e = beatbox::Engine::new(root.clone());
    e.call("import_sample", &json!({"path":path,"name":"wide"}))
        .unwrap();
    e.project.samples[0].path = "opposed.wav".into();
    let before = e.project.clone();
    let rev = e.revision;
    let depth = e.undo_depth();
    let a = e.call("analyze_audio", &json!({"sample":"WIDE"})).unwrap();
    let b = e.call("analyze_audio", &json!({"path":path})).unwrap();
    assert_eq!(a["bands"], b["bands"]);
    assert_eq!(a["spectral_centroid_hz"], b["spectral_centroid_hz"]);
    assert!((a["spectral_centroid_hz"].as_f64().unwrap() - 1000.).abs() < 10.);
    assert_eq!(a["stereo_correlation"], -1.0);
    assert_eq!(e.project, before);
    assert_eq!(e.revision, rev);
    assert_eq!(e.undo_depth(), depth);
    assert_eq!(bytes, std::fs::read(&path).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn mono_spectrum_matches_existing_mono_fft() {
    let x = tone(110., 88200);
    let spec = analysis::power_spectrum(&x);
    let hz = SR / 4096.;
    let total = spec.iter().skip(1).sum::<f32>().max(1e-12);
    let centroid = (spec
        .iter()
        .enumerate()
        .map(|(k, p)| k as f32 * hz * p)
        .sum::<f32>()
        / total)
        .round();
    let a = analysis::stats(&x, &x);
    assert_eq!(a.spectral_centroid_hz, centroid);
    for b in a.bands {
        let (_, lo, hi) = analysis::BANDS
            .iter()
            .find(|(n, _, _)| *n == b.band)
            .unwrap();
        let en = spec
            .iter()
            .enumerate()
            .filter(|(k, _)| *k as f32 * hz >= *lo && (*k as f32) * hz < *hi)
            .map(|(_, p)| p)
            .sum::<f32>();
        assert_eq!(b.percent, (en / total * 1000.).round() / 10.);
    }
}
