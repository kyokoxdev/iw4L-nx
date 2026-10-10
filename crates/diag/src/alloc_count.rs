use std::{
    alloc::{GlobalAlloc, Layout},
    cell::Cell,
    sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
};

use serde::Serialize;

const FALLBACK_SLOT: usize = 0;
const MAX_THREADS: usize = 256;

#[repr(C, align(64))]
struct ThreadCounters {
    alloc_count: AtomicU64,
    alloc_bytes: AtomicU64,
    dealloc_count: AtomicU64,
    dealloc_bytes: AtomicU64,
}

impl ThreadCounters {
    const fn new() -> Self {
        Self {
            alloc_count: AtomicU64::new(0),
            alloc_bytes: AtomicU64::new(0),
            dealloc_count: AtomicU64::new(0),
            dealloc_bytes: AtomicU64::new(0),
        }
    }
}

static SLOTS: [ThreadCounters; MAX_THREADS] = [const { ThreadCounters::new() }; MAX_THREADS];

static NEXT_SLOT: AtomicUsize = AtomicUsize::new(1);

static SATURATED: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static SLOT: Cell<usize> = const { Cell::new(usize::MAX) };
}

fn slot() -> &'static ThreadCounters {
    let idx = SLOT
        .try_with(|cell| {
            let mut idx = cell.get();
            if idx == usize::MAX {
                idx = NEXT_SLOT.fetch_add(1, Ordering::Relaxed);
                if idx >= MAX_THREADS {
                    SATURATED.fetch_add(1, Ordering::Relaxed);
                    idx = MAX_THREADS - 1;
                    NEXT_SLOT.store(MAX_THREADS, Ordering::Relaxed);
                }
                cell.set(idx);
            }
            idx
        })
        .unwrap_or(FALLBACK_SLOT);
    &SLOTS[idx]
}

fn record_alloc(size: u64) {
    if !counting_enabled() {
        return;
    }
    let s = slot();
    s.alloc_count.fetch_add(1, Ordering::Relaxed);
    s.alloc_bytes.fetch_add(size, Ordering::Relaxed);
}

fn record_dealloc(size: u64) {
    if !counting_enabled() {
        return;
    }
    let s = slot();
    s.dealloc_count.fetch_add(1, Ordering::Relaxed);
    s.dealloc_bytes.fetch_add(size, Ordering::Relaxed);
}

/// Whether the counting allocator is actually counting. It is off unless
/// `IW4L_COUNTING_ALLOC` is set, so every figure derived from it is absent
/// rather than zero on an ordinary run — and a caller that cannot tell those
/// apart will report "no allocations" for "nobody was counting".
pub fn counting_enabled() -> bool {
    static STATE: AtomicU8 = AtomicU8::new(2);
    match STATE.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            // Horizon has no environment; the OOM hunt needs the counts there.
            // ponytail: always on for Switch; gate once the heap fits.
            let on = cfg!(target_os = "horizon") || env_key_present(b"IW4L_COUNTING_ALLOC\0");
            STATE.store(u8::from(on), Ordering::Relaxed);
            on
        }
    }
}

fn env_key_present(key_nul: &[u8]) -> bool {
    debug_assert_eq!(key_nul.last().copied(), Some(0));
    !unsafe { getenv(key_nul.as_ptr().cast()) }.is_null()
}

unsafe extern "C" {
    fn getenv(name: *const core::ffi::c_char) -> *mut core::ffi::c_char;
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    fn malloc_trim(pad: usize) -> core::ffi::c_int;
}

pub fn release_freed_heap() -> std::time::Duration {
    let at = std::time::Instant::now();
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe { malloc_trim(0) };
    }
    at.elapsed()
}

