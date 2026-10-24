use glam::{Mat4, Vec3, Vec4};
use sauerteig_core::{
    ChunkKey, ObjectKind, WorldObject, CHUNK_SIZE, WORLD_HEIGHT, WORLD_MAX, WORLD_MIN,
};

pub const LOCAL_LIGHTS: usize = 8;
pub const LOCAL_SHADOW_SIZE: u32 = 512;
pub const LIGHT_RANGE: f32 = 12.0;
pub const LIGHT_NEAR: f32 = 0.12;
pub const LIGHT_HALF_ANGLE: f32 = std::f32::consts::FRAC_PI_3;
const LAMP_OFFSET: f32 = 0.28;
const CULL_EPSILON: f32 = 1.0e-4;

#[derive(Clone, Copy, Debug)]
pub struct LocalLight {
    pub position: Vec3,
    pub view_proj: Mat4,
}

impl LocalLight {
    fn from_fixture(position: Vec3) -> Self {
        // The emitter sits below the lens and cage, so the fixture does not
        // immediately occlude its own downward light.
        let position = position - Vec3::Y * LAMP_OFFSET;
        let view = Mat4::look_to_rh(position, -Vec3::Y, Vec3::Z);
        let projection = Mat4::perspective_rh(LIGHT_HALF_ANGLE * 2.0, 1.0, LIGHT_NEAR, LIGHT_RANGE);
        Self {
            position,
            view_proj: projection * view,
        }
    }
}

/// A fixed light budget with deterministic ties, independent of object order.
/// No temporary allocation is needed when this runs each frame.
pub fn closest_lights(objects: &[WorldObject], eye: Vec3) -> [Option<LocalLight>; LOCAL_LIGHTS] {
    let mut nearby: [Option<(f32, u32, LocalLight)>; LOCAL_LIGHTS] = [None; LOCAL_LIGHTS];
    if !eye.is_finite() {
        return [None; LOCAL_LIGHTS];
    }
    for object in objects {
        if !matches!(object.kind, ObjectKind::Light) || !object.position.is_finite() {
            continue;
        }
        let light = LocalLight::from_fixture(object.position);
        let distance = light.position.distance_squared(eye);
        if let Some(slot) = nearby.iter().position(|entry| {
            entry.is_none_or(|(d, id, _)| {
                distance
                    .total_cmp(&d)
                    .then_with(|| object.id.cmp(&id))
                    .is_lt()
            })
        }) {
            nearby.copy_within(slot..LOCAL_LIGHTS - 1, slot + 1);
            nearby[slot] = Some((distance, object.id, light));
        }
    }
    nearby.map(|entry| entry.map(|(_, _, light)| light))
}

fn box_corners(min: Vec3, max: Vec3) -> [Vec3; 8] {
    std::array::from_fn(|i| {
        Vec3::new(
            if i & 1 == 0 { min.x } else { max.x },
            if i & 2 == 0 { min.y } else { max.y },
            if i & 4 == 0 { min.z } else { max.z },
        )
    })
}

/// Conservative rejection only: retain intersecting chunks even when none of
/// their corners lies inside the light's frustum. The spherical range test
/// matches illumination falloff, while the six planes match WebGPU's ZO clip.
pub fn chunk_in_light(key: ChunkKey, light: &LocalLight) -> bool {
    let min = Vec3::from_array(key.map(|v| (v * CHUNK_SIZE) as f32));
    let max = min + Vec3::splat(CHUNK_SIZE as f32);
    let closest = light.position.clamp(min, max);
    if closest.distance_squared(light.position) > LIGHT_RANGE * LIGHT_RANGE + CULL_EPSILON {
        return false;
    }
    let clip = box_corners(min, max).map(|p| light.view_proj * p.extend(1.0));
    // A linear clip-plane inequality reaches its extrema at box corners.
    // Rejecting only if all eight corners fail one plane cannot lose a caster.
    let planes: [fn(Vec4) -> f32; 6] = [
        |p| p.x + p.w,
        |p| p.w - p.x,
        |p| p.y + p.w,
        |p| p.w - p.y,
        |p| p.z,
        |p| p.w - p.z,
    ];
    !planes
        .iter()
        .any(|plane| clip.iter().all(|&p| plane(p) < -CULL_EPSILON))
}

