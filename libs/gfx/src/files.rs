// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Files window: a file manager.
//!
//! A toolbar with back and up buttons and the path as a row of names
//! that can each be clicked; a sidebar of places; and the list of what
//! the current folder holds, with sizes and kinds. A click selects a
//! row; a click on the selected row, or Enter, opens it — a folder in
//! place, a file through whatever the program supplied for that.
//!
//! Where the files come from is the program's business ([`Storage`]),
//! which keeps this window testable on the build machine.
//!
//! Against the design system's Files this is the frame without most of
//! the furniture: no tabs, search, preview pane, context menu, renaming,
//! deleting or Recycle Bin, and the list cannot be sorted or scrolled
//! with the mouse.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::art;
use crate::desktop::{App, Context};
use crate::font::{MEDIUM_13, REGULAR_12, REGULAR_13};
use crate::geometry::{Point, Rect};
use crate::icon::icon;
use crate::surface::Canvas;
use crate::theme::Theme;

/// A good size for the window's content area.
pub const SIZE: (i32, i32) = (680, 420);

const TOOLBAR: i32 = 44;
const SIDEBAR: i32 = 176;
const HEADER: i32 = 30;
const ROW: i32 = 30;
const STATUS: i32 = 28;
const BUTTON: (i32, i32) = (30, 28);

/// Something in a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Its name, without the folders it is in.
    pub name: String,
    /// Whether it is a folder.
    pub is_dir: bool,
    /// Its size in bytes (not meaningful for folders).
    pub size: u64,
}

/// Where the files are.
pub trait Storage {
    /// What the folder at `path` (absolute, `/`-separated) holds, in any
    /// order; or why that cannot be told.
    fn list(&mut self, path: &str) -> Result<Vec<Entry>, String>;
}

/// A place in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// What it is called.
    pub label: String,
    /// The folder it leads to.
    pub path: String,
    /// The name of its icon (see [`crate::icon`]).
    pub icon: String,
}

/// What the path bar and the title call the root folder.
const ROOT_NAME: &str = "Computer";

/// What kind of thing an entry is, in words.
fn kind(entry: &Entry, folder: &str) -> &'static str {
    let name = entry.name.to_ascii_lowercase();
    let ends = |suffix: &str| name.ends_with(suffix);
    if entry.is_dir {
        "Folder"
    } else if ends(".exe") {
        "Windows program"
    } else if ends(".dll") {
        "Windows library"
    } else if ends(".txt") || ends(".md") || ends(".log") {
        "Text"
    } else if folder == "/bin" && !name.contains('.') {
        "Program"
    } else {
        "File"
    }
}

/// A size as people read it: bytes up to a kibibyte, then KB and MB
/// (of 1024), with one decimal below ten.
pub fn readable_size(bytes: u64) -> String {
    let scaled = |unit: u64, name: &str| {
        let tenths = bytes * 10 / unit;
        if tenths < 100 {
            format!("{}.{} {name}", tenths / 10, tenths % 10)
        } else {
            format!("{} {name}", tenths / 10)
        }
    };
    match bytes {
        1 => "1 byte".into(),
        0..1024 => format!("{bytes} bytes"),
        1024..0x10_0000 => scaled(1024, "KB"),
        _ => scaled(0x10_0000, "MB"),
    }
}

/// The folder above `path`.
fn parent(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".into(),
        Some(index) => path[..index].into(),
    }
}

fn join(folder: &str, name: &str) -> String {
    if folder == "/" {
        format!("/{name}")
    } else {
        format!("{folder}/{name}")
    }
}

/// What happens when a file is opened: called with its path.
pub type Opener = Box<dyn FnMut(&str, &mut Context)>;

/// The window's contents.
pub struct Files<S: Storage> {
    storage: S,
    places: Vec<Place>,
    open: Opener,
    path: String,
    title: String,
    entries: Vec<Entry>,
    error: Option<String>,
    selected: Option<usize>,
    first_row: usize,
    /// Folders left by going somewhere else, most recent last.
    history: Vec<String>,
    /// The size of the content area, as of the last draw.
    size: (i32, i32),
}

