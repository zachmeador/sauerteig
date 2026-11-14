//! What the renderer asks of an adapter: which graphics APIs to consider, which
//! features and limits to request, and which anti-aliasing sample counts the
//! adapter can run. Kept apart from `Renderer::create` so tests can feed it
//! adapters that don't exist on the build machine.
use super::post::HDR_FORMAT;
use wgpu::{
    AdapterInfo, Backend, Backends, DownlevelFlags, Features, Limits, TextureFormat,
    TextureFormatFeatureFlags as Flags,
};

/// The graphics APIs Sauerteig supports, the ones the minimum specification
/// names. OpenGL is left out. `WGPU_BACKEND` still overrides this for testing.
pub(super) const BACKENDS: Backends = Backends::VULKAN
    .union(Backends::DX12)
    .union(Backends::METAL);

const MINIMUM_SPECIFICATION: &str = "Sauerteig needs 8 GB of memory and a GPU about as fast as a \
Steam Deck's, with Vulkan, DirectX 12 or Metal";

pub(super) fn no_adapter_message() -> String {
    format!("No GPU with Vulkan, DirectX 12 or Metal support was found. {MINIMUM_SPECIFICATION}.")
}

/// Scene targets up to this many pixels across work on every adapter that has
/// the minimum specification. A bigger window asks for more.
const MIN_TEXTURE_DIMENSION: u32 = 8192;
/// The material arrays hold at most 64 layers (`MAX_MATERIALS` in
/// `materials.rs`).
const MATERIAL_LAYERS: u32 = 64;

/// Features the renderer uses where the adapter has them.
const OPTIONAL_FEATURES: Features =
    Features::TIMESTAMP_QUERY.union(Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES);

/// Adapter abilities that are not features or limits, which the renderer
/// cannot run without: compute shaders for the light culling pass, 32-bit
/// indices across the whole range for world meshes, and the anisotropic
/// sampler the materials use.
const REQUIRED_DOWNLEVEL: DownlevelFlags = DownlevelFlags::COMPUTE_SHADERS
    .union(DownlevelFlags::FULL_DRAW_INDEX_UINT32)
    .union(DownlevelFlags::ANISOTROPIC_FILTERING);

/// The limits the renderer's shaders and resources use. Everything not named
/// here keeps the value of wgpu's downlevel defaults, which Vulkan, DirectX 12
/// and Metal all guarantee. `target` is the largest scene target or surface
/// edge in pixels.
pub(super) fn requested_limits(target: u32) -> Limits {
    let side = MIN_TEXTURE_DIMENSION.max(target);
    Limits {
        max_texture_dimension_1d: side,
        max_texture_dimension_2d: side,
        max_texture_array_layers: MATERIAL_LAYERS,
        // Camera, lights and shadows, materials.
        max_bind_groups: 3,
        // The light list and cluster masks read in fragment shaders; the
        // culling pass reads the light spheres and writes the masks.
        max_storage_buffers_per_shader_stage: 2,
        // One 64-lane workgroup per 64 clusters.
        max_compute_invocations_per_workgroup: 64,
        max_compute_workgroup_size_x: 64,
        max_compute_workgroup_size_y: 1,
        max_compute_workgroup_size_z: 1,
        ..Limits::downlevel_defaults()
    }
}

/// What to pass to `Adapter::request_device`.
#[derive(Debug)]
pub(super) struct DeviceRequest {
    pub features: Features,
    pub limits: Limits,
}

/// Choose the features and limits for an adapter, or say what it lacks.
pub(super) fn plan_request(
    info: &AdapterInfo,
    features: Features,
    limits: &Limits,
    downlevel: DownlevelFlags,
    target: u32,
) -> Result<DeviceRequest, String> {
    let wanted = requested_limits(target);
    let mut missing = Vec::new();
    wanted.check_limits_with_fail_fn(limits, false, |name, wanted, offered| {
        missing.push(format!("the limit {name} (needs {wanted}, has {offered})"));
    });
    for (flag, name) in [
        (DownlevelFlags::COMPUTE_SHADERS, "compute shaders"),
        (
            DownlevelFlags::FULL_DRAW_INDEX_UINT32,
            "32-bit indices over their whole range",
        ),
        (
            DownlevelFlags::ANISOTROPIC_FILTERING,
            "anisotropic filtering",
        ),
    ] {
        debug_assert!(REQUIRED_DOWNLEVEL.contains(flag));
        if !downlevel.contains(flag) {
            missing.push(name.to_owned());
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "{} ({}) cannot run Sauerteig: it lacks {}. {MINIMUM_SPECIFICATION}.",
            info.name,
            backend_name(info.backend),
            missing.join(", ")
        ));
    }
    Ok(DeviceRequest {
        features: features & OPTIONAL_FEATURES,
        limits: wanted,
    })
}

pub(super) fn backend_name(backend: Backend) -> &'static str {
    match backend {
        Backend::Vulkan => "Vulkan",
        Backend::Dx12 => "DirectX 12",
        Backend::Metal => "Metal",
        Backend::Gl => "OpenGL",
        Backend::BrowserWebGpu => "WebGPU",
        Backend::Empty => "no backend",
    }
}

