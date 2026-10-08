//! Horizon platform layer. Linked by `launcher` on `target_os = "horizon"` only.
#![cfg(target_os = "horizon")]

use core::ffi::{c_char, c_int, c_long, c_void};

mod input;
pub mod swkbd;

unsafe extern "C" {
    fn randomGet(buf: *mut c_void, len: usize);
    fn socketInitialize(config: *const c_void) -> u32;
    fn nxlinkConnectToHost(redir_stdout: bool, redir_stderr: bool) -> c_int;
    static __nxlink_host: u32;
    fn svcGetInfo(out: *mut u64, id0: u32, handle: u32, id1: u64) -> u32;
    fn appletGetAppletType() -> u32;
    fn appletSetMediaPlaybackState(state: bool) -> u32;
    fn svcSetThreadCoreMask(handle: u32, preferred_core: i32, affinity_mask: u32) -> u32;
    fn svcGetCurrentProcessorNumber() -> u32;
    fn svcSetThreadPriority(handle: u32, priority: u32) -> u32;
    fn svcGetThreadPriority(priority: *mut u32, handle: u32) -> u32;
    fn envGetMainThreadHandle() -> u32;
    fn svcGetThreadCoreMask(core: *mut i32, mask: *mut u64, handle: u32) -> u32;
    fn clkrstInitialize() -> u32;
    fn clkrstExit();
    fn clkrstOpenSession(session: *mut ClkrstSession, module: u32, unk: u32) -> u32;
    fn clkrstCloseSession(session: *mut ClkrstSession);
    fn clkrstSetClockRate(session: *mut ClkrstSession, hz: u32) -> u32;
    fn __libnx_exit(code: c_int) -> !;
    static __code_start: u8;
    static __rodata_start: u8;
    fn mallinfo() -> MallInfo;
    static fake_heap_start: *const u8;
    static fake_heap_end: *const u8;
}

/// libnx `ClkrstSession` (one `Service`: handle, own_handle, object_id, u16).
#[repr(C)]
struct ClkrstSession([u32; 4]);

/// Applications run the CPU at 1020 MHz; 1785 MHz is the stock boost clock
/// (what `ApmCpuBoostMode_FastLoad` uses, minus its GPU underclock). Frames are
/// CPU-bound on both the main and render threads.
// ponytail: set once; a dock/undock or sleep resets it. Reapply on
// performance-mode change if that shows up in play.
fn boost_cpu() -> u32 {
    const PCV_MODULE_ID_CPU_BUS: u32 = 0x4000_0001;
    const CPU_HZ: u32 = 1_785_000_000;
    // SAFETY: plain FFI; the session is opened, used and closed here.
    unsafe {
        let rc = clkrstInitialize();
        if rc != 0 {
            return rc;
        }
        let mut session = ClkrstSession([0; 4]);
        let mut rc = clkrstOpenSession(&mut session, PCV_MODULE_ID_CPU_BUS, 3);
        if rc == 0 {
            rc = clkrstSetClockRate(&mut session, CPU_HZ);
            clkrstCloseSession(&mut session);
        }
        clkrstExit();
        rc
    }
}

/// libnx creates every pthread with `cpuid = -2`: the process default core,
/// affinity that core only. Without this the main thread, the render thread
/// and every pool share one core of the three. Logs the move to boot.txt.
pub fn pin_current_thread(preferred: i32, mask: u32, name: &str) {
    const CUR_THREAD_HANDLE: u32 = 0xFFFF_8000;
    // SAFETY: plain SVCs on the calling thread's pseudo-handle.
    let (before, rc) = unsafe {
        (
            svcGetCurrentProcessorNumber(),
            svcSetThreadCoreMask(CUR_THREAD_HANDLE, preferred, mask),
        )
    };
    breadcrumb(&format!(
        "thread {name}: was on core {before}, pin core {preferred} mask={mask:#05b} rc={rc:#x}"
    ));
}

/// Drops the calling thread to the lowest application priority: on its core
/// it runs only while every default-priority thread there waits. Horizon does
/// not time-slice equal priorities, so a long CPU-bound job at default
/// priority would starve its neighbours.
pub fn background_current_thread(name: &str) {
    const CUR_THREAD_HANDLE: u32 = 0xFFFF_8000;
    const LOWEST: u32 = 0x3B;
    // SAFETY: plain SVC on the calling thread's pseudo-handle.
    let rc = unsafe { svcSetThreadPriority(CUR_THREAD_HANDLE, LOWEST) };
    breadcrumb(&format!("thread {name}: priority {LOWEST:#x} rc={rc:#x}"));
}

