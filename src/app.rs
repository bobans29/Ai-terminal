//! Window loop.
//!
//! winit owns the frame. This module turns key and mouse events into PTY
//! bytes and asks the renderer to paint the panes. Tabs and splits live here; they should not leak into [`crate::terminal`] or
//! [`crate::pty`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, WindowSize};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::Side;
use alacritty_terminal::term::TermMode;
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState};
use winit::window::{Theme, Window, WindowId};

use crate::config::Config;
use crate::font::FontSet;
use crate::input::{self, KeyAction};
use crate::layout::{Axis, Layout, Rect};
use crate::pty::{PtyMsg, PtySession};
use crate::renderer::{CursorPaint, DrawResult, PanePaint, Renderer};
use crate::terminal::Terminal;

const DIVIDER: u32 = 4;

const BLINK: Duration = Duration::from_millis(530);

enum UserEvent {
    Pty(u64, Vec<u8>),
    Closed(u64),
}

struct Pane {
    id: u64,
    rect: Rect,
    terminal: Terminal,
    pty: PtySession,
    title: String,
}

struct Tab {
    panes: Vec<Pane>,
    active_pane: usize,
    layout: Layout,
}

impl Tab {
    fn title(&self) -> &str {
        &self.panes[self.active_pane].title
    }
}

#[derive(Default)]
struct Composition {
    preediting: bool,
    tab: Option<u64>,
}

impl Composition {
    fn preedit(&mut self, text: &str, active: Option<u64>) {
        if !text.is_empty() && !self.preediting {
            self.tab = active;
        }
        // Empty preedit can precede Commit, so retain the source until commit
        // or the beginning of a fresh composition after cancellation.
        self.preediting = !text.is_empty();
    }

    fn commit(&mut self, active: Option<u64>) -> bool {
        self.preediting = false;
        let owner = self.tab.take();
        owner.is_none() || owner == active
    }
}

struct App {
    config: Config,
    proxy: EventLoopProxy<UserEvent>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    tabs: Vec<Tab>,
    active: usize,
    next_pane_id: u64,
    modifiers: ModifiersState,
    pointer: PhysicalPosition<f64>,
    selecting: bool,
    divider_drag: Option<Vec<bool>>,
    /// Bits 0..2 are left, middle, and right while held.
    mouse_buttons: u8,
    /// Unused pixel movement from touchpad scrolling.
    pixel_scroll: f32,
    composition: Composition,
    focused: bool,
    cursor_on: bool,
    next_blink: Instant,
    close: bool,
    startup_error: Option<String>,
    /// Kept alive so X11 still owns the selection after `set_text` returns.
    /// Dropping it hands the data to a clipboard manager, which this session
    /// may not have.
    clipboard: Option<arboard::Clipboard>,
}

