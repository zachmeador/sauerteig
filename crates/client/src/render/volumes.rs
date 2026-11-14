//! The shared pool of transparent quads. Core meshes each mesh unit's water
//! and glass; a unit rebuilt replaces its own quads in the pool, and every
//! frame the visible quads of all units are sorted back to front together and
//! drawn with one indexed draw.

use super::{
    culling::{Bounds, Frustum},
    shaders::LightLayout,
};
use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use sauerteig_core::{ChunkKey, TransparentVertex, World};
use std::{collections::BTreeMap, ops::Range};
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct VolumeVertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    size: [f32; 2],
    material: u32,
    edges: u32,
}

impl From<&TransparentVertex> for VolumeVertex {
    fn from(v: &TransparentVertex) -> Self {
        Self {
            position: v.position,
            normal: v.normal,
            uv: v.uv,
            size: v.size,
            material: u32::from(v.material),
            edges: u32::from(v.edges),
        }
    }
}

impl VolumeVertex {
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![0=>Float32x3, 1=>Float32x3, 2=>Float32x2, 3=>Float32x2, 4=>Uint32, 5=>Uint32];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRIBUTES,
        }
    }
}

const VERTEX_BYTES: u64 = std::mem::size_of::<VolumeVertex>() as u64;
/// The fewest quad slots a pool holds.
const MIN_SLOTS: usize = 256;

/// Free runs of quad slots, with the pool's size in slots.
#[derive(Default)]
struct Slots {
    capacity: usize,
    /// Disjoint, ordered by start, never touching one another.
    free: Vec<Range<usize>>,
}

impl Slots {
    /// A run of `count` slots, growing the pool when none is large enough.
    fn allocate(&mut self, count: usize) -> usize {
        if let Some(i) = self.free.iter().position(|r| r.len() >= count) {
            let start = self.free[i].start;
            self.free[i].start += count;
            if self.free[i].is_empty() {
                self.free.remove(i);
            }
            return start;
        }
        // The last free run, if it reaches the end, counts toward the new one.
        let tail = self
            .free
            .last()
            .filter(|r| r.end == self.capacity)
            .map_or(0, Range::len);
        let start = self.capacity - tail;
        let needed = start + count;
        let capacity = needed.max(self.capacity * 2).max(MIN_SLOTS);
        if tail > 0 {
            self.free.pop();
        }
        if capacity > needed {
            self.free.push(needed..capacity);
        }
        self.capacity = capacity;
        start
    }

    fn release(&mut self, run: Range<usize>) {
        if run.is_empty() {
            return;
        }
        let at = self.free.partition_point(|r| r.start < run.start);
        self.free.insert(at, run);
        if at + 1 < self.free.len() && self.free[at].end == self.free[at + 1].start {
            self.free[at].end = self.free[at + 1].end;
            self.free.remove(at + 1);
        }
        if at > 0 && self.free[at - 1].end == self.free[at].start {
            self.free[at - 1].end = self.free[at].end;
            self.free.remove(at);
        }
    }
}

/// One mesh unit's quads: their slots in the pool and a hierarchy over their
/// bounds.
struct Unit {
    first: usize,
    count: usize,
    tree: SpatialTree,
    bounds: Bounds,
}

pub(super) struct VolumeRenderer {
    pipeline: Vec<wgpu::RenderPipeline>,
    underwater_pipeline: Vec<wgpu::RenderPipeline>,
    slots: Slots,
    /// The pool's vertices, four to a slot, as the GPU buffer holds them.
    vertices: Vec<VolumeVertex>,
    /// Each slot's bounds, for sorting.
    bounds: Vec<Bounds>,
    units: BTreeMap<ChunkKey, Unit>,
    vertex: Option<wgpu::Buffer>,
    index: wgpu::Buffer,
    visible: Vec<usize>,
    indices: Vec<u32>,
    local: Vec<usize>,
}