/// Lifts the calling thread one step above its creation priority. Every
/// `threadCreate` thread starts at 0x3B (boot.txt), so on a shared core it
/// then preempts its neighbours (the authority step, logging, audio workers)
/// instead of queueing behind them: Horizon does not time-slice equal
/// priorities.
pub fn raise_current_thread(name: &str) {
    const CUR_THREAD_HANDLE: u32 = 0xFFFF_8000;
    let mut priority = 0u32;
    // SAFETY: plain SVCs on the calling thread's pseudo-handle.
    let rc = unsafe {
        match svcGetThreadPriority(&mut priority, CUR_THREAD_HANDLE) {
            0 => {
                priority = priority.saturating_sub(1);
                svcSetThreadPriority(CUR_THREAD_HANDLE, priority)
            }
            rc => rc,
        }
    };
    breadcrumb(&format!("thread {name}: priority {priority:#x} rc={rc:#x}"));
}

/// Every thread nobody pins (SDL audio, Mesa queues, audio workers, `std`
/// spawns) is created on the process default core with that core as its only
/// affinity: the main or render thread's core, where Horizon will not
/// time-slice equal priorities. Start them on core 2 with the Bevy pools
/// instead; `pin_current_thread` still moves the main, render and pool threads.
#[unsafe(no_mangle)]
unsafe extern "C" fn __wrap_threadCreate(
    t: *mut c_void,
    entry: *const c_void,
    arg: *mut c_void,
    stack_mem: *mut c_void,
    stack_sz: usize,
    prio: c_int,
    cpuid: c_int,
) -> u32 {
    const DEFAULT_CORE: c_int = -2;
    unsafe extern "C" {
        fn __real_threadCreate(
            t: *mut c_void,
            entry: *const c_void,
            arg: *mut c_void,
            stack_mem: *mut c_void,
            stack_sz: usize,
            prio: c_int,
            cpuid: c_int,
        ) -> u32;
    }
    let moved = if cpuid == DEFAULT_CORE { 2 } else { cpuid };
    // SAFETY: arguments forwarded unchanged apart from the core.
    let rc = unsafe { __real_threadCreate(t, entry, arg, stack_mem, stack_sz, prio, moved) };
    if rc == 0 {
        // SAFETY: libnx `Thread` starts with its kernel handle, set on success.
        let handle = unsafe { *(t as *const u32) };
        if let Ok(mut threads) = THREADS.lock()
            && !threads.contains(&handle)
        {
            threads.push(handle);
        }
    }
    breadcrumb(&format!(
        "threadCreate: cpuid={cpuid} -> {moved} prio={prio:#x} stack={}K rc={rc:#x}",
        stack_sz >> 10
    ));
    rc
}

/// Kernel handles of every thread `threadCreate` made, for `core_ticks`.
static THREADS: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

/// Kernel ticks (19.2 MHz) this process's threads have run, by each thread's
/// preferred core 0..3, and the first failing SVC's rc (0 if none).
// ponytail: a thread's whole run time goes to its current preferred core (the
// per-core `ThreadTickCount` subtype failed on hardware); a closed thread's
// handle drops out or gets reused. Fine for in-match windows.
pub fn core_ticks() -> ([u64; 3], u32) {
    const INFO_THREAD_TICK_COUNT: u32 = 25;
    let mut handles = THREADS.lock().map(|t| t.clone()).unwrap_or_default();
    // SAFETY: plain FFI; libnx sets the main thread handle before main.
    handles.push(unsafe { envGetMainThreadHandle() });
    let (mut out, mut err) = ([0u64; 3], 0);
    for handle in handles {
        let (mut ticks, mut core, mut mask) = (0u64, 0i32, 0u64);
        // SAFETY: plain SVCs; a stale handle returns an error, skipped.
        let rc = unsafe {
            match svcGetInfo(&mut ticks, INFO_THREAD_TICK_COUNT, handle, u64::MAX) {
                0 => svcGetThreadCoreMask(&mut core, &mut mask, handle),
                rc => rc,
            }
        };
        match out.get_mut(core as usize) {
            Some(total) if rc == 0 => *total += ticks,
            _ if err == 0 => err = rc,
            _ => {}
        }
    }
    (out, err)
}

/// newlib `struct mallinfo` (`malloc.h`), all `size_t`.
#[repr(C)]
struct MallInfo {
    arena: usize,
    _ordblks: usize,
    _smblks: usize,
    _hblks: usize,
    _hblkhd: usize,
    _usmblks: usize,
    _fsmblks: usize,
    uordblks: usize,
    _fordblks: usize,
    _keepcost: usize,
}

/// newlib heap in MB: in use, grown from the libnx heap, and that heap's size.
/// Counts Rust and C (Mesa) allocations alike; `mem_line` cannot, since libnx
/// claims the whole heap up front.
pub fn heap_mb() -> (usize, usize, usize) {
    // SAFETY: plain FFI; libnx sets the heap bounds before main.
    let (info, size) = unsafe { (mallinfo(), fake_heap_end as usize - fake_heap_start as usize) };
    (info.uordblks >> 20, info.arena >> 20, size >> 20)
}

