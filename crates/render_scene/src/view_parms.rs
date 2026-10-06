use bevy::prelude::*;

use crate::frustum::{FrustumSpec, perspective_frustum_planes};

#[must_use]
pub fn pack_live_view_parms(
    view_from_world: Mat4,
    clip_from_view: Mat4,
    clip_from_world: Mat4,
    view_origin: Vec3,
) -> [u32; hud_iw4::GFX_VIEWPARMS_DWORDS] {
    use hud_iw4::{
        GFX_VIEWPARMS_INV_VP, GFX_VIEWPARMS_PROJECTION, GFX_VIEWPARMS_VIEW,
        GFX_VIEWPARMS_VIEW_PROJECTION, gfx_viewparms_write_matrix, gfx_viewparms_write_origin,
    };

    let mut parms = [0u32; hud_iw4::GFX_VIEWPARMS_DWORDS];
    let _ = gfx_viewparms_write_matrix(
        &mut parms,
        GFX_VIEWPARMS_VIEW,
        &view_from_world.to_cols_array(),
    );
    let _ = gfx_viewparms_write_matrix(
        &mut parms,
        GFX_VIEWPARMS_PROJECTION,
        &clip_from_view.to_cols_array(),
    );
    let _ = gfx_viewparms_write_matrix(
        &mut parms,
        GFX_VIEWPARMS_VIEW_PROJECTION,
        &clip_from_world.to_cols_array(),
    );
    let det = clip_from_world.determinant();
    if det != 0.0 && det.is_finite() {
        let inv = clip_from_world.inverse();
        let _ = gfx_viewparms_write_matrix(&mut parms, GFX_VIEWPARMS_INV_VP, &inv.to_cols_array());
    }
    gfx_viewparms_write_origin(&mut parms, view_origin.to_array(), 1.0);
    parms
}

#[must_use]
pub fn host_clip_from_view(fov: f32, aspect: f32, near: f32) -> Mat4 {
    Mat4::perspective_infinite_reverse_rh(fov, aspect, near)
}

#[derive(Resource, Clone, Debug)]
pub struct PreparedSceneView {
    pub ready: bool,
    pub eye: Vec3,

    pub forward: Vec3,
    pub view_from_world: Mat4,
    pub clip_from_view: Mat4,
    pub clip_from_world: Mat4,
    pub frustum_planes: Vec<[f32; 4]>,
    pub fov: f32,
    pub aspect: f32,
    pub near: f32,
    pub far: f32,
    pub rt_w: i32,
    pub rt_h: i32,
    pub parms: [u32; hud_iw4::GFX_VIEWPARMS_DWORDS],

    pub portal_bevels: Option<dpvs_iw4::PortalBevels>,

    pub depth_hack_near: f32,

    pub scene_viewport: hud_iw4::GfxViewport,
}

impl PreparedSceneView {
    #[must_use]
    pub fn tan_half_fov_y(&self) -> f32 {
        (self.fov * 0.5).tan()
    }
}

#[derive(Resource, Clone, Copy, Debug)]
pub struct RZnearDvar {
    pub value: f32,
}

impl Default for RZnearDvar {
    fn default() -> Self {
        Self {
            value: hud_iw4::R_ZNEAR_DEFAULT,
        }
    }
}

#[derive(Resource, Clone, Copy, Debug)]
pub struct RZnearDepthhackDvar {
    pub value: f32,
}

impl Default for RZnearDepthhackDvar {
    fn default() -> Self {
        Self {
            value: hud_iw4::R_ZNEAR_DEPTHHACK_DEFAULT,
        }
    }
}

#[derive(Resource, Clone, Copy, Debug)]
pub struct RSubwindowDvar {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
}

impl Default for RSubwindowDvar {
    fn default() -> Self {
        let [left, right, top, bottom] = hud_iw4::R_SUBWINDOW_DEFAULT;
        Self {
            left,
            right,
            top,
            bottom,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LockPvsView {
    pub eye: Vec3,
    pub forward: Vec3,
    pub frustum_planes: Vec<[f32; 4]>,
    pub portal_bevels: Option<dpvs_iw4::PortalBevels>,
    pub fov: f32,
    pub aspect: f32,
}

#[derive(Resource, Clone, Debug, Default)]
pub struct RLockPvs {
    pub enabled: bool,

    pub recapture: bool,
    pub frozen: Option<LockPvsView>,
    /// Switch: the cull view held across frames, see [`RLockPvs::hold`].
    pub held: Option<HeldCullView>,
}

/// A cull view wide enough to stay conservative while the live view turns by
/// up to [`HOLD_TURN_RAD`] and moves by up to [`HOLD_EYE_UNITS`].
#[derive(Clone, Debug)]
pub struct HeldCullView {
    pub view: LockPvsView,
    live_forward: Vec3,
    live_fov: f32,
    live_aspect: f32,
}

/// Rotation the held view absorbs: its FOV is this much wider on every side.
pub const HOLD_TURN_RAD: f32 = 8.0 * std::f32::consts::PI / 180.0;
/// Eye travel the held view absorbs: every frustum plane sits this far out.
/// Portal clipping still uses the held eye, so a doorway can reveal far cells
/// up to this many units late. 48 holds about three frames of a run at the
/// Switch's ~11 fps; at 16 the draw lists were rebuilt nearly every frame.
pub const HOLD_EYE_UNITS: f32 = 48.0;

#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct SmEnableDvar {
    pub enabled: Option<bool>,
}

#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct SmSunEnableDvar {
    pub enabled: Option<bool>,
}

impl RLockPvs {
    pub fn apply_after_stamp(&mut self, live: &PreparedSceneView) {
        if !self.enabled {
            self.frozen = None;
            self.recapture = false;
            return;
        }
        if self.recapture || self.frozen.is_none() {
            self.frozen = Some(LockPvsView {
                eye: live.eye,
                forward: live.forward,
                frustum_planes: live.frustum_planes.clone(),
                portal_bevels: live.portal_bevels,
                fov: live.fov,
                aspect: live.aspect,
            });
            self.recapture = false;
        }
    }

