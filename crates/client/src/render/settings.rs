use serde::{Deserialize, Serialize};

// Auto's rule, from throughput measurements at 4K on an M5 Pro (docs/tracking/
// renderer.md, N1): there 2× cost nothing measurable, 4× about 0.1 ms and 8×
// about 0.5 ms of a 6 ms frame. An M3 Pro measured 4× at 4K at 11 ms against 5
// ms without, and a discrete GPU pays more for multisampling than either, so
// Auto takes 4× only up to 1440p, 2× up to 4K, and none above, where pixel
// density hides most aliasing. It never chooses 8×.
const AUTO_4X_MAX_PIXELS: u64 = 2560 * 1440;
const AUTO_2X_MAX_PIXELS: u64 = 3840 * 2160;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Antialiasing {
    /// 4× MSAA up to 2560×1440 scene pixels, 2× up to 3840×2160, then none.
    #[default]
    Auto,
    Off,
    #[serde(rename = "x2")]
    X2,
    /// Saved settings from before 2× and 8× existed call this `on`.
    #[serde(rename = "x4", alias = "on")]
    X4,
    #[serde(rename = "x8")]
    X8,
}
impl Antialiasing {
    /// The sample count this choice asks for at this scene size.
    fn wanted(self, scene_pixels: u64) -> u32 {
        match self {
            Self::Auto if scene_pixels <= AUTO_4X_MAX_PIXELS => 4,
            Self::Auto if scene_pixels <= AUTO_2X_MAX_PIXELS => 2,
            Self::Auto | Self::Off => 1,
            Self::X2 => 2,
            Self::X4 => 4,
            Self::X8 => 8,
        }
    }

    /// The sample count to render with: the one asked for, or the next lower
    /// one the adapter supports. `supported` lists the counts it offers.
    pub fn samples(self, scene_pixels: u64, supported: &[u32]) -> u32 {
        let wanted = self.wanted(scene_pixels);
        supported
            .iter()
            .copied()
            .filter(|&count| count <= wanted)
            .max()
            .unwrap_or(1)
    }

    /// The choices Settings offers an adapter with these sample counts.
    pub fn choices(supported: &[u32]) -> Vec<Self> {
        let mut choices = vec![Self::Auto, Self::Off];
        for (count, choice) in [(2, Self::X2), (4, Self::X4), (8, Self::X8)] {
            if supported.contains(&count) {
                choices.push(choice);
            }
        }
        choices
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto (4× to 1440p, 2× to 4K)",
            Self::Off => "Off",
            Self::X2 => "2× MSAA",
            Self::X4 => "4× MSAA",
            Self::X8 => "8× MSAA",
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
    fn automatic_antialiasing_steps_down_as_the_scene_grows() {
        let all = [1, 2, 4, 8];
        let qhd = 2560 * 1440;
        let uhd = 3840 * 2160;
        assert_eq!(Antialiasing::Auto.samples(1280 * 800, &all), 4);
        assert_eq!(Antialiasing::Auto.samples(qhd, &all), 4);
        assert_eq!(Antialiasing::Auto.samples(qhd + 1, &all), 2);
        // 4K at 75% scale is still denser than 1440p.
        assert_eq!(Antialiasing::Auto.samples(2880 * 1620, &all), 2);
        assert_eq!(Antialiasing::Auto.samples(uhd, &all), 2);
        assert_eq!(Antialiasing::Auto.samples(uhd + 1, &all), 1);
        // Never 8×, and never more than the adapter supports.
        assert_eq!(Antialiasing::Auto.samples(1, &all), 4);
        assert_eq!(Antialiasing::Auto.samples(qhd, &[1, 2]), 2);
        assert_eq!(Antialiasing::Auto.samples(uhd, &[1]), 1);
    }

    #[test]
    fn a_fixed_choice_uses_the_next_lower_supported_count() {
        let uhd = 3840 * 2160;
        for (choice, expected) in [
            (Antialiasing::Off, 1),
            (Antialiasing::X2, 2),
            (Antialiasing::X4, 4),
            (Antialiasing::X8, 8),
        ] {
            assert_eq!(choice.samples(uhd, &[1, 2, 4, 8]), expected);
        }
        assert_eq!(Antialiasing::X8.samples(uhd, &[1, 2, 4]), 4);
        assert_eq!(Antialiasing::X8.samples(uhd, &[1, 4]), 4);
        assert_eq!(Antialiasing::X4.samples(uhd, &[1, 2]), 2);
        assert_eq!(Antialiasing::X2.samples(uhd, &[1]), 1);
    }

    #[test]
    fn settings_offer_only_the_sample_counts_the_adapter_supports() {
        use Antialiasing::*;
        assert_eq!(
            Antialiasing::choices(&[1, 2, 4, 8]),
            [Auto, Off, X2, X4, X8]
        );
        assert_eq!(Antialiasing::choices(&[1, 2, 4]), [Auto, Off, X2, X4]);
        assert_eq!(Antialiasing::choices(&[1, 4]), [Auto, Off, X4]);
        assert_eq!(Antialiasing::choices(&[1]), [Auto, Off]);
    }

    #[test]
    fn saved_antialiasing_choices_load() {
        let load = |text: &str| serde_json::from_str::<RenderSettings>(text).unwrap().msaa;
        // Before 2× and 8×, `on` meant 4×.
        assert_eq!(load(r#"{"msaa":"on"}"#), Antialiasing::X4);
        assert_eq!(load(r#"{"msaa":"off"}"#), Antialiasing::Off);
        assert_eq!(load(r#"{"msaa":"auto"}"#), Antialiasing::Auto);
        for choice in [Antialiasing::X2, Antialiasing::X4, Antialiasing::X8] {
            let saved = serde_json::to_string(&RenderSettings {
                msaa: choice,
                ..Default::default()
            })
            .unwrap();
            assert_eq!(load(&saved), choice);
        }
        assert_eq!(serde_json::to_string(&Antialiasing::X4).unwrap(), r#""x4""#);
    }
}
