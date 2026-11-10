// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Notes: a plain text editor.
//!
//! One file per window, in the fixed-width font: type, move with the
//! arrows or the mouse, save with the button or Ctrl+S. What it cannot
//! safely write back — a file that is not text, or one too large — it
//! shows without letting it be changed.
//!
//! Where files come from and go to is behind [`Documents`], which the
//! desktop program implements with system calls and the tests with a
//! map in memory.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::desktop::{App, Context};
use crate::font::{MEDIUM_13, REGULAR_12};
use crate::geometry::{Point, Rect};
use crate::surface::{CHAR_WIDTH, Canvas, TEXT_HEIGHT};
use crate::theme::Theme;

/// The size the window asks for.
pub const SIZE: (i32, i32) = (640, 420);
/// Files larger than this are shown, in part, but cannot be changed.
pub const EDIT_LIMIT: usize = 256 * 1024;
/// How far Tab moves, in characters.
pub const TAB: usize = 4;

const TOOLBAR: i32 = 44;
const STATUS: i32 = 28;
const LINE: i32 = TEXT_HEIGHT + 2;
const PAD: i32 = 8;
const SAVE_BUTTON: (i32, i32) = (72, 28);

/// Where notes are kept.
pub trait Documents {
    /// The contents of the file at `path`, or why they cannot be had.
    fn read(&mut self, path: &str) -> Result<Vec<u8>, String>;
    /// Replaces the contents of the file at `path`, creating it if need
    /// be; or says why that cannot be done.
    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String>;
    /// A path no file has yet, for a new note.
    fn fresh(&mut self) -> String;
}

/// What the line under the tool bar's button says.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Notice {
    Nothing,
    Saved,
    Failed(String),
    /// The window was asked to close with changes not saved; asked
    /// again, it closes.
    Unsaved,
}

/// A Notes window.
pub struct Notes<D: Documents> {
    documents: D,
    path: String,
    title: String,
    /// The text, a line per entry, without line ends; never empty.
    lines: Vec<Vec<char>>,
    /// The line and the column (in characters) the cursor is before.
    cursor: (usize, usize),
    /// The column the cursor tries to keep while moving up and down
    /// through shorter lines.
    wanted: usize,
    /// The first line and column shown.
    scroll: (usize, usize),
    modified: bool,
    /// Why the text cannot be changed, if it cannot.
    read_only: Option<String>,
    notice: Notice,
    /// Lines ended in carriage return and line feed in the file.
    crlf: bool,
    /// The file's last line ended in a line end.
    final_newline: bool,
    size: (i32, i32),
}

