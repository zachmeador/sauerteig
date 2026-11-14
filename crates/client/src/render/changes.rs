//! Finding where a rebuilt mesh really changed. A one-block edit rebuilds a
//! whole chunk, but only a few of its triangles differ; the sun's shadow maps
//! redraw only the texels those triangles touch. This works on the vertices a
//! mesh is built from, whatever keys or chunk sizes the mesher uses, so it
//! survives a change of how the world is divided into meshes.
use super::{culling::Bounds, GpuVertex};
use glam::Vec3;

/// Cells along each side of the grid laid over a mesh's own bounds.
const SIDE: usize = 4;
const CELLS: usize = SIDE * SIDE * SIDE;
/// How far a vertex may sit outside the cube before it counts as outside, in
/// metres: float error in positions is a few millimetres at most.
const SLACK: f32 = 0.01;

/// A fingerprint of a mesh by region: for each cell of a 4 × 4 × 4 grid over a
/// square block of space, a number that changes when any triangle reaching that
/// cell changes. 512 bytes a mesh.
#[derive(Clone, Copy)]
pub(super) struct Fingerprint {
    cells: [u64; CELLS],
    /// The same for triangles that reach outside the cube, with the bounds of
    /// those triangles, which no cell can describe.
    spill: u64,
    spill_min: Vec3,
    spill_max: Vec3,
}

impl PartialEq for Fingerprint {
    fn eq(&self, other: &Self) -> bool {
        self.cells == other.cells && self.spill == other.spill
    }
}

impl Fingerprint {
    pub const EMPTY: Self = Self {
        cells: [0; CELLS],
        spill: 0,
        spill_min: Vec3::INFINITY,
        spill_max: Vec3::NEG_INFINITY,
    };

    /// Fingerprint triangles inside the cube of `size` metres at `origin`.
    /// Triangles that reach outside it also count in the cells at its edge.
    pub fn of(origin: Vec3, size: f32, vertices: &[GpuVertex], indices: &[u32]) -> Self {
        let mut print = Self::EMPTY;
        let cells = &mut print.cells;
        let scale = SIDE as f32 / size;
        let cell_range = |low: f32, high: f32, origin: f32| {
            let clamp = |v: f32| (((v - origin) * scale).floor().max(0.0) as usize).min(SIDE - 1);
            clamp(low)..=clamp(high)
        };
        for triangle in indices.as_chunks::<3>().0 {
            let [a, b, c] = triangle.map(|i| Vec3::from_array(vertices[i as usize].position));
            let (low, high) = (a.min(b).min(c), a.max(b).max(c));
            let hash = mix(a) ^ mix(b).rotate_left(21) ^ mix(c).rotate_left(42);
            if low.cmplt(origin - Vec3::splat(SLACK)).any()
                || high.cmpgt(origin + Vec3::splat(size + SLACK)).any()
            {
                print.spill = print.spill.wrapping_add(hash);
                print.spill_min = print.spill_min.min(low);
                print.spill_max = print.spill_max.max(high);
            }
            for x in cell_range(low.x, high.x, origin.x) {
                for y in cell_range(low.y, high.y, origin.y) {
                    for z in cell_range(low.z, high.z, origin.z) {
                        let cell = &mut cells[(x * SIDE + y) * SIDE + z];
                        *cell = cell.wrapping_add(hash);
                    }
                }
            }
        }
        print
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::EMPTY
    }

    /// The region where this fingerprint and an older one differ, as bounds
    /// inside the cube of `size` metres at `origin`; `None` when they agree.
    pub fn changed_since(&self, old: &Self, origin: Vec3, size: f32) -> Option<Bounds> {
        let cell = size / SIDE as f32;
        let mut changed: Option<Bounds> = None;
        if self.spill != old.spill {
            for (min, max) in [
                (self.spill_min, self.spill_max),
                (old.spill_min, old.spill_max),
            ] {
                if min.cmple(max).all() {
                    let bounds = Bounds { min, max };
                    changed = Some(changed.map_or(bounds, |c| c.union(bounds)));
                }
            }
        }
        for index in 0..CELLS {
            if self.cells[index] == old.cells[index] {
                continue;
            }
            let (x, y, z) = (index / (SIDE * SIDE), index / SIDE % SIDE, index % SIDE);
            let min = origin + Vec3::new(x as f32, y as f32, z as f32) * cell;
            let bounds = Bounds {
                min,
                max: min + Vec3::splat(cell),
            };
            changed = Some(changed.map_or(bounds, |c| c.union(bounds)));
        }
        changed
    }
}

