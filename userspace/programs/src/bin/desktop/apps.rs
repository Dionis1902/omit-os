// SPDX-License-Identifier: GPL-2.0-only
//! The windows of the desktop.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use omit_gfx::desktop::{App, Context, theme};
use omit_gfx::geometry::Rect;
use omit_gfx::surface::{CHAR_WIDTH, Canvas, TEXT_HEIGHT};
use omit_rt::abi::{Errno, key};

const MARGIN: i32 = 8;
const LINE: i32 = TEXT_HEIGHT + 2;
const HIGHLIGHT: u32 = 0xC8_DC_F8;
const DIM: u32 = 0x60_60_68;
const ERROR: u32 = 0xB0_20_20;

const ENTER: i32 = 13;
const BACKSPACE: i32 = 127;
const CONTROL_C: i32 = 3;

/// Number of text rows that fit in `height` pixels below `top`.
fn rows(top: i32, height: i32) -> usize {
    ((height - top - MARGIN) / LINE).max(1) as usize
}

/// Number of characters that fit in `width` pixels.
fn columns(width: i32) -> usize {
    ((width - 2 * MARGIN) / CHAR_WIDTH).max(1) as usize
}

/// Makes arbitrary bytes displayable: tabs become spaces, anything else
/// that is not printable ASCII or valid UTF-8 becomes `.`.
fn printable(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .map(|c| match c {
            '\t' => ' ',
            c if c.is_control() || c == char::REPLACEMENT_CHARACTER => '.',
            c => c,
        })
        .collect()
}

// ---------------------------------------------------------------------

/// Information about the system and how to use the desktop.
pub struct About {
    screen: (usize, usize),
    seconds: u64,
}

impl About {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            screen: (width, height),
            seconds: omit_rt::uptime_ms() / 1000,
        }
    }
}

impl App for About {
    fn title(&self) -> &str {
        "About Omit OS"
    }

    fn draw(&mut self, canvas: &mut Canvas<'_>, _width: i32, _height: i32) {
        let lines = [
            format!("Omit OS {}", env!("CARGO_PKG_VERSION")),
            "Everything you need. Nothing you don't.".into(),
            String::new(),
            format!(
                "Screen {} x {}, running for {} s",
                self.screen.0, self.screen.1, self.seconds
            ),
            String::new(),
            "Drag a title bar to move a window, click to raise".into(),
            "it. The task bar opens more windows. Exit or F12".into(),
            "returns to the text console.".into(),
        ];
        for (index, line) in lines.iter().enumerate() {
            canvas.text(MARGIN, MARGIN + index as i32 * LINE, line, theme::TEXT);
        }
    }

    fn tick(&mut self, _: &mut Context) -> bool {
        let seconds = omit_rt::uptime_ms() / 1000;
        core::mem::replace(&mut self.seconds, seconds) != seconds
    }
}

// ---------------------------------------------------------------------

struct Entry {
    name: String,
    is_dir: bool,
}

/// A file browser: click a directory to enter it, a file to view it.
pub struct Files {
    path: String,
    title: String,
    entries: Vec<Entry>,
    error: Option<Errno>,
    selected: usize,
    first_row: usize,
    /// Rows that fit, as of the last draw.
    visible_rows: usize,
}

impl Files {
    const LIST_TOP: i32 = MARGIN + LINE + 4;

    pub fn new(path: &str) -> Self {
        let mut files = Self {
            path: String::new(),
            title: String::new(),
            entries: Vec::new(),
            error: None,
            selected: 0,
            first_row: 0,
            visible_rows: 1,
        };
        files.enter(path.into());
        files
    }

    fn enter(&mut self, path: String) {
        self.entries.clear();
        self.error = None;
        if path != "/" {
            self.entries.push(Entry {
                name: "..".into(),
                is_dir: true,
            });
        }
        match omit_rt::read_dir(&path) {
            Ok(mut listing) => {
                // Directories first, each group by name.
                listing.sort_by(|a, b| (!a.is_dir, &a.name).cmp(&(!b.is_dir, &b.name)));
                self.entries.extend(listing.into_iter().map(|entry| Entry {
                    name: entry.name,
                    is_dir: entry.is_dir,
                }));
            }
            Err(error) => self.error = Some(error),
        }
        self.title = format!("Files - {path}");
        self.path = path;
        self.selected = 0;
        self.first_row = 0;
    }

    fn child(&self, name: &str) -> String {
        if name == ".." {
            return match self.path.rfind('/') {
                Some(0) | None => "/".into(),
                Some(index) => self.path[..index].into(),
            };
        }
        if self.path == "/" {
            format!("/{name}")
        } else {
            format!("{}/{name}", self.path)
        }
    }