fn file_name(path: &str) -> &str {
    path.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

impl<D: Documents> Notes<D> {
    fn empty(documents: D, path: String) -> Self {
        let mut notes = Self {
            documents,
            title: String::new(),
            path,
            lines: vec![Vec::new()],
            cursor: (0, 0),
            wanted: 0,
            scroll: (0, 0),
            modified: false,
            read_only: None,
            notice: Notice::Nothing,
            crlf: false,
            final_newline: true,
            size: SIZE,
        };
        notes.retitle();
        notes
    }

    /// A new, empty note. It gets its file when it is first saved.
    pub fn new(mut documents: D) -> Self {
        let path = documents.fresh();
        Self::empty(documents, path)
    }

    /// Opens the file at `path`.
    pub fn open(mut documents: D, path: &str) -> Self {
        let read = documents.read(path);
        let mut notes = Self::empty(documents, path.into());
        match read {
            Err(reason) => {
                notes.read_only = Some(format!("Cannot be read: {reason}"));
            }
            Ok(data) => notes.load(&data),
        }
        notes
    }

    fn load(&mut self, data: &[u8]) {
        let shown = &data[..data.len().min(EDIT_LIMIT)];
        // Cut off in the middle of a character, the end is dropped.
        let text = match core::str::from_utf8(shown) {
            Ok(text) => text,
            Err(error) if data.len() > shown.len() && error.error_len().is_none() => {
                core::str::from_utf8(&shown[..error.valid_up_to()]).unwrap_or("")
            }
            Err(_) => {
                self.read_only = Some(format!("Not a text file ({} bytes)", data.len()));
                return;
            }
        };
        if text.contains('\0') {
            self.read_only = Some(format!("Not a text file ({} bytes)", data.len()));
            return;
        }
        if data.len() > shown.len() {
            self.read_only = Some(format!(
                "Too large to change: the first {} KB of {} are shown",
                EDIT_LIMIT / 1024,
                data.len() / 1024
            ));
        }
        self.crlf = text.contains("\r\n");
        self.final_newline = text.is_empty() || text.ends_with('\n');
        let body = text.strip_suffix('\n').unwrap_or(text);
        self.lines = body
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line).chars().collect())
            .collect();
    }

    fn retitle(&mut self) {
        self.title = format!(
            "Notes — {}{}",
            file_name(&self.path),
            if self.modified { " •" } else { "" }
        );
    }

    /// The text as it would be written to the file.
    pub fn text(&self) -> String {
        let end = if self.crlf { "\r\n" } else { "\n" };
        let mut text = String::new();
        for (index, line) in self.lines.iter().enumerate() {
            if index > 0 {
                text.push_str(end);
            }
            text.extend(line.iter());
        }
        if self.final_newline && !(self.lines.len() == 1 && self.lines[0].is_empty()) {
            text.push_str(end);
        }
        text
    }

    /// The line and column the cursor is at, from 0.
    pub fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    /// Whether there are changes that have not been saved.
    pub fn is_modified(&self) -> bool {
        self.modified
    }

    /// Whether the text can be changed.
    pub fn is_read_only(&self) -> bool {
        self.read_only.is_some()
    }

    /// The file this note is, or will be when saved.
    pub fn path(&self) -> &str {
        &self.path
    }

    fn changed(&mut self) {
        self.notice = Notice::Nothing;
        if !self.modified {
            self.modified = true;
            self.retitle();
        }
    }

    /// Writes the text to its file.
    pub fn save(&mut self) {
        if self.read_only.is_some() {
            return;
        }
        match self.documents.write(&self.path, self.text().as_bytes()) {
            Ok(()) => {
                self.modified = false;
                self.notice = Notice::Saved;
                self.retitle();
            }
            Err(reason) => self.notice = Notice::Failed(reason),
        }
    }

    fn line_len(&self, line: usize) -> usize {
        self.lines[line].len()
    }

    fn insert(&mut self, character: char) {
        let (line, column) = self.cursor;
        self.lines[line].insert(column, character);
        self.cursor.1 += 1;
        self.wanted = self.cursor.1;
        self.changed();
    }

    /// Enter: the rest of the line moves to a new one, which starts
    /// with the same spaces as this one.
    fn new_line(&mut self) {
        let (line, column) = self.cursor;
        let rest = self.lines[line].split_off(column);
        let indent: Vec<char> = self.lines[line]
            .iter()
            .take_while(|&&character| character == ' ')
            .copied()
            .collect();
        let column = indent.len();
        let mut next = indent;
        next.extend(rest);
        self.lines.insert(line + 1, next);
        self.cursor = (line + 1, column);
        self.wanted = column;
        self.changed();
    }

    fn backspace(&mut self) {
        let (line, column) = self.cursor;
        if column > 0 {
            self.lines[line].remove(column - 1);
            self.cursor.1 -= 1;
        } else if line > 0 {
            let moved = self.lines.remove(line);
            let column = self.line_len(line - 1);
            self.lines[line - 1].extend(moved);
            self.cursor = (line - 1, column);
        } else {
            return;
        }
        self.wanted = self.cursor.1;
        self.changed();
    }

    fn delete(&mut self) {
        let (line, column) = self.cursor;
        if column < self.line_len(line) {
            self.lines[line].remove(column);
        } else if line + 1 < self.lines.len() {
            let moved = self.lines.remove(line + 1);
            self.lines[line].extend(moved);
        } else {
            return;
        }
        self.changed();
    }

    fn visible_rows(&self) -> usize {
        ((self.size.1 - TOOLBAR - STATUS - 2 * PAD) / LINE).max(1) as usize
    }

    /// The width of the column of line numbers.
    fn gutter(&self) -> i32 {
        let digits = self.lines.len().max(1).ilog10() as i32 + 1;
        (digits.max(2) + 2) * CHAR_WIDTH
    }

    fn text_left(&self) -> i32 {
        self.gutter() + PAD
    }

    fn visible_columns(&self) -> usize {
        ((self.size.0 - self.text_left() - PAD) / CHAR_WIDTH).max(1) as usize
    }

    fn keep_cursor_visible(&mut self) {
        let (rows, columns) = (self.visible_rows(), self.visible_columns());
        let (line, column) = self.cursor;
        if line < self.scroll.0 {
            self.scroll.0 = line;
        } else if line >= self.scroll.0 + rows {
            self.scroll.0 = line + 1 - rows;
        }
        if column < self.scroll.1 {
            self.scroll.1 = column;
        } else if column >= self.scroll.1 + columns {
            self.scroll.1 = column + 1 - columns;
        }
    }

    /// Moves to `line`, keeping the wanted column as far as the line
    /// allows.
    fn go_to_line(&mut self, line: usize) {
        let line = line.min(self.lines.len() - 1);
        self.cursor = (line, self.wanted.min(self.line_len(line)));
    }

    /// Where the save button is, in a window `width` wide.
    pub fn save_button(width: i32) -> Rect {
        Rect::new(
            width - 12 - SAVE_BUTTON.0,
            (TOOLBAR - SAVE_BUTTON.1) / 2,
            SAVE_BUTTON.0,
            SAVE_BUTTON.1,
        )
    }

    /// Where the character at `line` and `column` is drawn, if that is
    /// on the screen.
    pub fn cell_frame(&self, line: usize, column: usize) -> Option<Rect> {
        let row = line.checked_sub(self.scroll.0)?;
        let across = column.checked_sub(self.scroll.1)?;
        (row < self.visible_rows() && across <= self.visible_columns()).then(|| {
            Rect::new(
                self.text_left() + across as i32 * CHAR_WIDTH,
                TOOLBAR + PAD + row as i32 * LINE,
                CHAR_WIDTH,
                LINE,
            )
        })
    }
}

