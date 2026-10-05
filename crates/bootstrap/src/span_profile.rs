//! Where a frame's CPU time goes, without a trace file.
//!
//! Bevy's `trace` feature opens a span around every system run, schedule run
//! and render-app update. This layer sums their wall time by name and, every
//! [`WINDOW`], logs the heaviest as milliseconds per main frame. Spans nest, so
//! a schedule's time includes its systems'; systems on different threads
//! overlap, so the column does not add up to the frame.
//!
//! The Switch has no profiler that reaches a homebrew process and an SD card
//! too slow for a Chrome trace; this is what answers "which system" there.
//! Feature `span-profile`: the subscriber itself costs a few ms a frame.

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bevy::log::BoxedLayer;
use bevy::log::tracing::field::{Field, Visit};
use bevy::log::tracing::span::{Attributes, Id};
use bevy::log::tracing::Subscriber;
use bevy::log::tracing_subscriber::layer::{Context, Layer};
use bevy::log::tracing_subscriber::registry::LookupSpan;

const WINDOW: Duration = Duration::from_secs(5);
const TOP: usize = 60;
/// The span `SubApp::update` opens around the main app's schedule: one per frame.
const FRAME_SPAN: &str = "main app";

/// The default log filter, plus Bevy's spans: they are `info`.
pub const FILTER: &str = "warn,iw4l=info,bevy_ecs=info,bevy_app=info,bevy_render=info";

pub fn layer(_: &mut bevy::app::App) -> Option<BoxedLayer> {
    Some(Box::new(SpanProfile))
}

struct SpanProfile;

/// The span's report name, fixed at creation: `system sim::step`, `schedule Update`.
struct Name(String);
struct Entered(Instant);

#[derive(Default)]
struct Totals {
    by_name: HashMap<String, (Duration, u32)>,
    frames: u32,
    since: Option<Instant>,
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
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(Name(name));
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().replace(Entered(Instant::now()));
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let now = Instant::now();
        let report = {
            let extensions = span.extensions();
            let (Some(Name(name)), Some(Entered(at))) =
                (extensions.get::<Name>(), extensions.get::<Entered>())
            else {
                return;
            };
            let Ok(mut totals) = TOTALS.lock() else { return };
            let totals = totals.get_or_insert_with(Totals::default);
            let entry = totals.by_name.entry(name.clone()).or_default();
            entry.0 += now - *at;
            entry.1 += 1;
            if name != FRAME_SPAN {
                return;
            }
            totals.frames += 1;
            let since = *totals.since.get_or_insert(now);
            if now - since < WINDOW {
                return;
            }
            let report = render(totals, now - since);
            *totals = Totals { since: Some(now), ..Totals::default() };
            report
        };
        // Logged with the lock released: the log writer may itself be traced.
        diag::info!(World, "{report}");
    }
}

fn render(totals: &Totals, wall: Duration) -> String {
    let frames = totals.frames.max(1) as f64;
    let mut rows: Vec<_> = totals.by_name.iter().collect();
    rows.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    let mut out = format!(
        "span profile: frames={} frame_ms={:.1} (ms/frame, runs/frame; nested, overlapping)",
        totals.frames,
        wall.as_secs_f64() * 1000.0 / frames
    );
    for (name, (time, runs)) in rows.into_iter().take(TOP) {
        out.push_str(&format!(
            " | {name}={:.2} x{:.1}",
            time.as_secs_f64() * 1000.0 / frames,
            *runs as f64 / frames
        ));
    }
    out
}
