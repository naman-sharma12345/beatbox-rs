//! Alias and cost bench for oversampled ADAA (src/dsp/adaa_oversampled.rs).
//! Prints the tables in docs/ADAA_OVERSAMPLING.md.
//!
//! For each curve (hard, tanh, cubic, quintic) and drive (+6 / +12 / +24 dB
//! on a unit 1 kHz sine at 44.1 kHz): alias suppression against the naive
//! shaper at 1x, for naive / ADAA-1 / ADAA-2 at 1x / 2x / 4x, on two
//! measures: total alias power up to Nyquist, and alias power below
//! 20 kHz only. Then the cost in ns per input (base-rate) sample.
//!
//! Note on the 2x/4x total figures for low notes: they stop at the half-band
//! oversampler's transition floor (0.43 to 0.57 fs: 19.0 to 25.1 kHz at
//! 44.1 kHz, 20.6 to 27.4 kHz at 48 kHz), whose leftovers fold to ~21 kHz.
//! That is filter leakage, not ADAA error, and 4x shares the stage that
//! sets it.
//!
//! usage: cargo run --release --example adaa_oversampled_bench

use beatbox::dsp::adaa::{AdaaOrder, HardClip, Nonlinearity, SoftClip3, SoftClip5, Tanh};
use beatbox::dsp::adaa_oversampled::{OversampledShaper, Oversampling};
use std::f64::consts::PI;
use std::hint::black_box;
use std::time::Instant;

const FS: f64 = 44_100.0;
const N: usize = 1 << 16;
const WARM: usize = 8192;
const BIN: usize = 1487; // 1000.6 Hz
const BLOCK: usize = 512;
const MODES: [Oversampling; 3] = [Oversampling::X1, Oversampling::X2, Oversampling::X4];
const ORDERS: [(AdaaOrder, &str); 3] = [(AdaaOrder::Naive, "naive"), (AdaaOrder::First, "ADAA-1"), (AdaaOrder::Second, "ADAA-2")];
const DRIVES_DB: [f64; 3] = [6.0, 12.0, 24.0];

