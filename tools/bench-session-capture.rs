use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

struct CountingAllocator;
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(size as u64, Ordering::Relaxed);
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn measure(name: &str, chunk: &[u8], capture_banner: bool) {
    let mut capture = session_capture::ChunkCapture::default();
    if capture_banner {
        let (_, uuid) = capture.feed(b"session id: 01a024d2-7cd6-7ea2-b907-531b0d261be7\n");
        assert!(uuid.is_some());
    } else {
        capture.mark_captured();
    }
    for _ in 0..1000 {
        let (text, _) = capture.feed(black_box(chunk));
        black_box(&text[..]);
    }
    const CHUNKS: u64 = 100_000;
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..CHUNKS {
        let (text, uuid) = capture.feed(black_box(chunk));
        black_box((&text[..], uuid));
    }
    let elapsed = start.elapsed();
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
    println!("{name}: chunks={CHUNKS} elapsed_ms={:.3} allocations={allocations} allocated_bytes={bytes}", elapsed.as_secs_f64() * 1000.0);
}

fn main() {
    measure("assigned/ascii-8KiB", &vec![b'x'; 8192], false);
    measure("captured/ascii-8KiB", &vec![b'x'; 8192], true);
    measure(
        "assigned/unicode-8KiB",
        "\u{2588}".repeat(2730).as_bytes(),
        false,
    );
    measure("assigned/echo", b"\x1b[32mhello\r\n", false);
    measure("assigned/split-utf8", &[0xe2, 0x96], false);
    measure_batch("isolated-echo", &[b"x".as_slice()]);
    measure_batch("bulk-four-fills", &[vec![b'x'; 8192].as_slice(); 4]);
}

fn measure_batch(name: &str, chunks: &[&[u8]]) {
    // Queue setup is excluded so the counters isolate the batcher's work.
    let (tx, rx) = std::sync::mpsc::channel();
    for chunk in chunks {
        tx.send(chunk.to_vec()).unwrap();
    }
    drop(tx);
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let mut delivered = 0;
    let mut capacity = 0;
    batch::drain_batched(rx, |bytes| {
        delivered += bytes.len();
        capacity += bytes.capacity();
        black_box(bytes);
    });
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
    println!("batch/{name}: delivered_bytes={delivered} capacity={capacity} allocations={allocations} allocated_bytes={bytes}");
}
