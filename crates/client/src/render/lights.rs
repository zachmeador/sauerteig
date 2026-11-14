//! GPU resources for clustered lighting: the light and mask buffers, the
//! culling pass that fills the masks, and the bind group the scene shaders read.
use super::{
    lightset::{FramePlan, LightConfig},
    shaders::{self, LightLayout},
    timing::GpuTiming,
};
use crate::lighting::GpuLight;
use bytemuck::{Pod, Zeroable};
use glam::Vec2;

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
    pub fn new(
        config: &LightConfig,
        view: ClusterView,
        count: usize,
        page_classes: u32,
        brute_force: bool,
    ) -> Self {
        let grid = config.grid;
        Self {
            grid: [grid.dims[0], grid.dims[1], grid.dims[2], count as u32],
            depth: [grid.near, grid.split, grid.ratio(), grid.inv_log_ratio()],
            screen: [view.size.x, view.size.y, view.scale.x, view.scale.y],
            range: [grid.far, 0.0, 0.0, 0.0],
            mode: [
                u32::from(!brute_force),
                page_classes,
                config.pages as u32,
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
    #[cfg(test)]
    masks: wgpu::Buffer,
}

pub(super) struct LightGpu {
    config: LightConfig,
    layout: LightLayout,
    pub bind_layout: wgpu::BindGroupLayout,
    pub group: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    atlas: wgpu::TextureView,
    lights: wgpu::Buffer,
    masks: wgpu::Buffer,
    params: wgpu::Buffer,
    cull: Cull,
    pub count: usize,
    /// View-space spheres and projection scale of the last upload.
    pub last_views: Vec<[f32; 4]>,
    last_scale: Vec2,
}

impl LightGpu {
    pub fn new(
        device: &wgpu::Device,
        config: LightConfig,
        sun: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
        atlas: &wgpu::TextureView,
    ) -> Self {
        let layout = LightLayout::new(&config);
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
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lights and shadows layout"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                ),
                entry(
                    1,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                ),
                entry(
                    2,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                ),
                entry(3, buffer(buffer_type)),
                entry(4, buffer(buffer_type)),
                entry(5, buffer(wgpu::BufferBindingType::Uniform)),
            ],
        });
        let light_bytes = (layout.max_lights as usize * std::mem::size_of::<GpuLight>()) as u64;
        let grid = config.grid;
        let mask_bytes = (grid.count() * grid.words as usize * 4) as u64;
        let usage = |extra| extra | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::STORAGE;
        let make = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let lights = make("lights", light_bytes, usage(wgpu::BufferUsages::empty()));
        let masks = make(
            "cluster masks",
            mask_bytes,
            usage(wgpu::BufferUsages::COPY_SRC),
        );
        let params = make(
            "light parameters",
            std::mem::size_of::<LightParams>() as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let group = Self::bind(
            device,
            &bind_layout,
            [sun, atlas],
            sampler,
            [&lights, &masks, &params],
        );
        let cull = Cull::new(device, config, layout, &params, &masks);
        Self {
            config,
            layout,
            bind_layout,
            group,
            sampler: sampler.clone(),
            atlas: atlas.clone(),
            lights,
            masks,
            params,
            cull,
            count: 0,
            last_views: Vec::new(),
            last_scale: Vec2::ONE,
        }
    }

    fn bind(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        [sun, atlas]: [&wgpu::TextureView; 2],
        sampler: &wgpu::Sampler,
        [lights, masks, params]: [&wgpu::Buffer; 3],
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
            ],
        })
    }

    /// Point the scene shaders at new sun cascade maps.
    pub fn set_sun(&mut self, device: &wgpu::Device, sun: &wgpu::TextureView) {
        self.group = Self::bind(
            device,
            &self.bind_layout,
            [sun, &self.atlas],
            &self.sampler,
            [&self.lights, &self.masks, &self.params],
        );
    }

    /// Write this frame's lights and parameters.
    pub fn upload(
        &mut self,
        queue: &wgpu::Queue,
        plan: &FramePlan,
        view: ClusterView,
        brute_force: bool,
    ) {
        let count = plan.lights.len().min(self.layout.max_lights as usize);
        self.count = count;
        let records: Vec<GpuLight> = plan.lights[..count].iter().map(|l| l.gpu).collect();
        if count > 0 {
            queue.write_buffer(&self.lights, 0, bytemuck::cast_slice(&records));
        }
        let params = LightParams::new(&self.config, view, count, plan.page_classes, brute_force);
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
        grid.build_masks(self.last_scale, &spheres)
            .chunks(grid.words as usize)
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
        &self.cull.masks
    }
}

impl Cull {
    fn new(
        device: &wgpu::Device,
        config: LightConfig,
        layout: LightLayout,
        params: &wgpu::Buffer,
        masks: &wgpu::Buffer,
    ) -> Self {
        let grid = config.grid;
        let light_view = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("light view spheres"),
            size: (layout.max_lights as usize * 16) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
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
                    resource: masks.as_entire_binding(),
                },
            ],
        });
        let module = shaders::module(device, "clusters", layout);
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
            #[cfg(test)]
            masks: masks.clone(),
        }
    }
}
