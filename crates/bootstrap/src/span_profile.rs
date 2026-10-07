//! Where a frame's CPU time goes, without a trace file.
//!
//! Bevy's `trace` feature opens a span around every system run, schedule run
//! and render-app update. This layer sums their wall time by name and, every
//! [`WINDOW`], logs the heaviest as milliseconds per main frame. Spans nest, so
//! a schedule's time includes its systems'; systems on different threads
//! overlap, so the column does not add up to the frame.
//!
//! A second line, `span heap:`, ranks spans by heap bytes they left live (their
//! thread's allocations minus its frees, `diag::thread_net_alloc_bytes`): a
//! system that keeps a positive row window after window is what grows the heap.
//! It ends with the spans making the most allocations per frame: on Switch each
//! one goes through newlib's locked malloc, so churn costs frame time.
//!
//! The Switch has no profiler that reaches a homebrew process and an SD card
//! too slow for a Chrome trace; this is what answers "which system" there.
//! Feature `span-profile`: the subscriber itself costs a few ms a frame.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use bevy::log::BoxedLayer;
use bevy::log::tracing::Subscriber;
use bevy::log::tracing::field::{Field, Visit};
use bevy::log::tracing::span::{Attributes, Id};
use bevy::log::tracing_subscriber::layer::{Context, Layer};
use bevy::log::tracing_subscriber::registry::LookupSpan;

const WINDOW: Duration = Duration::from_secs(5);
const TOP: usize = 160;
/// The span `SubApp::update` opens around the main app's schedule: one per frame.
const FRAME_SPAN: &str = "main app";

/// The default log filter, plus Bevy's and wgpu-core's spans: they are `info`.
pub const FILTER: &str =
    "warn,iw4l=info,bevy_ecs=info,bevy_app=info,bevy_render=info,wgpu_core=info";

pub fn layer(_: &mut bevy::app::App) -> Option<BoxedLayer> {
    Some(Box::new(SpanProfile))
}

struct SpanProfile;

/// The span's interned name, fixed at creation: `system sim::step`, `schedule Update`.
struct Name(usize);
struct Entered(Instant, i64, u64);

/// One thread's sums since the last window, indexed by interned name. Each
/// thread adds to its own under an uncontended lock; only the window close
/// takes them all. One shared table cost every span exit on every thread a
/// cross-core lock, and the window's report was built under it.
#[derive(Default)]
struct Sums {
    by_id: Vec<(Duration, u32)>,
    /// Net heap bytes each span left live this window.
    net: Vec<i64>,
    /// Allocations each span made this window.
    allocs: Vec<u64>,
}

impl Sums {
    fn add(&mut self, name: usize, time: Duration, net: i64, allocs: u64) {
        if self.by_id.len() <= name {
            self.by_id.resize(name + 1, Default::default());
            self.net.resize(name + 1, 0);
            self.allocs.resize(name + 1, 0);
        }
        self.by_id[name].0 += time;
        self.by_id[name].1 += 1;
        self.net[name] += net;
        self.allocs[name] += allocs;
    }

    fn absorb(&mut self, other: Sums) {
        for (name, (time, runs)) in other.by_id.into_iter().enumerate() {
            self.add(name, time, other.net[name], other.allocs[name]);
            self.by_id[name].1 = self.by_id[name].1 - 1 + runs;
        }
    }
}

/// Interned once per span creation, so a span exit only adds to a slot:
/// hashing and cloning the name on every exit cost more than many systems.
#[derive(Default)]
struct Totals {
    ids: HashMap<String, usize>,
    names: Vec<String>,
    threads: Vec<Arc<Mutex<Sums>>>,
    frames: u32,
    since: Option<Instant>,
}

impl Totals {
    fn intern(&mut self, name: String) -> usize {
        if let Some(&id) = self.ids.get(&name) {
            return id;
        }
        let id = self.names.len();
        if name == FRAME_SPAN {
            FRAME_ID.store(id, Ordering::Relaxed);
        }
        self.ids.insert(name.clone(), id);
        self.names.push(name);
        id
    }
}

