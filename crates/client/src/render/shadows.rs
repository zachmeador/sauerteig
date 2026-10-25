use crate::lighting::{LocalLight, LOCAL_LIGHTS};
use glam::{Mat4, Vec3};

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

/// Preserve GPU layers for retained lights as nearest-light distance ranks change.
pub(super) fn stable_slots(
    mut selected: [Option<LocalLight>; LOCAL_LIGHTS],
    previous: [Option<Vec3>; LOCAL_LIGHTS],
) -> [Option<LocalLight>; LOCAL_LIGHTS] {
    let mut slots = [None; LOCAL_LIGHTS];
    for (slot, previous) in slots.iter_mut().zip(previous) {
        if let Some(position) = previous {
            if let Some(i) = selected
                .iter()
                .position(|l| l.is_some_and(|l| l.position == position))
            {
                *slot = selected[i].take();
            }
        }
    }
    for light in selected.into_iter().flatten() {
        *slots.iter_mut().find(|s| s.is_none()).unwrap() = Some(light);
    }
    slots
}

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

pub(super) const STATIC_LABELS: [&str; LOCAL_LIGHTS] = [
    "local static 1",
    "local static 2",
    "local static 3",
    "local static 4",
    "local static 5",
    "local static 6",
    "local static 7",
    "local static 8",
];
pub(super) const ACTOR_LABELS: [&str; LOCAL_LIGHTS] = [
    "local actors 1",
    "local actors 2",
    "local actors 3",
    "local actors 4",
    "local actors 5",
    "local actors 6",
    "local actors 7",
    "local actors 8",
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_shadows_invalidate_on_geometry_light_or_window_change() {
        let mut stamp = ShadowStamp::default();
        assert!(stamp.update(Mat4::IDENTITY, 0));
        assert!(!stamp.update(Mat4::IDENTITY, 0));
        assert!(stamp.update(Mat4::IDENTITY, 1));
        assert!(stamp.update(Mat4::from_translation(Vec3::X), 1));
        assert!(!stamp.update(Mat4::from_translation(Vec3::X), 1));
    }
    #[test]
    fn nearest_light_reordering_does_not_invalidate_retained_layers() {
        let light = |x| {
            Some(LocalLight {
                position: Vec3::X * x,
                view_proj: Mat4::IDENTITY,
            })
        };
        let mut selected = [None; LOCAL_LIGHTS];
        selected[..3].copy_from_slice(&[light(2.0), light(1.0), light(3.0)]);
        let mut previous = [None; LOCAL_LIGHTS];
        previous[..3].copy_from_slice(&[Some(Vec3::X), Some(Vec3::X * 2.0), Some(Vec3::X * 4.0)]);
        let stable = stable_slots(selected, previous);
        assert_eq!(stable[0].unwrap().position, Vec3::X);
        assert_eq!(stable[1].unwrap().position, Vec3::X * 2.0);
        assert_eq!(stable[2].unwrap().position, Vec3::X * 3.0);
    }
}
