use bevy::prelude::*;

pub struct RenderGpuPlugin;

impl Plugin for RenderGpuPlugin {
    fn build(&self, app: &mut App) {
        match crate::SamplerTable::captured_2026_08_11() {
            Ok(table) => {
                let adapted = table.host_adapted_rows();
                if !adapted.is_empty() {
                    diag::info!(
                        World,
                        "drawsurf sampler host adaptation: {} of 24 table rows lift mip filter to linear for anisotropy (rows={adapted:?})",
                        adapted.len()
                    );
                }
                app.insert_resource(table);
            }
            Err(cause) => diag::warn!(World, "drawsurf sampler profile: cause={cause:?}"),
        }
        crate::drawsurf::register_drawsurf_render(app);
        app.init_resource::<crate::GpuSubmitReady>()
            .init_resource::<crate::ColourWorkingSet>();
        let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) else {
            return;
        };
        render_app
            .init_resource::<crate::GpuSubmitReady>()
            .init_resource::<crate::ColourWorkingSet>()
            .init_resource::<crate::WorldPipelineWarmup>();
    }

    fn finish(&self, app: &mut App) {
        #[cfg(target_os = "horizon")]
        if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
            use bevy::ecs::schedule::ScheduleCleanupPolicy;
            use bevy::render::{Render, RenderSystems};
            if render_app
                .remove_systems_in_set(
                    Render,
                    bevy::render::renderer::render_system,
                    ScheduleCleanupPolicy::RemoveSystemsOnlyAllowBreakages,
                )
                .is_ok()
            {
                render_app
                    .init_resource::<CapturePending>()
                    .add_systems(bevy::render::ExtractSchedule, extract_capture_pending)
                    .add_systems(
                        Render,
                        (
                            bevy::render::renderer::render_system
                                .run_if(|pending: Res<CapturePending>| pending.0),
                            render_and_present.run_if(|pending: Res<CapturePending>| !pending.0),
                        )
                            // Bevy chains these after the (private) pipeline
                            // queue system; unordered, a pipeline finished this
                            // frame is at worst used one frame later.
                            .in_set(RenderSystems::Render),
                    );
            }
        }
        #[cfg(not(target_os = "horizon"))]
        let _ = app;
    }
}

/// A screenshot or GPU readback is in flight: bevy's `render_system` must run
/// to encode its copy.
#[cfg(target_os = "horizon")]
#[derive(Resource, Default)]
struct CapturePending(bool);

#[cfg(target_os = "horizon")]
fn extract_capture_pending(
    mut pending: ResMut<CapturePending>,
    captures: bevy::render::Extract<
        Query<
            (),
            Or<(
                With<bevy::render::view::screenshot::Screenshot>,
                With<bevy::render::gpu_readback::Readback>,
            )>,
        >,
    >,
) {
    pending.0 = !captures.is_empty();
}

/// Bevy's `render_system` without its screenshot/readback encoder: Switch
/// rarely uses either, and that empty submit cost a full `Queue::submit`
/// (maintain/triage/cleanup) every frame.
#[cfg(target_os = "horizon")]
fn render_and_present(
    world: &mut World,
    state: &mut bevy::ecs::system::SystemState<
        Query<(
            &bevy::render::view::ViewTarget,
            &bevy::render::camera::ExtractedCamera,
        )>,
    >,
) {
    use bevy::camera::NormalizedRenderTarget;
    use bevy::render::view::ExtractedWindows;
    world.run_schedule(bevy::render::renderer::RenderGraph);
    world.resource_scope(|world, mut windows: Mut<ExtractedWindows>| {
        let views = state.get(world).unwrap();
        for window in windows.values_mut() {
            let view_needs_present = views.iter().any(|(view_target, camera)| {
                matches!(
                    camera.target,
                    Some(NormalizedRenderTarget::Window(w)) if w.entity() == window.entity
                ) && view_target.needs_present()
            });
            if view_needs_present || window.needs_initial_present {
                window.present();
                window.needs_initial_present = false;
            }
        }
    });
}