impl<S: Storage> Files<S> {
    /// A window showing the folder `path` of `storage`, with `places`
    /// in its sidebar; `open` is called when a file is opened.
    pub fn new(storage: S, places: Vec<Place>, path: &str, open: Opener) -> Self {
        let mut files = Self {
            storage,
            places,
            open,
            path: String::new(),
            title: String::new(),
            entries: Vec::new(),
            error: None,
            selected: None,
            first_row: 0,
            history: Vec::new(),
            size: SIZE,
        };
        files.load(path.into());
        files
    }

    /// The folder being shown.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The names listed, in the order shown.
    pub fn names(&self) -> Vec<&str> {
        self.entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect()
    }

    /// The name of the selected entry, if one is selected.
    pub fn selected(&self) -> Option<&str> {
        self.selected.map(|index| self.entries[index].name.as_str())
    }

    fn load(&mut self, path: String) {
        self.entries.clear();
        self.error = None;
        match self.storage.list(&path) {
            Ok(mut listing) => {
                // Folders first, each group by name, capitals among
                // small letters.
                listing.sort_by_cached_key(|entry| {
                    (!entry.is_dir, entry.name.to_lowercase(), entry.name.clone())
                });
                self.entries = listing;
            }
            Err(error) => self.error = Some(error),
        }
        let name = match path.rsplit('/').next() {
            Some(name) if !name.is_empty() => name,
            _ => ROOT_NAME,
        };
        self.title = format!("Files — {name}");
        self.path = path;
        self.selected = None;
        self.first_row = 0;
    }

    /// Goes to the folder `path`, remembering where it came from.
    fn enter(&mut self, path: String) {
        if path != self.path {
            self.history.push(self.path.clone());
            self.load(path);
        }
    }

    fn back(&mut self) {
        if let Some(previous) = self.history.pop() {
            self.load(previous);
        }
    }

    fn up(&mut self) {
        if self.path != "/" {
            // Going up leaves the folder just left selected, so that
            // going back down is one key.
            let left = self.path.rsplit('/').next().unwrap_or("").to_string();
            self.enter(parent(&self.path));
            self.selected = self.entries.iter().position(|entry| entry.name == left);
            self.keep_selection_visible();
        }
    }

    fn activate(&mut self, context: &mut Context) {
        let Some(entry) = self.selected.and_then(|index| self.entries.get(index)) else {
            return;
        };
        let target = join(&self.path, &entry.name);
        if entry.is_dir {
            self.enter(target);
        } else {
            (self.open)(&target, context);
        }
    }

    fn visible_rows(&self) -> usize {
        ((self.size.1 - TOOLBAR - HEADER - STATUS) / ROW).max(1) as usize
    }

    fn keep_selection_visible(&mut self) {
        let Some(selected) = self.selected else {
            return;
        };
        let rows = self.visible_rows();
        if selected < self.first_row {
            self.first_row = selected;
        } else if selected >= self.first_row + rows {
            self.first_row = selected + 1 - rows;
        }
    }

    // --- where things are, in the content area ---

    /// The back button.
    pub fn back_button(&self) -> Rect {
        Rect::new(10, (TOOLBAR - BUTTON.1) / 2, BUTTON.0, BUTTON.1)
    }

    /// The button that goes to the folder above.
    pub fn up_button(&self) -> Rect {
        self.back_button().translate(BUTTON.0 + 4, 0)
    }

    /// The names in the path bar, each with the folder it leads to.
    pub fn crumbs(&self) -> Vec<(Rect, String)> {
        let mut x = self.up_button().right() + 14;
        let mut path = String::new();
        let mut crumbs = Vec::new();
        let names =
            core::iter::once(ROOT_NAME).chain(self.path.split('/').filter(|part| !part.is_empty()));
        for (index, name) in names.enumerate() {
            if index == 0 {
                path.push('/');
            } else {
                if path.len() > 1 {
                    path.push('/');
                }
                path.push_str(name);
            }
            let width = MEDIUM_13.width(name) + 12;
            crumbs.push((Rect::new(x, (TOOLBAR - 26) / 2, width, 26), path.clone()));
            // The name, then room for the "›" between names.
            x += width + 14;
        }
        crumbs
    }

