//! Heads-up display: connection state, phase, scores, debug overlay.
//!
//! The HUD reads [`GameState`] only and draws in internal-resolution pixels so
//! it stays pixelated like the world. It never feeds back into gameplay, except
//! via input through the app (e.g. toggling the debug overlay).

use raylib::prelude::*;

use crate::config::RenderConfig;
use crate::core::PlayerId;
use crate::models::GameState;
use crate::net::client::{describe_status, phase_label};
use crate::net::connection::ConnectionStatus;

const PADDING: i32 = 4;
const FONT_SIZE: i32 = 8;

pub struct HudRenderer;

impl HudRenderer {
    pub fn draw(
        d: &mut impl RaylibDraw,
        state: &GameState,
        config: &RenderConfig,
        server: &str,
        pending_inputs: usize,
        fps: u32,
    ) {
        // --- top-left: connection + phase --------------------------------
        let status_text = format!("● {}", describe_status(state.connection));
        let status_color = match state.connection {
            crate::net::connection::ConnectionStatus::Connected => Color::new(120, 220, 140, 255),
            crate::net::connection::ConnectionStatus::Failed => Color::new(230, 90, 90, 255),
            _ => Color::new(230, 200, 90, 255),
        };
        d.draw_text(&status_text, PADDING, PADDING, FONT_SIZE, status_color);
        // The phase line is only meaningful once the server has told us one.
        let phase_text = if state.connection == ConnectionStatus::Connected {
            phase_label(state.phase)
        } else {
            "Waiting for the game server"
        };
        d.draw_text(
            phase_text,
            PADDING,
            PADDING + FONT_SIZE + 2,
            FONT_SIZE,
            Color::new(220, 220, 230, 255),
        );

        // --- center banner while not in a match --------------------------
        if !state.connection.can_send_gameplay() {
            Self::draw_connection_banner(d, config, server, state);
        }

        // --- top-right: scoreboard ---------------------------------------
        let mut y = PADDING;
        let mut players: Vec<_> = state.players_sorted();
        players.sort_by_key(|p| std::cmp::Reverse(p.score));
        for player in players.iter().take(8) {
            let is_local = state.is_local(&player.id);
            let text = format!(
                "{}{} {}",
                if is_local { ">" } else { " " },
                player.name,
                player.score
            );
            let color = if is_local {
                Color::new(250, 240, 140, 255)
            } else {
                Color::new(210, 210, 220, 255)
            };
            let width = measure_text(&text, FONT_SIZE);
            d.draw_text(
                &text,
                config.internal_width - width - PADDING,
                y,
                FONT_SIZE,
                color,
            );
            y += FONT_SIZE + 2;
        }

        // --- bottom-left: own health bar ---------------------------------
        if let Some(local) = state.local_player().filter(|p| p.is_alive) {
            let bar_width = 80;
            let bar_x = PADDING;
            let bar_y = config.internal_height - PADDING - 6;
            d.draw_rectangle(bar_x, bar_y, bar_width, 6, Color::new(20, 20, 28, 220));
            let fraction = local.hp_fraction();
            d.draw_rectangle(
                bar_x,
                bar_y,
                (bar_width as f32 * fraction).round() as i32,
                6,
                Color::new(200, 60, 70, 255),
            );
            d.draw_rectangle_lines(bar_x, bar_y, bar_width, 6, Color::new(240, 240, 240, 255));
            d.draw_text(
                &format!("{}/{}", local.hp, local.max_hp),
                bar_x + bar_width + 4,
                bar_y - 1,
                FONT_SIZE,
                Color::new(240, 240, 240, 255),
            );
        }

        // --- bottom-right: round timer -----------------------------------
        if state.round.time_left_ms > 0 {
            let secs = state.round.time_left_ms.div_ceil(1000);
            let text = format!("{}:{:02}", secs / 60, secs % 60);
            let width = measure_text(&text, FONT_SIZE * 2);
            d.draw_text(
                &text,
                config.internal_width - width - PADDING,
                config.internal_height - PADDING - FONT_SIZE * 2,
                FONT_SIZE * 2,
                Color::new(240, 240, 240, 255),
            );
        }

        // --- last error ---------------------------------------------------
        if let Some(error) = &state.last_error {
            d.draw_text(
                &truncate(error, 70),
                PADDING,
                config.internal_height - PADDING - FONT_SIZE * 3,
                FONT_SIZE,
                Color::new(235, 120, 120, 255),
            );
        }

        // --- debug overlay ------------------------------------------------
        if config.show_debug_overlay {
            Self::draw_debug(d, state, config, pending_inputs, fps);
        }
    }

