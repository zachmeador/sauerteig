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
/// near an edge stay inside the tile: this many texels on each side.
pub const TILE_GUARD_TEXELS: f32 = 2.0;
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
    /// Static geometry only, cached until a chunk edit or light edit. For
    /// comparison captures: nothing the game places has it.
    Static,
    /// Static geometry plus moving actors, which draw into per-frame tiles.
    /// Every light shadows actors this way.
    #[default]
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
            shadow: ShadowMode::Dynamic,
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

    /// Tangent of the half field of view of a tile without its guard band.
    pub fn base_tan_half(&self) -> f32 {
        match self.kind {
            LightKind::Point => 1.0,
            LightKind::Spot { outer, .. } => outer.tan(),
        }
    }

    /// Tangent of the half field of view of a tile of `size` texels, including
    /// the guard band.
    pub fn tile_tan_half(&self, size: u32) -> f32 {
        self.base_tan_half() * guard_scale(size)
    }

    /// World to clip transform of one shadow tile of `size` texels: a cube face
    /// for point lights, or the cone's axis for spot lights.
    pub fn tile_projection(&self, face: usize, size: u32) -> Mat4 {
        let (forward, up) = self.tile_axes(face);
        let fov = 2.0 * self.tile_tan_half(size).atan();
        Mat4::perspective_rh(fov, 1.0, SHADOW_NEAR, self.range)
            * Mat4::look_to_rh(self.position, forward, up)
    }

    pub fn tile_axes(&self, face: usize) -> (Vec3, Vec3) {
        match self.kind {
            LightKind::Point => cube_face_axes(face),
            LightKind::Spot { direction, .. } => (direction, spot_up(direction)),
        }
    }

    pub fn packed(&self, fade: f32, shadow: Option<[u32; 6]>, frame: Option<[u32; 6]>) -> GpuLight {
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
        let mut packed = [spot | (mode << 1), 0, 0, 0];
        if self.is_spot() {
            packed[2] = self.base_tan_half().to_bits();
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
            tiles: shadow.unwrap_or([NO_TILE; 6]),
            frame_tiles: if mode == 2 {
                frame.unwrap_or([NO_TILE; 6])
            } else {
                [NO_TILE; 6]
            },
        }
    }
}

/// How much wider than its face a tile of `size` texels renders, so that the
/// guard band is `TILE_GUARD_TEXELS` on each side.
pub fn guard_scale(size: u32) -> f32 {
    size as f32 / (size as f32 - 2.0 * TILE_GUARD_TEXELS)
}

/// Tile reference of a face without a tile.
pub const NO_TILE: u32 = u32::MAX;

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

/// One light as the shaders read it: 112 bytes in a storage buffer.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable, PartialEq)]
pub struct GpuLight {
    /// Position, range.
    pub position_range: [f32; 4],
    /// Color times intensity times fade, cone scale.
    pub color_scale: [f32; 4],
    /// Spot direction, cone offset.
    pub direction_offset: [f32; 4],
    /// Flags (bit 0 spot, bits 1-2 shadow mode), and a spot light's tile
    /// tangent as float bits in z.
    pub packed: [u32; 4],
    /// The static tile of each face, or `NO_TILE`.
    pub tiles: [u32; 6],
    /// The per-frame tile of each face that actors can shadow, or `NO_TILE`.
    pub frame_tiles: [u32; 6],
}

/// The light a light node places, until the world format stores properties.
pub fn light_from_object(object: &WorldObject) -> Option<LightDesc> {
    (object.kind == ObjectKind::Light)
        .then(|| LightDesc::point(object.id, object.position).sanitized())
        .flatten()
}

/// How many cascades the sun's shadow is split into.
pub const CASCADES: usize = 3;
/// How far from the eye each cascade is guaranteed to reach, in metres.
pub const CASCADE_SPLITS: [f32; CASCADES] = [15.0, 42.0, 150.0];
/// A cascade's window moves in steps of this many texels, so its cached map
/// stays valid while the eye walks inside a step.
const CASCADE_STEP_TEXELS: f32 = 64.0;
/// Half the depth range each cascade covers around its snapped centre.
const CASCADE_DEPTH: f32 = 256.0;

/// One cascade of the sun's shadow: a square window around the eye, looking
/// along the sun.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunCascade {
    pub matrix: Mat4,
    /// Metres per shadow-map texel.
    pub texel: f32,
    /// Half the window's width in metres.
    pub half_width: f32,
}

