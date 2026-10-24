//! Embedded, offline material library shared by native and WebGPU builds.
use sauerteig_core::MATERIAL_NAMES;
use wgpu::util::DeviceExt;

const TERRAIN_COUNT: usize = MATERIAL_NAMES.len();
const SIZE: u32 = 512;
const MIPS: u32 = 10;
const MAX_MATERIALS: usize = 64;

pub struct TextureAsset {
    pub name: &'static str,
    pub albedo: &'static [u8],
    pub normal_roughness: &'static [u8],
    pub metallic: f32,
    pub emission: f32,
    pub tile_metres: f32,
}
macro_rules! terrain {
    ($id:literal, $key:literal, $metal:expr, $emit:expr, $tile:expr) => {
        TextureAsset {
            name: MATERIAL_NAMES[$id],
            albedo: include_bytes!(concat!("../../../assets/materials/", $key, "/albedo.png")),
            normal_roughness: include_bytes!(concat!(
                "../../../assets/materials/",
                $key,
                "/normal_roughness.png"
            )),
            metallic: $metal,
            emission: $emit,
            tile_metres: $tile,
        }
    };
}
pub static TERRAIN: [TextureAsset; TERRAIN_COUNT] = [
    terrain!(0, "concrete", 0.0, 0.0, 2.0),
    terrain!(1, "gunmetal", 0.85, 0.0, 1.0),
    terrain!(2, "rust", 0.0, 0.0, 2.0),
    terrain!(3, "worn_concrete", 0.0, 0.0, 2.0),
    terrain!(4, "hazard", 0.15, 0.0, 1.0),
    terrain!(5, "amber", 0.0, 1.0, 1.0),
    terrain!(6, "rock", 0.0, 0.0, 2.0),
    terrain!(7, "earth", 0.0, 0.0, 2.0),
];

