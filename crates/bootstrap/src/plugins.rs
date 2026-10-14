use assets::AssetPlugin;
use audio::AudioPlugin;
use bevy::{
    log::LogPlugin,
    prelude::*,
    render::{
        RenderPlugin as BevyRenderPlugin,
        pipelined_rendering::PipelinedRenderingPlugin,
        settings::{RenderCreation, WgpuFeatures, WgpuSettings},
    },
};
use bots::BotsPlugin;
use console::ConsolePlugin;
use frame::RuntimeRole;
use hud::HudPlugin;
use net::NetPlugin;
use render::RenderPlugin;
use replay::ReplayPlugin;
use session::SessionPlugin;
use ui::UiPlugin;

pub fn add_runtime_plugins(app: &mut App) {
    add_runtime_plugins_with_role(app, RuntimeRole::Listen);
}

pub fn add_runtime_plugins_with_role(app: &mut App, role: RuntimeRole) {
    let net = match role {
        RuntimeRole::Listen => NetPlugin::listen(),
        RuntimeRole::Dedicated => NetPlugin::dedicated(),
        RuntimeRole::Client => NetPlugin::client(),
        RuntimeRole::Replay => NetPlugin::replay(),
    };
    #[cfg(not(target_os = "horizon"))]
    if crate::bench::enabled() {
        // Unfocused benchmarks must not inherit the window runner's 60 Hz sleep.
        app.insert_resource(bevy::winit::WinitSettings::continuous());
    }
    app.insert_resource(audio::AudioRuntime::new(
        role != RuntimeRole::Dedicated && !audio::AudioSilent::active(),
    ));
    app.add_plugins(AssetPlugin)
        .add_plugins(UiPlugin)
        .add_plugins(ConsolePlugin)
        .add_plugins(net)
        .add_plugins((BotsPlugin, HudPlugin))
        .add_plugins(AudioPlugin)
        .add_plugins(ReplayPlugin)
        .add_plugins(theater::TheaterPlugin)
        .add_plugins(RenderPlugin)
        .add_plugins(SessionPlugin);
    // Map loads run minutes on Switch with no input; keep the console awake.
    #[cfg(target_os = "horizon")]
    app.add_systems(Last, |screen: Option<Res<frame::AppScreen>>| {
        if let Some(screen) = screen.filter(|screen| screen.is_changed()) {
            platform_nx::stay_awake(*screen == frame::AppScreen::Loading);
        }
    });

    app.edit_schedule(Update, |schedule| {
        schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
    });

    app.edit_schedule(First, |schedule| {
        schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
    });
    app.edit_schedule(PreUpdate, |schedule| {
        schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
    });
    app.edit_schedule(PostUpdate, |schedule| {
        schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
    });
    app.edit_schedule(Last, |schedule| {
        schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
    });
    // The authority systems are one chain; the multithreaded executor only adds
    // a hand-off per system, twice a frame.
    app.edit_schedule(FixedUpdate, |schedule| {
        schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
    });

    if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
        // Measured: multithreaded render schedules compete with camera prepare's own
        // ComputeTaskPool lanes for the same workers and lose ~5% fps.
        render_app.edit_schedule(bevy::render::renderer::RenderGraph, |schedule| {
            schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
        });
        render_app.edit_schedule(bevy::core_pipeline::Core3d, |schedule| {
            schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
        });
        render_app.edit_schedule(bevy::core_pipeline::Core2d, |schedule| {
            schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
        });
        render_app.edit_schedule(bevy::render::Render, |schedule| {
            schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
        });
        render_app.edit_schedule(bevy::render::ExtractSchedule, |schedule| {
            schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
        });
        if cfg!(target_os = "macos") && pipelined_rendering() {
            // The Metal surface must be created on the main thread; pipelined, only
            // extraction runs there.
            render_app.add_systems(
                bevy::render::ExtractSchedule,
                bevy::render::view::create_surfaces
                    .run_if(bevy::render::view::need_surface_configuration)
                    .after(bevy::render::camera::extract_cameras),
            );
        }
    }
}

