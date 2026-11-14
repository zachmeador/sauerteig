//! Conservative bounds tests for camera and shadow draws.

use glam::{Mat4, Vec3, Vec4};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Bounds {
    pub min: Vec3,
    pub max: Vec3,
}

impl Bounds {
    pub fn from_positions(positions: impl Iterator<Item = Vec3>) -> Self {
        positions.fold(
            Self {
                min: Vec3::splat(f32::INFINITY),
                max: Vec3::splat(f32::NEG_INFINITY),
            },
            |bounds, position| Self {
                min: bounds.min.min(position),
                max: bounds.max.max(position),
            },
        )
    }

    pub fn within_radius(self, position: Vec3, radius: f32) -> bool {
        position
            .clamp(self.min, self.max)
            .distance_squared(position)
            <= radius * radius
    }

    pub fn around(center: Vec3, radius: f32) -> Self {
        Self {
            min: center - Vec3::splat(radius),
            max: center + Vec3::splat(radius),
        }
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.min.cmple(other.max).all() && other.min.cmple(self.max).all()
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
}

pub(crate) struct Frustum {
    planes: [Vec4; 6],
}

impl Frustum {
    /// WebGPU clip coordinates have -w <= x,y <= w and 0 <= z <= w.
    pub fn new(view_projection: Mat4) -> Self {
        let [x, y, z, w] = view_projection
            .transpose()
            .to_cols_array_2d()
            .map(Vec4::from_array);
        Self {
            planes: [w + x, w - x, w + y, w - y, z, w - z],
        }
    }

    pub fn intersects(&self, bounds: Bounds) -> bool {
        self.planes.iter().all(|plane| {
            // Only reject when even the furthest corner lies outside a plane.
            // This also retains boxes crossing the view with no corner inside it.
            let corner = Vec3::select(plane.truncate().cmpge(Vec3::ZERO), bounds.max, bounds.min);
            plane.truncate().dot(corner) + plane.w >= -0.0001
        })
    }

    /// Conservative: a sphere outside two planes' shared corner region can pass.
    pub fn intersects_sphere(&self, center: Vec3, radius: f32) -> bool {
        self.planes.iter().all(|plane| {
            let normal = plane.truncate();
            normal.dot(center) + plane.w >= -radius * normal.length() - 0.0001
        })
    }
}

/// Editor flight can put the eye in a wall or on a roof boundary. Preserve the
/// two-sided view there instead of exposing holes through back-face culling.
/// 12 cm conservatively encloses the 6 cm near plane at the current FOV/aspect.
pub(super) fn camera_intersects_terrain(world: &sauerteig_core::World, eye: Vec3) -> bool {
    world.intersects_box(eye - Vec3::splat(0.12), eye + Vec3::splat(0.12))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn camera_on_roof_boundary_keeps_two_sided_view_and_outside_culls() {
        let world = sauerteig_core::World::arena();
        assert!(camera_intersects_terrain(
            &world,
            Vec3::new(16.0, 9.0, 19.0)
        ));
        assert!(!camera_intersects_terrain(
            &world,
            Vec3::new(16.0, 12.0, 19.0)
        ));
        assert!(!camera_intersects_terrain(&world, Vec3::new(0.0, 3.0, 0.0)));
    }

    #[test]
    fn perspective_culling_keeps_crossing_boxes_and_rejects_hidden_ones() {
        let frustum = Frustum::new(Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0));
        assert!(frustum.intersects(Bounds {
            min: Vec3::new(-100.0, -100.0, -10.0),
            max: Vec3::new(100.0, 100.0, -9.0)
        }));
        assert!(frustum.intersects(Bounds {
            min: Vec3::splat(-0.2),
            max: Vec3::splat(0.2)
        }));
        assert!(!frustum.intersects(Bounds {
            min: Vec3::new(-1.0, -1.0, 1.0),
            max: Vec3::new(1.0, 1.0, 2.0)
        }));
        assert!(!frustum.intersects(Bounds {
            min: Vec3::new(-1.0, -1.0, -110.0),
            max: Vec3::new(1.0, 1.0, -101.0)
        }));
        assert!(!frustum.intersects(Bounds {
            min: Vec3::new(80.0, -1.0, -10.0),
            max: Vec3::new(90.0, 1.0, -9.0)
        }));
    }

    #[test]
    fn culling_is_stable_away_from_the_origin() {
        let eye = Vec3::new(3000.0, 600.0, -4000.0);
        let matrix =
            Mat4::perspective_rh(1.0, 1.0, 0.1, 180.0) * Mat4::look_to_rh(eye, -Vec3::Z, Vec3::Y);
        let frustum = Frustum::new(matrix);
        for offset in [Vec3::ZERO, Vec3::new(10.0, 5.0, -50.0)] {
            let center = eye - Vec3::Z * 10.0 + offset;
            assert!(frustum.intersects(Bounds {
                min: center - Vec3::ONE,
                max: center + Vec3::ONE
            }));
        }
    }

    #[test]
    fn sphere_culling_matches_box_culling_for_the_same_extent() {
        let frustum = Frustum::new(Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0));
        for (center, radius, expected) in [
            (Vec3::new(0.0, 0.0, -10.0), 1.0, true),
            (Vec3::new(0.0, 0.0, 5.0), 1.0, false),
            (Vec3::new(0.0, 0.0, 5.0), 6.0, true),
            (Vec3::new(0.0, 0.0, -130.0), 20.0, false),
            (Vec3::new(40.0, 0.0, -10.0), 3.0, false),
            (Vec3::new(40.0, 0.0, -10.0), 40.0, true),
        ] {
            assert_eq!(frustum.intersects_sphere(center, radius), expected);
            if expected {
                assert!(frustum.intersects(Bounds::around(center, radius)));
            }
        }
    }

    #[test]
    fn large_bounds_intersect_light_even_when_their_center_is_far_away() {
        let bounds = Bounds {
            min: Vec3::new(-1000.0, -1.0, -1000.0),
            max: Vec3::new(1000.0, 0.0, 1000.0),
        };
        assert!(bounds.within_radius(Vec3::new(900.0, 5.0, 900.0), 12.0));
        assert!(!bounds.within_radius(Vec3::new(900.0, 20.0, 900.0), 12.0));
    }
}
