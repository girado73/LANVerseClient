//! Rendering, split by concern.
//!
//! ```text
//! platform::desktop  ──owns──► raylib window + render texture
//!         │                         │
//!         │                    render::pipeline draws into the texture at
//!         │                    internal resolution (crisp pixels)
//!         │                         │
//!         │                    render::scene / hud read only from GameState
//!         │                         │
//!         └──── scales the texture to the window (nearest neighbour)
//! ```
//!
//! Only this module (and `platform`) may import raylib. Everything else stays
//! backend-agnostic so the renderer can be replaced without touching game code.

pub mod hud;
pub mod pipeline;
pub mod scene;
pub mod sprites;

pub use pipeline::RenderPipeline;
