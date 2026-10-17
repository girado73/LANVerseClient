//! Camera logic, independent of raylib.
//!
//! The camera decides *what part of the world* is visible; the renderer turns
//! that into raylib draw calls. Keeping the math here means it is unit-testable
//! and reusable if we ever switch render backends.

use crate::core::Vec2;

#[derive(Debug, Clone)]
pub struct Camera {
    /// World-space position of the camera center.
    pub center: Vec2,
    pub zoom: f32,
    /// Viewport size in world units (internal resolution / zoom).
    pub viewport: Vec2,
    /// Half-size of the "dead zone" around the center; the camera does not move
    /// while the target stays inside it. Keeps small movement from wobbling.
    pub dead_zone: Vec2,
    /// Exponential smoothing factor per second (1.0 = instant snap).
    pub smoothing: f32,
    /// Current shake offset, applied on top of `center` for rendering.
    pub shake: Vec2,
    pub shake_magnitude: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            center: Vec2::ZERO,
            zoom: 1.0,
            viewport: Vec2::new(480.0, 270.0),
            dead_zone: Vec2::new(8.0, 8.0),
            smoothing: 12.0,
            shake: Vec2::ZERO,
            shake_magnitude: 0.0,
        }
    }
}

impl Camera {
    /// Recomputes [`Camera::viewport`] from the internal resolution and zoom.
    pub fn set_viewport(&mut self, internal_width: f32, internal_height: f32, zoom: f32) {
        self.zoom = zoom.max(0.01);
        self.viewport = Vec2::new(internal_width / self.zoom, internal_height / self.zoom);
    }

    /// Smoothly follows `target` (our player) and applies a shake magnitude.
    pub fn follow(&mut self, target: Vec2, frame_time: f32, shake_magnitude: f32) {
        let delta = target - self.center;
        let inside_dead_zone =
            delta.x.abs() <= self.dead_zone.x && delta.y.abs() <= self.dead_zone.y;

        if !inside_dead_zone {
            // Move only far enough to bring the target back onto the dead zone edge.
            let wanted = self.center
                + Vec2::new(
                    (delta.x.abs() - self.dead_zone.x).max(0.0) * delta.x.signum(),
                    (delta.y.abs() - self.dead_zone.y).max(0.0) * delta.y.signum(),
                );
            self.center = self
                .center
                .lerp(wanted, smoothing_alpha(self.smoothing, frame_time));
        }

        self.update_shake(frame_time, shake_magnitude);
    }

    /// Teleports the camera (on respawn / round start) without smoothing.
    pub fn snap_to(&mut self, target: Vec2) {
        self.center = target;
    }

    fn update_shake(&mut self, frame_time: f32, shake_magnitude: f32) {
        self.shake_magnitude = shake_magnitude.max(0.0);
        if self.shake_magnitude <= 0.0 {
            self.shake = Vec2::ZERO;
            return;
        }
        // Deterministic pseudo-random jitter driven by elapsed time would need
        // a clock; a sine/cosine pair at incommensurate frequencies is enough.
        let _ = frame_time;
        let t = self.center.x * 0.7 + self.center.y * 1.3;
        self.shake = Vec2::new(
            (t * 37.0).sin() * self.shake_magnitude,
            (t * 53.0).cos() * self.shake_magnitude,
        );
    }

    /// Final camera center including shake.
    pub fn render_center(&self) -> Vec2 {
        self.center + self.shake
    }

    /// Top-left corner of the visible area in world units.
    pub fn top_left(&self) -> Vec2 {
        self.render_center() - self.viewport * 0.5
    }

    /// Converts a world position into internal-resolution screen pixels.
    pub fn world_to_screen(&self, world: Vec2) -> Vec2 {
        (world - self.top_left()) * self.zoom
    }

    /// Converts internal screen pixels back into world units.
    pub fn screen_to_world(&self, screen: Vec2) -> Vec2 {
        screen / self.zoom + self.top_left()
    }

    pub fn is_visible(&self, world: Vec2, margin: f32) -> bool {
        let screen = self.world_to_screen(world);
        screen.x >= -margin
            && screen.y >= -margin
            && screen.x <= self.viewport.x * self.zoom + margin
            && screen.y <= self.viewport.y * self.zoom + margin
    }
}

/// Framerate-independent exponential smoothing.
fn smoothing_alpha(smoothing: f32, frame_time: f32) -> f32 {
    (1.0 - (-smoothing * frame_time).exp()).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_snaps_and_converts_coordinates() {
        let mut camera = Camera::default();
        camera.set_viewport(480.0, 270.0, 1.0);
        camera.snap_to(Vec2::new(100.0, 50.0));

        assert_eq!(camera.top_left(), Vec2::new(-140.0, -85.0));
        assert_eq!(
            camera.world_to_screen(Vec2::new(100.0, 50.0)),
            Vec2::new(240.0, 135.0)
        );
        assert_eq!(
            camera.screen_to_world(Vec2::new(240.0, 135.0)),
            Vec2::new(100.0, 50.0)
        );
    }

    #[test]
    fn dead_zone_prevents_micro_movement() {
        let mut camera = Camera::default();
        camera.set_viewport(480.0, 270.0, 1.0);
        camera.snap_to(Vec2::ZERO);
        camera.follow(Vec2::new(4.0, 4.0), 0.016, 0.0);
        assert_eq!(camera.center, Vec2::ZERO);
    }

    #[test]
    fn zoom_scales_viewport() {
        let mut camera = Camera::default();
        camera.set_viewport(480.0, 270.0, 2.0);
        assert_eq!(camera.viewport, Vec2::new(240.0, 135.0));
    }
}