    /// Keep culling from the held view while it still covers `live`, so the
    /// visible set, and every draw list keyed on it, stays put across frames
    /// instead of being rebuilt on each turn of the camera. `widen` builds the
    /// replacement from `live` when it no longer does.
    pub fn hold(
        &mut self,
        live: &PreparedSceneView,
        widen: impl FnOnce(&PreparedSceneView) -> LockPvsView,
    ) {
        let covered = self.held.as_ref().is_some_and(|held| {
            held.live_fov == live.fov
                && held.live_aspect == live.aspect
                && held.view.eye.distance(live.eye) <= HOLD_EYE_UNITS
                && held.live_forward.dot(live.forward) >= HOLD_TURN_RAD.cos()
        });
        if !covered {
            // `span-profile`: runs/frame of this span is the held-view refresh rate.
            let _span = bevy::log::info_span!(target: "iw4l", "cull", name = "held_refresh").entered();
            self.held = Some(HeldCullView {
                view: widen(live),
                live_forward: live.forward,
                live_fov: live.fov,
                live_aspect: live.aspect,
            });
        }
    }

    fn cull_view(&self) -> Option<&LockPvsView> {
        match (self.enabled, &self.frozen) {
            (true, Some(f)) => Some(f),
            _ => self.held.as_ref().map(|held| &held.view),
        }
    }

    #[must_use]
    pub fn dpvs_eye(&self, live: &PreparedSceneView) -> Vec3 {
        self.cull_view().map_or(live.eye, |f| f.eye)
    }

    #[must_use]
    pub fn dpvs_forward(&self, live: &PreparedSceneView) -> Vec3 {
        self.cull_view().map_or(live.forward, |f| f.forward)
    }

    #[must_use]
    pub fn dpvs_frustum<'a>(&'a self, live: &'a PreparedSceneView) -> &'a [[f32; 4]] {
        self.cull_view()
            .map_or(live.frustum_planes.as_slice(), |f| {
                f.frustum_planes.as_slice()
            })
    }

    #[must_use]
    pub fn dpvs_portal_bevels(&self, live: &PreparedSceneView) -> Option<dpvs_iw4::PortalBevels> {
        self.cull_view()
            .map_or(live.portal_bevels, |f| f.portal_bevels)
    }

