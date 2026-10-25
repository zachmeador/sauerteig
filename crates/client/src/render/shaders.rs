const COMMON: &str = include_str!("../common.wgsl");

pub(super) fn source(name: &str) -> String {
    let (prefix, body) = match name {
        "world" => (COMMON, include_str!("../world.wgsl")),
        "sky" => (COMMON, include_str!("../sky.wgsl")),
        "volume" => (COMMON, include_str!("../volume.wgsl")),
        // Depth-only local shadows use the same light structure and transform,
        // without the full scene camera binding.
        "shadow" => (
            COMMON
                .split("// Camera and lighting bindings")
                .next()
                .unwrap(),
            include_str!("../shadow.wgsl"),
        ),
        "post" => ("", include_str!("../post.wgsl")),
        _ => unreachable!("unknown embedded shader"),
    };
    format!("{prefix}\n{body}")
}

pub(super) fn module(device: &wgpu::Device, name: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(name),
        source: wgpu::ShaderSource::Wgsl(source(name).into()),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_assembled_shader_validates() {
        for name in ["world", "sky", "volume", "shadow", "post"] {
            let source = super::source(name);
            let module = naga::front::wgsl::parse_str(&source)
                .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(&source)));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::empty(),
            )
            .validate(&module)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
    }
}
