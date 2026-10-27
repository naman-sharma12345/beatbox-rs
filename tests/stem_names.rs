use beatbox::Engine;
use serde_json::{json, Value};
use std::{collections::HashSet, fs};

fn fixture(names: &[&str]) -> Engine {
    let mut e = Engine::new(
        std::env::temp_dir().join(format!("bb238_{}", beatbox::creative::fresh_seed())),
    );
    e.call("new_project", &json!({"name":"stem safety"}))
        .unwrap();
    e.call("set_pattern_length", &json!({"bars":1})).unwrap();
    for (i, name) in names.iter().enumerate() {
        e.call("add_track", &json!({"name":name,"preset":"piano"}))
            .unwrap();
        e.call(
            "add_notes",
            &json!({"track":name,"notes":[{"start":0,"pitch":60+i*3,"len":1}]}),
        )
        .unwrap();
    }
    e
}
fn paths(v: &Value) -> Vec<String> {
    v["stems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["path"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn render_stems_keep_distinct_tracks_and_reserve_natural_suffixes() {
    let mut e = fixture(&["keys one", "keys_one", "keys_one_2", "solo"]);
    let prior = e.project.clone();
    let expected = e.render_stems_all(&prior).unwrap();
    let out = e
        .call("render", &json!({"stems_dir":"out","tail":1.5}))
        .unwrap();
    let files: Vec<&str> = out["stems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(files.len(), files.iter().collect::<HashSet<_>>().len());
    assert!(files[1].ends_with("keys_one_3.wav"));
    assert!(files[2].ends_with("keys_one_2.wav"));
    assert!(files[3].ends_with("solo.wav"));
    for (s, file) in expected.stems.iter().zip(&files) {
        let reference = e.workdir.join("expected.wav");
        beatbox::render::write_wav(&reference, &s.left, &s.right).unwrap();
        assert_eq!(fs::read(file).unwrap(), fs::read(reference).unwrap());
    }
    let before: Vec<_> = files.iter().map(|f| fs::read(f).unwrap()).collect();
    let again = e
        .call("render", &json!({"stems_dir":"out","tail":1.5}))
        .unwrap();
    assert_eq!(out["stems"], again["stems"]);
    assert_eq!(
        before,
        files
            .iter()
            .map(|f| fs::read(f).unwrap())
            .collect::<Vec<_>>()
    );
    assert_eq!(prior, e.project);
}

#[test]
fn delivery_names_are_stable_for_subsets_and_bus_prefix_collisions() {
    let mut e = fixture(&["keys one", "keys_one", "bus_room", "keys_one_2"]);
    e.call("add_bus", &json!({"name":"room","preset":"none"}))
        .unwrap();
    let prior = e.project.clone();
    let opts = json!({"dir":"delivery","stem_format":"f32","stem_gain":"common"});
    let all = e.call("export_stems", &opts).unwrap();
    let allpaths = paths(&all);
    assert_eq!(allpaths.len(), 5);
    assert_eq!(
        allpaths.len(),
        allpaths.iter().collect::<HashSet<_>>().len()
    );
    assert!(allpaths[4].ends_with("bus_room_2.wav"));
    let bytes: Vec<_> = allpaths.iter().map(|p| fs::read(p).unwrap()).collect();
    let mut subset = opts.clone();
    subset["tracks"] = json!(["keys_one"]);
    let one = e.call("export_stems", &subset).unwrap();
    assert_eq!(paths(&one), vec![allpaths[1].clone()]);
    assert_eq!(
        bytes,
        allpaths
            .iter()
            .map(|p| fs::read(p).unwrap())
            .collect::<Vec<_>>()
    );
    let mut without_buses = opts.clone();
    without_buses["include_buses"] = json!(false);
    let tracks_only = e.call("export_stems", &without_buses).unwrap();
    assert_eq!(paths(&tracks_only), allpaths[..4]);
    let mut replay = opts.clone();
    replay["stem_gain_db"] = all["stem_gain_db_pin"].clone();
    let again = e.call("export_stems", &replay).unwrap();
    assert_eq!(allpaths, paths(&again));
    assert_eq!(
        bytes,
        allpaths
            .iter()
            .map(|p| fs::read(p).unwrap())
            .collect::<Vec<_>>()
    );
    assert_eq!(prior, e.project);
    let save = e.workdir.join("reopen.json");
    e.call("save_project", &json!({"path":save})).unwrap();
    let mut reopened = Engine::new(e.workdir.clone());
    reopened
        .call("load_project", &json!({"path":save}))
        .unwrap();
    assert_eq!(reopened.project, prior);
    let loaded = reopened.call("export_stems", &replay).unwrap();
    assert_eq!(paths(&loaded), allpaths);
    assert_eq!(
        bytes,
        allpaths
            .iter()
            .map(|p| fs::read(p).unwrap())
            .collect::<Vec<_>>()
    );
}

#[test]
fn truncated_names_have_distinct_decodable_files() {
    let prefix = "a".repeat(48);
    let mut e = fixture(&[&format!("{prefix}x"), &format!("{prefix}y")]);
    let a = e
        .call(
            "export_stems",
            &json!({"dir":"out","format":"wav","bits":24}),
        )
        .unwrap();
    let files = paths(&a);
    assert_ne!(files[0], files[1]);
    let audio: Vec<_> = files
        .iter()
        .map(|p| beatbox::samples::decode_stereo(std::path::Path::new(p)).unwrap())
        .collect();
    assert_ne!(audio[0], audio[1]);
    assert_eq!(audio[0].0.len(), audio[1].0.len());
}

#[test]
fn flac_and_float_trim_use_the_same_distinct_mapping() {
    let mut e = fixture(&["keys one", "keys_one"]);
    let out = e
        .call(
            "export_stems",
            &json!({"dir":"float","stem_format":"f32","tp_trim":"common"}),
        )
        .unwrap();
    let floats = paths(&out);
    assert_ne!(floats[0], floats[1]);
    assert_eq!(out["tp_trim"]["converged"], true);
    let flac = e
        .call("export_stems", &json!({"dir":"flac","format":"flac"}))
        .unwrap();
    let files = paths(&flac);
    assert_ne!(files[0], files[1]);
    for (f, w) in files.iter().zip(floats) {
        assert_eq!(
            std::path::Path::new(f).file_stem(),
            std::path::Path::new(&w).file_stem()
        );
        let (l, r) = beatbox::samples::decode_stereo(std::path::Path::new(f)).unwrap();
        assert!(!l.is_empty());
        assert_eq!(l.len(), r.len());
    }
}