static TOTALS: Mutex<Option<Totals>> = Mutex::new(None);
static FRAME_ID: AtomicUsize = AtomicUsize::new(usize::MAX);

thread_local! {
    static SUMS: Arc<Mutex<Sums>> = {
        let sums = Arc::<Mutex<Sums>>::default();
        if let Ok(mut totals) = TOTALS.lock() {
            totals.get_or_insert_with(Totals::default).threads.push(sums.clone());
        }
        sums
    };
    /// Interned ids of name-less spans by callsite: wgpu-core opens a fresh
    /// span per call, and formatting + interning each cost more than the call.
    static CALLSITES: RefCell<HashMap<usize, usize>> = RefCell::default();
}

/// A closed window, rendered and logged on the reporter thread: the render
/// sorts thousands of rows and the log line is a ~13 KB SD append.
struct Window {
    sums: Sums,
    frames: u32,
    wall: Duration,
}

fn reporter() -> &'static Mutex<mpsc::Sender<Window>> {
    static TX: OnceLock<Mutex<mpsc::Sender<Window>>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Window>();
        let _ = std::thread::Builder::new()
            .name("span-report".into())
            .spawn(move || {
                for window in rx {
                    let Some(names) = TOTALS
                        .lock()
                        .ok()
                        .and_then(|totals| Some(totals.as_ref()?.names.clone()))
                    else {
                        continue;
                    };
                    diag::info!(World, "{}", render(&names, &window));
                    diag::info!(World, "{}", render_heap(&names, &window));
                }
            });
        Mutex::new(tx)
    })
}

struct NameField(Option<String>);

/// `name` on system and schedule spans; `system` on the per-system
/// `RenderContextState::apply` (command-encoder finish) span.
fn is_name(field: &Field) -> bool {
    matches!(field.name(), "name" | "system")
}

