//! aiterm — a cross-platform GPU terminal with tabs and split panes.
//!
//! Each tab contains a nested pane layout; each pane owns a terminal grid and
//! PTY session. Panes share one window, renderer device, and glyph atlas. The
//! modules below divide those concerns:
//!
//! * [`terminal`] wraps each pane's VT grid.
//! * [`pty`] owns the PTY and the child. A headless/daemon mode can call it
//!   without the window.
//! * [`renderer`] turns the visible pane grids into a wgpu frame using the shared
//!   device and glyph atlas.
//! * [`app`] is the winit shell and owns tab and pane layouts, focus, and input.

mod app;
mod color;
mod config;
mod font;
mod input;
mod layout;
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
