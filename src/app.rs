//! Window loop.
//!
//! winit owns the frame. This module turns key and mouse events into PTY
//! bytes and asks the renderer to paint the grid. Tabs, splits, and an agent
//! sidebar would live here; they should not leak into [`crate::terminal`] or
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
use crate::pty::{PtyMsg, PtySession};
use crate::renderer::{CursorPaint, DrawResult, Renderer};
use crate::terminal::Terminal;

const BLINK: Duration = Duration::from_millis(530);

enum UserEvent {
    Pty(Vec<u8>),
    Closed,
}

struct App {
    config: Config,
    proxy: EventLoopProxy<UserEvent>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    terminal: Option<Terminal>,
    pty: Option<PtySession>,
    modifiers: ModifiersState,
    pointer: PhysicalPosition<f64>,
    selecting: bool,
    /// Bits 0..2 are left, middle, and right while held.
    mouse_buttons: u8,
    /// Unused pixel movement from touchpad scrolling.
    pixel_scroll: f32,
    composing: bool,
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
            terminal: None,
            pty: None,
            modifiers: ModifiersState::empty(),
            pointer: PhysicalPosition::new(0.0, 0.0),
            selecting: false,
            mouse_buttons: 0,
            pixel_scroll: 0.0,
            composing: false,
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
        let (cell_w, cell_h) = renderer.cell_size();
        let size = window.inner_size();
        let cols = grid_dim(size.width, cell_w, 2);
        let rows = grid_dim(size.height, cell_h, 1);
        let terminal = Terminal::new(cols, rows, self.config.scrollback);
        let shell = self.config.shell.clone();
        let proxy = self.proxy.clone();
        let pty = PtySession::spawn(
            shell.as_deref(),
            cols as u16,
            rows as u16,
            size.width.min(u16::MAX as u32) as u16,
            size.height.min(u16::MAX as u32) as u16,
            move |msg| {
                let event = match msg {
                    PtyMsg::Data(bytes) => UserEvent::Pty(bytes),
                    PtyMsg::Closed => UserEvent::Closed,
                };
                let _ = proxy.send_event(event);
            },
        )?;
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.terminal = Some(terminal);
        self.pty = Some(pty);
        self.request_redraw();
        Ok(())
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn cursor_paint(&self) -> CursorPaint {
        let visible = self
            .terminal
            .as_ref()
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
                .terminal
                .as_ref()
                .map(|term| term.mode().contains(TermMode::SHOW_CURSOR))
                .unwrap_or(false)
    }

    fn wake_cursor(&mut self) {
        self.cursor_on = true;
        self.next_blink = Instant::now() + BLINK;
    }

