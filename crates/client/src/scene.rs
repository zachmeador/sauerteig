use crate::render::GpuVertex;
use glam::{Mat3, Vec3};
use sauerteig_core::{ObjectKind, Player, WorldObject};

pub const LIGHT_GUIDE_RADIUS: f32 = 0.18;

#[derive(Default)]
pub struct Geometry {
    pub vertices: Vec<GpuVertex>,
    pub indices: Vec<u32>,
    /// Leading indices containing physical geometry that casts dynamic shadows.
    pub shadow_index_count: u32,
}
impl Geometry {
    pub fn triangle(&mut self, a: Vec3, b: Vec3, c: Vec3, color: [f32; 3], emission: f32) {
        let normal = (b - a).cross(c - a).normalize_or_zero().to_array();
        let i = self.vertices.len() as u32;
        for p in [a, b, c] {
            self.vertices.push(GpuVertex {
                position: p.to_array(),
                normal,
                color,
                emission,
                uv: [0.0; 2],
                material: u32::MAX,
            });
        }
        self.indices.extend([i, i + 1, i + 2]);
    }
    pub fn cuboid(&mut self, center: Vec3, size: Vec3, yaw: f32, color: [f32; 3], emission: f32) {
        let r = Mat3::from_rotation_y(yaw);
        let h = size * 0.5;
        let pts = [
            [-1., -1., -1.],
            [1., -1., -1.],
            [1., 1., -1.],
            [-1., 1., -1.],
            [-1., -1., 1.],
            [1., -1., 1.],
            [1., 1., 1.],
            [-1., 1., 1.],
        ]
        .map(|v| center + r * (Vec3::from_array(v) * h));
        for [a, b, c, d] in [
            [0, 3, 2, 1],
            [4, 5, 6, 7],
            [0, 4, 7, 3],
            [1, 2, 6, 5],
            [0, 1, 5, 4],
            [3, 7, 6, 2],
        ] {
            self.triangle(pts[a], pts[b], pts[c], color, emission);
            self.triangle(pts[a], pts[c], pts[d], color, emission);
        }
    }
    pub fn beam(&mut self, a: Vec3, b: Vec3, width: f32, color: [f32; 3], emission: f32) {
        let direction = (b - a).normalize_or_zero();
        let axis = if direction.y.abs() > 0.9 {
            Vec3::X
        } else {
            Vec3::Y
        };
        let side = direction.cross(axis).normalize_or_zero() * width * 0.5;
        let up = direction.cross(side).normalize_or_zero() * width * 0.5;
        let p = [
            a - side - up,
            a + side - up,
            a + side + up,
            a - side + up,
            b - side - up,
            b + side - up,
            b + side + up,
            b - side + up,
        ];
        for [i, j, k, l] in [
            [0, 3, 2, 1],
            [4, 5, 6, 7],
            [0, 4, 7, 3],
            [1, 2, 6, 5],
            [0, 1, 5, 4],
            [3, 7, 6, 2],
        ] {
            self.triangle(p[i], p[j], p[k], color, emission);
            self.triangle(p[i], p[k], p[l], color, emission);
        }
    }
    pub fn outline(&mut self, min: Vec3, size: f32, color: [f32; 3]) {
        let p = [
            Vec3::ZERO,
            Vec3::X,
            Vec3::X + Vec3::Y,
            Vec3::Y,
            Vec3::Z,
            Vec3::X + Vec3::Z,
            Vec3::ONE,
            Vec3::Y + Vec3::Z,
        ]
        .map(|v| min + v * size);
        for (a, b) in [
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 0),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 4),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ] {
            self.beam(p[a], p[b], 0.024, color, 1.0);
        }
    }
    // Keep the primitive's independent dimensions and appearance explicit at
    // its two call sites rather than introducing a one-off parameter struct.
    #[allow(clippy::too_many_arguments)]
    fn ring(
        &mut self,
        base: Vec3,
        r0: f32,
        r1: f32,
        height: f32,
        sides: usize,
        phase: f32,
        color: [f32; 3],
    ) {
        for i in 0..sides {
            let a = phase + i as f32 * std::f32::consts::TAU / sides as f32;
            let b = phase + (i + 1) as f32 * std::f32::consts::TAU / sides as f32;
            let noise = 1.0 + 0.09 * (i as f32 * 13.7 + base.y * 3.3).sin();
            let p0 = base + Vec3::new(a.cos() * r0 * noise, 0.0, a.sin() * r0 * noise);
            let p1 = base + Vec3::new(b.cos() * r0, 0.0, b.sin() * r0);
            let p2 = base + Vec3::new(b.cos() * r1, height, b.sin() * r1);
            let p3 = base + Vec3::new(a.cos() * r1, height, a.sin() * r1);
            let shade = 0.90 + 0.10 * (i as f32 * 3.23).cos();
            let c = color.map(|v| v * shade);
            self.triangle(p0, p2, p1, c, 0.0);
            self.triangle(p0, p3, p2, c, 0.0);
            self.triangle(base, p1, p0, c.map(|v| v * 0.6), 0.0);
        }
    }
    fn tree(&mut self, o: &WorldObject) {
        let p = o.position;
        self.ring(p, 0.21, 0.08, 6.4, 10, o.rotation, [0.24, 0.17, 0.11]);
        for i in 0..7 {
            let h = 1.3 + i as f32 * 0.66;
            let radius = 1.8 - i as f32 * 0.21;
            let offset = Vec3::new(
                (i as f32 * 2.3).sin() * 0.10,
                h,
                (i as f32 * 1.7).cos() * 0.1,
            );
            self.ring(
                p + offset,
                radius,
                radius * 0.25,
                1.15,
                18,
                o.rotation + i as f32 * 0.37,
                [
                    0.16 + i as f32 * 0.005,
                    0.27 + i as f32 * 0.008,
                    0.19 + i as f32 * 0.004,
                ],
            );
        }
    }
    pub fn objects(objects: &[WorldObject]) -> Self {
        let mut mesh = Self::default();
        for object in objects {
            let first_vertex = mesh.vertices.len();
            // Author procedural parts around their shared pivot, then move the
            // whole model. This keeps offsets and normals in the same yaw frame
            // and preserves small details far from the world origin.
            let o = WorldObject {
                position: Vec3::ZERO,
                rotation: 0.0,
                ..object.clone()
            };
            match o.kind {
                ObjectKind::Tree => mesh.tree(&o),
                ObjectKind::WallLamp
                | ObjectKind::HangingLamp
                | ObjectKind::UtilityBox
                | ObjectKind::Pipe
                | ObjectKind::Rock
                // Light nodes have no physical model. Their editor guides are
                // appended to dynamic geometry after its shadow-casting prefix.
                | ObjectKind::Light => {}
                ObjectKind::Spawn => {
                    mesh.cuboid(
                        o.position + Vec3::Y * 0.025,
                        Vec3::new(0.85, 0.05, 0.85),
                        0.0,
                        [0.08, 0.09, 0.10],
                        0.0,
                    );
                    for z in [-0.32, 0.32] {
                        mesh.cuboid(
                            o.position + Vec3::new(0.0, 0.055, z),
                            Vec3::new(0.56, 0.02, 0.045),
                            0.0,
                            [0.43, 0.34, 0.16],
                            0.12,
                        );
                    }
                }
            }
            let rotation = Mat3::from_rotation_y(object.rotation);
            for vertex in &mut mesh.vertices[first_vertex..] {
                vertex.position =
                    (object.position + rotation * Vec3::from_array(vertex.position)).to_array();
                vertex.normal = (rotation * Vec3::from_array(vertex.normal)).to_array();
            }
        }
        mesh
    }
    pub fn light_guide(&mut self, position: Vec3) {
        let color = [1.0, 0.73, 0.22];
        let radius = LIGHT_GUIDE_RADIUS * 0.55;
        let points = [Vec3::X, Vec3::Z, -Vec3::X, -Vec3::Z].map(|v| position + v * radius);
        for i in 0..4 {
            let next = (i + 1) % 4;
            self.triangle(
                position + Vec3::Y * radius,
                points[next],
                points[i],
                color,
                1.0,
            );
            self.triangle(
                position - Vec3::Y * radius,
                points[i],
                points[next],
                color,
                1.0,
            );
        }
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            self.beam(
                position - axis * LIGHT_GUIDE_RADIUS,
                position + axis * LIGHT_GUIDE_RADIUS,
                0.016,
                color,
                1.0,
            );
        }
    }
    pub fn player(&mut self, player: &Player, time: f32) {
        let p = player.position;
        let yaw = -player.yaw;
        let armor = if player.editing {
            [0.16, 0.34, 0.30]
        } else {
            [0.40, 0.18, 0.08]
        };
        let r = Mat3::from_rotation_y(yaw);
        let stride = (time * 12.0).sin() * (player.velocity.xz().length().min(6.0) / 6.0) * 0.12;
        for sign in [-1.0, 1.0] {
            self.cuboid(
                p + r * Vec3::new(sign * 0.16, 0.35, stride * sign),
                Vec3::new(0.22, 0.67, 0.26),
                yaw,
                [0.055, 0.070, 0.080],
                0.0,
            );
            self.cuboid(
                p + r * Vec3::new(sign * 0.37, 1.02, 0.0),
                Vec3::new(0.17, 0.56, 0.24),
                yaw,
                armor,
                0.0,
            );
        }
        self.cuboid(
            p + Vec3::Y * 1.03,
            Vec3::new(0.57, 0.67, 0.35),
            yaw,
            armor,
            0.0,
        );
        self.cuboid(
            p + Vec3::Y * 1.56,
            Vec3::new(0.37, 0.34, 0.35),
            yaw,
            [0.080, 0.095, 0.10],
            0.0,
        );
        self.cuboid(
            p + r * Vec3::new(0.0, 1.58, -0.183),
            Vec3::new(0.30, 0.075, 0.025),
            yaw,
            [0.90, 0.48, 0.15],
            0.9,
        );
    }
    pub fn player_guide(&mut self, player: &Player) {
        if player.editing {
            let p = player.position;
            self.outline(p - Vec3::new(0.45, 0.0, 0.45), 0.9, [0.20, 0.75, 0.67]);
        }
    }
}
use glam::Vec3Swizzles;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_nodes_have_no_physical_mesh() {
        assert!(crate::props::definition(ObjectKind::Light).is_none());
        let node = WorldObject {
            id: 1,
            kind: ObjectKind::Light,
            position: Vec3::new(17.0, 8.0, -20.0),
            rotation: 0.7,
        };
        let mesh = Geometry::objects(std::slice::from_ref(&node));
        assert!(mesh.vertices.is_empty());
        assert!(mesh.indices.is_empty());

        let spawn = WorldObject {
            kind: ObjectKind::Spawn,
            ..node.clone()
        };
        let spawn_mesh = Geometry::objects(std::slice::from_ref(&spawn));
        let combined = Geometry::objects(&[node, spawn]);
        assert!(!spawn_mesh.indices.is_empty());
        assert_eq!(combined.vertices.len(), spawn_mesh.vertices.len());
        assert_eq!(combined.indices, spawn_mesh.indices);
    }

    #[test]
    fn light_guide_is_small_and_centered_on_the_stored_position() {
        let position = Vec3::new(4321.5, 927.25, -3210.5);
        let mut mesh = Geometry::default();
        mesh.light_guide(position);
        assert!(!mesh.indices.is_empty());
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for vertex in &mesh.vertices {
            let p = Vec3::from_array(vertex.position);
            min = min.min(p);
            max = max.max(p);
            assert!((p - position).abs().max_element() <= LIGHT_GUIDE_RADIUS + 0.001);
            assert_eq!(vertex.emission, 1.0);
        }
        assert!(((min + max) * 0.5).abs_diff_eq(position, 0.001));
    }
}