/// Blocks screen dimming and auto-sleep while `on` (any applet type).
pub fn stay_awake(on: bool) {
    // SAFETY: plain FFI; applet service is up before main.
    let rc = unsafe { appletSetMediaPlaybackState(on) };
    if rc != 0 {
        breadcrumb(&format!("stay_awake({on}): rc={rc:#x}"));
    }
}

pub fn heap_line() -> String {
    let (used, arena, size) = heap_mb();
    format!("heap used={used}MB arena={arena}MB of {size}MB")
}

// libnx calls this before main. Only an `nxlink -s` launch needs the BSD
// driver (stdout/stderr -> host); the socket buffers come out of a tight heap,
// and nothing else on Horizon opens sockets.
#[unsafe(no_mangle)]
extern "C" fn userAppInit() {
    // SAFETY: plain FFI; __nxlink_host is set by the loader before this runs.
    unsafe {
        if __nxlink_host != 0 && socketInitialize(core::ptr::null()) == 0 {
            nxlinkConnectToHost(true, true);
        }
    }
}

static IN_EXIT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// Native abort() (Mesa, libdrm_nouveau, libstdc++) otherwise returns to hbmenu
// silently. Log the caller; resolve with
//   addr2line -f -e iw4l.elf $(printf %x $((0x<link abort> + <ret> - <abort>)))
// where <link abort> is `aarch64-none-elf-nm iw4l.elf | grep ' T abort'`.
#[unsafe(no_mangle)]
extern "C" fn abort() -> ! {
    let lr: usize;
    // SAFETY: reads the link register before any call clobbers it.
    unsafe { core::arch::asm!("mov {}, x30", out(reg) lr) };
    if !IN_EXIT.swap(true, core::sync::atomic::Ordering::Relaxed) {
        breadcrumb(&format!(
            "abort: ret={lr:#x} abort={:#x} {} {}",
            abort as usize,
            mem_line(),
            stack_trace()
        ));
    }
    unsafe { __libnx_exit(134) }
}

// Replaces libsysbase's `_exit` (same libnx exit) so every exit leaves a line.
#[unsafe(no_mangle)]
extern "C" fn _exit(code: c_int) -> ! {
    if !IN_EXIT.swap(true, core::sync::atomic::Ordering::Relaxed) {
        breadcrumb(&format!("_exit: code={code} {}", mem_line()));
    }
    unsafe { __libnx_exit(code) }
}

// Horizon's RenameFile refuses an existing target (EEXIST); every save path
// (barracks, classes, profile, account, spill) is written as POSIX
// replace-by-rename. `--wrap=rename` (launcher/build.rs) restores that.
// ponytail: unlink then rename is not atomic; a crash between them loses the
// old file. Horizon has no atomic replace.
#[unsafe(no_mangle)]
unsafe extern "C" fn __wrap_rename(from: *const c_char, to: *const c_char) -> c_int {
    const EEXIST: c_int = 17;
    unsafe extern "C" {
        fn __real_rename(from: *const c_char, to: *const c_char) -> c_int;
        fn unlink(path: *const c_char) -> c_int;
        fn __errno() -> *mut c_int;
    }
    // SAFETY: C strings straight from the caller; newlib's errno is per thread.
    unsafe {
        let rc = __real_rename(from, to);
        if rc == 0 || *__errno() != EEXIST || unlink(to) != 0 {
            return rc;
        }
        __real_rename(from, to)
    }
}

/// Code addresses in the next 2 KiB of stack, as offsets from the load base:
/// `addr2line -f -e iw4l.elf <offset>`. Release builds keep no frame pointers,
/// so this is a conservative scan, not an unwind: expect some stale entries.
// ponytail: boot-debug only, drop once boot is stable.
pub fn stack_trace() -> String {
    let mut words = [0usize; 24];
    let n = code_words(&mut words);
    let mut out = format!("base={:#x} bt=", code_base());
    for word in &words[..n] {
        out.push_str(&format!("{word:x} "));
    }
    out
}

pub fn code_base() -> usize {
    // SAFETY: linker-provided section marker; only its address is used.
    unsafe { core::ptr::addr_of!(__code_start) as usize }
}

/// `stack_trace` without allocating: fills `out` with load-base offsets of the
/// code addresses found in the next 2 KiB of stack, returns how many.
pub fn code_words(out: &mut [usize]) -> usize {
    let sp: usize;
    // SAFETY: reads the stack pointer.
    unsafe { core::arch::asm!("mov {}, sp", out(reg) sp) };
    // SAFETY: linker-provided section marker; only its address is used.
    let (lo, hi) = (code_base(), unsafe { core::ptr::addr_of!(__rodata_start) as usize });
    let mut found = 0;
    for i in 0..256 {
        if found == out.len() {
            break;
        }
        // SAFETY: the frames above the current sp are mapped stack.
        let word = unsafe { core::ptr::read_volatile((sp + i * 8) as *const usize) };
        if (lo..hi).contains(&word) && word % 4 == 0 {
            out[found] = word - lo;
            found += 1;
        }
    }
    found
}

