pub(crate) mod culling;
mod post;
mod props;
pub mod settings;
mod shaders;
mod shadows;
mod timing;
mod volumes;
use post::{PostProcess, HDR_FORMAT};
use settings::RenderSettings;
use shadows::CachedShadow;

use self::{
    culling::{Bounds, Frustum},
    props::{instance_layout, PropRenderer},
    volumes::VolumeRenderer,
};
use crate::lighting::{
    closest_lights, sun_shadow_matrix, LIGHT_RANGE, LOCAL_LIGHTS, LOCAL_SHADOW_SIZE,
};
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use sauerteig_core::{ChunkKey, Mesh, World, WorldObject};
use std::{collections::BTreeMap, sync::Arc};
use wgpu::util::DeviceExt;
use winit::window::Window;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GpuVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 3],
    pub emission: f32,
    pub uv: [f32; 2],
    pub material: u32,
}
impl GpuVertex {
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![0=>Float32x3, 1=>Float32x3, 2=>Float32x3, 3=>Float32, 4=>Float32x2, 5=>Uint32];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRS,
        }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    light_view_proj: [[f32; 4]; 4],
    eye_time: [f32; 4],
    sun: [f32; 4],
    fog: [f32; 4],
    points: [[f32; 4]; 8],
    point_colors: [[f32; 4]; 8],
    local_view_proj: [[[f32; 4]; 4]; LOCAL_LIGHTS],
}
struct LocalShadow {
    cache: CachedShadow,
    position: Option<Vec3>,
    depth: wgpu::TextureView,
    camera: wgpu::Buffer,
    group: wgpu::BindGroup,
}
pub struct GpuMesh {
    vertex: wgpu::Buffer,
    index: wgpu::Buffer,
    count: u32,
    bounds: Bounds,
}
impl GpuMesh {
    fn new(device: &wgpu::Device, vertices: &[GpuVertex], indices: &[u32]) -> Option<Self> {
        if vertices.is_empty() || indices.is_empty() {
            return None;
        }
        Some(Self {
            vertex: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mesh vertices"),
                contents: bytemuck::cast_slice(vertices),
                usage: wgpu::BufferUsages::VERTEX,
            }),
            index: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mesh indices"),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            }),
            count: indices.len() as u32,
            bounds: Bounds::from_positions(vertices.iter().map(|v| Vec3::from_array(v.position))),
        })
    }
    fn dynamic(device: &wgpu::Device) -> Self {
        let buffer = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 256,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        Self {
            vertex: buffer("dynamic vertices", wgpu::BufferUsages::VERTEX),
            index: buffer("dynamic indices", wgpu::BufferUsages::INDEX),
            count: 0,
            bounds: Bounds {
                min: Vec3::ZERO,
                max: Vec3::ZERO,
            },
        }
    }
    fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertices: &[GpuVertex],
        indices: &[u32],
    ) {
        if vertices.is_empty() || indices.is_empty() {
            self.count = 0;
            return;
        }
        let vertex_bytes = bytemuck::cast_slice(vertices);
        let index_bytes = bytemuck::cast_slice(indices);
        // Retain allocations across frames, growing geometrically only when the
        // scene exceeds the previous capacity. Empty frames retain the buffers.
        for (buffer, bytes, usage, label) in [
            (
                &mut self.vertex,
                vertex_bytes,
                wgpu::BufferUsages::VERTEX,
                "dynamic vertices",
            ),
            (
                &mut self.index,
                index_bytes,
                wgpu::BufferUsages::INDEX,
                "dynamic indices",
            ),
        ] {
            if bytes.len() as u64 > buffer.size() {
                *buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: (bytes.len() as u64).next_power_of_two(),
                    usage: usage | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(buffer, 0, bytes);
        }
        self.count = indices.len() as u32;
    }
    fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        self.draw_prefix(pass, self.count);
    }
    fn draw_prefix<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, count: u32) {
        self.draw_range(pass, 0..count);
    }
    fn draw_range<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, range: std::ops::Range<u32>) {
        let range = range.start.min(self.count)..range.end.min(self.count);
        if range.is_empty() {
            return;
        }
        pass.set_vertex_buffer(0, self.vertex.slice(..));
        pass.set_index_buffer(self.index.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(range, 0, 0..1);
    }
}
pub struct Renderer {
    surface: Option<wgpu::Surface<'static>>,
    capture_target: Option<wgpu::Texture>,
    timing: timing::GpuTiming,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    post: PostProcess,
    settings: RenderSettings,
    msaa_supported: bool,
    present_modes: Vec<wgpu::PresentMode>,
    shadow: wgpu::TextureView,
    shadow_texture: wgpu::Texture,
    sun_cache: CachedShadow,
    local_depth: wgpu::Texture,
    geometry_revision: u64,
    pub shadow_updates: usize,
    camera_buffer: wgpu::Buffer,
    pub materials: crate::materials::Materials,
    identity_instance: wgpu::Buffer,
    props: PropRenderer,
    volumes: VolumeRenderer,
    camera_group: wgpu::BindGroup,
    shadow_group: wgpu::BindGroup,
    pipeline: Vec<wgpu::RenderPipeline>,
    two_sided_pipeline: Vec<wgpu::RenderPipeline>,
    shadow_pipeline: wgpu::RenderPipeline,
    local_shadow_pipeline: wgpu::RenderPipeline,
    local_shadows: [LocalShadow; LOCAL_LIGHTS],
    sky_pipeline: Vec<wgpu::RenderPipeline>,
    chunks: BTreeMap<ChunkKey, GpuMesh>,
    objects: Option<GpuMesh>,
    dynamic: GpuMesh,
    pub egui: egui_wgpu::Renderer,
    pub triangles: usize,
    visible_world_triangles: usize,
    pub adapter_name: String,
}
impl Renderer {
    pub async fn new(window: Arc<Window>) -> Result<Self, String> {
        let size = window.inner_size();
        Self::create(Some(window), size.width.max(1), size.height.max(1)).await
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn headless(width: u32, height: u32) -> Result<Self, String> {
        Self::create(None, width, height).await
    }
    async fn create(window: Option<Arc<Window>>, width: u32, height: u32) -> Result<Self, String> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let surface = window
            .as_ref()
            .map(|window| instance.create_surface(window.clone()))
            .transpose()
            .map_err(|e| e.to_string())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: surface.as_ref(),
                force_fallback_adapter: false,
            })
            .await
            .ok_or("No compatible GPU was found. This build needs WebGPU in the browser.")?;
        let adapter_name = adapter.get_info().name;
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("Sauerteig"),
                    required_features: adapter.features() & wgpu::Features::TIMESTAMP_QUERY,
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::MemoryUsage,
                },
                None,
            )
            .await
            .map_err(|e| e.to_string())?;
        // Browser window sizing can settle while adapter/device creation awaits.
        let [width, height] = window.as_ref().map_or([width, height], |window| {
            let size = window.inner_size();
            [size.width.max(1), size.height.max(1)]
        });
        let caps = surface
            .as_ref()
            .map(|surface| surface.get_capabilities(&adapter));
        // egui outputs display-encoded color. Use the same unorm output for captures.
        let format = caps
            .as_ref()
            .map_or(wgpu::TextureFormat::Rgba8Unorm, |caps| {
                caps.formats
                    .iter()
                    .copied()
                    .find(|f| !f.is_srgb())
                    .unwrap_or(caps.formats[0])
            });
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 1,
            alpha_mode: caps
                .as_ref()
                .map_or(wgpu::CompositeAlphaMode::Opaque, |c| c.alpha_modes[0]),
            view_formats: vec![],
        };
        if let Some(surface) = &surface {
            surface.configure(&device, &config);
        }
        let capture_target = surface.is_none().then(|| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("headless output"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        });
        let timing = timing::GpuTiming::new(&device, &queue);
        let msaa_supported = [HDR_FORMAT, wgpu::TextureFormat::Depth32Float]
            .into_iter()
            .all(|format| {
                adapter
                    .get_texture_format_features(format)
                    .flags
                    .contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X4)
            })
            && adapter
                .get_texture_format_features(HDR_FORMAT)
                .flags
                .contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_RESOLVE);
        let settings = RenderSettings::default();
        let post = PostProcess::new(&device, width, height, format, settings, msaa_supported);
        let present_modes = caps.map_or_else(|| vec![wgpu::PresentMode::Fifo], |c| c.present_modes);
        let sample_counts: &[u32] = if msaa_supported { &[1, 4] } else { &[1] };
        let shadow_texture = shadows::depth_texture(&device, 2048, 1, "sun working shadows");
        let shadow = shadow_texture.create_view(&Default::default());
        let sun_cache = CachedShadow::new(&device, 2048);
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera and lights"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        let local_depth = shadows::depth_texture(
            &device,
            LOCAL_SHADOW_SIZE,
            LOCAL_LIGHTS as u32,
            "local working shadows",
        );
        let local_depth_array = local_depth.create_view(&wgpu::TextureViewDescriptor {
            label: Some("all fixture shadows"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let local_shadows = std::array::from_fn(|i| {
            let camera = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fixture shadow camera"),
                size: 64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("fixture shadow camera"),
                layout: &camera_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera.as_entire_binding(),
                }],
            });
            LocalShadow {
                cache: CachedShadow::new(&device, LOCAL_SHADOW_SIZE),
                position: None,
                depth: local_depth.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("fixture shadow layer"),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: i as u32,
                    array_layer_count: Some(1),
                    ..Default::default()
                }),
                camera,
                group,
            }
        });
        let shadow_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadows layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow comparison"),
            compare: Some(wgpu::CompareFunction::LessEqual),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let shadow_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadows"),
            layout: &shadow_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&shadow),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&local_depth_array),
                },
            ],
        });
        let materials = crate::materials::Materials::new(&device, &queue)?;
        let props = PropRenderer::new(&device)?;
        let volumes = VolumeRenderer::new(&device, HDR_FORMAT, &camera_layout, sample_counts);
        let identity_instance = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("world identity transform"),
            contents: bytemuck::cast_slice(&[[0.0f32; 4]]),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let shader = shaders::module(&device, "world");
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("world pipeline layout"),
            bind_group_layouts: &[&camera_layout, &shadow_layout, &materials.layout],
            push_constant_ranges: &[],
        });
        let world_pipeline = |sample_count, cull_mode| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("world"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[GpuVertex::layout(), instance_layout()],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: HDR_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    cull_mode,
                    ..Default::default()
                },
                depth_stencil: Some(depth_state()),
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    ..Default::default()
                },
                multiview: None,
                cache: None,
            })
        };
        let pipeline = sample_counts
            .iter()
            .map(|&n| world_pipeline(n, Some(wgpu::Face::Back)))
            .collect();
        let two_sided_pipeline = sample_counts
            .iter()
            .map(|&n| world_pipeline(n, None))
            .collect();
        let shadow_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("shadow pipeline layout"),
                bind_group_layouts: &[&camera_layout],
                push_constant_ranges: &[],
            });
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sun depth"),
            layout: Some(&shadow_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_shadow"),
                buffers: &[GpuVertex::layout(), instance_layout()],
                compilation_options: Default::default(),
            },
            fragment: None,
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                bias: wgpu::DepthBiasState {
                    constant: 1,
                    slope_scale: 0.0,
                    clamp: 0.0,
                },
                ..depth_state()
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let local_shader = shaders::module(&device, "shadow");
        let local_shadow_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("fixture depth"),
                layout: Some(&shadow_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &local_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[GpuVertex::layout(), instance_layout()],
                    compilation_options: Default::default(),
                },
                fragment: None,
                primitive: wgpu::PrimitiveState {
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    bias: wgpu::DepthBiasState {
                        constant: 1,
                        slope_scale: 0.0,
                        clamp: 0.0,
                    },
                    ..depth_state()
                }),
                multisample: Default::default(),
                multiview: None,
                cache: None,
            });
        let sky = shaders::module(&device, "sky");
        let sky_pipeline = sample_counts
            .iter()
            .map(|&sample_count| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("sky"),
                    layout: Some(&shadow_pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &sky,
                        entry_point: Some("vs_main"),
                        buffers: &[],
                        compilation_options: Default::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &sky,
                        entry_point: Some("fs_main"),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: HDR_FORMAT,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                        compilation_options: Default::default(),
                    }),
                    primitive: Default::default(),
                    depth_stencil: Some(wgpu::DepthStencilState {
                        depth_write_enabled: false,
                        ..depth_state()
                    }),
                    multisample: wgpu::MultisampleState {
                        count: sample_count,
                        ..Default::default()
                    },
                    multiview: None,
                    cache: None,
                })
            })
            .collect();
        let egui = egui_wgpu::Renderer::new(&device, format, None, 1, false);
        let dynamic = GpuMesh::dynamic(&device);
        Ok(Self {
            surface,
            capture_target,
            timing,
            device,
            queue,
            config,
            post,
            settings,
            msaa_supported,
            present_modes,
            shadow,
            shadow_texture,
            sun_cache,
            local_depth,
            geometry_revision: 0,
            shadow_updates: 0,
            camera_buffer,
            materials,
            identity_instance,
            props,
            volumes,
            camera_group,
            shadow_group,
            pipeline,
            two_sided_pipeline,
            shadow_pipeline,
            local_shadow_pipeline,
            local_shadows,
            sky_pipeline,
            chunks: BTreeMap::new(),
            objects: None,
            dynamic,
            egui,
            triangles: 0,
            visible_world_triangles: 0,
            adapter_name,
        })
    }
    pub fn apply_settings(&mut self, settings: RenderSettings) {
        let settings = settings.normalized();
        if settings.scale != self.settings.scale || settings.msaa != self.settings.msaa {
            self.post = PostProcess::new(
                &self.device,
                self.config.width,
                self.config.height,
                self.config.format,
                settings,
                self.msaa_supported,
            );
        }
        let mode = settings::present_mode(settings.vsync, &self.present_modes);
        if self.config.present_mode != mode {
            self.config.present_mode = mode;
            if let Some(surface) = &self.surface {
                surface.configure(&self.device, &self.config);
            }
        }
        self.settings = settings;
    }
    pub fn render_description(&self) -> String {
        format!(
            "{}x{} / {}x MSAA / {:?}",
            self.post.size[0], self.post.size[1], self.post.samples, self.config.present_mode
        )
    }
    pub fn gpu_times(&self) -> Option<&BTreeMap<String, f64>> {
        self.timing.supported().then_some(&self.timing.latest)
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn wait_for_capture(&mut self) -> std::time::Instant {
        self.device.poll(wgpu::Maintain::Wait);
        let completed = std::time::Instant::now();
        self.timing.collect(&self.device, &self.queue);
        self.device.poll(wgpu::Maintain::Wait);
        self.timing.collect(&self.device, &self.queue);
        completed
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save_capture(&self, path: &std::path::Path) -> Result<(), String> {
        image::save_buffer(
            path,
            &self.capture_pixels()?,
            self.config.width,
            self.config.height,
            image::ColorType::Rgba8,
        )
        .map_err(|e| e.to_string())
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn capture_pixels(&self) -> Result<Vec<u8>, String> {
        let texture = self
            .capture_target
            .as_ref()
            .ok_or("Not a headless renderer")?;
        let row = (self.config.width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture readback"),
            size: row as u64 * self.config.height as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(self.config.height),
                },
            },
            texture.size(),
        );
        self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let data = buffer.slice(..).get_mapped_range();
        let pixels: Vec<u8> = data
            .chunks(row as usize)
            .flat_map(|line| line[..self.config.width as usize * 4].iter().copied())
            .collect();
        Ok(pixels)
    }
    pub fn visible_triangles(&self) -> usize {
        self.visible_world_triangles
            + self.props.visible_triangles()
            + self.volumes.visible_triangles()
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        if let Some(surface) = &self.surface {
            surface.configure(&self.device, &self.config);
        }
        self.post = PostProcess::new(
            &self.device,
            width,
            height,
            self.config.format,
            self.settings,
            self.msaa_supported,
        );
    }
    pub fn rebuild_all(&mut self, world: &World) {
        self.geometry_revision = self.geometry_revision.wrapping_add(1);
        self.chunks.clear();
        self.rebuild(world, &world.chunk_keys());
        self.rebuild_volumes(world);
    }
    pub fn rebuild_volumes(&mut self, world: &World) {
        self.volumes.rebuild(&self.device, world);
    }
    pub fn rebuild(&mut self, world: &World, keys: &[ChunkKey]) {
        if !keys.is_empty() {
            self.geometry_revision = self.geometry_revision.wrapping_add(1);
        }
        for &key in keys {
            let Mesh { vertices, indices } = world.mesh_chunk(key);
            let gpu: Vec<_> = vertices
                .iter()
                .map(|v| GpuVertex {
                    position: v.position,
                    normal: v.normal,
                    color: [1.0; 3],
                    emission: 0.0,
                    uv: crate::materials::world_uv(v.position, v.normal, v.material),
                    material: v.material as u32,
                })
                .collect();
            if let Some(mesh) = GpuMesh::new(&self.device, &gpu, &indices) {
                self.chunks.insert(key, mesh);
            } else {
                self.chunks.remove(&key);
            }
        }
        self.triangles = self.chunks.values().map(|m| m.count as usize / 3).sum();
    }
    pub fn objects(&mut self, vertices: &[GpuVertex], indices: &[u32]) {
        self.objects = GpuMesh::new(&self.device, vertices, indices);
        self.geometry_revision = self.geometry_revision.wrapping_add(1);
    }
    // These inputs borrow the frame's game and UI data directly; a second state
    // container would only duplicate the caller's existing ownership.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        eye: Vec3,
        direction: Vec3,
        time: f32,
        world: &World,
        objects: &[WorldObject],
        vertices: &[GpuVertex],
        indices: &[u32],
        shadow_index_count: u32,
        opaque_index_count: u32,
        output: egui::FullOutput,
        context: &egui::Context,
    ) -> Result<(), wgpu::SurfaceError> {
        self.timing.begin(&self.device, &self.queue);
        let underwater =
            world.volume_material_at(eye) == Some(sauerteig_core::VolumeMaterial::Water);
        let two_sided_terrain = culling::camera_intersects_terrain(world, eye);
        let view = Mat4::look_to_rh(eye, direction, Vec3::Y);
        let proj = Mat4::perspective_rh(
            78.0_f32.to_radians(),
            self.config.width as f32 / self.config.height as f32,
            0.06,
            180.0,
        );
        let view_projection = proj * view;
        let camera_frustum = Frustum::new(view_projection);
        let sun = Vec3::new(-0.44, 0.76, 0.34).normalize();
        let sun_projection = sun_shadow_matrix(sun, eye);
        let sun_frustum = Frustum::new(sun_projection);
        let mut uniform = CameraUniform {
            view_proj: view_projection.to_cols_array_2d(),
            light_view_proj: sun_projection.to_cols_array_2d(),
            eye_time: [eye.x, eye.y, eye.z, time],
            sun: [sun.x, sun.y, sun.z, 0.0],
            fog: if underwater {
                [0.022, 0.105, 0.13, 0.0025]
            } else {
                [0.105, 0.125, 0.135, 0.00005]
            },
            points: [[0.0; 4]; 8],
            point_colors: [[0.0; 4]; 8],
            local_view_proj: [Mat4::IDENTITY.to_cols_array_2d(); LOCAL_LIGHTS],
        };
        self.visible_world_triangles = self
            .chunks
            .values()
            .filter(|mesh| camera_frustum.intersects(mesh.bounds))
            .map(|mesh| mesh.count as usize / 3)
            .sum();
        let nearby = shadows::stable_slots(
            closest_lights(objects, eye),
            self.local_shadows.each_ref().map(|s| s.position),
        );
        for (shadow, light) in self.local_shadows.iter_mut().zip(nearby) {
            shadow.position = light.map(|l| l.position);
        }
        self.props
            .prepare(&self.queue, objects, eye, &nearby, &sun_frustum);
        self.volumes
            .prepare(&self.queue, eye, direction, &camera_frustum);
        for (i, light) in nearby.iter().enumerate() {
            if let Some(light) = light {
                let p = light.position;
                uniform.points[i] = [p.x, p.y, p.z, 40.0];
                uniform.point_colors[i] = [1.0, 0.74, 0.46, LIGHT_RANGE];
                uniform.local_view_proj[i] = light.view_proj.to_cols_array_2d();
                self.queue.write_buffer(
                    &self.local_shadows[i].camera,
                    0,
                    bytemuck::bytes_of(&uniform.local_view_proj[i]),
                );
            }
        }
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&uniform));
        let frame = self
            .surface
            .as_ref()
            .map(|s| s.get_current_texture())
            .transpose()?;
        let texture = frame
            .as_ref()
            .map(|f| &f.texture)
            .or(self.capture_target.as_ref())
            .unwrap();
        let target = texture.create_view(&Default::default());
        self.dynamic
            .update(&self.device, &self.queue, vertices, indices);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        let paint_jobs = context.tessellate(output.shapes, output.pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: output.pixels_per_point,
        };
        for (id, delta) in &output.textures_delta.set {
            self.egui
                .update_texture(&self.device, &self.queue, *id, delta);
        }
        let commands = self.egui.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &paint_jobs,
            &screen,
        );
        self.shadow_updates = 0;
        if self
            .sun_cache
            .stamp
            .update(sun_projection, self.geometry_revision)
        {
            self.shadow_updates += 1;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sun static"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.sun_cache.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: self.timing.pass("sun static"),
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.shadow_pipeline);
            pass.set_bind_group(0, &self.camera_group, &[]);
            pass.set_vertex_buffer(1, self.identity_instance.slice(..));
            for mesh in self.chunks.values() {
                if sun_frustum.intersects(mesh.bounds) {
                    mesh.draw(&mut pass);
                }
            }
            if let Some(mesh) = &self.objects {
                mesh.draw(&mut pass);
            }
            self.props.draw_shadow(&mut pass, None);
        }
        self.sun_cache
            .copy_to(&mut encoder, &self.shadow_texture, 0);
        if shadow_index_count > 0 {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sun actors"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.shadow,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: self.timing.pass("sun actors"),
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.shadow_pipeline);
            pass.set_bind_group(0, &self.camera_group, &[]);
            pass.set_vertex_buffer(1, self.identity_instance.slice(..));
            self.dynamic.draw_prefix(&mut pass, shadow_index_count);
        }
        for (i, light) in nearby.iter().enumerate() {
            let Some(light) = light else {
                continue;
            };
            let shadow = &mut self.local_shadows[i];
            if shadow
                .cache
                .stamp
                .update(light.view_proj, self.geometry_revision)
            {
                self.shadow_updates += 1;
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("local static"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &shadow.cache.depth,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: self.timing.pass(shadows::STATIC_LABELS[i]),
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&self.local_shadow_pipeline);
                pass.set_bind_group(0, &shadow.group, &[]);
                pass.set_vertex_buffer(1, self.identity_instance.slice(..));
                let light_frustum = Frustum::new(light.view_proj);
                for mesh in self.chunks.values() {
                    if mesh.bounds.within_radius(light.position, LIGHT_RANGE)
                        && light_frustum.intersects(mesh.bounds)
                    {
                        mesh.draw(&mut pass);
                    }
                }
                if let Some(mesh) = &self.objects {
                    mesh.draw(&mut pass);
                }
                self.props.draw_shadow(&mut pass, Some(i));
            }
            // Copy the static map afresh so last frame's actor shadows cannot persist.
            shadow
                .cache
                .copy_to(&mut encoder, &self.local_depth, i as u32);
            if shadow_index_count > 0 {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("local actors"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &shadow.depth,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: self.timing.pass(shadows::ACTOR_LABELS[i]),
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&self.local_shadow_pipeline);
                pass.set_bind_group(0, &shadow.group, &[]);
                pass.set_vertex_buffer(1, self.identity_instance.slice(..));
                self.dynamic.draw_prefix(&mut pass, shadow_index_count);
            }
        }
        {
            let sample_index = usize::from(self.post.samples == 4);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self.post.multisampled.as_ref().unwrap_or(&self.post.scene),
                    resolve_target: self.post.multisampled.as_ref().map(|_| &self.post.scene),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.2,
                            g: 0.25,
                            b: 0.28,
                            a: 1.0,
                        }),
                        store: if self.post.samples > 1 {
                            wgpu::StoreOp::Discard
                        } else {
                            wgpu::StoreOp::Store
                        },
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.post.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: self.timing.pass("scene"),
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.sky_pipeline[sample_index]);
            pass.set_bind_group(0, &self.camera_group, &[]);
            pass.draw(0..3, 0..1);
            pass.set_pipeline(if two_sided_terrain {
                &self.two_sided_pipeline[sample_index]
            } else {
                &self.pipeline[sample_index]
            });
            pass.set_bind_group(0, &self.camera_group, &[]);
            pass.set_bind_group(1, &self.shadow_group, &[]);
            pass.set_bind_group(2, &self.materials.group, &[]);
            pass.set_vertex_buffer(1, self.identity_instance.slice(..));
            for mesh in self.chunks.values() {
                if camera_frustum.intersects(mesh.bounds) {
                    mesh.draw(&mut pass);
                }
            }
            pass.set_pipeline(&self.two_sided_pipeline[sample_index]);
            if let Some(mesh) = &self.objects {
                mesh.draw(&mut pass);
            }
            self.dynamic.draw_prefix(&mut pass, opaque_index_count);
            self.props.draw(&mut pass);
            self.volumes.draw(&mut pass, underwater, sample_index);
            // Guides and the first-person weapon remain readable through the
            // material tint. Remote actors and physical effects were drawn
            // before the transparent boundaries and retain normal occlusion.
            pass.set_pipeline(&self.two_sided_pipeline[sample_index]);
            pass.set_bind_group(0, &self.camera_group, &[]);
            pass.set_bind_group(1, &self.shadow_group, &[]);
            pass.set_bind_group(2, &self.materials.group, &[]);
            pass.set_vertex_buffer(1, self.identity_instance.slice(..));
            self.dynamic
                .draw_range(&mut pass, opaque_index_count..self.dynamic.count);
        }
        self.post.draw(
            &mut encoder,
            &self.queue,
            &target,
            self.settings,
            self.config.format.is_srgb(),
            &mut self.timing,
        );
        if !paint_jobs.is_empty() {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("interface"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: self.timing.pass("egui"),
                occlusion_query_set: None,
            });
            self.egui
                .render(&mut pass.forget_lifetime(), &paint_jobs, &screen);
        }
        self.queue.submit(
            commands
                .into_iter()
                .chain(std::iter::once(encoder.finish())),
        );
        self.timing.submitted(&self.queue);
        if let Some(frame) = frame {
            frame.present();
        }
        for id in &output.textures_delta.free {
            self.egui.free_texture(id);
        }
        Ok(())
    }
}
fn depth_state() -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: true,
        depth_compare: wgpu::CompareFunction::LessEqual,
        stencil: Default::default(),
        bias: Default::default(),
    }
}