fn fft64(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f64;
        for k in 0..len / 2 {
            let (wi, wr) = (ang * k as f64).sin_cos();
            for start in (0..n).step_by(len) {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * wr - im[b] * wi;
                let ti = re[b] * wi + im[b] * wr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

/// (total alias dB, below-20-kHz alias dB), relative to the fundamental.
fn alias_db(y: &[f32]) -> (f64, f64) {
    alias_db_at(y, BIN)
}

/// Same for a fundamental at FFT bin `bin`; its harmonics are not aliases.
fn alias_db_at(y: &[f32], bin: usize) -> (f64, f64) {
    let n = y.len();
    let mut re: Vec<f64> = y.iter().map(|v| *v as f64).collect();
    let mut im = vec![0.0; n];
    fft64(&mut re, &mut im);
    let p = |k: usize| re[k] * re[k] + im[k] * im[k];
    let (mut tot, mut aud) = (0.0, 0.0);
    for k in (1..=n / 2).filter(|k| k % bin != 0) {
        tot += p(k);
        if k as f64 * FS / n as f64 <= 20_000.0 {
            aud += p(k);
        }
    }
    (10.0 * (tot / p(bin)).log10(), 10.0 * (aud / p(bin)).log10())
}

fn run<N2: Nonlinearity + Clone>(nl: &N2, order: AdaaOrder, mode: Oversampling, drive: f64, x: &[f32]) -> Vec<f32> {
    let mut s = OversampledShaper::with_oversampling(nl.clone(), order, drive, mode);
    let mut buf = x.to_vec();
    buf.chunks_mut(BLOCK).for_each(|c| s.process_block(c));
    buf
}

fn ns_per_sample<N2: Nonlinearity + Clone>(nl: &N2, order: AdaaOrder, mode: Oversampling, x: &[f32]) -> f64 {
    let mut s = OversampledShaper::with_oversampling(nl.clone(), order, 4.0, mode);
    let mut buf = x.to_vec();
    let mut runs = Vec::new();
    for _ in 0..9 {
        buf.copy_from_slice(x);
        let t = Instant::now();
        for c in buf.chunks_mut(BLOCK) {
            s.process_block(black_box(c));
        }
        runs.push(t.elapsed().as_nanos() as f64 / x.len() as f64);
        black_box(&buf);
    }
    runs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    runs[4]
}

fn curve<N2: Nonlinearity + Clone>(name: &str, nl: N2, x: &[f32], noise: &[f32]) {
    println!("\n## {name}\n");
    println!("Alias suppression vs naive 1x, dB (total to Nyquist / below 20 kHz). Higher is better.\n");
    println!("| drive | rate | naive | ADAA-1 | ADAA-2 |");
    println!("|---|---|---|---|---|");
    for d in DRIVES_DB {
        let drive = 10f64.powf(d / 20.0);
        let (rt, ra) = alias_db(&run(&nl, AdaaOrder::Naive, Oversampling::X1, drive, x)[WARM..]);
        for mode in MODES {
            let mut row = format!("| +{d:.0} dB | {}x |", mode.factor());
            for (order, _) in ORDERS {
                let (t, a) = alias_db(&run(&nl, order, mode, drive, x)[WARM..]);
                row += &format!(" {:.1} / {:.1} |", rt - t, ra - a);
            }
            println!("{row}");
        }
        println!("| | naive 1x level | {rt:.1} / {ra:.1} dBc | | |");
    }
    println!("\nCost, ns per input sample (median of 9, blocks of {BLOCK}, noise at drive 4):\n");
    println!("| rate | naive | ADAA-1 | ADAA-2 |");
    println!("|---|---|---|---|");
    for mode in MODES {
        let mut row = format!("| {}x |", mode.factor());
        for (order, _) in ORDERS {
            row += &format!(" {:.1} |", ns_per_sample(&nl, order, mode, noise));
        }
        println!("{row}");
    }
}

fn main() {
    let x: Vec<f32> = (0..N + WARM).map(|i| (2.0 * PI * BIN as f64 * i as f64 / N as f64).sin() as f32).collect();
    let mut s = 0x2545_F491_4F6C_DD1Du64;
    let noise: Vec<f32> = (0..1 << 18)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            ((s >> 11) as f64 / (1u64 << 52) as f64 - 1.0) as f32
        })
        .collect();
    println!("# Oversampled ADAA bench: 1 kHz ({:.1} Hz) unit sine at 44.1 kHz, {N}-point FFT", BIN as f64 * FS / N as f64);
    println!("\nLatency (base-rate samples, naive / ADAA-1 / ADAA-2):");
    for mode in MODES {
        println!("- {}x: {} / {} / {}", mode.factor(), mode.latency(AdaaOrder::Naive), mode.latency(AdaaOrder::First), mode.latency(AdaaOrder::Second));
    }
    // A high note, where 4x does pay off on total alias power.
    let bin5 = 7431; // 5000.3 Hz
    let x5: Vec<f32> = (0..N + WARM).map(|i| (2.0 * PI * bin5 as f64 * i as f64 / N as f64).sin() as f32).collect();
    let drive = 10f64.powf(20.0 / 20.0);
    let (rt, ra) = alias_db_at(&run(&HardClip, AdaaOrder::Naive, Oversampling::X1, drive, &x5)[WARM..], bin5);
    println!("\n## hard clip, 5 kHz sine at +20 dB\n");
    println!("Alias suppression vs naive 1x ({rt:.1} / {ra:.1} dBc), dB (total / below 20 kHz).\n");
    println!("| rate | naive | ADAA-1 | ADAA-2 |");
    println!("|---|---|---|---|");
    for mode in MODES {
        let mut row = format!("| {}x |", mode.factor());
        for (order, _) in ORDERS {
            let (t, a) = alias_db_at(&run(&HardClip, order, mode, drive, &x5)[WARM..], bin5);
            row += &format!(" {:.1} / {:.1} |", rt - t, ra - a);
        }
        println!("{row}");
    }
    curve("hard clip", HardClip, &x, &noise);
    curve("tanh", Tanh, &x, &noise);
    curve("cubic (x - x^3/3)", SoftClip3, &x, &noise);
    curve("quintic (C2)", SoftClip5, &x, &noise);
}
