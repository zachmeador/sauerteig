//! Light descriptions, their shadow-tile projections, and the packed record the
//! shaders read. Everything here is plain math; GPU resources live in `render`.
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use sauerteig_core::{ObjectKind, WorldObject};

pub const DEFAULT_COLOR: [f32; 3] = [1.0, 0.74, 0.46];
pub const DEFAULT_INTENSITY: f32 = 16.0;
pub const DEFAULT_RANGE: f32 = 12.0;
/// Lights with a smaller radius are drawn without shadows.
pub const MIN_SHADOW_RANGE: f32 = 2.0;
/// Shadow tiles ignore casters closer to the light than this.
pub const SHADOW_NEAR: f32 = 0.1;
/// Each tile renders slightly past its nominal field of view, so filter taps
/// near an edge stay inside the tile: two texels at the smallest (128 px) size.
pub const TILE_GUARD_SCALE: f32 = 128.0 / 124.0;
const MAX_SPOT_ANGLE: f32 = 75.0 * std::f32::consts::PI / 180.0;
const MAX_RANGE: f32 = 256.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LightKind {
    Point,
    /// A cone along `direction` (unit length) with half-angles in radians. Only
    /// captures and tests make these until object properties can.
    Spot {
        direction: Vec3,
        inner: f32,
        outer: f32,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShadowMode {
    Off,
    /// Static geometry only, cached until a chunk edit or light edit. The most
    /// important shadowed lights also shadow moving actors, as dynamic ones do.
    #[default]
    Static,
    /// Static geometry plus moving actors, which draw into an overlay.
    Dynamic,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightDesc {
    /// Stable identity: the object ID for light nodes.
    pub id: u32,
    pub position: Vec3,
    /// Linear RGB.
    pub color: Vec3,
    pub intensity: f32,
    pub range: f32,
    pub kind: LightKind,
    pub shadow: ShadowMode,
}

impl LightDesc {
    pub fn point(id: u32, position: Vec3) -> Self {
        Self {
            id,
            position,
            color: Vec3::from_array(DEFAULT_COLOR),
            intensity: DEFAULT_INTENSITY,
            range: DEFAULT_RANGE,
            kind: LightKind::Point,
            shadow: ShadowMode::Static,
        }
    }

    pub fn spot(id: u32, position: Vec3, direction: Vec3, inner: f32, outer: f32) -> Self {
        Self {
            kind: LightKind::Spot {
                direction,
                inner,
                outer,
            },
            ..Self::point(id, position)
        }
    }

    /// Clamp values the shaders cannot represent. Returns `None` for lights
    /// that cannot be drawn at all.
    pub fn sanitized(mut self) -> Option<Self> {
        let finite = self.position.is_finite()
            && self.color.is_finite()
            && self.intensity.is_finite()
            && self.range.is_finite();
        if !finite || self.range <= 0.0 || self.intensity < 0.0 {
            return None;
        }
        self.range = self.range.min(MAX_RANGE);
        self.color = self.color.max(Vec3::ZERO);
        if let LightKind::Spot {
            direction,
            inner,
            outer,
        } = &mut self.kind
        {
            *direction = direction.try_normalize()?;
            *outer = if outer.is_finite() {
                outer.clamp(0.01, MAX_SPOT_ANGLE)
            } else {
                return None;
            };
            *inner = if inner.is_finite() {
                inner.clamp(0.0, *outer)
            } else {
                0.0
            };
        }
        Some(self)
    }

    pub fn is_spot(&self) -> bool {
        matches!(self.kind, LightKind::Spot { .. })
    }

    pub fn needs_shadow(&self) -> bool {
        self.shadow != ShadowMode::Off && self.range >= MIN_SHADOW_RANGE
    }

    pub fn face_count(&self) -> usize {
        if self.is_spot() {
            1
        } else {
            6
        }
    }

    /// Projected radius of the light's range in pixels, from the camera.
    pub fn projected_radius(&self, eye: Vec3, pixels_per_radian: f32) -> f32 {
        self.range / self.position.distance(eye).max(0.5) * pixels_per_radian
    }

    /// Ranking for shadow tiles and the visible-light budget: nearer and larger
    /// lights first.
    pub fn importance(&self, eye: Vec3) -> f32 {
        self.range / self.position.distance(eye).max(0.5)
    }

    /// Tangent of the half field of view of a tile, including the guard band.
    pub fn tile_tan_half(&self) -> f32 {
        let tan = match self.kind {
            LightKind::Point => 1.0,
            LightKind::Spot { outer, .. } => outer.tan(),
        };
        tan * TILE_GUARD_SCALE
    }

    /// World to clip transform of one shadow tile: a cube face for point
    /// lights, or the cone's axis for spot lights.
    pub fn tile_projection(&self, face: usize) -> Mat4 {
        let (forward, up) = self.tile_axes(face);
        let fov = 2.0 * self.tile_tan_half().atan();
        Mat4::perspective_rh(fov, 1.0, SHADOW_NEAR, self.range)
            * Mat4::look_to_rh(self.position, forward, up)
    }

    pub fn tile_axes(&self, face: usize) -> (Vec3, Vec3) {
        match self.kind {
            LightKind::Point => cube_face_axes(face),
            LightKind::Spot { direction, .. } => (direction, spot_up(direction)),
        }
    }

    pub fn packed(
        &self,
        fade: f32,
        shadow: Option<[u16; 6]>,
        overlay: Option<[u8; 6]>,
    ) -> GpuLight {
        let (spot, direction, scale, offset) = match self.kind {
            LightKind::Point => (0, Vec3::ZERO, 0.0, 1.0),
            LightKind::Spot {
                direction,
                inner,
                outer,
            } => {
                let scale = 1.0 / (inner.cos() - outer.cos()).max(1.0e-3);
                (1, direction, scale, -outer.cos() * scale)
            }
        };
        let mode = match (self.shadow, shadow) {
            (ShadowMode::Off, _) | (_, None) => 0,
            (ShadowMode::Static, _) => 1,
            (ShadowMode::Dynamic, _) => 2,
        };
        let mut flags = spot | (mode << 1);
        if mode == 2 {
            let mut slots = 0u32;
            for face in 0..6 {
                let slot = overlay.map_or(OVERLAY_NONE, |s| s[face]);
                slots |= u32::from(slot) << (OVERLAY_SHIFT + 4 * face as u32);
            }
            flags |= slots;
        }
        let refs = shadow.unwrap_or([NO_TILE; 6]);
        let tile_pair = |i: usize| u32::from(refs[i]) | u32::from(refs[i + 1]) << 16;
        let mut packed = [flags, tile_pair(0), tile_pair(2), tile_pair(4)];
        if self.is_spot() {
            packed[2] = self.tile_tan_half().to_bits();
        }
        let color = self.color * self.intensity * fade;
        GpuLight {
            position_range: [
                self.position.x,
                self.position.y,
                self.position.z,
                self.range,
            ],
            color_scale: [color.x, color.y, color.z, scale],
            direction_offset: [direction.x, direction.y, direction.z, offset],
            packed,
        }
    }
}

/// Overlay slot value for a face without actor shadows.
pub const OVERLAY_NONE: u8 = 15;
const OVERLAY_SHIFT: u32 = 3;
pub const NO_TILE: u16 = 0xFFFF;

pub fn cube_face_axes(face: usize) -> (Vec3, Vec3) {
    let axis = face / 2;
    let mut forward = Vec3::ZERO;
    forward[axis] = if face.is_multiple_of(2) { 1.0 } else { -1.0 };
    (forward, if axis == 1 { Vec3::Z } else { Vec3::Y })
}

pub fn spot_up(direction: Vec3) -> Vec3 {
    if direction.y.abs() > 0.99 {
        Vec3::Z
    } else {
        Vec3::Y
    }
}

/// One light as the shaders read it: 64 bytes, laid out for both storage and
/// uniform buffers.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable, PartialEq)]
pub struct GpuLight {
    /// Position, range.
    pub position_range: [f32; 4],
    /// Color times intensity times fade, cone scale.
    pub color_scale: [f32; 4],
    /// Spot direction, cone offset.
    pub direction_offset: [f32; 4],
    /// Flags (spot, shadow mode, overlay slots) and six 16-bit tile references.
    pub packed: [u32; 4],
}

/// The light a light node places, until the world format stores properties.
pub fn light_from_object(object: &WorldObject) -> Option<LightDesc> {
    (object.kind == ObjectKind::Light)
        .then(|| LightDesc::point(object.id, object.position).sanitized())
        .flatten()
}

/// A bounded sun-shadow window follows the camera on large maps. Snap its light-space
/// origin to texels so small camera motions do not make stationary shadows crawl.
pub fn sun_shadow_matrix(sun: Vec3, focus: Vec3) -> Mat4 {
    const HALF_WIDTH: f32 = 96.0;
    const DEPTH: f32 = 256.0;
    const RESOLUTION: f32 = 2048.0;
    let sun = sun.try_normalize().unwrap_or(Vec3::Y);
    let up = if sun.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let view = Mat4::look_to_rh(Vec3::ZERO, -sun, up);
    let mut center = view.transform_point3(focus);
    let texel = 2.0 * HALF_WIDTH / RESOLUTION;
    // A six-metre window step (64 texels) keeps the projection stable while
    // strafing. Quantize depth too; a continuous depth origin defeats caching.
    let step = texel * 64.0;
    center = (center / step).round() * step;
    Mat4::orthographic_rh(
        center.x - HALF_WIDTH,
        center.x + HALF_WIDTH,
        center.y - HALF_WIDTH,
        center.y + HALF_WIDTH,
        -center.z - DEPTH,
        -center.z + DEPTH,
    ) * view
}

#[cfg(test)]
mod tests {
    use super::*;
    use sauerteig_core::{WORLD_HEIGHT, WORLD_MAX, WORLD_MIN};

    fn box_corners(min: Vec3, max: Vec3) -> [Vec3; 8] {
        std::array::from_fn(|i| {
            Vec3::new(
                if i & 1 == 0 { min.x } else { max.x },
                if i & 2 == 0 { min.y } else { max.y },
                if i & 4 == 0 { min.z } else { max.z },
            )
        })
    }

    fn ndc(matrix: Mat4, position: Vec3) -> Vec3 {
        let clip = matrix * position.extend(1.0);
        clip.truncate() / clip.w
    }

    fn node(id: u32, kind: ObjectKind, position: Vec3) -> WorldObject {
        WorldObject {
            id,
            position,
            kind,
            rotation: 0.0,
        }
    }

    #[test]
    fn sun_shadows_follow_distant_players_and_snap_small_camera_motion() {
        let focus = Vec3::new(3000.0, 800.0, -4000.0);
        let matrix = sun_shadow_matrix(Vec3::Y, focus);
        for corner in box_corners(focus - Vec3::splat(40.0), focus + Vec3::splat(40.0)) {
            let p = ndc(matrix, corner);
            assert!(p.x.abs() < 1.0 && p.y.abs() < 1.0 && p.z > 0.0 && p.z < 1.0);
        }
        assert_eq!(
            sun_shadow_matrix(Vec3::Y, Vec3::ZERO),
            sun_shadow_matrix(Vec3::Y, Vec3::X * 0.001)
        );
    }

    #[test]
    fn sun_projection_contains_the_full_world_with_positive_depth_and_padding() {
        let corners = box_corners(
            Vec3::new(WORLD_MIN as f32, 0.0, WORLD_MIN as f32),
            Vec3::new(WORLD_MAX as f32, WORLD_HEIGHT as f32, WORLD_MAX as f32),
        );
        for sun in [Vec3::new(-0.44, 0.76, 0.34), Vec3::Y, -Vec3::Y, Vec3::X] {
            let matrix = sun_shadow_matrix(sun, Vec3::new(0.0, WORLD_HEIGHT as f32 * 0.5, 0.0));
            for corner in corners {
                let p = ndc(matrix, corner);
                assert!(p.x > -1.0 && p.x < 1.0, "sun {sun:?}, {p:?}");
                assert!(p.y > -1.0 && p.y < 1.0, "sun {sun:?}, {p:?}");
                assert!(p.z > 0.0 && p.z < 1.0, "sun {sun:?}, {p:?}");
            }
        }
    }

    #[test]
    fn only_light_nodes_become_lights_at_their_stored_position() {
        let position = Vec3::new(-12.5, 9.25, 17.75);
        let light = light_from_object(&node(7, ObjectKind::Light, position)).unwrap();
        assert_eq!(light.id, 7);
        assert_eq!(light.position, position);
        assert_eq!(light.kind, LightKind::Point);
        assert_eq!(light.shadow, ShadowMode::Static);
        assert_eq!(light.color.to_array(), DEFAULT_COLOR);
        assert_eq!(light.range, 12.0);
        for kind in [
            ObjectKind::WallLamp,
            ObjectKind::HangingLamp,
            ObjectKind::Tree,
            ObjectKind::Spawn,
        ] {
            assert!(light_from_object(&node(1, kind, position)).is_none());
        }
        assert!(light_from_object(&node(1, ObjectKind::Light, Vec3::NAN)).is_none());

        // Both optional lamp models still load through the ordinary prop path.
        let assets = crate::props::load().unwrap();
        for kind in [ObjectKind::WallLamp, ObjectKind::HangingLamp] {
            let asset = assets
                .iter()
                .find(|asset| asset.definition.kind == kind)
                .unwrap();
            assert!(!asset.near.indices.is_empty());
        }
    }

    #[test]
    fn sanitizing_rejects_unusable_lights_and_clamps_cones() {
        let mut light = LightDesc::point(1, Vec3::ZERO);
        light.range = 0.0;
        assert!(light.sanitized().is_none());
        light.range = f32::NAN;
        assert!(light.sanitized().is_none());
        let spot = LightDesc::spot(2, Vec3::ZERO, Vec3::new(0.0, -3.0, 0.0), 2.0, 3.0);
        let LightKind::Spot {
            direction,
            inner,
            outer,
        } = spot.sanitized().unwrap().kind
        else {
            unreachable!()
        };
        assert!((direction - -Vec3::Y).length() < 1.0e-6);
        assert!(outer <= MAX_SPOT_ANGLE && inner <= outer);
        let degenerate = LightDesc::spot(3, Vec3::ZERO, Vec3::ZERO, 0.1, 0.2);
        assert!(degenerate.sanitized().is_none());
    }

    #[test]
    fn cube_faces_cover_the_sphere_and_match_webgpu_clip_space() {
        let light = LightDesc::point(1, Vec3::new(3.0, 9.0, -2.0));
        let inner = 1.0 / TILE_GUARD_SCALE;
        for face in 0..6 {
            let (forward, up) = cube_face_axes(face);
            assert!((forward.dot(up)).abs() < 1.0e-6);
            let matrix = light.tile_projection(face);
            for (depth, expected_z) in [(SHADOW_NEAR, 0.0), (light.range, 1.0)] {
                let p = ndc(matrix, light.position + forward * depth);
                assert!(p.x.abs() < 1.0e-5 && p.y.abs() < 1.0e-5);
                assert!((p.z - expected_z).abs() < 1.0e-4, "face {face}: {p:?}");
            }
            // The face's own 90 degree wedge spans the unguarded part of the tile.
            let right = forward.cross(up).normalize();
            let p = ndc(matrix, light.position + forward * 4.0 + right * 4.0);
            assert!((p.x - inner).abs() < 1.0e-5 && p.y.abs() < 1.0e-5, "{p:?}");
            let p = ndc(matrix, light.position + forward * 4.0 + up * 4.0);
            assert!((p.y - inner).abs() < 1.0e-5 && p.x.abs() < 1.0e-5, "{p:?}");
        }
    }

    #[test]
    fn spot_tile_covers_exactly_its_cone_with_a_guard_band() {
        let direction = Vec3::new(1.0, -1.0, 0.5).normalize();
        let light = LightDesc::spot(1, Vec3::new(0.0, 5.0, 0.0), direction, 0.3, 0.6)
            .sanitized()
            .unwrap();
        let matrix = light.tile_projection(0);
        let (forward, up) = light.tile_axes(0);
        let right = forward.cross(up).normalize();
        let true_up = right.cross(forward);
        let edge = light.position + (forward + right * 0.6_f32.tan()) * 3.0;
        let p = ndc(matrix, edge);
        assert!((p.x - 1.0 / TILE_GUARD_SCALE).abs() < 1.0e-5 && p.y.abs() < 1.0e-5);
        let edge = light.position + (forward + true_up * 0.6_f32.tan()) * 3.0;
        let p = ndc(matrix, edge);
        assert!((p.y - 1.0 / TILE_GUARD_SCALE).abs() < 1.0e-5 && p.x.abs() < 1.0e-5);
        assert_eq!(spot_up(Vec3::Y), Vec3::Z);
        assert_eq!(spot_up(Vec3::X), Vec3::Y);
    }

    #[test]
    fn packed_records_hold_cone_shadow_and_overlay_state() {
        let point = LightDesc::point(1, Vec3::new(1.0, 2.0, 3.0));
        let refs = [0x0001, 0x0102, 0x0203, 0x0304, 0x0405, 0x0506];
        let record = point.packed(0.5, Some(refs), None);
        assert_eq!(record.position_range, [1.0, 2.0, 3.0, 12.0]);
        assert_eq!(
            record.color_scale[0],
            0.5 * DEFAULT_INTENSITY * DEFAULT_COLOR[0]
        );
        assert_eq!(record.packed[0], 1 << 1);
        assert_eq!(record.packed[1], 0x0102_0001);
        assert_eq!(record.packed[3], 0x0506_0405);
        assert_eq!(point.packed(1.0, None, None).packed[0], 0);

        let mut dynamic = point;
        dynamic.shadow = ShadowMode::Dynamic;
        let record = dynamic.packed(1.0, Some(refs), Some([0, 1, 2, 3, 4, OVERLAY_NONE]));
        assert_eq!(record.packed[0] & 7, 2 << 1);
        for (face, slot) in [0, 1, 2, 3, 4, 15].into_iter().enumerate() {
            assert_eq!((record.packed[0] >> (3 + 4 * face)) & 15, slot);
        }

        let spot = LightDesc::spot(2, Vec3::ZERO, -Vec3::Y, 0.2, 0.5)
            .sanitized()
            .unwrap();
        let record = spot.packed(1.0, Some(refs), None);
        assert_eq!(record.packed[0] & 1, 1);
        assert_eq!(record.direction_offset[..3], [0.0, -1.0, 0.0]);
        assert_eq!(f32::from_bits(record.packed[2]), spot.tile_tan_half());
        let cone =
            |cos: f32| (cos * record.color_scale[3] + record.direction_offset[3]).clamp(0.0, 1.0);
        assert!((cone(0.5_f32.cos()) - 0.0).abs() < 1.0e-5);
        assert!((cone(0.2_f32.cos()) - 1.0).abs() < 1.0e-5);
        assert_eq!(std::mem::size_of::<GpuLight>(), 64);
    }

    #[test]
    fn importance_prefers_near_large_lights() {
        let eye = Vec3::ZERO;
        let near = LightDesc::point(1, Vec3::X * 3.0);
        let far = LightDesc::point(2, Vec3::X * 30.0);
        let mut big = LightDesc::point(3, Vec3::X * 30.0);
        big.range = 150.0;
        assert!(near.importance(eye) > far.importance(eye));
        assert!(big.importance(eye) > near.importance(eye));
        assert_eq!(
            near.projected_radius(eye, 100.0),
            near.importance(eye) * 100.0
        );
        // The camera inside a light never divides by zero.
        assert!(near.importance(near.position).is_finite());
    }
}