pub struct Materials {
    pub layout: wgpu::BindGroupLayout,
    pub group: wgpu::BindGroup,
    pub thumbnails: Vec<egui::ColorImage>,
    pub texture_bytes: usize,
}
impl Materials {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Self, String> {
        let mut assets: Vec<&TextureAsset> = TERRAIN.iter().collect();
        let prop_textures: Vec<_> = crate::props::textures()
            .iter()
            .map(|p| {
                log::debug!("Loading material {} ({})", p.name, p.key);
                TextureAsset {
                    name: p.key,
                    albedo: p.albedo,
                    normal_roughness: p.normal_roughness,
                    metallic: p.metallic,
                    emission: p.emission,
                    tile_metres: 1.0,
                }
            })
            .collect();
        assets.extend(prop_textures.iter());
        if assets.len() > MAX_MATERIALS {
            return Err("Too many embedded materials".into());
        }
        let make_texture = |label, format| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: SIZE,
                    height: SIZE,
                    depth_or_array_layers: assets.len() as u32,
                },
                mip_level_count: MIPS,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let albedo = make_texture(
            "material albedo and AO",
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        let normals = make_texture(
            "material normal and roughness",
            wgpu::TextureFormat::Rgba8Unorm,
        );
        let mut params = [[0.0f32; 4]; MAX_MATERIALS];
        let mut thumbnails = Vec::new();
        for (layer, asset) in assets.iter().enumerate() {
            params[layer] = [asset.metallic, asset.emission, 0.0, 0.0];
            for (bytes, texture, is_color) in [
                (asset.albedo, &albedo, true),
                (asset.normal_roughness, &normals, false),
            ] {
                let image = image::load_from_memory(bytes)
                    .map_err(|e| format!("{}: {e}", asset.name))?
                    .into_rgba8();
                if image.dimensions() != (SIZE, SIZE) {
                    return Err(format!("{} must be 512 x 512", asset.name));
                }
                if is_color && layer < TERRAIN_COUNT {
                    let thumb = image::imageops::resize(
                        &image,
                        64,
                        64,
                        image::imageops::FilterType::Triangle,
                    );
                    let rgb: Vec<_> = thumb.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect();
                    thumbnails.push(egui::ColorImage::from_rgb([64, 64], &rgb));
                }
                let mut pixels = image.into_raw();
                let mut size = SIZE;
                for mip in 0..MIPS {
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture,
                            mip_level: mip,
                            origin: wgpu::Origin3d {
                                x: 0,
                                y: 0,
                                z: layer as u32,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        &pixels,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(size * 4),
                            rows_per_image: Some(size),
                        },
                        wgpu::Extent3d {
                            width: size,
                            height: size,
                            depth_or_array_layers: 1,
                        },
                    );
                    if size > 1 {
                        pixels = downsample(&pixels, size, is_color);
                        size /= 2;
                    }
                }
            }
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("materials"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let view = |t: &wgpu::Texture| {
            t.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            })
        };
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("tiling materials"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            anisotropy_clamp: 4,
            ..Default::default()
        });
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("material parameters"),
            contents: bytemuck::cast_slice(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("materials"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view(&albedo)),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view(&normals)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        let texture_bytes = (0..MIPS)
            .map(|level| ((SIZE >> level).pow(2) * 8) as usize * assets.len())
            .sum();
        Ok(Self {
            layout,
            group,
            thumbnails,
            texture_bytes,
        })
    }
}
fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}
fn linear(v: u8) -> f32 {
    let x = v as f32 / 255.0;
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}
fn srgb(x: f32) -> u8 {
    let v = if x <= 0.0031308 {
        x * 12.92
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    };
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}
/// Box-filter in linear light; renormalize normals instead of averaging encoded vectors.
fn downsample(src: &[u8], size: u32, color: bool) -> Vec<u8> {
    let next = size / 2;
    let mut out = vec![0; (next * next * 4) as usize];
    for y in 0..next {
        for x in 0..next {
            let mut sum = [0.0; 4];
            for dy in 0..2 {
                for dx in 0..2 {
                    let i = (((y * 2 + dy) * size + x * 2 + dx) * 4) as usize;
                    for c in 0..4 {
                        sum[c] += if c == 3 {
                            src[i + c] as f32 / 255.0
                        } else if color {
                            linear(src[i + c])
                        } else {
                            src[i + c] as f32 / 127.5 - 1.0
                        };
                    }
                }
            }
            let i = ((y * next + x) * 4) as usize;
            if color {
                for c in 0..3 {
                    out[i + c] = srgb(sum[c] * 0.25);
                }
            } else {
                let n = glam::Vec3::new(sum[0], sum[1], sum[2])
                    .try_normalize()
                    .unwrap_or(glam::Vec3::Z);
                for c in 0..3 {
                    out[i + c] = ((n[c] * 0.5 + 0.5) * 255.0).round() as u8;
                }
            }
            out[i + 3] = (sum[3] * 63.75).round() as u8;
        }
    }
    out
}
/// World-space planar coordinates: subdivision does not reset texture scale.
pub fn world_uv(position: [f32; 3], normal: [f32; 3], material: u8) -> [f32; 2] {
    let [x, y, z] = position;
    let n = glam::Vec3::from_array(normal).abs();
    let uv = if n.x >= n.y && n.x >= n.z {
        [z, -y]
    } else if n.y >= n.z {
        [x, z]
    } else {
        [x, -y]
    };
    uv.map(|v| v / TERRAIN[material as usize].tile_metres)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mipmaps_average_light_not_encoded_brightness() {
        let pixels = [
            0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
        ];
        assert_eq!(downsample(&pixels, 2, true), [188, 188, 188, 255]);
        let normal = [128, 128, 255, 100].repeat(4);
        assert_eq!(downsample(&normal, 2, false), [128, 128, 255, 100]);
    }
    #[test]
    fn world_mapping_preserves_scale_and_survives_slopes() {
        assert_eq!(world_uv([2.0, 0.0, 4.0], [0.0, 1.0, 0.0], 0), [1.0, 2.0]);
        assert_eq!(world_uv([2.0, 0.125, 4.0], [0.0, 0.9, 0.3], 0), [1.0, 2.0]);
        assert_eq!(world_uv([2.0, 4.0, 1.0], [1.0, 0.0, 0.0], 1), [1.0, -4.0]);
    }
}
