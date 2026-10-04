#![cfg_attr(target_os = "horizon", feature(alloc_error_hook))]

use std::path::PathBuf;

// Link the platform layer: its symbols are referenced only by C and std.
#[cfg(target_os = "horizon")]
extern crate platform_nx;

use asset_transport::{ensure_artifacts_dir, games_root_from_env};

#[cfg(not(target_os = "horizon"))]
#[global_allocator]
static PROCESS_ALLOCATOR: diag::ProcessCountingAllocator = diag::ProcessCountingAllocator;

#[cfg(target_os = "horizon")]
#[global_allocator]
static PROCESS_ALLOCATOR: NxAllocator = NxAllocator;

/// Counting allocator that also keeps the live allocations of 1 MB or more
/// (up to `RING`), with the code addresses on the caller's stack, in static
/// memory. A free clears its slot, so the alloc-failure hook dumps what holds
/// the heap at the failure to boot.txt: the heap is full by then, so the
/// record itself must never need it.
// ponytail: OOM hunt only; drop once the menu fits.
#[cfg(target_os = "horizon")]
struct NxAllocator;

#[cfg(target_os = "horizon")]
mod big_allocs {
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    pub const MIN: usize = 1 << 20;
    const RING: usize = 4096;
    const PCS: usize = 12;

    struct Slot {
        ptr: AtomicUsize,
        size: AtomicUsize,
        pcs: [AtomicUsize; PCS],
    }

    static SLOTS: [Slot; RING] = [const {
        Slot {
            ptr: AtomicUsize::new(0),
            size: AtomicUsize::new(0),
            pcs: [const { AtomicUsize::new(0) }; PCS],
        }
    }; RING];
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    pub fn record(ptr: *mut u8, size: usize) {
        if ptr.is_null() {
            return;
        }
        let mut pcs = [0usize; PCS];
        let n = platform_nx::code_words(&mut pcs);
        let slot = &SLOTS[NEXT.fetch_add(1, Relaxed) % RING];
        slot.ptr.store(ptr as usize, Relaxed);
        slot.size.store(size, Relaxed);
        for (to, pc) in slot.pcs.iter().zip(pcs.iter().take(n)) {
            to.store(*pc, Relaxed);
        }
    }

    // ponytail: linear scan per big free; big allocs are a few thousand per load.
    pub fn forget(ptr: *mut u8) {
        let ptr = ptr as usize;
        for slot in &SLOTS {
            if slot.ptr.load(Relaxed) == ptr {
                slot.size.store(0, Relaxed);
                slot.ptr.store(0, Relaxed);
                return;
            }
        }
    }

    /// Live allocations, oldest first: `big <bytes> <offset>...`, offsets
    /// for addr2line.
    pub fn dump() {
        use std::fmt::Write as _;
        // 4096 lines of ~150 bytes; static so a full heap cannot stop the dump.
        static mut BUF: [u8; 1 << 20] = [0; 1 << 20];
        struct Cursor(usize);
        impl std::fmt::Write for Cursor {
            fn write_str(&mut self, text: &str) -> std::fmt::Result {
                // SAFETY: only the failing thread's alloc-error hook reaches here, once.
                let buf = unsafe { &mut *core::ptr::addr_of_mut!(BUF) };
                let end = (self.0 + text.len()).min(buf.len());
                buf[self.0..end].copy_from_slice(&text.as_bytes()[..end - self.0]);
                self.0 = end;
                Ok(())
            }
        }
        let total = NEXT.load(Relaxed);
        let mut out = Cursor(0);
        let live: usize = SLOTS.iter().map(|slot| slot.size.load(Relaxed)).sum();
        let _ = writeln!(
            out,
            "big allocs: {total} >= 1MB made; live ones follow ({}MB, ring {RING})",
            live >> 20
        );
        for i in total.saturating_sub(RING)..total {
            let slot = &SLOTS[i % RING];
            if slot.size.load(Relaxed) == 0 {
                continue;
            }
            let _ = write!(out, "big {}", slot.size.load(Relaxed));
            for pc in &slot.pcs {
                let _ = write!(out, " {:x}", pc.load(Relaxed));
            }
            let _ = writeln!(out);
        }
        // SAFETY: as above.
        let text = unsafe { &(&*core::ptr::addr_of!(BUF))[..out.0] };
        platform_nx::breadcrumb(String::from_utf8_lossy(text).trim_end());
    }
}

#[cfg(target_os = "horizon")]
impl NxAllocator {
    fn note(ptr: *mut u8, size: usize) -> *mut u8 {
        if size >= big_allocs::MIN {
            big_allocs::record(ptr, size);
        }
        ptr
    }
}

#[cfg(target_os = "horizon")]
unsafe impl std::alloc::GlobalAlloc for NxAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        Self::note(unsafe { diag::ProcessCountingAllocator.alloc(layout) }, layout.size())
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        Self::note(unsafe { diag::ProcessCountingAllocator.alloc_zeroed(layout) }, layout.size())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        if layout.size() >= big_allocs::MIN {
            big_allocs::forget(ptr);
        }
        unsafe { diag::ProcessCountingAllocator.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { diag::ProcessCountingAllocator.realloc(ptr, layout, new_size) };
        if !new.is_null() && layout.size() >= big_allocs::MIN {
            big_allocs::forget(ptr);
        }
        Self::note(new, new_size)
    }
}

