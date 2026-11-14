//! The sun's cascaded shadow maps: three square depth maps around the eye, a
//! cache of the static geometry in each, and the bookkeeping that redraws only
//! the part of a cascade that a change touches. The maps are whole array layers,
//! so copying a cache over its working layer is a whole-subresource copy, which
//! DirectX 12 allows for depth.
use super::{
    culling::Bounds,
    props::instance_layout,
    shadows::{depth_texture, CAMERA_STRIDE},
    timing::GpuTiming,
    GpuVertex,
};
use crate::lighting::{SunCascade, CASCADES};
use glam::{Mat4, Vec2, Vec3};

/// The cascades that moving actors draw over a copy of: the near two.
pub(super) const ACTOR_CASCADES: usize = 2;
/// A cascade redraws at most this many separate rectangles; beyond that, one
/// rectangle around all of them.
const MAX_RECTS: usize = 4;
/// Changes held for one cascade before it simply redraws everything.
const MAX_PENDING: usize = 256;
const STATIC_LABELS: [&str; CASCADES] = ["sun cascade 1", "sun cascade 2", "sun cascade 3"];
const ACTOR_LABELS: [&str; ACTOR_CASCADES] = ["sun actors 1", "sun actors 2"];

/// A rectangle of texels in one cascade, `x0..x1` by `y0..y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Rect {
    fn full(size: u32) -> Self {
        Self {
            x0: 0,
            y0: 0,
            x1: size,
            y1: size,
        }
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }

    fn union(self, other: Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    pub fn width(self) -> u32 {
        self.x1 - self.x0
    }

    pub fn height(self) -> u32 {
        self.y1 - self.y0
    }
}

/// The texels of a `size`-texel cascade that a box can change, with one texel
/// to spare on every side, or `None` when it misses the cascade.
pub(super) fn texel_rect(matrix: Mat4, bounds: Bounds, size: u32) -> Option<Rect> {
    let (min, max) = (bounds.min, bounds.max);
    if !(min.is_finite() && max.is_finite()) {
        return Some(Rect::full(size));
    }
    let mut low = Vec2::splat(f32::INFINITY);
    let mut high = Vec2::splat(f32::NEG_INFINITY);
    for i in 0..8 {
        let corner = Vec3::new(
            if i & 1 == 0 { min.x } else { max.x },
            if i & 2 == 0 { min.y } else { max.y },
            if i & 4 == 0 { min.z } else { max.z },
        );
        let ndc = matrix.project_point3(corner);
        let uv = Vec2::new(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        low = low.min(uv);
        high = high.max(uv);
    }
    let size_f = size as f32;
    let lo = (low * size_f).floor() - Vec2::ONE;
    let hi = (high * size_f).ceil() + Vec2::ONE;
    let clamp = |v: f32| v.clamp(0.0, size_f) as u32;
    let rect = Rect {
        x0: clamp(lo.x),
        y0: clamp(lo.y),
        x1: clamp(hi.x),
        y1: clamp(hi.y),
    };
    (rect.x0 < rect.x1 && rect.y0 < rect.y1).then_some(rect)
}

/// Overlapping rectangles joined, and no more than `MAX_RECTS` of them.
pub(super) fn merge_rects(mut rects: Vec<Rect>) -> Vec<Rect> {
    'again: loop {
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                if rects[i].overlaps(rects[j]) {
                    rects[i] = rects[i].union(rects[j]);
                    rects.swap_remove(j);
                    continue 'again;
                }
            }
        }
        break;
    }
    if rects.len() > MAX_RECTS {
        let all = rects.iter().copied().reduce(Rect::union);
        return all.into_iter().collect();
    }
    rects
}

/// What a cascade needs this frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Update {
    /// Its cache is still right.
    Keep,
    /// Everything: its window moved, or too much changed.
    Full,
    /// These rectangles only.
    Partial(Vec<Rect>),
}

#[derive(Default)]
struct CascadeState {
    /// The matrix its cache was drawn with.
    matrix: Option<Mat4>,
    pending: Vec<Rect>,
    everything: bool,
}

