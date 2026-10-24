mod props;

use self::props::{instance_layout, PropRenderer};
use crate::lighting::{
    chunk_in_light, closest_lights, sun_shadow_matrix, LIGHT_RANGE, LOCAL_LIGHTS, LOCAL_SHADOW_SIZE,
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
    depth: wgpu::TextureView,
    camera: wgpu::Buffer,
    group: wgpu::BindGroup,
}
pub struct GpuMesh {
    vertex: wgpu::Buffer,
    index: wgpu::Buffer,
    count: u32,
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
        let count = count.min(self.count);
        if count == 0 {
            return;
        }
        pass.set_vertex_buffer(0, self.vertex.slice(..));
        pass.set_index_buffer(self.index.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..count, 0, 0..1);
    }
}
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    depth: wgpu::TextureView,
    shadow: wgpu::TextureView,
    camera_buffer: wgpu::Buffer,
    pub materials: crate::materials::Materials,
    identity_instance: wgpu::Buffer,
    props: PropRenderer,
    camera_group: wgpu::BindGroup,
    shadow_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    local_shadow_pipeline: wgpu::RenderPipeline,
    local_shadows: [LocalShadow; LOCAL_LIGHTS],
    sky_pipeline: wgpu::RenderPipeline,
    chunks: BTreeMap<ChunkKey, GpuMesh>,
    objects: Option<GpuMesh>,
    dynamic: GpuMesh,
    pub egui: egui_wgpu::Renderer,
    pub triangles: usize,
    pub adapter_name: String,
}
impl Renderer {
    pub async fn new(window: Arc<Window>) -> Result<Self, String> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| e.to_string())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or("No compatible GPU was found. This build needs WebGPU in the browser.")?;
        let adapter_name = adapter.get_info().name;
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("Sauerteig"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::MemoryUsage,
                },
                None,
            )
            .await
            .map_err(|e| e.to_string())?;
        let caps = surface.get_capabilities(&adapter);
        // egui outputs display-encoded color. Prefer an unorm surface and encode
        // the world's linear lighting explicitly, matching browser and native.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);
        let depth = depth_texture(&device, config.width, config.height, "scene depth");
        let shadow = depth_texture(&device, 2048, 2048, "sun shadows");
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
        let local_depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fixture shadow array"),
            size: wgpu::Extent3d {
                width: LOCAL_SHADOW_SIZE,
                height: LOCAL_SHADOW_SIZE,
                depth_or_array_layers: LOCAL_LIGHTS as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
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
        let identity_instance = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("world identity transform"),
            contents: bytemuck::cast_slice(&[[0.0f32; 4]]),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("world.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("world pipeline layout"),
            bind_group_layouts: &[&camera_layout, &shadow_layout, &materials.layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(depth_state()),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
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
        let local_shader = device.create_shader_module(wgpu::include_wgsl!("shadow.wgsl"));
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
        let sky = device.create_shader_module(wgpu::include_wgsl!("sky.wgsl"));
        let sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
                    format,
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
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let egui = egui_wgpu::Renderer::new(&device, format, None, 1, false);
        let dynamic = GpuMesh::dynamic(&device);
        Ok(Self {
            surface,
            device,
            queue,
            config,
            depth,
            shadow,
            camera_buffer,
            materials,
            identity_instance,
            props,
            camera_group,
            shadow_group,
            pipeline,
            shadow_pipeline,
            local_shadow_pipeline,
            local_shadows,
            sky_pipeline,
            chunks: BTreeMap::new(),
            objects: None,
            dynamic,
            egui,
            triangles: 0,
            adapter_name,
        })
    }
    pub fn visible_triangles(&self) -> usize {
        self.triangles + self.props.visible_triangles()
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.depth = depth_texture(&self.device, width, height, "scene depth");
    }
    pub fn rebuild_all(&mut self, world: &World) {
        self.chunks.clear();
        self.rebuild(world, &world.chunk_keys());
    }
    pub fn rebuild(&mut self, world: &World, keys: &[ChunkKey]) {
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
    }
    // These inputs borrow the frame's game and UI data directly; a second state
    // container would only duplicate the caller's existing ownership.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        eye: Vec3,
        direction: Vec3,
        time: f32,
        objects: &[WorldObject],
        vertices: &[GpuVertex],
        indices: &[u32],
        shadow_index_count: u32,
        output: egui::FullOutput,
        context: &egui::Context,
    ) -> Result<(), wgpu::SurfaceError> {
        let view = Mat4::look_to_rh(eye, direction, Vec3::Y);
        let proj = Mat4::perspective_rh(
            78.0_f32.to_radians(),
            self.config.width as f32 / self.config.height as f32,
            0.06,
            180.0,
        );
        let sun = Vec3::new(-0.44, 0.76, 0.34).normalize();
        let encode_srgb = if self.config.format.is_srgb() {
            0.0
        } else {
            1.0
        };
        let mut uniform = CameraUniform {
            view_proj: (proj * view).to_cols_array_2d(),
            light_view_proj: sun_shadow_matrix(sun).to_cols_array_2d(),
            eye_time: [eye.x, eye.y, eye.z, time],
            sun: [sun.x, sun.y, sun.z, encode_srgb],
            fog: [0.105, 0.125, 0.135, 0.00005],
            points: [[0.0; 4]; 8],
            point_colors: [[0.0; 4]; 8],
            local_view_proj: [Mat4::IDENTITY.to_cols_array_2d(); LOCAL_LIGHTS],
        };
        let nearby = closest_lights(objects, eye);
        self.props.prepare(&self.queue, objects, eye, &nearby);
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
        let frame = self.surface.get_current_texture()?;
        let target = frame.texture.create_view(&Default::default());
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
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sun shadow pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.shadow,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.shadow_pipeline);
            pass.set_bind_group(0, &self.camera_group, &[]);
            pass.set_vertex_buffer(1, self.identity_instance.slice(..));
            for mesh in self.chunks.values() {
                mesh.draw(&mut pass);
            }
            if let Some(mesh) = &self.objects {
                mesh.draw(&mut pass);
            }
            self.dynamic.draw_prefix(&mut pass, shadow_index_count);
            self.props.draw(&mut pass, None);
        }
        // Eight broad fixture views, not six cubemap faces per light. Geometry
        // and moving actors update these maps on every frame, including edits.
        for (i, light) in nearby.iter().enumerate() {
            let Some(light) = light else {
                continue;
            };
            let shadow = &self.local_shadows[i];
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fixture shadow pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &shadow.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.local_shadow_pipeline);
            pass.set_bind_group(0, &shadow.group, &[]);
            pass.set_vertex_buffer(1, self.identity_instance.slice(..));
            for (key, mesh) in &self.chunks {
                if chunk_in_light(*key, light) {
                    mesh.draw(&mut pass);
                }
            }
            if let Some(mesh) = &self.objects {
                mesh.draw(&mut pass);
            }
            self.dynamic.draw_prefix(&mut pass, shadow_index_count);
            self.props.draw(&mut pass, Some(i));
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.2,
                            g: 0.25,
                            b: 0.28,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.sky_pipeline);
            pass.set_bind_group(0, &self.camera_group, &[]);
            pass.draw(0..3, 0..1);
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_group, &[]);
            pass.set_bind_group(1, &self.shadow_group, &[]);
            pass.set_bind_group(2, &self.materials.group, &[]);
            pass.set_vertex_buffer(1, self.identity_instance.slice(..));
            for mesh in self.chunks.values() {
                mesh.draw(&mut pass);
            }
            if let Some(mesh) = &self.objects {
                mesh.draw(&mut pass);
            }
            self.dynamic.draw(&mut pass);
            self.props.draw(&mut pass, None);
        }
        {
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
                timestamp_writes: None,
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
        frame.present();
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
fn depth_texture(device: &wgpu::Device, width: u32, height: u32, label: &str) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

#[cfg(test)]
mod material_shader_tests {
    #[test]
    fn textured_instanced_world_and_shadow_shaders_validate() {
        for shader in [include_str!("world.wgsl"), include_str!("shadow.wgsl")] {
            let module = naga::front::wgsl::parse_str(shader).unwrap();
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::empty(),
            )
            .validate(&module)
            .unwrap();
        }
    }
}
