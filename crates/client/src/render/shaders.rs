use super::{atlas::PAGE_SIZE, device::MAX_ARRAY_LAYERS, lightset::MAX_LIGHTS_IN_VIEW};
use crate::lighting::{NO_TILE, SHADOW_NEAR, TILE_GUARD_TEXELS};

const COMMON: &str = include_str!("../common.wgsl");

/// The constants every shader gets ahead of its source. The loops over lights
/// are bounded by `MAX_LIGHTS` and `MAX_LIGHT_WORDS`, however many lights a
/// frame has; the frame's own count is read from the light parameters.
fn header() -> String {
    format!(
        "const MAX_LIGHTS: u32 = {}u;\nconst MAX_LIGHT_WORDS: u32 = {}u;\n\
         const ATLAS_SIZE: f32 = {:?};\nconst SHADOW_NEAR: f32 = {:?};\n\
         const GUARD_TEXELS: f32 = {:?};\nconst NO_TILE: u32 = {}u;\n\
         const FRAME_PAGE_BASE: u32 = {}u;\n",
        MAX_LIGHTS_IN_VIEW,
        MAX_LIGHTS_IN_VIEW.div_ceil(32),
        PAGE_SIZE as f32,
        SHADOW_NEAR,
        TILE_GUARD_TEXELS,
        NO_TILE,
        MAX_ARRAY_LAYERS,
    )
}

pub(super) fn source(name: &str) -> String {
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
    format!("{}\n{common}\n{body}", header())
}

fn types_only() -> &'static str {
    COMMON
        .split("// Camera and lighting bindings")
        .next()
        .unwrap()
}

pub(super) fn module(device: &wgpu::Device, name: &str) -> wgpu::ShaderModule {
    trusted(device, name, source(name))
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
    // corners, the 32 bits of a mask word), or to a count that is clamped to a
    // constant in the shader before the loop (at most `MAX_LIGHTS` lights and
    // `MAX_LIGHT_WORDS` words of a cluster's mask, whatever the light
    // parameters say), and the clustered loop over the set bits of a mask word
    // clears its lowest bit each time round, so it ends within 32 iterations.
    // `bounds_checks` stays true, so out-of-range reads and writes are still
    // contained.
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
    use crate::{lighting::GpuLight, render::device::requested_limits};

    const SHADERS: [&str; 6] = ["world", "sky", "volume", "shadow", "clusters", "post"];

    fn validate(name: &str) -> (naga::Module, naga::valid::ModuleInfo) {
        let source = source(name);
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(&source)));
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        (module, info)
    }

    #[test]
    fn every_assembled_shader_validates() {
        for name in SHADERS {
            validate(name);
        }
    }

    #[test]
    fn entry_points_stay_within_the_requested_limits() {
        let limits = requested_limits(1920);
        for name in SHADERS {
            let (module, info) = validate(name);
            for (index, entry) in module.entry_points.iter().enumerate() {
                let usage = info.get_entry_point(index);
                let used = |wanted: &dyn Fn(&naga::GlobalVariable) -> bool| {
                    module
                        .global_variables
                        .iter()
                        .filter(|(handle, var)| wanted(var) && !usage[*handle].is_empty())
                        .count() as u32
                };
                let storage = used(&|var| matches!(var.space, naga::AddressSpace::Storage { .. }));
                let uniform = used(&|var| var.space == naga::AddressSpace::Uniform);
                let textures = used(&|var| {
                    matches!(
                        module.types[var.ty].inner,
                        naga::TypeInner::Image {
                            class: naga::ImageClass::Sampled { .. }
                                | naga::ImageClass::Depth { .. },
                            ..
                        }
                    )
                });
                let samplers = used(&|var| {
                    matches!(module.types[var.ty].inner, naga::TypeInner::Sampler { .. })
                });
                let label = format!("{name} {}", entry.name);
                assert!(
                    storage <= limits.max_storage_buffers_per_shader_stage,
                    "{label}: {storage} storage buffers"
                );
                assert!(
                    uniform <= limits.max_uniform_buffers_per_shader_stage,
                    "{label}: {uniform} uniform buffers"
                );
                assert!(
                    textures <= limits.max_sampled_textures_per_shader_stage,
                    "{label}: {textures} sampled textures"
                );
                assert!(
                    samplers <= limits.max_samplers_per_shader_stage,
                    "{label}: {samplers} samplers"
                );
                if entry.stage == naga::ShaderStage::Compute {
                    let [x, y, z] = entry.workgroup_size;
                    assert!(x <= limits.max_compute_workgroup_size_x, "{label}");
                    assert!(y <= limits.max_compute_workgroup_size_y, "{label}");
                    assert!(z <= limits.max_compute_workgroup_size_z, "{label}");
                    assert!(
                        x * y * z <= limits.max_compute_invocations_per_workgroup,
                        "{label}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_light_buffers_fit_the_requested_binding_sizes() {
        let limits = requested_limits(1920);
        let grid = crate::render::clusters::ClusterGrid::CLUSTERED;
        let masks = grid.count() as u64 * MAX_LIGHTS_IN_VIEW.div_ceil(32) as u64 * 4;
        let lights = MAX_LIGHTS_IN_VIEW as u64 * std::mem::size_of::<GpuLight>() as u64;
        assert!(masks.max(lights) <= u64::from(limits.max_storage_buffer_binding_size));
        // A page table entry for every page of both atlases.
        assert!(
            MAX_ARRAY_LAYERS as u64 * 2 * 4 <= u64::from(limits.max_storage_buffer_binding_size)
        );
    }

    #[test]
    fn shader_structs_match_the_rust_records() {
        let (module, _) = validate("world");
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
