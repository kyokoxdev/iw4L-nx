//! Fullscreen render resolution: the world camera renders the chosen number of
//! rows at the window's aspect into an image that a present camera stretches
//! over the window.

use bevy::camera::{CameraOutputMode, ImageRenderTarget, RenderTarget};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::image::ImageSampler;
use bevy::picking::Pickable;
use bevy::prelude::*;
use bevy::render::render_resource::{
    BlendComponent, BlendFactor, BlendOperation, BlendState, TextureFormat,
};
use bevy::render::view::DebandDither;
use bevy::ui::UiTargetCamera;
use bevy::ui::widget::NodeImageMode;
use bevy::window::{PrimaryWindow, WindowRef};
use render_scene::FpvLens;

#[derive(Component)]
struct PresentCamera;

#[derive(Component)]
struct PresentImage;

#[derive(Resource, Default)]
struct ScaledTarget(Option<(UVec2, Handle<Image>)>);

pub(crate) fn register(app: &mut App) {
    app.init_resource::<ScaledTarget>()
        .add_systems(Update, sync_render_resolution);
}

/// World render scale, `IW4L_RENDER_SCALE` in 0.25..=1. The Switch defaults
/// to 0.625 (800x450 of 720p), the way the 360/PS3 ports drew below 720p.
fn render_scale() -> f32 {
    let default = if cfg!(target_os = "horizon") { 0.625 } else { 1.0 };
    std::env::var("IW4L_RENDER_SCALE")
        .ok()
        .and_then(|value| value.trim().parse::<f32>().ok())
        .filter(|scale| scale.is_finite())
        .unwrap_or(default)
        .clamp(0.25, 1.0)
}

fn scaled_size(settings: &frame::GameSettings, window: UVec2) -> Option<UVec2> {
    let scale = render_scale();
    if scale < 1.0 && window.y > 0 && window.x > 0 {
        let rows = ((window.y as f32 * scale).round() as u32).max(1);
        let columns = ((window.x as f32 * scale).round() as u32).max(1);
        return Some(UVec2::new(columns, rows));
    }
    let rows = settings.resolution.height;
    if !settings.fullscreen || rows == 0 || window.y == 0 || rows >= window.y {
        return None;
    }
    let columns =
        (u64::from(window.x) * u64::from(rows) + u64::from(window.y) / 2) / u64::from(window.y);
    Some(UVec2::new(u32::try_from(columns).ok()?.max(1), rows))
}

fn target_image(size: UVec2) -> Image {
    let format = if cfg!(target_os = "horizon") {
        TextureFormat::Rgba8Unorm
    } else {
        TextureFormat::Rgba8UnormSrgb
    };
    let mut image = Image::new_target_texture(size.x, size.y, format, None);
    image.data = None;
    image.copy_on_resize = false;
    image.sampler = ImageSampler::linear();
    image
}

/// The scene and HUD passes leave arbitrary alpha behind (sky, compass) that an
/// opaque window ignores. The image is blended onto the window, so its alpha is
/// kept at the cleared 1.0.
fn opaque_output() -> CameraOutputMode {
    CameraOutputMode::Write {
        blend_state: Some(BlendState {
            color: BlendComponent::REPLACE,
            alpha: BlendComponent {
                src_factor: BlendFactor::Zero,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::Add,
            },
        }),
        clear_color: ClearColorConfig::Custom(Color::BLACK),
    }
}

#[allow(clippy::too_many_arguments)]
fn sync_render_resolution(
    mut commands: Commands,
    settings: Option<Res<frame::GameSettings>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut lenses: Query<(&mut Camera, &mut RenderTarget, &mut Projection), With<FpvLens>>,
    presents: Query<Entity, With<PresentCamera>>,
    nodes: Query<Entity, With<PresentImage>>,
    mut images: ResMut<Assets<Image>>,
    mut scaled: ResMut<ScaledTarget>,
) {
    let want = match (settings, windows.single()) {
        (Some(settings), Ok(window)) => scaled_size(
            &settings,
            UVec2::new(window.physical_width(), window.physical_height()),
        )
        .map(|size| (size, window.height())),
        _ => None,
    };
    let Some((size, logical_height)) = want.filter(|_| !lenses.is_empty()) else {
        if scaled.0.take().is_some() {
            for entity in presents.iter().chain(nodes.iter()) {
                commands.entity(entity).despawn();
            }
            for (mut camera, mut target, mut projection) in &mut lenses {
                if !matches!(*target, RenderTarget::Window(WindowRef::Primary)) {
                    *target = RenderTarget::Window(WindowRef::Primary);
                    camera.output_mode = CameraOutputMode::default();
                    // The camera recomputes its target size only for changed windows,
                    // images or projections, not for a replaced target.
                    projection.set_changed();
                }
            }
        }
        return;
    };
    if scaled
        .0
        .as_ref()
        .is_none_or(|(current, _)| *current != size)
    {
        diag::info!(
            World,
            "render scale: active size={}x{} -> logical_height={:.1} (scale {:.3})",
            size.x,
            size.y,
            logical_height,
            size.y as f32 / logical_height.max(1.0),
        );
        let handle = images.add(target_image(size));
        for entity in &nodes {
            commands.entity(entity).despawn();
        }
        let camera = presents.iter().next().unwrap_or_else(|| {
            commands
                .spawn((
                    PresentCamera,
                    frame::DisplayEncodedCamera,
                    Camera2d,
                    Camera {
                        order: 1,
                        clear_color: ClearColorConfig::Custom(Color::BLACK),
                        ..default()
                    },
                    bevy::render::view::Msaa::Off,
                    Tonemapping::None,
                    DebandDither::Disabled,
                ))
                .id()
        });
        commands.spawn((
            PresentImage,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            ImageNode::new(handle.clone()).with_mode(NodeImageMode::Stretch),
            UiTargetCamera(camera),
            Pickable::IGNORE,
        ));
        scaled.0 = Some((size, handle));
    }
    let Some((_, handle)) = scaled.0.as_ref() else {
        return;
    };
    // Logical size matches the window, so HUD projections of world points line up.
    let scale_factor = size.y as f32 / logical_height.max(1.0);
    for (mut camera, mut target, mut projection) in &mut lenses {
        let current = matches!(
            &*target,
            RenderTarget::Image(image)
                if image.handle == *handle && image.scale_factor == scale_factor
        );
        if !current {
            *target = RenderTarget::Image(ImageRenderTarget {
                handle: handle.clone(),
                scale_factor,
            });
            camera.output_mode = opaque_output();
            projection.set_changed();
        }
    }
}