#[cfg(windows)]
static BACKING: mimalloc::MiMalloc = mimalloc::MiMalloc;
#[cfg(not(any(windows, target_os = "horizon")))]
static BACKING: std::alloc::System = std::alloc::System;
/// newlib's malloc (dlmalloc 2.6) keeps large bins sorted, so on a fragmented
/// match heap each free walked a bin: retiring a match's assets took 13–27 s
/// of frees, and every thread queued behind those walks on newlib's one lock.
/// Talc frees in O(1); it takes 16 MiB blocks from newlib and hands a block
/// back once everything in it is freed.
#[cfg(target_os = "horizon")]
static BACKING: talc::TalcLock<
    nx_mutex::NxMutex,
    talc::source::GlobalAllocSource<std::alloc::System>,
> = talc::TalcLock::new(talc::source::GlobalAllocSource::with_block_size(
    std::alloc::System,
    16 << 20,
));

/// libnx's kernel-arbitrated mutex: no allocation, and a waiter sleeps
/// instead of spinning against a holder on its own core.
#[cfg(target_os = "horizon")]
mod nx_mutex {
    use std::cell::UnsafeCell;

    unsafe extern "C" {
        fn mutexLock(m: *mut u32);
        fn mutexTryLock(m: *mut u32) -> bool;
        fn mutexUnlock(m: *mut u32);
    }

    pub struct NxMutex(UnsafeCell<u32>);

    // SAFETY: the word is only touched through libnx's atomic mutex calls.
    unsafe impl Sync for NxMutex {}

    unsafe impl talc::lock_api::RawMutex for NxMutex {
        #[allow(clippy::declare_interior_mutable_const)]
        const INIT: Self = Self(UnsafeCell::new(0));
        // libnx tags the word with the owning thread's handle.
        type GuardMarker = talc::lock_api::GuardNoSend;

        fn lock(&self) {
            unsafe { mutexLock(self.0.get()) }
        }

        fn try_lock(&self) -> bool {
            unsafe { mutexTryLock(self.0.get()) }
        }

        unsafe fn unlock(&self) {
            unsafe { mutexUnlock(self.0.get()) }
        }
    }
}

pub struct ProcessCountingAllocator;

/// Small blocks go through per-thread free lists on Switch only.
const TCACHE: bool = cfg!(target_os = "horizon");

/// Switch: blocks this large skip talc for newlib's malloc. Talc returns a
/// 16 MiB block only once all of it is free, so a match's freed textures and
/// arrays stayed inside talc while Mesa, which allocates from newlib, ran out.
const DIRECT_MIN: usize = if cfg!(target_os = "horizon") {
    1 << 20
} else {
    usize::MAX
};

fn direct(layout: Layout) -> bool {
    layout.size() >= DIRECT_MIN
}

unsafe fn raw_alloc(layout: Layout) -> *mut u8 {
    if direct(layout) {
        return unsafe { std::alloc::System.alloc(layout) };
    }
    match tcache::class(layout) {
        Some(class) if TCACHE => unsafe { tcache::alloc(class) },
        _ => unsafe { BACKING.alloc(layout) },
    }
}

unsafe fn raw_dealloc(ptr: *mut u8, layout: Layout) {
    if direct(layout) {
        return unsafe { std::alloc::System.dealloc(ptr, layout) };
    }
    match tcache::class(layout) {
        Some(class) if TCACHE => unsafe { tcache::dealloc(ptr, class) },
        _ => unsafe { BACKING.dealloc(ptr, layout) },
    }
}

