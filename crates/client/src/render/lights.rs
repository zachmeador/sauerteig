//! GPU resources for clustered lighting: the light and mask buffers, the
//! culling pass that fills the masks, and the bind group the scene shaders read.
use super::{
    clusters::ClusterGrid,
    device::MAX_ARRAY_LAYERS,
    lightset::{FramePlan, LightConfig, MAX_LIGHTS_IN_VIEW},
    shaders,
    timing::GpuTiming,
};
use crate::lighting::GpuLight;
use bytemuck::{Pod, Zeroable};
use glam::Vec2;

/// Lights the buffers hold before the first frame needs more.
const MIN_CAPACITY: usize = 256;

/// What the shaders need to find a fragment's cluster and read its lights.
/// Mirrors `LightParams` in `common.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct LightParams {
    pub grid: [u32; 4],
    pub depth: [f32; 4],
    pub screen: [f32; 4],
    pub range: [f32; 4],
    pub mode: [u32; 4],
}

/// The scene target and projection the clusters are laid over.
#[derive(Clone, Copy, Debug)]
pub struct ClusterView {
    pub size: Vec2,
    /// Projection x and y scales.
    pub scale: Vec2,
}

impl LightParams {
    pub fn new(config: &LightConfig, view: ClusterView, count: usize, brute_force: bool) -> Self {
        let grid = config.grid;
        Self {
            grid: [grid.dims[0], grid.dims[1], grid.dims[2], count as u32],
            depth: [grid.near, grid.split, grid.ratio(), grid.inv_log_ratio()],
            screen: [view.size.x, view.size.y, view.scale.x, view.scale.y],
            range: [grid.far, 0.0, 0.0, 0.0],
            mode: [
                u32::from(!brute_force),
                ClusterGrid::mask_words(count) as u32,
                0,
                config.light_cap,
            ],
        }
    }
}

struct Cull {
    pipeline: wgpu::ComputePipeline,
    group: wgpu::BindGroup,
    light_view: wgpu::Buffer,
    workgroups: u32,
}

/// The buffers that grow with the number of lights in view.
struct Buffers {
    capacity: usize,
    lights: wgpu::Buffer,
    masks: wgpu::Buffer,
}

pub(super) struct LightGpu {
    config: LightConfig,
    pub bind_layout: wgpu::BindGroupLayout,
    pub group: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    sun: wgpu::TextureView,
    atlas: wgpu::TextureView,
    frame_atlas: wgpu::TextureView,
    buffers: Buffers,
    params: wgpu::Buffer,
    page_table: wgpu::Buffer,
    cull: Cull,
    pub count: usize,
    /// View-space spheres and projection scale of the last upload.
    pub last_views: Vec<[f32; 4]>,
    last_scale: Vec2,
}

fn make_buffer(
    device: &wgpu::Device,
    label: &str,
    size: usize,
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size as u64,
        usage,
        mapped_at_creation: false,
    })
}

impl Buffers {
    fn new(device: &wgpu::Device, grid: ClusterGrid, capacity: usize) -> Self {
        let usage = wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::STORAGE;
        let mask_bytes = grid.count() * ClusterGrid::mask_words(capacity) * 4;
        Self {
            capacity,
            lights: make_buffer(
                device,
                "lights",
                capacity * std::mem::size_of::<GpuLight>(),
                usage,
            ),
            masks: make_buffer(
                device,
                "cluster masks",
                mask_bytes,
                usage | wgpu::BufferUsages::COPY_SRC,
            ),
        }
    }
}

