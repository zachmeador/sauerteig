//! Cached transparent material boundaries. Boxes stay compact in world storage;
//! neither meshing nor culling expands them into metre cells.

use super::culling::{Bounds, Frustum};
use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use sauerteig_core::{MaterialVolume, VolumeMaterial, World};
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
}

impl VolumeVertex {
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![0=>Float32x3, 1=>Float32x3, 2=>Float32x2, 3=>Float32x2, 4=>Uint32];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRIBUTES,
        }
    }
}

pub(super) struct VolumeRenderer {
    pipeline: Vec<wgpu::RenderPipeline>,
    underwater_pipeline: Vec<wgpu::RenderPipeline>,
    vertex: Option<wgpu::Buffer>,
    index: wgpu::Buffer,
    tree: SpatialTree,
    authored: Vec<MaterialVolume>,
    visible: Vec<usize>,
    indices: Vec<u32>,
}

impl VolumeRenderer {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        camera_layout: &wgpu::BindGroupLayout,
        sample_counts: &[u32],
    ) -> Self {
        let shader = super::shaders::module(device, "volume");
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("transparent volume layout"),
            bind_group_layouts: &[camera_layout],
            push_constant_ranges: &[],
        });
        let pipeline = |label, vertex_entry, fragment_entry, buffers, depth_compare, bias| {
            sample_counts
                .iter()
                .map(|&sample_count| {
                    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                        label: Some(label),
                        layout: Some(&layout),
                        vertex: wgpu::VertexState {
                            module: &shader,
                            entry_point: Some(vertex_entry),
                            buffers,
                            compilation_options: Default::default(),
                        },
                        fragment: Some(wgpu::FragmentState {
                            module: &shader,
                            entry_point: Some(fragment_entry),
                            targets: &[Some(wgpu::ColorTargetState {
                                format,
                                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
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
            ),
            underwater_pipeline: pipeline(
                "underwater tint",
                "vs_underwater",
                "fs_underwater",
                &[],
                wgpu::CompareFunction::Always,
                wgpu::DepthBiasState::default(),
            ),
            vertex: None,
            index: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("visible transparent indices"),
                size: 256,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            tree: SpatialTree::default(),
            authored: Vec::new(),
            visible: Vec::new(),
            indices: Vec::new(),
        }
    }

    pub fn rebuild(&mut self, device: &wgpu::Device, world: &World) {
        if self.authored == world.volumes() {
            return;
        }
        self.authored = world.volumes().to_vec();
        let vertices = boundary_vertices(&self.authored);
        self.tree = SpatialTree::new(
            vertices
                .as_chunks::<4>()
                .0
                .iter()
                .map(|quad| {
                    Bounds::from_positions(quad.iter().map(|v| Vec3::from_array(v.position)))
                })
                .collect(),
        );
        self.vertex = (!vertices.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("transparent volume boundaries"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        let index_bytes = self.tree.bounds.len() as u64 * 6 * 4;
        if index_bytes > self.index.size() {
            self.index = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("visible transparent indices"),
                size: index_bytes.next_power_of_two(),
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        self.visible.clear();
        self.indices.clear();
    }

    pub fn prepare(&mut self, queue: &wgpu::Queue, eye: Vec3, direction: Vec3, frustum: &Frustum) {
        self.visible.clear();
        self.tree.visible(frustum, &mut self.visible);
        // Cached vertices never change during a frame. Sort only the visible
        // boundary quads and stream their small index order as a single draw.
        // Centroid sorting is deliberately bounded alpha transparency, not OIT.
        sorted_indices(
            &self.tree.bounds,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    min: [i32; 2],
    max: [i32; 2],
}

impl Rect {
    fn subtract(self, other: Self) -> Vec<Self> {
        let cut = Self {
            min: std::array::from_fn(|a| self.min[a].max(other.min[a])),
            max: std::array::from_fn(|a| self.max[a].min(other.max[a])),
        };
        if (0..2).any(|a| cut.min[a] >= cut.max[a]) {
            return vec![self];
        }
        let mut remaining = self;
        let mut result = Vec::with_capacity(4);
        for axis in 0..2 {
            if remaining.min[axis] < cut.min[axis] {
                let mut piece = remaining;
                piece.max[axis] = cut.min[axis];
                result.push(piece);
                remaining.min[axis] = cut.min[axis];
            }
            if remaining.max[axis] > cut.max[axis] {
                let mut piece = remaining;
                piece.min[axis] = cut.max[axis];
                result.push(piece);
                remaining.max[axis] = cut.max[axis];
            }
        }
        result
    }

    fn bounds(self) -> Bounds {
        Bounds {
            min: Vec3::new(self.min[0] as f32, self.min[1] as f32, -1.0),
            max: Vec3::new(self.max[0] as f32, self.max[1] as f32, 1.0),
        }
    }
}

#[derive(Clone, Copy)]
struct Face {
    axis: usize,
    positive: bool,
    plane: i32,
    rect: Rect,
    material: u32,
}

impl Face {
    fn key(self, positive: bool) -> (usize, i32, u32, bool) {
        (self.axis, self.plane, self.material, positive)
    }

    fn vertices(self, rect: Rect) -> [VolumeVertex; 4] {
        let u = (self.axis + 1) % 3;
        let v = (self.axis + 2) % 3;
        let mut normal = [0.0; 3];
        normal[self.axis] = if self.positive { 1.0 } else { -1.0 };
        // u cross v follows the positive axis for this cyclic axis order.
        let corners = if self.positive {
            [[0, 0], [1, 0], [1, 1], [0, 1]]
        } else {
            [[0, 0], [0, 1], [1, 1], [1, 0]]
        };
        corners.map(|corner| {
            let coordinates = std::array::from_fn::<_, 2, _>(|a| {
                if corner[a] == 0 {
                    rect.min[a]
                } else {
                    rect.max[a]
                }
            });
            let mut position = [0.0; 3];
            position[self.axis] = self.plane as f32 / 8.0;
            position[u] = coordinates[0] as f32 / 8.0;
            position[v] = coordinates[1] as f32 / 8.0;
            VolumeVertex {
                position,
                normal,
                // Keep edge shading tied to the authored face, rather than
                // outlining the temporary rectangles left by neighbor cuts.
                uv: std::array::from_fn(|a| (coordinates[a] - self.rect.min[a]) as f32 / 8.0),
                size: std::array::from_fn(|a| (self.rect.max[a] - self.rect.min[a]) as f32 / 8.0),
                material: self.material,
            }
        })
    }
}

fn boundary_vertices(volumes: &[MaterialVolume]) -> Vec<VolumeVertex> {
    let mut faces = Vec::with_capacity(volumes.len() * 6);
    let mut planes: BTreeMap<_, Vec<Rect>> = BTreeMap::new();
    for volume in volumes {
        if volume.bounds.is_empty() {
            continue;
        }
        let material = match volume.material {
            VolumeMaterial::Water => 0,
            VolumeMaterial::Glass => 1,
        };
        for axis in 0..3 {
            let u = (axis + 1) % 3;
            let v = (axis + 2) % 3;
            for positive in [false, true] {
                let face = Face {
                    axis,
                    positive,
                    plane: if positive {
                        volume.bounds.max[axis]
                    } else {
                        volume.bounds.min[axis]
                    },
                    rect: Rect {
                        min: [volume.bounds.min[u], volume.bounds.min[v]],
                        max: [volume.bounds.max[u], volume.bounds.max[v]],
                    },
                    material,
                };
                planes
                    .entry(face.key(positive))
                    .or_default()
                    .push(face.rect);
                faces.push(face);
            }
        }
    }
    // A plane can contain thousands of disjoint rectangles. Its hierarchy
    // finds actual contacts without comparing every authored box with every
    // other box, and has no grid-size-dependent expansion.
    let planes: BTreeMap<_, _> = planes
        .into_iter()
        .map(|(key, rectangles)| {
            let tree = SpatialTree::new(rectangles.iter().map(|rect| rect.bounds()).collect());
            (key, (rectangles, tree))
        })
        .collect();
    let mut vertices = Vec::new();
    let mut neighbors = Vec::new();
    for face in faces {
        let mut visible = vec![face.rect];
        if let Some((rectangles, tree)) = planes.get(&face.key(!face.positive)) {
            neighbors.clear();
            tree.overlapping(face.rect.bounds(), &mut neighbors);
            for &neighbor in &neighbors {
                visible = visible
                    .into_iter()
                    .flat_map(|rect| rect.subtract(rectangles[neighbor]))
                    .collect();
                if visible.is_empty() {
                    break;
                }
            }
        }
        for rect in visible {
            vertices.extend_from_slice(&face.vertices(rect));
        }
    }
    vertices
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

    fn overlapping(&self, query: Bounds, output: &mut Vec<usize>) {
        self.collect(
            |bounds| query.min.cmplt(bounds.max).all() && bounds.min.cmplt(query.max).all(),
            output,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sauerteig_core::FineBox;

    fn volume(min: [i32; 3], max: [i32; 3], material: VolumeMaterial) -> MaterialVolume {
        MaterialVolume {
            bounds: FineBox { min, max },
            material,
        }
    }

    fn area(vertices: &[VolumeVertex], normal: [f32; 3], plane: f32) -> f32 {
        vertices
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|quad| {
                quad[0].normal == normal
                    && Vec3::from_array(quad[0].position).dot(Vec3::from_array(normal)) == plane
            })
            .map(|quad| {
                let a = Vec3::from_array(quad[1].position) - Vec3::from_array(quad[0].position);
                let b = Vec3::from_array(quad[3].position) - Vec3::from_array(quad[0].position);
                a.cross(b).length()
            })
            .sum()
    }

    #[test]
    fn equal_material_neighbors_remove_both_internal_faces() {
        let vertices = boundary_vertices(&[
            volume([0, 0, 0], [8, 8, 8], VolumeMaterial::Water),
            volume([8, 0, 0], [16, 8, 8], VolumeMaterial::Water),
        ]);
        assert_eq!(vertices.len(), 10 * 4);
        assert_eq!(area(&vertices, [1.0, 0.0, 0.0], 1.0), 0.0);
        assert_eq!(area(&vertices, [-1.0, 0.0, 0.0], -1.0), 0.0);
    }

    #[test]
    fn partial_contacts_cut_only_the_covered_rectangle() {
        let vertices = boundary_vertices(&[
            volume([0, 0, 0], [8, 16, 16], VolumeMaterial::Glass),
            volume([8, 4, 4], [16, 12, 12], VolumeMaterial::Glass),
        ]);
        assert_eq!(area(&vertices, [1.0, 0.0, 0.0], 1.0), 3.0);
        assert_eq!(area(&vertices, [-1.0, 0.0, 0.0], -1.0), 0.0);
        // Neighbor cuts must not introduce shader outlines at the cut edges.
        for quad in vertices.as_chunks::<4>().0.iter() {
            if quad[0].normal == [1.0, 0.0, 0.0] && quad[0].position[0] == 1.0 {
                assert!(quad.iter().all(|vertex| vertex.size == [2.0, 2.0]));
            }
        }
    }

    #[test]
    fn different_media_keep_their_interface_and_all_faces_are_outward() {
        let vertices = boundary_vertices(&[
            volume([0, 0, 0], [8, 8, 8], VolumeMaterial::Glass),
            volume([8, 0, 0], [16, 8, 8], VolumeMaterial::Water),
        ]);
        assert_eq!(vertices.len(), 12 * 4);
        for quad in vertices.as_chunks::<4>().0.iter() {
            let position = |i: usize| Vec3::from_array(quad[i].position);
            let geometric = (position(1) - position(0))
                .cross(position(2) - position(0))
                .normalize();
            assert_eq!(geometric, Vec3::from_array(quad[0].normal));
        }
    }

    #[test]
    fn giant_box_costs_six_quads_and_culling_retains_inside_faces() {
        let vertices =
            boundary_vertices(&[volume([-32_768; 3], [32_768; 3], VolumeMaterial::Water)]);
        assert_eq!(vertices.len(), 6 * 4);
        let tree = SpatialTree::new(
            vertices
                .as_chunks::<4>()
                .0
                .iter()
                .map(|quad| {
                    Bounds::from_positions(quad.iter().map(|v| Vec3::from_array(v.position)))
                })
                .collect(),
        );
        let mut visible = Vec::new();
        tree.visible(
            &Frustum::new(glam::Mat4::perspective_rh(1.0, 1.0, 0.06, 8192.0)),
            &mut visible,
        );
        assert!(!visible.is_empty());
    }

    #[test]
    fn hierarchical_contacts_match_direct_rectangle_queries() {
        let bounds: Vec<_> = (0..1024)
            .map(|i| {
                Rect {
                    min: [(i % 32) * 8, (i / 32) * 8],
                    max: [(i % 32) * 8 + 4, (i / 32) * 8 + 4],
                }
                .bounds()
            })
            .collect();
        let tree = SpatialTree::new(bounds.clone());
        let query = Rect {
            min: [95, 71],
            max: [111, 103],
        }
        .bounds();
        let mut actual = Vec::new();
        tree.overlapping(query, &mut actual);
        actual.sort_unstable();
        let expected: Vec<_> = bounds
            .iter()
            .enumerate()
            .filter(|(_, bounds)| {
                query.min.cmplt(bounds.max).all() && bounds.min.cmplt(query.max).all()
            })
            .map(|(i, _)| i)
            .collect();
        assert_eq!(actual, expected);
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
}