pub fn run(config: Config) -> Result<(), String> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|err| format!("event loop: {err}"))?;
    let proxy = event_loop.create_proxy();
    let mut app = App::new(config, proxy);
    event_loop
        .run_app(&mut app)
        .map_err(|err| format!("event loop: {err}"))?;
    match app.startup_error.take() {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

impl App {
    fn new(config: Config, proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            config,
            proxy,
            window: None,
            renderer: None,
            tabs: Vec::new(),
            active: 0,
            next_pane_id: 0,
            modifiers: ModifiersState::empty(),
            pointer: PhysicalPosition::new(0.0, 0.0),
            selecting: false,
            divider_drag: None,
            mouse_buttons: 0,
            pixel_scroll: 0.0,
            composition: Composition::default(),
            focused: true,
            cursor_on: true,
            next_blink: Instant::now() + BLINK,
            close: false,
            startup_error: None,
            clipboard: None,
        }
    }

    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("aiterm")
                        .with_theme(Some(Theme::Dark))
                        .with_inner_size(PhysicalSize::new(1040u32, 640u32)),
                )
                .map_err(|err| format!("creating window: {err}"))?,
        );
        window.set_ime_allowed(true);
        let scale = window.scale_factor();
        let px = (self.config.font_size * scale as f32).clamp(6.0, 128.0);
        let fonts = FontSet::load(&self.config.font_family, px)?;
        let renderer = Renderer::new(
            Arc::clone(&window),
            fonts,
            self.config.font_family.clone(),
            self.config.font_size,
            self.config.palette.background,
        )?;
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.new_tab()?;
        self.request_redraw();
        Ok(())
    }

    fn pane(&self) -> Option<&Pane> {
        let tab = self.tabs.get(self.active)?;
        tab.panes.get(tab.active_pane)
    }

    fn pane_mut(&mut self) -> Option<&mut Pane> {
        let tab = self.tabs.get_mut(self.active)?;
        tab.panes.get_mut(tab.active_pane)
    }

    fn active_id(&self) -> Option<u64> {
        self.pane().map(|pane| pane.id)
    }

    fn pane_location(&self, id: u64) -> Option<(usize, usize)> {
        self.tabs.iter().enumerate().find_map(|(ti, tab)| {
            tab.panes
                .iter()
                .position(|pane| pane.id == id)
                .map(|pi| (ti, pi))
        })
    }

    fn terminal(&self) -> Option<&Terminal> {
        self.pane().map(|pane| &pane.terminal)
    }

    fn terminal_mut(&mut self) -> Option<&mut Terminal> {
        self.pane_mut().map(|pane| &mut pane.terminal)
    }

    fn content_bounds(&self) -> Option<Rect> {
        let size = self.window.as_ref()?.inner_size();
        let header = self.renderer.as_ref()?.cell_size().1 as u32;
        Some(Rect {
            x: 0,
            y: header.min(size.height),
            width: size.width,
            height: size.height.saturating_sub(header),
        })
    }

    fn leaf_minimum(&self) -> (u32, u32) {
        let (cw, ch) = self
            .renderer
            .as_ref()
            .map(Renderer::cell_size)
            .unwrap_or((8.0, 16.0));
        ((cw * 4.0).ceil() as u32, (ch * 2.0).ceil() as u32)
    }

    fn spawn_pane(&mut self, rect: Rect) -> Result<Pane, String> {
        let (cw, ch) = self
            .renderer
            .as_ref()
            .ok_or("renderer not initialized")?
            .cell_size();
        let cols = grid_dim(rect.width, cw, 2);
        let rows = grid_dim(rect.height, ch, 1);
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        let proxy = self.proxy.clone();
        let pty = PtySession::spawn(
            self.config.shell.as_deref(),
            cols as u16,
            rows as u16,
            rect.width.min(u16::MAX as u32) as u16,
            rect.height.min(u16::MAX as u32) as u16,
            move |msg| {
                let event = match msg {
                    PtyMsg::Data(bytes) => UserEvent::Pty(id, bytes),
                    PtyMsg::Closed => UserEvent::Closed(id),
                };
                let _ = proxy.send_event(event);
            },
        )?;
        Ok(Pane {
            id,
            rect,
            terminal: Terminal::new(cols, rows, self.config.scrollback),
            pty,
            title: "aiterm".to_string(),
        })
    }

    fn new_tab(&mut self) -> Result<(), String> {
        let rect = self.content_bounds().ok_or("window not initialized")?;
        let pane = self.spawn_pane(rect)?;
        let id = pane.id;
        self.tabs.push(Tab {
            panes: vec![pane],
            active_pane: 0,
            layout: Layout::Leaf(id),
        });
        self.switch_tab(self.tabs.len() - 1);
        Ok(())
    }

    fn split_pane(&mut self, axis: Axis) -> Result<(), String> {
        let pane = self.pane().ok_or("no focused pane")?;
        let rect = pane.rect;
        let old_id = pane.id;
        let (min_w, min_h) = self.leaf_minimum();
        if rect.width < min_w || rect.height < min_h {
            return Err("pane too small to split".into());
        }
        let mut new_rect = rect;
        match axis {
            Axis::Vertical => {
                if rect.width < min_w * 2 + DIVIDER {
                    return Err("pane too narrow to split".into());
                }
                let first = (rect.width - DIVIDER) / 2;
                new_rect.x += first + DIVIDER;
                new_rect.width = rect.width - first - DIVIDER;
            }
            Axis::Horizontal => {
                if rect.height < min_h * 2 + DIVIDER {
                    return Err("pane too short to split".into());
                }
                let first = (rect.height - DIVIDER) / 2;
                new_rect.y += first + DIVIDER;
                new_rect.height = rect.height - first - DIVIDER;
            }
        }
        // Spawn before changing the tree so an invalid shell leaves the old
        // layout and focused session intact.
        let pane = self.spawn_pane(new_rect)?;
        let tab = &mut self.tabs[self.active];
        if !tab.layout.split(old_id, pane.id, axis) {
            return Err("focused pane missing from layout".into());
        }
        tab.panes.push(pane);
        self.sync_grid();
        self.focus_pane(self.tabs[self.active].panes.len() - 1);
        Ok(())
    }

    fn focus_pane(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        if index >= tab.panes.len() || index == tab.active_pane {
            return;
        }
        self.release_mouse();
        self.report_focus(false);
        self.tabs[self.active].active_pane = index;
        self.report_focus(self.focused);
        self.pixel_scroll = 0.0;
        self.divider_drag = None;
        self.wake_cursor();
        self.update_title();
        self.request_redraw();
    }

    fn cycle_pane(&mut self, forward: bool) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let count = tab.panes.len();
        let index = (tab.active_pane + if forward { 1 } else { count - 1 }) % count;
        self.focus_pane(index);
    }

    fn close_pane(&mut self, id: u64) {
        let Some((ti, pi)) = self.pane_location(id) else {
            return;
        };
        if self.tabs[ti].panes.len() == 1 {
            self.close_tab(ti);
            return;
        }
        let was_focused = self.tabs[ti].active_pane == pi;
        let tab = &mut self.tabs[ti];
        tab.layout.remove(id);
        tab.panes.remove(pi);
        if pi < tab.active_pane {
            tab.active_pane -= 1;
        }
        tab.active_pane = tab.active_pane.min(tab.panes.len() - 1);
        if ti == self.active {
            self.divider_drag = None;
            if was_focused {
                self.selecting = false;
                self.mouse_buttons = 0;
                self.pixel_scroll = 0.0;
                self.report_focus(self.focused);
                self.wake_cursor();
            }
        }
        self.sync_grid();
        self.update_title();
    }

    fn report_focus(&mut self, focused: bool) {
        if self
            .terminal()
            .is_some_and(|term| term.mode().contains(TermMode::FOCUS_IN_OUT))
        {
            self.write_pty(if focused { b"\x1b[I" } else { b"\x1b[O" });
        }
    }

    fn switch_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if index != self.active {
            self.release_mouse();
            self.report_focus(false);
            self.active = index;
            self.report_focus(self.focused);
        }
        self.selecting = false;
        self.divider_drag = None;
        self.mouse_buttons = 0;
        self.pixel_scroll = 0.0;
        self.wake_cursor();
        self.update_title();
        self.request_redraw();
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let was_active = index == self.active;
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.close = true;
            return;
        }
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        if was_active {
            self.report_focus(self.focused);
            self.switch_tab(self.active);
        } else {
            // Removing another tab may shift the active index, but it does
            // not change the session or interrupt its selection/mouse drag.
            self.update_title();
            self.request_redraw();
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn cursor_paint(&self) -> CursorPaint {
        let visible = self
            .terminal()
            .map(|term| term.mode().contains(TermMode::SHOW_CURSOR))
            .unwrap_or(false);
        if !visible {
            CursorPaint::Off
        } else if !self.focused {
            CursorPaint::Hollow
        } else if self.cursor_on {
            CursorPaint::Active
        } else {
            CursorPaint::Off
        }
    }

    fn should_blink(&self) -> bool {
        self.focused
            && self
                .terminal()
                .map(|term| term.mode().contains(TermMode::SHOW_CURSOR))
                .unwrap_or(false)
    }

    fn wake_cursor(&mut self) {
        self.cursor_on = true;
        self.next_blink = Instant::now() + BLINK;
    }

    fn redraw(&mut self) {
        let paint = self.cursor_paint();
        let titles: Vec<String> = self
            .tabs
            .iter()
            .map(|tab| tab.title().to_string())
            .collect();
        let retry = match (self.renderer.as_mut(), self.tabs.get(self.active)) {
            (Some(renderer), Some(tab)) => {
                let panes: Vec<PanePaint<'_>> = tab
                    .panes
                    .iter()
                    .enumerate()
                    .map(|(i, pane)| PanePaint {
                        terminal: &pane.terminal,
                        rect: pane.rect,
                        cursor: if i == tab.active_pane {
                            paint
                        } else if pane.terminal.mode().contains(TermMode::SHOW_CURSOR) {
                            CursorPaint::Hollow
                        } else {
                            CursorPaint::Off
                        },
                        focused: i == tab.active_pane,
                    })
                    .collect();
                matches!(
                    renderer.draw(&panes, &self.config.palette, &titles, self.active),
                    DrawResult::Retry
                )
            }
            _ => false,
        };
        if retry {
            self.request_redraw();
        }
    }

    fn sync_grid(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let size = window.inner_size();
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.resize(size.width, size.height);
        }
        let Some((cw, ch)) = self.renderer.as_ref().map(Renderer::cell_size) else {
            return;
        };
        if size.width == 0 || size.height == 0 {
            return;
        }
        let Some(bounds) = self.content_bounds() else {
            return;
        };
        for tab in &mut self.tabs {
            for (id, rect) in tab.layout.rects(bounds, DIVIDER) {
                let Some(pane) = tab.panes.iter_mut().find(|pane| pane.id == id) else {
                    continue;
                };
                let cols = grid_dim(rect.width, cw, 2);
                let rows = grid_dim(rect.height, ch, 1);
                let changed = pane.rect != rect
                    || pane.terminal.cols() != cols
                    || pane.terminal.rows() != rows;
                pane.rect = rect;
                pane.terminal.resize(cols, rows);
                if changed {
                    if let Err(err) = pane.pty.resize(
                        cols as u16,
                        rows as u16,
                        rect.width.min(u16::MAX as u32) as u16,
                        rect.height.min(u16::MAX as u32) as u16,
                    ) {
                        eprintln!("aiterm: {err}");
                    }
                }
            }
        }
        self.request_redraw();
    }

    fn apply_scale(&mut self, scale: f64) {
        let changed = match self.renderer.as_mut() {
            Some(renderer) => renderer.set_scale(scale),
            None => return,
        };
        match changed {
            Ok(true) => self.sync_grid(),
            Ok(false) => {}
            Err(err) => eprintln!("aiterm: {err}"),
        }
    }

    fn on_pty(&mut self, id: u64, bytes: Vec<u8>) {
        let Some((ti, pi)) = self.pane_location(id) else {
            return;
        };
        let events = self.tabs[ti].panes[pi].terminal.process(&bytes);
        self.handle_events_for(id, events);
        self.request_redraw();
    }

    fn handle_events(&mut self, events: Vec<Event>) {
        if let Some(id) = self.active_id() {
            self.handle_events_for(id, events);
        }
    }

    fn handle_events_for(&mut self, id: u64, events: Vec<Event>) {
        for event in events {
            // Process replies for the source tab, including background tabs.
            let Some((ti, pi)) = self.pane_location(id) else {
                return;
            };
            match event {
                Event::Title(title) => {
                    self.tabs[ti].panes[pi].title = title;
                    self.update_title();
                }
                Event::ResetTitle => {
                    self.tabs[ti].panes[pi].title = "aiterm".to_string();
                    self.update_title();
                }
                Event::PtyWrite(data) => self.write_pty_to(id, data.as_bytes()),
                Event::ClipboardStore(_, text) => self.clipboard_set(&text),
                Event::ClipboardLoad(_, format) => {
                    if let Some(text) = self.clipboard_get() {
                        self.write_pty_to(id, format(&text).as_bytes());
                    }
                }
                Event::ColorRequest(index_color, format) => {
                    let rgb = self
                        .config
                        .palette
                        .dynamic(self.tabs[ti].panes[pi].terminal.colors(), index_color);
                    self.write_pty_to(id, format(rgb.to_vte()).as_bytes());
                }
                Event::TextAreaSizeRequest(format) => {
                    let term = &self.tabs[ti].panes[pi].terminal;
                    let (cell_w, cell_h) = self
                        .renderer
                        .as_ref()
                        .map(Renderer::cell_size)
                        .unwrap_or((8.0, 16.0));
                    self.write_pty_to(
                        id,
                        format(WindowSize {
                            num_cols: term.cols() as u16,
                            num_lines: term.rows() as u16,
                            cell_width: cell_w as u16,
                            cell_height: cell_h as u16,
                        })
                        .as_bytes(),
                    );
                }
                Event::Exit => self.close_pane(id),
                _ => {}
            }
        }
    }

    fn update_title(&self) {
        if let (Some(window), Some(tab)) = (&self.window, self.tabs.get(self.active)) {
            window.set_title(tab.title());
        }
    }

    fn write_input(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let events = match self.terminal_mut() {
            Some(terminal) => terminal.scroll_to_bottom(),
            None => Vec::new(),
        };
        self.handle_events(events);
        self.write_pty(bytes);
        self.request_redraw();
    }

    fn write_pty(&mut self, bytes: &[u8]) {
        if let Some(id) = self.active_id() {
            self.write_pty_to(id, bytes);
        }
    }

    fn write_pty_to(&mut self, id: u64, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some((ti, pi)) = self.pane_location(id) {
            if let Err(err) = self.tabs[ti].panes[pi].pty.write(bytes) {
                // Reader EOF owns session removal; a failed background reply
                // must never close the active tab or the whole window.
                eprintln!("aiterm: tab {id}: {err}");
            }
        }
    }

    fn on_key(&mut self, key: &Key) {
        let mode = match self.terminal() {
            Some(terminal) => terminal.mode(),
            None => return,
        };
        match input::encode_key(key, self.modifiers, mode) {
            KeyAction::Bytes(bytes) => {
                if bytes.is_empty() {
                    return;
                }
                self.clear_selection();
                self.wake_cursor();
                self.write_input(&bytes);
            }
            KeyAction::Scroll(scroll) => self.scroll(scroll),
            KeyAction::Copy => self.copy_selection(),
            KeyAction::Paste => self.paste(),
            KeyAction::NewTab => {
                if let Err(err) = self.new_tab() {
                    eprintln!("aiterm: {err}");
                }
            }
            KeyAction::CloseTab => self.close_tab(self.active),
            KeyAction::NextTab => self.switch_tab((self.active + 1) % self.tabs.len()),
            KeyAction::PreviousTab => {
                self.switch_tab((self.active + self.tabs.len() - 1) % self.tabs.len())
            }
            KeyAction::SplitVertical => {
                if let Err(err) = self.split_pane(Axis::Vertical) {
                    eprintln!("aiterm: {err}");
                }
            }
            KeyAction::SplitHorizontal => {
                if let Err(err) = self.split_pane(Axis::Horizontal) {
                    eprintln!("aiterm: {err}");
                }
            }
            KeyAction::NextPane => self.cycle_pane(true),
            KeyAction::PreviousPane => self.cycle_pane(false),
            KeyAction::ClosePane => {
                if let Some(id) = self.active_id() {
                    self.close_pane(id);
                }
            }
            KeyAction::None => {}
        }
    }

    fn scroll(&mut self, scroll: Scroll) {
        let events = match self.terminal_mut() {
            Some(terminal) => terminal.scroll(scroll),
            None => return,
        };
        self.handle_events(events);
        self.request_redraw();
    }

    fn clear_selection(&mut self) {
        if let Some(terminal) = self.terminal_mut() {
            terminal.clear_selection();
        }
    }

    fn copy_selection(&mut self) {
        if let Some(text) = self.terminal().and_then(Terminal::selection_text) {
            self.clipboard_set(&text);
        }
    }

    fn paste(&mut self) {
        let Some(text) = self.clipboard_get() else {
            return;
        };
        let normalized = input::sanitize_paste(&text);
        let mode = self
            .terminal()
            .map(Terminal::mode)
            .unwrap_or_else(TermMode::empty);
        let bytes = if mode.contains(TermMode::BRACKETED_PASTE) {
            format!("\x1b[200~{normalized}\x1b[201~")
        } else {
            normalized
        };
        self.wake_cursor();
        self.write_input(bytes.as_bytes());
    }

    fn on_focus(&mut self, focused: bool) {
        if !focused {
            self.divider_drag = None;
            self.release_mouse();
        }
        self.focused = focused;
        self.wake_cursor();
        let report = self
            .terminal()
            .map(|term| term.mode().contains(TermMode::FOCUS_IN_OUT))
            .unwrap_or(false);
        if report {
            let seq: &[u8] = if focused { b"\x1b[I" } else { b"\x1b[O" };
            self.write_pty(seq);
        }
        self.request_redraw();
    }

    fn cell_at(&self) -> Option<(usize, usize, Side)> {
        let (cell_w, cell_h) = self.renderer.as_ref()?.cell_size();
        let cols = self.terminal()?.cols();
        let rows = self.terminal()?.rows();
        if cell_w < 1.0 || cell_h < 1.0 || cols == 0 || rows == 0 {
            return None;
        }
        let rect = self.pane()?.rect;
        let local_x = self.pointer.x - f64::from(rect.x);
        let local_y = self.pointer.y - f64::from(rect.y);
        let mut col = (local_x / f64::from(cell_w)).floor() as isize;
        let mut row = (local_y / f64::from(cell_h)).floor() as isize;
        let past_right = col >= cols as isize;
        let past_left = col < 0;
        col = col.clamp(0, cols as isize - 1);
        row = row.clamp(0, rows as isize - 1);
        let side = if past_right {
            Side::Right
        } else if past_left {
            Side::Left
        } else {
            let x_in = local_x - col as f64 * f64::from(cell_w);
            if x_in > f64::from(cell_w) / 2.0 {
                Side::Right
            } else {
                Side::Left
            }
        };
        Some((col as usize, row as usize, side))
    }

    fn over_tab_bar(&self) -> bool {
        self.renderer
            .as_ref()
            .is_some_and(|renderer| self.pointer.y < f64::from(renderer.cell_size().1))
    }

    fn release_mouse(&mut self) {
        if self.mouse_mode() && !self.modifiers.shift_key() {
            for code in 0..3 {
                if self.mouse_buttons & (1 << code) != 0 {
                    self.send_mouse(code, false);
                }
            }
        }
        self.mouse_buttons = 0;
        self.selecting = false;
    }

    fn mouse_mode(&self) -> bool {
        self.terminal()
            .map(|term| term.mode().intersects(TermMode::MOUSE_MODE))
            .unwrap_or(false)
    }

    fn note_button(&mut self, button: MouseButton, pressed: bool) {
        let Some(code) = input::mouse_button_code(button) else {
            return;
        };
        let mask = 1u8 << code.min(7);
        if pressed {
            self.mouse_buttons |= mask;
        } else {
            self.mouse_buttons &= !mask;
        }
    }

    /// Lowest held button, which is what a drag report names.
    fn held_button(&self) -> Option<u16> {
        if self.mouse_buttons & 1 != 0 {
            Some(0)
        } else if self.mouse_buttons & 2 != 0 {
            Some(1)
        } else if self.mouse_buttons & 4 != 0 {
            Some(2)
        } else {
            None
        }
    }

    fn on_mouse_button(&mut self, button: MouseButton, state: ElementState) {
        let pressed = state == ElementState::Pressed;
        if !pressed && button == MouseButton::Left && self.divider_drag.take().is_some() {
            return;
        }
        if self.over_tab_bar() {
            if pressed && button == MouseButton::Left {
                let width = self
                    .window
                    .as_ref()
                    .map(|w| w.inner_size().width as f32)
                    .unwrap_or(0.0);
                let cell_w = self
                    .renderer
                    .as_ref()
                    .map(|r| r.cell_size().0)
                    .unwrap_or(8.0);
                match crate::tab_bar::hit(self.pointer.x as f32, width, cell_w, self.tabs.len()) {
                    Some(crate::tab_bar::Hit::Select(index)) => self.switch_tab(index),
                    Some(crate::tab_bar::Hit::Close(index)) => self.close_tab(index),
                    Some(crate::tab_bar::Hit::New) => {
                        if let Err(err) = self.new_tab() {
                            eprintln!("aiterm: {err}");
                        }
                    }
                    None => {}
                }
            }
            if !pressed {
                // Complete a drag which began in the terminal, clamped to its
                // top row. Header clicks never set terminal button bits.
                if let Some(code) = input::mouse_button_code(button) {
                    if self.mouse_buttons & (1 << code) != 0
                        && self.mouse_mode()
                        && !self.modifiers.shift_key()
                    {
                        self.send_mouse(code, false);
                    }
                }
                if self.selecting {
                    self.copy_selection();
                }
                self.selecting = false;
                self.note_button(button, false);
            }
            return;
        }
        if pressed {
            if button == MouseButton::Left {
                if let Some(bounds) = self.content_bounds() {
                    if let Some(path) = self.tabs[self.active].layout.divider_at(
                        bounds,
                        DIVIDER,
                        self.pointer.x,
                        self.pointer.y,
                    ) {
                        self.release_mouse();
                        self.divider_drag = Some(path);
                        return;
                    }
                }
            }
            let pane = self.tabs[self.active]
                .panes
                .iter()
                .position(|pane| pane.rect.contains(self.pointer.x, self.pointer.y));
            let Some(index) = pane else {
                return;
            };
            self.focus_pane(index);
            self.note_button(button, true);
        }
        let shift = self.modifiers.shift_key();
        if self.mouse_mode() && !shift {
            if let Some(code) = input::mouse_button_code(button) {
                self.send_mouse(code, pressed);
            }
            if !pressed {
                self.note_button(button, false);
            }
            return;
        }
        if !pressed {
            self.note_button(button, false);
        }
        if button != MouseButton::Left {
            return;
        }
        if state == ElementState::Pressed {
            self.selecting = true;
            if let Some((col, row, side)) = self.cell_at() {
                if let Some(terminal) = self.terminal_mut() {
                    terminal.start_selection(col, row, side);
                }
            }
            self.request_redraw();
        } else {
            self.selecting = false;
            let text = self.terminal().and_then(Terminal::selection_text);
            if let Some(text) = text {
                self.clipboard_set(&text);
            } else if let Some(terminal) = self.terminal_mut() {
                terminal.clear_selection();
                self.request_redraw();
            }
        }
    }

    fn on_cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        self.pointer = position;
        if let Some(path) = self.divider_drag.clone() {
            if let Some(bounds) = self.content_bounds() {
                let (min_w, min_h) = self.leaf_minimum();
                if self.tabs[self.active]
                    .layout
                    .resize_divider(&path, bounds, DIVIDER, position.x, position.y, min_w, min_h)
                {
                    self.sync_grid();
                }
            }
            return;
        }
        if self.selecting {
            if let Some((col, row, side)) = self.cell_at() {
                if let Some(terminal) = self.terminal_mut() {
                    terminal.update_selection(col, row, side);
                }
            }
            self.request_redraw();
            return;
        }
        if !self.over_tab_bar()
            && (self.mouse_buttons != 0
                || self
                    .pane()
                    .is_some_and(|pane| pane.rect.contains(self.pointer.x, self.pointer.y)))
            && self.mouse_mode()
            && !self.modifiers.shift_key()
        {
            let mode = match self.terminal() {
                Some(terminal) => terminal.mode(),
                None => return,
            };
            let drag = mode.contains(TermMode::MOUSE_DRAG);
            let motion = mode.contains(TermMode::MOUSE_MOTION);
            // 32 added to the button code means motion. Button 3 is no button.
            if let Some(button) = self.held_button() {
                if motion || drag {
                    self.send_mouse(button + 32, true);
                }
            } else if motion {
                self.send_mouse(3 + 32, true);
            }
        }
    }

    fn on_wheel(&mut self, delta: MouseScrollDelta) {
        if self.divider_drag.is_some()
            || !self
                .pane()
                .is_some_and(|pane| pane.rect.contains(self.pointer.x, self.pointer.y))
        {
            return;
        }
        let cell_h = self
            .renderer
            .as_ref()
            .map(|renderer| renderer.cell_size().1)
            .unwrap_or(16.0);
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => input::notch_to_lines(y),
            MouseScrollDelta::PixelDelta(pos) => {
                input::consume_pixel_scroll(&mut self.pixel_scroll, pos.y as f32, cell_h)
            }
        };
        if lines == 0 {
            return;
        }
        let mode = match self.terminal() {
            Some(terminal) => terminal.mode(),
            None => return,
        };
        let shift = self.modifiers.shift_key();
        if !shift && mode.intersects(TermMode::MOUSE_MODE) {
            let code = if lines > 0 { 64 } else { 65 };
            let steps = lines.abs().min(10);
            for _ in 0..steps {
                self.send_mouse(code, true);
            }
            return;
        }
        if !shift
            && mode.contains(TermMode::ALT_SCREEN)
            && mode.contains(TermMode::ALTERNATE_SCROLL)
        {
            let seq = input::arrow_bytes(lines > 0, mode);
            let steps = lines.abs().min(30) as usize;
            let mut bytes = Vec::with_capacity(seq.len() * steps);
            for _ in 0..steps {
                bytes.extend_from_slice(&seq);
            }
            self.write_input(&bytes);
            return;
        }
        self.scroll(Scroll::Delta(lines));
    }

    fn send_mouse(&mut self, code: u16, press: bool) {
        let Some((col, row, _)) = self.cell_at() else {
            return;
        };
        let mode = match self.terminal() {
            Some(terminal) => terminal.mode(),
            None => return,
        };
        let bytes = input::encode_mouse(code, col, row, press, mode);
        self.write_pty(&bytes);
    }
}

