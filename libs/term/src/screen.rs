// SPDX-License-Identifier: GPL-2.0-only
//! A character-cell terminal.
//!
//! The screen is a grid of cells. The top `status_rows` rows are a fixed
//! status area written with [`Terminal::set_status`]; the rest scrolls.
//!
//! Understood on input: printable ASCII, newline, carriage return,
//! backspace (cursor left), tab, and the two escape sequences the system
//! itself emits: `ESC [ 2 J` (clear) and `ESC [ H` (cursor home). Other
//! escape sequences are consumed and ignored; other characters are shown as
//! `?`.

use alloc::vec;
use alloc::vec::Vec;

/// How a cell is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Style {
    /// Ordinary text.
    #[default]
    Normal,
    /// Status-bar text.
    Status,
}

/// One character position on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// The character, always printable ASCII.
    pub ch: u8,
    /// Its rendering style.
    pub style: Style,
}

impl Cell {
    const BLANK: Self = Self {
        ch: b' ',
        style: Style::Normal,
    };
}

/// Something that can show cells; implemented by the framebuffer driver.
pub trait Display {
    /// Draws `cell` at column `col`, row `row`. `cursor` asks for the cell
    /// to be highlighted as the cursor position.
    fn draw(&mut self, col: usize, row: usize, cell: Cell, cursor: bool);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escape {
    None,
    Started,
    /// Inside `ESC [`; holds the parameter bytes seen so far if they are a
    /// single small number, which is all the supported sequences use.
    Csi(Option<u16>),
}

/// Terminal state: cell contents, cursor and escape-sequence parser.
#[derive(Debug)]
pub struct Terminal {
    cols: usize,
    rows: usize,
    status_rows: usize,
    cells: Vec<Cell>,
    cursor_col: usize,
    cursor_row: usize,
    escape: Escape,
}

impl Terminal {
    /// Creates a blank terminal of `cols` x `rows` cells whose first
    /// `status_rows` rows are reserved for the status area. Returns `None`
    /// if that leaves no room for text.
    pub fn new(cols: usize, rows: usize, status_rows: usize) -> Option<Self> {
        if cols == 0 || rows <= status_rows {
            return None;
        }
        Some(Self {
            cols,
            rows,
            status_rows,
            cells: vec![Cell::BLANK; cols * rows],
            cursor_col: 0,
            cursor_row: status_rows,
            escape: Escape::None,
        })
    }

    /// Number of columns.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Number of rows, including the status area.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// The cell at (`col`, `row`).
    pub fn cell(&self, col: usize, row: usize) -> Cell {
        self.cells[row * self.cols + col]
    }