/// Fit the entire editable volume, anchored to world coordinates so walking
/// cannot move the shadow texel grid. `sun` points from the world toward the sun.
pub fn sun_shadow_matrix(sun: Vec3) -> Mat4 {
    const PADDING: f32 = 2.0;
    let min = Vec3::new(WORLD_MIN as f32, 0.0, WORLD_MIN as f32);
    let max = Vec3::new(WORLD_MAX as f32, WORLD_HEIGHT as f32, WORLD_MAX as f32);
    let center = (min + max) * 0.5;
    let radius = (max - center).length();
    let sun = sun.try_normalize().unwrap_or(Vec3::Y);
    let up = if sun.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let view = Mat4::look_at_rh(center + sun * (radius + PADDING + 1.0), center, up);
    let mut view_min = Vec3::splat(f32::INFINITY);
    let mut view_max = Vec3::splat(f32::NEG_INFINITY);
    for corner in box_corners(min, max) {
        let p = view.transform_point3(corner);
        view_min = view_min.min(p);
        view_max = view_max.max(p);
    }
    let projection = Mat4::orthographic_rh(
        view_min.x - PADDING,
        view_max.x + PADDING,
        view_min.y - PADDING,
        view_max.y + PADDING,
        -view_max.z - PADDING,
        -view_min.z + PADDING,
    );
    projection * view
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(id: u32, position: Vec3) -> WorldObject {
        WorldObject {
            id,
            position,
            kind: ObjectKind::Light,
            rotation: 0.0,
        }
    }

    fn ndc(matrix: Mat4, position: Vec3) -> Vec3 {
        let clip = matrix * position.extend(1.0);
        clip.truncate() / clip.w
    }

    #[test]
    fn spotlight_center_edges_and_depth_match_webgpu_clip_space() {
        let light = LocalLight::from_fixture(Vec3::new(3.0, 9.0, -2.0));
        assert!((light.position.y - 8.72).abs() < 1.0e-5);
        for (depth, expected_z) in [(LIGHT_NEAR, 0.0), (LIGHT_RANGE, 1.0)] {
            let p = ndc(light.view_proj, light.position - Vec3::Y * depth);
            assert!(p.x.abs() < 1.0e-5 && p.y.abs() < 1.0e-5);
            assert!((p.z - expected_z).abs() < 1.0e-4, "{p:?}");
        }
        let depth = 4.0;
        let width = LIGHT_HALF_ANGLE.tan() * depth;
        for side in [Vec3::X, -Vec3::X, Vec3::Z, -Vec3::Z] {
            let p = ndc(
                light.view_proj,
                light.position - Vec3::Y * depth + side * width,
            );
            assert!((p.x.abs().max(p.y.abs()) - 1.0).abs() < 1.0e-5);
            assert!(p.z > 0.0 && p.z < 1.0);
        }
    }

    #[test]
    fn closest_lights_respects_budget_and_breaks_ties_by_id() {
        let mut objects = vec![
            fixture(20, Vec3::new(2.0, 2.0, 0.0)),
            fixture(10, Vec3::new(-2.0, 2.0, 0.0)),
        ];
        for i in 0..10 {
            objects.push(fixture(30 + i, Vec3::new(4.0 + i as f32, 2.0, 0.0)));
        }
        let mut tree = fixture(1, Vec3::ZERO);
        tree.kind = ObjectKind::Tree;
        objects.push(tree);
        let selected = closest_lights(&objects, Vec3::ZERO);
        assert!(selected.iter().all(Option::is_some));
        assert_eq!(selected[0].unwrap().position.x, -2.0);
        assert_eq!(selected[1].unwrap().position.x, 2.0);
        assert_eq!(selected[7].unwrap().position.x, 9.0);
        objects.reverse();
        let reversed = closest_lights(&objects, Vec3::ZERO);
        assert_eq!(
            selected.map(|l| l.unwrap().position),
            reversed.map(|l| l.unwrap().position)
        );
    }

    #[test]
    fn chunk_culling_keeps_near_plane_crossings_and_rejects_distant_or_above_chunks() {
        let light = LocalLight::from_fixture(Vec3::new(4.0, 12.28, 4.0));
        assert!(chunk_in_light([0, 1, 0], &light), "chunk contains emitter");
        assert!(chunk_in_light([0, 0, 0], &light), "floor beneath emitter");
        assert!(!chunk_in_light([0, 2, 0], &light), "entirely above emitter");
        assert!(!chunk_in_light([3, 0, 0], &light), "outside light range");
    }

    #[test]
    fn every_sampled_illuminated_point_retains_its_chunk() {
        // Exercise negative chunk coordinates and cone/range boundaries. A
        // caster touching an illuminated point must never disappear via culling.
        let light = LocalLight::from_fixture(Vec3::new(-7.7, 17.58, 3.8));
        for depth in [LIGHT_NEAR, 0.5, 1.0, 2.0, 4.0, 7.0, 11.0] {
            for x in -4..=4 {
                for z in -4..=4 {
                    let tangent = Vec3::new(x as f32 / 4.0, 0.0, z as f32 / 4.0);
                    if tangent.length_squared() > 1.0 {
                        continue;
                    }
                    let offset = -Vec3::Y * depth + tangent * (LIGHT_HALF_ANGLE.tan() * depth);
                    if offset.length_squared() > LIGHT_RANGE * LIGHT_RANGE {
                        continue;
                    }
                    let p = light.position + offset;
                    let key = p
                        .to_array()
                        .map(|v| (v.floor() as i32).div_euclid(CHUNK_SIZE));
                    assert!(chunk_in_light(key, &light), "lost {p:?} in {key:?}");
                }
            }
        }
    }

    #[test]
    fn sun_projection_contains_the_full_world_with_positive_depth_and_padding() {
        let corners = box_corners(
            Vec3::new(WORLD_MIN as f32, 0.0, WORLD_MIN as f32),
            Vec3::new(WORLD_MAX as f32, WORLD_HEIGHT as f32, WORLD_MAX as f32),
        );
        for sun in [Vec3::new(-0.44, 0.76, 0.34), Vec3::Y, -Vec3::Y, Vec3::X] {
            let matrix = sun_shadow_matrix(sun);
            for corner in corners {
                let p = ndc(matrix, corner);
                assert!(p.x > -1.0 && p.x < 1.0, "sun {sun:?}, {p:?}");
                assert!(p.y > -1.0 && p.y < 1.0, "sun {sun:?}, {p:?}");
                assert!(p.z > 0.0 && p.z < 1.0, "sun {sun:?}, {p:?}");
            }
        }
    }
}
