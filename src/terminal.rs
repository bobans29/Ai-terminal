//! Terminal grid.
//!
//! This is a thin owner of `alacritty_terminal`: parser, scrollback, selection,
//! and the modes programs like vim use (alternate screen, bracketed paste,
//! application cursor keys). Bytes from the PTY go in; a grid the renderer can
//! walk comes out. One [`Terminal`] is one pane. Tabs would hold several.

use std::cell::RefCell;
use std::rc::Rc;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{self, viewport_to_point, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;

struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

struct Listener {
    events: Rc<RefCell<Vec<Event>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.events.borrow_mut().push(event);
    }
}

pub struct Terminal {
    term: Term<Listener>,
    parser: Processor,
    events: Rc<RefCell<Vec<Event>>>,
    cols: usize,
    rows: usize,
}

impl Terminal {
    pub fn new(cols: usize, rows: usize, scrollback: usize) -> Self {
        let cols = cols.max(2);
        let rows = rows.max(1);
        let events = Rc::new(RefCell::new(Vec::new()));
        let listener = Listener {
            events: Rc::clone(&events),
        };
        let config = term::Config {
            scrolling_history: scrollback,
            ..term::Config::default()
        };
        let term = Term::new(config, &Size { cols, rows }, listener);
        Self {
            term,
            parser: Processor::new(),
            events,
            cols,
            rows,
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Feed PTY bytes through the VT parser. Returns UI events the parser raised
    /// (title changes, clipboard, replies the PTY must see).
    pub fn process(&mut self, bytes: &[u8]) -> Vec<Event> {
        for &byte in bytes {
            self.parser.advance(&mut self.term, byte);
        }
        self.take_events()
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(2);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.term.resize(Size { cols, rows });
    }

    pub fn scroll(&mut self, scroll: Scroll) -> Vec<Event> {
        self.term.scroll_display(scroll);
        self.take_events()
    }

    pub fn scroll_to_bottom(&mut self) -> Vec<Event> {
        if self.display_offset() == 0 {
            Vec::new()
        } else {
            self.scroll(Scroll::Bottom)
        }
    }

    pub fn content(&self) -> term::RenderableContent<'_> {
        self.term.renderable_content()
    }

    pub fn colors(&self) -> &term::color::Colors {
        self.term.colors()
    }

    pub fn start_selection(&mut self, col: usize, row: usize, side: Side) {
        let point = self.viewport_point(col, row);
        self.term.selection = Some(Selection::new(SelectionType::Simple, point, side));
    }

    pub fn update_selection(&mut self, col: usize, row: usize, side: Side) {
        let point = self.viewport_point(col, row);
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term
            .selection_to_string()
            .filter(|text| !text.is_empty())
    }

    fn viewport_point(&self, col: usize, row: usize) -> Point {
        let col = col.min(self.cols - 1);
        let row = row.min(self.rows - 1);
        viewport_to_point(self.display_offset(), Point::new(row, Column(col)))
    }

    fn take_events(&self) -> Vec<Event> {
        self.events.borrow_mut().drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

    fn cell(term: &Terminal, line: i32, col: usize) -> (char, Color) {
        let cell = &term.term.grid()[alacritty_terminal::index::Line(line)][Column(col)];
        (cell.c, cell.fg)
    }

    #[test]
    fn sgr_truecolor_and_alternate_screen() {
        let mut term = Terminal::new(8, 4, 100);
        term.process(b"\x1b[31mHi\x1b[0m");
        assert_eq!(cell(&term, 0, 0), ('H', Color::Named(NamedColor::Red)));
        assert_eq!(cell(&term, 0, 1).0, 'i');

        term.process(b"\r\n\x1b[38;2;10;20;30mZ");
        assert_eq!(
            cell(&term, 1, 0),
            (
                'Z',
                Color::Spec(Rgb {
                    r: 10,
                    g: 20,
                    b: 30
                })
            )
        );

        term.process(b"\x1b[38;5;196mQ");
        assert_eq!(cell(&term, 1, 1), ('Q', Color::Indexed(196)));

        term.process(b"\x1b[?1049h");
        assert!(term.mode().contains(TermMode::ALT_SCREEN));
        term.process(b"\x1b[2J\x1b[Hvim");
        assert_eq!(cell(&term, 0, 0).0, 'v');
        term.process(b"\x1b[?1049l");
        assert!(!term.mode().contains(TermMode::ALT_SCREEN));
        // The primary grid is restored.
        assert_eq!(cell(&term, 0, 0).0, 'H');
    }

    #[test]
    fn cursor_movement_and_erase() {
        let mut term = Terminal::new(6, 3, 10);
        term.process(b"abcdef\x1b[1;1H\x1b[2Kxy");
        assert_eq!(cell(&term, 0, 0).0, 'x');
        assert_eq!(cell(&term, 0, 1).0, 'y');
        assert_eq!(cell(&term, 0, 2).0, ' ');
    }
}
