use super::atlas::TileClass;
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

/// How much detail the sun's and the lights' shadows have: the sun cascades'
/// resolution and the largest tile a light's shadow may use. It never changes
/// which lights are shadowed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadowDetail {
    Low,
    Medium,
    #[default]
    High,
    Ultra,
}
impl ShadowDetail {
    pub const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::Ultra];

    /// Texels along one side of each sun cascade.
    pub fn cascade_size(self) -> u32 {
        match self {
            Self::Low => 512,
            Self::Medium | Self::High => 1024,
            Self::Ultra => 2048,
        }
    }

    /// The largest atlas tile a light's shadow may use.
    pub fn max_tile_class(self) -> TileClass {
        match self {
            Self::Low => TileClass::Small,
            Self::Medium => TileClass::Medium,
            Self::High | Self::Ultra => TileClass::Large,
        }
    }

    /// The shadow atlas's memory in 1024² pages of 4 MiB, static tiles and
    /// per-frame tiles together. The lowest level keeps 60 MiB; the others
    /// allow 256 MiB. A bound changes tile sizes, never which lights are
    /// shadowed.
    pub fn atlas_pages(self) -> usize {
        match self {
            Self::Low => 15,
            Self::Medium | Self::High | Self::Ultra => 64,
        }
    }

    pub fn label(self) -> String {
        format!(
            "{} (sun {}, lights {})",
            match self {
                Self::Low => "Low",
                Self::Medium => "Medium",
                Self::High => "High",
                Self::Ultra => "Ultra",
            },
            self.cascade_size(),
            self.max_tile_class().size(),
        )
    }
}

/// The fixed quality levels, lowest first. A level sets the render scale, the
/// anti-aliasing count and the shadow detail, and nothing else: every level
/// lights and shadows the same lights.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityLevel {
    Low,
    Medium,
    High,
    Ultra,
}

/// What a level or a custom choice asks the renderer for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resolved {
    pub scale: f32,
    pub msaa: Antialiasing,
    pub shadows: ShadowDetail,
}

impl QualityLevel {
    pub const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::Ultra];
    /// Where Auto begins.
    pub const START: Self = Self::High;

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn from_index(index: usize) -> Self {
        Self::ALL[index.min(Self::ALL.len() - 1)]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::Ultra => "Ultra",
        }
    }

    pub fn resolved(self) -> Resolved {
        let (scale, msaa, shadows) = match self {
            Self::Low => (0.75, Antialiasing::Off, ShadowDetail::Low),
            Self::Medium => (1.0, Antialiasing::X2, ShadowDetail::Medium),
            Self::High => (1.0, Antialiasing::X4, ShadowDetail::High),
            Self::Ultra => (1.0, Antialiasing::X8, ShadowDetail::Ultra),
        };
        Resolved {
            scale,
            msaa,
            shadows,
        }
    }
}

/// The Quality setting: Auto, a fixed level, or the individual choices below.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    /// Moves between levels from measured frame times.
    #[default]
    Auto,
    Low,
    Medium,
    High,
    Ultra,
    /// Render scale, shadow detail and anti-aliasing chosen by hand.
    Custom,
}
impl Quality {
    pub fn level(self) -> Option<QualityLevel> {
        match self {
            Self::Low => Some(QualityLevel::Low),
            Self::Medium => Some(QualityLevel::Medium),
            Self::High => Some(QualityLevel::High),
            Self::Ultra => Some(QualityLevel::Ultra),
            Self::Auto | Self::Custom => None,
        }
    }

    pub const ALL: [Self; 6] = [
        Self::Auto,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Ultra,
        Self::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Custom => "Custom",
            fixed => fixed.level().map_or("", QualityLevel::label),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "RawSettings")]
pub struct RenderSettings {
    pub bloom: f32,
    /// The anti-aliasing choice when `quality` is Custom.
    pub msaa: Antialiasing,
    /// The render scale when `quality` is Custom.
    pub scale: f32,
    pub exposure: f32,
    pub vsync: bool,
    pub quality: Quality,
    /// The shadow detail when `quality` is Custom.
    pub shadows: ShadowDetail,
}
impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            bloom: 0.06,
            msaa: Antialiasing::Auto,
            scale: 1.0,
            exposure: 1.0,
            vsync: true,
            quality: Quality::Auto,
            shadows: ShadowDetail::High,
        }
    }
}