/// The adapter's name with its graphics API and driver, for F3 and the capture
/// report: `NVIDIA GeForce RTX 4070 (Vulkan, NVIDIA 561.09)`.
pub(super) fn describe_adapter(info: &AdapterInfo) -> String {
    let driver = [info.driver.trim(), info.driver_info.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if driver.is_empty() {
        format!("{} ({})", info.name, backend_name(info.backend))
    } else {
        format!("{} ({}, {driver})", info.name, backend_name(info.backend))
    }
}

/// The sample counts the scene's color and depth targets can use, 1 first:
/// those both formats allow, with resolve for the color target. Without the
/// adapter-specific format features, wgpu allows only the counts WebGPU
/// guarantees, whatever the adapter reports.
pub(super) fn sample_counts(color: Flags, depth: Flags, adapter_specific: bool) -> Vec<u32> {
    let (color, depth) = if adapter_specific {
        (color, depth)
    } else {
        let guaranteed = Flags::MULTISAMPLE_X4 | Flags::MULTISAMPLE_RESOLVE;
        (color & guaranteed, depth & guaranteed)
    };
    let mut counts = vec![1];
    if color.contains(Flags::MULTISAMPLE_RESOLVE) {
        for count in [2, 4, 8] {
            if color.sample_count_supported(count) && depth.sample_count_supported(count) {
                counts.push(count);
            }
        }
    }
    counts
}

pub(super) fn adapter_sample_counts(adapter: &wgpu::Adapter, features: Features) -> Vec<u32> {
    sample_counts(
        adapter.get_texture_format_features(HDR_FORMAT).flags,
        adapter
            .get_texture_format_features(TextureFormat::Depth32Float)
            .flags,
        features.contains(Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> AdapterInfo {
        AdapterInfo {
            name: "Test GPU".into(),
            vendor: 0,
            device: 0,
            device_type: wgpu::DeviceType::DiscreteGpu,
            driver: "Test driver".into(),
            driver_info: "1.2".into(),
            backend: Backend::Vulkan,
        }
    }

    fn plan(
        features: Features,
        limits: &Limits,
        downlevel: DownlevelFlags,
    ) -> Result<DeviceRequest, String> {
        plan_request(&info(), features, limits, downlevel, 1920)
    }

    #[test]
    fn an_adapter_with_what_the_renderer_uses_gets_the_request() {
        let request = plan(Features::all(), &Limits::default(), DownlevelFlags::all()).unwrap();
        // Only the two optional features, though the adapter offers all.
        assert_eq!(request.features, OPTIONAL_FEATURES);
        assert_eq!(request.limits.max_storage_buffers_per_shader_stage, 2);
        assert_eq!(request.limits.max_texture_dimension_2d, 8192);
        // Without them, none are requested.
        let request = plan(Features::empty(), &Limits::default(), DownlevelFlags::all()).unwrap();
        assert!(request.features.is_empty());
    }

    #[test]
    fn a_larger_target_raises_the_texture_limit() {
        assert_eq!(requested_limits(2160).max_texture_dimension_2d, 8192);
        assert_eq!(requested_limits(10_000).max_texture_dimension_2d, 10_000);
    }

    #[test]
    fn a_missing_limit_is_named_with_the_minimum_specification() {
        let limits = Limits {
            max_storage_buffers_per_shader_stage: 0,
            max_compute_invocations_per_workgroup: 32,
            ..Limits::default()
        };
        let error = plan(Features::empty(), &limits, DownlevelFlags::all()).unwrap_err();
        assert!(error.contains("Test GPU (Vulkan)"), "{error}");
        assert!(
            error.contains("max_storage_buffers_per_shader_stage"),
            "{error}"
        );
        assert!(
            error.contains("max_compute_invocations_per_workgroup"),
            "{error}"
        );
        assert!(error.contains("Steam Deck"), "{error}");
        assert!(error.contains("Vulkan, DirectX 12 or Metal"), "{error}");
    }

    #[test]
    fn a_missing_downlevel_ability_is_named() {
        let downlevel = DownlevelFlags::all() - DownlevelFlags::COMPUTE_SHADERS;
        let error = plan(Features::empty(), &Limits::default(), downlevel).unwrap_err();
        assert!(error.contains("compute shaders"), "{error}");
        assert!(error.contains("Steam Deck"), "{error}");
    }

    #[test]
    fn the_adapter_description_names_backend_and_driver() {
        assert_eq!(
            describe_adapter(&info()),
            "Test GPU (Vulkan, Test driver 1.2)"
        );
        let metal = AdapterInfo {
            driver: String::new(),
            driver_info: String::new(),
            backend: Backend::Metal,
            ..info()
        };
        assert_eq!(describe_adapter(&metal), "Test GPU (Metal)");
        let dx12 = AdapterInfo {
            backend: Backend::Dx12,
            ..info()
        };
        assert!(describe_adapter(&dx12).contains("DirectX 12"));
    }

    #[test]
    fn sample_counts_need_both_targets_and_a_resolve() {
        let resolve = Flags::MULTISAMPLE_RESOLVE;
        let all = Flags::MULTISAMPLE_X2 | Flags::MULTISAMPLE_X4 | Flags::MULTISAMPLE_X8;
        assert_eq!(sample_counts(all | resolve, all, true), [1, 2, 4, 8]);
        // An adapter whose depth format has no 8× isn't offered it.
        let no_eight = Flags::MULTISAMPLE_X2 | Flags::MULTISAMPLE_X4;
        assert_eq!(sample_counts(all | resolve, no_eight, true), [1, 2, 4]);
        assert_eq!(sample_counts(no_eight | resolve, all, true), [1, 2, 4]);
        // No resolve, no multisampling.
        assert_eq!(sample_counts(all, all, true), [1]);
        // Without the adapter-specific features only 4× is allowed.
        assert_eq!(sample_counts(all | resolve, all, false), [1, 4]);
        assert_eq!(sample_counts(Flags::empty(), Flags::empty(), true), [1]);
    }
}