pub fn assemble_listen_app() -> App {
    let mut app = App::new();
    app.add_plugins(default_plugins_with_quiet_log(WindowPlugin {
        primary_window: None,
        ..default()
    }));
    add_runtime_plugins(&mut app);
    app
}

pub fn default_plugins_with_quiet_log(mut window: WindowPlugin) -> bevy::app::PluginGroupBuilder {
    if let Some(primary) = window.primary_window.as_mut() {
        primary.desired_maximum_frame_latency = core::num::NonZeroU32::new(frame_latency());
    }
    let mut wgpu = WgpuSettings::default();
    wgpu.features |= WgpuFeatures::TEXTURE_COMPRESSION_BC | WgpuFeatures::POLYGON_MODE_LINE;
    // GL (Switch, or desktop under `IW4L_BOUND_SLOTS=1`) has no norm16 or
    // binding arrays; the bound-slot render path (M3) replaces bindless there.
    let bound_slots =
        cfg!(target_os = "horizon") || std::env::var("IW4L_BOUND_SLOTS").is_ok_and(|v| v == "1");
    if !bound_slots {
        wgpu.features |= WgpuFeatures::TEXTURE_FORMAT_16BIT_NORM
            | WgpuFeatures::TEXTURE_BINDING_ARRAY
            | WgpuFeatures::SAMPLED_TEXTURE_AND_STORAGE_BUFFER_ARRAY_NON_UNIFORM_INDEXING
            | WgpuFeatures::PARTIALLY_BOUND_BINDING_ARRAY;
    } else {
        // For `unclipped_depth` on the exact colour pipelines (GL depth clamp).
        wgpu.features |= WgpuFeatures::DEPTH_CLIP_CONTROL;
    }
    let plugins = DefaultPlugins
        .set(window)
        .set(LogPlugin {
            #[cfg(all(not(feature = "span-profile"), not(target_os = "horizon")))]
            filter: "warn,iw4l=info".into(),
            // `iw4l` is only the profiler's span target (one per GSC thread
            // resume, name formatted); nothing reads them without `span-profile`.
            #[cfg(all(not(feature = "span-profile"), target_os = "horizon"))]
            filter: "warn".into(),
            #[cfg(feature = "span-profile")]
            filter: crate::span_profile::FILTER.into(),
            level: bevy::log::Level::WARN,
            #[cfg(feature = "span-profile")]
            custom_layer: crate::span_profile::layer,
            #[cfg(target_os = "horizon")]
            fmt_layer: |_| {
                use bevy::log::tracing_subscriber::Layer as _;
                // Events only: under `span-profile` it would otherwise format
                // the fields of every span opened, thousands a frame.
                Some(Box::new(
                    bevy::log::tracing_subscriber::fmt::Layer::default()
                        .with_ansi(false)
                        .without_time()
                        .with_writer(|| platform_nx::BootLog)
                        .with_filter(bevy::log::tracing_subscriber::filter::filter_fn(|meta| {
                            meta.is_event()
                        })),
                ))
            },
            ..default()
        })
        .set(BevyRenderPlugin {
            render_creation: RenderCreation::Automatic(Box::new(wgpu)),
            ..default()
        });
    #[cfg(target_os = "horizon")]
    let plugins = plugins
        .add_after::<WindowPlugin>(platform_nx::NxPlugin)
        .set(bevy::app::TaskPoolPlugin {
            task_pool_options: horizon_task_pools(),
        });
    #[cfg(target_os = "horizon")]
    let plugins = if perf::switch(SINGLE_EXECUTOR_ENV) {
        plugins.add(SingleExecutorPlugin)
    } else {
        plugins
    };
    if pipelined_rendering() {
        plugins
    } else {
        plugins.disable::<PipelinedRenderingPlugin>()
    }
}

#[cfg(target_os = "horizon")]
const SINGLE_EXECUTOR_ENV: &str = "IW4L_SINGLE_EXECUTOR";

/// `IW4L_SINGLE_EXECUTOR=1`: every schedule of every app runs on the thread
/// that runs it. With one compute thread, the multi-threaded executor's
/// per-system task handoff can cost more than the overlap it buys.
#[cfg(target_os = "horizon")]
struct SingleExecutorPlugin;