/// Saved settings as they come off disk. Files from before Quality existed
/// have a scale and an anti-aliasing choice but no quality: those that differ
/// from the old defaults were made by hand, so they load as Custom.
#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSettings {
    bloom: Option<f32>,
    msaa: Option<Antialiasing>,
    scale: Option<f32>,
    exposure: Option<f32>,
    vsync: Option<bool>,
    quality: Option<Quality>,
    shadows: Option<ShadowDetail>,
}
impl From<RawSettings> for RenderSettings {
    fn from(raw: RawSettings) -> Self {
        let defaults = Self::default();
        let msaa = raw.msaa.unwrap_or(defaults.msaa);
        let scale = raw.scale.unwrap_or(defaults.scale);
        Self {
            bloom: raw.bloom.unwrap_or(defaults.bloom),
            msaa,
            scale,
            exposure: raw.exposure.unwrap_or(defaults.exposure),
            vsync: raw.vsync.unwrap_or(defaults.vsync),
            quality: raw
                .quality
                .unwrap_or(if msaa == defaults.msaa && scale == defaults.scale {
                    Quality::Auto
                } else {
                    Quality::Custom
                }),
            shadows: raw.shadows.unwrap_or(defaults.shadows),
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

    /// These settings with the scale and anti-aliasing a resolved choice asks
    /// for, as the scene targets are built from.
    pub fn with_resolved(self, resolved: Resolved) -> Self {
        Self {
            scale: resolved.scale,
            msaa: resolved.msaa,
            shadows: resolved.shadows,
            ..self
        }
    }

    /// What the renderer should draw with: the chosen level's settings, Auto's
    /// current level's, or the individual choices.
    pub fn resolved(&self, auto: QualityLevel) -> Resolved {
        match self.quality {
            Quality::Auto => auto.resolved(),
            Quality::Custom => Resolved {
                scale: self.scale,
                msaa: self.msaa,
                shadows: self.shadows,
            },
            fixed => fixed
                .level()
                .map_or_else(Resolved::default, QualityLevel::resolved),
        }
    }
}
impl Default for Resolved {
    fn default() -> Self {
        QualityLevel::START.resolved()
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

    #[test]
    fn saved_settings_without_a_quality_keep_what_the_player_chose() {
        let load = |text: &str| serde_json::from_str::<RenderSettings>(text).unwrap();
        // Settings that only change what Quality doesn't govern stay Auto.
        assert_eq!(
            load(r#"{"bloom":0.1,"vsync":false}"#).quality,
            Quality::Auto
        );
        assert_eq!(
            load(r#"{"scale":1.0,"msaa":"auto"}"#).quality,
            Quality::Auto
        );
        // A hand-picked scale or anti-aliasing count is Custom, with its values.
        let scaled = load(r#"{"scale":0.8}"#);
        assert_eq!((scaled.quality, scaled.scale), (Quality::Custom, 0.8));
        let aa = load(r#"{"msaa":"x4"}"#);
        assert_eq!((aa.quality, aa.msaa), (Quality::Custom, Antialiasing::X4));
        assert_eq!(load(r#"{"msaa":"on"}"#).quality, Quality::Custom);
        // A saved quality wins, and round trips.
        for quality in Quality::ALL {
            let saved = serde_json::to_string(&RenderSettings {
                quality,
                shadows: ShadowDetail::Ultra,
                ..Default::default()
            })
            .unwrap();
            let back = load(&saved);
            assert_eq!(back.quality, quality);
            assert_eq!(back.shadows, ShadowDetail::Ultra);
        }
        assert_eq!(
            load(r#"{"quality":"high","scale":0.6}"#).quality,
            Quality::High
        );
    }

    #[test]
    fn levels_get_stronger_and_a_choice_by_hand_is_custom() {
        let costs: Vec<_> = QualityLevel::ALL.iter().map(|l| l.resolved()).collect();
        let samples = |r: &Resolved| r.msaa.samples(1, &[1, 2, 4, 8]);
        for pair in costs.windows(2) {
            assert!(pair[0].scale <= pair[1].scale);
            assert!(samples(&pair[0]) <= samples(&pair[1]));
            assert!(pair[0].shadows.cascade_size() <= pair[1].shadows.cascade_size());
            assert!(pair[0].shadows.max_tile_class() <= pair[1].shadows.max_tile_class());
        }
        let settings = RenderSettings {
            quality: Quality::Ultra,
            scale: 0.6,
            msaa: Antialiasing::Off,
            ..Default::default()
        };
        // A fixed level ignores the individual fields; Custom uses them.
        assert_eq!(
            settings.resolved(QualityLevel::Low),
            QualityLevel::Ultra.resolved()
        );
        let custom = RenderSettings {
            quality: Quality::Custom,
            ..settings
        };
        assert_eq!(custom.resolved(QualityLevel::Ultra).scale, 0.6);
        let auto = RenderSettings {
            quality: Quality::Auto,
            ..settings
        };
        assert_eq!(
            auto.resolved(QualityLevel::Medium),
            QualityLevel::Medium.resolved()
        );
    }
}