impl VolumeRenderer {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        camera_layout: &wgpu::BindGroupLayout,
        lights_layout: &wgpu::BindGroupLayout,
        light_buffers: LightLayout,
        sample_counts: &[u32],
    ) -> Self {
        let shader = super::shaders::module(device, "volume", light_buffers);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("transparent volume layout"),
            bind_group_layouts: &[camera_layout, lights_layout],
            push_constant_ranges: &[],
        });
        let pipeline = |label, vs, fs, buffers, depth_compare, bias, blend| {
            sample_counts
                .iter()
                .map(|&sample_count| {
                    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                        label: Some(label),
                        layout: Some(&layout),
                        vertex: wgpu::VertexState {
                            module: &shader,
                            entry_point: Some(vs),
                            buffers,
                            compilation_options: Default::default(),
                        },
                        fragment: Some(wgpu::FragmentState {
                            module: &shader,
                            entry_point: Some(fs),
                            targets: &[Some(wgpu::ColorTargetState {
                                format,
                                blend: Some(blend),
                                write_mask: wgpu::ColorWrites::ALL,
                            })],
                            compilation_options: Default::default(),
                        }),
                        primitive: wgpu::PrimitiveState {
                            // Both sides remain visible when swimming inside a volume.
                            cull_mode: None,
                            ..Default::default()
                        },
                        depth_stencil: Some(wgpu::DepthStencilState {
                            depth_write_enabled: false,
                            depth_compare,
                            bias,
                            ..super::depth_state()
                        }),
                        multisample: wgpu::MultisampleState {
                            count: sample_count,
                            ..Default::default()
                        },
                        multiview: None,
                        cache: None,
                    })
                })
                .collect()
        };
        Self {
            pipeline: pipeline(
                "transparent water and glass",
                "vs_main",
                "fs_main",
                &[VolumeVertex::layout()],
                wgpu::CompareFunction::Less,
                // Imported material boxes can share a plane with differently
                // tessellated terrain. Strict Less alone leaves interpolation
                // rounding streaks; a small positive bias lets opaque terrain
                // consistently win, including at grazing angles.
                wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 1.0,
                    clamp: 0.0,
                },
                // Highlights are added after the surface is weighted by its
                // opacity, so the shader returns premultiplied color.
                wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
            ),
            underwater_pipeline: pipeline(
                "underwater tint",
                "vs_underwater",
                "fs_underwater",
                &[],
                wgpu::CompareFunction::Always,
                wgpu::DepthBiasState::default(),
                wgpu::BlendState::ALPHA_BLENDING,
            ),
            slots: Slots::default(),
            vertices: Vec::new(),
            bounds: Vec::new(),
            units: BTreeMap::new(),
            vertex: None,
            index: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("visible transparent indices"),
                size: 256,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            visible: Vec::new(),
            indices: Vec::new(),
            local: Vec::new(),
        }
    }

    /// Forgets every unit and meshes all of the world's water and glass.
    pub fn rebuild_all(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, world: &World) {
        self.units.clear();
        self.slots = Slots::default();
        self.vertices.clear();
        self.bounds.clear();
        self.vertex = None;
        self.visible.clear();
        self.indices.clear();
        self.rebuild(device, queue, world, &world.transparent_keys());
    }

    /// Meshes these units again, each replacing its own quads, and returns how
    /// many were built.
    pub fn rebuild(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        world: &World,
        keys: &[ChunkKey],
    ) -> usize {
        let capacity = self.slots.capacity;
        let mut written: Vec<Range<usize>> = Vec::new();
        for &key in keys {
            if let Some(old) = self.units.remove(&key) {
                self.slots.release(old.first..old.first + old.count);
            }
            let mesh = world.transparent_chunk(key);
            let count = mesh.quads();
            if count == 0 {
                continue;
            }
            let first = self.slots.allocate(count);
            if self.vertices.len() < self.slots.capacity * 4 {
                self.vertices
                    .resize(self.slots.capacity * 4, VolumeVertex::zeroed());
                self.bounds.resize(
                    self.slots.capacity,
                    Bounds {
                        min: Vec3::ZERO,
                        max: Vec3::ZERO,
                    },
                );
            }
            let mut quad_bounds = Vec::with_capacity(count);
            for (q, quad) in mesh.vertices.as_chunks::<4>().0.iter().enumerate() {
                let slot = first + q;
                for (i, v) in quad.iter().enumerate() {
                    self.vertices[slot * 4 + i] = v.into();
                }
                let b = Bounds::from_positions(quad.iter().map(|v| Vec3::from_array(v.position)));
                self.bounds[slot] = b;
                quad_bounds.push(b);
            }
            let tree = SpatialTree::new(quad_bounds);
            let bounds = tree.nodes[0].bounds;
            self.units.insert(
                key,
                Unit {
                    first,
                    count,
                    tree,
                    bounds,
                },
            );
            written.push(first..first + count);
        }
        if self.slots.capacity > capacity || self.vertex.is_none() && !self.units.is_empty() {
            // The pool grew: the new buffer takes everything.
            self.vertex = Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("transparent quads"),
                    contents: bytemuck::cast_slice(&self.vertices),
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                }),
            );
            self.index = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("visible transparent indices"),
                size: self.slots.capacity as u64 * 6 * 4,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        } else if let Some(buffer) = &self.vertex {
            for run in written {
                queue.write_buffer(
                    buffer,
                    run.start as u64 * 4 * VERTEX_BYTES,
                    bytemuck::cast_slice(&self.vertices[run.start * 4..run.end * 4]),
                );
            }
        }
        keys.len()
    }

    /// How many units hold quads, and how many quads the pool holds.
    pub fn stats(&self) -> (usize, usize) {
        (
            self.units.len(),
            self.units.values().map(|unit| unit.count).sum(),
        )
    }

    pub fn prepare(&mut self, queue: &wgpu::Queue, eye: Vec3, direction: Vec3, frustum: &Frustum) {
        self.visible.clear();
        for unit in self.units.values() {
            if !frustum.intersects(unit.bounds) {
                continue;
            }
            self.local.clear();
            unit.tree.visible(frustum, &mut self.local);
            self.visible
                .extend(self.local.iter().map(|&quad| unit.first + quad));
        }
        // Cached vertices never change during a frame. Sort only the visible
        // quads of every unit together and stream their small index order as
        // a single draw. Centroid sorting is deliberately bounded alpha
        // transparency, not OIT.
        sorted_indices(
            &self.bounds,
            &mut self.visible,
            &mut self.indices,
            eye,
            direction,
        );
        if !self.indices.is_empty() {
            queue.write_buffer(&self.index, 0, bytemuck::cast_slice(&self.indices));
        }
    }

    pub fn visible_triangles(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn draw<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        underwater: bool,
        sample_index: usize,
    ) {
        if let Some(vertex) = &self.vertex {
            if !self.indices.is_empty() {
                pass.set_pipeline(&self.pipeline[sample_index]);
                pass.set_vertex_buffer(0, vertex.slice(..));
                pass.set_index_buffer(self.index.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..self.indices.len() as u32, 0, 0..1);
            }
        }
        if underwater {
            pass.set_pipeline(&self.underwater_pipeline[sample_index]);
            pass.draw(0..3, 0..1);
        }
    }
}