    #[must_use]
    pub fn dpvs_fov_aspect(&self, live: &PreparedSceneView) -> (f32, f32) {
        self.cull_view()
            .map_or((live.fov, live.aspect), |f| (f.fov, f.aspect))
    }
}

/// `view`'s cull view widened by [`HOLD_TURN_RAD`] on every side and pushed
/// out by [`HOLD_EYE_UNITS`]; `prepare` rebuilds a view at another FOV.
#[must_use]
pub fn widened_cull_view(
    view: &PreparedSceneView,
    prepare: impl FnOnce(f32, f32) -> PreparedSceneView,
) -> LockPvsView {
    let tan_y = (view.fov * 0.5).tan();
    let tan_x = tan_y * view.aspect;
    let wide = |tan: f32| (tan.atan() + HOLD_TURN_RAD).min(1.5).tan();
    let (wide_y, wide_x) = (wide(tan_y), wide(tan_x));
    let wide_view = prepare(2.0 * wide_y.atan(), wide_x / wide_y);
    let mut frustum_planes = wide_view.frustum_planes;
    for plane in &mut frustum_planes {
        plane[3] += HOLD_EYE_UNITS;
    }
    LockPvsView {
        eye: view.eye,
        forward: view.forward,
        frustum_planes,
        portal_bevels: wide_view.portal_bevels,
        fov: wide_view.fov,
        aspect: wide_view.aspect,
    }
}

impl Default for PreparedSceneView {
    fn default() -> Self {
        Self {
            ready: false,
            eye: Vec3::ZERO,
            forward: Vec3::NEG_Z,
            view_from_world: Mat4::IDENTITY,
            clip_from_view: Mat4::IDENTITY,
            clip_from_world: Mat4::IDENTITY,
            frustum_planes: Vec::new(),
            fov: 0.0,
            aspect: 0.0,
            near: 0.0,
            far: 0.0,
            rt_w: 0,
            rt_h: 0,
            parms: [0u32; hud_iw4::GFX_VIEWPARMS_DWORDS],
            portal_bevels: None,
            depth_hack_near: hud_iw4::R_ZNEAR_DEPTHHACK_DEFAULT,
            scene_viewport: hud_iw4::GfxViewport::default(),
        }
    }
}

#[must_use]
pub fn lens_world_from_parent_child(parent: Transform, child: Transform) -> Mat4 {
    parent.to_matrix() * child.to_matrix()
}

#[must_use]
pub fn prepare_scene_view(
    world_from_local: Mat4,
    fov: f32,
    aspect: f32,
    near: f32,
    far: f32,
    rt_w: i32,
    rt_h: i32,
    dpvs_z_near: f32,
    depth_hack_near: f32,
    scene_viewport: hud_iw4::GfxViewport,
) -> PreparedSceneView {
    let eye = world_from_local.transform_point3(Vec3::ZERO);
    let view_from_world = world_from_local.inverse();
    let clip_from_view = host_clip_from_view(fov, aspect, near);
    let clip_from_world = clip_from_view * view_from_world;
    let forward = world_from_local.transform_vector3(Vec3::NEG_Z);
    let right = world_from_local.transform_vector3(Vec3::X);
    let up = world_from_local.transform_vector3(Vec3::Y);
    let mut frustum_planes = perspective_frustum_planes(FrustumSpec {
        eye,
        forward,
        right,
        up,
        fov_y_rad: fov,
        aspect,
        near,
        far,
    })
    .to_vec();
    if no_frustum_env() {
        frustum_planes.clear();
    } else if frustum_nearfar_env() {
        frustum_planes.truncate(2);
    }
    let parms = pack_live_view_parms(view_from_world, clip_from_view, clip_from_world, eye);
    let tan_half_y = (fov * 0.5).tan();
    let tan_half_x = tan_half_y * aspect;
    let axis = [
        [forward.x, forward.y, forward.z],
        [right.x, right.y, right.z],
        [up.x, up.y, up.z],
    ];
    let portal_bevels =
        hud_iw4::set_view_parms_matrices(eye.to_array(), axis, tan_half_x, tan_half_y, dpvs_z_near)
            .map(|(vp, inv)| dpvs_iw4::portal_bevels_from_d3d_row_major(&vp, &inv));
    PreparedSceneView {
        ready: true,
        eye,
        forward,
        view_from_world,
        clip_from_view,
        clip_from_world,
        frustum_planes,
        fov,
        aspect,
        near,
        far,
        rt_w,
        rt_h,
        parms,
        portal_bevels,
        depth_hack_near,
        scene_viewport,
    }
}

fn no_frustum_env() -> bool {
    matches!(
        std::env::var("IW4L_NO_FRUSTUM").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

fn frustum_nearfar_env() -> bool {
    matches!(
        std::env::var("IW4L_FRUSTUM_NEARFAR").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inside(planes: &[[f32; 4]], p: Vec3) -> bool {
        planes
            .iter()
            .all(|n| n[0] * p.x + n[1] * p.y + n[2] * p.z + n[3] >= 0.0)
    }

    #[test]
    fn widened_cull_view_covers_held_turns_and_moves() {
        let (fov, aspect) = (65f32.to_radians(), 16.0 / 9.0);
        let at = |yaw: f32, eye: Vec3| {
            let pose = Mat4::from_translation(eye) * Mat4::from_rotation_y(yaw);
            move |fov, aspect| {
                prepare_scene_view(
                    pose,
                    fov,
                    aspect,
                    4.0,
                    8000.0,
                    1280,
                    720,
                    4.0,
                    4.0,
                    Default::default(),
                )
            }
        };
        let live = at(0.0, Vec3::ZERO)(fov, aspect);
        let held = widened_cull_view(&live, at(0.0, Vec3::ZERO));
        let half_x = ((fov * 0.5).tan() * aspect).atan();
        // Just past the live edge, as a turn within the hold would show it.
        let edge = |angle: f32, dist: f32| Mat3::from_rotation_y(angle) * Vec3::NEG_Z * dist;
        let past_edge = edge(half_x + HOLD_TURN_RAD * 0.9, 500.0);
        assert!(!inside(&live.frustum_planes, past_edge));
        assert!(inside(&held.frustum_planes, past_edge));
        // Beside the eye, where a step within the hold would bring it in view.
        let beside = Vec3::new(-HOLD_EYE_UNITS * 0.9, 0.0, 0.0);
        assert!(inside(&held.frustum_planes, beside));
        // Well outside still culls.
        assert!(!inside(
            &held.frustum_planes,
            edge(half_x + 3.0 * HOLD_TURN_RAD, 500.0)
        ));
        assert!(!inside(&held.frustum_planes, Vec3::new(0.0, 0.0, 500.0)));
    }
}
