use super::{atlas::PAGE_SIZE, lightset::TierConfig};
use crate::lighting::{NO_TILE, OVERLAY_NONE, SHADOW_NEAR, TILE_GUARD_SCALE};

const COMMON: &str = include_str!("../common.wgsl");
const STORAGE_BINDINGS: &str = "\
@group(1) @binding(3) var<storage, read> lights: array<Light>;
@group(1) @binding(4) var<storage, read> cluster_masks: array<u32>;
fn light_at(i: u32) -> Light { return lights[i]; }
fn light_sphere(i: u32) -> vec4<f32> { return lights[i].position_range; }
fn mask_word(cluster: u32, word: u32) -> u32 { return cluster_masks[cluster * MASK_WORDS + word]; }
";
// Where storage buffers or compute shaders are missing, lights and CPU-built
// masks come from uniform buffers.
const UNIFORM_BINDINGS: &str = "\
@group(1) @binding(3) var<uniform> lights: array<Light, MAX_LIGHTS>;
@group(1) @binding(4) var<uniform> cluster_masks: array<vec4<u32>, MASK_VECS>;
fn light_at(i: u32) -> Light { return lights[min(i, MAX_LIGHTS - 1u)]; }
fn light_sphere(i: u32) -> vec4<f32> { return lights[min(i, MAX_LIGHTS - 1u)].position_range; }
fn mask_word(cluster: u32, word: u32) -> u32 {
    let index = cluster * MASK_WORDS + word;
    return cluster_masks[index / 4u][index % 4u];
}
";

/// How the light buffers are declared, and how large they are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LightLayout {
    pub storage: bool,
    pub mask_words: u32,
    pub max_lights: u32,
    pub mask_vectors: u32,
}

impl LightLayout {
    pub fn for_tier(config: &TierConfig) -> Self {
        let storage = config.grid.is_some_and(|g| g.words > 2);
        let (mask_words, mask_clusters) = config
            .grid
            .map_or((2, 2), |g| (g.words, g.count() as u32 * g.words));
        Self {
            storage,
            mask_words,
            max_lights: config.max_lights as u32,
            mask_vectors: mask_clusters.div_ceil(4).max(1),
        }
    }

    fn header(&self) -> String {
        format!(
            "const MASK_WORDS: u32 = {}u;\nconst MAX_LIGHTS: u32 = {}u;\nconst MASK_VECS: u32 = {}u;\n\
             const ATLAS_SIZE: f32 = {:?};\nconst SHADOW_NEAR: f32 = {:?};\n\
             const TILE_GUARD_SCALE: f32 = {:?};\nconst NO_TILE: u32 = {}u;\nconst OVERLAY_NONE: u32 = {}u;\n",
            self.mask_words,
            self.max_lights,
            self.mask_vectors,
            PAGE_SIZE as f32,
            SHADOW_NEAR,
            TILE_GUARD_SCALE,
            NO_TILE,
            OVERLAY_NONE,
        )
    }
}

pub(super) fn source(name: &str, layout: LightLayout) -> String {
    let (common, body) = match name {
        "world" => (COMMON, include_str!("../world.wgsl")),
        "sky" => (COMMON, include_str!("../sky.wgsl")),
        "volume" => (COMMON, include_str!("../volume.wgsl")),
        // Depth-only tiles and the culling pass use the light structures and
        // transform, without the scene's bindings.
        "shadow" => (types_only(), include_str!("../shadow.wgsl")),
        "clusters" => (types_only(), include_str!("../clusters.wgsl")),
        "post" => return include_str!("../post.wgsl").to_owned(),
        _ => unreachable!("unknown embedded shader"),
    };
    let bindings = if layout.storage {
        STORAGE_BINDINGS
    } else {
        UNIFORM_BINDINGS
    };
    let common = common.replace("//LIGHT_STORAGE", bindings);
    format!("{}\n{common}\n{body}", layout.header())
}

fn types_only() -> &'static str {
    COMMON
        .split("// Camera and lighting bindings")
        .next()
        .unwrap()
}

pub(super) fn module(device: &wgpu::Device, name: &str, layout: LightLayout) -> wgpu::ShaderModule {
    trusted(device, name, source(name, layout))
}

