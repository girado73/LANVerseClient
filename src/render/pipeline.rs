//! Render pipeline: pixelart-faithful output.
//!
//! All gameplay rendering happens into a render texture at the *internal*
//! resolution (e.g. 480×270). That texture is then scaled up to the window
//! with nearest-neighbour filtering, so every internal pixel becomes a crisp
//! square block regardless of window size or monitor DPI.
//!
//! ```text
//! window (1440×810)             internal texture (480×270)
//! ┌───────────────────────┐     ┌──────────────┐
//! │  nearest-neighbour    │ ◄── │  world + HUD │
//! │  upscale of 3×3 px    │     │  at 1×       │
//! └───────────────────────┘     └──────────────┘
//! ```
//!
//! Resizing the window changes only the upscale factor. The game never knows
//! how large the window is (except the HUD, which uses the internal size).

use raylib::core::texture::{RaylibRenderTexture2D, RaylibTexture2D};
use raylib::prelude::*;

use crate::config::RenderConfig;

pub struct RenderPipeline {
    internal: RenderTexture2D,
    width: i32,
    height: i32,
    filter: TextureFilter,
}

impl RenderPipeline {
    pub fn new(
        rl: &mut RaylibHandle,
        thread: &RaylibThread,
        config: &RenderConfig,
    ) -> anyhow::Result<Self> {
        let width = config.internal_width.max(16);
        let height = config.internal_height.max(16);
        let internal = rl
            .load_render_texture(thread, width as u32, height as u32)
            .map_err(|err| anyhow::anyhow!("failed to create render texture: {err}"))?;
        // POINT filtering keeps pixels crisp when the window is upscaled.
        internal
            .texture()
            .set_texture_filter(thread, TextureFilter::TEXTURE_FILTER_POINT);
        Ok(Self {
            internal,
            width,
            height,
            filter: TextureFilter::TEXTURE_FILTER_POINT,
        })
    }

    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    pub fn filter(&self) -> TextureFilter {
        self.filter
    }

    /// Mutable access to the target texture, for drivers that render manually
    /// with an explicit `begin_texture_mode` guard.
    pub fn internal_mut(&mut self) -> &mut RenderTexture2D {
        &mut self.internal
    }

    /// Draws the world into the internal-resolution texture.
    ///
    /// The closure receives a draw handle limited to the internal size, so
    /// rendering code can only use internal coordinates.
    pub fn draw_internal<F>(&mut self, rl: &mut RaylibHandle, thread: &RaylibThread, draw: F)
    where
        F: FnOnce(&mut RaylibTextureMode<'_, '_, RaylibHandle>),
    {
        // Render textures are vertically flipped in OpenGL; the upscale below
        // compensates with a negative source height.
        let internal = &mut self.internal;
        let mut mode = rl.begin_texture_mode(thread, internal);
        draw(&mut mode);
    }

    /// Mean luminance of the last rendered frame (0..255).
    ///
    /// Reads the internal texture back from the GPU. Diagnostic only: it is
    /// slow and must not be used in the hot path. Used to tell "the content
    /// really changes" apart from "the window presents wrongly", which look
    /// identical to a player describing a flicker.
    pub fn mean_luma(&self) -> Option<f32> {
        // SAFETY: read-only GPU readback of a texture this struct owns; the
        // image is unloaded before returning and the slice never outlives it.
        unsafe {
            let texture = self.internal.texture();
            let image = raylib::ffi::LoadImageFromTexture(*texture.as_ref());
            if image.data.is_null() || image.width <= 0 || image.height <= 0 {
                return None;
            }
            let pixels = (image.width * image.height) as usize;
            let bytes = std::slice::from_raw_parts(image.data as *const u8, pixels * 4);
            let sum: u64 = bytes
                .chunks_exact(4)
                .map(|p| (u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])) as u64 / 3)
                .sum();
            raylib::ffi::UnloadImage(image);
            Some(sum as f32 / pixels as f32)
        }
    }

    /// Blits the internal texture onto the window, letterboxed and integer-scaled.
    pub fn present(&self, d: &mut impl RaylibDraw, window_width: i32, window_height: i32) {
        let scale = (window_width as f32 / self.width as f32)
            .min(window_height as f32 / self.height as f32);
        // Integer scaling gives perfectly square pixels in windowed mode.
        let scale = scale.max(1.0);
        let draw_width = (self.width as f32 * scale).round();
        let draw_height = (self.height as f32 * scale).round();
        let offset_x = ((window_width as f32 - draw_width) * 0.5).round();
        let offset_y = ((window_height as f32 - draw_height) * 0.5).round();

        let source = Rectangle::new(0.0, 0.0, self.width as f32, -self.height as f32);
        let dest = Rectangle::new(offset_x, offset_y, draw_width, draw_height);
        d.draw_texture_pro(
            self.internal.texture(),
            source,
            dest,
            Vector2::new(0.0, 0.0),
            0.0,
            Color::WHITE,
        );
    }

    /// Screen rectangle occupied by the game view (for mouse mapping).
    pub fn view_rect(&self, window_width: i32, window_height: i32) -> Rectangle {
        let scale = (window_width as f32 / self.width as f32)
            .min(window_height as f32 / self.height as f32)
            .max(1.0);
        let draw_width = (self.width as f32 * scale).round();
        let draw_height = (self.height as f32 * scale).round();
        Rectangle::new(
            ((window_width as f32 - draw_width) * 0.5).round(),
            ((window_height as f32 - draw_height) * 0.5).round(),
            draw_width,
            draw_height,
        )
    }
}
