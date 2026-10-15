//! Proof that the sidechain band carver never touches the heap while
//! it processes. A separate test binary, so its counting global allocator
//! cannot collide with any allocator the library's unit tests install.

use beatbox::dsp::carve_eq::{CarveParams, Carver};
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
    let key: Vec<f32> = (0..20_000).map(|i| if (i / 3000) % 2 == 0 { noise() * 0.2 } else { 0.0 }).collect();
    for (sr, nch, band) in [(44_100.0f32, 2usize, (150.0f32, 400.0f32)), (48_000.0, 1, (2000.0, 4000.0)), (96_000.0, 6, (500.0, 900.0))] {
        let p = CarveParams { low_hz: band.0, high_hz: band.1, threshold_db: -50.0, ..Default::default() };
        let mut c = Carver::new(sr, p);
        let mut bufs: Vec<Vec<f32>> = (0..nch).map(|_| big.clone()).collect();
        let mut refs: Vec<&mut [f32]> = bufs.iter_mut().map(|b| b.as_mut_slice()).collect();
        let n = count(|| {
            let mut off = 0;
            for len in [1usize, 37, 512, 2048, 6402] {
                let mut parts: [&mut [f32]; 6] = Default::default();
                for (k, r) in refs.iter_mut().enumerate() {
                    parts[k] = &mut r[off..off + len];
                }
                c.process(&key[off..off + len], &mut parts[..nch]);
                off += len;
            }
            let _ = c.stats();
        });
        assert_eq!(n, 0, "{n} heap allocations while processing ({sr} Hz, {nch} ch)");
        assert!(c.stats().0 > 0.0, "the key never carved: the test would prove nothing");
        drop(refs);
        assert!(bufs.iter().flatten().all(|v| v.is_finite()));
    }
}
