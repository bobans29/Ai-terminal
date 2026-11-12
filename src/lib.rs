//! aiterm — Milestone 1 of a cross-platform GPU terminal.
//!
//! The process is one window and one shell. The modules below are the seams
//! later milestones hang off:
//!
//! * [`terminal`] wraps the VT grid (one instance per pane, once tabs exist).
//! * [`pty`] owns the PTY and the child. A headless/daemon mode can call it
//!   without the window.
//! * [`renderer`] turns a grid into a wgpu frame. Tabs would share the device
//!   and the glyph atlas, not the grid.
//! * [`app`] is the winit shell. The sidebar, tab strip, and agent status
//!   belong here later, not inside the grid or the PTY.

mod app;
mod color;
mod config;
mod font;
mod input;
mod pty;
mod renderer;
mod terminal;

pub use config::Config;

/// Open the window and run until it closes.
pub fn run(config_path: Option<std::path::PathBuf>) -> Result<(), String> {
    let config = Config::load(config_path)?;
    app::run(config)
}
