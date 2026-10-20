//! Proof that the half-band oversampler and the oversampled ADAA shaper
//! never touch the heap while they process. A separate test binary, so its
//! counting global allocator cannot collide with any allocator the
//! library's unit tests install.

use beatbox::dsp::adaa::{AdaaOrder, HardClip, OversampleHook, SoftClip5, Tanh};
use beatbox::dsp::oversample::{AdaaOversampled, HalfBandOversampler, OversampleFactor};
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
        (s as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    // setup (allocates, including the one-time filter design) outside the counted region
    let mut buf: Vec<f32> = (0..20_000).map(|_| noise()).collect();
    for f in [OversampleFactor::X2, OversampleFactor::X4] {
        let mut os = HalfBandOversampler::new(f);
        let mut a1 = AdaaOversampled::draft(HardClip, 4.0, f);
        let mut a2 = AdaaOversampled::new(Tanh, 8.0, f);
        let mut a3 = AdaaOversampled::with_order(SoftClip5, AdaaOrder::Naive, 2.0, f);
        let n = count(|| {
            for len in [1usize, 37, 512, 513, 2048, 6402, 20_000] {
                os.run(&mut buf[..len], &mut |hi: &mut [f32]| {
                    for v in hi.iter_mut() {
                        *v *= 0.5;
                    }
                });
                a1.process_block(&mut buf[..len]);
                a2.process_block(&mut buf[..len]);
                a3.process_block(&mut buf[..len]);
            }
            a2.set_drive(2.0);
            a2.set_order(AdaaOrder::First);
            a2.set_out_gain(0.5);
            a2.process_block(&mut buf[..300]);
            os.reset();
            a1.reset();
            a2.reset();
            let _ = a1.latency() + os.latency();
        });
        assert_eq!(n, 0, "{n} heap allocations while processing ({f:?})");
        assert!(buf.iter().all(|v| v.is_finite()));
    }
}