    fn activate(&mut self, context: &mut Context) {
        let Some(entry) = self.entries.get(self.selected) else {
            return;
        };
        let target = self.child(&entry.name);
        if entry.is_dir {
            self.enter(target);
        } else {
            context.open(Box::new(Viewer::new(&target)), 600, 380);
        }
    }

    fn keep_selection_visible(&mut self) {
        if self.selected < self.first_row {
            self.first_row = self.selected;
        } else if self.selected >= self.first_row + self.visible_rows {
            self.first_row = self.selected + 1 - self.visible_rows;
        }
    }
}

impl App for Files {
    fn title(&self) -> &str {
        &self.title
    }

    fn draw(&mut self, canvas: &mut Canvas<'_>, width: i32, height: i32) {
        canvas.text(MARGIN, MARGIN, &self.path, DIM);
        canvas.fill(
            Rect::new(MARGIN, Self::LIST_TOP - 3, width - 2 * MARGIN, 1),
            DIM,
        );
        if let Some(error) = self.error {
            canvas.text(
                MARGIN,
                Self::LIST_TOP,
                &format!("Cannot list: {error}"),
                ERROR,
            );
        }
        self.visible_rows = rows(Self::LIST_TOP, height);
        self.keep_selection_visible();
        let shown = self.entries.iter().enumerate().skip(self.first_row);
        for (row, (index, entry)) in shown.take(self.visible_rows).enumerate() {
            let y = Self::LIST_TOP + row as i32 * LINE;
            if index == self.selected {
                canvas.fill(
                    Rect::new(MARGIN - 2, y - 1, width - 2 * MARGIN + 4, LINE),
                    HIGHLIGHT,
                );
            }
            let kind = if entry.is_dir { "[dir] " } else { "      " };
            let x = canvas.text(MARGIN, y, kind, DIM);
            canvas.text(x, y, &entry.name, theme::TEXT);
        }
    }

    fn click(&mut self, _x: i32, y: i32, context: &mut Context) {
        if y < Self::LIST_TOP {
            return;
        }
        let index = self.first_row + ((y - Self::LIST_TOP) / LINE) as usize;
        if index < self.entries.len() {
            self.selected = index;
            self.activate(context);
        }
    }

