//! Embedded, offline-cooked CC0 props. Ordinary builds do not contact asset services.
//! The source manifest and reproducible cooker live in assets/props and scripts.

#[derive(Clone, Copy, Debug)]
pub struct PropVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    /// Global texture-array layer; terrain reserves layers 0 through 7.
    pub material: u32,
    pub emission: f32,
}

pub struct PropMesh {
    pub vertices: Vec<PropVertex>,
    pub indices: Vec<u32>,
}

pub struct PropAsset {
    pub key: &'static str,
    pub name: &'static str,
    pub near: PropMesh,
    pub distant: PropMesh,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    pub lod_distance: f32,
}

pub struct PropTexture {
    pub key: &'static str,
    pub name: &'static str,
    /// PNG: sRGB base color in RGB, linear ambient occlusion in alpha.
    pub albedo: &'static [u8],
    /// PNG: OpenGL tangent-space normal in RGB, linear roughness in alpha.
    pub normal_roughness: &'static [u8],
    pub metallic: f32,
    pub emission: f32,
}

macro_rules! texture {
    ($key:literal, $name:literal, $metallic:expr, $emission:expr) => {
        PropTexture {
            key: $key,
            name: $name,
            albedo: include_bytes!(concat!("../../../assets/props/", $key, "/albedo.png")),
            normal_roughness: include_bytes!(concat!(
                "../../../assets/props/",
                $key,
                "/normal_roughness.png"
            )),
            metallic: $metallic,
            emission: $emission,
        }
    };
}

pub fn textures() -> &'static [PropTexture] {
    &TEXTURES
}

// Order is the material-array contract, starting at layer 8.
static TEXTURES: [PropTexture; 7] = [
    texture!("08_wall_lamp", "industrial_wall_lamp", 0.0, 0.0),
    texture!("09_wall_lamp_glass", "industrial_wall_lamp_glass", 0.0, 0.8),
    texture!("10_hanging_lamp", "hanging_industrial_lamp", 0.2776, 0.0),
    texture!(
        "11_hanging_lamp_glass",
        "hanging_industrial_lamp_glass",
        0.0,
        0.8
    ),
    texture!("12_utility_box", "utility_box_01", 0.1328, 0.0),
    texture!(
        "13_pipe",
        "modular_industrial_pipes_01_group01",
        0.5632,
        0.0
    ),
    texture!("14_rock", "rock_07", 0.0, 0.0),
];

struct PropSource {
    key: &'static str,
    name: &'static str,
    near: &'static [u8],
    distant: &'static [u8],
    min: [f32; 3],
    max: [f32; 3],
}
static SOURCES: [PropSource; 5] = [
    PropSource {
        key: "wall_lamp",
        name: "Wall lamp",
        near: include_bytes!("../../../assets/props/wall_lamp.spm"),
        distant: include_bytes!("../../../assets/props/wall_lamp_distant.spm"),
        min: [
            -0.13672421872615814,
            -0.21495330333709717,
            1.702955643168025e-08,
        ],
        max: [0.13673660159111023, 0.21495330333709717, 0.1403784602880478],
    },
    PropSource {
        key: "hanging_lamp",
        name: "Hanging lamp",
        near: include_bytes!("../../../assets/props/hanging_lamp.spm"),
        distant: include_bytes!("../../../assets/props/hanging_lamp_distant.spm"),
        min: [
            -0.27480319142341614,
            -1.3400129079818726,
            -0.27480337023735046,
        ],
        max: [0.2748033106327057, 0.015337205491960049, 0.2748030722141266],
    },
    PropSource {
        key: "utility_box",
        name: "Utility box",
        near: include_bytes!("../../../assets/props/utility_box.spm"),
        distant: include_bytes!("../../../assets/props/utility_box_distant.spm"),
        min: [-0.2600001096725464, 0.0, -0.21599999070167542],
        max: [0.2600001096725464, 1.1203334331512451, 0.21599999070167542],
    },
    PropSource {
        key: "pipe",
        name: "Pipe",
        near: include_bytes!("../../../assets/props/pipe.spm"),
        distant: include_bytes!("../../../assets/props/pipe_distant.spm"),
        min: [-0.1035519540309906, 0.0, -0.10316049307584763],
        max: [0.1035519540309906, 1.9546396732330322, 0.10316049307584763],
    },
    PropSource {
        key: "rock",
        name: "Rock",
        near: include_bytes!("../../../assets/props/rock.spm"),
        distant: include_bytes!("../../../assets/props/rock_distant.spm"),
        min: [-0.6755276322364807, 0.0, -1.2811079025268555],
        max: [0.6755276322364807, 1.1497611999511719, 1.2811079025268555],
    },
];

