//! GPU meshes and instance batches for embedded props.

use super::{GpuMesh, GpuVertex};
use crate::{
    lighting::{LocalLight, LIGHT_RANGE, LOCAL_LIGHTS},
    props::{self, PropMesh},
};
use glam::Vec3;
use sauerteig_core::{ObjectKind, WorldObject};

const MAX_INSTANCES_PER_KIND: usize = 1024;
const DRAW_DISTANCE: f32 = 180.0;
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

struct PropLod {
    mesh: GpuMesh,
    instances: wgpu::Buffer,
    transforms: Vec<Transform>,
    world_count: u32,
    light_ranges: [(u32, u32); LOCAL_LIGHTS],
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
        // The world pass and each local light have their own instance range.
        let capacity = MAX_INSTANCES_PER_KIND * (LOCAL_LIGHTS + 1);
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
            light_ranges: [(0, 0); LOCAL_LIGHTS],
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
        lights: &[Option<LocalLight>; LOCAL_LIGHTS],
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
                if distance > DRAW_DISTANCE * DRAW_DISTANCE {
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
            for lod in [&mut batch.near, &mut batch.distant] {
                lod.world_count = lod.transforms.len() as u32;
                for (i, light) in lights.iter().enumerate() {
                    let start = lod.transforms.len() as u32;
                    if let Some(light) = light {
                        for index in 0..lod.world_count as usize {
                            let t = lod.transforms[index];
                            let center = Vec3::new(t[0], t[1], t[2])
                                + glam::Mat3::from_rotation_y(t[3]) * batch.bounds_center;
                            if center.distance_squared(light.position)
                                <= (LIGHT_RANGE + batch.bounds_radius).powi(2)
                            {
                                lod.transforms.push(t);
                            }
                        }
                    }
                    lod.light_ranges[i] = (start, lod.transforms.len() as u32);
                }
                if !lod.transforms.is_empty() {
                    queue.write_buffer(&lod.instances, 0, bytemuck::cast_slice(&lod.transforms));
                }
            }
        }
    }

    pub(super) fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, light: Option<usize>) {
        for batch in &self.batches {
            for lod in [&batch.near, &batch.distant] {
                let (start, end) = light.map_or((0, lod.world_count), |i| lod.light_ranges[i]);
                if start == end {
                    continue;
                }
                pass.set_vertex_buffer(0, lod.mesh.vertex.slice(..));
                pass.set_vertex_buffer(1, lod.instances.slice(..));
                pass.set_index_buffer(lod.mesh.index.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..lod.mesh.count, 0, start..end);
            }
        }
    }
}
