//! `beatbox waveform`: the headless waveform PNG from the command line.

use std::process::Command;

fn pixels(png: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut rd = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .unwrap();
    let mut buf = vec![0u8; rd.output_buffer_size().unwrap()];
    let info = rd.next_frame(&mut buf).unwrap();
    buf.truncate(info.buffer_size());
    (info.width, info.height, buf)
}

#[test]
fn cli_writes_a_waveform_png_from_a_wav() {
    let dir = std::env::temp_dir().join(format!("beatbox_waveform_cli_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // left: full-scale square, right: silence
    let l: Vec<f32> = (0..44_100)
        .map(|i| if (i / 50) % 2 == 0 { 0.999 } else { -0.999 })
        .collect();
    let r = vec![0.0f32; l.len()];
    let wav = dir.join("sq.wav");
    std::fs::write(&wav, beatbox::media::wav_bytes(&l, &r, 44_100)).unwrap();
    let mut outs = Vec::new();
    for (name, extra) in [
        ("a.png", vec![]),
        ("b.png", vec![]),
        ("c.png", vec!["--stored"]),
    ] {
        let out = dir.join(name);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_beatbox"));
        cmd.arg("--workdir")
            .arg(&dir)
            .arg("waveform")
            .arg(&wav)
            .arg("-o")
            .arg(&out)
            .args([
                "--width",
                "64",
                "--height",
                "40",
                "--stereo",
                "--background",
                "#000000",
                "--wave",
                "#ffffff",
                "--center",
                "#ff0000",
            ])
            .args(extra);
        let o = cmd.output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let report: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(report["width"], 64);
        assert_eq!(report["lanes"], "stereo");
        outs.push(std::fs::read(&out).unwrap());
    }
    assert_eq!(outs[0], outs[1], "same input, same bytes");
    let (w, h, px) = pixels(&outs[0]);
    assert_eq!((w, h), (64, 40));
    assert_eq!(
        pixels(&outs[2]).2,
        px,
        "stored and fixed decode to the same pixels"
    );
    let at = |x: usize, y: usize| {
        [
            px[(y * 64 + x) * 3],
            px[(y * 64 + x) * 3 + 1],
            px[(y * 64 + x) * 3 + 2],
        ]
    };
    for x in 0..64 {
        // top lane (rows 0..20): the square fills it
        for y in 0..20 {
            assert_eq!(at(x, y), [255, 255, 255], "({x},{y})");
        }
        // bottom lane (rows 20..40): silence is one row at 20 + round(9.5) = 30
        for y in 20..40 {
            assert_eq!(
                at(x, y),
                if y == 30 { [255, 255, 255] } else { [0, 0, 0] },
                "({x},{y})"
            );
        }
    }
    let bad = Command::new(env!("CARGO_BIN_EXE_beatbox"))
        .arg("--workdir")
        .arg(&dir)
        .args(["waveform"])
        .arg(&wav)
        .args(["--track", "kick"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
    let _ = std::fs::remove_dir_all(&dir);
}