// libnx hands out 12 pthread keys per process (`threadTlsAlloc`); std, tracing,
// SDL, Mesa and bevy want more, and std aborts with "out of TLS keys". The
// `--wrap=pthread_key_*` hooks (launcher/build.rs) serve unlimited keys from
// one real libnx key whose per-thread value is a heap table of KEYS slots.
mod tls {
    use core::{
        ffi::{c_int, c_uint, c_void},
        sync::atomic::{AtomicU8, AtomicUsize, Ordering::*},
    };

    const KEYS: usize = 512;
    type Dtor = Option<unsafe extern "C" fn(*mut c_void)>;

    unsafe extern "C" {
        fn __real_pthread_key_create(key: *mut c_uint, dtor: Dtor) -> c_int;
        fn __real_pthread_setspecific(key: c_uint, value: *const c_void) -> c_int;
        fn __real_pthread_getspecific(key: c_uint) -> *mut c_void;
        fn calloc(n: usize, size: usize) -> *mut c_void;
        fn free(ptr: *mut c_void);
    }

    // 0 free, 1 taken; DTORS[i] is the key's destructor (0 = none).
    static USED: [AtomicU8; KEYS] = [const { AtomicU8::new(0) }; KEYS];
    static DTORS: [AtomicUsize; KEYS] = [const { AtomicUsize::new(0) }; KEYS];
    // 0 = not made, 1 = being made, 2 = ready.
    static STATE: AtomicU8 = AtomicU8::new(0);
    static REAL: AtomicUsize = AtomicUsize::new(0);

    pub fn in_use() -> usize {
        USED.iter().filter(|u| u.load(Relaxed) != 0).count()
    }

    fn real_key() -> c_uint {
        if STATE.compare_exchange(0, 1, AcqRel, Acquire).is_ok() {
            let mut key = 0;
            // SAFETY: plain FFI into an out-parameter.
            let rc = unsafe { __real_pthread_key_create(&mut key, Some(thread_exit)) };
            assert_eq!(rc, 0, "no libnx TLS slot left for the key table");
            REAL.store(key as usize, Relaxed);
            STATE.store(2, Release);
        }
        while STATE.load(Acquire) != 2 {
            core::hint::spin_loop();
        }
        REAL.load(Relaxed) as c_uint
    }

    /// This thread's value table; allocated on first use when `make`.
    fn table(make: bool) -> *mut *mut c_void {
        let real = real_key();
        // SAFETY: plain FFI on our own key.
        let mut t = unsafe { __real_pthread_getspecific(real) }.cast::<*mut c_void>();
        if t.is_null() && make {
            // SAFETY: calloc'd table, owned by this thread until `thread_exit`.
            t = unsafe { calloc(KEYS, size_of::<*mut c_void>()) }.cast();
            if !t.is_null() {
                unsafe { __real_pthread_setspecific(real, t.cast()) };
            }
        }
        t
    }