    /// The cursor position as (`col`, `row`).
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_col, self.cursor_row)
    }

    fn set(&mut self, col: usize, row: usize, cell: Cell, display: &mut dyn Display) {
        let slot = &mut self.cells[row * self.cols + col];
        if *slot != cell {
            *slot = cell;
            display.draw(col, row, cell, false);
        }
    }

    /// Draws every cell and the cursor; for the initial paint.
    pub fn redraw(&self, display: &mut dyn Display) {
        for row in 0..self.rows {
            for col in 0..self.cols {
                let cursor = (col, row) == self.cursor();
                display.draw(col, row, self.cell(col, row), cursor);
            }
        }
    }

    /// Replaces the status area with `text` (truncated or space-padded to
    /// the screen width; non-ASCII characters become `?`). With more than
    /// one status row, the text goes on the first and the rest are blank.
    pub fn set_status(&mut self, text: &str, display: &mut dyn Display) {
        let mut chars = text.chars();
        for row in 0..self.status_rows {
            for col in 0..self.cols {
                let ch = if row == 0 { chars.next() } else { None };
                let cell = Cell {
                    ch: ch.map_or(b' ', printable),
                    style: Style::Status,
                };
                self.set(col, row, cell, display);
            }
        }
    }

    /// Scrolls the text area up by one row, redrawing only cells whose
    /// contents changed.
    fn scroll(&mut self, display: &mut dyn Display) {
        for row in self.status_rows..self.rows {
            for col in 0..self.cols {
                let cell = if row + 1 < self.rows {
                    self.cell(col, row + 1)
                } else {
                    Cell::BLANK
                };
                self.set(col, row, cell, display);
            }
        }
    }

    fn newline(&mut self, display: &mut dyn Display) {
        self.cursor_col = 0;
        if self.cursor_row + 1 < self.rows {
            self.cursor_row += 1;
        } else {
            self.scroll(display);
        }
    }

    fn clear(&mut self, display: &mut dyn Display) {
        for row in self.status_rows..self.rows {
            for col in 0..self.cols {
                self.set(col, row, Cell::BLANK, display);
            }
        }
    }

    fn put(&mut self, ch: u8, display: &mut dyn Display) {
        if self.cursor_col == self.cols {
            self.newline(display);
        }
        let cell = Cell {
            ch,
            style: Style::Normal,
        };
        self.set(self.cursor_col, self.cursor_row, cell, display);
        // The cursor may rest one past the last column; the wrap happens
        // when the next character arrives, so a full line followed by a
        // newline does not produce a blank line.
        self.cursor_col += 1;
    }

    fn process(&mut self, c: char, display: &mut dyn Display) {
        match self.escape {
            Escape::Started => {
                self.escape = if c == '[' {
                    Escape::Csi(Some(0))
                } else {
                    Escape::None
                };
                return;
            }
            Escape::Csi(parameter) => {
                match c {
                    '0'..='9' => {
                        let digit = c as u16 - '0' as u16;
                        self.escape = Escape::Csi(
                            parameter.and_then(|p| p.checked_mul(10)?.checked_add(digit)),
                        );
                    }
                    // Other parameter and intermediate bytes: a sequence we
                    // do not support; keep consuming it.
                    ' '..='?' => self.escape = Escape::Csi(None),
                    _ => {
                        self.escape = Escape::None;
                        match (c, parameter) {
                            ('J', Some(2)) => self.clear(display),
                            ('H', Some(0)) => {
                                self.cursor_col = 0;
                                self.cursor_row = self.status_rows;
                            }
                            _ => {}
                        }
                    }
                }
                return;
            }
            Escape::None => {}
        }

        match c {
            '\n' => self.newline(display),
            '\r' => self.cursor_col = 0,
            '\x08' => self.cursor_col = self.cursor_col.saturating_sub(1),
            '\t' => {
                let next = (self.cursor_col / 8 + 1) * 8;
                while self.cursor_col < next.min(self.cols) {
                    self.put(b' ', display);
                }
            }
            '\x1b' => self.escape = Escape::Started,
            c if (c as u32) < 0x20 || c == '\x7f' => {}
            c => self.put(printable(c), display),
        }
    }

    /// Writes `text` to the terminal and moves the cursor highlight.
    pub fn write_str(&mut self, text: &str, display: &mut dyn Display) {
        let before = self.visible_cursor();
        for c in text.chars() {
            self.process(c, display);
        }
        let after = self.visible_cursor();
        // Cells under the old and new cursor need their highlight updated
        // even if their contents did not change.
        display.draw(
            before.0,
            before.1,
            self.cell(before.0, before.1),
            before == after,
        );
        display.draw(after.0, after.1, self.cell(after.0, after.1), true);
    }

    /// Where the cursor highlight is shown: the cursor position, clamped
    /// to the last column while a wrap is pending.
    fn visible_cursor(&self) -> (usize, usize) {
        (self.cursor_col.min(self.cols - 1), self.cursor_row)
    }
}