unsafe impl GlobalAlloc for ProcessCountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_alloc(layout.size() as u64);
        unsafe { raw_alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_alloc(layout.size() as u64);
        if direct(layout) {
            return unsafe { std::alloc::System.alloc_zeroed(layout) };
        }
        match tcache::class(layout) {
            Some(class) if TCACHE => unsafe {
                let ptr = tcache::alloc(class);
                if !ptr.is_null() {
                    ptr.write_bytes(0, layout.size());
                }
                ptr
            },
            _ => unsafe { BACKING.alloc_zeroed(layout) },
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        record_dealloc(layout.size() as u64);
        unsafe { raw_dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record_alloc(new_size as u64);
        record_dealloc(layout.size() as u64);
        // SAFETY: `GlobalAlloc::realloc` contract: `new_size` fits `layout.align()`.
        let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        if direct(layout) && direct(new_layout) {
            return unsafe { std::alloc::System.realloc(ptr, layout, new_size) };
        }
        let cached =
            TCACHE && (tcache::class(layout).is_some() || tcache::class(new_layout).is_some());
        if !direct(layout) && !direct(new_layout) && !cached {
            return unsafe { BACKING.realloc(ptr, layout, new_size) };
        }
        // The block changes owner (class list, talc or newlib): move it.
        unsafe {
            let moved = raw_alloc(new_layout);
            if !moved.is_null() {
                moved.copy_from_nonoverlapping(ptr, layout.size().min(new_size));
                raw_dealloc(ptr, layout);
            }
            moved
        }
    }
}

/// Per-thread free lists of small blocks in front of the shared allocator, so
/// small allocs/frees from the main, render and pool threads skip its lock.
/// A cached block is a whole block of its class size, so any thread may cache it.
mod tcache {
    use std::alloc::{GlobalAlloc, Layout};
    use std::cell::Cell;
    use std::ptr::null_mut;

    use super::BACKING;

    const STEP: usize = 16;
    const MAX: usize = 512;
    const CLASSES: usize = MAX / STEP;
    // ponytail: fixed refill/cap, and an exiting thread strands its lists
    // (at most CAP blocks a class) unless it calls `flush_thread_cache` last;
    // short-lived threads do. A TLS destructor would cover every thread, but
    // registering one allocates from inside the allocator.
    const REFILL: u32 = 16;
    const CAP: u32 = 64;

    #[derive(Clone, Copy)]
    struct List {
        head: *mut u8,
        len: u32,
    }

    thread_local! {
        static LISTS: [Cell<List>; CLASSES] =
            const { [const { Cell::new(List { head: null_mut(), len: 0 }) }; CLASSES] };
    }

    /// The class serving `layout`, if it is small and at most malloc-aligned.
    pub(super) fn class(layout: Layout) -> Option<usize> {
        (layout.size() <= MAX && layout.align() <= STEP)
            .then(|| layout.size().max(1).div_ceil(STEP) - 1)
    }

    fn block(class: usize) -> Layout {
        // SAFETY: a nonzero multiple of 16, aligned 16.
        unsafe { Layout::from_size_align_unchecked((class + 1) * STEP, STEP) }
    }

    unsafe fn pop(list: &mut List) -> *mut u8 {
        let head = list.head;
        if !head.is_null() {
            list.head = unsafe { head.cast::<*mut u8>().read() };
            list.len -= 1;
        }
        head
    }

    unsafe fn push(list: &mut List, ptr: *mut u8) {
        unsafe { ptr.cast::<*mut u8>().write(list.head) };
        list.head = ptr;
        list.len += 1;
    }

    /// SAFETY: `GlobalAlloc::alloc` contract, for a layout of `class`.
    pub(super) unsafe fn alloc(class: usize) -> *mut u8 {
        let cached = LISTS.try_with(|lists| {
            let mut list = lists[class].get();
            if list.head.is_null() {
                for _ in 0..REFILL {
                    let ptr = unsafe { BACKING.alloc(block(class)) };
                    if ptr.is_null() {
                        break;
                    }
                    unsafe { push(&mut list, ptr) };
                }
            }
            let ptr = unsafe { pop(&mut list) };
            lists[class].set(list);
            ptr
        });
        match cached {
            Ok(ptr) if !ptr.is_null() => ptr,
            // No TLS (thread start/exit) or out of memory: one plain block.
            _ => unsafe { BACKING.alloc(block(class)) },
        }
    }

    /// SAFETY: `GlobalAlloc::dealloc` contract, for a layout of `class`.
    pub(super) unsafe fn dealloc(ptr: *mut u8, class: usize) {
        let cached = LISTS.try_with(|lists| {
            let mut list = lists[class].get();
            unsafe { push(&mut list, ptr) };
            if list.len > CAP {
                while list.len > CAP / 2 {
                    unsafe { BACKING.dealloc(pop(&mut list), block(class)) };
                }
            }
            lists[class].set(list);
        });
        if cached.is_err() {
            unsafe { BACKING.dealloc(ptr, block(class)) };
        }
    }

    /// Hands every block this thread holds back to the shared allocator.
    pub(super) fn flush() {
        let _ = LISTS.try_with(|lists| {
            for (class, cell) in lists.iter().enumerate() {
                let mut list = cell.get();
                while !list.head.is_null() {
                    unsafe { BACKING.dealloc(pop(&mut list), block(class)) };
                }
                cell.set(list);
            }
        });
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn classes_reuse_and_spill() {
            let at = |size, align| class(Layout::from_size_align(size, align).unwrap());
            assert_eq!(
                (at(1, 1), at(16, 8), at(17, 8), at(512, 16)),
                (Some(0), Some(0), Some(1), Some(31))
            );
            assert_eq!((at(513, 8), at(64, 32)), (None, None));

            unsafe {
                // Freed blocks come back LIFO, whole class size writable.
                let a = alloc(3);
                a.write_bytes(0xab, 64);
                dealloc(a, 3);
                assert_eq!(alloc(3), a);
                dealloc(a, 3);

                // Past CAP the list spills back to malloc down to CAP/2.
                let blocks: Vec<_> = (0..CAP * 2).map(|_| alloc(5)).collect();
                for &ptr in &blocks {
                    ptr.write_bytes(0xcd, 96);
                    dealloc(ptr, 5);
                }
                let len = LISTS.with(|lists| lists[5].get().len);
                assert!(len > CAP / 2 && len <= CAP, "len {len}");

                // A thread about to exit gives every cached block back.
                flush();
                assert!(LISTS.with(|lists| lists.iter().all(|l| l.get().len == 0)));
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ProcessAllocationStats {
    pub process_allocations: u64,
    pub process_allocation_bytes: u64,
    pub process_deallocations: u64,
    pub process_deallocation_bytes: u64,

    pub process_allocation_saturated: u64,
}

pub fn process_allocations() -> ProcessAllocationStats {
    let n = NEXT_SLOT.load(Ordering::Relaxed).clamp(1, MAX_THREADS);
    let mut stats = ProcessAllocationStats::default();
    for slot in &SLOTS[..n] {
        stats.process_allocations += slot.alloc_count.load(Ordering::Relaxed);
        stats.process_allocation_bytes += slot.alloc_bytes.load(Ordering::Relaxed);
        stats.process_deallocations += slot.dealloc_count.load(Ordering::Relaxed);
        stats.process_deallocation_bytes += slot.dealloc_bytes.load(Ordering::Relaxed);
    }
    stats.process_allocation_saturated = SATURATED.load(Ordering::Relaxed);
    stats
}

pub fn process_live_heap_bytes() -> Option<u64> {
    if !counting_enabled() {
        return None;
    }
    let stats = process_allocations();
    Some(
        stats
            .process_allocation_bytes
            .saturating_sub(stats.process_deallocation_bytes),
    )
}

/// Call last on a short-lived thread: on Switch its small-block cache would
/// otherwise stay allocated after it exits.
pub fn flush_thread_cache() {
    if TCACHE {
        tcache::flush();
    }
}

/// Bytes this thread allocated minus bytes it freed, so far. A span's delta
/// is what that span left live, less what it freed of other threads' memory.
pub fn thread_net_alloc_bytes() -> Option<i64> {
    if !counting_enabled() {
        return None;
    }
    let s = slot();
    Some(
        s.alloc_bytes.load(Ordering::Relaxed) as i64
            - s.dealloc_bytes.load(Ordering::Relaxed) as i64,
    )
}

/// Allocations this thread has made so far, for per-span churn.
pub fn thread_alloc_count() -> Option<u64> {
    counting_enabled().then(|| slot().alloc_count.load(Ordering::Relaxed))
}

pub fn process_allocation_slots_used() -> usize {
    NEXT_SLOT.load(Ordering::Relaxed).clamp(1, MAX_THREADS)
}

pub fn process_allocation_saturated() -> u64 {
    SATURATED.load(Ordering::Relaxed)
}
