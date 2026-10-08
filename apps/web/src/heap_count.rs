//! Test-only: heap bytes live on the threads of one runtime, and their
//! high-water mark. Only threads that ask to be counted are: the test
//! binary runs tests side by side, and only the runtime standing in for
//! apps/web's process enrols its threads. Counted across them, not per
//! thread, because production runs a multi-threaded runtime
//! (`#[tokio::main]` in `src/main.rs`), whose workers pass tasks and
//! buffers between them (#250). The counters are the process's, so a test
//! that measures holds `exclusive()` for as long as it counts: the upload
//! pool of the attachments (#250) and the barcode photo's decode (#402).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, MutexGuard};

struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    static COUNTED: Cell<bool> = const { Cell::new(false) };
}

fn add(delta: isize) {
    if COUNTED.try_with(Cell::get).unwrap_or(false) {
        let now = LIVE.fetch_add(delta, Ordering::Relaxed) + delta;
        PEAK.fetch_max(now, Ordering::Relaxed);
    }
}

fn size(n: usize) -> isize {
    isize::try_from(n).unwrap_or(isize::MAX)
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            add(size(layout.size()));
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc_zeroed(layout);
        if !ptr.is_null() {
            add(size(layout.size()));
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        add(-size(layout.size()));
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = System.realloc(ptr, layout, new_size);
        if !moved.is_null() {
            add(size(new_size) - size(layout.size()));
        }
        moved
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// From now on, what this thread allocates and frees is counted.
pub fn count_this_thread() {
    COUNTED.with(|counted| counted.set(true));
}

/// Starts a new high-water mark from what is live now, and
/// returns it.
pub fn start() -> isize {
    let live = LIVE.load(Ordering::Relaxed);
    PEAK.store(live, Ordering::Relaxed);
    live
}

pub fn peak() -> isize {
    PEAK.load(Ordering::Relaxed)
}

/// One measuring test at a time: held while its threads are counted.
pub fn exclusive() -> MutexGuard<'static, ()> {
    static MEASURING: Mutex<()> = Mutex::new(());
    MEASURING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
