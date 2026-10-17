//! Proof that the multiband compressor never touches the heap while
//! it processes. A separate test binary, so its counting global allocator
//! cannot collide with any allocator the library's unit tests install.

use beatbox::dsp::multiband::{BandParams, MultibandDynamics};
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

fn count<F: FnOnce()>(f: F) -> usize {
    let before = ALLOCS.with(|c| c.get());
    f();
    ALLOCS.with(|c| c.get()) - before
}

#[test]
fn processing_never_allocates() {
    // positive control: the counter sees an allocation on this thread
    assert!(count(|| drop(std::hint::black_box(vec![1u8; 64]))) >= 1);

    let mut s = 1u32;
    let mut noise = move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        (s as f32 / u32::MAX as f32) * 8.0 - 4.0
    };
    // setup (allocates) outside the counted region
    let big: Vec<f32> = (0..20_000).map(|_| noise()).collect();
    let hot = BandParams {
        threshold_db: -30.0,
        ratio: 8.0,
        ..Default::default()
    };
    for (sr, nch, xo) in [
        (44_100.0f32, 2usize, vec![150.0f32, 2500.0]),
        (48_000.0, 1, vec![1000.0]),
        (96_000.0, 6, vec![80.0, 400.0, 3000.0, 9000.0]),
    ] {
        let mut m = MultibandDynamics::new(sr, nch, &xo, &[hot; 5]);
        let mut bufs: Vec<Vec<f32>> = (0..nch).map(|_| big.clone()).collect();
        let mut refs: Vec<&mut [f32]> = bufs.iter_mut().map(|b| b.as_mut_slice()).collect();
        let n = count(|| {
            for len in [1usize, 37, 512, 2048, 6402] {
                let mut parts: [&mut [f32]; 6] = Default::default();
                for (k, r) in refs.iter_mut().enumerate() {
                    parts[k] = &mut r[..len];
                }
                m.process(&mut parts[..nch]);
            }
            m.set_band(0, BandParams { solo: true, ..hot });
            m.set_band(
                1,
                BandParams {
                    bypass: true,
                    makeup_db: 3.0,
                    ..hot
                },
            );
            let _ = m.take_max_gain_reduction_db();
            m.reset();
            let mut parts: [&mut [f32]; 6] = Default::default();
            for (k, r) in refs.iter_mut().enumerate() {
                parts[k] = &mut r[..];
            }
            m.process(&mut parts[..nch]);
        });
        assert_eq!(
            n, 0,
            "{n} heap allocations while processing ({sr} Hz, {nch} ch)"
        );
        drop(refs);
        assert!(bufs.iter().flatten().all(|v| v.is_finite()));
    }
}
