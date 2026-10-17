//! Minimal math types used across the whole client.
//!
//! We deliberately do **not** depend on a full math library: the game is 2D,
//! the networking layer needs `serde` and raylib has its own `Vector2` type.
//! Keeping a tiny `Vec2` here means protocol/models/rendering all agree on one
//! representation and we convert exactly once at the raylib boundary.

use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

use serde::{Deserialize, Serialize};

/// A 2D vector in world units.
///
/// World units are *not* pixels: rendering multiplies them by the camera zoom.
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };
    pub const ONE: Self = Self { x: 1.0, y: 1.0 };
    pub const UP: Self = Self { x: 0.0, y: -1.0 };
    pub const DOWN: Self = Self { x: 0.0, y: 1.0 };
    pub const LEFT: Self = Self { x: -1.0, y: 0.0 };
    pub const RIGHT: Self = Self { x: 1.0, y: 0.0 };

    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn length_squared(self) -> f32 {
        self.x * self.x + self.y * self.y
    }

    pub fn length(self) -> f32 {
        self.length_squared().sqrt()
    }

    /// Returns a vector with the same direction but length `1.0`.
    ///
    /// A zero vector stays zero (no NaN) – important because a player that
    /// stands still sends a zero movement vector every tick.
    pub fn normalized(self) -> Self {
        let len = self.length();
        if len > f32::EPSILON {
            self / len
        } else {
            Self::ZERO
        }
    }

    /// Clamps the length to at most `max`, preserving direction.
    pub fn clamped_length(self, max: f32) -> Self {
        let len_sq = self.length_squared();
        if len_sq > max * max && len_sq > 0.0 {
            self.normalized() * max
        } else {
            self
        }
    }

    pub fn lerp(self, to: Self, t: f32) -> Self {
        self + (to - self) * t
    }

    pub fn distance(self, other: Self) -> f32 {
        (self - other).length()
    }

    pub fn dot(self, other: Self) -> f32 {
        self.x * other.x + self.y * other.y
    }

    pub fn is_zero(self) -> bool {
        self.x == 0.0 && self.y == 0.0
    }

    /// Rounds to whole tiles – useful for snapping to a pixelart grid.
    pub fn snapped(self) -> Self {
        Self::new(self.x.round(), self.y.round())
    }

    /// Rounds toward zero – matches the protocol's `{x,y}` integer convention
    /// only after [`Vec2::snapped`]; movement sent to the server is normalized.
    pub fn signum_components(self) -> Self {
        Self::new(self.x.signum(), self.y.signum())
    }
}

impl Add for Vec2 {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl AddAssign for Vec2 {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl Sub for Vec2 {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl SubAssign for Vec2 {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

impl Mul<f32> for Vec2 {
    type Output = Self;
    fn mul(self, rhs: f32) -> Self {
        Self::new(self.x * rhs, self.y * rhs)
    }
}

impl Div<f32> for Vec2 {
    type Output = Self;
    fn div(self, rhs: f32) -> Self {
        Self::new(self.x / rhs, self.y / rhs)
    }
}

impl Neg for Vec2 {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y)
    }
}

impl From<(f32, f32)> for Vec2 {
    fn from((x, y): (f32, f32)) -> Self {
        Self::new(x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_vector_does_not_produce_nan() {
        let n = Vec2::ZERO.normalized();
        assert_eq!(n, Vec2::ZERO);
        assert!(!n.x.is_nan());
    }

    #[test]
    fn diagonal_input_is_normalized() {
        let n = Vec2::new(1.0, 1.0).normalized();
        assert!((n.length() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn clamping_preserves_direction() {
        let v = Vec2::new(10.0, 0.0).clamped_length(2.0);
        assert_eq!(v, Vec2::new(2.0, 0.0));
        let v = Vec2::new(1.0, 0.0).clamped_length(2.0);
        assert_eq!(v, Vec2::new(1.0, 0.0));
    }
}
