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

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Mutex;
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

/// Interned once per span creation, so a span exit only adds to a slot:
/// hashing and cloning the name on every exit cost more than many systems.
#[derive(Default)]
struct Totals {
    ids: HashMap<String, usize>,
    names: Vec<String>,
    by_id: Vec<(Duration, u32)>,
    /// Net heap bytes each span left live this window.
    net: Vec<i64>,
    /// Allocations each span made this window.
    allocs: Vec<u64>,
    frame_id: Option<usize>,
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
            self.frame_id = Some(id);
        }
        self.ids.insert(name.clone(), id);
        self.names.push(name);
        self.by_id.push(Default::default());
        self.net.push(0);
        self.allocs.push(0);
        id
    }
}

static TOTALS: Mutex<Option<Totals>> = Mutex::new(None);

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

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for SpanProfile {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut field = NameField(None);
        attrs.record(&mut field);
        let kind = attrs.metadata().name();
        let name = match field.0 {
            Some(name) => format!("{kind} {name}"),
            None => kind.to_owned(),
        };
        let Ok(mut totals) = TOTALS.lock() else {
            return;
        };
        let name = totals.get_or_insert_with(Totals::default).intern(name);
        drop(totals);
        if let Some(span) = ctx.span(id) {
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
        let (report, heap) = {
            let extensions = span.extensions();
            let (Some(&Name(name)), Some(Entered(at, net_at, allocs_at))) =
                (extensions.get::<Name>(), extensions.get::<Entered>())
            else {
                return;
            };
            let Ok(mut totals) = TOTALS.lock() else {
                return;
            };
            let Some(totals) = totals.as_mut() else {
                return;
            };
            let entry = &mut totals.by_id[name];
            entry.0 += now - *at;
            entry.1 += 1;
            totals.net[name] += net - net_at;
            totals.allocs[name] += allocs.saturating_sub(*allocs_at);
            if totals.frame_id != Some(name) {
                return;
            }
            totals.frames += 1;
            let since = *totals.since.get_or_insert(now);
            if now - since < WINDOW {
                return;
            }
            let report = (render(totals, now - since), render_heap(totals));
            totals.by_id.fill(Default::default());
            totals.net.fill(0);
            totals.allocs.fill(0);
            totals.frames = 0;
            totals.since = Some(now);
            report
        };
        // Logged with the lock released: the log writer may itself be traced.
        diag::info!(World, "{report}");
        diag::info!(World, "{heap}");
    }
}

fn render(totals: &Totals, wall: Duration) -> String {
    let frames = totals.frames.max(1) as f64;
    let mut rows: Vec<_> = totals.names.iter().zip(&totals.by_id).collect();
    rows.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    let mut out = format!(
        "span profile: frames={} frame_ms={:.1} (ms/frame, runs/frame; nested, overlapping)",
        totals.frames,
        wall.as_secs_f64() * 1000.0 / frames
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
fn render_heap(totals: &Totals) -> String {
    const ROWS: usize = 15;
    let mut rows: Vec<_> = totals
        .names
        .iter()
        .zip(&totals.net)
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
    let frames = totals.frames.max(1) as f64;
    let mut churn: Vec<_> = totals
        .names
        .iter()
        .zip(&totals.allocs)
        .filter(|(name, n)| !name.starts_with("schedule") && **n > 0)
        .collect();
    churn.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    out.push_str(" || allocs/frame");
    for (name, n) in churn.into_iter().take(ROWS) {
        out.push_str(&format!(" | {name}={:.0}", *n as f64 / frames));
    }
    out
}