impl<D: Documents> App for Notes<D> {
    fn title(&self) -> &str {
        &self.title
    }

    fn icon(&self) -> &str {
        "notes"
    }

    fn scroll(&mut self, lines: i32) -> bool {
        let last = self.lines.len().saturating_sub(self.visible_rows());
        let to = (self.scroll.0 as i64 + i64::from(lines)).clamp(0, last as i64) as usize;
        let moved = to != self.scroll.0;
        self.scroll.0 = to;
        moved
    }

    fn draw(&mut self, canvas: &mut Canvas<'_>, width: i32, height: i32, theme: &Theme) {
        self.size = (width, height);
        // (The cursor is brought into view when it moves, not here:
        // the wheel may scroll it out of sight.)
        self.scroll.0 = self.scroll.0.min(self.lines.len() - 1);
        let hairline = |canvas: &mut Canvas<'_>, area: Rect| {
            canvas.tint(area, theme.separator.colour, theme.separator.alpha);
        };

        // The tool bar: the file, what happened to it, and Save.
        canvas.fill(Rect::new(0, 0, width, TOOLBAR), theme.chrome);
        hairline(canvas, Rect::new(0, TOOLBAR - 1, width, 1));
        let button = Self::save_button(width);
        let middle = |font: crate::font::Font| (TOOLBAR - font.height()) / 2;
        let (said, ink) = match (&self.read_only, &self.notice) {
            (Some(reason), _) => (reason.clone(), theme.text_3),
            (None, Notice::Failed(reason)) => (format!("Not saved: {reason}"), theme.danger),
            (None, Notice::Saved) => ("Saved".into(), theme.success),
            (None, Notice::Unsaved) => (
                "Not saved: close again to lose the changes".into(),
                theme.danger,
            ),
            (None, Notice::Nothing) if self.modified => ("Changed".into(), theme.text_3),
            (None, Notice::Nothing) => (String::new(), theme.text_3),
        };
        {
            let mut bar = canvas.clipped(Rect::new(0, 0, button.x - 12, TOOLBAR));
            let end = bar.write(
                14,
                middle(MEDIUM_13),
                file_name(&self.path),
                MEDIUM_13,
                theme.text,
            );
            bar.write(end + 12, middle(REGULAR_12), &said, REGULAR_12, ink);
        }
        if self.read_only.is_none() {
            let label = "Save";
            let ink = if self.modified {
                canvas.round_rect(button, 8, theme.accent, 255);
                theme.on_accent
            } else {
                canvas.round_rect(button, 8, theme.fill.colour, theme.fill.alpha);
                theme.text_3
            };
            canvas.write(
                button.x + (button.w - MEDIUM_13.width(label)) / 2,
                button.y + (button.h - MEDIUM_13.height()) / 2,
                label,
                MEDIUM_13,
                ink,
            );
        }

