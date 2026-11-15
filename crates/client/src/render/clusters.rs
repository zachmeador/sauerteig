//! View-cluster grid math shared by the culling shader, the CPU mask builder
//! behind the capture report's lights-per-cluster counts, and the tests that
//! compare the two.
use glam::{Vec2, Vec3, Vec4};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClusterGrid {
    /// Clusters across, down and in depth.
    pub dims: [u32; 3],
    pub near: f32,
    /// The first slice ends here; the others grow exponentially up to `far`.
    pub split: f32,
    pub far: f32,
}

impl ClusterGrid {
    /// The grid's shape; `far` is the default, and the renderer sets it each
    /// frame to the view distance.
    pub const CLUSTERED: Self = Self {
        dims: [16, 9, 24],
        near: 0.06,
        split: 4.0,
        far: 180.0,
    };

    pub fn count(&self) -> usize {
        self.dims.iter().map(|&d| d as usize).product()
    }

    /// 32-bit words in each cluster's light mask for `lights` lights.
    pub fn mask_words(lights: usize) -> usize {
        lights.div_ceil(32)
    }

    pub fn ratio(&self) -> f32 {
        (self.far / self.split).powf(1.0 / (self.dims[2] - 1) as f32)
    }

    pub fn inv_log_ratio(&self) -> f32 {
        1.0 / self.ratio().ln()
    }

    /// Near and far depth of a slice, in metres along the view direction.
    pub fn slice_depths(&self, slice: u32) -> (f32, f32) {
        if slice == 0 {
            return (self.near, self.split);
        }
        let ratio = self.ratio();
        let far = if slice == self.dims[2] - 1 {
            self.far
        } else {
            self.split * ratio.powi(slice as i32)
        };
        (self.split * ratio.powi(slice as i32 - 1), far)
    }

    #[cfg(test)]
    pub fn slice_of(&self, depth: f32) -> u32 {
        if depth < self.split {
            return 0;
        }
        (((depth / self.split).ln() * self.inv_log_ratio()) as u32 + 1).min(self.dims[2] - 1)
    }

    /// The cluster holding a fragment at `pixel` of a `screen`-sized target.
    #[cfg(test)]
    pub fn index_of(&self, pixel: Vec2, screen: Vec2, depth: f32) -> u32 {
        let [gx, gy, _] = self.dims;
        let x = ((pixel.x * gx as f32 / screen.x) as u32).min(gx - 1);
        let y = ((pixel.y * gy as f32 / screen.y) as u32).min(gy - 1);
        (self.slice_of(depth) * gy + y) * gx + x
    }

    /// View-space bounds of one cluster. `scale` holds the projection's x and
    /// y scales: view x = ndc x * depth / scale.x.
    pub fn bounds(&self, scale: Vec2, index: u32) -> (Vec3, Vec3) {
        let [gx, gy, _] = self.dims;
        let (x, y, z) = (index % gx, index / gx % gy, index / (gx * gy));
        let (near, far) = self.slice_depths(z);
        let left = -1.0 + 2.0 * x as f32 / gx as f32;
        let right = -1.0 + 2.0 * (x + 1) as f32 / gx as f32;
        let top = 1.0 - 2.0 * y as f32 / gy as f32;
        let bottom = 1.0 - 2.0 * (y + 1) as f32 / gy as f32;
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for depth in [near, far] {
            for ndc in [(left, top), (right, top), (left, bottom), (right, bottom)] {
                let corner = Vec3::new(ndc.0 * depth / scale.x, ndc.1 * depth / scale.y, -depth);
                min = min.min(corner);
                max = max.max(corner);
            }
        }
        (min, max)
    }

    /// For each cluster, a bit per light whose view-space sphere (xyz, radius)
    /// reaches it, in `mask_words(lights.len())` words a cluster.
    pub fn build_masks(&self, scale: Vec2, lights: &[Vec4]) -> Vec<u32> {
        let words = Self::mask_words(lights.len());
        let mut masks = vec![0; self.count() * words];
        for cluster in 0..self.count() {
            let (min, max) = self.bounds(scale, cluster as u32);
            for (i, light) in lights.iter().enumerate() {
                if sphere_reaches(min, max, light.truncate(), light.w) {
                    masks[cluster * words + i / 32] |= 1 << (i % 32);
                }
            }
        }
        masks
    }
}

pub fn sphere_reaches(min: Vec3, max: Vec3, center: Vec3, radius: f32) -> bool {
    center.clamp(min, max).distance_squared(center) <= radius * radius
}

