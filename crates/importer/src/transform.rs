use crate::{Cube, ImportOptions};
use sauerteig_core::{
    FineBox, Vec3, WorldBounds, FINE_UNITS, MAX_FRAGMENT_SOURCE_SIZE, WORLD_HEIGHT, WORLD_MAX,
    WORLD_MIN,
};

/// One exact scale and reflection for geometry, entities, and crop reporting.
/// Sauerbraten's world is Z-up and left-handed, so the right-handed Y-up
/// import is a reflection: (x, y, z) -> (-x, z, -y). Source north (+y) stays
/// Sauerteig's -Z and up stays up, and east and west trade places.
pub(crate) struct ImportTransform {
    units_per_metre: u32,
    center_fine: [i32; 3],
    /// The centre as asked for, before a crop moves it onto the metre grid.
    pub requested_center: [f64; 3],
    pub center: [f64; 3],
    pub source_region: [[f64; 3]; 2],
    pub bounds: WorldBounds,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegionOverlap {
    Outside,
    Clipped,
    Inside,
}

impl ImportTransform {
    pub fn new(world_size: u32, options: &ImportOptions) -> Result<Self, String> {
        let units = options.units_per_metre;
        if !units.is_power_of_two() || units > 1024 {
            return Err("units-per-metre must be a power of two from 1 through 1024".into());
        }
        let requested = options.center.unwrap_or([f64::from(world_size) / 2.0; 3]);
        let factor = f64::from(FINE_UNITS) / f64::from(units);
        let mut center_fine = [0; 3];
        for (axis, value) in requested.into_iter().enumerate() {
            let scaled = value * factor;
            if !scaled.is_finite() || scaled.fract().abs() > 1e-8 || scaled.abs() > 8_388_608.0 {
                return Err("center must align to Sauerteig's eighth-metre grid and be within import coordinate limits".into());
            }
            center_fine[axis] = scaled as i32;
        }
        let mut center = requested;
        if options.crop {
            // The crop's edges are whole metres, and the tree can only cut a
            // leaf where its grid allows: an edge off the one-metre grid
            // costs leaves down to an eighth of a metre (nub1 around its
            // spawn: 714,000 leaves instead of 42,000). So the centre moves
            // to the nearest whole metre, at most half a metre.
            for axis in 0..3 {
                center_fine[axis] =
                    (center_fine[axis] + FINE_UNITS / 2).div_euclid(FINE_UNITS) * FINE_UNITS;
                center[axis] = f64::from(center_fine[axis]) / factor;
            }
        }
        let source_units_per_metre = f64::from(units);
        let half_height = f64::from(WORLD_HEIGHT) / 2.0;
        let crop_region = [
            [
                center[0] - f64::from(WORLD_MAX) * source_units_per_metre,
                center[1] - f64::from(WORLD_MAX) * source_units_per_metre,
                center[2] - half_height * source_units_per_metre,
            ],
            [
                center[0] - f64::from(WORLD_MIN) * source_units_per_metre,
                center[1] - f64::from(WORLD_MIN) * source_units_per_metre,
                center[2] + half_height * source_units_per_metre,
            ],
        ];
        let source_region = if options.crop {
            crop_region
        } else {
            [[0.0; 3], [f64::from(world_size); 3]]
        };
        let source_size = f64::from(world_size) / source_units_per_metre;
        let min = [
            center[0] / source_units_per_metre - source_size,
            half_height - center[2] / source_units_per_metre,
            center[1] / source_units_per_metre - source_size,
        ];
        let max = min.map(|value| value + source_size);
        let bounds = if options.crop {
            WorldBounds::default()
        } else {
            WorldBounds {
                min: min.map(|value| value.floor() as i32),
                max: max.map(|value| value.ceil() as i32),
            }
        };
        Ok(Self {
            units_per_metre: units,
            center_fine,
            requested_center: requested,
            center,
            source_region,
            bounds,
        })
    }

    /// Classify before grid validation so explicit cropping can omit distant,
    /// finer-than-supported leaves without rounding any retained geometry.
    pub fn overlap(&self, cube: &Cube) -> RegionOverlap {
        let min = cube.origin.map(f64::from);
        let max = min.map(|value| value + f64::from(cube.size));
        if (0..3).any(|axis| {
            max[axis] <= self.source_region[0][axis] || min[axis] >= self.source_region[1][axis]
        }) {
            RegionOverlap::Outside
        } else if (0..3).any(|axis| {
            min[axis] < self.source_region[0][axis] || max[axis] > self.source_region[1][axis]
        }) {
            RegionOverlap::Clipped
        } else {
            RegionOverlap::Inside
        }
    }

    /// A source size in eighth-metres, if it falls on the import grid.
    pub fn fine_size(&self, size: u32) -> Option<u32> {
        let numerator = size.checked_mul(FINE_UNITS as u32)?;
        (numerator % self.units_per_metre == 0).then(|| numerator / self.units_per_metre)
    }

