//! Short, original synthesized cues. Audio failure never prevents play.

use std::time::Instant;

#[derive(Clone, Copy)]
pub enum Cue {
    Shot,
    Rocket,
    Hit,
    Edit,
}

#[derive(Clone, Copy)]
enum Wave {
    Sine,
    Triangle,
}

#[derive(Clone, Copy)]
struct Tone {
    start_hz: f32,
    end_hz: f32,
    seconds: f32,
    level: f32,
    wave: Wave,
}

impl Cue {
    fn tones(self) -> [Tone; 2] {
        let tone = |start_hz, end_hz, seconds, level, wave| Tone {
            start_hz,
            end_hz,
            seconds,
            level,
            wave,
        };
        match self {
            Self::Shot => [
                tone(880.0, 110.0, 0.085, 0.04, Wave::Triangle),
                tone(150.0, 48.0, 0.105, 0.055, Wave::Sine),
            ],
            Self::Rocket => [
                tone(180.0, 45.0, 0.24, 0.05, Wave::Triangle),
                tone(75.0, 32.0, 0.26, 0.055, Wave::Sine),
            ],
            Self::Hit => [
                tone(1100.0, 1550.0, 0.065, 0.035, Wave::Sine),
                tone(1650.0, 2100.0, 0.035, 0.02, Wave::Sine),
            ],
            Self::Edit => [
                tone(460.0, 690.0, 0.045, 0.022, Wave::Sine),
                tone(920.0, 1380.0, 0.03, 0.009, Wave::Sine),
            ],
        }
    }
}

pub struct Audio {
    backend: backend::AudioBackend,
    last_play: [Option<Instant>; 4],
}

impl Audio {
    pub fn new() -> Self {
        Self {
            backend: backend::AudioBackend::new(),
            last_play: [None; 4],
        }
    }

    pub fn play(&mut self, cue: Cue) {
        let now = Instant::now();
        let previous = &mut self.last_play[cue as usize];
        // Scroll bursts and repeated network effects should not pile up voices.
        if previous.is_some_and(|time| now.duration_since(time).as_secs_f32() < 0.04) {
            return;
        }
        *previous = Some(now);
        self.backend.play(cue);
    }
}

mod backend {
    use super::{Cue, Wave};
    use rodio::{buffer::SamplesBuffer, OutputStream, OutputStreamHandle};

    const SAMPLE_RATE: u32 = 44_100;

    struct Output {
        // The stream must live as long as its playback handle.
        _stream: OutputStream,
        handle: OutputStreamHandle,
        clips: [SamplesBuffer<f32>; 4],
    }

    pub(super) struct AudioBackend {
        output: Option<Output>,
    }

    impl AudioBackend {
        pub(super) fn new() -> Self {
            let output = OutputStream::try_default()
                .ok()
                .map(|(stream, handle)| Output {
                    _stream: stream,
                    handle,
                    clips: [Cue::Shot, Cue::Rocket, Cue::Hit, Cue::Edit].map(synthesize),
                });
            Self { output }
        }

        pub(super) fn play(&mut self, cue: Cue) {
            if let Some(output) = &self.output {
                let _ = output.handle.play_raw(output.clips[cue as usize].clone());
            }
        }
    }

    fn synthesize(cue: Cue) -> SamplesBuffer<f32> {
        let tones = cue.tones();
        let duration = tones.iter().map(|tone| tone.seconds).fold(0.0, f32::max);
        let mut samples = vec![0.0; (duration * SAMPLE_RATE as f32).ceil() as usize];
        for tone in tones {
            let mut phase = 0.0f32;
            for (index, sample) in samples.iter_mut().enumerate() {
                let time = index as f32 / SAMPLE_RATE as f32;
                if time >= tone.seconds {
                    break;
                }
                let progress = time / tone.seconds;
                let frequency = tone.start_hz * (tone.end_hz / tone.start_hz).powf(progress);
                let signal = match tone.wave {
                    Wave::Sine => (phase * std::f32::consts::TAU).sin(),
                    Wave::Triangle => 1.0 - 4.0 * (phase - 0.5).abs(),
                };
                let attack = (time / 0.002).min(1.0);
                let release = ((tone.seconds - time) / 0.006).min(1.0);
                *sample += signal * tone.level * attack * (-6.0 * progress).exp() * release;
                phase = (phase + frequency / SAMPLE_RATE as f32).fract();
            }
        }
        SamplesBuffer::new(1, SAMPLE_RATE, samples)
    }
}