pub fn load() -> Result<Vec<PropAsset>, String> {
    SOURCES
        .iter()
        .map(|source| {
            Ok(PropAsset {
                key: source.key,
                name: source.name,
                near: decode(source.near).map_err(|e| format!("{}: {e}", source.key))?,
                distant: decode(source.distant)
                    .map_err(|e| format!("{} distant: {e}", source.key))?,
                bounds_min: source.min,
                bounds_max: source.max,
                lod_distance: 20.0,
            })
        })
        .collect()
}

/// Local source bounds; no mesh allocation or decoding is needed for picking.
pub fn bounds(key: &str) -> Option<([f32; 3], [f32; 3])> {
    SOURCES
        .iter()
        .find(|source| source.key == key)
        .map(|source| (source.min, source.max))
}

/// Ray distance to the rotated prop's bounds, padded by 6 cm for small fixtures.
/// Direction must be normalized if the result is to be interpreted as metres.
/// A caller can compare this distance with the world ray hit to reject occluded props.
pub fn ray_distance(
    key: &str,
    position: [f32; 3],
    rotation: f32,
    eye: [f32; 3],
    direction: [f32; 3],
) -> Option<f32> {
    if !position
        .into_iter()
        .chain(eye)
        .chain(direction)
        .chain([rotation])
        .all(f32::is_finite)
        || direction.iter().map(|v| v * v).sum::<f32>() < 1e-12
    {
        return None;
    }
    let (min, max) = bounds(key)?;
    let (s, c) = rotation.sin_cos();
    let delta: [f32; 3] = std::array::from_fn(|i| eye[i] - position[i]);
    let origin = [
        c * delta[0] - s * delta[2],
        delta[1],
        s * delta[0] + c * delta[2],
    ];
    let ray = [
        c * direction[0] - s * direction[2],
        direction[1],
        s * direction[0] + c * direction[2],
    ];
    let mut near = 0.0f32;
    let mut far = f32::INFINITY;
    for axis in 0..3 {
        let low = min[axis] - 0.06;
        let high = max[axis] + 0.06;
        if ray[axis].abs() < 1e-8 {
            if origin[axis] < low || origin[axis] > high {
                return None;
            }
        } else {
            let a = (low - origin[axis]) / ray[axis];
            let b = (high - origin[axis]) / ray[axis];
            near = near.max(a.min(b));
            far = far.min(a.max(b));
            if near > far {
                return None;
            }
        }
    }
    Some(near)
}