fn sorted_indices(
    bounds: &[Bounds],
    visible: &mut [usize],
    indices: &mut Vec<u32>,
    eye: Vec3,
    direction: Vec3,
) {
    visible.sort_unstable_by(|&a, &b| {
        let depth = |i: usize| {
            let bounds = bounds[i];
            ((bounds.min + bounds.max) * 0.5 - eye).dot(direction)
        };
        depth(b).total_cmp(&depth(a)).then_with(|| a.cmp(&b))
    });
    indices.clear();
    for &quad in visible.iter() {
        let vertex = quad as u32 * 4;
        indices.extend_from_slice(&[
            vertex,
            vertex + 1,
            vertex + 2,
            vertex,
            vertex + 2,
            vertex + 3,
        ]);
    }
}

#[derive(Default)]
struct SpatialTree {
    bounds: Vec<Bounds>,
    order: Vec<usize>,
    nodes: Vec<Node>,
}

struct Node {
    bounds: Bounds,
    contents: NodeContents,
}

enum NodeContents {
    Branch([usize; 2]),
    Leaf(Range<usize>),
}

impl SpatialTree {
    fn new(bounds: Vec<Bounds>) -> Self {
        let mut tree = Self {
            order: (0..bounds.len()).collect(),
            bounds,
            nodes: Vec::new(),
        };
        if !tree.order.is_empty() {
            tree.build(0..tree.order.len());
        }
        tree
    }

    fn build(&mut self, range: Range<usize>) -> usize {
        let bounds = Bounds::from_positions(
            self.order[range.clone()]
                .iter()
                .flat_map(|&i| [self.bounds[i].min, self.bounds[i].max]),
        );
        let index = self.nodes.len();
        self.nodes.push(Node {
            bounds,
            contents: NodeContents::Leaf(range.clone()),
        });
        if range.len() > 16 {
            // Long parallel water strips can share their longest box axis.
            // Split by center spread so their short separating axis still
            // produces useful contact and camera culling hierarchies.
            let centers = Bounds::from_positions(
                self.order[range.clone()]
                    .iter()
                    .map(|&i| (self.bounds[i].min + self.bounds[i].max) * 0.5),
            );
            let extent = centers.max - centers.min;
            let axis = if extent.x >= extent.y && extent.x >= extent.z {
                0
            } else if extent.y >= extent.z {
                1
            } else {
                2
            };
            let middle = range.start + range.len() / 2;
            self.order[range.clone()].select_nth_unstable_by(middle - range.start, |&a, &b| {
                let center = |i: usize| self.bounds[i].min[axis] + self.bounds[i].max[axis];
                center(a).total_cmp(&center(b)).then_with(|| a.cmp(&b))
            });
            let left = self.build(range.start..middle);
            let right = self.build(middle..range.end);
            self.nodes[index].contents = NodeContents::Branch([left, right]);
        }
        index
    }