/// Maps a character to the byte shown for it.
fn printable(c: char) -> u8 {
    match c {
        ' '..='~' => c as u8,
        // Typographic dashes are common in the kernel's own messages.
        '\u{2013}' | '\u{2014}' => b'-',
        _ => b'?',
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::String;
    use std::vec::Vec;

    /// A display that mirrors what it is told to draw, so tests can check
    /// that the visible screen always matches the terminal's cells.
    struct Mirror {
        cols: usize,
        cells: Vec<Cell>,
        cursor: Option<(usize, usize)>,
        draws: usize,
    }

    impl Mirror {
        fn new(cols: usize, rows: usize) -> Self {
            Self {
                cols,
                cells: vec![Cell::BLANK; cols * rows],
                cursor: None,
                draws: 0,
            }
        }

        fn row(&self, row: usize) -> String {
            self.cells[row * self.cols..(row + 1) * self.cols]
                .iter()
                .map(|c| c.ch as char)
                .collect::<String>()
                .trim_end()
                .into()
        }
    }

    impl Display for Mirror {
        fn draw(&mut self, col: usize, row: usize, cell: Cell, cursor: bool) {
            self.cells[row * self.cols + col] = cell;
            self.draws += 1;
            if cursor {
                self.cursor = Some((col, row));
            } else if self.cursor == Some((col, row)) {
                self.cursor = None;
            }
        }
    }

    fn setup(cols: usize, rows: usize, status: usize) -> (Terminal, Mirror) {
        (
            Terminal::new(cols, rows, status).unwrap(),
            Mirror::new(cols, rows),
        )
    }

    fn assert_in_sync(term: &Terminal, mirror: &Mirror) {
        for row in 0..term.rows() {
            for col in 0..term.cols() {
                assert_eq!(mirror.cells[row * term.cols() + col], term.cell(col, row));
            }
        }
        let (col, row) = term.cursor();
        assert_eq!(mirror.cursor, Some((col.min(term.cols() - 1), row)));
    }

    #[test]
    fn rejects_degenerate_sizes() {
        assert!(Terminal::new(0, 10, 0).is_none());
        assert!(Terminal::new(10, 1, 1).is_none());
        assert!(Terminal::new(10, 2, 1).is_some());
    }

    #[test]
    fn prints_text_and_handles_line_endings() {
        let (mut t, mut m) = setup(10, 4, 0);
        t.write_str("ab\ncd\r\nef", &mut m);
        assert_eq!(m.row(0), "ab");
        assert_eq!(m.row(1), "cd");
        assert_eq!(m.row(2), "ef");
        assert_eq!(t.cursor(), (2, 2));
        t.write_str("\rX", &mut m);
        assert_eq!(m.row(2), "Xf");
        assert_in_sync(&t, &m);
    }

    #[test]
    fn wraps_long_lines_without_extra_blank_line() {
        let (mut t, mut m) = setup(4, 4, 0);
        t.write_str("abcd", &mut m);
        assert_eq!(t.cursor(), (4, 0), "wrap is deferred");
        assert_in_sync(&t, &m);
        t.write_str("\n", &mut m);
        assert_eq!(
            t.cursor(),
            (0, 1),
            "newline after a full line: one row down"
        );
        t.write_str("efghij", &mut m);
        assert_eq!(m.row(1), "efgh");
        assert_eq!(m.row(2), "ij");
        assert_in_sync(&t, &m);
    }

    #[test]
    fn scrolls_only_the_text_area() {
        let (mut t, mut m) = setup(6, 4, 1);
        t.set_status("STATUS", &mut m);
        t.write_str("one\ntwo\nthree\nfour", &mut m);
        assert_eq!(m.row(0), "STATUS");
        assert_eq!(m.row(1), "two");
        assert_eq!(m.row(2), "three");
        assert_eq!(m.row(3), "four");
        assert_eq!(t.cell(0, 0).style, Style::Status);
        assert_eq!(t.cell(0, 1).style, Style::Normal);
        assert_in_sync(&t, &m);
    }

    #[test]
    fn backspace_sequence_erases_a_character() {
        let (mut t, mut m) = setup(10, 2, 0);
        t.write_str("lx", &mut m);
        t.write_str("\x08 \x08", &mut m);
        t.write_str("s", &mut m);
        assert_eq!(m.row(0), "ls");
        t.write_str("\r\x08\x08", &mut m);
        assert_eq!(t.cursor(), (0, 0), "backspace stops at the left edge");
        assert_in_sync(&t, &m);
    }

    #[test]
    fn clear_and_home() {
        let (mut t, mut m) = setup(8, 4, 1);
        t.set_status("bar", &mut m);
        t.write_str("junk\nmore", &mut m);
        t.write_str("\x1b[2J\x1b[H", &mut m);
        assert_eq!(m.row(0), "bar", "status survives a clear");
        assert_eq!(m.row(1), "");
        assert_eq!(m.row(2), "");
        assert_eq!(t.cursor(), (0, 1));
        t.write_str("ok", &mut m);
        assert_eq!(m.row(1), "ok");
        assert_in_sync(&t, &m);
    }

    #[test]
    fn unsupported_sequences_and_characters() {
        let (mut t, mut m) = setup(20, 2, 0);
        t.write_str("a\x1b[31mb\x1b[1;5Hc\x1b[?25ld\x1bZe\x07\x00", &mut m);
        assert_eq!(m.row(0), "abcde");
        t.write_str("\r\u{2014}\u{e9}\u{4e2d}", &mut m);
        assert_eq!(m.row(0), "-??de");
        // An escape sequence split across writes is still recognised.
        t.write_str("\x1b[2", &mut m);
        t.write_str("J", &mut m);
        assert_eq!(m.row(0), "");
        assert_in_sync(&t, &m);
    }

    #[test]
    fn tabs_advance_to_multiples_of_eight() {
        let (mut t, mut m) = setup(20, 2, 0);
        t.write_str("a\tb\t\tc", &mut m);
        assert_eq!(
            m.row(0),
            "a       b               c".get(..20).unwrap().trim_end()
        );
        assert_in_sync(&t, &m);
    }

    #[test]
    fn status_is_padded_truncated_and_cheap_to_repeat() {
        let (mut t, mut m) = setup(5, 3, 1);
        t.set_status("ab", &mut m);
        assert_eq!(m.row(0), "ab");
        assert!((0..5).all(|c| t.cell(c, 0).style == Style::Status));
        t.set_status("abcdefgh", &mut m);
        assert_eq!(m.row(0), "abcde");
        let draws = m.draws;
        t.set_status("abcdefgh", &mut m);
        assert_eq!(m.draws, draws, "unchanged status redraws nothing");
    }

    #[test]
    fn redraw_paints_everything() {
        let (mut t, mut scratch) = setup(4, 3, 1);
        t.set_status("s", &mut scratch);
        t.write_str("hi", &mut scratch);
        let mut fresh = Mirror::new(4, 3);
        t.redraw(&mut fresh);
        assert_eq!(fresh.draws, 12);
        assert_in_sync(&t, &fresh);
    }
}
