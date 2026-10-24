//! Short, original synthesized cues. Audio failure never prevents play.

use web_time::Instant;

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

    /// Call directly from a user click so browser autoplay rules permit audio.
    pub fn unlock(&mut self) {
        self.backend.unlock();
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

#[cfg(not(target_arch = "wasm32"))]
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

        pub(super) fn unlock(&mut self) {}

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

#[cfg(target_arch = "wasm32")]
mod backend {
    use super::{Cue, Tone, Wave};
    use wasm_bindgen::JsValue;
    use wasm_bindgen_futures::{spawn_local, JsFuture};
    use web_sys::{AudioContext, AudioContextState, GainNode, OscillatorNode, OscillatorType};

    struct Voice {
        oscillator: OscillatorNode,
        gain: GainNode,
        ends: f64,
    }

    impl Drop for Voice {
        fn drop(&mut self) {
            let _ = self.oscillator.stop();
            let _ = self.oscillator.disconnect();
            let _ = self.gain.disconnect();
        }
    }

    pub(super) struct AudioBackend {
        context: Option<AudioContext>,
        voices: Vec<Voice>,
    }

    impl AudioBackend {
        pub(super) fn new() -> Self {
            Self {
                context: None,
                voices: Vec::new(),
            }
        }

        pub(super) fn unlock(&mut self) {
            if self.context.is_none() {
                self.context = AudioContext::new().ok();
            }
            if let Some(context) = &self.context {
                if context.state() == AudioContextState::Suspended {
                    if let Ok(resumed) = context.resume() {
                        // A denied resume is an optional-audio failure, not an
                        // unhandled promise rejection for the game bootstrap.
                        spawn_local(async move {
                            let _ = JsFuture::from(resumed).await;
                        });
                    }
                }
            }
        }

        pub(super) fn play(&mut self, cue: Cue) {
            let Some(context) = &self.context else {
                return;
            };
            if context.state() != AudioContextState::Running {
                return;
            }
            let now = context.current_time();
            self.voices.retain(|voice| voice.ends > now);
            for tone in cue.tones() {
                // Keep graph references bounded even during rapid edits/hits.
                if self.voices.len() >= 16 {
                    self.voices.remove(0);
                }
                if let Ok(voice) = schedule(context, tone, now) {
                    self.voices.push(voice);
                }
            }
        }
    }

    impl Drop for AudioBackend {
        fn drop(&mut self) {
            self.voices.clear();
            if let Some(context) = self.context.take() {
                if let Ok(closed) = context.close() {
                    spawn_local(async move {
                        let _ = JsFuture::from(closed).await;
                    });
                }
            }
        }
    }

    fn schedule(context: &AudioContext, tone: Tone, now: f64) -> Result<Voice, JsValue> {
        let oscillator = context.create_oscillator()?;
        let gain = context.create_gain()?;
        let ends = now + tone.seconds as f64;
        // Construct ownership before connecting, so every error path disconnects.
        let voice = Voice {
            oscillator,
            gain,
            ends,
        };
        voice.oscillator.set_type(match tone.wave {
            Wave::Sine => OscillatorType::Sine,
            Wave::Triangle => OscillatorType::Triangle,
        });
        let frequency = voice.oscillator.frequency();
        frequency.set_value_at_time(tone.start_hz, now)?;
        frequency.exponential_ramp_to_value_at_time(tone.end_hz, ends)?;
        let envelope = voice.gain.gain();
        envelope.set_value_at_time(0.0, now)?;
        envelope.linear_ramp_to_value_at_time(tone.level, now + 0.002)?;
        envelope.exponential_ramp_to_value_at_time(0.0001, ends - 0.004)?;
        envelope.linear_ramp_to_value_at_time(0.0, ends)?;
        voice.oscillator.connect_with_audio_node(&voice.gain)?;
        voice.gain.connect_with_audio_node(&context.destination())?;
        voice.oscillator.start_with_when(now)?;
        voice.oscillator.stop_with_when(ends)?;
        Ok(voice)
    }
}