    fn key(&mut self, code: i32, context: &mut Context) {
        let last = self.entries.len().saturating_sub(1);
        match code {
            key::UP => self.selected = self.selected.saturating_sub(1),
            key::DOWN => self.selected = (self.selected + 1).min(last),
            key::HOME => self.selected = 0,
            key::END => self.selected = last,
            key::PAGE_UP => self.selected = self.selected.saturating_sub(self.visible_rows),
            key::PAGE_DOWN => self.selected = (self.selected + self.visible_rows).min(last),
            ENTER => self.activate(context),
            BACKSPACE if self.path != "/" => {
                let parent = self.child("..");
                self.enter(parent);
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------

/// Shows a text file. Arrow keys, Page Up/Down, Home and End scroll.
pub struct Viewer {
    title: String,
    lines: Vec<String>,
    note: Option<String>,
    first_row: usize,
    visible_rows: usize,
}

impl Viewer {
    /// Larger files are shown truncated.
    const LIMIT: usize = 256 * 1024;

    pub fn new(path: &str) -> Self {
        let name = path.rsplit('/').next().unwrap_or(path);
        let mut viewer = Self {
            title: format!("{name} - Viewer"),
            lines: Vec::new(),
            note: None,
            first_row: 0,
            visible_rows: 1,
        };
        match omit_rt::read_file(path) {
            Err(error) => viewer.note = Some(format!("Cannot read {path}: {error}")),
            Ok(data) if data.iter().take(4096).any(|&byte| byte == 0) => {
                viewer.note = Some(format!("{path}: binary file, {} bytes", data.len()));
            }
            Ok(data) => {
                let shown = &data[..data.len().min(Self::LIMIT)];
                viewer.lines = shown
                    .split(|&byte| byte == b'\n')
                    .map(|line| printable(line.strip_suffix(b"\r").unwrap_or(line)))
                    .collect();
                if data.len() > shown.len() {
                    viewer.note = Some(format!("(first {} KiB shown)", Self::LIMIT / 1024));
                }
            }
        }
        viewer
    }

    fn last_first_row(&self) -> usize {
        self.lines.len().saturating_sub(self.visible_rows)
    }
}

impl App for Viewer {
    fn title(&self) -> &str {
        &self.title
    }

    fn draw(&mut self, canvas: &mut Canvas<'_>, _width: i32, height: i32) {
        let mut top = MARGIN;
        if let Some(note) = &self.note {
            canvas.text(MARGIN, top, note, DIM);
            top += LINE;
        }
        self.visible_rows = rows(top, height);
        self.first_row = self.first_row.min(self.last_first_row());
        let shown = self
            .lines
            .iter()
            .skip(self.first_row)
            .take(self.visible_rows);
        for (row, line) in shown.enumerate() {
            canvas.text(MARGIN, top + row as i32 * LINE, line, theme::TEXT);
        }
    }

    fn key(&mut self, code: i32, context: &mut Context) {
        let page = self.visible_rows.saturating_sub(1).max(1);
        let target = match code {
            key::UP => self.first_row.saturating_sub(1),
            key::DOWN => self.first_row + 1,
            key::PAGE_UP => self.first_row.saturating_sub(page),
            key::PAGE_DOWN | 0x20 => self.first_row + page,
            key::HOME => 0,
            key::END => usize::MAX,
            27 => {
                context.close();
                return;
            }
            _ => return,
        };
        self.first_row = target.min(self.last_first_row());
    }
}

// ---------------------------------------------------------------------

struct Child {
    pid: u64,
    name: String,
    /// Reading end of the pipe the program's output and errors arrive on.
    output: u64,
    /// Writing end of the pipe to the program's standard input; `None`
    /// once the input has been ended with Ctrl+D.
    input: Option<u64>,
}

/// A line-mode terminal: runs programs, shows what they print, and passes
/// typed lines to them.
///
/// A command is a program name (looked up in `/bin` unless it contains a
/// `/`) followed by arguments separated by spaces. The program's standard
/// output and standard error arrive through a pipe and are shown as they
/// are produced. While a program runs, a typed line is sent to its
/// standard input when Enter is pressed; Ctrl+D ends its input and Ctrl+C
/// stops it. There are no escape sequences, no cursor addressing and no
/// shell syntax (pipes, redirection, built-in commands): for those there
/// is the text console.
pub struct Run {
    lines: Vec<String>,
    /// Whether the last line is still open (output did not end in a
    /// newline).
    open_line: bool,
    input: String,
    child: Option<Child>,
}

impl Run {
    const SCROLLBACK: usize = 400;

    pub fn new() -> Self {
        let mut run = Self {
            lines: Vec::new(),
            open_line: false,
            input: String::new(),
            child: None,
        };
        run.line("Type a program name and press Enter, for example:");
        run.line("  hello      ls /bin      winhello.exe      echo some text");
        run.line("While a program runs, lines you type go to it;");
        run.line("Ctrl+D ends its input, Ctrl+C stops it.");
        run
    }

    fn line(&mut self, text: &str) {
        self.lines.push(text.into());
        self.open_line = false;
        if self.lines.len() > Self::SCROLLBACK {
            let extra = self.lines.len() - Self::SCROLLBACK;
            self.lines.drain(..extra);
        }
    }

    /// Appends program output, which may end in the middle of a line.
    fn output(&mut self, bytes: &[u8]) {
        for (index, piece) in bytes.split(|&byte| byte == b'\n').enumerate() {
            let text = printable(piece.strip_suffix(b"\r").unwrap_or(piece));
            if index == 0 && self.open_line {
                if let Some(last) = self.lines.last_mut() {
                    last.push_str(&text);
                }
            } else {
                self.line(&text);
            }
            self.open_line = true;
        }
        // A trailing newline produced an empty final piece: drop it and
        // start the next output on a fresh line.
        if bytes.last() == Some(&b'\n') {
            self.lines.pop();
            self.open_line = false;
        }
    }

    fn start(&mut self, command: &str) {
        let words: Vec<&str> = command.split(' ').filter(|word| !word.is_empty()).collect();
        let Some(&name) = words.first() else {
            return;
        };
        let path = if name.contains('/') {
            String::from(name)
        } else {
            format!("/bin/{name}")
        };
        let started = (|| {
            let (output, output_write) = omit_rt::pipe()?;
            let (input_read, input) = match omit_rt::pipe() {
                Ok(ends) => ends,
                Err(error) => {
                    let _ = omit_rt::close(output);
                    let _ = omit_rt::close(output_write);
                    return Err(error);
                }
            };
            let pid = omit_rt::spawn_with_stdio(
                &path,
                &words,
                Some(input_read),
                Some(output_write),
                Some(output_write),
            );
            // The program has its own copies of its ends. Keeping ours
            // would mean never seeing the end of its output.
            let _ = omit_rt::close(input_read);
            let _ = omit_rt::close(output_write);
            match pid {
                Ok(pid) => Ok(Child {
                    pid,
                    name: name.into(),
                    output,
                    input: Some(input),
                }),
                Err(error) => {
                    let _ = omit_rt::close(output);
                    let _ = omit_rt::close(input);
                    Err(error)
                }
            }
        })();
        match started {
            Ok(child) => self.child = Some(child),
            Err(Errno::NotFound) => self.line(&format!("{name}: program not found")),
            Err(error) => self.line(&format!("{name}: {error}")),
        }
    }

    /// Shows output the program has produced since the last call, without
    /// waiting for more. Returns whether anything arrived.
    fn collect_output(&mut self) -> bool {
        let Some(output) = self.child.as_ref().map(|child| child.output) else {
            return false;
        };
        let mut chunk = [0u8; 4096];
        let mut changed = false;
        // Bounded, so that a program printing without pause cannot keep
        // the desktop from handling input.
        for _ in 0..16 {
            match omit_rt::read_no_wait(output, &mut chunk) {
                Ok(Some(count)) if count > 0 => {
                    self.output(&chunk[..count]);
                    changed = true;
                }
                // Nothing right now, end of output, or an error.
                _ => break,
            }
        }
        changed
    }

    fn end_input(&mut self) {
        if let Some(input) = self.child.as_mut().and_then(|child| child.input.take()) {
            let _ = omit_rt::close(input);
        }
    }
}

impl App for Run {
    fn title(&self) -> &str {
        "Run"
    }

    fn draw(&mut self, canvas: &mut Canvas<'_>, width: i32, height: i32) {
        let columns = columns(width);
        let prompt_y = height - MARGIN - LINE;
        canvas.fill(Rect::new(MARGIN, prompt_y - 4, width - 2 * MARGIN, 1), DIM);
        // Show the end of a line that is too long to fit.
        let visible: String = {
            let count = self.input.chars().count();
            self.input
                .chars()
                .skip(count.saturating_sub(columns.saturating_sub(12)))
                .collect()
        };
        let prompt = match &self.child {
            Some(child) if child.input.is_some() => format!("{}< {visible}_", child.name),
            Some(child) => format!("{} (input ended)", child.name),
            None => format!("> {visible}_"),
        };
        canvas.text(MARGIN, prompt_y, &prompt, theme::TEXT);

        // Wrap the lines to the window and show the last ones that fit.
        let available = ((prompt_y - 4 - MARGIN) / LINE).max(1) as usize;
        let mut wrapped: Vec<&str> = Vec::new();
        for line in self.lines.iter().rev() {
            let start = wrapped.len();
            if line.is_empty() {
                wrapped.push("");
            }
            let mut rest = line.as_str();
            while !rest.is_empty() {
                let split = rest
                    .char_indices()
                    .nth(columns)
                    .map_or(rest.len(), |(at, _)| at);
                wrapped.push(&rest[..split]);
                rest = &rest[split..];
            }
            // The pieces of one line were pushed first-to-last, but the
            // list is being built bottom-up.
            wrapped[start..].reverse();
            if wrapped.len() >= available {
                break;
            }
        }
        wrapped.truncate(available);
        for (row, text) in wrapped.iter().rev().enumerate() {
            canvas.text(MARGIN, MARGIN + row as i32 * LINE, text, theme::TEXT);
        }
    }

    fn key(&mut self, code: i32, _: &mut Context) {
        const CONTROL_D: i32 = 4;
        match code {
            CONTROL_C => {
                if let Some(child) = &self.child {
                    let _ = omit_rt::kill(child.pid);
                }
            }
            CONTROL_D => self.end_input(),
            ENTER => {
                let mut text = core::mem::take(&mut self.input);
                match self.child.as_ref().map(|child| child.input) {
                    // A program is running: the line is its input.
                    Some(Some(input)) => {
                        self.line(&text);
                        text.push('\n');
                        if omit_rt::write_all(input, text.as_bytes()).is_err() {
                            // It no longer reads its input.
                            self.end_input();
                        }
                    }
                    Some(None) => {}
                    None => {
                        self.line(&format!("> {text}"));
                        self.start(&text);
                    }
                }
            }
            BACKSPACE => {
                self.input.pop();
            }
            0x20..=0x7E if self.input.len() < 200 => self.input.push(code as u8 as char),
            _ => {}
        }
    }

    fn tick(&mut self, _: &mut Context) -> bool {
        let Some(pid) = self.child.as_ref().map(|child| child.pid) else {
            return false;
        };
        let mut changed = self.collect_output();
        match omit_rt::try_wait(pid) {
            Ok(None) => {}
            finished => {
                // Whatever it wrote before it ended.
                while self.collect_output() {}
                self.end_input();
                if let Some(child) = self.child.take() {
                    let _ = omit_rt::close(child.output);
                    match finished {
                        Ok(Some(0)) => {}
                        Ok(Some(status)) => {
                            self.line(&format!("[{} exited with status {status}]", child.name));
                        }
                        _ => self.line(&format!("[{}: lost track of the program]", child.name)),
                    }
                }
                self.input.clear();
                changed = true;
            }
        }
        changed
    }
}
