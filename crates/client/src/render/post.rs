use super::{settings::RenderSettings, timing::GpuTiming};

pub(super) const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

pub(super) struct PostProcess {
    pub scene: wgpu::TextureView,
    pub multisampled: Option<wgpu::TextureView>,
    pub depth: wgpu::TextureView,
    pub size: [u32; 2],
    pub samples: u32,
    bloom: Vec<wgpu::TextureView>,
    down_groups: Vec<wgpu::BindGroup>,
    up_groups: Vec<wgpu::BindGroup>,
    post_group: wgpu::BindGroup,
    params: wgpu::Buffer,
    threshold: wgpu::RenderPipeline,
    down: wgpu::RenderPipeline,
    up: wgpu::RenderPipeline,
    post: wgpu::RenderPipeline,
}

fn texture(
    device: &wgpu::Device,
    size: [u32; 2],
    samples: u32,
    format: wgpu::TextureFormat,
    label: &str,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

impl PostProcess {
    pub fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        output: wgpu::TextureFormat,
        settings: RenderSettings,
        msaa_supported: bool,
    ) -> Self {
        let size = [
            (width as f32 * settings.scale).round().max(1.0) as u32,
            (height as f32 * settings.scale).round().max(1.0) as u32,
        ];
        let samples = settings
            .msaa
            .samples(u64::from(size[0]) * u64::from(size[1]), msaa_supported);
        let scene = texture(device, size, 1, HDR_FORMAT, "resolved linear scene");
        let multisampled =
            (samples > 1).then(|| texture(device, size, samples, HDR_FORMAT, "MSAA linear scene"));
        let depth = texture(
            device,
            size,
            samples,
            wgpu::TextureFormat::Depth32Float,
            "scene depth",
        );
        let bloom: Vec<_> = (1..=5)
            .map(|level| {
                texture(
                    device,
                    [(size[0] >> level).max(1), (size[1] >> level).max(1)],
                    1,
                    HDR_FORMAT,
                    "bloom mip",
                )
            })
            .collect();
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("post parameters"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("post linear clamp"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let group = |source: &wgpu::TextureView, bloom: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("post inputs"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(bloom),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: params.as_entire_binding(),
                    },
                ],
            })
        };
        // Bind unused bloom slots to the source too, never to the render attachment.
        let down_groups = (0..bloom.len())
            .map(|i| {
                let input = if i == 0 { &scene } else { &bloom[i - 1] };
                group(input, input)
            })
            .collect();
        let up_groups = (1..bloom.len())
            .map(|i| group(&bloom[i], &bloom[i]))
            .collect();
        let post_group = group(&scene, &bloom[0]);
        let shader = device.create_shader_module(wgpu::include_wgsl!("../post.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("post pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = |entry: &'static str, format, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview: None,
                cache: None,
            })
        };
        let additive = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        Self {
            scene,
            multisampled,
            depth,
            size,
            samples,
            bloom,
            down_groups,
            up_groups,
            post_group,
            params,
            threshold: pipeline("fs_threshold", HDR_FORMAT, None),
            down: pipeline("fs_down", HDR_FORMAT, None),
            up: pipeline(
                "fs_up",
                HDR_FORMAT,
                Some(wgpu::BlendState {
                    color: additive,
                    alpha: additive,
                }),
            ),
            post: pipeline("fs_post", output, None),
        }
    }
    pub fn draw(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        settings: RenderSettings,
        output_srgb: bool,
        timing: &mut GpuTiming,
    ) {
        queue.write_buffer(
            &self.params,
            0,
            bytemuck::cast_slice(&[
                settings.exposure,
                settings.bloom,
                if output_srgb { 0.0f32 } else { 1.0 },
                0.0,
            ]),
        );
        let mut draw = |label, target, pipeline, group, load| {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: timing.pass(label),
                occlusion_query_set: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        };
        if settings.bloom > 0.0 {
            for i in 0..self.bloom.len() {
                draw(
                    [
                        "bloom down 1",
                        "bloom down 2",
                        "bloom down 3",
                        "bloom down 4",
                        "bloom down 5",
                    ][i],
                    &self.bloom[i],
                    if i == 0 { &self.threshold } else { &self.down },
                    &self.down_groups[i],
                    wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                );
            }
            for i in (0..self.up_groups.len()).rev() {
                draw(
                    ["bloom up 1", "bloom up 2", "bloom up 3", "bloom up 4"][i],
                    &self.bloom[i],
                    &self.up,
                    &self.up_groups[i],
                    wgpu::LoadOp::Load,
                );
            }
        }
        draw(
            "post",
            target,
            &self.post,
            &self.post_group,
            wgpu::LoadOp::Clear(wgpu::Color::BLACK),
        );
    }
}