/// A well-mixed 64-bit number for a position: adding these over a cell's
/// triangles is the same in any order, and a different triangle gives a
/// different sum.
fn mix(p: Vec3) -> u64 {
    let mut h = 0x9e37_79b9_7f4a_7c15_u64;
    for bits in [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()] {
        h = (h ^ u64::from(bits)).wrapping_mul(0xff51_afd7_ed55_8ccd);
        h ^= h >> 32;
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vertex(x: f32, y: f32, z: f32) -> GpuVertex {
        GpuVertex {
            position: [x, y, z],
            normal: [0.0, 1.0, 0.0],
            color: [1.0; 3],
            emission: 0.0,
            uv: [0.0; 2],
            material: 0,
        }
    }

    /// A flat square of two triangles whose corner is at x, z, 1 m up.
    fn square(x: f32, z: f32, size: f32) -> (Vec<GpuVertex>, Vec<u32>) {
        (
            vec![
                vertex(x, 1.0, z),
                vertex(x + size, 1.0, z),
                vertex(x + size, 1.0, z + size),
                vertex(x, 1.0, z + size),
            ],
            vec![0, 1, 2, 0, 2, 3],
        )
    }

    fn merged(parts: &[(Vec<GpuVertex>, Vec<u32>)]) -> (Vec<GpuVertex>, Vec<u32>) {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for (v, i) in parts {
            let base = vertices.len() as u32;
            vertices.extend_from_slice(v);
            indices.extend(i.iter().map(|i| i + base));
        }
        (vertices, indices)
    }

    #[test]
    fn identical_meshes_have_no_changed_region_in_any_triangle_order() {
        let origin = Vec3::new(8.0, 0.0, 16.0);
        let a = merged(&[square(8.0, 16.0, 1.0), square(12.0, 20.0, 1.0)]);
        let b = merged(&[square(12.0, 20.0, 1.0), square(8.0, 16.0, 1.0)]);
        let (fa, fb) = (
            Fingerprint::of(origin, 8.0, &a.0, &a.1),
            Fingerprint::of(origin, 8.0, &b.0, &b.1),
        );
        assert!(fa == fb);
        assert_eq!(fa.changed_since(&fb, origin, 8.0).map(|_| ()), None);
    }

    #[test]
    fn a_block_added_to_a_chunk_changes_only_the_cells_it_reaches() {
        let origin = Vec3::new(8.0, 0.0, 16.0);
        let floor = square(8.0, 16.0, 8.0);
        let before = Fingerprint::of(origin, 8.0, &floor.0, &floor.1);
        let with_block = merged(&[floor, square(13.0, 21.0, 0.5)]);
        let after = Fingerprint::of(origin, 8.0, &with_block.0, &with_block.1);
        let changed = after.changed_since(&before, origin, 8.0).unwrap();
        // The block sits in one 2 m cell of the chunk's 8 m. (A triangle that ends
        // exactly on a cell's edge marks the next cell too, which is harmless.)
        assert_eq!(changed.min, Vec3::new(12.0, 0.0, 20.0));
        assert_eq!(changed.max, Vec3::new(14.0, 2.0, 22.0));
        // Taking it away again changes the same place.
        let back = before.changed_since(&after, origin, 8.0).unwrap();
        assert_eq!((back.min, back.max), (changed.min, changed.max));
    }

    #[test]
    fn a_triangle_spanning_cells_marks_every_cell_it_reaches() {
        let origin = Vec3::ZERO;
        let big = square(0.0, 0.0, 8.0);
        let nudged = square(0.0, 0.0, 7.5);
        let a = Fingerprint::of(origin, 8.0, &big.0, &big.1);
        let b = Fingerprint::of(origin, 8.0, &nudged.0, &nudged.1);
        let changed = b.changed_since(&a, origin, 8.0).unwrap();
        // A resized quad changes every cell its old or new shape covers.
        assert_eq!(changed.min.x, 0.0);
        assert_eq!(changed.max.x, 8.0);
        assert_eq!(changed.max.z, 8.0);
    }

    #[test]
    fn an_empty_mesh_is_the_empty_fingerprint_and_a_new_mesh_changes_everything_it_has() {
        let origin = Vec3::ZERO;
        assert!(Fingerprint::of(origin, 8.0, &[], &[]).is_empty());
        let one = square(1.0, 1.0, 1.0);
        let fresh = Fingerprint::of(origin, 8.0, &one.0, &one.1);
        assert!(!fresh.is_empty());
        let changed = fresh
            .changed_since(&Fingerprint::EMPTY, origin, 8.0)
            .unwrap();
        assert!(changed.min.x <= 1.0 && changed.max.x >= 2.0);
        // Triangles outside the cube are not lost: a change out there is
        // reported where it is.
        let outside = square(-5.0, 20.0, 1.0);
        let spilled = Fingerprint::of(origin, 8.0, &outside.0, &outside.1);
        assert!(!spilled.is_empty());
        let changed = spilled
            .changed_since(&Fingerprint::EMPTY, origin, 8.0)
            .unwrap();
        assert!(changed.min.x <= -5.0 && changed.max.z >= 21.0);
    }
}
