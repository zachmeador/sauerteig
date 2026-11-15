//! GPU meshes and instance batches for embedded props.

use super::{
    culling::{Bounds, Frustum},
    lightset::MAX_TILE_JOBS,
    GpuMesh, GpuVertex,
};
use crate::{
    lighting::CASCADES,
    props::{self, PropMesh},
};
use glam::Vec3;
use sauerteig_core::{ObjectKind, WorldObject};

const MAX_INSTANCES_PER_KIND: usize = 1024;
// Translation in xyz, followed by rotation around the y axis.
type Transform = [f32; 4];

pub(super) fn instance_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![6=>Float32x4];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Transform>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &ATTRS,
    }
}

/// A shadow tile's view of the props: which are inside it and in the light's reach.
pub(super) struct TileView {
    pub frustum: Frustum,
    pub center: Vec3,
    pub range: f32,
}

/// What a shadow draw of props is for.
#[derive(Clone, Copy)]
pub(super) enum ShadowTarget {
    Cascade(usize),
    Tile(usize),
}

struct PropLod {
    mesh: GpuMesh,
    instances: wgpu::Buffer,
    transforms: Vec<Transform>,
    world_count: u32,
    tile_ranges: Vec<(u32, u32)>,
    sun_ranges: [(u32, u32); CASCADES],
}

impl PropLod {
    fn new(device: &wgpu::Device, mesh: PropMesh) -> Result<Self, String> {
        let vertices: Vec<_> = mesh
            .vertices
            .into_iter()
            .map(|v| GpuVertex {
                position: v.position,
                normal: v.normal,
                color: [1.0; 3],
                emission: v.emission,
                uv: v.uv,
                material: v.material,
            })
            .collect();
        // The world pass, each cascade of the sun and each tile rendered in a
        // frame have their own instance range.
        let capacity = MAX_INSTANCES_PER_KIND * (MAX_TILE_JOBS + 1 + CASCADES);
        Ok(Self {
            mesh: GpuMesh::new(device, &vertices, &mesh.indices).ok_or("Empty prop mesh")?,
            instances: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("prop instances"),
                size: (capacity * std::mem::size_of::<Transform>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            transforms: Vec::with_capacity(capacity),
            world_count: 0,
            tile_ranges: Vec::new(),
            sun_ranges: [(0, 0); CASCADES],
        })
    }
}

struct PropBatch {
    kind: ObjectKind,
    near: PropLod,
    distant: PropLod,
    lod_distance: f32,
    bounds_center: Vec3,
    bounds_radius: f32,
}

pub(super) struct PropRenderer {
    batches: Vec<PropBatch>,
}

impl PropRenderer {
    pub(super) fn new(device: &wgpu::Device) -> Result<Self, String> {
        let batches = props::load()?
            .into_iter()
            .map(|asset| {
                let definition = asset.definition;
                let min = Vec3::from_array(definition.bounds_min);
                let max = Vec3::from_array(definition.bounds_max);
                log::debug!("Loaded prop {}", definition.name);
                Ok(PropBatch {
                    kind: definition.kind,
                    near: PropLod::new(device, asset.near)?,
                    distant: PropLod::new(device, asset.distant)?,
                    lod_distance: definition.lod_distance,
                    bounds_center: (min + max) * 0.5,
                    bounds_radius: min.distance(max) * 0.5,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(Self { batches })
    }

    pub(super) fn visible_triangles(&self) -> usize {
        self.batches
            .iter()
            .map(|b| {
                (b.near.mesh.count as usize * b.near.world_count as usize
                    + b.distant.mesh.count as usize * b.distant.world_count as usize)
                    / 3
            })
            .sum()
    }

    pub(super) fn prepare(
        &mut self,
        queue: &wgpu::Queue,
        objects: &[WorldObject],
        eye: Vec3,
        draw_distance: f32,
        sun_frustums: &[Frustum; CASCADES],
        tiles: &[TileView],
    ) {
        for batch in &mut self.batches {
            batch.near.transforms.clear();
            batch.distant.transforms.clear();
            for object in objects
                .iter()
                .filter(|o| o.kind == batch.kind)
                .take(MAX_INSTANCES_PER_KIND)
            {
                let distance = eye.distance_squared(object.position);
                if distance > draw_distance * draw_distance {
                    continue;
                }
                let lod = if distance < batch.lod_distance.powi(2) {
                    &mut batch.near
                } else {
                    &mut batch.distant
                };
                lod.transforms.push([
                    object.position.x,
                    object.position.y,
                    object.position.z,
                    object.rotation,
                ]);
            }
            batch.near.world_count = batch.near.transforms.len() as u32;
            batch.distant.world_count = batch.distant.transforms.len() as u32;
            // Static shadow geometry uses the near mesh, independent of view
            // LOD or draw distance. Its cached projection is stable as we move.
            let mut append = |frustum: &Frustum, reach: Option<(Vec3, f32)>| {
                let start = batch.near.transforms.len() as u32;
                for object in objects
                    .iter()
                    .filter(|o| o.kind == batch.kind)
                    .take(MAX_INSTANCES_PER_KIND)
                {
                    let center = object.position
                        + glam::Mat3::from_rotation_y(object.rotation) * batch.bounds_center;
                    let radius = Vec3::splat(batch.bounds_radius);
                    if frustum.intersects(Bounds {
                        min: center - radius,
                        max: center + radius,
                    }) && reach.is_none_or(|(p, range)| {
                        center.distance_squared(p) <= (range + batch.bounds_radius).powi(2)
                    }) {
                        batch.near.transforms.push([
                            object.position.x,
                            object.position.y,
                            object.position.z,
                            object.rotation,
                        ]);
                    }
                }
                (start, batch.near.transforms.len() as u32)
            };
            for (range, frustum) in batch.near.sun_ranges.iter_mut().zip(sun_frustums) {
                *range = append(frustum, None);
            }
            batch.near.tile_ranges = tiles
                .iter()
                .map(|tile| append(&tile.frustum, Some((tile.center, tile.range))))
                .collect();
            for lod in [&mut batch.near, &mut batch.distant] {
                if !lod.transforms.is_empty() {
                    queue.write_buffer(&lod.instances, 0, bytemuck::cast_slice(&lod.transforms));
                }
            }
        }
    }

    pub(super) fn draw_shadow(&self, pass: &mut wgpu::RenderPass<'_>, target: ShadowTarget) {
        for batch in &self.batches {
            let lod = &batch.near;
            let (start, end) = match target {
                ShadowTarget::Cascade(i) => lod.sun_ranges[i],
                ShadowTarget::Tile(i) => lod.tile_ranges.get(i).copied().unwrap_or_default(),
            };
            Self::draw_lod(pass, lod, start, end);
        }
    }
    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        for batch in &self.batches {
            for lod in [&batch.near, &batch.distant] {
                Self::draw_lod(pass, lod, 0, lod.world_count);
            }
        }
    }
    fn draw_lod(pass: &mut wgpu::RenderPass<'_>, lod: &PropLod, start: u32, end: u32) {
        if start == end {
            return;
        }
        pass.set_vertex_buffer(0, lod.mesh.vertex.slice(..));
        pass.set_vertex_buffer(1, lod.instances.slice(..));
        pass.set_index_buffer(lod.mesh.index.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..lod.mesh.count, 0, start..end);
    }
}
