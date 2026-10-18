//! Sprite drawing helpers.
//!
//! The prototype has no art pipeline yet, so players are drawn as *procedural*
//! pixelart: a body, a head and eyes derived from the player's state. When real
//! sprite sheets arrive, only the functions in this file change – call sites in
//! `render::scene` stay the same.

use raylib::prelude::*;

use crate::core::{PlayerId, Vec2};
use crate::models::visual::PlayerVisuals;
use crate::models::world::PlayerState;

/// Body size in world units (which are also "pixels" at 1x zoom).
pub const PLAYER_WIDTH: f32 = 12.0;
pub const PLAYER_HEIGHT: f32 = 16.0;

/// Deterministic colour per player id, so the same player always looks the same
/// even across reconnects.
pub fn player_color(id: &PlayerId) -> Color {
    let hash = id.as_str().bytes().fold(0u32, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(u32::from(byte))
    });
    let hue = (hash % 360) as f32;
    let (r, g, b) = hue_to_rgb(hue / 360.0);
    Color::new(r, g, b, 255)
}

fn hue_to_rgb(h: f32) -> (u8, u8, u8) {
    let h = h.clamp(0.0, 1.0) * 6.0;
    let x = (1.0 - (h % 2.0 - 1.0).abs()).clamp(0.0, 1.0);
    let (r, g, b) = match h as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    // Slight desaturation keeps the pixelart readable against the floor.
    let to_byte = |c: f32| ((c * 0.75 + 0.15) * 255.0) as u8;
    (to_byte(r), to_byte(g), to_byte(b))
}

/// Draws a player at `screen_pos` (internal resolution pixels).
///
/// `screen_pos` is the *feet* anchor of the sprite. `visuals` supplies the
/// walk bob; it never changes the authoritative position.
pub fn draw_player(
    d: &mut impl RaylibDraw,
    screen_pos: Vec2,
    player: &PlayerState,
    visuals: Option<&PlayerVisuals>,
    elapsed: f32,
    is_local: bool,
) {
    let body = player_color(&player.id);
    let flash = visuals.is_some_and(|v| v.hit_flash > 0.0);
    // Dead players stay on the map (the server keeps them in the snapshot
    // until the respawn) but are drawn desaturated and flattened.
    let fill = if !player.is_alive {
        Color::new(70, 68, 82, 255)
    } else if flash {
        Color::WHITE
    } else {
        body
    };
    let outline = if is_local {
        Color::new(250, 240, 140, 255)
    } else {
        Color::new(20, 18, 28, 255)
    };

    // Idle bob / walk offset – purely cosmetic.
    let seed = visuals.map(|v| v.animation_seed).unwrap_or(0.0);
    let bob = ((elapsed * 6.0 + seed).sin() * 0.5 + 0.5).round() * 0.6;
    let y = screen_pos.y - bob;

    let x = screen_pos.x;
    let w = PLAYER_WIDTH;
    let h = PLAYER_HEIGHT;

    // Shadow (flattened ellipse drawn as a dark rect with transparent corners).
    d.draw_rectangle(
        (x - w * 0.5).round() as i32,
        (screen_pos.y + 1.0).round() as i32,
        w.round() as i32,
        3,
        Color::new(0, 0, 0, 60),
    );

    // Body.
    d.draw_rectangle(
        (x - w * 0.5).round() as i32,
        (y - h).round() as i32,
        w.round() as i32,
        h.round() as i32,
        fill,
    );
    // Outline (1px top + sides) for readability against light floors.
    d.draw_rectangle_lines(
        (x - w * 0.5).round() as i32,
        (y - h).round() as i32,
        w.round() as i32,
        h.round() as i32,
        outline,
    );

    // Head highlight.
    d.draw_rectangle(
        (x - w * 0.25).round() as i32,
        (y - h + 2.0).round() as i32,
        (w * 0.5).round() as i32,
        4,
        Color::new(255, 230, 200, 255),
    );

    // Eyes indicate facing direction (server-sent `facing`).
    let eye_x = x + player.facing.x.signum() * (w * 0.18);
    let eye_y = y - h + 4.0;
    d.draw_rectangle(
        eye_x.round() as i32,
        eye_y.round() as i32,
        1,
        2,
        Color::BLACK,
    );

    // Health bar above the head for non-local players; HUD shows our own.
    if !is_local {
        let bar_width = 14.0;
        let fraction = player.hp_fraction();
        let bar_x = (x - bar_width * 0.5).round();
        let bar_y = (y - h - 6.0).round();
        d.draw_rectangle(
            bar_x as i32,
            bar_y as i32,
            bar_width as i32,
            2,
            Color::new(30, 30, 40, 200),
        );
        let hp_color = if fraction > 0.5 {
            Color::new(90, 200, 90, 255)
        } else if fraction > 0.25 {
            Color::new(230, 190, 70, 255)
        } else {
            Color::new(220, 70, 70, 255)
        };
        d.draw_rectangle(
            bar_x as i32,
            bar_y as i32,
            (bar_width * fraction).round() as i32,
            2,
            hp_color,
        );
    }
}

/// Draws floating damage numbers from confirmed hit events.
pub fn draw_damage_popups(d: &mut impl RaylibDraw, screen_pos: Vec2, visuals: &PlayerVisuals) {
    for popup in &visuals.damage_popups {
        let progress = popup.progress();
        let alpha = ((1.0 - progress) * 255.0) as u8;
        let offset = Vec2::new((progress * 6.0).sin() * 2.0, -(progress * 14.0));
        let text = format!("-{}", popup.amount);
        d.draw_text(
            &text,
            (screen_pos.x + offset.x).round() as i32,
            (screen_pos.y - PLAYER_HEIGHT - 8.0 + offset.y).round() as i32,
            8,
            Color::new(255, 90, 90, alpha),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_are_deterministic_and_vary_by_id() {
        let a = player_color(&PlayerId::from("alpha"));
        let b = player_color(&PlayerId::from("alpha"));
        let c = player_color(&PlayerId::from("beta"));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
