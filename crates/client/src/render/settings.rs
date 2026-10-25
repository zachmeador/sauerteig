use serde::{Deserialize, Serialize};

// 4× MSAA on a 4K HDR scene measured 11 ms on an M3 Pro, over a 144 Hz budget;
// without it the same frame took 5 ms. Pixel density hides most aliasing there.
const AUTO_MSAA_MAX_PIXELS: u64 = 2560 * 1440;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Antialiasing {
    /// 4× MSAA while the scene has at most 2560×1440 pixels.
    #[default]
    Auto,
    On,
    Off,
}
impl Antialiasing {
    pub fn samples(self, scene_pixels: u64, supported: bool) -> u32 {
        let wanted = match self {
            Self::Auto => scene_pixels <= AUTO_MSAA_MAX_PIXELS,
            Self::On => true,
            Self::Off => false,
        };
        if wanted && supported {
            4
        } else {
            1
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderSettings {
    pub bloom: f32,
    pub msaa: Antialiasing,
    pub scale: f32,
    pub exposure: f32,
    pub vsync: bool,
}
impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            bloom: 0.06,
            msaa: Antialiasing::Auto,
            scale: 1.0,
            exposure: 1.0,
            vsync: true,
        }
    }
}
impl RenderSettings {
    pub fn normalized(self) -> Self {
        let finite = |v: f32, fallback: f32, min, max| {
            if v.is_finite() {
                v.clamp(min, max)
            } else {
                fallback
            }
        };
        Self {
            bloom: finite(self.bloom, 0.06, 0.0, 0.2),
            scale: finite(self.scale, 1.0, 0.5, 1.0),
            exposure: finite(self.exposure, 1.0, 0.25, 4.0),
            ..self
        }
    }
}

pub(super) fn present_mode(vsync: bool, supported: &[wgpu::PresentMode]) -> wgpu::PresentMode {
    if !vsync {
        for mode in [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate] {
            if supported.contains(&mode) {
                return mode;
            }
        }
    }
    wgpu::PresentMode::Fifo
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_settings_get_defaults_and_invalid_dimensions_are_bounded() {
        assert_eq!(
            serde_json::from_str::<RenderSettings>("{}").unwrap(),
            RenderSettings::default()
        );
        let fixed = RenderSettings {
            scale: f32::NAN,
            exposure: 10.0,
            bloom: -1.0,
            ..Default::default()
        }
        .normalized();
        assert_eq!((fixed.scale, fixed.exposure, fixed.bloom), (1.0, 4.0, 0.0));
        assert_eq!(
            present_mode(false, &[wgpu::PresentMode::Fifo]),
            wgpu::PresentMode::Fifo
        );
        assert_eq!(
            present_mode(
                false,
                &[wgpu::PresentMode::Fifo, wgpu::PresentMode::Immediate]
            ),
            wgpu::PresentMode::Immediate
        );
        assert_eq!(
            present_mode(true, &[wgpu::PresentMode::Fifo, wgpu::PresentMode::Mailbox]),
            wgpu::PresentMode::Fifo
        );
    }

    #[test]
    fn automatic_antialiasing_skips_msaa_above_1440p() {
        let qhd = 2560 * 1440;
        let uhd = 3840 * 2160;
        assert_eq!(Antialiasing::Auto.samples(qhd, true), 4);
        assert_eq!(Antialiasing::Auto.samples(uhd, true), 1);
        // 4K at 75% scale is still denser than 1440p.
        assert_eq!(Antialiasing::Auto.samples(2880 * 1620, true), 1);
        assert_eq!(Antialiasing::On.samples(uhd, true), 4);
        assert_eq!(Antialiasing::On.samples(qhd, false), 1);
        assert_eq!(Antialiasing::Off.samples(qhd, true), 1);
        assert_eq!(
            serde_json::from_str::<RenderSettings>(r#"{"msaa":"on"}"#)
                .unwrap()
                .msaa,
            Antialiasing::On
        );
    }
}