/// Culling radius for a light's range, slightly inflated so rounding at slice
/// and tile boundaries cannot drop a light that reaches a fragment.
pub fn cull_radius(range: f32) -> f32 {
    range * 1.002 + 0.02
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Mat4;

    fn scale_of(projection: Mat4) -> Vec2 {
        Vec2::new(projection.x_axis.x, projection.y_axis.y)
    }

    #[test]
    fn slices_tile_the_depth_range_without_gaps() {
        for grid in [
            ClusterGrid::CLUSTERED,
            ClusterGrid {
                far: 7_232.0,
                ..ClusterGrid::CLUSTERED
            },
        ] {
            let mut previous = grid.near;
            for slice in 0..grid.dims[2] {
                let (near, far) = grid.slice_depths(slice);
                assert!((near - previous).abs() < 1.0e-3 * previous, "{slice}");
                assert!(far > near);
                previous = far;
            }
            assert!((previous - grid.far).abs() < 1.0e-3);
            assert_eq!(grid.slice_depths(0), (0.06, 4.0));
        }
        assert_eq!(ClusterGrid::CLUSTERED.count(), 3456);
        assert_eq!(ClusterGrid::mask_words(256), 8);
        assert_eq!(ClusterGrid::mask_words(257), 9);
        assert_eq!(ClusterGrid::mask_words(1000), 32);
        assert_eq!(ClusterGrid::mask_words(0), 0);
    }

    #[test]
    fn every_depth_falls_in_the_slice_whose_range_contains_it() {
        let grid = ClusterGrid::CLUSTERED;
        for i in 0..4000 {
            let depth = 0.06 + i as f32 * 0.045;
            let slice = grid.slice_of(depth);
            let (near, far) = grid.slice_depths(slice);
            // The last slice also takes everything past the far plane.
            let slack = 1.0e-4 * depth;
            assert!(
                depth >= near - slack && (depth <= far + slack || slice == grid.dims[2] - 1),
                "depth {depth} in slice {slice} = {near}..{far}"
            );
        }
        assert_eq!(grid.slice_of(0.0), 0);
        assert_eq!(grid.slice_of(1.0e6), grid.dims[2] - 1);
    }

    #[test]
    fn a_fragment_lies_inside_its_clusters_bounds() {
        let projection = Mat4::perspective_rh(78.0_f32.to_radians(), 16.0 / 9.0, 0.06, 180.0);
        let scale = scale_of(projection);
        let screen = Vec2::new(1920.0, 1080.0);
        for grid in [ClusterGrid::CLUSTERED] {
            for px in (0..1920).step_by(37) {
                for py in (0..1080).step_by(41) {
                    for depth in [0.07, 0.5, 3.9, 4.1, 17.3, 90.0, 179.0] {
                        let pixel = Vec2::new(px as f32 + 0.5, py as f32 + 0.5);
                        let cluster = grid.index_of(pixel, screen, depth);
                        let (min, max) = grid.bounds(scale, cluster);
                        let ndc = Vec2::new(
                            2.0 * pixel.x / screen.x - 1.0,
                            1.0 - 2.0 * pixel.y / screen.y,
                        );
                        let point =
                            Vec3::new(ndc.x * depth / scale.x, ndc.y * depth / scale.y, -depth);
                        let slack = Vec3::splat(1.0e-3 * (1.0 + depth));
                        assert!(
                            point.cmpge(min - slack).all() && point.cmple(max + slack).all(),
                            "{pixel:?} {depth} {cluster} {min:?} {max:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn masks_hold_every_light_that_reaches_a_sampled_fragment() {
        let projection = Mat4::perspective_rh(78.0_f32.to_radians(), 16.0 / 9.0, 0.06, 180.0);
        let scale = scale_of(projection);
        let screen = Vec2::new(1920.0, 1080.0);
        // A deterministic scatter, including lights behind the camera.
        let mut state = 0x2545_f491_u32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as f32 / u32::MAX as f32
        };
        for grid in [ClusterGrid::CLUSTERED] {
            let lights: Vec<Vec4> = (0..1000)
                .map(|_| {
                    Vec4::new(
                        next() * 80.0 - 40.0,
                        next() * 30.0 - 15.0,
                        next() * -100.0 + 10.0,
                        cull_radius(2.0 + next() * 14.0),
                    )
                })
                .collect();
            let masks = grid.build_masks(scale, &lights);
            let words = ClusterGrid::mask_words(lights.len());
            for _ in 0..4000 {
                let pixel = Vec2::new(next() * 1919.0 + 0.5, next() * 1079.0 + 0.5);
                let depth = 0.06 + next() * 120.0;
                let ndc = Vec2::new(
                    2.0 * pixel.x / screen.x - 1.0,
                    1.0 - 2.0 * pixel.y / screen.y,
                );
                let point = Vec3::new(ndc.x * depth / scale.x, ndc.y * depth / scale.y, -depth);
                let cluster = grid.index_of(pixel, screen, depth) as usize;
                for (i, light) in lights.iter().enumerate() {
                    let inside = point.distance(light.truncate()) <= light.w - 0.02;
                    let bit = masks[cluster * words + i / 32] >> (i % 32) & 1;
                    assert!(!inside || bit == 1, "light {i} dropped for {point:?}");
                }
            }
        }
    }

    #[test]
    fn a_light_reaches_only_nearby_clusters() {
        let projection = Mat4::perspective_rh(78.0_f32.to_radians(), 16.0 / 9.0, 0.06, 180.0);
        let grid = ClusterGrid::CLUSTERED;
        let light = Vec4::new(0.0, 0.0, -50.0, cull_radius(2.0));
        let masks = grid.build_masks(scale_of(projection), &[light]);
        let hit: Vec<_> = (0..grid.count()).filter(|&c| masks[c] & 1 == 1).collect();
        assert!(!hit.is_empty());
        assert!(hit.len() < grid.count() / 20, "{} clusters", hit.len());
        // Fixed light order: the masks of an empty list are empty.
        assert!(grid
            .build_masks(scale_of(projection), &[])
            .iter()
            .all(|&w| w == 0));
    }
}