    fn redraw(&mut self) {
        let paint = self.cursor_paint();
        let retry = match (self.renderer.as_mut(), self.terminal.as_ref()) {
            (Some(renderer), Some(terminal)) => {
                matches!(
                    renderer.draw(terminal, &self.config.palette, paint),
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
        let Some((cell_w, cell_h)) = self.renderer.as_ref().map(Renderer::cell_size) else {
            return;
        };
        if size.width == 0 || size.height == 0 {
            return;
        }
        let cols = grid_dim(size.width, cell_w, 2);
        let rows = grid_dim(size.height, cell_h, 1);
        let changed = self
            .terminal
            .as_ref()
            .map(|term| term.cols() != cols || term.rows() != rows)
            .unwrap_or(true);
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.resize(cols, rows);
        }
        if changed {
            if let Some(pty) = self.pty.as_ref() {
                if let Err(err) = pty.resize(
                    cols as u16,
                    rows as u16,
                    size.width.min(u16::MAX as u32) as u16,
                    size.height.min(u16::MAX as u32) as u16,
                ) {
                    eprintln!("aiterm: {err}");
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

    fn on_pty(&mut self, bytes: Vec<u8>) {
        let events = match self.terminal.as_mut() {
            Some(terminal) => terminal.process(&bytes),
            None => return,
        };
        self.handle_events(events);
        self.request_redraw();
    }

    fn handle_events(&mut self, events: Vec<Event>) {
        for event in events {
            match event {
                Event::Title(title) => self.set_title(title),
                Event::ResetTitle => self.set_title("aiterm".to_string()),
                Event::PtyWrite(data) => self.write_pty(data.as_bytes()),
                Event::ClipboardStore(_, text) => self.clipboard_set(&text),
                Event::ClipboardLoad(_, format) => {
                    if let Some(text) = self.clipboard_get() {
                        let reply = format(&text);
                        self.write_pty(reply.as_bytes());
                    }
                }
                Event::ColorRequest(index, format) => {
                    let rgb = match self.terminal.as_ref() {
                        Some(terminal) => self.config.palette.dynamic(terminal.colors(), index),
                        None => self.config.palette.foreground,
                    };
                    let reply = format(rgb.to_vte());
                    self.write_pty(reply.as_bytes());
                }
                Event::TextAreaSizeRequest(format) => {
                    let (cols, rows, cell_w, cell_h) = self.area_size();
                    let reply = format(WindowSize {
                        num_cols: cols,
                        num_lines: rows,
                        cell_width: cell_w,
                        cell_height: cell_h,
                    });
                    self.write_pty(reply.as_bytes());
                }
                Event::Exit => self.close = true,
                Event::Bell
                | Event::Wakeup
                | Event::MouseCursorDirty
                | Event::CursorBlinkingChange
                | Event::ChildExit(_) => {}
            }
        }
    }

    fn area_size(&self) -> (u16, u16, u16, u16) {
        let (cols, rows) = self
            .terminal
            .as_ref()
            .map(|term| (term.cols(), term.rows()))
            .unwrap_or((80, 24));
        let (cell_w, cell_h) = self
            .renderer
            .as_ref()
            .map(Renderer::cell_size)
            .unwrap_or((8.0, 16.0));
        (cols as u16, rows as u16, cell_w as u16, cell_h as u16)
    }

    fn set_title(&self, title: String) {
        if let Some(window) = &self.window {
            window.set_title(&title);
        }
    }

    fn write_input(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let events = match self.terminal.as_mut() {
            Some(terminal) => terminal.scroll_to_bottom(),
            None => Vec::new(),
        };
        self.handle_events(events);
        self.write_pty(bytes);
        self.request_redraw();
    }

    fn write_pty(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some(pty) = self.pty.as_mut() {
            if let Err(err) = pty.write(bytes) {
                eprintln!("aiterm: {err}");
                self.close = true;
            }
        }
    }

    fn on_key(&mut self, key: &Key) {
        let mode = match self.terminal.as_ref() {
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
            KeyAction::None => {}
        }
    }

    fn scroll(&mut self, scroll: Scroll) {
        let events = match self.terminal.as_mut() {
            Some(terminal) => terminal.scroll(scroll),
            None => return,
        };
        self.handle_events(events);
        self.request_redraw();
    }

    fn clear_selection(&mut self) {
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.clear_selection();
        }
    }

    fn copy_selection(&mut self) {
        if let Some(text) = self.terminal.as_ref().and_then(Terminal::selection_text) {
            self.clipboard_set(&text);
        }
    }

    fn paste(&mut self) {
        let Some(text) = self.clipboard_get() else {
            return;
        };
        let normalized = input::sanitize_paste(&text);
        let mode = self
            .terminal
            .as_ref()
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
        self.focused = focused;
        self.wake_cursor();
        let report = self
            .terminal
            .as_ref()
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
        let cols = self.terminal.as_ref()?.cols();
        let rows = self.terminal.as_ref()?.rows();
        if cell_w < 1.0 || cell_h < 1.0 || cols == 0 || rows == 0 {
            return None;
        }
        let mut col = (self.pointer.x / f64::from(cell_w)).floor() as isize;
        let mut row = (self.pointer.y / f64::from(cell_h)).floor() as isize;
        let past_right = col >= cols as isize;
        let past_left = col < 0;
        col = col.clamp(0, cols as isize - 1);
        row = row.clamp(0, rows as isize - 1);
        let side = if past_right {
            Side::Right
        } else if past_left {
            Side::Left
        } else {
            let x_in = self.pointer.x - col as f64 * f64::from(cell_w);
            if x_in > f64::from(cell_w) / 2.0 {
                Side::Right
            } else {
                Side::Left
            }
        };
        Some((col as usize, row as usize, side))
    }

    fn mouse_mode(&self) -> bool {
        self.terminal
            .as_ref()
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
        if pressed {
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
                if let Some(terminal) = self.terminal.as_mut() {
                    terminal.start_selection(col, row, side);
                }
            }
            self.request_redraw();
        } else {
            self.selecting = false;
            let text = self.terminal.as_ref().and_then(Terminal::selection_text);
            if let Some(text) = text {
                self.clipboard_set(&text);
            } else if let Some(terminal) = self.terminal.as_mut() {
                terminal.clear_selection();
                self.request_redraw();
            }
        }
    }

    fn on_cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        self.pointer = position;
        if self.selecting {
            if let Some((col, row, side)) = self.cell_at() {
                if let Some(terminal) = self.terminal.as_mut() {
                    terminal.update_selection(col, row, side);
                }
            }
            self.request_redraw();
            return;
        }
        if self.mouse_mode() && !self.modifiers.shift_key() {
            let mode = match self.terminal.as_ref() {
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
        let mode = match self.terminal.as_ref() {
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
        let mode = match self.terminal.as_ref() {
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
            UserEvent::Pty(bytes) => self.on_pty(bytes),
            UserEvent::Closed => {
                eprintln!("aiterm: shell exited");
                event_loop.exit();
            }
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
                if self.composing && !ime_bypass(&event.logical_key) {
                    return;
                }
                self.on_key(&event.logical_key);
            }
            WindowEvent::Ime(Ime::Enabled) => self.composing = false,
            WindowEvent::Ime(Ime::Disabled) => self.composing = false,
            WindowEvent::Ime(Ime::Preedit(text, _)) => self.composing = !text.is_empty(),
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.composing = false;
                self.wake_cursor();
                self.write_input(text.as_bytes());
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
