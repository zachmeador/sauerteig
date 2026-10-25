//! Render-only history. Simulation and network state remain authoritative.
use glam::Vec3;

pub(crate) struct Motion {
    previous: Vec3,
    current: Vec3,
    previous_yaw: f32,
    yaw: f32,
}
fn angle_delta(from: f32, to: f32) -> f32 {
    (to - from + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}
impl Motion {
    pub fn new(position: Vec3, yaw: f32) -> Self {
        Self {
            previous: position,
            current: position,
            previous_yaw: yaw,
            yaw,
        }
    }
    /// Move both endpoints with a prediction correction. Large corrections
    /// (respawn/teleport) reset history instead of drawing a flight across the map.
    pub fn correct(&mut self, position: Vec3) {
        let delta = position - self.current;
        if delta.length_squared() >= 4.0 {
            self.previous = position;
        } else {
            self.previous += delta;
        }
        self.current = position;
    }
    pub fn tick(&mut self, position: Vec3, yaw: f32) {
        self.previous = self.current;
        self.previous_yaw = self.yaw;
        self.current = position;
        self.yaw += angle_delta(self.yaw, yaw);
    }
    pub fn remote_tick(&mut self, position: Vec3, yaw: f32, dt: f32) {
        if self.current.distance_squared(position) > 25.0 {
            *self = Self::new(position, yaw);
        } else {
            let blend = 1.0 - (-dt * 18.0).exp();
            self.tick(
                self.current.lerp(position, blend),
                self.yaw + angle_delta(self.yaw, yaw) * blend,
            );
        }
    }
    pub fn position(&self, alpha: f32) -> Vec3 {
        self.previous.lerp(self.current, alpha.clamp(0.0, 1.0))
    }
    pub fn yaw(&self, alpha: f32) -> f32 {
        self.previous_yaw + (self.yaw - self.previous_yaw) * alpha.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sixty_hz_steps_render_at_uniform_144_hz_intervals() {
        let mut motion = Motion::new(Vec3::ZERO, 0.0);
        let mut accumulator = 0.0;
        let mut position = Vec3::ZERO;
        let mut last: Option<f32> = None;
        for frame in 0..288 {
            accumulator += 1.0 / 144.0;
            while accumulator >= 1.0 / 60.0 {
                accumulator -= 1.0 / 60.0;
                position.x += 6.0 / 60.0;
                motion.tick(position, 0.0);
            }
            let rendered = motion.position(accumulator * 60.0).x;
            if frame > 3 {
                assert!((rendered - last.unwrap() - 6.0 / 144.0).abs() < 1e-5);
            }
            last = Some(rendered);
        }
    }
    #[test]
    fn corrections_translate_history_and_teleports_reset_it() {
        let mut motion = Motion::new(Vec3::ZERO, 0.0);
        motion.tick(Vec3::X, 0.0);
        motion.correct(Vec3::X * 1.25);
        assert_eq!(motion.position(0.5), Vec3::X * 0.75);
        motion.correct(Vec3::X * 20.0);
        assert_eq!(motion.position(0.0), Vec3::X * 20.0);
        motion.remote_tick(Vec3::X * 40.0, 1.0, 1.0 / 60.0);
        assert_eq!(motion.position(0.5), Vec3::X * 40.0);
    }
    #[test]
    fn remote_turns_take_the_short_path_across_pi() {
        let mut motion = Motion::new(Vec3::ZERO, 3.13);
        motion.tick(Vec3::X, -3.13);
        assert!((motion.yaw(0.5) - std::f32::consts::PI).abs() < 0.001);
    }
}
