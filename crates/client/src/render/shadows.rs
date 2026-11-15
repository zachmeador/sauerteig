use super::{
    atlas::PAGE_SIZE,
    lightset::{FrameJob, TileJob},
    timing::GpuTiming,
};
use glam::Mat4;

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

/// Pass labels for the first pages; later pages share the last one.
const PAGE_LABELS: [&str; 8] = [
    "shadow page 1",
    "shadow page 2",
    "shadow page 3",
    "shadow page 4",
    "shadow page 5",
    "shadow page 6",
    "shadow page 7",
    "shadow page 8+",
];

fn page_label(page: usize) -> &'static str {
    PAGE_LABELS[page.min(PAGE_LABELS.len() - 1)]
}

/// Bytes of one 1024² `Depth32Float` page.
pub(super) const PAGE_BYTES: usize = (PAGE_SIZE * PAGE_SIZE * 4) as usize;
/// The atlas texture gains pages in steps of this many layers, so a growing
/// world copies it a few times, not once a page.
const GROWTH_PAGES: usize = 4;
/// The atlas texture gives layers back once this many are unused.
const SHRINK_PAGES: usize = 12;
/// Tile cameras are bound with dynamic offsets of this stride: a matrix, then
/// the offset to the static tile for a per-frame tile that starts as a copy.
pub(super) const CAMERA_STRIDE: u64 = 256;
/// What the shadow shader reads of a camera: a matrix and the copy offset.
pub(super) const CAMERA_BYTES: u64 = 80;

/// A depth texture of 1024² pages, with a view of the whole array and of each
/// page.
struct Pages {
    texture: wgpu::Texture,
    array: wgpu::TextureView,
    views: Vec<wgpu::TextureView>,
}

impl Pages {
    fn new(device: &wgpu::Device, layers: usize, label: &str) -> Self {
        let texture = depth_texture(device, PAGE_SIZE, layers as u32, label);
        let array = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some(label),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let views = (0..layers as u32)
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
        Self {
            texture,
            array,
            views,
        }
    }

    fn layers(&self) -> usize {
        self.views.len()
    }
}

/// The shadow atlas: depth pages shared by the tiles of many lights, which
/// grow with the lights, plus a second texture of per-frame tiles that moving
/// actors draw into afresh each frame. Nothing renders into the texture it
/// samples.
pub(super) struct ShadowAtlasGpu {
    device_layers: usize,
    atlas: Pages,
    frame: Pages,
    camera_buffer: wgpu::Buffer,
    camera_slots: usize,
    camera_layout: wgpu::BindGroupLayout,
    camera_group: wgpu::BindGroup,
    copy_layout: wgpu::BindGroupLayout,
    copy_group: wgpu::BindGroup,
    clear_pipeline: wgpu::RenderPipeline,
    tile_pipeline: wgpu::RenderPipeline,
    copy_pipeline: wgpu::RenderPipeline,
}

fn camera_buffer(device: &wgpu::Device, slots: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("shadow tile cameras"),
        size: CAMERA_STRIDE * slots as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn camera_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("shadow tile cameras"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: 0,
                size: wgpu::BufferSize::new(CAMERA_BYTES),
            }),
        }],
    })
}

fn copy_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    atlas: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("shadow tile copy source"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(atlas),
        }],
    })
}

