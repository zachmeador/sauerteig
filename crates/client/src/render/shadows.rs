use super::{
    atlas::PAGE_SIZE,
    lightset::{OverlayJob, TileJob, OVERLAY_CLASS},
    timing::GpuTiming,
};
use glam::Mat4;

#[derive(Default)]
pub(super) struct ShadowStamp {
    matrix: Option<Mat4>,
    revision: u64,
}
impl ShadowStamp {
    pub fn update(&mut self, matrix: Mat4, revision: u64) -> bool {
        let changed = self.matrix != Some(matrix) || self.revision != revision;
        self.matrix = Some(matrix);
        self.revision = revision;
        changed
    }
}

/// The sun's cached static map; actors draw over a copy of it each frame.
pub(super) struct CachedShadow {
    pub texture: wgpu::Texture,
    pub depth: wgpu::TextureView,
    pub stamp: ShadowStamp,
}
impl CachedShadow {
    pub fn new(device: &wgpu::Device, size: u32) -> Self {
        let texture = depth_texture(device, size, 1, "static shadow cache");
        let depth = texture.create_view(&Default::default());
        Self {
            texture,
            depth,
            stamp: ShadowStamp::default(),
        }
    }
    pub fn copy_to(&self, encoder: &mut wgpu::CommandEncoder, texture: &wgpu::Texture, layer: u32) {
        encoder.copy_texture_to_texture(
            self.texture.as_image_copy(),
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            self.texture.size(),
        );
    }
}