fn main() {
    // panic=abort on Switch shows only hbmenu's generic error; keep the message.
    #[cfg(target_os = "horizon")]
    {
        let _ = std::fs::create_dir_all("sdmc:/switch/iw4l");
        let _ = std::fs::remove_file("sdmc:/switch/iw4l/boot.txt");
        platform_nx::breadcrumb("main");
        platform_nx::boot_env();
        std::panic::set_hook(Box::new(|info| {
            platform_nx::breadcrumb(&format!("panic: {info}"));
            diag::exit_launch_error(&format!("panic: {info}"))
        }));
        // Allocation failure aborts without running the panic hook.
        // The dump's file writes need heap too: free this first. Later failing
        // threads park, so they cannot eat it before the dump is out.
        static PARACHUTE: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());
        static FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if let Ok(mut parachute) = PARACHUTE.lock() {
            *parachute = vec![1; 32 << 20];
        }
        std::alloc::set_alloc_error_hook(|layout| {
            if FAILED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                loop {
                    std::thread::park();
                }
            }
            if let Ok(mut parachute) = PARACHUTE.lock() {
                *parachute = Vec::new();
            }
            platform_nx::breadcrumb(&format!(
                "alloc failed: {layout:?} {}",
                platform_nx::heap_line()
            ));
            big_allocs::dump();
            std::process::abort()
        });
    }
    step("bench arm");
    bootstrap::bench::arm();
    step("process root");
    prepare_process_root().unwrap_or_else(|e| launch_error(&e));
    step("artifacts dir");
    let artifacts = ensure_artifacts_dir().unwrap_or_else(|e| launch_error(&e));
    step("init log");
    let log = diag::init_log(&artifacts);
    step(&format!("log at {}", log.display()));
    announce_log(log);
    step("log ready");
    let (mode, acceptance) =
        bootstrap::parse_cli(std::env::args().skip(1)).unwrap_or_else(|e| launch_error(&e));
    step(&format!("mode {mode:?}"));
    #[cfg(target_os = "horizon")]
    spawn_heap_sampler();
    let games = match games_root_from_env() {
        Ok(games) => games,
        // No game files on the SD card yet: still prove display + runner.
        #[cfg(target_os = "horizon")]
        Err(e) => {
            diag::warn!(Launch, "{e}; running display smoke test");
            return bootstrap::run_nx_smoke();
        }
        #[cfg(not(target_os = "horizon"))]
        Err(e) => launch_error(&e),
    };
    bootstrap::launch(games, artifacts, mode, acceptance);
}

/// Switch boot trace (see `platform_nx::breadcrumb`); no-op elsewhere.
fn step(_line: &str) {
    #[cfg(target_os = "horizon")]
    platform_nx::breadcrumb(_line);
}

/// Heap growth on Switch, into both boot.txt and the log so it lines up with
/// load stages. Writes only after a 64 MB move, to keep SD writes rare.
// ponytail: 250 ms polling misses short spikes; fine for finding the stage that grows.
#[cfg(target_os = "horizon")]
fn spawn_heap_sampler() {
    let start = std::time::Instant::now();
    let _ = std::thread::Builder::new()
        .name("heap-sampler".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            let mut last = 0usize;
            loop {
                let (used, ..) = platform_nx::heap_mb();
                if used.abs_diff(last) >= 64 {
                    last = used;
                    // The rest of `used` is C: Mesa, GL buffers and textures.
                    let rust = diag::process_live_heap_bytes().unwrap_or(0) >> 20;
                    let line = format!(
                        "t={}ms {} rust={rust}MB",
                        start.elapsed().as_millis(),
                        platform_nx::heap_line()
                    );
                    platform_nx::breadcrumb(&line);
                    diag::info!(Launch, "{line}");
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        });
}

/// Before `init_log`, diag has no file to report a launch error into.
fn launch_error(message: &str) -> ! {
    step(&format!("launch error: {message}"));
    diag::exit_launch_error(message)
}

fn prepare_process_root() -> Result<(), String> {
    #[cfg(windows)]
    {
        let exe =
            std::env::current_exe().map_err(|error| format!("cannot locate iw4l.exe: {error}"))?;
        let root = exe
            .parent()
            .ok_or_else(|| format!("iw4l.exe has no parent directory: {}", exe.display()))?;
        std::env::set_current_dir(root).map_err(|error| {
            format!(
                "cannot enter launcher directory {}: {error}",
                root.display()
            )
        })?;
    }
    // Switch: no exe directory to anchor on. The SD-card install dir holds
    // `.env`, `iw4l-artifacts/` and the default `games/` root.
    #[cfg(target_os = "horizon")]
    {
        let root = std::path::Path::new("sdmc:/switch/iw4l");
        std::fs::create_dir_all(root)
            .and_then(|()| std::env::set_current_dir(root))
            .map_err(|error| format!("cannot enter {}: {error}", root.display()))?;
        // Bevy's file reader otherwise anchors on current_exe(), which Horizon lacks.
        // SAFETY: no other threads exist yet.
        unsafe { std::env::set_var("BEVY_ASSET_ROOT", root) };
    }
    Ok(())
}

fn announce_log(path: PathBuf) {
    diag::announce_log_stdout(&path, diag::latest_log_path().as_deref());
    diag::info!(Launch, "log: {}", path.display());
}