    fn collect(&self, intersects: impl Fn(Bounds) -> bool, output: &mut Vec<usize>) {
        let mut pending = Vec::with_capacity(32);
        if !self.nodes.is_empty() {
            pending.push(0);
        }
        while let Some(index) = pending.pop() {
            let node = &self.nodes[index];
            if !intersects(node.bounds) {
                continue;
            }
            match &node.contents {
                NodeContents::Branch(children) => pending.extend_from_slice(children),
                NodeContents::Leaf(range) => output.extend(
                    self.order[range.clone()]
                        .iter()
                        .copied()
                        .filter(|&i| intersects(self.bounds[i])),
                ),
            }
        }
    }

    fn visible(&self, frustum: &Frustum, output: &mut Vec<usize>) {
        self.collect(|bounds| frustum.intersects(bounds), output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(slots: &Slots) -> Vec<(usize, usize)> {
        slots.free.iter().map(|r| (r.start, r.end)).collect()
    }

    #[test]
    fn slots_reuse_released_runs_and_grow_by_doubling() {
        let mut slots = Slots::default();
        assert_eq!(slots.allocate(100), 0);
        assert_eq!(slots.capacity, MIN_SLOTS);
        assert_eq!(slots.allocate(100), 100);
        assert_eq!(runs(&slots), [(200, 256)]);
        // A run too big for the free tail grows the pool, taking the tail.
        assert_eq!(slots.allocate(100), 200);
        assert_eq!(slots.capacity, 512);
        assert_eq!(runs(&slots), [(300, 512)]);
        // Released runs join their neighbours.
        slots.release(100..200);
        slots.release(0..100);
        assert_eq!(runs(&slots), [(0, 200), (300, 512)]);
        slots.release(200..300);
        assert_eq!(runs(&slots), [(0, 512)]);
        // A unit rebuilt the same size lands in the same place.
        assert_eq!(slots.allocate(40), 0);
        slots.release(0..40);
        assert_eq!(slots.allocate(40), 0);
        assert_eq!(runs(&slots), [(40, 512)]);
    }

    #[test]
    fn slots_never_overlap_under_random_use() {
        let mut slots = Slots::default();
        let mut live: Vec<Range<usize>> = Vec::new();
        let mut seed = 12345_u64;
        let mut next = move |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        for _ in 0..2000 {
            if !live.is_empty() && next(3) == 0 {
                let run = live.swap_remove(next(live.len() as u64) as usize);
                slots.release(run);
            } else {
                let count = 1 + next(300) as usize;
                let start = slots.allocate(count);
                assert!(start + count <= slots.capacity);
                live.push(start..start + count);
            }
            let mut all: Vec<_> = live.iter().chain(&slots.free).cloned().collect();
            all.sort_by_key(|r| r.start);
            let mut end = 0;
            for r in &all {
                assert_eq!(r.start, end, "{all:?}");
                end = r.end;
            }
            assert_eq!(end, slots.capacity);
        }
    }

    #[test]
    fn transparency_sorts_visible_quads_by_view_depth_and_preserves_winding() {
        let bounds: Vec<_> = [
            Vec3::new(100.0, 0.0, -1.0),
            Vec3::new(0.0, 0.0, -10.0),
            Vec3::new(0.0, 0.0, -4.0),
            Vec3::new(0.0, 0.0, -100.0),
        ]
        .map(|min| Bounds {
            min,
            max: min + Vec3::new(1.0, 1.0, 0.0),
        })
        .to_vec();
        let mut visible = vec![0, 1, 2];
        let mut indices = Vec::new();
        sorted_indices(&bounds, &mut visible, &mut indices, Vec3::ZERO, -Vec3::Z);
        assert_eq!(visible, [1, 2, 0]);
        assert_eq!(
            indices,
            [4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11, 0, 1, 2, 0, 2, 3]
        );
        // Off-screen quad 3 contributes no indices, even though it is furthest.
        assert!(indices.iter().all(|&index| index < 12));
        sorted_indices(&bounds, &mut visible, &mut indices, Vec3::ZERO, Vec3::Z);
        assert_eq!(visible, [0, 2, 1]);
    }

    #[test]
    fn culling_keeps_a_surface_the_camera_is_inside() {
        let bounds = vec![
            Bounds {
                min: Vec3::splat(-4096.0),
                max: Vec3::splat(4096.0),
            };
            6
        ];
        let tree = SpatialTree::new(bounds);
        let mut visible = Vec::new();
        tree.visible(
            &Frustum::new(glam::Mat4::perspective_rh(1.0, 1.0, 0.06, 8192.0)),
            &mut visible,
        );
        assert_eq!(visible.len(), 6);
    }

    #[test]
    fn vertex_layout_matches_the_struct() {
        assert_eq!(std::mem::size_of::<VolumeVertex>(), 48);
    }
}
