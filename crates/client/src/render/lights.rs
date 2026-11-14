//! GPU resources for clustered lighting: the light and mask buffers, the
//! culling pass that fills the masks, and the bind group the scene shaders read.
use super::{
    clusters::ClusterGrid,
    lightset::{FramePlan, LightingTier, TierConfig},
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
        config: &TierConfig,
        view: ClusterView,
        count: usize,
        page_classes: u32,
        brute_force: bool,
    ) -> Self {
        let grid = config.grid.unwrap_or(ClusterGrid::COMPACT);
        let clustered = config.grid.is_some() && !brute_force;
        Self {
            grid: [grid.dims[0], grid.dims[1], grid.dims[2], count as u32],
            depth: [grid.near, grid.split, grid.ratio(), grid.inv_log_ratio()],
            screen: [view.size.x, view.size.y, view.scale.x, view.scale.y],
            range: [grid.far, 0.0, 0.0, 0.0],
            mode: [
                u32::from(clustered),
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
}

pub(super) struct LightGpu {
    config: TierConfig,
    layout: LightLayout,
    pub bind_layout: wgpu::BindGroupLayout,
    pub group: wgpu::BindGroup,
    lights: wgpu::Buffer,
    masks: wgpu::Buffer,
    params: wgpu::Buffer,
    cull: Option<Cull>,
    cpu_masks: Vec<u32>,
    pub count: usize,
    /// View-space spheres and projection scale of the last upload.
    pub last_views: Vec<[f32; 4]>,
    last_scale: Vec2,
}

impl LightGpu {
    pub fn new(
        device: &wgpu::Device,
        config: TierConfig,
        sun: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
        atlas: &wgpu::TextureView,
    ) -> Self {
        let layout = LightLayout::for_tier(&config);
        let buffer_type = if layout.storage {
            wgpu::BufferBindingType::Storage { read_only: true }
        } else {
            wgpu::BufferBindingType::Uniform
        };
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
                        view_dimension: wgpu::TextureViewDimension::D2,
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
        let mask_bytes = if layout.storage {
            let grid = config.grid.expect("clustered tiers have a grid");
            (grid.count() * grid.words as usize * 4) as u64
        } else {
            u64::from(layout.mask_vectors) * 16
        };
        let usage = |extra| {
            extra
                | wgpu::BufferUsages::COPY_DST
                | if layout.storage {
                    wgpu::BufferUsages::STORAGE
                } else {
                    wgpu::BufferUsages::UNIFORM
                }
        };
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
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lights and shadows"),
            layout: &bind_layout,
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
        });
        let cull = (config.tier == LightingTier::Clustered)
            .then(|| Cull::new(device, config, layout, &params, &masks));
        Self {
            config,
            layout,
            bind_layout,
            group,
            lights,
            masks,
            params,
            cull,
            cpu_masks: Vec::new(),
            count: 0,
            last_views: Vec::new(),
            last_scale: Vec2::ONE,
        }
    }

    /// Write this frame's lights and parameters. Without a culling pass the
    /// masks are built here.
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
        if let Some(cull) = &self.cull {
            if count > 0 {
                queue.write_buffer(&cull.light_view, 0, bytemuck::cast_slice(&views));
            }
        } else if let (Some(grid), true) = (self.config.grid, count > 0) {
            let spheres: Vec<_> = views.iter().map(|&v| glam::Vec4::from_array(v)).collect();
            self.cpu_masks = grid.build_masks(view.scale, &spheres);
            self.cpu_masks
                .resize(self.layout.mask_vectors as usize * 4, 0);
            queue.write_buffer(&self.masks, 0, bytemuck::cast_slice(&self.cpu_masks));
        }
        self.last_views = views;
        self.last_scale = view.scale;
    }

    /// How many of the last frame's lights reach each cluster.
    pub fn cluster_counts(&self) -> Vec<u32> {
        let Some(grid) = self.config.grid else {
            return Vec::new();
        };
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
        let Some(cull) = &self.cull else { return };
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
        &self.masks
    }
}

impl Cull {
    fn new(
        device: &wgpu::Device,
        config: TierConfig,
        layout: LightLayout,
        params: &wgpu::Buffer,
        masks: &wgpu::Buffer,
    ) -> Self {
        let grid = config.grid.expect("clustered tiers have a grid");
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
        }
    }
}