pub(super) fn depth_texture(
    device: &wgpu::Device,
    size: u32,
    layers: u32,
    label: &str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

const PAGE_LABELS: [&str; 15] = [
    "shadow page 1",
    "shadow page 2",
    "shadow page 3",
    "shadow page 4",
    "shadow page 5",
    "shadow page 6",
    "shadow page 7",
    "shadow page 8",
    "shadow page 9",
    "shadow page 10",
    "shadow page 11",
    "shadow page 12",
    "shadow page 13",
    "shadow page 14",
    "shadow page 15",
];
/// The most atlas pages: their sizes share one 32-bit table with the overlay's.
pub(super) const MAX_PAGES: usize = PAGE_LABELS.len() - 1;
/// Tile cameras are bound with dynamic offsets of this stride.
const CAMERA_STRIDE: u64 = 256;
/// Atlas tiles and overlay tiles that can render in one frame.
pub(super) const MAX_TILE_VIEWS: usize = 40;

/// The shadow atlas: depth pages shared by the tiles of many lights, plus one
/// overlay page that moving actors draw into afresh each frame.
pub(super) struct ShadowAtlasGpu {
    pub array: wgpu::TextureView,
    pages: Vec<wgpu::TextureView>,
    overlay_page: usize,
    camera_buffer: wgpu::Buffer,
    camera_group: wgpu::BindGroup,
    clear_pipeline: wgpu::RenderPipeline,
    tile_pipeline: wgpu::RenderPipeline,
}

impl ShadowAtlasGpu {
    pub fn new(
        device: &wgpu::Device,
        pages: usize,
        shader: &wgpu::ShaderModule,
        buffers: &[wgpu::VertexBufferLayout<'_>],
    ) -> Self {
        assert!(pages <= MAX_PAGES);
        let texture = depth_texture(device, PAGE_SIZE, pages as u32 + 1, "shadow atlas");
        let array = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("shadow atlas"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let views = (0..=pages as u32)
            .map(|layer| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("shadow page"),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: layer,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shadow tile cameras"),
            size: CAMERA_STRIDE * MAX_TILE_VIEWS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow tile camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(64),
                },
                count: None,
            }],
        });
        let camera_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow tile cameras"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &camera_buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(64),
                }),
            }],
        });
        let depth_pipeline = |label, entry, layout: &wgpu::PipelineLayout, buffers, state| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: shader,
                    entry_point: Some(entry),
                    buffers,
                    compilation_options: Default::default(),
                },
                fragment: None,
                primitive: wgpu::PrimitiveState {
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(state),
                multisample: Default::default(),
                multiview: None,
                cache: None,
            })
        };
        let tile_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow tile layout"),
            bind_group_layouts: &[&camera_layout],
            push_constant_ranges: &[],
        });
        let clear_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow tile clear layout"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });
        let tile_pipeline = depth_pipeline(
            "shadow tile depth",
            "vs_main",
            &tile_layout,
            buffers,
            wgpu::DepthStencilState {
                bias: wgpu::DepthBiasState {
                    constant: 1,
                    slope_scale: 0.0,
                    clamp: 0.0,
                },
                ..super::depth_state()
            },
        );
        let clear_pipeline = depth_pipeline(
            "shadow tile clear",
            "vs_clear",
            &clear_layout,
            &[],
            wgpu::DepthStencilState {
                depth_compare: wgpu::CompareFunction::Always,
                ..super::depth_state()
            },
        );
        Self {
            array,
            pages: views,
            overlay_page: pages,
            camera_buffer,
            camera_group,
            clear_pipeline,
            tile_pipeline,
        }
    }

    /// Render each dirty tile into its page: one pass per page, with the tile
    /// cleared by a far-depth triangle inside its viewport, then `draw` called
    /// for the casters.
    pub fn render(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        timing: &mut GpuTiming,
        jobs: &[TileJob],
        draw: &mut dyn FnMut(&mut wgpu::RenderPass<'_>, usize),
    ) {
        for (i, job) in jobs.iter().enumerate() {
            self.write_camera(queue, i, &job.matrix);
        }
        let mut pages: Vec<usize> = jobs.iter().map(|j| usize::from(j.tile.page)).collect();
        pages.sort_unstable();
        pages.dedup();
        for page in pages {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(PAGE_LABELS[page]),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.pages[page],
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: timing.pass(PAGE_LABELS[page]),
                occlusion_query_set: None,
            });
            for (i, job) in jobs
                .iter()
                .enumerate()
                .filter(|(_, j)| usize::from(j.tile.page) == page)
            {
                let [x, y] = job.class.origin(job.tile.slot);
                self.place(&mut pass, x, y, job.class.size());
                pass.set_pipeline(&self.clear_pipeline);
                pass.draw(0..3, 0..1);
                pass.set_pipeline(&self.tile_pipeline);
                pass.set_bind_group(0, &self.camera_group, &[i as u32 * CAMERA_STRIDE as u32]);
                draw(&mut pass, i);
            }
        }
    }

    /// Draw moving actors into the overlay page, cleared first: one tile per
    /// overlay job, after the cameras of the atlas jobs.
    pub fn render_overlay(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        timing: &mut GpuTiming,
        first_view: usize,
        jobs: &[OverlayJob],
        draw: &mut dyn FnMut(&mut wgpu::RenderPass<'_>),
    ) {
        if jobs.is_empty() {
            return;
        }
        for (i, job) in jobs.iter().enumerate() {
            self.write_camera(queue, first_view + i, &job.matrix);
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shadow overlay"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.pages[self.overlay_page],
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: timing.pass("shadow overlay"),
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.tile_pipeline);
        for (i, job) in jobs.iter().enumerate() {
            let [x, y] = OVERLAY_CLASS.origin(job.slot);
            self.place(&mut pass, x, y, OVERLAY_CLASS.size());
            let offset = (first_view + i) as u32 * CAMERA_STRIDE as u32;
            pass.set_bind_group(0, &self.camera_group, &[offset]);
            draw(&mut pass);
        }
    }

    fn write_camera(&self, queue: &wgpu::Queue, index: usize, matrix: &Mat4) {
        queue.write_buffer(
            &self.camera_buffer,
            index as u64 * CAMERA_STRIDE,
            bytemuck::bytes_of(&matrix.to_cols_array_2d()),
        );
    }

    fn place(&self, pass: &mut wgpu::RenderPass<'_>, x: u32, y: u32, size: u32) {
        pass.set_viewport(x as f32, y as f32, size as f32, size as f32, 0.0, 1.0);
        pass.set_scissor_rect(x, y, size, size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::atlas::TileClass;
    use glam::Vec3;

    #[test]
    fn cached_shadows_invalidate_on_geometry_or_window_change() {
        let mut stamp = ShadowStamp::default();
        assert!(stamp.update(Mat4::IDENTITY, 0));
        assert!(!stamp.update(Mat4::IDENTITY, 0));
        assert!(stamp.update(Mat4::IDENTITY, 1));
        assert!(stamp.update(Mat4::from_translation(Vec3::X), 1));
        assert!(!stamp.update(Mat4::from_translation(Vec3::X), 1));
    }

    #[test]
    fn tile_classes_tile_the_page_exactly() {
        for class in TileClass::ALL {
            assert_eq!(class.per_row() * class.size(), PAGE_SIZE);
            assert_eq!(class.per_page() as u32, class.per_row() * class.per_row());
        }
        assert!(OVERLAY_CLASS.per_page() > super::super::lightset::OVERLAY_SLOTS);
    }
}
