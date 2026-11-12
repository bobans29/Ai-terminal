//! aiterm — a cross-platform GPU terminal with tabs.
//!
//! Each tab owns a terminal grid and PTY session. Tabs share one window,
//! renderer device, and glyph atlas. The modules below divide those concerns:
//!
//! * [`terminal`] wraps each tab's VT grid.
//! * [`pty`] owns the PTY and the child. A headless/daemon mode can call it
//!   without the window.
//! * [`renderer`] turns the active grid into a wgpu frame using the shared
//!   device and glyph atlas.
//! * [`app`] is the winit shell and owns tab selection and window input.

mod app;
mod color;
mod config;
mod font;
mod input;
mod pty;
mod renderer;
mod tab_bar;
mod terminal;

pub use config::Config;

/// Open the window and run until it closes.
pub fn run(config_path: Option<std::path::PathBuf>) -> Result<(), String> {
    let config = Config::load(config_path)?;
    app::run(config)
}