        // The text, with line numbers beside it.
        let gutter = self.gutter();
        let body = Rect::new(0, TOOLBAR, width, height - TOOLBAR - STATUS);
        canvas.fill(Rect::new(0, body.y, gutter, body.h), theme.chrome);
        let (rows, columns) = (self.visible_rows(), self.visible_columns());
        let left = self.text_left();
        {
            let mut page = canvas.clipped(body);
            for row in 0..rows {
                let line = self.scroll.0 + row;
                let Some(text) = self.lines.get(line) else {
                    break;
                };
                let y = TOOLBAR + PAD + row as i32 * LINE;
                let here = line == self.cursor.0;
                if here && self.read_only.is_none() {
                    page.tint(
                        Rect::new(gutter, y, width - gutter, LINE),
                        theme.fill.colour,
                        theme.fill.alpha,
                    );
                }
                let number = format!("{}", line + 1);
                page.text(
                    gutter - CHAR_WIDTH - crate::surface::text_width(&number),
                    y + 1,
                    &number,
                    if here { theme.text_2 } else { theme.text_3 },
                );
                let shown: String = text
                    .iter()
                    .skip(self.scroll.1)
                    .take(columns)
                    // A tab takes one place here: it is kept as it is
                    // in the file, not laid out.
                    .map(|&character| if character == '\t' { ' ' } else { character })
                    .collect();
                page.text(left, y + 1, &shown, theme.text);
            }
            if self.read_only.is_none()
                && let Some(cell) = self.cell_frame(self.cursor.0, self.cursor.1)
            {
                page.fill(Rect::new(cell.x - 1, cell.y, 2, cell.h), theme.accent);
            }
        }