fn ime_bypass(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(
            winit::keyboard::NamedKey::Escape
                | winit::keyboard::NamedKey::Backspace
                | winit::keyboard::NamedKey::Enter
        )
    )
}

fn grid_dim(pixels: u32, cell: f32, min: usize) -> usize {
    if cell < 1.0 {
        return min;
    }
    ((pixels as f32 / cell).floor() as usize).max(min)
}

impl App {
    fn clipboard(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(clipboard) => self.clipboard = Some(clipboard),
                Err(err) => {
                    eprintln!("aiterm: clipboard unavailable: {err}");
                    return None;
                }
            }
        }
        self.clipboard.as_mut()
    }

    fn clipboard_set(&mut self, text: &str) {
        let Some(clipboard) = self.clipboard() else {
            return;
        };
        if let Err(err) = clipboard.set_text(text) {
            eprintln!("aiterm: copy failed: {err}");
            #[cfg(target_os = "linux")]
            return;
        }
        #[cfg(target_os = "linux")]
        {
            use arboard::SetExtLinux;
            if let Err(err) = clipboard
                .set()
                .clipboard(arboard::LinuxClipboardKind::Primary)
                .text(text.to_string())
            {
                eprintln!("aiterm: primary selection failed: {err}");
            }
        }
    }

    fn clipboard_get(&mut self) -> Option<String> {
        self.clipboard()?.get_text().ok()
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() || self.startup_error.is_some() {
            return;
        }
        if let Err(err) = self.start(event_loop) {
            self.startup_error = Some(err);
            event_loop.exit();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Pty(id, bytes) => self.on_pty(id, bytes),
            UserEvent::Closed(id) => {
                if self.pane_location(id).is_some() {
                    eprintln!("aiterm: pane {id} shell exited");
                    self.close_pane(id);
                }
            }
        }
        if self.close {
            event_loop.exit();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) => self.sync_grid(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => self.apply_scale(scale_factor),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Focused(focused) => self.on_focus(focused),
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                // While an IME composition is open, character keys belong to the
                // IME. Escape, Enter, and Backspace still go to the shell.
                if self.composition.preediting && !ime_bypass(&event.logical_key) {
                    return;
                }
                self.on_key(&event.logical_key);
            }
            WindowEvent::Ime(Ime::Enabled | Ime::Disabled) => {
                self.composition = Composition::default();
            }
            WindowEvent::Ime(Ime::Preedit(text, _)) => {
                self.composition.preedit(&text, self.active_id());
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                if self.composition.commit(self.active_id()) {
                    self.wake_cursor();
                    self.write_input(text.as_bytes());
                }
            }
            WindowEvent::CursorMoved { position, .. } => self.on_cursor_moved(position),
            WindowEvent::MouseInput { state, button, .. } => self.on_mouse_button(button, state),
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(delta),
            _ => {}
        }
        if self.close {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.close {
            event_loop.exit();
            return;
        }
        if self.should_blink() {
            let now = Instant::now();
            if now >= self.next_blink {
                self.cursor_on = !self.cursor_on;
                self.next_blink = now + BLINK;
                self.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_blink));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Composition;

    #[test]
    fn composition_does_not_leak_after_tab_switch() {
        let mut ime = Composition::default();
        ime.preedit("composing", Some(1));
        // Platforms can send an empty preedit immediately before commit.
        ime.preedit("", Some(2));
        assert!(!ime.commit(Some(2)));
        assert!(!ime.preediting);
        // Direct commits without a preedit remain supported.
        assert!(ime.commit(Some(2)));
    }

    #[test]
    fn fresh_composition_after_cancellation_belongs_to_current_tab() {
        let mut ime = Composition::default();
        ime.preedit("cancelled", Some(1));
        ime.preedit("", Some(1));
        ime.preedit("new input", Some(2));
        ime.preedit("", Some(2));
        assert!(ime.commit(Some(2)));
    }
}