    // POSIX order: up to four rounds, since a destructor may set values again.
    unsafe extern "C" fn thread_exit(table: *mut c_void) {
        let t = table.cast::<*mut c_void>();
        for _ in 0..4 {
            for i in 0..KEYS {
                // SAFETY: `t` has KEYS entries and belongs to the exiting thread.
                let value = unsafe { t.add(i).replace(core::ptr::null_mut()) };
                let dtor = DTORS[i].load(Relaxed);
                if !value.is_null() && dtor != 0 {
                    // SAFETY: stored from a `Dtor` in `create`.
                    unsafe { core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void)>(dtor)(value) };
                }
            }
        }
        // SAFETY: allocated with calloc in `table`.
        unsafe { free(table) };
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn __wrap_pthread_key_create(key: *mut c_uint, dtor: Dtor) -> c_int {
        if key.is_null() {
            return 22; // EINVAL
        }
        for i in 0..KEYS {
            if USED[i].compare_exchange(0, 1, AcqRel, Relaxed).is_ok() {
                DTORS[i].store(dtor.map_or(0, |d| d as usize), Relaxed);
                // SAFETY: caller's out-parameter.
                unsafe { *key = i as c_uint };
                return 0;
            }
        }
        11 // EAGAIN, as POSIX says for PTHREAD_KEYS_MAX
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn __wrap_pthread_key_delete(key: c_uint) -> c_int {
        match USED.get(key as usize) {
            Some(u) if u.swap(0, AcqRel) != 0 => {
                DTORS[key as usize].store(0, Relaxed);
                0
            }
            _ => 22,
        }
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn __wrap_pthread_setspecific(key: c_uint, value: *const c_void) -> c_int {
        let t = table(true);
        if t.is_null() {
            return 12; // ENOMEM
        }
        if key as usize >= KEYS {
            return 22;
        }
        // SAFETY: in range of this thread's table.
        unsafe { *t.add(key as usize) = value.cast_mut() };
        0
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn __wrap_pthread_getspecific(key: c_uint) -> *mut c_void {
        let t = table(false);
        if t.is_null() || key as usize >= KEYS {
            return core::ptr::null_mut();
        }
        // SAFETY: in range of this thread's table.
        unsafe { *t.add(key as usize) }
    }
}

// The newlib heap backs Rust, Mesa and libdrm_nouveau's GPU buffers alike, so
// the `--wrap=malloc,...` hooks (launcher/build.rs) sample it all: one
// allocation per INTERVAL bytes allocated, plus every one of INTERVAL or more,
// kept with its stack until freed. `dump_heap_samples` writes the live ones
// to boot.txt from the alloc-failure hook; `switch/heap_report.py` sums them.
// Opt-in (`--features heap-samples`): every free probes the table, which cost
// whole frames once tombstones filled it.
// ponytail: OOM hunt only; up to PROBES loads per free.
#[cfg(feature = "heap-samples")]
mod heap_samples {
    use core::{
        ffi::c_void,
        sync::atomic::{AtomicUsize, Ordering::Relaxed},
    };

    const INTERVAL: usize = 512 << 10;
    const BITS: u32 = 14;
    const PROBES: usize = 64;
    // Mesa's driver frames alone fill ten; sixteen reach the wgpu/bevy caller.
    const PCS: usize = 16;
    const EMPTY: usize = 0;
    const GONE: usize = 1;

    unsafe extern "C" {
        fn __real_malloc(size: usize) -> *mut c_void;
        fn __real_calloc(n: usize, size: usize) -> *mut c_void;
        fn __real_realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
        fn __real_memalign(align: usize, size: usize) -> *mut c_void;
        fn __real_aligned_alloc(align: usize, size: usize) -> *mut c_void;
        fn __real_free(ptr: *mut c_void);
    }

    struct Slot {
        ptr: AtomicUsize,
        weight: AtomicUsize,
        size: AtomicUsize,
        pcs: [AtomicUsize; PCS],
    }

    static SLOTS: [Slot; 1 << BITS] = [const {
        Slot {
            ptr: AtomicUsize::new(EMPTY),
            weight: AtomicUsize::new(0),
            size: AtomicUsize::new(0),
            pcs: [const { AtomicUsize::new(0) }; PCS],
        }
    }; 1 << BITS];
    static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
    static DROPPED: AtomicUsize = AtomicUsize::new(0);

    fn probe(ptr: usize) -> impl Iterator<Item = &'static Slot> {
        let start = (ptr >> 4).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (usize::BITS - BITS);
        (0..PROBES).map(move |i| &SLOTS[(start + i) & ((1 << BITS) - 1)])
    }

    fn note(ptr: *mut c_void, size: usize) -> *mut c_void {
        let before = ALLOCATED.fetch_add(size, Relaxed);
        if ptr.is_null() || before / INTERVAL == (before + size) / INTERVAL {
            return ptr;
        }
        let mut pcs = [0usize; PCS];
        let n = super::code_words(&mut pcs);
        for slot in probe(ptr as usize) {
            let won = [EMPTY, GONE].into_iter().any(|free| {
                slot.ptr.compare_exchange(free, ptr as usize, Relaxed, Relaxed).is_ok()
            });
            if won {
                slot.weight.store(size.max(INTERVAL), Relaxed);
                slot.size.store(size, Relaxed);
                for (i, to) in slot.pcs.iter().enumerate() {
                    to.store(if i < n { pcs[i] } else { 0 }, Relaxed);
                }
                return ptr;
            }
        }
        DROPPED.fetch_add(1, Relaxed);
        ptr
    }

    // Before the real free: the address cannot be handed out again meanwhile.
    fn forget(ptr: *mut c_void) {
        if ptr.is_null() {
            return;
        }
        for slot in probe(ptr as usize) {
            match slot.ptr.load(Relaxed) {
                EMPTY => return,
                p if p == ptr as usize => {
                    let _ = slot.ptr.compare_exchange(p, GONE, Relaxed, Relaxed);
                    return;
                }
                _ => {}
            }
        }
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn __wrap_malloc(size: usize) -> *mut c_void {
        note(unsafe { __real_malloc(size) }, size)
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn __wrap_calloc(n: usize, size: usize) -> *mut c_void {
        note(unsafe { __real_calloc(n, size) }, n.saturating_mul(size))
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn __wrap_realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
        forget(ptr);
        note(unsafe { __real_realloc(ptr, size) }, size)
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn __wrap_memalign(align: usize, size: usize) -> *mut c_void {
        note(unsafe { __real_memalign(align, size) }, size)
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn __wrap_aligned_alloc(align: usize, size: usize) -> *mut c_void {
        note(unsafe { __real_aligned_alloc(align, size) }, size)
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn __wrap_free(ptr: *mut c_void) {
        forget(ptr);
        unsafe { __real_free(ptr) }
    }

    /// `smp <weight> <size> <offset>...` per live sample, offsets for
    /// addr2line; weights sum to an estimate of the live heap.
    pub fn dump() {
        use std::fmt::Write as _;
        // Once: the alloc-error hook and the panic hook both call this.
        static DUMPED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
        if DUMPED.swap(true, Relaxed) {
            return;
        }
        // ~6k lines of ~160 bytes at a full 3 GB heap; static so a full heap
        // cannot stop the dump.
        static mut BUF: [u8; 2 << 20] = [0; 2 << 20];
        struct Cursor(usize);
        impl std::fmt::Write for Cursor {
            fn write_str(&mut self, text: &str) -> std::fmt::Result {
                // SAFETY: `DUMPED` lets one caller through, once.
                let buf = unsafe { &mut *core::ptr::addr_of_mut!(BUF) };
                let end = (self.0 + text.len()).min(buf.len());
                buf[self.0..end].copy_from_slice(&text.as_bytes()[..end - self.0]);
                self.0 = end;
                Ok(())
            }
        }
        let live = |slot: &Slot| slot.ptr.load(Relaxed) > GONE;
        let estimate: usize = SLOTS.iter().filter(|s| live(s)).map(|s| s.weight.load(Relaxed)).sum();
        let mut out = Cursor(0);
        let _ = writeln!(
            out,
            "heap samples: 1 per {}KB, ~{}MB live, {} dropped",
            INTERVAL >> 10,
            estimate >> 20,
            DROPPED.load(Relaxed)
        );
        for slot in SLOTS.iter().filter(|s| live(s)) {
            let _ = write!(out, "smp {} {}", slot.weight.load(Relaxed), slot.size.load(Relaxed));
            for pc in slot.pcs.iter().map(|pc| pc.load(Relaxed)).filter(|&pc| pc != 0) {
                let _ = write!(out, " {pc:x}");
            }
            let _ = writeln!(out);
        }
        // SAFETY: as above.
        let text = unsafe { &(&*core::ptr::addr_of!(BUF))[..out.0] };
        super::breadcrumb(String::from_utf8_lossy(text).trim_end());
    }
}

#[cfg(feature = "heap-samples")]
pub use heap_samples::dump as dump_heap_samples;
#[cfg(not(feature = "heap-samples"))]
pub fn dump_heap_samples() {}

/// Process memory in use / available; `svcGetInfo` on the current process.
fn mem_line() -> String {
    const CUR_PROCESS: u32 = 0xFFFF_8001;
    let (mut total, mut used) = (0u64, 0u64);
    // SAFETY: plain FFI into out-parameters.
    unsafe {
        svcGetInfo(&mut total, 6, CUR_PROCESS, 0);
        svcGetInfo(&mut used, 7, CUR_PROCESS, 0);
    }
    format!("mem={}/{}MB tls_keys={}", used >> 20, total >> 20, tls::in_use())
}

/// Launch context: applet type (0 = title takeover) and memory budget.
pub fn boot_env() {
    // SAFETY: plain FFI; applet services are up by `main`.
    let applet = unsafe { appletGetAppletType() };
    let cpu_rc = boost_cpu();
    pin_current_thread(0, 0b001, "main");
    // Which cores this process may use at all (NPDM): 0b1111 would mean core 3.
    const INFO_CORE_MASK: u32 = 0;
    const CUR_PROCESS_HANDLE: u32 = 0xFFFF_8001;
    let mut cores = 0u64;
    // SAFETY: plain SVC into a local.
    let cores_rc = unsafe { svcGetInfo(&mut cores, INFO_CORE_MASK, CUR_PROCESS_HANDLE, 0) };
    breadcrumb(&format!(
        "env: applet_type={applet} cpu_boost_rc={cpu_rc:#x} cores={cores:#06b} rc={cores_rc:#x} {} {}",
        mem_line(),
        heap_line()
    ));
}

// newlib declares sysconf but libnx does not implement it. std asks for the
// page size and the CPU count; the application gets three of the four cores.
#[unsafe(no_mangle)]
extern "C" fn sysconf(name: c_int) -> c_long {
    const SC_PAGESIZE: c_int = 8;
    const SC_NPROCESSORS_ONLN: c_int = 10;
    match name {
        SC_PAGESIZE => 0x1000,
        SC_NPROCESSORS_ONLN => 3,
        _ => -1,
    }
}

// Static EGL/Mesa: no dynamic loader. Callers (wgpu-hal) treat null as "absent".
#[unsafe(no_mangle)]
extern "C" fn dlopen(_file: *const c_char, _mode: c_int) -> *mut c_void {
    core::ptr::null_mut()
}

#[unsafe(no_mangle)]
extern "C" fn dlsym(_handle: *mut c_void, _name: *const c_char) -> *mut c_void {
    core::ptr::null_mut()
}

#[unsafe(no_mangle)]
extern "C" fn dlclose(_handle: *mut c_void) -> c_int {
    0
}

#[unsafe(no_mangle)]
extern "C" fn dlerror() -> *const c_char {
    c"dlopen unsupported on Horizon".as_ptr()
}

#[unsafe(no_mangle)]
extern "C" fn getrandom(buf: *mut c_void, len: usize, _flags: u32) -> isize {
    unsafe { randomGet(buf, len) };
    len as isize
}

// getrandom 0.4 `custom` backend (`--cfg getrandom_backend="custom"`).
#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    unsafe { randomGet(dest.cast(), len) };
    Ok(())
}

/// Plain-file boot trace at `sdmc:/switch/iw4l/boot.txt`. Works before diag's
/// log exists and from a panic hook; Switch stdio reaches nothing.
// ponytail: unbuffered open-per-line trace; drop once boot is stable.
pub fn breadcrumb(line: &str) {
    use std::io::Write as _;
    // Two threads appending at once both write at the end they saw on open, so
    // one line overwrote the other (the render thread's pin line went missing).
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _held = LOCK.lock();
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("sdmc:/switch/iw4l/boot.txt")
    {
        let _ = writeln!(file, "{line}");
        // A kernel panic resets the console before FAT flushes a closed file.
        let _ = file.sync_all();
    }
}

/// `io::Write` into `boot.txt`, for Bevy's log layer: stderr reaches nothing.
pub struct BootLog;

impl std::io::Write for BootLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        breadcrumb(String::from_utf8_lossy(buf).trim_end());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

mod window {
    use core::{ffi::c_void, ptr::NonNull};

    use bevy::{
        app::{AppExit, PluginsState},
        prelude::*,
        window::{
            PrimaryWindow, RawHandleWrapper, RawHandleWrapperHolder, WindowFocused, WindowWrapper,
        },
    };
    use raw_window_handle::{
        AndroidDisplayHandle, AndroidNdkWindowHandle, DisplayHandle, HandleError,
        HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle, WindowHandle,
    };

    unsafe extern "C" {
        fn nwindowGetDefault() -> *mut c_void;
        fn appletMainLoop() -> bool;
        fn appletGetFocusState() -> u8;
    }

    /// libnx's default `NWindow`. The wgpu fork reads an `AndroidNdk` handle
    /// on Horizon as an `NWindow*` for `eglCreateWindowSurface`.
    struct NxWindow(NonNull<c_void>);

    // SAFETY: the default NWindow is a process-lifetime static; wgpu only hands
    // the pointer to EGL.
    unsafe impl Send for NxWindow {}
    unsafe impl Sync for NxWindow {}

    impl HasWindowHandle for NxWindow {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            let raw = RawWindowHandle::AndroidNdk(AndroidNdkWindowHandle::new(self.0));
            // SAFETY: the pointer outlives `self` (see above).
            Ok(unsafe { WindowHandle::borrow_raw(raw) })
        }
    }

    impl HasDisplayHandle for NxWindow {
        fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
            let raw = RawDisplayHandle::Android(AndroidDisplayHandle::new());
            // SAFETY: an Android display handle carries no pointer.
            Ok(unsafe { DisplayHandle::borrow_raw(raw) })
        }
    }

    /// Replaces winit on Horizon: binds the primary window to the default
    /// `NWindow` and drives `App::update` from `appletMainLoop`.
    /// Must be added after `WindowPlugin` and before the render plugin, which
    /// reads the handle while it builds.
    pub struct NxPlugin;

    // ponytail: fixed cap; raise if map loads wait visibly on GPU images.
    const UPLOAD_BYTES_PER_FRAME: usize = 32 << 20;
    /// Two 20 Hz fixed ticks per frame at most.
    const MAX_FRAME_DELTA: std::time::Duration = std::time::Duration::from_millis(100);

    impl Plugin for NxPlugin {
        fn build(&self, app: &mut App) {
            crate::breadcrumb("NxPlugin build");
            let window = NonNull::new(unsafe { nwindowGetDefault() }).expect("libnx default NWindow");
            let handle = RawHandleWrapper::new(&WindowWrapper::new(NxWindow(window)))
                .expect("NWindow raw handle");
            let world = app.world_mut();
            let (entity, holder) = world
                .query_filtered::<(Entity, &RawHandleWrapperHolder), With<PrimaryWindow>>()
                .single(world)
                .map(|(entity, holder)| (entity, holder.clone()))
                .expect("NxPlugin needs WindowPlugin's primary window");
            *holder.0.lock().expect("window handle lock") = Some(handle.clone());
            world.entity_mut(entity).insert(handle);
            // Each uploaded byte sits in a wgpu staging buffer and a Mesa
            // transfer copy until the frame's fence passes, all on the same
            // heap: an uncapped map load (2550 images, 361 MB) or spawn burst
            // held ~300 MB of staging at the OOM after class select.
            app.insert_resource(bevy::render::render_asset::RenderAssetBytesPerFrame::new(
                UPLOAD_BYTES_PER_FRAME,
            ));
            // A 20 Hz sim tick costs ~30 ms here: with Bevy's 250 ms default a
            // slow frame queued up to 5 ticks, which made the next frame slower
            // still (in-game frames climbed 140 -> 250 ms at 2.9 -> 4.8 ticks).
            // ponytail: game time runs slow below 10 fps; drop once a tick is cheap.
            if let Some(mut time) = app.world_mut().get_resource_mut::<Time<Virtual>>() {
                time.set_max_delta(MAX_FRAME_DELTA);
            }
            app.add_plugins(crate::input::InputPlugin).set_runner(run);
            crate::breadcrumb("NxPlugin ready");
        }

        fn finish(&self, app: &mut App) {
            // Pipelined rendering runs the render app on its own thread: give it core 1.
            if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
                render_app.add_systems(bevy::render::Render, pin_render_thread);
            }
        }
    }

    /// Exclusive so it runs on the thread driving the `Render` schedule: as a
    /// plain system the executor ran it on a compute-pool thread, which pinned
    /// that thread instead and left the render thread on the main thread's core.
    fn pin_render_thread(_: &mut World, mut pinned: Local<bool>) {
        if !std::mem::replace(&mut *pinned, true) {
            crate::pin_current_thread(1, 0b010, "render");
        }
    }

    /// Mirrors winit's focus handling: the game reads `Window::focused`.
    fn set_focus(app: &mut App, focused: bool) {
        let world = app.world_mut();
        let Ok(entity) = world
            .query_filtered::<Entity, With<PrimaryWindow>>()
            .single(world)
        else {
            return;
        };
        if let Some(mut window) = world.get_mut::<Window>(entity) {
            window.focused = focused;
        }
        world.write_message(WindowFocused { window: entity, focused });
    }

    /// What the GL backend offers on switch-mesa; sizes the bound-slot render path.
    // ponytail: boot-debug only, drop once the render path is settled.
    fn gpu_caps(app: &mut App) {
        use bevy::render::renderer::RenderAdapter;
        let Some(adapter) = app.world().get_resource::<RenderAdapter>() else {
            crate::breadcrumb("gpu: no RenderAdapter");
            return;
        };
        let (info, l) = (adapter.get_info(), adapter.limits());
        crate::breadcrumb(&format!(
            "gpu: {} / {} / {:?}\ngpu features: {:?}\ngpu downlevel: {:?}\n\
             gpu limits: sampled_tex/stage={} samplers/stage={} storage_buf/stage={} \
             storage_buf_size={} uniform_buf/stage={} uniform_buf_size={} bind_groups={} \
             tex2d={} immediates={} vertex_attrs={}",
            info.name,
            info.driver_info,
            info.backend,
            adapter.features(),
            adapter.get_downlevel_capabilities().flags,
            l.max_sampled_textures_per_shader_stage,
            l.max_samplers_per_shader_stage,
            l.max_storage_buffers_per_shader_stage,
            l.max_storage_buffer_binding_size,
            l.max_uniform_buffers_per_shader_stage,
            l.max_uniform_buffer_binding_size,
            l.max_bind_groups,
            l.max_texture_dimension_2d,
            l.max_immediate_size,
            l.max_vertex_attributes,
        ));
    }

    fn run(mut app: App) -> AppExit {
        if app.plugins_state() != PluginsState::Cleaned {
            while app.plugins_state() == PluginsState::Adding {
                bevy::tasks::tick_global_task_pools_on_main_thread();
            }
            app.finish();
            app.cleanup();
        }
        crate::breadcrumb("runner: plugins finished");
        gpu_caps(&mut app);
        let mut frames = 0u64;
        let mut focused = true;
        // appletMainLoop turns false when the user closes the app from HOME.
        while unsafe { appletMainLoop() } {
            // AppletFocusState_InFocus = 1; HOME menu, sleep mode and system
            // applets (software keyboard) all take the focus away.
            let now_focused = unsafe { appletGetFocusState() } == 1;
            if now_focused != focused {
                focused = now_focused;
                set_focus(&mut app, focused);
                crate::breadcrumb(&format!("runner: focus {focused}"));
            } else if !focused {
                // Nothing to draw to while backgrounded; idle until HOME returns.
                std::thread::sleep(std::time::Duration::from_millis(16));
                continue;
            }
            app.update();
            frames += 1;
            if frames == 1 {
                crate::breadcrumb("runner: first update done");
            }
            if let Some(exit) = app.should_exit() {
                return exit;
            }
        }
        AppExit::Success
    }
}

pub use window::NxPlugin;