        // The status line.
        hairline(canvas, Rect::new(0, height - STATUS, width, 1));
        let status = format!(
            "Line {}, column {}    {} {}",
            self.cursor.0 + 1,
            self.cursor.1 + 1,
            self.lines.len(),
            if self.lines.len() == 1 {
                "line"
            } else {
                "lines"
            }
        );
        canvas.write(
            14,
            height - STATUS + (STATUS - REGULAR_12.height()) / 2,
            &status,
            REGULAR_12,
            theme.text_2,
        );
    }

    fn click(&mut self, x: i32, y: i32, _: &mut Context) {
        let at = Point::new(x, y);
        if self.notice == Notice::Unsaved {
            self.notice = Notice::Nothing;
        }
        if y < TOOLBAR {
            if Self::save_button(self.size.0).contains(at) {
                self.save();
            }
            return;
        }
        if y >= self.size.1 - STATUS {
            return;
        }
        // The nearest gap between two characters, on the nearest line.
        let row = ((y - TOOLBAR - PAD).max(0) / LINE) as usize;
        let line = (self.scroll.0 + row).min(self.lines.len() - 1);
        let across = ((x - self.text_left() + CHAR_WIDTH / 2).max(0) / CHAR_WIDTH) as usize;
        let column = (self.scroll.1 + across).min(self.line_len(line));
        self.cursor = (line, column);
        self.wanted = column;
        self.keep_cursor_visible();
    }

    /// Closing with changes not saved is refused once, and says why.
    /// Anything done in between (typing, moving, a failed save) and it
    /// asks again the next time.
    fn closing(&mut self) -> bool {
        if !self.modified || self.notice == Notice::Unsaved {
            return true;
        }
        self.notice = Notice::Unsaved;
        false
    }

    fn key(&mut self, code: i32, _: &mut Context) {
        const TAB_KEY: i32 = 9;
        const ENTER: i32 = 13;
        const SAVE: i32 = 19; // Ctrl+S
        const BACKSPACE: i32 = 127;
        const UP: i32 = 0x101;
        const DOWN: i32 = 0x102;
        const LEFT: i32 = 0x103;
        const RIGHT: i32 = 0x104;
        const HOME: i32 = 0x105;
        const END: i32 = 0x106;
        const PAGE_UP: i32 = 0x107;
        const PAGE_DOWN: i32 = 0x108;
        const DELETE: i32 = 0x10A;
        let (line, column) = self.cursor;
        let page = self.visible_rows().saturating_sub(1).max(1);
        if self.notice == Notice::Unsaved {
            self.notice = Notice::Nothing;
        }
        match code {
            UP => self.go_to_line(line.saturating_sub(1)),
            DOWN => self.go_to_line(line + 1),
            PAGE_UP => self.go_to_line(line.saturating_sub(page)),
            PAGE_DOWN => self.go_to_line(line + page),
            LEFT | RIGHT | HOME | END => {
                self.cursor = match code {
                    LEFT if column > 0 => (line, column - 1),
                    LEFT if line > 0 => (line - 1, self.line_len(line - 1)),
                    RIGHT if column < self.line_len(line) => (line, column + 1),
                    RIGHT if line + 1 < self.lines.len() => (line + 1, 0),
                    HOME => (line, 0),
                    END => (line, self.line_len(line)),
                    _ => (line, column),
                };
                self.wanted = self.cursor.1;
            }
            SAVE => self.save(),
            _ if self.read_only.is_some() => {}
            ENTER => self.new_line(),
            BACKSPACE => self.backspace(),
            DELETE => self.delete(),
            TAB_KEY => {
                for _ in 0..(TAB - column % TAB) {
                    self.insert(' ');
                }
            }
            0x20..=0x7E => self.insert(char::from(code as u8)),
            _ => {}
        }
        self.keep_cursor_visible();
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;
    use alloc::rc::Rc;
    use core::cell::RefCell;

    use super::*;
    use crate::surface::Surface;
    use crate::theme::{Accent, Mode};

    const UP: i32 = 0x101;
    const DOWN: i32 = 0x102;
    const LEFT: i32 = 0x103;
    const RIGHT: i32 = 0x104;
    const HOME: i32 = 0x105;
    const END: i32 = 0x106;
    const PAGE_DOWN: i32 = 0x108;
    const DELETE: i32 = 0x10A;

    /// Files in memory; a path starting with `/locked` cannot be
    /// written.
    #[derive(Clone, Default)]
    struct Shelf(Rc<RefCell<BTreeMap<String, Vec<u8>>>>);

    impl Shelf {
        fn with(files: &[(&str, &[u8])]) -> Self {
            let shelf = Self::default();
            for (path, data) in files {
                shelf.0.borrow_mut().insert((*path).into(), data.to_vec());
            }
            shelf
        }

        fn get(&self, path: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(path).cloned()
        }
    }

    impl Documents for Shelf {
        fn read(&mut self, path: &str) -> Result<Vec<u8>, String> {
            self.get(path).ok_or_else(|| "no such file".into())
        }

        fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
            if path.starts_with("/locked") {
                return Err("read-only filesystem".into());
            }
            self.0.borrow_mut().insert(path.into(), data.to_vec());
            Ok(())
        }

        fn fresh(&mut self) -> String {
            (1..)
                .map(|number| format!("/note-{number}.txt"))
                .find(|path| !self.0.borrow().contains_key(path))
                .unwrap()
        }
    }

    fn keys<D: Documents>(notes: &mut Notes<D>, codes: &[i32]) {
        let mut context = Context::default();
        for &code in codes {
            notes.key(code, &mut context);
        }
    }

    fn typed<D: Documents>(notes: &mut Notes<D>, text: &str) {
        let codes: Vec<i32> = text
            .chars()
            .map(|character| {
                if character == '\n' {
                    13
                } else {
                    character as i32
                }
            })
            .collect();
        keys(notes, &codes);
    }

    fn click<D: Documents>(notes: &mut Notes<D>, area: Rect) {
        let mut context = Context::default();
        notes.click(area.x + area.w / 2, area.y + area.h / 2, &mut context);
    }

    #[test]
    fn a_new_note_is_typed_and_saved_to_a_fresh_file() {
        let shelf = Shelf::with(&[("/note-1.txt", b"taken")]);
        let mut notes = Notes::new(shelf.clone());
        assert_eq!(notes.path(), "/note-2.txt");
        assert_eq!(notes.title(), "Notes — note-2.txt");
        assert!(!notes.is_modified());
        assert_eq!(notes.text(), "");

        typed(&mut notes, "Milk\nEggs");
        assert_eq!(notes.text(), "Milk\nEggs\n");
        assert_eq!(notes.cursor(), (1, 4));
        assert!(notes.is_modified());
        assert_eq!(notes.title(), "Notes — note-2.txt •");
        // Nothing is written until it is saved.
        assert_eq!(shelf.get("/note-2.txt"), None);

        keys(&mut notes, &[19]);
        assert_eq!(shelf.get("/note-2.txt").unwrap(), b"Milk\nEggs\n");
        assert!(!notes.is_modified());
        assert_eq!(notes.title(), "Notes — note-2.txt");
        assert_eq!(notes.notice, Notice::Saved);
        // The next change says so again.
        typed(&mut notes, "!");
        assert!(notes.is_modified());
        assert_eq!(notes.notice, Notice::Nothing);
    }

    #[test]
    fn a_file_is_opened_changed_and_written_back_as_it_was_laid_out() {
        // Windows line ends and no line end after the last line: both
        // are kept.
        let shelf = Shelf::with(&[("/a.txt", b"one\r\ntwo\r\nthree")]);
        let mut notes = Notes::open(shelf.clone(), "/a.txt");
        assert_eq!(notes.text(), "one\r\ntwo\r\nthree");
        assert!(!notes.is_modified() && !notes.is_read_only());
        keys(&mut notes, &[DOWN, END]);
        typed(&mut notes, "!");
        notes.save();
        assert_eq!(shelf.get("/a.txt").unwrap(), b"one\r\ntwo!\r\nthree");

        // Unix line ends, with one at the end.
        let shelf = Shelf::with(&[("/b.txt", "héllo\nwörld\n".as_bytes())]);
        let mut notes = Notes::open(shelf.clone(), "/b.txt");
        assert_eq!(notes.lines.len(), 2);
        keys(&mut notes, &[END]);
        assert_eq!(notes.cursor(), (0, 5), "columns count characters");
        keys(&mut notes, &[127]);
        notes.save();
        assert_eq!(shelf.get("/b.txt").unwrap(), "héll\nwörld\n".as_bytes());
        // Saving without a change writes the same bytes.
        let mut notes = Notes::open(shelf.clone(), "/b.txt");
        notes.save();
        assert_eq!(shelf.get("/b.txt").unwrap(), "héll\nwörld\n".as_bytes());
    }

    #[test]
    fn enter_backspace_and_delete_split_and_join_lines() {
        let mut notes = Notes::new(Shelf::default());
        typed(&mut notes, "abcdef");
        keys(&mut notes, &[LEFT, LEFT, LEFT, 13]);
        assert_eq!(notes.text(), "abc\ndef\n");
        assert_eq!(notes.cursor(), (1, 0));
        // Backspace at the start of a line joins it to the one before.
        keys(&mut notes, &[127]);
        assert_eq!(notes.text(), "abcdef\n");
        assert_eq!(notes.cursor(), (0, 3));
        // Delete at the end of a line joins the next one to it.
        keys(&mut notes, &[13, UP, END, DELETE]);
        assert_eq!(notes.text(), "abcdef\n");
        assert_eq!(notes.cursor(), (0, 3));
        // Delete and Backspace inside a line.
        keys(&mut notes, &[DELETE, 127]);
        assert_eq!(notes.text(), "abef\n");
        // At the very start and the very end they do nothing, and do
        // not count as changes.
        let shelf = Shelf::with(&[("/x", b"x")]);
        let mut notes = Notes::open(shelf, "/x");
        keys(&mut notes, &[127, END, DELETE]);
        assert_eq!(notes.text(), "x");
        assert!(!notes.is_modified());
    }

    #[test]
    fn a_new_line_starts_with_the_indent_of_the_one_before_and_tab_moves_to_a_stop() {
        let mut notes = Notes::new(Shelf::default());
        keys(&mut notes, &[9]);
        typed(&mut notes, "if x:");
        assert_eq!(notes.text(), "    if x:\n");
        keys(&mut notes, &[13]);
        assert_eq!(notes.cursor(), (1, 4));
        typed(&mut notes, "ab");
        // From column 6, Tab goes to 8.
        keys(&mut notes, &[9]);
        assert_eq!(notes.cursor(), (1, 8));
        assert_eq!(notes.text(), "    if x:\n    ab  \n");
    }

    #[test]
    fn the_cursor_moves_through_lines_of_different_lengths() {
        let shelf = Shelf::with(&[("/p", b"a long first line\nab\n\nthe fourth line\n")]);
        let mut notes = Notes::open(shelf, "/p");
        keys(&mut notes, &[END]);
        assert_eq!(notes.cursor(), (0, 17));
        // Down through the short lines and into a long one: the column
        // comes back.
        keys(&mut notes, &[DOWN]);
        assert_eq!(notes.cursor(), (1, 2));
        keys(&mut notes, &[DOWN]);
        assert_eq!(notes.cursor(), (2, 0));
        keys(&mut notes, &[DOWN]);
        assert_eq!(notes.cursor(), (3, 15));
        // Not past the last line.
        keys(&mut notes, &[DOWN, DOWN]);
        assert_eq!(notes.cursor(), (3, 15));
        // Right at the end of a line goes to the start of the next,
        // left at the start to the end of the one before.
        keys(&mut notes, &[UP, UP, END, RIGHT]);
        assert_eq!(notes.cursor(), (2, 0));
        keys(&mut notes, &[LEFT]);
        assert_eq!(notes.cursor(), (1, 2));
        keys(&mut notes, &[HOME, LEFT]);
        assert_eq!(notes.cursor(), (0, 17));
        keys(&mut notes, &[HOME, LEFT, UP]);
        assert_eq!(notes.cursor(), (0, 0));
        // Moving is not changing.
        assert!(!notes.is_modified());
    }

    #[test]
    fn a_click_puts_the_cursor_at_the_nearest_gap() {
        let shelf = Shelf::with(&[("/p", b"hello\nhi\n")]);
        let mut notes = Notes::open(shelf, "/p");
        let cell = notes.cell_frame(0, 3).unwrap();
        // The left half of a character: before it; the right half:
        // after it.
        let mut context = Context::default();
        notes.click(cell.x + 1, cell.y + 5, &mut context);
        assert_eq!(notes.cursor(), (0, 3));
        notes.click(cell.x + CHAR_WIDTH - 2, cell.y + 5, &mut context);
        assert_eq!(notes.cursor(), (0, 4));
        // Beyond the end of a line: its end. Below the last line: the
        // last line.
        notes.click(cell.x + 300, cell.y + LINE + 5, &mut context);
        assert_eq!(notes.cursor(), (1, 2));
        notes.click(cell.x, cell.y + 10 * LINE, &mut context);
        assert_eq!(notes.cursor(), (1, 2));
        // In the line numbers: the start of the line.
        notes.click(3, cell.y + 5, &mut context);
        assert_eq!(notes.cursor(), (0, 0));
    }

    #[test]
    fn long_texts_scroll_to_keep_the_cursor_on_the_screen() {
        let text: String = (1..=200).map(|number| format!("line {number}\n")).collect();
        let shelf = Shelf::with(&[("/long", text.as_bytes())]);
        let mut notes = Notes::open(shelf, "/long");
        let rows = notes.visible_rows();
        assert!(notes.cell_frame(rows, 0).is_none());
        keys(&mut notes, &[PAGE_DOWN, PAGE_DOWN, PAGE_DOWN]);
        let (line, _) = notes.cursor();
        assert_eq!(line, 3 * (rows - 1));
        let cell = notes.cell_frame(line, 0).expect("the cursor is shown");
        assert!(cell.bottom() <= SIZE.1 - STATUS);
        assert!(notes.cell_frame(0, 0).is_none());
        // Sideways too.
        let columns = notes.visible_columns();
        typed(&mut notes, &"x".repeat(columns + 20));
        let (line, column) = notes.cursor();
        let cell = notes.cell_frame(line, column).expect("the cursor is shown");
        assert!(cell.right() <= SIZE.0);
        assert!(notes.cell_frame(line, 0).is_none());
        keys(&mut notes, &[HOME]);
        assert!(notes.cell_frame(line, 0).is_some());
    }

    #[test]
    fn the_wheel_scrolls_without_moving_the_cursor() {
        let text: String = (1..=200).map(|number| format!("line {number}\n")).collect();
        let shelf = Shelf::with(&[("/long", text.as_bytes())]);
        let mut notes = Notes::open(shelf, "/long");
        assert!(notes.scroll(30));
        assert_eq!(notes.cursor(), (0, 0));
        assert!(notes.cell_frame(0, 0).is_none());
        // Drawing does not pull the text back to the cursor.
        let theme = Theme::default();
        let mut surface = Surface::new(SIZE.0 as usize, SIZE.1 as usize, theme.window);
        notes.draw(&mut surface.canvas(), SIZE.0, SIZE.1, &theme);
        assert!(notes.cell_frame(30, 0).is_some());
        // A click puts the cursor where it lands; a key brings the
        // cursor's line back into view.
        let cell = notes.cell_frame(32, 2).unwrap();
        let mut context = Context::default();
        notes.click(cell.x + 1, cell.y + 3, &mut context);
        assert_eq!(notes.cursor(), (32, 2));
        assert!(notes.scroll(-1000));
        assert!(!notes.scroll(-1));
        assert!(notes.cell_frame(32, 2).is_none());
        keys(&mut notes, &[LEFT]);
        assert!(notes.cell_frame(32, 1).is_some());
        // The end is the last screenful.
        assert!(notes.scroll(100_000));
        assert!(notes.cell_frame(199, 0).is_some());
        assert!(!notes.scroll(1));
    }

    #[test]
    fn what_cannot_be_written_back_safely_cannot_be_changed() {
        let mut big = Vec::new();
        while big.len() <= EDIT_LIMIT {
            big.extend_from_slice("größer\n".as_bytes());
        }
        let shelf = Shelf::with(&[
            ("/binary", &[0x7F, b'E', b'L', b'F', 0, 1, 2][..]),
            ("/latin1", &[b'c', b'a', b'f', 0xE9][..]),
            ("/big", &big[..]),
        ]);
        for (path, said) in [
            ("/binary", "Not a text file"),
            ("/latin1", "Not a text file"),
            ("/big", "Too large"),
            ("/missing", "Cannot be read"),
        ] {
            let mut notes = Notes::open(shelf.clone(), path);
            assert!(notes.is_read_only(), "{path}");
            assert!(
                notes.read_only.as_ref().unwrap().starts_with(said),
                "{path}"
            );
            let before = notes.text();
            typed(&mut notes, "x\n");
            keys(&mut notes, &[127, DELETE, 9, 19]);
            click(&mut notes, Notes::<Shelf>::save_button(SIZE.0));
            assert_eq!(notes.text(), before, "{path}");
            assert!(!notes.is_modified(), "{path}");
        }
        // Nothing was written.
        assert_eq!(shelf.get("/big").unwrap(), big);
        assert_eq!(shelf.get("/missing"), None);
        // The large file is shown up to the limit, cut between
        // characters, and can be moved about in.
        let mut notes = Notes::open(shelf, "/big");
        assert!(notes.lines.len() > 1000);
        keys(&mut notes, &[DOWN, END]);
        assert_eq!(notes.cursor(), (1, 6));
    }

    #[test]
    fn closing_with_unsaved_changes_is_refused_once() {
        let shelf = Shelf::with(&[("/a", b"old\n")]);
        // Nothing changed: it closes.
        let mut notes = Notes::open(shelf.clone(), "/a");
        assert!(notes.closing());
        // Changed: refused, and said; asked again straight away, it
        // closes (and nothing was written).
        typed(&mut notes, "x");
        assert!(!notes.closing());
        assert_eq!(notes.notice, Notice::Unsaved);
        assert!(notes.closing());
        assert_eq!(shelf.get("/a").unwrap(), b"old\n");
        // Anything done in between, and it asks again.
        let mut notes = Notes::open(shelf.clone(), "/a");
        typed(&mut notes, "x");
        assert!(!notes.closing());
        keys(&mut notes, &[LEFT]);
        assert_eq!(notes.notice, Notice::Nothing);
        assert!(!notes.closing());
        typed(&mut notes, "y");
        assert!(!notes.closing());
        // Saved: it closes.
        keys(&mut notes, &[19]);
        assert!(notes.closing());
        assert_eq!(shelf.get("/a").unwrap(), b"yxold\n");
        // What cannot be changed has nothing to lose.
        let mut notes = Notes::open(shelf, "/missing");
        typed(&mut notes, "x");
        assert!(notes.closing());
    }

    #[test]
    fn a_failed_save_keeps_the_changes_and_says_why() {
        let shelf = Shelf::with(&[("/locked/a", b"old\n")]);
        let mut notes = Notes::open(shelf.clone(), "/locked/a");
        typed(&mut notes, "new ");
        click(&mut notes, Notes::<Shelf>::save_button(SIZE.0));
        assert!(notes.is_modified());
        assert_eq!(notes.notice, Notice::Failed("read-only filesystem".into()));
        assert_eq!(shelf.get("/locked/a").unwrap(), b"old\n");
        assert_eq!(notes.text(), "new old\n");
    }

    #[test]
    fn it_draws_at_any_size_in_every_theme_and_shows_the_cursor() {
        let shelf = Shelf::with(&[("/p", b"hello\n\tworld\n")]);
        let mut notes = Notes::open(shelf, "/p");
        typed(&mut notes, "x");
        for mode in [Mode::Light, Mode::Dark] {
            let theme = Theme::new(mode, Accent::Blue);
            for (width, height) in [SIZE, (1280, 700), (200, 120), (60, 40)] {
                let mut surface = Surface::new(width as usize, height as usize, theme.window);
                notes.draw(&mut surface.canvas(), width, height, &theme);
                if (width, height) == SIZE {
                    // The cursor: a bar in the accent colour before the
                    // second character of the first line.
                    let cell = notes.cell_frame(0, 1).unwrap();
                    assert_eq!(surface.pixel(cell.x - 1, cell.y + 4), Some(theme.accent));
                    // The save button is filled with the accent while
                    // there is something to save.
                    let button = Notes::<Shelf>::save_button(width);
                    assert_eq!(
                        surface.pixel(button.x + 6, button.y + button.h / 2),
                        Some(theme.accent)
                    );
                }
            }
        }
        // A read-only note has neither.
        let mut notes = Notes::open(Shelf::default(), "/missing");
        let theme = Theme::default();
        let mut surface = Surface::new(SIZE.0 as usize, SIZE.1 as usize, theme.window);
        notes.draw(&mut surface.canvas(), SIZE.0, SIZE.1, &theme);
        let accent = surface
            .pixels()
            .iter()
            .filter(|&&pixel| pixel == theme.accent)
            .count();
        assert_eq!(accent, 0);
    }
}