    /// Shown until the handshake completed: the server endpoint and the last
    /// error, so a client started before the server explains itself instead of
    /// rendering an empty world.
    fn draw_connection_banner(
        d: &mut impl RaylibDraw,
        config: &RenderConfig,
        server: &str,
        state: &GameState,
    ) {
        let status = describe_status(state.connection);
        let lines = [
            ("LAN Bash", Color::new(250, 240, 140, 255)),
            (status.as_str(), Color::WHITE),
            (server, Color::new(170, 220, 255, 255)),
            (
                "Start the server with: php artisan game:serve --port=8080",
                Color::new(190, 190, 200, 255),
            ),
        ];
        let line_height = FONT_SIZE + 3;
        let y0 = config.internal_height / 2 - (lines.len() as i32 * line_height) / 2;
        for (index, (text, color)) in lines.iter().enumerate() {
            let width = measure_text(text, FONT_SIZE);
            d.draw_text(
                text,
                (config.internal_width - width) / 2,
                y0 + index as i32 * line_height,
                FONT_SIZE,
                *color,
            );
        }
        if let Some(error) = &state.last_error {
            let text = truncate(error, 72);
            let width = measure_text(&text, FONT_SIZE);
            d.draw_text(
                &text,
                (config.internal_width - width) / 2,
                y0 + lines.len() as i32 * line_height + 4,
                FONT_SIZE,
                Color::new(235, 120, 120, 255),
            );
        }
    }

    fn draw_debug(
        d: &mut impl RaylibDraw,
        state: &GameState,
        config: &RenderConfig,
        pending_inputs: usize,
        fps: u32,
    ) {
        let lines = [
            format!("fps {fps}"),
            format!("tick {} @ {} Hz", state.server_tick.0, state.tick_rate),
            format!("rtt {:.1} ms", state.latency_ms),
            format!(
                "pending inputs {pending_inputs} (ack {:?}, overflow {})",
                state.acked_input_sequence, state.buffer_overflows
            ),
            format!("world {:.0}x{:.0}", state.world.width, state.world.height),
            format!("players {}", state.players.len()),
            format!(
                "local {:?}",
                state.local_player.as_ref().map(PlayerId::as_str)
            ),
        ];
        let y0 = config.internal_height - PADDING - (lines.len() as i32) * (FONT_SIZE + 1);
        // Offset vertically to the middle-right so it does not overlap the HUD.
        let x = config.internal_width / 2 - 40;
        for (index, line) in lines.iter().enumerate() {
            d.draw_text(
                line,
                x,
                y0 + index as i32 * (FONT_SIZE + 1),
                FONT_SIZE,
                Color::new(170, 220, 255, 200),
            );
        }
    }
}

/// Width of `text` in pixels using raylib's default font.
///
/// The default font is a bitmap font, so the width is font-size independent
/// per character; `MeasureText` (C API) is not exposed on draw handles, hence
/// this approximation with a 0.6 glyph ratio and explicit line-break handling.
fn measure_text(text: &str, font_size: i32) -> i32 {
    (text.chars().count() as f32 * font_size as f32 * 0.6).round() as i32
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    text.chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>()
        + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_char_boundaries() {
        let text = "ä".repeat(30);
        let cut = truncate(&text, 10);
        assert_eq!(cut.chars().count(), 10);
        assert!(cut.ends_with('…'));
    }
}
