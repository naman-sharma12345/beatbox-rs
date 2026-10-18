//! Proof that the RT engine's block path (command drain, graph process,
//! xrun accounting) never touches the heap. A separate test binary, so its
//! counting global allocator cannot collide with any allocator the
//! library's unit tests install.

use beatbox::rt::{engine_pair, BlockCtx, GainNode, Node, NodeId, ParamEvent};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

fn bump() {
    let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        bump();
        System.alloc(l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        bump();
        System.alloc_zeroed(l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        bump();
        System.realloc(p, l, n)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
}

#[global_allocator]
static A: Counting = Counting;

/// A node that also takes param events, so the event path is exercised
/// inside the measured window too.
struct Pad;

impl Node for Pad {
    fn process(&mut self, _ctx: &BlockCtx, _inputs: &[&[f32]], outputs: &mut [&mut [f32]]) {
        for out in outputs.iter_mut() {
            out.fill(0.5);
        }
    }
    fn push_event(&mut self, ev: ParamEvent) {
        let _ = ev;
    }
}

/// Negative control: if the counting allocator is NOT the installed
/// global allocator in this binary, this test must fail - guarding the
/// no-alloc proof against a silently inert counter.
#[test]
fn counting_allocator_is_installed() {
    let before = ALLOCS.with(|c| c.get());
    let deliberate: Vec<u8> = vec![1, 2, 3];
    let after = ALLOCS.with(|c| c.get());
    assert!(
        after > before,
        "counting allocator did not observe a deliberate allocation"
    );
    std::hint::black_box(deliberate);
}

#[test]
fn render_block_allocates_nothing() {
    fn still_clock() -> u64 {
        0 // never advances: no xruns here; the xrun path itself is fill + fetch_add
    }
    // Everything that is allowed to allocate happens BEFORE the window.
    let (mut engine, mut handle) = engine_pair(512, u64::MAX, still_clock);
    let gain = engine
        .graph()
        .add_node(Box::new(GainNode::new(1.0, 44_100.0)));
    engine.graph().add_node(Box::new(Pad));
    handle.set_param(gain, 100, 0.5).map_err(|_| ()).unwrap();
    let mut out = vec![0.0f32; 512];
    engine.render_block(&mut out); // drains the queued event; warm any lazy path

    let before = ALLOCS.with(|c| c.get());
    for _ in 0..64 {
        engine.render_block(&mut out);
    }
    assert_eq!(
        ALLOCS.with(|c| c.get()),
        before,
        "the audio block path must not allocate"
    );
}

/// RT3 gate: the chunked voice streamer's block path (activation,
/// mixing, retirement) never touches the heap. The whole track is
/// rendered inside the measured window, so a realloc of an active list
/// or a hidden allocation in activation would be caught.
#[test]
fn voice_streamer_render_block_allocates_nothing() {
    // Everything that is allowed to allocate happens BEFORE the window:
    // the project, the plans, the streamer (which pre-reserves its
    // active lists to the full plan sizes) and the output buffers.
    let d = std::env::temp_dir().join("beatbox_noalloc_streamer");
    std::fs::create_dir_all(&d).unwrap();
    let mut e = beatbox::engine::Engine::new(d);
    e.call(
        "generate_beat",
        &serde_json::json!({"style": "trap", "seed": 9, "bars": 1}),
    )
    .unwrap();
    e.call("set_tempo", &serde_json::json!({"bpm": 200.0}))
        .unwrap();
    e.bank.sync(&e.project.samples);
    e.project.arrangement = vec![beatbox::project::Section {
        pattern: "main".into(),
        repeats: 1,
    }];
    let opts = beatbox::render::RenderOptions {
        tail: 0.03,
        ..Default::default()
    };
    let mut st = beatbox::rt::VoiceStreamer::new(&e.project, &e.bank, &opts, 512, 0);
    let mut l = vec![0.0f32; 512];
    let mut r = vec![0.0f32; 512];

    let before = ALLOCS.with(|c| c.get());
    while !st.done() {
        st.render_block(&mut l, &mut r);
    }
    assert_eq!(
        ALLOCS.with(|c| c.get()),
        before,
        "VoiceStreamer::render_block must not allocate"
    );
}

/// RT3 relay item 4: MixNode with a OneShot-fallback track (limiter)
/// plus streamed tracks, driven block by block through Node::process at
/// 512, 256 and 63 - no heap on the audio path. The OneShot track's
/// buffer was rendered at build (prepare), before this window.
#[test]
fn mix_node_with_oneshot_track_allocates_nothing() {
    use beatbox::rt::MixNode;
    let d = std::env::temp_dir().join("beatbox_noalloc_mixnode");
    std::fs::create_dir_all(&d).unwrap();
    let mut e = beatbox::engine::Engine::new(d);
    e.call(
        "generate_beat",
        &serde_json::json!({"style": "trap", "seed": 9, "bars": 1}),
    )
    .unwrap();
    e.call("set_tempo", &serde_json::json!({"bpm": 200.0}))
        .unwrap();
    e.bank.sync(&e.project.samples);
    e.project.arrangement = vec![beatbox::project::Section {
        pattern: "main".into(),
        repeats: 1,
    }];
    // force one track onto the OneShot fallback path
    e.call(
        "add_effect",
        &serde_json::json!({"track": "lead", "type": "limiter"}),
    )
    .unwrap();
    let opts = beatbox::render::RenderOptions {
        tail: 0.03,
        ..Default::default()
    };
    for block in [512usize, 256, 63] {
        let mut node = MixNode::new(&e.project, &e.bank, &opts, block);
        let total = node.total_samples();
        let mut l = vec![0.0f32; block];
        let mut r = vec![0.0f32; block];
        let before = ALLOCS.with(|c| c.get());
        let mut pos = 0u64;
        while (pos as usize) < total {
            let n = block.min(total - pos as usize);
            let ctx = BlockCtx {
                sample_rate: 44_100.0,
                block_len: n,
                frame_offset: pos,
            };
            let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
            node.process(&ctx, &[], &mut outs);
            pos += n as u64;
        }
        assert_eq!(
            ALLOCS.with(|c| c.get()),
            before,
            "MixNode::process must not allocate (block {block})"
        );
    }
}

/// RT4 relay: MixNode with PDC active - a 10ms-lookahead compressor on
/// the lead gives every dry track a 441-sample compensation delay line.
/// The lines are sized at prepare; process() itself must not allocate,
/// including driven past the render end (the PDC flush region).
#[test]
fn mix_node_with_pdc_allocates_nothing() {
    use beatbox::rt::MixNode;
    let d = std::env::temp_dir().join("beatbox_noalloc_pdc");
    std::fs::create_dir_all(&d).unwrap();
    let mut e = beatbox::engine::Engine::new(d);
    e.call(
        "generate_beat",
        &serde_json::json!({"style": "trap", "seed": 9, "bars": 1}),
    )
    .unwrap();
    e.call("set_tempo", &serde_json::json!({"bpm": 200.0}))
        .unwrap();
    e.bank.sync(&e.project.samples);
    e.project.arrangement = vec![beatbox::project::Section {
        pattern: "main".into(),
        repeats: 1,
    }];
    e.call(
        "add_effect",
        &serde_json::json!({
            "track": "lead", "type": "compressor",
            "params": {
                "threshold_db": -14.0, "ratio": 3.0,
                "knee_db": 6.0, "lookahead_ms": 10.0
            }
        }),
    )
    .unwrap();
    let opts = beatbox::render::RenderOptions {
        tail: 0.03,
        ..Default::default()
    };
    for block in [512usize, 256, 63] {
        let mut node = MixNode::new(&e.project, &e.bank, &opts, block);
        assert_eq!(node.latency(), 441, "10ms lookahead at 44.1k");
        let total = node.total_samples() + node.latency(); // include flush
        let mut l = vec![0.0f32; block];
        let mut r = vec![0.0f32; block];
        let before = ALLOCS.with(|c| c.get());
        let mut pos = 0u64;
        while (pos as usize) < total {
            let n = block.min(total - pos as usize);
            let ctx = BlockCtx {
                sample_rate: 44_100.0,
                block_len: n,
                frame_offset: pos,
            };
            let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
            node.process(&ctx, &[], &mut outs);
            pos += n as u64;
        }
        assert_eq!(
            ALLOCS.with(|c| c.get()),
            before,
            "MixNode::process with PDC must not allocate (block {block})"
        );
    }
}

/// RT5: a streamed master chain (lookahead compressor + non-true-peak
/// limiter) on the master bus must not allocate in process() either -
/// the limiter's sliding-min window is a fixed-capacity ring.
#[test]
fn mix_node_with_master_chain_allocates_nothing() {
    use beatbox::rt::MixNode;
    let d = std::env::temp_dir().join("beatbox_noalloc_master");
    std::fs::create_dir_all(&d).unwrap();
    let mut e = beatbox::engine::Engine::new(d);
    e.call(
        "generate_beat",
        &serde_json::json!({"style": "trap", "seed": 9, "bars": 1}),
    )
    .unwrap();
    e.call("set_tempo", &serde_json::json!({"bpm": 200.0}))
        .unwrap();
    e.bank.sync(&e.project.samples);
    e.project.arrangement = vec![beatbox::project::Section {
        pattern: "main".into(),
        repeats: 1,
    }];
    // streamed master chain: 2ms lookahead compressor + non-TP limiter
    // (the default true-peak limiter would keep the one-shot fallback)
    e.project.master_effects = vec![
        serde_json::from_value(serde_json::json!({
            "type": "compressor", "threshold_db": -14.0, "ratio": 3.0,
            "knee_db": 6.0, "lookahead_ms": 2.0
        }))
        .unwrap(),
        serde_json::from_value(serde_json::json!({
            "type": "limiter", "ceiling_db": -1.0, "release_ms": 80.0,
            "true_peak": false
        }))
        .unwrap(),
    ];
    let opts = beatbox::render::RenderOptions {
        tail: 0.03,
        ..Default::default()
    };
    for block in [512usize, 256, 63] {
        let mut node = MixNode::new(&e.project, &e.bank, &opts, block);
        assert_eq!(
            node.latency(),
            88 + 88,
            "master chain latency (block {block})"
        );
        let total = node.total_samples() + node.latency(); // include flush
        let mut l = vec![0.0f32; block];
        let mut r = vec![0.0f32; block];
        let before = ALLOCS.with(|c| c.get());
        let mut pos = 0u64;
        while (pos as usize) < total {
            let n = block.min(total - pos as usize);
            let ctx = BlockCtx {
                sample_rate: 44_100.0,
                block_len: n,
                frame_offset: pos,
            };
            let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
            node.process(&ctx, &[], &mut outs);
            pos += n as u64;
        }
        assert_eq!(
            ALLOCS.with(|c| c.get()),
            before,
            "MixNode::process with a streamed master chain must not allocate (block {block})"
        );
    }
}

/// RT17: a streamed master-chain multiband compressor must not
/// allocate in process() - the crossover cascades (8 biquads) and
/// the three band detector envelopes are built once and carried.
#[test]
fn mix_node_with_master_multiband_allocates_nothing() {
    use beatbox::rt::MixNode;
    let d = std::env::temp_dir().join("beatbox_noalloc_multiband");
    std::fs::create_dir_all(&d).unwrap();
    let mut e = beatbox::engine::Engine::new(d);
    e.call(
        "generate_beat",
        &serde_json::json!({"style": "trap", "seed": 9, "bars": 1}),
    )
    .unwrap();
    e.call("set_tempo", &serde_json::json!({"bpm": 200.0}))
        .unwrap();
    e.bank.sync(&e.project.samples);
    e.project.arrangement = vec![beatbox::project::Section {
        pattern: "main".into(),
        repeats: 1,
    }];
    e.project.master_effects = vec![serde_json::from_value(serde_json::json!({
        "type": "multiband", "low_freq": 200.0, "high_freq": 3000.0,
        "low_threshold_db": -20.0, "mid_threshold_db": -20.0, "high_threshold_db": -22.0,
        "low_ratio": 3.0, "mid_ratio": 2.0, "high_ratio": 2.5,
        "attack_ms": 15.0, "release_ms": 150.0, "mix": 1.0
    }))
    .unwrap()];
    let opts = beatbox::render::RenderOptions {
        tail: 0.03,
        ..Default::default()
    };
    for block in [512usize, 256] {
        let mut node = MixNode::new(&e.project, &e.bank, &opts, block);
        assert_eq!(
            node.latency(),
            0,
            "master multiband latency (block {block})"
        );
        let total = node.total_samples();
        let mut l = vec![0.0f32; block];
        let mut r = vec![0.0f32; block];
        let before = ALLOCS.with(|c| c.get());
        let mut pos = 0u64;
        while (pos as usize) < total {
            let n = block.min(total - pos as usize);
            let ctx = BlockCtx {
                sample_rate: 44_100.0,
                block_len: n,
                frame_offset: pos,
            };
            let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
            node.process(&ctx, &[], &mut outs);
            pos += n as u64;
        }
        assert_eq!(
            ALLOCS.with(|c| c.get()),
            before,
            "MixNode::process with a streamed master multiband must not allocate (block {block})"
        );
    }
}

/// RT18: a streamed master-chain dynamic EQ (unkeyed) must not
/// allocate in process() - the two band-split biquads and the shared
/// detector envelope are built once and carried. The threshold sits
/// above the fixture's level so the detector engages and the full
/// process path (bandpass + envelope + gain) is measured, not just
/// construction.
#[test]
fn mix_node_with_master_dynamic_eq_allocates_nothing() {
    use beatbox::rt::MixNode;
    let d = std::env::temp_dir().join("beatbox_noalloc_dynamic_eq");
    std::fs::create_dir_all(&d).unwrap();
    let mut e = beatbox::engine::Engine::new(d);
    e.call(
        "generate_beat",
        &serde_json::json!({"style": "trap", "seed": 9, "bars": 1}),
    )
    .unwrap();
    e.call("set_tempo", &serde_json::json!({"bpm": 200.0}))
        .unwrap();
    e.bank.sync(&e.project.samples);
    e.project.arrangement = vec![beatbox::project::Section {
        pattern: "main".into(),
        repeats: 1,
    }];
    e.project.master_effects = vec![serde_json::from_value(serde_json::json!({
        "type": "dynamic_eq", "freq": 3000.0, "q": 1.5,
        "threshold_db": -26.0, "ratio": 3.0, "range_db": -8.0,
        "attack_ms": 3.0, "release_ms": 90.0, "source": ""
    }))
    .unwrap()];
    let opts = beatbox::render::RenderOptions {
        tail: 0.03,
        ..Default::default()
    };
    for block in [512usize, 256] {
        let mut node = MixNode::new(&e.project, &e.bank, &opts, block);
        assert_eq!(
            node.latency(),
            0,
            "master dynamic_eq latency (block {block})"
        );
        let total = node.total_samples();
        let mut l = vec![0.0f32; block];
        let mut r = vec![0.0f32; block];
        let before = ALLOCS.with(|c| c.get());
        let mut pos = 0u64;
        while (pos as usize) < total {
            let n = block.min(total - pos as usize);
            let ctx = BlockCtx {
                sample_rate: 44_100.0,
                block_len: n,
                frame_offset: pos,
            };
            let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
            node.process(&ctx, &[], &mut outs);
            pos += n as u64;
        }
        assert_eq!(
            ALLOCS.with(|c| c.get()),
            before,
            "MixNode::process with a streamed master dynamic_eq must not allocate (block {block})"
        );
    }
}

/// Granular insert (streamed): the grain pool and the history rings are
/// allocated in ChainStreamer::build; process() must not allocate with
/// grains live, pitch-shifted reads and the frozen-slice path engaged.
#[test]
fn mix_node_with_master_granular_allocates_nothing() {
    use beatbox::rt::MixNode;
    let d = std::env::temp_dir().join("beatbox_noalloc_granular");
    std::fs::create_dir_all(&d).unwrap();
    let mut e = beatbox::engine::Engine::new(d);
    e.call(
        "generate_beat",
        &serde_json::json!({"style": "trap", "seed": 9, "bars": 1}),
    )
    .unwrap();
    e.call("set_tempo", &serde_json::json!({"bpm": 200.0}))
        .unwrap();
    e.bank.sync(&e.project.samples);
    e.project.arrangement = vec![beatbox::project::Section {
        pattern: "main".into(),
        repeats: 1,
    }];
    e.project.master_effects = vec![serde_json::from_value(serde_json::json!({
        "type": "granular", "grain_ms": 120.0, "density_hz": 60.0,
        "pitch_semitones": 5.0, "position_jitter_ms": 80.0, "spread": 0.8,
        "freeze_at_ms": 300.0, "mix": 1.0, "seed": 3
    }))
    .unwrap()];
    let opts = beatbox::render::RenderOptions {
        tail: 0.03,
        ..Default::default()
    };
    for block in [512usize, 256] {
        let mut node = MixNode::new(&e.project, &e.bank, &opts, block);
        assert_eq!(node.latency(), 0, "master granular latency (block {block})");
        let total = node.total_samples();
        let mut l = vec![0.0f32; block];
        let mut r = vec![0.0f32; block];
        let before = ALLOCS.with(|c| c.get());
        let mut pos = 0u64;
        while (pos as usize) < total {
            let n = block.min(total - pos as usize);
            let ctx = BlockCtx {
                sample_rate: 44_100.0,
                block_len: n,
                frame_offset: pos,
            };
            let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
            node.process(&ctx, &[], &mut outs);
            pos += n as u64;
        }
        assert_eq!(
            ALLOCS.with(|c| c.get()),
            before,
            "MixNode::process with a streamed master granular must not allocate (block {block})"
        );
    }
}

/// RT10: a streamed master cab_ir (partitioned FFT convolver, latency
/// 2047 reported to PDC) must not allocate in process() - the IR part
/// FFTs, delay lines, block buffers and accumulators are all
/// preallocated in ChainStreamer::build.
#[test]
fn mix_node_with_master_cab_ir_allocates_nothing() {
    use beatbox::rt::MixNode;
    let d = std::env::temp_dir().join("beatbox_noalloc_cab_ir");
    std::fs::create_dir_all(&d).unwrap();
    // deterministic stereo IR spanning two convolver parts
    let irl: Vec<f32> = (0..3000usize)
        .map(|i| (((i * 2654435761) % 2000) as f32 / 1000.0 - 1.0) * 0.5f32.powf(i as f32 / 1200.0))
        .collect();
    let irr: Vec<f32> = (0..3000usize)
        .map(|i| {
            (((i * 2246822519 + 7) % 2000) as f32 / 1000.0 - 1.0) * 0.5f32.powf(i as f32 / 1500.0)
        })
        .collect();
    let ir_path = d.join("test_ir.wav");
    beatbox::render::write_wav(&ir_path, &irl, &irr).unwrap();
    let mut e = beatbox::engine::Engine::new(d);
    e.call(
        "generate_beat",
        &serde_json::json!({"style": "trap", "seed": 9, "bars": 1}),
    )
    .unwrap();
    e.call("set_tempo", &serde_json::json!({"bpm": 200.0}))
        .unwrap();
    e.bank.sync(&e.project.samples);
    e.project.arrangement = vec![beatbox::project::Section {
        pattern: "main".into(),
        repeats: 1,
    }];
    e.project.master_effects = vec![serde_json::from_value(serde_json::json!({
        "type": "cab_ir", "path": ir_path.to_str().unwrap(),
        "max_ms": 200.0, "mix": 0.7, "gain_db": -2.0
    }))
    .unwrap()];
    let opts = beatbox::render::RenderOptions {
        tail: 0.03,
        ..Default::default()
    };
    for block in [512usize, 256, 63] {
        let mut node = MixNode::new(&e.project, &e.bank, &opts, block);
        assert_eq!(
            node.latency(),
            2047,
            "master cab_ir latency (block {block})"
        );
        let total = node.total_samples() + node.latency(); // include flush
        let mut l = vec![0.0f32; block];
        let mut r = vec![0.0f32; block];
        let before = ALLOCS.with(|c| c.get());
        let mut pos = 0u64;
        while (pos as usize) < total {
            let n = block.min(total - pos as usize);
            let ctx = BlockCtx {
                sample_rate: 44_100.0,
                block_len: n,
                frame_offset: pos,
            };
            let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
            node.process(&ctx, &[], &mut outs);
            pos += n as u64;
        }
        assert_eq!(
            ALLOCS.with(|c| c.get()),
            before,
            "MixNode::process with a streamed master cab_ir must not allocate (block {block})"
        );
    }
}

/// RT3 relay item 4: an Engine graph swap landing mid-run is pointer
/// moves only - the payload Vec is built control-side before the window,
/// old nodes return through the trash ring and are dropped control-side
/// after the window.
#[test]
fn graph_swap_mid_run_allocates_nothing() {
    fn still_clock() -> u64 {
        0
    }
    for block in [512usize, 256, 63] {
        let (mut engine, mut handle) = engine_pair(block, u64::MAX, still_clock);
        let g1 = engine
            .graph()
            .add_node(Box::new(GainNode::new(1.0, 44_100.0)));
        let nodes: Vec<(NodeId, Box<dyn Node>)> =
            vec![(g1, Box::new(GainNode::new(0.5, 44_100.0)))];
        let mut out = vec![0.0f32; block];
        engine.render_block(&mut out); // warm
        handle.swap_graph(nodes).map_err(|_| ()).unwrap();
        let before = ALLOCS.with(|c| c.get());
        for _ in 0..64 {
            engine.render_block(&mut out);
        }
        assert_eq!(
            ALLOCS.with(|c| c.get()),
            before,
            "graph swap mid-run must not allocate (block {block})"
        );
        // control-side deallocation, outside the measured window
        let _ = handle.collect_trash();
    }
}