// SPM1 is little-endian: magic, u32 vertex/index counts, then 40-byte
// position/normal/UV/material/emission vertices followed by u32 indices.
fn decode(data: &[u8]) -> Result<PropMesh, String> {
    if data.len() < 12 || &data[..4] != b"SPM1" {
        return Err("invalid mesh header".into());
    }
    let u32_at = |offset: usize| u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    let f32_at = |offset: usize| f32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    let vertex_count = u32_at(4) as usize;
    let index_count = u32_at(8) as usize;
    if vertex_count == 0
        || vertex_count > 100_000
        || index_count == 0
        || index_count > 300_000
        || index_count % 3 != 0
        || data.len() != 12 + vertex_count * 40 + index_count * 4
    {
        return Err("invalid mesh size".into());
    }
    let mut vertices = Vec::with_capacity(vertex_count);
    for i in 0..vertex_count {
        let start = 12 + i * 40;
        let vertex = PropVertex {
            position: std::array::from_fn(|j| f32_at(start + j * 4)),
            normal: std::array::from_fn(|j| f32_at(start + 12 + j * 4)),
            uv: std::array::from_fn(|j| f32_at(start + 24 + j * 4)),
            material: u32_at(start + 32),
            emission: f32_at(start + 36),
        };
        if !vertex
            .position
            .into_iter()
            .chain(vertex.normal)
            .chain(vertex.uv)
            .chain([vertex.emission])
            .all(f32::is_finite)
            || !(8..15).contains(&vertex.material)
        {
            return Err("invalid mesh vertex".into());
        }
        vertices.push(vertex);
    }
    let indices: Vec<u32> = (0..index_count)
        .map(|i| u32_at(12 + vertex_count * 40 + i * 4))
        .collect();
    if indices.iter().any(|i| *i as usize >= vertex_count) {
        return Err("mesh index outside vertex array".into());
    }
    Ok(PropMesh { vertices, indices })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picking_reaches_the_fixture_below_its_mount_and_tall_prop_tops() {
        // The hanging lens is more than a metre below the mounting origin.
        let hit = ray_distance(
            "hanging_lamp",
            [0.0, 3.0, 0.0],
            0.0,
            [0.0, 1.8, 4.0],
            [0.0, 0.0, -1.0],
        )
        .unwrap();
        assert!((3.6..4.0).contains(&hit));
        assert!(ray_distance("pipe", [0.0; 3], 0.0, [0.0, 1.8, 4.0], [0.0, 0.0, -1.0]).is_some());
        assert!(ray_distance(
            "hanging_lamp",
            [0.0, 3.0, 0.0],
            0.0,
            [0.0, 0.5, 4.0],
            [0.0, 0.0, -1.0]
        )
        .is_none());
    }

    #[test]
    fn picking_rotates_asymmetric_bounds_and_ignores_behind_camera_objects() {
        let ray = [0.0, 0.0, -1.0];
        let eye = [1.0, 0.5, 4.0];
        assert!(ray_distance("rock", [0.0; 3], 0.0, eye, ray).is_none());
        assert!(ray_distance("rock", [0.0; 3], std::f32::consts::FRAC_PI_2, eye, ray).is_some());
        assert!(ray_distance("rock", [0.0; 3], 0.0, [0.0, 0.5, -4.0], ray).is_none());
        assert!(ray_distance("rock", [0.0; 3], 0.0, eye, [0.0; 3]).is_none());
    }

    #[test]
    fn shipped_props_have_valid_normals_bounds_and_smaller_distant_meshes() {
        let assets = load().unwrap();
        assert_eq!(assets.len(), 5);
        for asset in assets {
            assert!(asset.near.indices.len() / 3 <= 4_000, "{}", asset.key);
            assert!(asset.distant.indices.len() < asset.near.indices.len());
            for mesh in [&asset.near, &asset.distant] {
                for vertex in &mesh.vertices {
                    let length_squared = vertex.normal.iter().map(|n| n * n).sum::<f32>();
                    assert!((length_squared - 1.0).abs() < 0.01);
                    for axis in 0..3 {
                        assert!(vertex.position[axis] >= asset.bounds_min[axis] - 0.0001);
                        assert!(vertex.position[axis] <= asset.bounds_max[axis] + 0.0001);
                    }
                }
            }
        }
    }

    #[test]
    fn malformed_prop_mesh_is_rejected_without_panicking() {
        let original = include_bytes!("../../../assets/props/pipe.spm");
        for length in [0, 4, 11, 12, original.len() - 1] {
            assert!(decode(&original[..length]).is_err());
        }
        let mut invalid = original.to_vec();
        let last = invalid.len() - 4;
        invalid[last..].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&invalid).is_err());
        invalid = original.to_vec();
        invalid[12..16].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(decode(&invalid).is_err());
    }
}