    /// Return the authored cube bounds in integer eighth-metres, before cropping.
    pub fn cube_bounds(&self, cube: &Cube) -> Result<FineBox, String> {
        let numerator = cube
            .size
            .checked_mul(FINE_UNITS as u32)
            .ok_or("source scale overflow")?;
        if numerator % self.units_per_metre != 0 {
            return Err(format!(
                "leaf at {:?} is finer than 1/8m at this scale; choose fewer units per metre",
                cube.origin
            ));
        }
        let size = numerator / self.units_per_metre;
        if size == 0 || size > MAX_FRAGMENT_SOURCE_SIZE {
            return Err(format!(
                "source leaf at {:?} exceeds supported authored size ({} eighths); choose a larger units-per-metre value",
                cube.origin, MAX_FRAGMENT_SOURCE_SIZE
            ));
        }
        let mut scaled = [0; 3];
        for (axis, value) in cube.origin.into_iter().enumerate() {
            let numerator = value
                .checked_mul(FINE_UNITS)
                .ok_or("source origin overflow")?;
            if numerator % self.units_per_metre as i32 != 0 {
                return Err(format!(
                    "leaf at {:?} cannot align to the 1/8m grid at this scale",
                    cube.origin
                ));
            }
            scaled[axis] = numerator / self.units_per_metre as i32 - self.center_fine[axis];
        }
        let min = [
            -scaled[0] - size as i32,
            scaled[2] + WORLD_HEIGHT * FINE_UNITS / 2,
            -scaled[1] - size as i32,
        ];
        Ok(FineBox {
            min,
            max: min.map(|value| value + size as i32),
        })
    }

    /// The source cube's minimum corner and size in eighth-metres: the grid
    /// the imported octree's leaves sit on.
    pub fn source_grid(&self, world_size: u32) -> Result<FineBox, String> {
        let numerator = world_size
            .checked_mul(FINE_UNITS as u32)
            .ok_or("source scale overflow")?;
        if numerator % self.units_per_metre != 0 {
            return Err("the map is finer than 1/8m at this scale".into());
        }
        let size = i32::try_from(numerator / self.units_per_metre).map_err(|e| e.to_string())?;
        let min = [
            self.center_fine[0] - size,
            WORLD_HEIGHT * FINE_UNITS / 2 - self.center_fine[2],
            self.center_fine[1] - size,
        ];
        Ok(FineBox {
            min,
            max: min.map(|v| v + size),
        })
    }

    pub fn position(&self, source: [f32; 3]) -> Vec3 {
        let relative: [f32; 3] = std::array::from_fn(|axis| {
            ((f64::from(source[axis]) - self.center[axis]) / f64::from(self.units_per_metre)) as f32
        });
        Vec3::new(
            -relative[0],
            relative[2] + WORLD_HEIGHT as f32 / 2.0,
            -relative[1],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities_and_cubes_share_the_same_reflection_scale_and_center() {
        let options = ImportOptions {
            units_per_metre: 16,
            center: Some([128.0, 256.0, 384.0]),
            crop: true,
            ..ImportOptions::default()
        };
        let transform = ImportTransform::new(1024, &options).unwrap();
        let cube = Cube {
            origin: [144, 224, 400],
            size: 16,
            kind: 2,
            edges: [0x80; 12],
            textures: [0; 6],
            volume_material: 0,
        };
        let bounds = transform.cube_bounds(&cube).unwrap();
        assert_eq!(bounds.min, [-16, 200, 8]);
        assert_eq!(bounds.max, [-8, 208, 16]);
        for x in [144.0, 160.0] {
            for y in [224.0, 240.0] {
                for z in [400.0, 416.0] {
                    let position = transform.position([x, y, z]);
                    assert!(position.cmpge(bounds.world_min()).all());
                    assert!(position.cmple(bounds.world_max()).all());
                }
            }
        }
        let [min, max] = transform.source_region;
        assert_eq!(
            transform.position(min.map(|value| value as f32)),
            Vec3::new(64.0, 0.0, 64.0)
        );
        assert_eq!(
            transform.position(max.map(|value| value as f32)),
            Vec3::new(-64.0, 48.0, -64.0)
        );
    }

    #[test]
    fn the_mapping_is_a_reflection_that_keeps_north_and_up() {
        let transform = ImportTransform::new(1024, &ImportOptions::default()).unwrap();
        let origin = transform.position([512.0, 512.0, 512.0]);
        let step = |axis: usize| {
            let mut point = [512.0; 3];
            point[axis] += 8.0;
            transform.position(point) - origin
        };
        let (east, north, up) = (step(0), step(1), step(2));
        // Sauerteig's Y is up and -Z is forward; source +y is north.
        assert_eq!(up, Vec3::Y);
        assert_eq!(north, -Vec3::Z);
        assert_eq!(east, -Vec3::X);
        // A rotation would keep the triple product positive.
        assert!(east.cross(north).dot(up) < 0.0);
    }
}