/// Compile an embedded shader without wgpu's forced loop bounding.
///
/// On Metal, wgpu makes the compiler keep every loop by reading a `volatile`
/// flag in each iteration, which made the clustered light loop about three
/// times slower (the sun's filtering loops too). Bounds checks stay on.
pub(super) fn trusted(device: &wgpu::Device, label: &str, source: String) -> wgpu::ShaderModule {
    // SAFETY: `force_loop_bounding` is the only protection turned off. It exists
    // for shaders that might never finish, and none of the shaders embedded in
    // this crate can run forever, because every loop has a bounded trip count:
    // counted loops run to a constant (4 shadow taps, 3 × 3 filters, 8 cluster
    // corners, `MASK_WORDS` words of a light mask, and at most `MAX_LIGHTS`
    // lights), and the clustered loop over the set bits of a mask word clears its
    // lowest bit each time round, so it ends within 32 iterations, 256 across a
    // cluster's 8 words. `bounds_checks` stays true, so out-of-range reads and
    // writes are still contained. The WebGPU backend ignores these options and
    // compiles the shader as `create_shader_module` would; the browser applies
    // its own limits.
    unsafe {
        device.create_shader_module_trusted(
            wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            },
            wgpu::ShaderRuntimeChecks {
                bounds_checks: true,
                force_loop_bounding: false,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lighting::GpuLight, render::lightset::LightingTier};

    fn layouts() -> Vec<LightLayout> {
        [
            LightingTier::Clustered,
            LightingTier::Uniform,
            LightingTier::Nearest,
        ]
        .map(|tier| LightLayout::for_tier(&tier.config()))
        .to_vec()
    }

    fn validate(name: &str, layout: LightLayout) -> (naga::Module, naga::valid::ModuleInfo) {
        let source = source(name, layout);
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(&source)));
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{name} {layout:?}: {e:?}"));
        (module, info)
    }

    #[test]
    fn every_assembled_shader_validates() {
        for layout in layouts() {
            for name in ["world", "sky", "volume", "shadow", "clusters", "post"] {
                validate(name, layout);
            }
        }
    }

    #[test]
    fn entry_points_stay_within_compatibility_limits() {
        for layout in layouts() {
            for name in ["world", "sky", "volume", "shadow", "clusters", "post"] {
                let (module, info) = validate(name, layout);
                for (index, entry) in module.entry_points.iter().enumerate() {
                    let usage = info.get_entry_point(index);
                    let storage = module
                        .global_variables
                        .iter()
                        .filter(|(handle, var)| {
                            matches!(var.space, naga::AddressSpace::Storage { .. })
                                && !usage[*handle].is_empty()
                        })
                        .count();
                    assert!(
                        storage <= 4,
                        "{name} {}: {storage} storage buffers",
                        entry.name
                    );
                    if entry.stage == naga::ShaderStage::Compute {
                        let invocations: u32 = entry.workgroup_size.iter().product();
                        assert!(invocations <= 64, "{name} {}", entry.name);
                    }
                }
            }
        }
    }

    #[test]
    fn fallback_buffers_fit_the_smallest_guaranteed_uniform_binding() {
        for tier in [LightingTier::Uniform, LightingTier::Nearest] {
            let layout = LightLayout::for_tier(&tier.config());
            assert!(!layout.storage);
            assert!(layout.max_lights as usize * std::mem::size_of::<GpuLight>() <= 16384);
            assert!(layout.mask_vectors as usize * 16 <= 16384);
        }
        let layout = LightLayout::for_tier(&LightingTier::Clustered.config());
        assert!(layout.storage && layout.mask_words == 8 && layout.max_lights == 256);
    }

    #[test]
    fn shader_structs_match_the_rust_records() {
        let (module, _) = validate("world", layouts()[0]);
        let mut layouter = naga::proc::Layouter::default();
        layouter.update(module.to_ctx()).unwrap();
        let size_of = |name: &str| {
            let (handle, _) = module
                .types
                .iter()
                .find(|(_, ty)| ty.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("no struct {name}"));
            layouter[handle].size as usize
        };
        assert_eq!(size_of("Light"), std::mem::size_of::<GpuLight>());
        assert_eq!(
            size_of("LightParams"),
            std::mem::size_of::<crate::render::lights::LightParams>()
        );
        assert_eq!(
            size_of("Camera"),
            std::mem::size_of::<crate::render::CameraUniform>()
        );
    }
}
