//! B4: the pipelines that go through bevy's pipeline cache. A windowless app on
//! the recording device draws what the game's 2D cameras draw (image, UI node,
//! sprite) into an image target. Format and blend don't reach the GLSL, so an
//! image target gives the same keys as the window the Switch presents to. The
//! render_gpu plugin queues the engine pipelines and each zone's films.

use bevy::camera::RenderTarget;
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::render::render_resource::{CachedPipelineState, PipelineCache, PipelineDescriptor};
use bevy::render::settings::RenderCreation;
use bevy::render::view::Msaa;
use bevy::render::{RenderApp, RenderPlugin};
use bevy::shader::ShaderCacheError;
use bevy::window::ExitCondition;

use crate::record::Stages;

/// Frames before trusting an empty pipeline queue: assets load, then the
/// pipelines queue.
const MIN_FRAMES: usize = 30;
const MAX_FRAMES: usize = 600;

fn scene(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let target = images.add(Image::new_target_texture(64, 64, TextureFormat::Rgba8Unorm, None));
    let camera = commands
        .spawn((
            Camera2d,
            RenderTarget::Image(target.clone().into()),
            Msaa::Off,
            Tonemapping::None,
            DebandDither::Disabled,
        ))
        .id();
    commands.spawn((
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundColor(Color::WHITE),
        ImageNode::new(target.clone()),
        UiTargetCamera(camera),
    ));
    commands.spawn((Sprite::from_color(Color::WHITE, Vec2::splat(8.0)), Transform::default()));
}

pub fn app() -> Result<App, String> {
    let resources = crate::record::noop_resources()?;
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .build()
            .disable::<bevy::log::LogPlugin>()
            // Keeps the render world in the app: `settle` reads its pipeline cache.
            .disable::<bevy::render::pipelined_rendering::PipelinedRenderingPlugin>()
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                ..default()
            })
            .set(RenderPlugin {
                render_creation: RenderCreation::Manual(Box::new(resources)),
                synchronous_pipeline_compilation: true,
                ..default()
            }),
    );
    app.add_plugins(render_gpu::BakeEnginePipelinesPlugin);
    app.add_systems(Startup, scene);
    while app.plugins_state() == bevy::app::PluginsState::Adding {
        bevy::tasks::tick_global_task_pools_on_main_thread();
    }
    app.finish();
    app.cleanup();
    Ok(app)
}

/// Runs frames until the pipeline cache is idle. Returns the pipelines that
/// failed in bevy, before they reached the recording device.
pub fn settle(app: &mut App) -> Vec<(Option<String>, String)> {
    for frame in 0..MAX_FRAMES {
        app.update();
        let cache = app.sub_app(RenderApp).world().resource::<PipelineCache>();
        if frame >= MIN_FRAMES && !pending(cache) {
            break;
        }
    }
    let cache = app.sub_app(RenderApp).world().resource::<PipelineCache>();
    cache
        .pipelines()
        .filter_map(|pipeline| {
            let error = match &pipeline.state {
                CachedPipelineState::Ok(_) => return None,
                CachedPipelineState::Err(error) if !is_shader_pending(error) => error.to_string(),
                _ => "still pending".to_owned(),
            };
            let label = match &pipeline.descriptor {
                PipelineDescriptor::RenderPipelineDescriptor(desc) => desc.label.clone(),
                _ => None,
            };
            Some((label.map(|label| label.to_string()), error))
        })
        .collect()
}

fn pending(cache: &PipelineCache) -> bool {
    cache.pipelines().any(|pipeline| match &pipeline.state {
        CachedPipelineState::Queued | CachedPipelineState::Creating(_) => true,
        CachedPipelineState::Err(error) => is_shader_pending(error),
        CachedPipelineState::Ok(_) => false,
    })
}

fn is_shader_pending(error: &ShaderCacheError) -> bool {
    matches!(
        error,
        ShaderCacheError::ShaderNotLoaded(_) | ShaderCacheError::ShaderImportNotYetAvailable
    )
}

pub fn common_stages(app: &mut App) -> Stages {
    let failed = settle(app);
    let mut stages = Stages::default();
    stages.drain();
    stages.failed.extend(failed);
    stages
}

/// The zone's postfx and blood films, recorded through the app's cache. The
/// shaders must be in the app's `Assets<Shader>`.
pub fn queue_films(app: &mut App, films: render_gpu::BakeFilms, stages: &mut Stages) {
    app.sub_app_mut(RenderApp).insert_resource(films);
    stages.failed.extend(settle(app));
    stages.drain();
}
