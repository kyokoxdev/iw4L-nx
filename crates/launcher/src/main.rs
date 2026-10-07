#![cfg_attr(target_os = "horizon", feature(alloc_error_hook))]

use std::path::PathBuf;

// Link the platform layer: its symbols are referenced only by C and std.
#[cfg(target_os = "horizon")]
extern crate platform_nx;

use asset_transport::{ensure_artifacts_dir, games_root_from_env};

#[global_allocator]
static PROCESS_ALLOCATOR: diag::ProcessCountingAllocator = diag::ProcessCountingAllocator;

const LICENSES: [(&str, &str); 5] = [
    ("LICENSE", include_str!("../../../LICENSE")),
    ("NOTICE", include_str!("../../../NOTICE")),
    (
        "OFL-Oxanium.txt",
        include_str!("../../ui/assets/OFL-Oxanium.txt"),
    ),
    (
        "OFL-FiraMono.txt",
        include_str!("../../console/assets/OFL-FiraMono.txt"),
    ),
    (
        "THIRD-PARTY-LICENSES.txt",
        include_str!("../../../THIRD-PARTY-LICENSES.txt"),
    ),
];

fn main() {
    // panic=abort on Switch shows only hbmenu's generic error; keep the message.
    #[cfg(target_os = "horizon")]
    {
        let _ = std::fs::create_dir_all("sdmc:/switch/iw4l");
        // Keep the last run's trace: a crash's alloc-failure/abort lines are
        // only there, and relaunching to test another map used to erase them.
        let _ = std::fs::remove_file("sdmc:/switch/iw4l/boot.prev.txt");
        let _ = std::fs::rename(
            "sdmc:/switch/iw4l/boot.txt",
            "sdmc:/switch/iw4l/boot.prev.txt",
        );
        platform_nx::breadcrumb("main");
        platform_nx::boot_env();
        std::panic::set_hook(Box::new(|info| {
            platform_nx::breadcrumb(&format!("panic: {info}"));
            // A GL allocation failure surfaces as DeviceLost, then a panic.
            platform_nx::dump_heap_samples();
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
            platform_nx::dump_heap_samples();
            std::process::abort()
        });
    }
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "licenses")
    {
        for (name, text) in LICENSES {
            println!("==> {name} <==\n\n{text}\n");
        }
        return;
    }
    // Switch: no community descriptor or self-update; argv is taken as is.
    #[cfg(target_os = "horizon")]
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    #[cfg(not(target_os = "horizon"))]
    let mut args = match updater::startup().unwrap_or_else(|e| launch_error(&e)) {
        Some(args) => args,
        None => return,
    };
    if args.is_empty() {
        args.push("menu".into());
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
    let (mode, acceptance, cheats) = bootstrap::parse_cli(
        args.into_iter()
            .map(|arg| arg.to_string_lossy().into_owned()),
    )
    .unwrap_or_else(|e| launch_error(&e));
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
    bootstrap::launch(games, artifacts, mode, acceptance, cheats);
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
            let mut cpu = (std::time::Instant::now(), platform_nx::core_ticks());
            loop {
                // Per-core busy share: main on 0, render on 1, pools on 2.
                if cpu.0.elapsed() >= std::time::Duration::from_secs(5) {
                    let now = (std::time::Instant::now(), platform_nx::core_ticks());
                    let wall = (now.0 - cpu.0).as_secs_f64() * 19_200_000.0;
                    let pct =
                        |c: usize| now.1.0[c].saturating_sub(cpu.1.0[c]) as f64 / wall * 100.0;
                    diag::info!(
                        Launch,
                        "cpu busy: core0={:.0}% core1={:.0}% core2={:.0}% rc={:#x}",
                        pct(0),
                        pct(1),
                        pct(2),
                        now.1.1
                    );
                    cpu = now;
                }
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