#[cfg(target_os = "horizon")]
impl Plugin for SingleExecutorPlugin {
    fn build(&self, _: &mut App) {}

    // `finish`: every schedule exists, and the render app is not yet moved
    // to its thread (pipelined rendering does that in `cleanup`).
    fn finish(&self, app: &mut App) {
        use bevy::ecs::schedule::{Schedules, SingleThreadedExecutor};
        let single = |world: &mut World| {
            if let Some(mut schedules) = world.get_resource_mut::<Schedules>() {
                for (_, schedule) in schedules.iter_mut() {
                    schedule.set_executor(SingleThreadedExecutor::new());
                }
            }
        };
        single(app.world_mut());
        for sub in app.sub_apps_mut().sub_apps.values_mut() {
            single(sub.world_mut());
        }
        platform_nx::breadcrumb("executor: single-threaded");
    }
}

/// One thread per Bevy pool, all on core 2: core 0 is the main thread's and
/// core 1 the render thread's (`platform_nx::pin_current_thread`).
#[cfg(target_os = "horizon")]
fn horizon_task_pools() -> bevy::app::TaskPoolOptions {
    use bevy::app::{TaskPoolOptions, TaskPoolThreadAssignmentPolicy};
    let pool = |name: &'static str| TaskPoolThreadAssignmentPolicy {
        min_threads: 1,
        max_threads: 1,
        percent: 1.0,
        on_thread_spawn: Some(std::sync::Arc::new(move || {
            platform_nx::pin_current_thread(2, 0b100, name);
            // Frame-critical fan-out (colour row lanes, parallel systems)
            // must not wait out a sim step sharing core 2.
            if name == "compute" {
                platform_nx::raise_current_thread(name);
            }
        })),
        on_thread_destroy: None,
    };
    TaskPoolOptions {
        min_total_threads: 3,
        max_total_threads: 3,
        io: pool("io"),
        async_compute: pool("async"),
        compute: pool("compute"),
    }
}

const PIPELINED_RENDERING_ENV: &str = "IW4L_PIPELINED_RENDERING";

/// Overlap rendering with the next main frame. Extraction remains the ownership
/// boundary; the bounded render channel permits one outstanding frame.
/// Set IW4L_PIPELINED_RENDERING=0 for synchronous presentation.
///
fn pipelined_rendering() -> bool {
    match std::env::var_os(PIPELINED_RENDERING_ENV) {
        None => !cfg!(target_os = "macos"),
        Some(_) => perf::switch(PIPELINED_RENDERING_ENV),
    }
}

const FRAME_LATENCY_ENV: &str = "IW4L_FRAME_LATENCY";

/// How many frames the surface is asked to let the CPU run ahead of the GPU.
///
/// wgpu calls this a hint and the backend is free to clamp it: on Vulkan it is
/// bound to the number of swapchain images, so a run that asked for two did
/// not necessarily get two, and only a measurement says which. One is the
/// default because it is what the runtime shipped; the variable exists so the
/// other arm needs no rebuild, and the manifest records the number that was
/// asked for — never the number the driver granted, which this process cannot
/// read back.
///
/// The value is a count, not a switch: anything unparseable or zero is the
/// default, and says so rather than silently picking an arm. Read once, so the
/// window and the manifest cannot disagree and the complaint is made once.
pub(crate) fn frame_latency() -> u32 {
    const DEFAULT: u32 = 1;
    static FRAMES: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *FRAMES.get_or_init(|| {
        let Some(asked) = std::env::var_os(FRAME_LATENCY_ENV) else {
            return DEFAULT;
        };
        match asked.to_str().map(str::trim).and_then(|v| v.parse().ok()) {
            Some(frames) if frames > 0 => frames,
            _ => {
                diag::warn!(
                    Launch,
                    "{FRAME_LATENCY_ENV}={} is not a frame count; using {DEFAULT}",
                    asked.to_string_lossy(),
                );
                DEFAULT
            }
        }
    })
}
