#![cfg_attr(target_os = "horizon", feature(alloc_error_hook))]

use std::path::PathBuf;

// Link the platform layer: its symbols are referenced only by C and std.
#[cfg(target_os = "horizon")]
extern crate platform_nx;

use asset_transport::{ensure_artifacts_dir, games_root_from_env};

#[global_allocator]
static PROCESS_ALLOCATOR: diag::ProcessCountingAllocator = diag::ProcessCountingAllocator;

fn main() {
    // panic=abort on Switch shows only hbmenu's generic error; keep the message.
    #[cfg(target_os = "horizon")]
    {
        let _ = std::fs::create_dir_all("sdmc:/switch/iw4l");
        let _ = std::fs::remove_file("sdmc:/switch/iw4l/boot.txt");
        platform_nx::breadcrumb("main");
        std::panic::set_hook(Box::new(|info| {
            platform_nx::breadcrumb(&format!("panic: {info}"));
            diag::exit_launch_error(&format!("panic: {info}"))
        }));
        // Allocation failure aborts without running the panic hook.
        std::alloc::set_alloc_error_hook(|layout| {
            platform_nx::breadcrumb(&format!("alloc failed: {layout:?}"));
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
