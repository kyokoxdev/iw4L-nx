//! The Switch log file, written by its own thread.
//!
//! Every line used to be an SD append on the logging thread under the sink
//! lock. An SD write can stall for seconds; run 1791357905 lost ~7 s with the
//! main, render and sampler threads all queued behind one. Lines now gather
//! in a buffer the writer drains. Errors, `diag::flush` and process exit drain
//! it on the calling thread, so what precedes a crash or exit is on disk.

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};

struct SdLog {
    pending: Mutex<Vec<u8>>,
    ready: Condvar,
    /// Held while writing; taken before `pending` so lines land in order.
    file: Mutex<File>,
}

static LOG: OnceLock<SdLog> = OnceLock::new();

/// Set when a write fails: later lines go straight to the sink's own handle.
static BROKEN: AtomicBool = AtomicBool::new(false);

/// Takes over writes to the log at `path`. False if the writer thread could
/// not start: the caller keeps writing directly.
///
/// Its own append handle, not `File::try_clone`: libnx has no working `dup`,
/// and run 1791385559's clone took every line and left the log at 0 bytes.
pub(crate) fn start(path: &std::path::Path) -> bool {
    let Ok(file) = std::fs::OpenOptions::new().append(true).open(path) else {
        return false;
    };
    let log = LOG.get_or_init(|| SdLog {
        pending: Mutex::new(Vec::new()),
        ready: Condvar::new(),
        file: Mutex::new(file),
    });
    let started = std::thread::Builder::new()
        .name("sd-log".into())
        .spawn(move || {
            loop {
                {
                    let Ok(pending) = log.pending.lock() else {
                        return;
                    };
                    let Ok(pending) = log.ready.wait_while(pending, |p| p.is_empty()) else {
                        return;
                    };
                    drop(pending);
                }
                drain(log);
            }
        })
        .is_ok();
    if started {
        crate::exit::at_exit(flush);
    }
    started
}

/// Queues `line`; `now` also writes everything queued before returning.
/// False if the writer is not running.
pub(crate) fn push(line: &[u8], now: bool) -> bool {
    let Some(log) = LOG.get() else { return false };
    if BROKEN.load(Ordering::Relaxed) {
        return false;
    }
    if let Ok(mut pending) = log.pending.lock() {
        pending.extend_from_slice(line);
    }
    if now {
        drain(log);
    } else {
        log.ready.notify_one();
    }
    true
}

pub(crate) fn flush() {
    if let Some(log) = LOG.get() {
        drain(log);
    }
}

fn drain(log: &SdLog) {
    let Ok(mut file) = log.file.lock() else {
        return;
    };
    let bytes = match log.pending.lock() {
        Ok(mut pending) => std::mem::take(&mut *pending),
        Err(_) => return,
    };
    if bytes.is_empty() {
        return;
    }
    let at = std::time::Instant::now();
    if file.write_all(&bytes).is_err() {
        BROKEN.store(true, Ordering::Relaxed);
        return;
    }
    let took = at.elapsed();
    // Before this thread, a write this slow stalled whichever thread logged.
    if took.as_millis() >= 200 {
        let line = format!(
            "{:>8} launch  warn  sd-log: {}KB write took {}ms\n",
            crate::process_elapsed_ns() / 1_000_000,
            bytes.len() >> 10,
            took.as_millis()
        );
        let _ = file.write_all(line.as_bytes());
    }
}