/// What the cascades' bookkeeping decides, apart from the GPU.
pub(super) struct SunPlanner {
    size: u32,
    cascades: [CascadeState; CASCADES],
}

impl SunPlanner {
    pub fn new(size: u32) -> Self {
        Self {
            size,
            cascades: Default::default(),
        }
    }

    pub fn invalidate_all(&mut self) {
        for cascade in &mut self.cascades {
            cascade.everything = true;
            cascade.pending.clear();
        }
    }

    /// Geometry changed inside `bounds`, old and new: each cascade redraws
    /// the texels it covers.
    pub fn invalidate_bounds(&mut self, bounds: Bounds) {
        let size = self.size;
        for cascade in &mut self.cascades {
            if cascade.everything {
                continue;
            }
            let Some(matrix) = cascade.matrix else {
                cascade.everything = true;
                continue;
            };
            if let Some(rect) = texel_rect(matrix, bounds, size) {
                if cascade.pending.len() >= MAX_PENDING {
                    cascade.everything = true;
                    cascade.pending.clear();
                } else {
                    cascade.pending.push(rect);
                }
            }
        }
    }

    /// Decide each cascade's update for a frame that uses these windows.
    pub fn plan(&mut self, matrices: &[Mat4; CASCADES]) -> [Update; CASCADES] {
        let size = self.size;
        std::array::from_fn(|i| {
            let cascade = &mut self.cascades[i];
            let moved = cascade.matrix != Some(matrices[i]);
            cascade.matrix = Some(matrices[i]);
            let pending = std::mem::take(&mut cascade.pending);
            if std::mem::take(&mut cascade.everything) || moved {
                return Update::Full;
            }
            let rects = merge_rects(pending);
            match rects.as_slice() {
                [] => Update::Keep,
                [only] if *only == Rect::full(size) => Update::Full,
                _ => Update::Partial(rects),
            }
        })
    }
}

/// The depth textures: the working array the shaders read, and the cache of the
/// static geometry in the cascades actors draw over.
struct SunMaps {
    size: u32,
    working: wgpu::Texture,
    /// All cascades, as the shaders see them.
    array: wgpu::TextureView,
    layers: Vec<wgpu::TextureView>,
    cache: wgpu::Texture,
    cache_layers: Vec<wgpu::TextureView>,
}

impl SunMaps {
    fn new(device: &wgpu::Device, size: u32) -> Self {
        let working = depth_texture(device, size, CASCADES as u32, "sun cascades");
        let cache = depth_texture(device, size, ACTOR_CASCADES as u32, "sun cascade caches");
        let layer = |texture: &wgpu::Texture, index| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("sun cascade"),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: index,
                array_layer_count: Some(1),
                ..Default::default()
            })
        };
        Self {
            size,
            array: working.create_view(&wgpu::TextureViewDescriptor {
                label: Some("sun cascades"),
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            }),
            layers: (0..CASCADES as u32).map(|i| layer(&working, i)).collect(),
            cache_layers: (0..ACTOR_CASCADES as u32)
                .map(|i| layer(&cache, i))
                .collect(),
            working,
            cache,
        }
    }

    /// GPU memory of the working array and the caches, in bytes.
    fn bytes(size: u32) -> u64 {
        u64::from(size) * u64::from(size) * 4 * (CASCADES + ACTOR_CASCADES) as u64
    }
}

pub(super) struct SunShadows {
    maps: SunMaps,
    planner: SunPlanner,
    camera_buffer: wgpu::Buffer,
    camera_group: wgpu::BindGroup,
    clear_pipeline: wgpu::RenderPipeline,
    tile_pipeline: wgpu::RenderPipeline,
}

