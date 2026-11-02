//! iw4l-bake: queues the engine pipelines a Switch frame can use, through the
//! same pipeline cache as the game. Format, blend and sample count don't reach
//! the GLSL, so one key per GLSL variant is enough. Sun effects and
//! geometry_diagnostic are off on Switch.

use super::postfx::{ExtractedBlood, ExtractedFilm};
use bevy::prelude::*;
use bevy::render::render_resource::{PipelineCache, TextureFormat};
use bevy::render::renderer::RenderDevice;
use bevy::render::{Render, RenderApp, RenderSystems};

/// A zone's films. Insert into the render world; the next frame queues them.
#[derive(Resource, Default)]
pub struct BakeFilms {
    pub films: Vec<ExtractedFilm>,
    pub blood: Option<ExtractedBlood>,
}

pub struct BakeEnginePipelinesPlugin;

impl Plugin for BakeEnginePipelinesPlugin {
    fn build(&self, app: &mut App) {
        super::floatz::register(app);
        super::iw_tess::bake_register(app);
        let grade = super::postfx::grade_shader(app);
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .insert_resource(super::script_grade::GradeShader(grade))
            .init_resource::<BakeFilms>()
            .add_systems(
                Render,
                (queue_engine, queue_films).in_set(RenderSystems::Prepare),
            );
    }
}

fn queue_engine(
    mut done: Local<bool>,
    cache: Res<PipelineCache>,
    device: Res<RenderDevice>,
    floatz: Res<super::floatz::ExactFloatZResolve>,
    tess: Res<super::iw_tess::IwTessPipeline>,
    mut shellshock: ResMut<super::shellshock_screen::TemporalScreenPipeline>,
    grade: Res<super::script_grade::GradeShader>,
) {
    if std::mem::replace(&mut *done, true) {
        return;
    }
    super::floatz::bake_queue(&floatz, &cache);
    super::iw_tess::bake_queue(&tess, &cache);
    shellshock.pipeline(TextureFormat::Rgba8Unorm, &cache);
    super::script_grade::GradeGpu::new(&device, &cache, grade.0.clone(), UVec2::splat(64));
}

fn queue_films(mut bake: ResMut<BakeFilms>, cache: Res<PipelineCache>, device: Res<RenderDevice>) {
    for film in std::mem::take(&mut bake.films) {
        if let Err(cause) = super::postfx::queue_postfx_pipeline(&film, &cache) {
            eprintln!("  film {}: {cause:?}", film.name);
        }
    }
    if let Some(blood) = bake.blood.take() {
        let port = super::hud_blood::BloodPortGpu::create(
            &blood,
            TextureFormat::Rgba8Unorm,
            &device,
            &cache,
        );
        if let Err(cause) = port {
            eprintln!("  blood: {cause:?}");
        }
    }
}
