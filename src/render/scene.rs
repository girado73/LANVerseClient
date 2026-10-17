//! World rendering.
//!
//! Reads [`GameState`] (authoritative) plus [`Camera`] and the interpolation
//! buffer, and issues draw calls. **No state is mutated here.**

use raylib::prelude::*;

use crate::camera::Camera;
use crate::core::{PlayerId, Vec2};
use crate::interpolation::{InterpolatedPose, Interpolator};
use crate::models::GameState;
use crate::render::sprites;
use crate::simulation::tick_rate::TickRate;

/// Grid cell size in world units. Purely decorative until the server sends a map.
const GRID_SIZE: f32 = 32.0;

pub struct SceneRenderer;

impl SceneRenderer {
    /// Draws the world into the internal-resolution frame.
    ///
    /// The renderer applies the camera by translating all draws; because we
    /// render into a fixed-size texture we do not need a raylib `Camera2D`.
    pub fn draw(
        d: &mut impl RaylibDraw,
        state: &GameState,
        camera: &Camera,
        interpolator: &mut Interpolator,
        render_tick: f32,
    ) {
        let top_left = camera.top_left();

        // --- floor -------------------------------------------------------
        d.clear_background(Color::new(26, 24, 34, 255));
        draw_grid(d, top_left, camera);

        // --- players -----------------------------------------------------
        // Draw remote players first, then ourselves on top so we are never hidden.
        let local_id = state.local_player.clone();
        let players = state.players_sorted();

        for player in players.iter().filter(|p| Some(&p.id) != local_id.as_ref()) {
            draw_one(d, state, camera, interpolator, render_tick, player);
        }
        for player in players.iter().filter(|p| Some(&p.id) == local_id.as_ref()) {
            draw_one(d, state, camera, interpolator, render_tick, player);
        }

        // --- border of the playfield (until the server owns maps) --------
        let field_min = camera.world_to_screen(Vec2::new(0.0, 0.0));
        let field_max = camera.world_to_screen(Vec2::new(480.0, 270.0));
        d.draw_rectangle_lines(
            field_min.x.round() as i32,
            field_min.y.round() as i32,
            (field_max.x - field_min.x).round() as i32,
            (field_max.y - field_min.y).round() as i32,
            Color::new(90, 200, 160, 255),
        );
    }

    /// Screen position (feet anchor) of a player for this frame.
    ///
    /// * local player → prediction (already injected via `state.local_override`)
    /// * remote players → interpolation between the two surrounding snapshots
    /// * fallback → last authoritative position
    pub fn player_screen_position(
        state: &GameState,
        camera: &Camera,
        interpolator: &mut Interpolator,
        render_tick: f32,
        player_id: &PlayerId,
    ) -> Option<Vec2> {
        if state.is_local(player_id)
            && let Some(position) = state.local_override
        {
            return Some(camera.world_to_screen(position));
        }

        let world = interpolator
            .pose(player_id, render_tick)
            .map(|pose: InterpolatedPose| pose.position)
            .or_else(|| state.player(player_id).map(|p| p.position))?;
        Some(camera.world_to_screen(world))
    }
}

fn draw_one(
    d: &mut impl RaylibDraw,
    state: &GameState,
    camera: &Camera,
    interpolator: &mut Interpolator,
    render_tick: f32,
    player: &crate::models::world::PlayerState,
) {
    let Some(screen) =
        SceneRenderer::player_screen_position(state, camera, interpolator, render_tick, &player.id)
    else {
        return;
    };
    // Cull players outside the viewport (common in big LAN matches).
    if !camera.is_visible(camera.screen_to_world(screen), sprites::PLAYER_HEIGHT * 2.0) {
        return;
    }

    let visuals = state.visuals.player(&player.id);
    let is_local = state.is_local(&player.id);
    sprites::draw_player(
        d,
        screen,
        player,
        visuals,
        state.visuals.clock.elapsed,
        is_local,
    );

    if let Some(visuals) = visuals {
        sprites::draw_damage_popups(d, screen, visuals);
    }

    // Name tag (skip while the HUD shows our own name).
    if !is_local && !player.name.is_empty() {
        let width = player.name.chars().count() as f32 * 8.0 * 0.6;
        d.draw_text(
            &player.name,
            (screen.x - width * 0.5).round() as i32,
            (screen.y - sprites::PLAYER_HEIGHT - 16.0).round() as i32,
            8,
            Color::new(230, 230, 240, 220),
        );
    }
}

fn draw_grid(d: &mut impl RaylibDraw, top_left: Vec2, camera: &Camera) {
    let cell = GRID_SIZE * camera.zoom;
    let color = Color::new(38, 36, 50, 255);
    let width = camera.viewport.x * camera.zoom;
    let height = camera.viewport.y * camera.zoom;

    // Grid lines are aligned to world space so they scroll with the camera.
    let start_x = (-top_left.x * camera.zoom).rem_euclid(cell);
    let start_y = (-top_left.y * camera.zoom).rem_euclid(cell);

    let mut x = start_x;
    while x <= width {
        d.draw_rectangle(x.round() as i32, 0, 1, height.round() as i32, color);
        x += cell;
    }
    let mut y = start_y;
    while y <= height {
        d.draw_rectangle(0, y.round() as i32, width.round() as i32, 1, color);
        y += cell;
    }
}

/// Renders at `states.players` positions without interpolation (used by the
/// minimap and tests).
pub fn world_positions(state: &GameState) -> Vec<(PlayerId, Vec2)> {
    state
        .players_sorted()
        .into_iter()
        .map(|p| (p.id.clone(), p.position))
        .collect()
}

/// Render tick helper so app and renderer agree on the formula.
pub fn compute_render_tick(
    current_tick: u64,
    latency_ms: f32,
    tick_rate: TickRate,
    interpolator: &Interpolator,
) -> f32 {
    interpolator.render_tick(current_tick, latency_ms, tick_rate)
}