impl SunShadows {
    pub fn new(device: &wgpu::Device, size: u32, shader: &wgpu::ShaderModule) -> Self {
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sun cascade cameras"),
            size: CAMERA_STRIDE * CASCADES as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sun cascade camera layout"),
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
            label: Some("sun cascade cameras"),
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
            label: Some("sun cascade layout"),
            bind_group_layouts: &[&camera_layout],
            push_constant_ranges: &[],
        });
        let clear_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sun cascade clear layout"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });
        let vertex_buffers = [GpuVertex::layout(), instance_layout()];
        let tile_pipeline = depth_pipeline(
            "sun cascade depth",
            "vs_main",
            &tile_layout,
            &vertex_buffers,
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
            "sun cascade clear",
            "vs_clear",
            &clear_layout,
            &[],
            wgpu::DepthStencilState {
                depth_compare: wgpu::CompareFunction::Always,
                ..super::depth_state()
            },
        );
        Self {
            maps: SunMaps::new(device, size),
            planner: SunPlanner::new(size),
            camera_buffer,
            camera_group,
            clear_pipeline,
            tile_pipeline,
        }
    }

    pub fn size(&self) -> u32 {
        self.maps.size
    }

    /// The cascades as the scene shaders sample them.
    pub fn array(&self) -> &wgpu::TextureView {
        &self.maps.array
    }

    /// GPU memory the maps use at a cascade resolution, in bytes.
    pub fn bytes(size: u32) -> u64 {
        SunMaps::bytes(size)
    }

    /// Replace the maps with ones of another size. Everything redraws.
    pub fn resize(&mut self, device: &wgpu::Device, size: u32) {
        self.maps = SunMaps::new(device, size);
        self.planner = SunPlanner::new(size);
    }

    pub fn invalidate_all(&mut self) {
        self.planner.invalidate_all();
    }

    pub fn invalidate_bounds(&mut self, bounds: Bounds) {
        self.planner.invalidate_bounds(bounds);
    }

    /// How many texels these updates draw.
    pub fn texels(&self, updates: &[Update; CASCADES]) -> u64 {
        let size = u64::from(self.maps.size);
        updates
            .iter()
            .map(|update| match update {
                Update::Keep => 0,
                Update::Full => size * size,
                Update::Partial(rects) => rects
                    .iter()
                    .map(|r| u64::from(r.width()) * u64::from(r.height()))
                    .sum(),
            })
            .sum()
    }

    /// Decide this frame's updates, and write the cascades' cameras.
    pub fn prepare(
        &mut self,
        queue: &wgpu::Queue,
        cascades: &[SunCascade; CASCADES],
    ) -> [Update; CASCADES] {
        let matrices = cascades.map(|c| c.matrix);
        for (i, matrix) in matrices.iter().enumerate() {
            queue.write_buffer(
                &self.camera_buffer,
                i as u64 * CAMERA_STRIDE,
                bytemuck::bytes_of(&matrix.to_cols_array_2d()),
            );
        }
        self.planner.plan(&matrices)
    }

    /// Redraw what each cascade needs. `draw` puts a cascade's static casters
    /// in a pass: all of them for `None`, or those that can reach the
    /// rectangle. Returns how many cascades drew.
    pub fn render_static(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        timing: &mut GpuTiming,
        updates: &[Update; CASCADES],
        draw: &mut dyn FnMut(&mut wgpu::RenderPass<'_>, usize, Option<Rect>),
    ) -> usize {
        let mut drawn = 0;
        for (i, update) in updates.iter().enumerate() {
            if *update == Update::Keep {
                continue;
            }
            drawn += 1;
            // The near cascades draw into their cache, which is copied over the
            // working layer each frame; the far one has no actors and draws
            // straight into its working layer.
            let view = self
                .maps
                .cache_layers
                .get(i)
                .unwrap_or(&self.maps.layers[i]);
            let full = *update == Update::Full;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(STATIC_LABELS[i]),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view,
                    depth_ops: Some(wgpu::Operations {
                        load: if full {
                            wgpu::LoadOp::Clear(1.0)
                        } else {
                            wgpu::LoadOp::Load
                        },
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: timing.pass(STATIC_LABELS[i]),
                occlusion_query_set: None,
            });
            let offset = i as u32 * CAMERA_STRIDE as u32;
            match update {
                Update::Partial(rects) => {
                    for &rect in rects {
                        pass.set_scissor_rect(rect.x0, rect.y0, rect.width(), rect.height());
                        pass.set_pipeline(&self.clear_pipeline);
                        pass.draw(0..3, 0..1);
                        pass.set_pipeline(&self.tile_pipeline);
                        pass.set_bind_group(0, &self.camera_group, &[offset]);
                        draw(&mut pass, i, Some(rect));
                    }
                }
                _ => {
                    pass.set_pipeline(&self.tile_pipeline);
                    pass.set_bind_group(0, &self.camera_group, &[offset]);
                    draw(&mut pass, i, None);
                }
            }
        }
        drawn
    }

    /// Copy each near cache over its working layer, then draw the moving
    /// actors on top with `draw`.
    pub fn render_actors(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        timing: &mut GpuTiming,
        with_actors: bool,
        draw: &mut dyn FnMut(&mut wgpu::RenderPass<'_>),
    ) {
        let size = self.maps.size;
        for i in 0..ACTOR_CASCADES as u32 {
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.maps.cache,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: 0, z: i },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &self.maps.working,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: 0, z: i },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
            );
        }
        if !with_actors {
            return;
        }
        for (i, label) in ACTOR_LABELS.into_iter().enumerate() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.maps.layers[i],
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: timing.pass(label),
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.tile_pipeline);
            pass.set_bind_group(0, &self.camera_group, &[i as u32 * CAMERA_STRIDE as u32]);
            draw(&mut pass);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lighting::sun_cascades;

    fn moving_matrices(eye: Vec3) -> [Mat4; CASCADES] {
        sun_cascades(Vec3::new(-0.44, 0.76, 0.34), eye, 1024).map(|c| c.matrix)
    }

    fn all(update: Update) -> [Update; CASCADES] {
        [update.clone(), update.clone(), update]
    }

    fn block(center: Vec3) -> Bounds {
        Bounds::around(center, 0.5)
    }

    #[test]
    fn the_first_frame_and_a_moved_window_redraw_everything() {
        let mut planner = SunPlanner::new(1024);
        // The origin is the middle of every window's step, so how far the eye
        // can go before a window moves is the same in every direction.
        let eye = Vec3::ZERO;
        assert_eq!(planner.plan(&moving_matrices(eye)), all(Update::Full));
        assert_eq!(planner.plan(&moving_matrices(eye)), all(Update::Keep));
        // Within a step, nothing moves. Far enough, the near windows do, and
        // the far one holds on longer.
        let near = eye + Vec3::new(0.3, 0.0, 0.0);
        assert_eq!(planner.plan(&moving_matrices(near)), all(Update::Keep));
        let plan = planner.plan(&moving_matrices(eye + Vec3::new(6.0, 0.0, 0.0)));
        assert_eq!(plan[0], Update::Full);
        assert_eq!(plan[1], Update::Full);
        assert_eq!(plan[2], Update::Keep);
    }

    #[test]
    fn a_block_edit_redraws_only_the_texels_it_touches() {
        let eye = Vec3::new(5.0, 20.0, 5.0);
        let matrices = moving_matrices(eye);
        let mut planner = SunPlanner::new(1024);
        planner.plan(&matrices);
        planner.invalidate_bounds(block(eye + Vec3::new(2.0, -1.0, 3.0)));
        let plan = planner.plan(&matrices);
        for (i, update) in plan.iter().enumerate() {
            let Update::Partial(rects) = update else {
                panic!("cascade {i}: {update:?}");
            };
            assert_eq!(rects.len(), 1);
            let rect = rects[0];
            // The rectangle is small, and holds the block's own texels.
            assert!(
                rect.width() < 1024 / 4 && rect.height() < 1024 / 4,
                "{rect:?}"
            );
            let center = texel_rect(matrices[i], block(eye + Vec3::new(2.0, -1.0, 3.0)), 1024);
            assert_eq!(center, Some(rect));
        }
        // Nothing left over: the next frame keeps all three.
        assert_eq!(planner.plan(&matrices), all(Update::Keep));
    }

    #[test]
    fn a_change_outside_a_cascade_leaves_it_alone() {
        let eye = Vec3::new(5.0, 20.0, 5.0);
        let matrices = moving_matrices(eye);
        let mut planner = SunPlanner::new(1024);
        planner.plan(&matrices);
        // 100 m away: outside the near two windows, inside the far one.
        planner.invalidate_bounds(block(eye + Vec3::new(100.0, 0.0, 0.0)));
        let plan = planner.plan(&matrices);
        assert_eq!(plan[0], Update::Keep);
        assert_eq!(plan[1], Update::Keep);
        assert!(matches!(plan[2], Update::Partial(_)));
    }

    #[test]
    fn separate_changes_stay_separate_and_overlapping_ones_join() {
        let eye = Vec3::new(5.0, 20.0, 5.0);
        let matrices = moving_matrices(eye);
        let mut planner = SunPlanner::new(1024);
        planner.plan(&matrices);
        planner.invalidate_bounds(block(eye + Vec3::new(8.0, 0.0, 0.0)));
        planner.invalidate_bounds(block(eye + Vec3::new(-8.0, 0.0, 0.0)));
        planner.invalidate_bounds(block(eye + Vec3::new(8.2, 0.0, 0.1)));
        match &planner.plan(&matrices)[0] {
            Update::Partial(rects) => assert_eq!(rects.len(), 2, "{rects:?}"),
            other => panic!("{other:?}"),
        }
        // Past four separate places, one rectangle around all of them.
        for i in 0..6 {
            planner.invalidate_bounds(block(eye + Vec3::new(-12.0 + i as f32 * 4.5, 0.0, 0.0)));
        }
        match &planner.plan(&matrices)[0] {
            Update::Partial(rects) => assert!(rects.len() <= MAX_RECTS, "{rects:?}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn invalidating_everything_or_too_much_redraws_whole_cascades() {
        let eye = Vec3::new(5.0, 20.0, 5.0);
        let matrices = moving_matrices(eye);
        let mut planner = SunPlanner::new(1024);
        planner.plan(&matrices);
        planner.invalidate_all();
        assert_eq!(planner.plan(&matrices), all(Update::Full));
        for i in 0..MAX_PENDING + 1 {
            planner.invalidate_bounds(block(eye + Vec3::new(0.01 * i as f32, 0.0, 0.0)));
        }
        assert_eq!(planner.plan(&matrices)[0], Update::Full);
        // A box that covers the cascade is a full redraw, not a scissor.
        planner.invalidate_bounds(Bounds::around(eye, 500.0));
        assert_eq!(planner.plan(&matrices)[0], Update::Full);
        // Boxes with no extent are treated as everything, not skipped.
        planner.invalidate_bounds(Bounds::from_positions(std::iter::empty()));
        assert_eq!(planner.plan(&matrices)[1], Update::Full);
    }

    #[test]
    fn rectangles_are_clamped_to_the_map_and_missing_boxes_have_none() {
        let matrices = moving_matrices(Vec3::ZERO);
        let rect = texel_rect(matrices[0], Bounds::around(Vec3::ZERO, 200.0), 512).unwrap();
        assert_eq!(rect, Rect::full(512));
        assert_eq!(
            texel_rect(matrices[0], block(Vec3::splat(400.0)), 512),
            None
        );
        let rects = merge_rects(vec![
            Rect {
                x0: 0,
                y0: 0,
                x1: 10,
                y1: 10,
            },
            Rect {
                x0: 5,
                y0: 5,
                x1: 20,
                y1: 20,
            },
            Rect {
                x0: 100,
                y0: 100,
                x1: 110,
                y1: 110,
            },
        ]);
        assert_eq!(rects.len(), 2);
    }

    #[test]
    fn cascade_memory_follows_resolution() {
        // Three working layers and two caches of 4-byte depth.
        assert_eq!(SunMaps::bytes(1024), 5 * 4 * 1024 * 1024);
        assert_eq!(SunMaps::bytes(2048), 80 * 1024 * 1024);
        assert_eq!(SunMaps::bytes(512), 5 * 1024 * 1024);
    }
}