    /// The `index`-th place in the sidebar.
    pub fn place_frame(&self, index: usize) -> Option<Rect> {
        (index < self.places.len())
            .then(|| Rect::new(8, TOOLBAR + 10 + index as i32 * 32, SIDEBAR - 16, 30))
    }

    /// The `row`-th row of the list as shown (0 is the top one), if
    /// there is an entry in it.
    pub fn row_frame(&self, row: usize) -> Option<Rect> {
        (row < self.visible_rows() && self.first_row + row < self.entries.len()).then(|| {
            Rect::new(
                SIDEBAR + 8,
                TOOLBAR + HEADER + row as i32 * ROW,
                self.size.0 - SIDEBAR - 16,
                ROW,
            )
        })
    }
}

impl<S: Storage> App for Files<S> {
    fn title(&self) -> &str {
        &self.title
    }

    fn icon(&self) -> &str {
        "files"
    }

    fn draw(&mut self, canvas: &mut Canvas<'_>, width: i32, height: i32, theme: &Theme) {
        self.size = (width, height);
        self.keep_selection_visible();
        let line = |canvas: &mut Canvas<'_>, area: Rect| {
            canvas.tint(area, theme.separator.colour, theme.separator.alpha);
        };
        let glyph = |canvas: &mut Canvas<'_>, area: Rect, name: &str, size: i32, colour| {
            if let Some(mask) = icon(name, size) {
                canvas.mask(
                    area.x + (area.w - size) / 2,
                    area.y + (area.h - size) / 2,
                    mask,
                    colour,
                    255,
                );
            }
        };

        // The toolbar: back, up, and the path.
        canvas.fill(Rect::new(0, 0, width, TOOLBAR), theme.chrome);
        line(canvas, Rect::new(0, TOOLBAR - 1, width, 1));
        let dim = |enabled: bool| {
            if enabled {
                theme.text_2
            } else {
                theme.separator.over(theme.chrome)
            }
        };
        glyph(
            canvas,
            self.back_button(),
            "back",
            16,
            dim(!self.history.is_empty()),
        );
        glyph(canvas, self.up_button(), "up", 16, dim(self.path != "/"));
        let crumbs = self.crumbs();
        {
            let mut bar = canvas.clipped(Rect::new(0, 0, width - 8, TOOLBAR));
            for (index, (area, _)) in crumbs.iter().enumerate() {
                let last = index + 1 == crumbs.len();
                let name = if index == 0 {
                    ROOT_NAME
                } else {
                    self.path
                        .split('/')
                        .filter(|part| !part.is_empty())
                        .nth(index - 1)
                        .unwrap_or("")
                };
                let y = area.y + (area.h - MEDIUM_13.height()) / 2;
                if last {
                    bar.write(area.x + 6, y, name, MEDIUM_13, theme.text);
                } else {
                    bar.write(area.x + 6, y, name, REGULAR_13, theme.text_2);
                    bar.write(area.right() + 4, y, "›", REGULAR_13, theme.text_3);
                }
            }
        }

        // The sidebar: places.
        canvas.fill(
            Rect::new(0, TOOLBAR, SIDEBAR, height - TOOLBAR),
            theme.chrome,
        );
        line(canvas, Rect::new(SIDEBAR - 1, TOOLBAR, 1, height - TOOLBAR));
        for (index, place) in self.places.iter().enumerate() {
            let Some(area) = self.place_frame(index) else {
                continue;
            };
            let here = place.path == self.path;
            if here {
                canvas.round_rect(area, 7, theme.fill_2.colour, theme.fill_2.alpha);
            }
            glyph(
                canvas,
                Rect::new(area.x + 8, area.y + 5, 20, 20),
                &place.icon,
                20,
                theme.accent_text,
            );
            let font = if here { MEDIUM_13 } else { REGULAR_13 };
            canvas.write(
                area.x + 38,
                area.y + (area.h - font.height()) / 2,
                &place.label,
                font,
                theme.text,
            );
        }

        // The list: column titles, then the rows.
        let list = Rect::new(SIDEBAR, TOOLBAR, width - SIDEBAR, height - TOOLBAR - STATUS);
        let size_right = list.right() - 150;
        let kind_left = list.right() - 132;
        let title_y = list.y + (HEADER - REGULAR_12.height()) / 2;
        canvas.write(list.x + 48, title_y, "Name", REGULAR_12, theme.text_3);
        canvas.write(
            size_right - REGULAR_12.width("Size"),
            title_y,
            "Size",
            REGULAR_12,
            theme.text_3,
        );
        canvas.write(kind_left, title_y, "Kind", REGULAR_12, theme.text_3);
        line(
            canvas,
            Rect::new(list.x + 8, list.y + HEADER - 1, list.w - 16, 1),
        );
        if let Some(error) = &self.error {
            canvas.write(
                list.x + 20,
                list.y + HEADER + 14,
                &format!("This folder cannot be shown: {error}"),
                REGULAR_13,
                theme.danger,
            );
        } else if self.entries.is_empty() {
            canvas.write(
                list.x + 20,
                list.y + HEADER + 14,
                "This folder is empty",
                REGULAR_13,
                theme.text_3,
            );
        }
        for row in 0..self.visible_rows() {
            let Some(area) = self.row_frame(row) else {
                break;
            };
            let index = self.first_row + row;
            let entry = &self.entries[index];
            if self.selected == Some(index) {
                canvas.round_rect(area, 7, theme.accent_soft.colour, theme.accent_soft.alpha);
            }
            let icon_area = Rect::new(area.x + 10, area.y + 5, 20, 20);
            match art::icon("folder", 20) {
                Some(picture) if entry.is_dir => canvas.picture(icon_area.x, icon_area.y, &picture),
                _ => glyph(canvas, icon_area, "doc", 20, theme.text_3),
            }
            let text_y = area.y + (area.h - REGULAR_13.height()) / 2;
            let room = size_right - 70 - (area.x + 40);
            let (name, cut) = REGULAR_13.fit(&entry.name, room);
            let end = canvas.write(area.x + 40, text_y, name, REGULAR_13, theme.text);
            if cut {
                canvas.write(end, text_y, "…", REGULAR_13, theme.text);
            }
            let small_y = area.y + (area.h - REGULAR_12.height()) / 2;
            if !entry.is_dir {
                let size = readable_size(entry.size);
                canvas.write(
                    size_right - REGULAR_12.width(&size),
                    small_y,
                    &size,
                    REGULAR_12,
                    theme.text_2,
                );
            }
            canvas.write(
                kind_left,
                small_y,
                kind(entry, &self.path),
                REGULAR_12,
                theme.text_3,
            );
        }

        // The status line.
        let status = Rect::new(SIDEBAR, height - STATUS, width - SIDEBAR, STATUS);
        line(canvas, Rect::new(status.x, status.y, status.w, 1));
        let count = match self.entries.len() {
            1 => "1 item".into(),
            count => format!("{count} items"),
        };
        let text = match self.selected() {
            Some(name) => format!("{count} · {name} selected"),
            None => count,
        };
        let (text, _) = REGULAR_12.fit(&text, status.w - 32);
        canvas.write(
            status.x + 16,
            status.y + (STATUS - REGULAR_12.height()) / 2 + 1,
            text,
            REGULAR_12,
            theme.text_3,
        );
    }

    fn click(&mut self, x: i32, y: i32, context: &mut Context) {
        let at = Point::new(x, y);
        if self.back_button().contains(at) {
            return self.back();
        }
        if self.up_button().contains(at) {
            return self.up();
        }
        if let Some((_, path)) = self
            .crumbs()
            .into_iter()
            .find(|(area, _)| area.contains(at))
        {
            return self.enter(path);
        }
        let place = (0..self.places.len()).find(|&index| {
            self.place_frame(index)
                .is_some_and(|area| area.contains(at))
        });
        if let Some(index) = place {
            let path = self.places[index].path.clone();
            return self.enter(path);
        }
        let row = (0..self.visible_rows())
            .find(|&row| self.row_frame(row).is_some_and(|area| area.contains(at)));
        match row {
            Some(row) => {
                let index = self.first_row + row;
                if self.selected == Some(index) {
                    self.activate(context);
                } else {
                    self.selected = Some(index);
                }
            }
            // A click on nothing in the list selects nothing.
            None if x >= SIDEBAR && y >= TOOLBAR + HEADER => self.selected = None,
            None => {}
        }
    }

    fn key(&mut self, code: i32, context: &mut Context) {
        const ENTER: i32 = 13;
        const BACKSPACE: i32 = 127;
        const UP: i32 = 0x101;
        const DOWN: i32 = 0x102;
        const HOME: i32 = 0x105;
        const END: i32 = 0x106;
        const PAGE_UP: i32 = 0x107;
        const PAGE_DOWN: i32 = 0x108;
        let last = self.entries.len().saturating_sub(1);
        let page = self.visible_rows();
        let from = self.selected;
        let moved = match code {
            // With nothing selected, any movement selects the first row.
            UP | DOWN | PAGE_UP | PAGE_DOWN if from.is_none() => Some(0),
            UP => from.map(|at| at.saturating_sub(1)),
            DOWN => from.map(|at| (at + 1).min(last)),
            PAGE_UP => from.map(|at| at.saturating_sub(page)),
            PAGE_DOWN => from.map(|at| (at + page).min(last)),
            HOME => Some(0),
            END => Some(last),
            ENTER => return self.activate(context),
            BACKSPACE => return self.up(),
            _ => return,
        };
        if !self.entries.is_empty() {
            self.selected = moved;
            self.keep_selection_visible();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::Surface;
    use alloc::rc::Rc;
    use alloc::vec;
    use core::cell::RefCell;

    /// A small tree: `/` holds `bin`, `data` and two files; `/bin` holds
    /// forty programs and two Windows files; `/data` is empty; `/secret`
    /// cannot be listed.
    struct Tree;

    impl Storage for Tree {
        fn list(&mut self, path: &str) -> Result<Vec<Entry>, String> {
            let file = |name: &str, size| Entry {
                name: name.into(),
                is_dir: false,
                size,
            };
            let dir = |name: &str| Entry {
                name: name.into(),
                is_dir: true,
                size: 0,
            };
            match path {
                "/" => Ok(vec![
                    file("notes.txt", 300),
                    dir("data"),
                    file("Zebra", 5_000_000),
                    dir("bin"),
                    dir("secret"),
                ]),
                "/bin" => {
                    let mut all: Vec<Entry> = (0..40)
                        .map(|i| file(&format!("tool{i:02}"), 1000 * (i + 1)))
                        .collect();
                    all.push(file("winhello.exe", 2048));
                    all.push(file("winsample.dll", 1));
                    all.push(dir("extras"));
                    Ok(all)
                }
                "/bin/extras" | "/data" => Ok(Vec::new()),
                _ => Err("No such file or directory".into()),
            }
        }
    }

    type Opened = Rc<RefCell<Vec<String>>>;

    fn files(path: &str) -> (Files<Tree>, Opened) {
        let opened: Opened = Rc::default();
        let record = opened.clone();
        let places = vec![
            Place {
                label: "Computer".into(),
                path: "/".into(),
                icon: "mon".into(),
            },
            Place {
                label: "Programs".into(),
                path: "/bin".into(),
                icon: "grid".into(),
            },
            Place {
                label: "Data".into(),
                path: "/data".into(),
                icon: "drive".into(),
            },
        ];
        let window = Files::new(
            Tree,
            places,
            path,
            Box::new(move |path, _| record.borrow_mut().push(path.into())),
        );
        (window, opened)
    }

    fn click_in(window: &mut Files<Tree>, area: Rect) {
        window.click(
            area.x + area.w / 2,
            area.y + area.h / 2,
            &mut Context::default(),
        );
    }

    #[test]
    fn a_folder_is_listed_folders_first_then_by_name() {
        let (window, _) = files("/");
        assert_eq!(
            window.names(),
            ["bin", "data", "secret", "notes.txt", "Zebra"]
        );
        assert_eq!(window.path(), "/");
        assert_eq!(window.title(), "Files — Computer");
        assert_eq!(window.selected(), None);
        let (window, _) = files("/bin");
        assert_eq!(window.title(), "Files — bin");
        assert_eq!(window.names()[0], "extras");
        assert_eq!(window.names().len(), 43);
    }

    #[test]
    fn sizes_and_kinds_read_naturally() {
        for (bytes, text) in [
            (0, "0 bytes"),
            (1, "1 byte"),
            (1023, "1023 bytes"),
            (1024, "1.0 KB"),
            (1536, "1.5 KB"),
            (10_240, "10 KB"),
            (1_048_575, "1023 KB"),
            (1_048_576, "1.0 MB"),
            (5_000_000, "4.7 MB"),
            (300_000_000, "286 MB"),
        ] {
            assert_eq!(readable_size(bytes), text);
        }
        let entry = |name: &str, is_dir| Entry {
            name: name.into(),
            is_dir,
            size: 0,
        };
        assert_eq!(kind(&entry("bin", true), "/"), "Folder");
        assert_eq!(
            kind(&entry("WinHello.EXE", false), "/bin"),
            "Windows program"
        );
        assert_eq!(kind(&entry("a.dll", false), "/x"), "Windows library");
        assert_eq!(kind(&entry("hello", false), "/bin"), "Program");
        assert_eq!(kind(&entry("hello", false), "/data"), "File");
        assert_eq!(kind(&entry("notes.txt", false), "/"), "Text");
        assert_eq!(parent("/a/b"), "/a");
        assert_eq!(parent("/a"), "/");
        assert_eq!(parent("/"), "/");
        assert_eq!(join("/", "a"), "/a");
        assert_eq!(join("/a", "b"), "/a/b");
    }

    #[test]
    fn a_click_selects_and_a_second_click_opens() {
        let (mut window, opened) = files("/");
        let first = window.row_frame(0).unwrap();
        click_in(&mut window, first);
        assert_eq!(window.selected(), Some("bin"));
        assert_eq!(window.path(), "/", "selecting is not opening");
        click_in(&mut window, first);
        assert_eq!(window.path(), "/bin");
        assert_eq!(window.selected(), None);
        // A file: opened through what the program supplied, with its path.
        let (mut window, opened_files) = files("/");
        let notes = window.row_frame(3).unwrap();
        click_in(&mut window, notes);
        assert!(opened_files.borrow().is_empty());
        click_in(&mut window, notes);
        assert_eq!(*opened_files.borrow(), ["/notes.txt"]);
        assert_eq!(window.path(), "/");
        // Another row in between: selection moves, nothing opens.
        let zebra = window.row_frame(4).unwrap();
        click_in(&mut window, zebra);
        click_in(&mut window, notes);
        assert_eq!(opened_files.borrow().len(), 1);
        assert_eq!(window.selected(), Some("notes.txt"));
        // Below the last row: nothing selected.
        window.click(
            SIDEBAR + 100,
            TOOLBAR + HEADER + 6 * ROW + 5,
            &mut Context::default(),
        );
        assert_eq!(window.selected(), None);
        assert_eq!(window.row_frame(5), None);
        assert!(opened.borrow().is_empty());
    }

    #[test]
    fn back_up_the_path_bar_and_the_places_move_between_folders() {
        let (mut window, _) = files("/");
        // Places.
        let programs = window.place_frame(1).unwrap();
        click_in(&mut window, programs);
        assert_eq!(window.path(), "/bin");
        // Into a folder (it is the first row), then up: the folder just
        // left is selected.
        let extras = window.row_frame(0).unwrap();
        click_in(&mut window, extras);
        click_in(&mut window, extras);
        assert_eq!(window.path(), "/bin/extras");
        assert!(window.names().is_empty());
        let up = window.up_button();
        click_in(&mut window, up);
        assert_eq!((window.path(), window.selected()), ("/bin", Some("extras")));
        // Back retraces the steps, one at a time, and then stops.
        let back = window.back_button();
        click_in(&mut window, back);
        assert_eq!(window.path(), "/bin/extras");
        click_in(&mut window, back);
        assert_eq!(window.path(), "/bin");
        click_in(&mut window, back);
        assert_eq!(window.path(), "/");
        click_in(&mut window, back);
        assert_eq!(window.path(), "/");
        // Up from the top does nothing.
        click_in(&mut window, up);
        assert_eq!(window.path(), "/");

        // The path bar: one name per folder, each leading there.
        let (mut window, _) = files("/bin/extras");
        let crumbs = window.crumbs();
        let paths: Vec<&str> = crumbs.iter().map(|(_, path)| path.as_str()).collect();
        assert_eq!(paths, ["/", "/bin", "/bin/extras"]);
        for pair in crumbs.windows(2) {
            assert!(pair[1].0.x > pair[0].0.right());
        }
        click_in(&mut window, crumbs[1].0);
        assert_eq!(window.path(), "/bin");
        click_in(&mut window, crumbs[0].0);
        assert_eq!(window.path(), "/");
        // Clicking the name of the folder one is in changes nothing,
        // and is not a step to go back over.
        let here = window.crumbs()[0].0;
        click_in(&mut window, here);
        click_in(&mut window, back);
        assert_eq!(window.path(), "/bin");
    }

    #[test]
    fn the_keyboard_moves_the_selection_and_scrolls_the_list() {
        let (mut window, opened) = files("/bin");
        let mut context = Context::default();
        let rows = window.visible_rows();
        assert_eq!(rows, ((SIZE.1 - TOOLBAR - HEADER - STATUS) / ROW) as usize);
        // The first movement selects the first row.
        window.key(0x102, &mut context);
        assert_eq!(window.selected(), Some("extras"));
        window.key(0x102, &mut context);
        assert_eq!(window.selected(), Some("tool00"));
        window.key(0x101, &mut context);
        window.key(0x101, &mut context);
        window.key(0x101, &mut context);
        assert_eq!(window.selected(), Some("extras"));
        // End: the last entry, and the list has scrolled to show it.
        window.key(0x106, &mut context);
        assert_eq!(window.selected(), Some("winsample.dll"));
        assert_eq!(window.first_row, 43 - rows);
        assert!(window.row_frame(rows - 1).is_some());
        assert_eq!(window.row_frame(rows), None);
        // A page up, and home.
        window.key(0x107, &mut context);
        assert_eq!(window.selected, Some(42 - rows));
        window.key(0x105, &mut context);
        assert_eq!((window.selected, window.first_row), (Some(0), 0));
        window.key(0x108, &mut context);
        assert_eq!(window.selected, Some(rows));
        // Enter opens: a file, then (from the top) a folder.
        window.key(13, &mut context);
        assert_eq!(opened.borrow().len(), 1);
        window.key(0x105, &mut context);
        window.key(13, &mut context);
        assert_eq!(window.path(), "/bin/extras");
        // Nothing to select in an empty folder; Backspace goes up.
        window.key(0x102, &mut context);
        window.key(13, &mut context);
        assert_eq!(window.selected(), None);
        window.key(127, &mut context);
        assert_eq!((window.path(), window.selected()), ("/bin", Some("extras")));
        // Other keys do nothing.
        window.key(i32::from(b'x'), &mut context);
        assert_eq!(window.path(), "/bin");
    }

    #[test]
    fn a_folder_that_cannot_be_listed_says_so_and_can_be_left() {
        let (mut window, _) = files("/");
        let secret = window.row_frame(2).unwrap();
        click_in(&mut window, secret);
        click_in(&mut window, secret);
        assert_eq!(window.path(), "/secret");
        assert!(window.names().is_empty());
        assert_eq!(window.error.as_deref(), Some("No such file or directory"));
        let theme = Theme::default();
        let mut surface = Surface::new(SIZE.0 as usize, SIZE.1 as usize, theme.window);
        window.draw(&mut surface.canvas(), SIZE.0, SIZE.1, &theme);
        // The message is written in the danger colour's shades.
        let reddish = surface
            .pixels()
            .iter()
            .filter(|&&p| (p >> 16) > 0xC0 && ((p >> 8) & 0xFF) < 0x90)
            .count();
        assert!(reddish > 40, "{reddish}");
        let back = window.back_button();
        click_in(&mut window, back);
        assert_eq!(window.path(), "/");
        assert_eq!(window.error, None);
    }

    #[test]
    fn the_window_draws_its_parts_where_they_are_clicked() {
        for theme in [
            Theme::default(),
            Theme::new(crate::theme::Mode::Dark, crate::theme::Accent::Teal),
        ] {
            let (mut window, _) = files("/");
            let (width, height) = (760, 500);
            let mut surface = Surface::new(width as usize, height as usize, theme.window);
            window.draw(&mut surface.canvas(), width, height, &theme);
            let at = |x, y| surface.pixel(x, y).unwrap();
            // Toolbar and sidebar in the chrome colour, the list on the
            // window colour.
            assert_eq!(at(width - 20, 6), theme.chrome);
            assert_eq!(at(20, height - 20), theme.chrome);
            assert_eq!(at(width - 20, height - 60), theme.window);
            // The place one is in is marked; the others are not.
            let here = window.place_frame(0).unwrap();
            let there = window.place_frame(1).unwrap();
            assert_eq!(
                at(here.right() - 6, here.y + 15),
                theme.fill_2.over(theme.chrome)
            );
            assert_eq!(at(there.right() - 6, there.y + 15), theme.chrome);
            // The selected row is filled with the soft accent.
            let row = window.row_frame(1).unwrap();
            assert_eq!(at(row.right() - 4, row.y + 15), theme.window);
            click_in(&mut window, row);
            let mut again = Surface::new(width as usize, height as usize, theme.window);
            window.draw(&mut again.canvas(), width, height, &theme);
            assert_eq!(
                again.pixel(row.right() - 4, row.y + 15),
                Some(theme.accent_soft.over(theme.window))
            );
            // The wider window has wider rows, and more of them.
            assert_eq!(row.w, width - SIDEBAR - 16);
            assert!(window.visible_rows() > ((SIZE.1 - TOOLBAR - HEADER - STATUS) / ROW) as usize);
        }
    }

    #[test]
    fn tiny_and_odd_sizes_do_not_panic() {
        let theme = Theme::default();
        for (width, height) in [(1, 1), (160, 60), (200, 500), (2000, 80), (680, 420)] {
            let (mut window, _) = files("/bin");
            let mut surface = Surface::new(width.max(1) as usize, height.max(1) as usize, 0);
            window.draw(&mut surface.canvas(), width, height, &theme);
            let mut context = Context::default();
            for code in [0x102, 0x106, 0x107, 0x105, 13] {
                window.key(code, &mut context);
                window.draw(&mut surface.canvas(), width, height, &theme);
            }
            for (x, y) in [
                (0, 0),
                (width - 1, height - 1),
                (width / 2, height / 2),
                (-5, 900),
            ] {
                window.click(x, y, &mut context);
            }
        }
    }
}