/// Three square windows centred on the eye, reaching `CASCADE_SPLITS` metres in
/// every direction, in `resolution`-texel maps. A window depends on the eye's
/// position only, never on where it looks, and moves in whole steps of 64
/// texels, with its depth origin snapped the same way, so a cached map stays
/// valid until the eye leaves its step and no stationary shadow crawls.
pub fn sun_cascades(sun: Vec3, eye: Vec3, resolution: u32) -> [SunCascade; CASCADES] {
    let sun = sun.try_normalize().unwrap_or(Vec3::Y);
    let up = if sun.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let view = Mat4::look_to_rh(Vec3::ZERO, -sun, up);
    let focus = view.transform_point3(eye);
    let resolution = resolution as f32;
    CASCADE_SPLITS.map(|split| {
        // The window must hold the whole circle of radius `split` around an eye
        // that can sit half a step from the window's centre.
        let half_width = split / (1.0 - CASCADE_STEP_TEXELS / resolution);
        let texel = 2.0 * half_width / resolution;
        let step = texel * CASCADE_STEP_TEXELS;
        let center = (focus / step).round() * step;
        SunCascade {
            matrix: Mat4::orthographic_rh(
                center.x - half_width,
                center.x + half_width,
                center.y - half_width,
                center.y + half_width,
                -center.z - CASCADE_DEPTH,
                -center.z + CASCADE_DEPTH,
            ) * view,
            texel,
            half_width,
        }
    })
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

    fn inside(matrix: Mat4, position: Vec3) -> bool {
        let p = ndc(matrix, position);
        p.x.abs() < 1.0 && p.y.abs() < 1.0 && p.z > 0.0 && p.z < 1.0
    }

    #[test]
    fn every_cascade_holds_the_ball_around_the_eye_that_it_promises() {
        let sun = Vec3::new(-0.44, 0.76, 0.34);
        for resolution in [512, 1024, 2048] {
            for eye in [
                Vec3::new(3000.0, 800.0, -4000.0),
                Vec3::new(0.3, 2.6, 0.3),
                Vec3::new(-17.9, 40.1, 63.7),
            ] {
                let cascades = sun_cascades(sun, eye, resolution);
                for (cascade, split) in cascades.iter().zip(CASCADE_SPLITS) {
                    let reach = split * 0.999;
                    for offset in [
                        Vec3::X,
                        -Vec3::X,
                        Vec3::Y,
                        -Vec3::Y,
                        Vec3::Z,
                        -Vec3::Z,
                        Vec3::new(1.0, 1.0, 1.0).normalize(),
                        Vec3::new(-1.0, 1.0, -1.0).normalize(),
                    ] {
                        assert!(
                            inside(cascade.matrix, eye + offset * reach),
                            "eye {eye:?}, {resolution}, split {split}, {offset:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn cascade_windows_move_in_whole_steps_and_ignore_where_the_eye_looks() {
        let sun = Vec3::new(-0.44, 0.76, 0.34);
        let eye = Vec3::new(10.0, 20.0, 30.0);
        let base = sun_cascades(sun, eye, 1024);
        // Moving by less than a texel never changes a window, and neither does
        // turning, which the eye position alone decides.
        for nudge in [Vec3::X, Vec3::Y, Vec3::Z, -Vec3::X, -Vec3::Y, -Vec3::Z] {
            let moved = sun_cascades(sun, eye + nudge * 0.001, 1024);
            for (a, b) in base.iter().zip(&moved) {
                assert_eq!(a.matrix, b.matrix);
            }
        }
        // The three windows have the texel sizes the splits promise at 1024.
        for (cascade, expected) in base.iter().zip([0.031, 0.0875, 0.3125]) {
            assert!((cascade.texel - expected).abs() < 0.0005, "{cascade:?}");
        }
        // Every window's translation is a whole number of texels, so a shadow
        // texel never lands at a different place after the window moves.
        let walked = sun_cascades(sun, eye + Vec3::new(40.0, 0.0, 0.0), 1024);
        for (a, b) in base.iter().zip(&walked) {
            let corner = Vec3::new(5.0, 6.0, 7.0);
            let (pa, pb) = (ndc(a.matrix, corner), ndc(b.matrix, corner));
            let texels = |d: f32| d * 0.5 * 1024.0;
            let shift = [texels(pa.x - pb.x), texels(pa.y - pb.y)];
            for s in shift {
                assert!((s - s.round()).abs() < 0.01, "{s}");
            }
        }
    }

    #[test]
    fn the_far_cascade_holds_the_whole_world_with_positive_depth_and_padding() {
        let corners = box_corners(
            Vec3::new(WORLD_MIN as f32, 0.0, WORLD_MIN as f32),
            Vec3::new(WORLD_MAX as f32, WORLD_HEIGHT as f32, WORLD_MAX as f32),
        );
        for sun in [Vec3::new(-0.44, 0.76, 0.34), Vec3::Y, -Vec3::Y, Vec3::X] {
            let eye = Vec3::new(0.0, WORLD_HEIGHT as f32 * 0.5, 0.0);
            for cascade in sun_cascades(sun, eye, 1024) {
                for corner in corners {
                    let p = ndc(cascade.matrix, corner);
                    assert!(p.z > 0.0 && p.z < 1.0, "sun {sun:?}, {p:?}");
                }
            }
            let far = sun_cascades(sun, eye, 1024)[CASCADES - 1];
            for corner in corners {
                assert!(inside(far.matrix, corner), "sun {sun:?}, {corner:?}");
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
        assert_eq!(light.shadow, ShadowMode::Dynamic);
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
        let inner = 1.0 / guard_scale(128);
        for face in 0..6 {
            let (forward, up) = cube_face_axes(face);
            assert!((forward.dot(up)).abs() < 1.0e-6);
            let matrix = light.tile_projection(face, 128);
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
        let matrix = light.tile_projection(0, 128);
        let (forward, up) = light.tile_axes(0);
        let right = forward.cross(up).normalize();
        let true_up = right.cross(forward);
        let edge = light.position + (forward + right * 0.6_f32.tan()) * 3.0;
        let p = ndc(matrix, edge);
        assert!((p.x - 1.0 / guard_scale(128)).abs() < 1.0e-5 && p.y.abs() < 1.0e-5);
        let edge = light.position + (forward + true_up * 0.6_f32.tan()) * 3.0;
        let p = ndc(matrix, edge);
        assert!((p.y - 1.0 / guard_scale(128)).abs() < 1.0e-5 && p.x.abs() < 1.0e-5);
        assert_eq!(spot_up(Vec3::Y), Vec3::Z);
        assert_eq!(spot_up(Vec3::X), Vec3::Y);
    }

    #[test]
    fn guard_bands_hold_two_texels_whatever_the_tile_size() {
        for size in [32, 64, 128, 256, 512] {
            // A wedge of half-tangent 1 spans size - 4 texels of a tile.
            let texels = size as f32 / guard_scale(size);
            assert!((texels - (size as f32 - 4.0)).abs() < 1.0e-4, "{size}");
        }
        assert_eq!(guard_scale(128), 128.0 / 124.0);
        let light = LightDesc::point(1, Vec3::ZERO);
        assert_eq!(light.tile_tan_half(32), 32.0 / 28.0);
        let matrix = light.tile_projection(0, 32);
        let p = ndc(matrix, Vec3::new(4.0, 0.0, 4.0));
        assert!((p.x - 28.0 / 32.0).abs() < 1.0e-5, "{p:?}");
    }

    #[test]
    fn packed_records_hold_cone_shadow_and_tile_state() {
        let point = LightDesc::point(1, Vec3::new(1.0, 2.0, 3.0));
        let refs = [1, 258, 515, 772, 1029, 1286];
        let record = point.packed(0.5, Some(refs), None);
        assert_eq!(record.position_range, [1.0, 2.0, 3.0, 12.0]);
        assert_eq!(
            record.color_scale[0],
            0.5 * DEFAULT_INTENSITY * DEFAULT_COLOR[0]
        );
        assert_eq!(record.packed[0], 2 << 1);
        assert_eq!(record.tiles, refs);
        assert_eq!(record.frame_tiles, [NO_TILE; 6]);
        let unshadowed = point.packed(1.0, None, None);
        assert_eq!(unshadowed.packed[0], 0);
        assert_eq!(unshadowed.tiles, [NO_TILE; 6]);

        let mut dynamic = point;
        dynamic.shadow = ShadowMode::Dynamic;
        let frame = [0, 1, NO_TILE, 3, 4, NO_TILE];
        let record = dynamic.packed(1.0, Some(refs), Some(frame));
        assert_eq!(record.packed[0] & 7, 2 << 1);
        assert_eq!(record.frame_tiles, frame);
        // A light that only shadows static geometry takes no per-frame tiles.
        let mut fixed = point;
        fixed.shadow = ShadowMode::Static;
        let record = fixed.packed(1.0, Some(refs), Some(frame));
        assert_eq!(record.packed[0] & 7, 1 << 1);
        assert_eq!(record.frame_tiles, [NO_TILE; 6]);

        let spot = LightDesc::spot(2, Vec3::ZERO, -Vec3::Y, 0.2, 0.5)
            .sanitized()
            .unwrap();
        let record = spot.packed(1.0, Some(refs), None);
        assert_eq!(record.packed[0] & 1, 1);
        assert_eq!(record.direction_offset[..3], [0.0, -1.0, 0.0]);
        assert_eq!(f32::from_bits(record.packed[2]), 0.5_f32.tan());
        let cone =
            |cos: f32| (cos * record.color_scale[3] + record.direction_offset[3]).clamp(0.0, 1.0);
        assert!((cone(0.5_f32.cos()) - 0.0).abs() < 1.0e-5);
        assert!((cone(0.2_f32.cos()) - 1.0).abs() < 1.0e-5);
        assert_eq!(std::mem::size_of::<GpuLight>(), 112);
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
