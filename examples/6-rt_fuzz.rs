//! Fuzz-ish probe: degenerate-but-parseable project JSONs through render.
//! Each case runs under catch_unwind; a panic is a finding.
//! usage: rt_fuzz
use beatbox::project::Project;
use beatbox::render::{self, RenderOptions};
use beatbox::samples::SampleBank;

type Case = (&'static str, fn(&mut serde_json::Value));

fn base() -> serde_json::Value {
    serde_json::json!({
        "name":"f","bpm":120.0,"swing":0.0,"key_root":"C","scale":"minor",
        "tracks":[{"id":"t1","name":"kick","instrument":{"type":"drum","kind":"kick"},
            "volume_db":-1.0,"pan":0.0,"mute":false,"solo":false,"effects":[]}],
        "patterns":[{"id":"p1","name":"main","bars":1,"clips":{
            "kick":[{"start":0.0,"len":1.0,"pitch":36,"vel":0.9,"id":"n1"}]}}],
        "arrangement":[{"pattern":"main","repeats":1}],
        "master_effects":[],"master_volume_db":0.0,"samples":[],
        "schema_version":2,"id_seq":10,"seed_log":[]
    })
}

fn main() {
    let cases: Vec<Case> = vec![
        ("bpm=0", |v| v["bpm"] = 0.0.into()),
        ("bpm=-120", |v| v["bpm"] = (-120.0).into()),
        ("bpm=1e300", |v| v["bpm"] = 1e300.into()),
        ("swing=5", |v| v["swing"] = 5.0.into()),
        ("swing=-1", |v| v["swing"] = (-1.0).into()),
        ("no tracks", |v| v["tracks"] = serde_json::json!([])),
        ("bars=0", |v| v["patterns"][0]["bars"] = 0.into()),
        ("clip len=0", |v| {
            v["patterns"][0]["clips"]["kick"][0]["len"] = 0.0.into()
        }),
        ("clip len=-2", |v| {
            v["patterns"][0]["clips"]["kick"][0]["len"] = (-2.0).into()
        }),
        ("clip start=1e9", |v| {
            v["patterns"][0]["clips"]["kick"][0]["start"] = 1e9.into()
        }),
        ("pitch=255", |v| {
            v["patterns"][0]["clips"]["kick"][0]["pitch"] = 255.into()
        }),
        ("pitch=-3", |v| {
            v["patterns"][0]["clips"]["kick"][0]["pitch"] = (-3).into()
        }),
        ("vel=1e9", |v| {
            v["patterns"][0]["clips"]["kick"][0]["vel"] = 1e9.into()
        }),
        ("vel=-2", |v| {
            v["patterns"][0]["clips"]["kick"][0]["vel"] = (-2.0).into()
        }),
        ("volume_db=-400", |v| {
            v["tracks"][0]["volume_db"] = (-400.0).into()
        }),
        ("volume_db=1e38", |v| {
            v["tracks"][0]["volume_db"] = 1e38.into()
        }),
        ("pan=99", |v| v["tracks"][0]["pan"] = 99.0.into()),
        ("arrangement missing pattern", |v| {
            v["arrangement"] = serde_json::json!([{"pattern":"ghost","repeats":1}])
        }),
        ("arrangement repeats=0", |v| {
            v["arrangement"][0]["repeats"] = 0.into()
        }),
        ("empty arrangement+patterns", |v| {
            v["arrangement"] = serde_json::json!([]);
            v["patterns"] = serde_json::json!([]);
        }),
        ("clip for unknown track", |v| {
            let c = v["patterns"][0]["clips"].clone();
            let mut c = c;
            c["ghost"] =
                serde_json::json!([{"start":0.0,"len":1.0,"pitch":36,"vel":0.9,"id":"n2"}]);
            v["patterns"][0]["clips"] = c;
        }),
        (
            "fx cutoff=-100",
            |v| v["tracks"][0]["effects"] = serde_json::json!([{"type":"filter","mode":"lowpass","cutoff":-100.0,"resonance":0.2}]),
        ),
        (
            "fx delay steps=0",
            |v| {
                v["tracks"][0]["effects"] =
                    serde_json::json!([{"type":"delay","steps":0.0,"feedback":0.35,"mix":0.3}])
            },
        ),
        (
            "fx delay feedback=2",
            |v| {
                v["tracks"][0]["effects"] =
                    serde_json::json!([{"type":"delay","steps":3.0,"feedback":2.0,"mix":0.3}])
            },
        ),
        (
            "fx stutter slice=0",
            |v| v["tracks"][0]["effects"] = serde_json::json!([{"type":"stutter","slice_steps":0.0,"cycle_steps":8.0,"mix":1.0}]),
        ),
        (
            "fx bitcrush bits=0",
            |v| {
                v["tracks"][0]["effects"] =
                    serde_json::json!([{"type":"bitcrush","bits":0.0,"downsample":1,"mix":1.0}])
            },
        ),
        (
            "fx reverb size=99",
            |v| {
                v["tracks"][0]["effects"] =
                    serde_json::json!([{"type":"reverb","size":99.0,"mix":0.5}])
            },
        ),
        (
            "fx pitch_shift semis=1e9",
            |v| {
                v["tracks"][0]["effects"] =
                    serde_json::json!([{"type":"pitch_shift","semitones":1e9}])
            },
        ),
        (
            "fx limiter ceiling=+20",
            |v| v["master_effects"] = serde_json::json!([{"type":"limiter","ceiling_db":20.0,"release_ms":80.0,"true_peak":true}]),
        ),
        (
            "fx mix=-1",
            |v| {
                v["tracks"][0]["effects"] =
                    serde_json::json!([{"type":"distortion","drive":0.5,"mix":-1.0}])
            },
        ),
        ("key_root=H", |v| v["key_root"] = "H".into()),
        ("scale=weird", |v| v["scale"] = "weird".into()),
        (
            "automation neg beat",
            |v| v["automation"] = serde_json::json!([{"target":"kick","param":"volume","points":[{"beat":-5.0,"value":0.0},{"beat":2.0,"value":-6.0}]}]),
        ),
        (
            "automation beats reversed",
            |v| v["automation"] = serde_json::json!([{"target":"kick","param":"volume","points":[{"beat":8.0,"value":0.0},{"beat":2.0,"value":-6.0}]}]),
        ),
        (
            "automation dup beats",
            |v| v["automation"] = serde_json::json!([{"target":"kick","param":"volume","points":[{"beat":2.0,"value":0.0},{"beat":2.0,"value":-6.0}]}]),
        ),
        (
            "automation missing track",
            |v| v["automation"] = serde_json::json!([{"target":"ghost","param":"volume","points":[{"beat":0.0,"value":0.0},{"beat":2.0,"value":-6.0}]}]),
        ),
        (
            "automation weird param",
            |v| v["automation"] = serde_json::json!([{"target":"kick","param":"fx.99.nonexistent","points":[{"beat":0.0,"value":0.0},{"beat":2.0,"value":1.0}]}]),
        ),
        ("bpm=0 bars=0", |v| {
            v["bpm"] = 0.0.into();
            v["patterns"][0]["bars"] = 0.into();
        }),
    ];
    let bank = SampleBank::default();
    let opts = RenderOptions {
        tail: 0.1,
        ..Default::default()
    };
    let mut panics = 0;
    for (name, mutate) in &cases {
        let mut v = base();
        mutate(&mut v);
        let p: Project = match serde_json::from_value(v) {
            Ok(p) => p,
            Err(e) => {
                println!("{name}: serde reject ({e})");
                continue;
            }
        };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            render::render(&p, &bank, &opts)
        }));
        match r {
            Ok(Ok(m)) => {
                let bad = m.left.iter().any(|x| x.is_nan() || x.is_infinite())
                    || m.right.iter().any(|x| x.is_nan() || x.is_infinite());
                println!(
                    "{name}: rendered {} samples{}",
                    m.left.len(),
                    if bad { " <NaN/inf in output>" } else { "" }
                );
            }
            Ok(Err(e)) => println!(
                "{name}: clean error: {}",
                &e.to_string()[..e.to_string().len().min(90)]
            ),
            Err(_) => {
                panics += 1;
                println!("{name}: *** PANIC ***");
            }
        }
    }
    println!("---\n{} panics out of {}", panics, cases.len());
}