impl LightGpu {
    pub fn new(
        device: &wgpu::Device,
        config: LightConfig,
        sun: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
        [atlas, frame_atlas]: [&wgpu::TextureView; 2],
    ) -> Self {
        let buffer_type = wgpu::BufferBindingType::Storage { read_only: true };
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty,
            count: None,
        };
        let buffer = |ty| wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let depth_array = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Depth,
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        };
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lights and shadows layout"),
            entries: &[
                entry(0, depth_array),
                entry(
                    1,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                ),
                entry(2, depth_array),
                entry(3, buffer(buffer_type)),
                entry(4, buffer(buffer_type)),
                entry(5, buffer(wgpu::BufferBindingType::Uniform)),
                entry(6, buffer(buffer_type)),
                entry(7, depth_array),
            ],
        });
        let grid = config.grid;
        let buffers = Buffers::new(device, grid, MIN_CAPACITY);
        let params = make_buffer(
            device,
            "light parameters",
            std::mem::size_of::<LightParams>(),
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let page_table = make_buffer(
            device,
            "shadow page table",
            MAX_ARRAY_LAYERS as usize * 2 * 4,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let group = Self::bind(
            device,
            &bind_layout,
            [sun, atlas, frame_atlas],
            sampler,
            [&buffers.lights, &buffers.masks, &params, &page_table],
        );
        let cull = Cull::new(device, config, &params, &buffers);
        Self {
            config,
            bind_layout,
            group,
            sampler: sampler.clone(),
            sun: sun.clone(),
            atlas: atlas.clone(),
            frame_atlas: frame_atlas.clone(),
            buffers,
            params,
            page_table,
            cull,
            count: 0,
            last_views: Vec::new(),
            last_scale: Vec2::ONE,
        }
    }

    fn bind(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        [sun, atlas, frame_atlas]: [&wgpu::TextureView; 3],
        sampler: &wgpu::Sampler,
        [lights, masks, params, page_table]: [&wgpu::Buffer; 4],
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lights and shadows"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(sun),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(atlas),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: lights.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: masks.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: page_table.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(frame_atlas),
                },
            ],
        })
    }

    fn rebind(&mut self, device: &wgpu::Device) {
        self.group = Self::bind(
            device,
            &self.bind_layout,
            [&self.sun, &self.atlas, &self.frame_atlas],
            &self.sampler,
            [
                &self.buffers.lights,
                &self.buffers.masks,
                &self.params,
                &self.page_table,
            ],
        );
    }

    /// Point the scene shaders at new sun cascade maps.
    pub fn set_sun(&mut self, device: &wgpu::Device, sun: &wgpu::TextureView) {
        self.sun = sun.clone();
        self.rebind(device);
    }

    /// Point the scene shaders at a grown atlas or per-frame texture.
    pub fn set_atlas(
        &mut self,
        device: &wgpu::Device,
        atlas: &wgpu::TextureView,
        frame_atlas: &wgpu::TextureView,
    ) {
        self.atlas = atlas.clone();
        self.frame_atlas = frame_atlas.clone();
        self.rebind(device);
    }

    /// Make the light and mask buffers hold `count` lights.
    fn reserve(&mut self, device: &wgpu::Device, count: usize) {
        if count <= self.buffers.capacity {
            return;
        }
        let capacity = count.next_power_of_two().max(MIN_CAPACITY);
        self.buffers = Buffers::new(device, self.config.grid, capacity);
        self.cull = Cull::new(device, self.config, &self.params, &self.buffers);
        self.rebind(device);
    }

    /// Bytes of the buffers that grow with the lights in view.
    pub fn buffer_bytes(&self) -> usize {
        let b = &self.buffers;
        (b.lights.size() + b.masks.size() + self.cull.light_view.size()) as usize
    }

    /// Write this frame's lights, page table and parameters.
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: &FramePlan,
        view: ClusterView,
        brute_force: bool,
    ) {
        let count = plan.lights.len().min(MAX_LIGHTS_IN_VIEW);
        self.reserve(device, count);
        self.count = count;
        let records: Vec<GpuLight> = plan.lights[..count].iter().map(|l| l.gpu).collect();
        if count > 0 {
            queue.write_buffer(&self.buffers.lights, 0, bytemuck::cast_slice(&records));
        }
        if !plan.page_classes.is_empty() {
            queue.write_buffer(
                &self.page_table,
                0,
                bytemuck::cast_slice(&plan.page_classes),
            );
        }
        if !plan.frame_page_classes.is_empty() {
            queue.write_buffer(
                &self.page_table,
                u64::from(MAX_ARRAY_LAYERS) * 4,
                bytemuck::cast_slice(&plan.frame_page_classes),
            );
        }
        let params = LightParams::new(&self.config, view, count, brute_force);
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        let views: Vec<[f32; 4]> = plan.lights[..count].iter().map(|l| l.view).collect();
        if count > 0 {
            queue.write_buffer(&self.cull.light_view, 0, bytemuck::cast_slice(&views));
        }
        self.last_views = views;
        self.last_scale = view.scale;
    }

    /// How many of the last frame's lights reach each cluster.
    pub fn cluster_counts(&self) -> Vec<u32> {
        let grid = self.config.grid;
        let spheres: Vec<_> = self
            .last_views
            .iter()
            .map(|&v| glam::Vec4::from_array(v))
            .collect();
        let words = ClusterGrid::mask_words(spheres.len()).max(1);
        grid.build_masks(self.last_scale, &spheres)
            .chunks(words)
            .map(|words| words.iter().map(|w| w.count_ones()).sum())
            .collect()
    }

    /// Fill the cluster masks on the GPU.
    pub fn cull(&self, encoder: &mut wgpu::CommandEncoder, timing: &mut GpuTiming) {
        let cull = &self.cull;
        if self.count == 0 {
            return;
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("light cull"),
            timestamp_writes: timing.compute_pass("light cull"),
        });
        pass.set_pipeline(&cull.pipeline);
        pass.set_bind_group(0, &cull.group, &[]);
        pass.dispatch_workgroups(cull.workgroups, 1, 1);
    }

    #[cfg(test)]
    pub fn mask_buffer(&self) -> &wgpu::Buffer {
        &self.buffers.masks
    }
}

impl Cull {
    fn new(
        device: &wgpu::Device,
        config: LightConfig,
        params: &wgpu::Buffer,
        buffers: &Buffers,
    ) -> Self {
        let grid = config.grid;
        let light_view = make_buffer(
            device,
            "light view spheres",
            buffers.capacity * 16,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let storage = |read_only| wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("light cull layout"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                entry(1, storage(true)),
                entry(2, storage(false)),
            ],
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("light cull"),
            layout: &bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: light_view.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buffers.masks.as_entire_binding(),
                },
            ],
        });
        let module = shaders::module(device, "clusters");
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("light cull pipeline layout"),
            bind_group_layouts: &[&bind_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("light cull"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("cs_cull"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            pipeline,
            group,
            light_view,
            workgroups: (grid.count() as u32).div_ceil(64),
        }
    }
}