impl ShadowAtlasGpu {
    pub fn new(
        device: &wgpu::Device,
        shader: &wgpu::ShaderModule,
        buffers: &[wgpu::VertexBufferLayout<'_>],
    ) -> Self {
        let device_layers = device.limits().max_texture_array_layers as usize;
        let atlas = Pages::new(device, GROWTH_PAGES, "shadow atlas");
        let frame = Pages::new(device, 1, "shadow actor tiles");
        let camera_slots = 64;
        let camera_buffer = camera_buffer(device, camera_slots);
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow tile camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(CAMERA_BYTES),
                },
                count: None,
            }],
        });
        let camera_group = camera_group(device, &camera_layout, &camera_buffer);
        let copy_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow tile copy layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let copy_group = copy_group(device, &copy_layout, &atlas.array);
        let depth_pipeline = |label,
                              vertex,
                              fragment: Option<&str>,
                              layout: &wgpu::PipelineLayout,
                              buffers,
                              state| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: shader,
                    entry_point: Some(vertex),
                    buffers,
                    compilation_options: Default::default(),
                },
                fragment: fragment.map(|entry_point| wgpu::FragmentState {
                    module: shader,
                    entry_point: Some(entry_point),
                    targets: &[],
                    compilation_options: Default::default(),
                }),
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
        let copy_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow tile copy layout"),
            bind_group_layouts: &[&camera_layout, &copy_layout],
            push_constant_ranges: &[],
        });
        let tile_pipeline = depth_pipeline(
            "shadow tile depth",
            "vs_main",
            None,
            &tile_layout,
            buffers,
            wgpu::DepthStencilState {
                bias: wgpu::DepthBiasState {
                    constant: 1,
                    slope_scale: 0.0,
                    clamp: 0.0,
                },
                ..super::shadow_depth_state()
            },
        );
        let always = wgpu::DepthStencilState {
            depth_compare: wgpu::CompareFunction::Always,
            ..super::shadow_depth_state()
        };
        let clear_pipeline = depth_pipeline(
            "shadow tile clear",
            "vs_clear",
            None,
            &clear_layout,
            &[],
            always.clone(),
        );
        let copy_pipeline = depth_pipeline(
            "shadow tile copy",
            "vs_clear",
            Some("fs_copy"),
            &copy_pipeline_layout,
            &[],
            always,
        );
        Self {
            device_layers,
            atlas,
            frame,
            camera_buffer,
            camera_slots,
            camera_layout,
            camera_group,
            copy_layout,
            copy_group,
            clear_pipeline,
            tile_pipeline,
            copy_pipeline,
        }
    }

    pub fn array(&self) -> &wgpu::TextureView {
        &self.atlas.array
    }

    pub fn frame_array(&self) -> &wgpu::TextureView {
        &self.frame.array
    }

    /// Pages the textures have layers for: the atlas's, then the per-frame one's.
    pub fn layers(&self) -> (usize, usize) {
        (self.atlas.layers(), self.frame.layers())
    }

    /// Bytes of both textures.
    pub fn bytes(&self) -> usize {
        (self.atlas.layers() + self.frame.layers()) * PAGE_BYTES
    }

    /// Give the atlas layers for `pages` pages, copying what it holds into a
    /// larger texture when it grows, the per-frame texture layers for
    /// `frame_pages`, and the tile cameras room for `tiles` tiles. Returns whether either texture was replaced, so that
    /// shaders can be pointed at the new one.
    pub fn reserve(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        pages: usize,
        frame_pages: usize,
        tiles: usize,
    ) -> bool {
        self.reserve_cameras(device, tiles);
        let mut replaced = false;
        let wanted = pages
            .next_multiple_of(GROWTH_PAGES)
            .max(GROWTH_PAGES)
            .min(self.device_layers);
        // It shrinks only once a good many layers sit empty at the end, so a
        // light or two coming and going never copies it.
        let have = self.atlas.layers();
        if wanted > have || wanted + SHRINK_PAGES <= have {
            let resized = Pages::new(device, wanted, "shadow atlas");
            // Whole layers, which every backend can copy for depth textures.
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::DepthOnly,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &resized.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::DepthOnly,
                },
                wgpu::Extent3d {
                    width: PAGE_SIZE,
                    height: PAGE_SIZE,
                    depth_or_array_layers: have.min(wanted) as u32,
                },
            );
            self.atlas = resized;
            self.copy_group = copy_group(device, &self.copy_layout, &self.atlas.array);
            replaced = true;
        }
        // The per-frame texture holds nothing between frames, so it follows
        // the need in both directions, with room to spare before it shrinks.
        let have = self.frame.layers();
        if frame_pages > have || (frame_pages * 4 < have && have > 2) {
            let layers = frame_pages.max(1).min(self.device_layers);
            self.frame = Pages::new(device, layers, "shadow actor tiles");
            replaced = true;
        }
        replaced
    }

    fn reserve_cameras(&mut self, device: &wgpu::Device, slots: usize) {
        if slots <= self.camera_slots {
            return;
        }
        self.camera_slots = slots.next_power_of_two();
        self.camera_buffer = camera_buffer(device, self.camera_slots);
        self.camera_group = camera_group(device, &self.camera_layout, &self.camera_buffer);
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
            self.write_camera(queue, i, &job.matrix, [0; 4]);
        }
        let mut pages: Vec<usize> = jobs.iter().map(|j| usize::from(j.tile.page)).collect();
        pages.sort_unstable();
        pages.dedup();
        for page in pages {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(page_label(page)),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.atlas.views[page],
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: timing.pass(page_label(page)),
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

    /// Draw moving actors into the per-frame tiles, one pass per page of that
    /// texture, cleared first: one tile per job, after the cameras of the atlas
    /// jobs. Each tile first receives its static tile's depth, drawn as a
    /// triangle that reads the atlas and writes the depth, so that shading needs
    /// only the per-frame tile. (Copying the rectangle would be simpler, and
    /// DirectX 12 does not allow it for depth textures on every GPU.)
    pub fn render_frame(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        timing: &mut GpuTiming,
        first_view: usize,
        jobs: &[FrameJob],
        draw: &mut dyn FnMut(&mut wgpu::RenderPass<'_>),
    ) {
        if jobs.is_empty() {
            return;
        }
        for (i, job) in jobs.iter().enumerate() {
            let [x, y] = job.class.origin(job.tile.slot);
            let [sx, sy] = job.class.origin(job.source.slot);
            self.write_camera(
                queue,
                first_view + i,
                &job.matrix,
                [
                    sx as i32 - x as i32,
                    sy as i32 - y as i32,
                    i32::from(job.source.page),
                    0,
                ],
            );
        }
        let mut pages: Vec<usize> = jobs.iter().map(|j| usize::from(j.tile.page)).collect();
        pages.sort_unstable();
        pages.dedup();
        for page in pages {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow actor tiles"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.frame.views[page],
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: timing.pass("shadow actor tiles"),
                occlusion_query_set: None,
            });
            for (i, job) in jobs
                .iter()
                .enumerate()
                .filter(|(_, j)| usize::from(j.tile.page) == page)
            {
                let [x, y] = job.class.origin(job.tile.slot);
                self.place(&mut pass, x, y, job.class.size());
                let offset = (first_view + i) as u32 * CAMERA_STRIDE as u32;
                pass.set_pipeline(&self.copy_pipeline);
                pass.set_bind_group(0, &self.camera_group, &[offset]);
                pass.set_bind_group(1, &self.copy_group, &[]);
                pass.draw(0..3, 0..1);
                pass.set_pipeline(&self.tile_pipeline);
                pass.set_bind_group(0, &self.camera_group, &[offset]);
                draw(&mut pass);
            }
        }
    }

    fn write_camera(&self, queue: &wgpu::Queue, index: usize, matrix: &Mat4, copy: [i32; 4]) {
        let mut bytes = [0u8; CAMERA_BYTES as usize];
        bytes[..64].copy_from_slice(bytemuck::bytes_of(&matrix.to_cols_array_2d()));
        bytes[64..].copy_from_slice(bytemuck::bytes_of(&copy));
        queue.write_buffer(&self.camera_buffer, index as u64 * CAMERA_STRIDE, &bytes);
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

    #[test]
    fn tile_classes_tile_the_page_exactly() {
        for class in TileClass::ALL {
            assert_eq!(class.per_row() * class.size(), PAGE_SIZE);
            assert_eq!(class.per_page() as u32, class.per_row() * class.per_row());
        }
    }

    #[test]
    fn a_page_is_four_mebibytes() {
        assert_eq!(PAGE_BYTES, 4 << 20);
        assert_eq!(CAMERA_BYTES % 16, 0);
        const { assert!(CAMERA_BYTES <= CAMERA_STRIDE) };
    }
}
