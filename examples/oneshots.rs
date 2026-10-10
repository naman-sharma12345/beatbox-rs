//! Render one-shots of every instrument preset to 32-bit float WAVs so the
//! sound palette can be audited (scripts/sound_audit.py) and compared
//! before/after a change. Uses only the stable public API (presets +
//! render_note), so it builds against older revisions too.
//!
//! usage: cargo run --release --example oneshots -- OUT_DIR

use beatbox::dsp::SR;
use beatbox::instruments::{preset, render_note, Instrument, PRESETS};
use beatbox::samples::SampleBank;
use std::io::Write;
use std::path::Path;

fn write_f32_wav(path: &Path, x: &[f32]) -> std::io::Result<()> {
    let data_len = (x.len() * 4) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&(SR as u32).to_le_bytes());
    out.extend_from_slice(&((SR as u32) * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in x {
        out.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::File::create(path)?.write_all(&out)
}

/// (pitch, velocity, gate) points to audition a preset at.
fn points(name: &str, inst: &Instrument) -> Vec<(&'static str, f32, f32, f32)> {
    let low = name.contains("808") || name.contains("bass") || name == "sub_bass";
    let pad = name.contains("pad")
        || name.contains("string")
        || name.contains("choir")
        || name.contains("tanpura")
        || name.contains("brass_section")
        || name.contains("cello");
    if inst.is_drum() {
        vec![("v100", 60.0, 1.0, 0.25), ("v060", 60.0, 0.6, 0.25)]
    } else if low {
        vec![
            ("c1", 24.0, 1.0, 0.6),
            ("g1", 31.0, 0.8, 0.6),
            ("c2", 36.0, 1.0, 0.6),
        ]
    } else if pad {
        vec![("c3", 48.0, 0.8, 1.5), ("c5", 72.0, 0.8, 1.5)]
    } else {
        vec![
            ("c4_v100", 60.0, 1.0, 0.5),
            ("c4_v050", 60.0, 0.5, 0.5),
            ("c6", 84.0, 0.9, 0.5),
            ("c7", 96.0, 0.9, 0.3),
        ]
    }
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "oneshots".into());
    let dir = Path::new(&out);
    std::fs::create_dir_all(dir).expect("out dir");
    let bank = SampleBank::default();
    let mut index = String::from("name,kind,point,pitch,vel,gate,seed,file\n");
    for (name, _) in PRESETS {
        let Some(inst) = preset(name) else { continue };
        if inst.kind_name() == "sampler" || inst.kind_name() == "multisample" {
            continue;
        }
        for (tag, pitch, vel, gate) in points(name, &inst) {
            // drums: four seeds to see round-robin variation
            let seeds: &[u64] = if inst.is_drum() && tag == "v100" {
                &[1, 2, 3, 4]
            } else {
                &[1]
            };
            for &seed in seeds {
                let buf = render_note(&inst, pitch, vel, gate, &bank, seed);
                let file = if seeds.len() > 1 {
                    format!("{name}__{tag}__rr{seed}.wav")
                } else {
                    format!("{name}__{tag}.wav")
                };
                write_f32_wav(&dir.join(&file), &buf).expect("write wav");
                index.push_str(&format!(
                    "{name},{},{tag},{pitch},{vel},{gate},{seed},{file}\n",
                    inst.kind_name()
                ));
            }
        }
    }
    std::fs::write(dir.join("index.csv"), index).expect("index");
    println!("wrote one-shots to {}", dir.display());
}