impl Visit for NameField {
    fn record_str(&mut self, field: &Field, value: &str) {
        if is_name(field) {
            self.0 = Some(value.to_owned());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if is_name(field) {
            self.0 = Some(format!("{value:?}"));
        }
    }
}

fn intern(name: String) -> Option<usize> {
    let mut totals = TOTALS.lock().ok()?;
    Some(totals.get_or_insert_with(Totals::default).intern(name))
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for SpanProfile {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let metadata = attrs.metadata();
        let kind = metadata.name();
        let name = if metadata.fields().iter().any(|field| is_name(&field)) {
            let mut field = NameField(None);
            attrs.record(&mut field);
            match field.0 {
                Some(name) => intern(format!("{kind} {name}")),
                None => intern(kind.to_owned()),
            }
        } else {
            let callsite = std::ptr::from_ref(metadata) as usize;
            CALLSITES
                .try_with(|cache| {
                    if let Some(&id) = cache.borrow().get(&callsite) {
                        return Some(id);
                    }
                    let id = intern(kind.to_owned())?;
                    cache.borrow_mut().insert(callsite, id);
                    Some(id)
                })
                .ok()
                .flatten()
        };
        if let (Some(name), Some(span)) = (name, ctx.span(id)) {
            span.extensions_mut().insert(Name(name));
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let net = diag::thread_net_alloc_bytes().unwrap_or(0);
            let allocs = diag::thread_alloc_count().unwrap_or(0);
            span.extensions_mut().replace(Entered(Instant::now(), net, allocs));
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let now = Instant::now();
        let net = diag::thread_net_alloc_bytes().unwrap_or(0);
        let allocs = diag::thread_alloc_count().unwrap_or(0);
        let name = {
            let extensions = span.extensions();
            let (Some(&Name(name)), Some(Entered(at, net_at, allocs_at))) =
                (extensions.get::<Name>(), extensions.get::<Entered>())
            else {
                return;
            };
            let _ = SUMS.try_with(|sums| {
                if let Ok(mut sums) = sums.lock() {
                    sums.add(name, now - *at, net - net_at, allocs.saturating_sub(*allocs_at));
                }
            });
            name
        };
        if name == FRAME_ID.load(Ordering::Relaxed) {
            close_window(now);
        }
    }
}

/// Counts a main frame; past [`WINDOW`], gathers every thread's sums and
/// hands them to the reporter.
fn close_window(now: Instant) {
    let window = {
        let Ok(mut totals) = TOTALS.lock() else { return };
        let Some(totals) = totals.as_mut() else { return };
        totals.frames += 1;
        let since = *totals.since.get_or_insert(now);
        if now - since < WINDOW {
            return;
        }
        let mut sums = Sums::default();
        // A finished thread's sums are gathered once more, then dropped.
        totals.threads.retain(|thread| {
            if let Ok(mut thread) = thread.lock() {
                sums.absorb(std::mem::take(&mut *thread));
            }
            Arc::strong_count(thread) > 1
        });
        let window = Window { sums, frames: totals.frames, wall: now - since };
        totals.frames = 0;
        totals.since = Some(now);
        window
    };
    if let Ok(tx) = reporter().lock() {
        let _ = tx.send(window);
    }
}

fn render(names: &[String], window: &Window) -> String {
    let frames = window.frames.max(1) as f64;
    let mut rows: Vec<_> = names.iter().zip(&window.sums.by_id).collect();
    rows.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    let mut out = format!(
        "span profile: frames={} frame_ms={:.1} (ms/frame, runs/frame; nested, overlapping)",
        window.frames,
        window.wall.as_secs_f64() * 1000.0 / frames
    );
    // Systems summed by crate: the many below the cut add up too.
    let mut crates: HashMap<&str, (Duration, u32)> = HashMap::new();
    for (name, (time, _)) in &rows {
        if let Some(path) = name.strip_prefix("system ")
            && !time.is_zero()
        {
            let entry = crates
                .entry(path.split("::").next().unwrap_or(path))
                .or_default();
            entry.0 += *time;
            entry.1 += 1;
        }
    }
    let mut crates: Vec<_> = crates.into_iter().collect();
    crates.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    for (name, (time, systems)) in crates {
        out.push_str(&format!(
            " | crate {name}={:.2} systems={systems}",
            time.as_secs_f64() * 1000.0 / frames
        ));
    }
    for (name, (time, runs)) in rows.into_iter().take(TOP) {
        out.push_str(&format!(
            " | {name}={:.2} x{:.1}",
            time.as_secs_f64() * 1000.0 / frames,
            *runs as f64 / frames
        ));
    }
    out
}

/// Systems ranked by heap bytes left live this window, most first, then the
/// biggest net freers. Schedules are skipped: they repeat their systems' bytes.
fn render_heap(names: &[String], window: &Window) -> String {
    const ROWS: usize = 15;
    let mut rows: Vec<_> = names
        .iter()
        .zip(&window.sums.net)
        .filter(|(name, net)| !name.starts_with("schedule") && **net != 0)
        .collect();
    rows.sort_by_key(|(_, net)| std::cmp::Reverse(**net));
    let live = diag::process_live_heap_bytes().unwrap_or(0);
    let mut out = format!(
        "span heap: live={}MB (KB left live per window; + grew, - freed)",
        live >> 20
    );
    let kb = |net: i64| net as f64 / 1024.0;
    for (name, net) in rows.iter().take(ROWS).filter(|(_, net)| **net > 0) {
        out.push_str(&format!(" | {name}={:+.0}", kb(**net)));
    }
    for (name, net) in rows.iter().rev().take(5).filter(|(_, net)| **net < 0) {
        out.push_str(&format!(" | {name}={:+.0}", kb(**net)));
    }
    let frames = window.frames.max(1) as f64;
    let mut churn: Vec<_> = names
        .iter()
        .zip(&window.sums.allocs)
        .filter(|(name, n)| !name.starts_with("schedule") && **n > 0)
        .collect();
    churn.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    out.push_str(" || allocs/frame");
    for (name, n) in churn.into_iter().take(ROWS) {
        out.push_str(&format!(" | {name}={:.0}", *n as f64 / frames));
    }
    out
}
