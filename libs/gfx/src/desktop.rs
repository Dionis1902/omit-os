// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The window manager, drawn to the Omit OS design system.
//!
//! The [`Desktop`] owns a stack of windows, a task bar, the Start panel
//! and the pointer. It is driven entirely by method calls —
//! [`Desktop::mouse`], [`Desktop::key`], [`Desktop::tick`] — and draws on
//! request into a [`Surface`] with [`Desktop::render`], which repaints
//! only the area that changed and reports it, so the caller can copy
//! just that part to the screen.
//!
//! Window contents come from the program as [`App`] objects. An app draws
//! through a [`Canvas`] confined to its window and reacts to keys and
//! clicks; it can ask for windows to be opened or closed, or for the
//! desktop to end, through the [`Context`] it is handed.
//!
//! What is on screen, back to front: the wallpaper; the windows, each a
//! rounded sheet with a soft shadow, a title bar with the program's icon
//! and name, and one capsule holding its minimise, maximise and close
//! buttons; the task bar, a strip of translucent glass along the bottom
//! with the Start mark, one bare icon per program (a dot under those
//! that are running, a short accent bar under the one in front) and the
//! clock; the Start panel, a list of every program; and the pointer.
//!
//! Behaviour: clicking a window raises it and gives it the keyboard;
//! dragging its title bar moves it and its bottom-right corner resizes
//! it. A task bar icon starts its program, or — if the program has a
//! window — brings that window to the front, or minimises it if it is
//! in front already. The Start mark opens the list of programs, which
//! always starts a new one.
//!
//! Simplifications against the design: no animation; the task bar is
//! tinted glass without blur; windows are found for a task bar icon by
//! their title beginning with the icon's label.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use crate::Colour;
use crate::art;
use crate::font::{MEDIUM_13, REGULAR_12, REGULAR_13, SEMIBOLD_13};
use crate::geometry::{Point, Rect};
use crate::icon::icon;
use crate::paint::mix;
use crate::surface::{Canvas, Surface};
use crate::theme::{Theme, Tone};

/// Height of the task bar at the bottom of the screen.
pub const TASKBAR_HEIGHT: i32 = 52;
/// Height of a window's title bar.
pub const TITLE_HEIGHT: i32 = 44;
/// Width of the line around a window.
pub const BORDER: i32 = 1;
/// Radius of a window's corners.
pub const RADIUS: i32 = 14;
/// How much of a window must stay on screen horizontally.
const KEEP_VISIBLE: i32 = 48;
/// Side of the square in a window's bottom-right corner that resizes it.
pub const GRIP: i32 = 14;
/// Smallest content area a window can be resized to.
const MIN_CONTENT: (i32, i32) = (160, 60);

/// Side of a task bar button.
const TASK_BUTTON: i32 = 42;
/// Space between task bar buttons.
const TASK_GAP: i32 = 4;
/// A window button in the title bar's capsule.
const CONTROL: (i32, i32) = (30, 24);
/// How far a window's shadow reaches, and how far below the window it
/// is cast: for the window in front, and for the others.
const SHADOW_FRONT: (i32, i32) = (36, 12);
const SHADOW_BEHIND: (i32, i32) = (22, 6);
/// Width of the Start panel, and the height of one of its rows.
const START_WIDTH: i32 = 300;
const START_ROW: i32 = 36;
/// The top of the Start panel: the search field and the space around it.
const START_HEADER: i32 = 60;
const START_FOOTER: i32 = 52;

/// The pointer: `#` is outline, `.` is fill. The hot spot is the top-left
/// corner.
const POINTER: [&str; 17] = [
    "#           ",
    "##          ",
    "#.#         ",
    "#..#        ",
    "#...#       ",
    "#....#      ",
    "#.....#     ",
    "#......#    ",
    "#.......#   ",
    "#........#  ",
    "#.........# ",
    "#......#####",
    "#...#..#    ",
    "#..# #..#   ",
    "#.#  #..#   ",
    "##    #..#  ",
    "#      ##   ",
];
const POINTER_WIDTH: i32 = 12;
const POINTER_HEIGHT: i32 = POINTER.len() as i32;
/// Pointer outline and fill, the same in every theme.
pub const POINTER_EDGE: Colour = 0x00_00_00;
/// See [`POINTER_EDGE`].
pub const POINTER_FILL: Colour = 0xFF_FF_FF;

/// Something an [`App`] or task bar button wants the desktop to do.
enum Request {
    Open {
        app: Box<dyn App>,
        width: i32,
        height: i32,
    },
    Close,
    Quit,
    Theme(Theme),
    Mode(crate::theme::Mode),
    Accent(crate::theme::Accent),
    Align(Alignment),
}

/// Where the programs' icons sit on the task bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Alignment {
    /// After the Start mark, at the left.
    #[default]
    Left,
    /// In the middle of the bar.
    Centre,
}

/// Handed to apps and task bar actions so they can talk to the desktop.
#[derive(Default)]
pub struct Context {
    requests: Vec<Request>,
}

impl Context {
    /// Opens a new window showing `app`, with a content area of `width` x
    /// `height` pixels.
    pub fn open(&mut self, app: Box<dyn App>, width: i32, height: i32) {
        self.requests.push(Request::Open { app, width, height });
    }

    /// Closes the window of the app making the call.
    pub fn close(&mut self) {
        self.requests.push(Request::Close);
    }

    /// Ends the desktop: [`Desktop::quit_requested`] becomes true.
    pub fn quit(&mut self) {
        self.requests.push(Request::Quit);
    }

    /// Changes the colours of the whole desktop.
    pub fn set_theme(&mut self, theme: Theme) {
        self.requests.push(Request::Theme(theme));
    }

    /// Switches the desktop to light or dark, keeping its accent.
    pub fn set_mode(&mut self, mode: crate::theme::Mode) {
        self.requests.push(Request::Mode(mode));
    }

    /// Changes the desktop's accent colour, keeping its mode.
    pub fn set_accent(&mut self, accent: crate::theme::Accent) {
        self.requests.push(Request::Accent(accent));
    }

    /// Moves the programs' icons on the task bar.
    pub fn set_alignment(&mut self, alignment: Alignment) {
        self.requests.push(Request::Align(alignment));
    }
}

/// The contents of a window.
pub trait App {
    /// The text in the title bar.
    fn title(&self) -> &str;

    /// The name of the icon in the title bar (see [`crate::icon`]).
    fn icon(&self) -> &str {
        "doc"
    }

    /// Draws the contents. (0, 0) is the top-left corner of the content
    /// area, which is `width` x `height` pixels and already filled with
    /// the theme's window colour.
    fn draw(&mut self, canvas: &mut Canvas<'_>, width: i32, height: i32, theme: &Theme);

    /// A key was pressed while the window had the keyboard. Codes are
    /// those of the `input_read` system call: ASCII, or the special keys
    /// from 0x101.
    fn key(&mut self, _code: i32, _context: &mut Context) {}

    /// The left mouse button was pressed at (`x`, `y`) in the content
    /// area.
    fn click(&mut self, _x: i32, _y: i32, _context: &mut Context) {}

    /// The pointer moved, or a button changed, at (`x`, `y`) in the
    /// content area, between a press there and the release of the left
    /// button (which is reported too, with bit 0 of `buttons` clear).
    /// The position can lie outside the content area.
    fn pointer(&mut self, _x: i32, _y: i32, _buttons: u32, _context: &mut Context) {}

    /// Whether the window can be resized and maximised: `draw` must
    /// then cope with any `width` and `height`. Windows whose contents
    /// have a fixed size (a picture someone else draws, say) say no.
    fn resizable(&self) -> bool {
        true
    }

    /// Called regularly. Returns whether the contents changed and need to
    /// be drawn again.
    fn tick(&mut self, _context: &mut Context) -> bool {
        false
    }

    /// Called before each `draw` with where the task bar's icons sit,
    /// for windows that show it.
    fn alignment(&mut self, _alignment: Alignment) {}
}

/// Identifies a window for as long as it is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowId(u32);

/// A button in a window's title bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Takes the window off the screen; its task bar icon brings it back.
    Minimise,
    /// Makes the window fill the screen, or puts it back.
    Maximise,
    /// Closes the window.
    Close,
}

struct Window {
    id: WindowId,
    /// The whole window, border included.
    frame: Rect,
    app: Box<dyn App>,
    /// While the window is maximised: the frame to go back to.
    restore: Option<Rect>,
    /// While the window is arranged in half of the screen: the size to
    /// give it back when it is dragged away.
    arranged: Option<(i32, i32)>,
    /// Off the screen until its task bar icon is clicked.
    minimised: bool,
}

impl Window {
    fn title_bar(&self) -> Rect {
        Rect::new(
            self.frame.x + BORDER,
            self.frame.y + BORDER,
            self.frame.w - 2 * BORDER,
            TITLE_HEIGHT,
        )
    }

    /// The buttons this window has, left to right.
    fn controls(&self) -> &'static [Control] {
        if self.app.resizable() {
            &[Control::Minimise, Control::Maximise, Control::Close]
        } else {
            &[Control::Minimise, Control::Close]
        }
    }

    /// The capsule that holds the buttons, at the right of the title bar.
    fn capsule(&self) -> Rect {
        let bar = self.title_bar();
        let width = self.controls().len() as i32 * CONTROL.0 + 4;
        Rect::new(
            bar.right() - 8 - width,
            bar.y + (TITLE_HEIGHT - CONTROL.1 - 4) / 2,
            width,
            CONTROL.1 + 4,
        )
    }

    fn control(&self, which: Control) -> Option<Rect> {
        let capsule = self.capsule();
        let index = self.controls().iter().position(|&c| c == which)? as i32;
        Some(Rect::new(
            capsule.x + 2 + index * CONTROL.0,
            capsule.y + 2,
            CONTROL.0,
            CONTROL.1,
        ))
    }

    fn control_at(&self, at: Point) -> Option<Control> {
        self.controls()
            .iter()
            .copied()
            .find(|&which| self.control(which).is_some_and(|area| area.contains(at)))
    }

    /// The corner that resizes the window when dragged. Maximised
    /// windows have none.
    fn grip(&self) -> Option<Rect> {
        (self.app.resizable() && self.restore.is_none()).then(|| {
            Rect::new(
                self.frame.right() - BORDER - GRIP,
                self.frame.bottom() - BORDER - GRIP,
                GRIP,
                GRIP,
            )
        })
    }

    fn content(&self) -> Rect {
        Rect::new(
            self.frame.x + BORDER,
            self.frame.y + BORDER + TITLE_HEIGHT,
            self.frame.w - 2 * BORDER,
            self.frame.h - 2 * BORDER - TITLE_HEIGHT,
        )
    }

    /// The radius of the window's corners: none when it fills the screen.
    fn radius(&self) -> i32 {
        if self.restore.is_some() { 0 } else { RADIUS }
    }

    /// Everything the window draws on: its frame and its shadow.
    fn extent(&self) -> Rect {
        if self.restore.is_some() {
            return self.frame;
        }
        let (blur, drop) = SHADOW_FRONT;
        Rect::new(
            self.frame.x - blur,
            self.frame.y - blur + drop,
            self.frame.w + 2 * blur,
            self.frame.h + 2 * blur,
        )
        .union(&self.frame)
    }
}

struct Launcher {
    label: String,
    icon: String,
    action: Box<dyn Fn(&mut Context)>,
}

/// Where a window dragged to an edge of the screen goes when let go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// The left half of the screen.
    Left,
    /// The right half.
    Right,
    /// All of it: maximised.
    Whole,
}

/// The space left between arranged windows, and around them.
const GUTTER: i32 = 8;

/// What the pointer is over, of the things that show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hover {
    Nothing,
    Start,
    Launcher(usize),
    Control(WindowId, Control),
    StartItem(usize),
    Power,
}

/// The window manager. See the module documentation.
pub struct Desktop {
    width: i32,
    height: i32,
    theme: Theme,
    /// Back to front: the last window that is not minimised is on top
    /// and has the keyboard.
    windows: Vec<Window>,
    launchers: Vec<Launcher>,
    /// The clock at the right of the task bar: time, and date under it.
    clock: (String, String),
    pointer: Point,
    left_button: bool,
    hover: Hover,
    /// Whether the Start panel is open.
    start_open: bool,
    /// What has been typed into the Start panel's search field.
    start_query: String,
    /// Which of the listed programs Enter would start.
    start_selected: usize,
    alignment: Alignment,
    /// The wallpaper at the size of the screen, and the mode it is for;
    /// made when first needed.
    wallpaper: Option<(crate::theme::Mode, Vec<Colour>)>,
    /// The window being moved, and where in its frame it was grabbed.
    drag: Option<(WindowId, Point)>,
    /// Where the window being moved would go if it were let go now.
    zone: Option<Zone>,
    /// The window being resized by its corner, and where in the corner
    /// (relative to the frame's bottom-right) it was grabbed.
    resize: Option<(WindowId, Point)>,
    /// The window whose content area the left button was pressed in and
    /// is still held over: it receives pointer movement.
    capture: Option<WindowId>,
    /// The area that needs repainting.
    dirty: Rect,
    next_id: u32,
    quit: bool,
}

impl Desktop {
    /// Creates an empty desktop for a `width` x `height` screen, with the
    /// pointer in the middle, in the default theme.
    pub fn new(width: usize, height: usize) -> Self {
        let (width, height) = (width as i32, height as i32);
        Self {
            width,
            height,
            theme: Theme::default(),
            windows: Vec::new(),
            launchers: Vec::new(),
            clock: (String::new(), String::new()),
            pointer: Point::new(width / 2, height / 2),
            left_button: false,
            hover: Hover::Nothing,
            start_open: false,
            start_query: String::new(),
            start_selected: 0,
            alignment: Alignment::Left,
            wallpaper: None,
            drag: None,
            zone: None,
            resize: None,
            capture: None,
            dirty: Rect::new(0, 0, width, height),
            next_id: 0,
            quit: false,
        }
    }

    fn screen(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    fn taskbar(&self) -> Rect {
        Rect::new(0, self.height - TASKBAR_HEIGHT, self.width, TASKBAR_HEIGHT)
    }

    /// The colours in use.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Changes the colours; everything is repainted.
    pub fn set_theme(&mut self, theme: Theme) {
        if self.theme != theme {
            self.theme = theme;
            self.damage(self.screen());
        }
    }

    /// The Start button, at the left end of the task bar.
    pub fn start_button(&self) -> Rect {
        let bar = self.taskbar();
        Rect::new(
            10,
            bar.y + (TASKBAR_HEIGHT - TASK_BUTTON) / 2,
            TASK_BUTTON,
            TASK_BUTTON,
        )
    }

    fn launcher_button(&self, index: usize) -> Rect {
        let start = self.start_button();
        let count = self.launchers.len() as i32;
        let all = count * TASK_BUTTON + (count - 1).max(0) * TASK_GAP;
        let after_start = start.right() + 10;
        let first = match self.alignment {
            Alignment::Left => after_start,
            // In the middle, but never under the Start mark.
            Alignment::Centre => ((self.width - all) / 2).max(after_start),
        };
        Rect::new(
            first + index as i32 * (TASK_BUTTON + TASK_GAP),
            start.y,
            TASK_BUTTON,
            TASK_BUTTON,
        )
    }

    /// Where the programs' icons sit on the task bar.
    pub fn alignment(&self) -> Alignment {
        self.alignment
    }

    /// Moves the programs' icons on the task bar.
    pub fn set_alignment(&mut self, alignment: Alignment) {
        if self.alignment != alignment {
            // Whatever the pointer was over has moved from under it.
            self.damage(self.hover_area(self.hover));
            self.hover = Hover::Nothing;
            self.alignment = alignment;
            self.damage(self.taskbar());
        }
    }

    /// The area of the `index`-th program's task bar button.
    pub fn launcher_frame(&self, index: usize) -> Option<Rect> {
        (index < self.launchers.len()).then(|| self.launcher_button(index))
    }

    /// The Start panel: above the task bar, at the left.
    fn start_panel(&self) -> Rect {
        let rows = self.launchers.len() as i32;
        let wanted = START_HEADER + rows * START_ROW + 8 + START_FOOTER;
        let height = wanted.min(self.height - TASKBAR_HEIGHT - 20).max(0);
        Rect::new(
            10,
            self.height - TASKBAR_HEIGHT - 10 - height,
            START_WIDTH.min(self.width - 20),
            height,
        )
    }

    /// The programs the Start panel lists: all of them, or those whose
    /// name contains what has been typed (whatever the case).
    fn start_matches(&self) -> Vec<usize> {
        let query = self.start_query.to_lowercase();
        (0..self.launchers.len())
            .filter(|&index| self.launchers[index].label.to_lowercase().contains(&query))
            .collect()
    }

    /// The search field at the top of the Start panel.
    fn start_field(&self) -> Rect {
        let panel = self.start_panel();
        Rect::new(panel.x + 12, panel.y + 12, panel.w - 24, 36)
    }

    /// What has been typed into the Start panel's search field.
    pub fn start_query(&self) -> &str {
        &self.start_query
    }

    /// The area of the `row`-th row of the Start panel's list.
    fn start_item(&self, index: usize) -> Rect {
        let panel = self.start_panel();
        Rect::new(
            panel.x + 8,
            panel.y + START_HEADER + index as i32 * START_ROW,
            panel.w - 16,
            START_ROW,
        )
    }

    /// The `row`-th row of the Start panel's list, while the panel is
    /// open and lists that many programs.
    pub fn start_item_frame(&self, row: usize) -> Option<Rect> {
        (self.start_open && row < self.start_matches().len()).then(|| self.start_item(row))
    }

    /// The button in the Start panel that ends the desktop.
    fn power_button(&self) -> Rect {
        let panel = self.start_panel();
        Rect::new(panel.right() - 8 - 36, panel.bottom() - 8 - 36, 36, 36)
    }

    /// The power button, while the Start panel is open.
    pub fn power_button_frame(&self) -> Option<Rect> {
        self.start_open.then(|| self.power_button())
    }

    /// Whether the Start panel is open.
    pub fn start_is_open(&self) -> bool {
        self.start_open
    }

    fn set_start(&mut self, open: bool) {
        if self.start_open != open {
            self.start_open = open;
            self.start_query.clear();
            self.start_selected = 0;
            self.damage(self.start_panel().inset(-(SHADOW_FRONT.0 + SHADOW_FRONT.1)));
            self.damage(self.start_button());
        }
    }

    fn pointer_rect(&self) -> Rect {
        Rect::new(
            self.pointer.x,
            self.pointer.y,
            POINTER_WIDTH,
            POINTER_HEIGHT,
        )
    }

    fn damage(&mut self, area: Rect) {
        self.dirty = self.dirty.union(&area.intersect(&self.screen()));
    }

    fn damage_window(&mut self, index: usize) {
        let extent = self.windows[index].extent();
        self.damage(extent);
    }

    /// Adds a program to the task bar and the Start panel: `icon` names
    /// its icon (see [`crate::icon`]), and choosing it calls `action`.
    pub fn add_launcher(
        &mut self,
        label: &str,
        icon: &str,
        action: impl Fn(&mut Context) + 'static,
    ) {
        self.launchers.push(Launcher {
            label: label.into(),
            icon: icon.into(),
            action: Box::new(action),
        });
        self.damage(self.taskbar());
    }

    /// Sets the clock at the right end of the task bar: the time, and
    /// the date shown under it.
    pub fn set_clock(&mut self, time: &str, date: &str) {
        if self.clock.0 != time || self.clock.1 != date {
            self.clock = (time.into(), date.into());
            self.damage(self.clock_area());
        }
    }

    fn clock_area(&self) -> Rect {
        let bar = self.taskbar();
        Rect::new(bar.right() - 150, bar.y, 150, bar.h)
    }

    /// Opens a window showing `app` with a `width` x `height` content
    /// area, on top of the others. Each new window is placed a little
    /// further down and right than the previous one.
    pub fn open(&mut self, app: Box<dyn App>, width: i32, height: i32) -> WindowId {
        let id = WindowId(self.next_id);
        self.next_id += 1;
        let frame_width = width.max(MIN_CONTENT.0) + 2 * BORDER;
        let frame_height = height.max(40) + 2 * BORDER + TITLE_HEIGHT;
        let step = (id.0 % 8) as i32 * 28;
        let max_x = (self.width - frame_width).max(0);
        let max_y = (self.height - TASKBAR_HEIGHT - frame_height).max(0);
        let frame = Rect::new(
            (40 + step).min(max_x),
            (30 + step).min(max_y),
            frame_width,
            frame_height,
        );
        // The window that was in front loses its place.
        self.damage_front();
        self.windows.push(Window {
            id,
            frame,
            app,
            restore: None,
            arranged: None,
            minimised: false,
        });
        self.damage_window(self.windows.len() - 1);
        self.damage(self.taskbar());
        id
    }

    /// Number of open windows, minimised ones included.
    pub fn window_count(&self) -> usize {
        self.windows.len()
    }

    /// The windows' titles, from the bottom of the stack to the top.
    pub fn titles(&self) -> Vec<String> {
        self.windows.iter().map(|w| w.app.title().into()).collect()
    }

    /// The area window `id` occupies, border included.
    pub fn window_frame(&self, id: WindowId) -> Option<Rect> {
        self.windows.iter().find(|w| w.id == id).map(|w| w.frame)
    }

    /// The area of a button in window `id`'s title bar, if it has it.
    pub fn control_frame(&self, id: WindowId, which: Control) -> Option<Rect> {
        self.windows
            .iter()
            .find(|w| w.id == id)
            .and_then(|w| w.control(which))
    }

    /// Whether window `id` is minimised.
    pub fn is_minimised(&self, id: WindowId) -> bool {
        self.windows.iter().any(|w| w.id == id && w.minimised)
    }

    /// The window in front, which has the keyboard.
    pub fn front(&self) -> Option<WindowId> {
        self.front_index().map(|index| self.windows[index].id)
    }

    fn front_index(&self) -> Option<usize> {
        self.windows.iter().rposition(|w| !w.minimised)
    }

    fn damage_front(&mut self) {
        if let Some(index) = self.front_index() {
            self.damage_window(index);
        }
    }

    /// Where the pointer is.
    pub fn pointer(&self) -> Point {
        self.pointer
    }

    /// Whether something asked for the desktop to end.
    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    fn index_of(&self, id: WindowId) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    fn close(&mut self, id: WindowId) {
        if let Some(index) = self.index_of(id) {
            self.damage_window(index);
            self.windows.remove(index);
            self.damage_front();
            self.damage(self.taskbar());
            if self.drag.is_some_and(|(dragged, _)| dragged == id) {
                self.drag = None;
            }
            if self.resize.is_some_and(|(resized, _)| resized == id) {
                self.resize = None;
            }
        }
    }

    /// The windows that belong to the `index`-th program: those whose
    /// title begins with its label.
    fn windows_of(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        let label = &self.launchers[index].label;
        self.windows
            .iter()
            .enumerate()
            .filter(move |(_, w)| w.app.title().starts_with(label.as_str()))
            .map(|(position, _)| position)
    }

    /// Brings the window at `index` to the front, onto the screen if it
    /// was minimised.
    fn raise(&mut self, index: usize) {
        self.damage_front();
        let mut window = self.windows.remove(index);
        window.minimised = false;
        self.windows.push(window);
        self.damage_window(self.windows.len() - 1);
        self.damage(self.taskbar());
    }

    fn minimise(&mut self, index: usize) {
        self.damage_window(index);
        self.windows[index].minimised = true;
        let id = self.windows[index].id;
        if self.capture == Some(id) {
            self.capture = None;
        }
        // The next window down comes to the front.
        self.damage_front();
        self.damage(self.taskbar());
    }

    /// Carries out what an app or launcher asked for; `source` is the
    /// window the requests came from, if any.
    fn perform(&mut self, context: Context, source: Option<WindowId>) {
        for request in context.requests {
            match request {
                Request::Open { app, width, height } => {
                    self.open(app, width, height);
                }
                Request::Close => {
                    if let Some(id) = source {
                        self.close(id);
                    }
                }
                Request::Quit => self.quit = true,
                Request::Theme(theme) => self.set_theme(theme),
                Request::Mode(mode) => {
                    self.set_theme(Theme::new(mode, self.theme.accent_name));
                }
                Request::Accent(accent) => {
                    self.set_theme(Theme::new(self.theme.mode, accent));
                }
                Request::Align(alignment) => self.set_alignment(alignment),
            }
        }
    }

    fn run_launcher(&mut self, index: usize) {
        let mut context = Context::default();
        (self.launchers[index].action)(&mut context);
        self.perform(context, None);
    }

    /// What of the things that react to it the pointer is over.
    fn hover_at(&self, at: Point) -> Hover {
        if self.start_open {
            let panel = self.start_panel();
            if panel.contains(at) {
                if self.power_button().contains(at) {
                    return Hover::Power;
                }
                return (0..self.start_matches().len())
                    .find(|&row| self.start_item(row).contains(at))
                    .map_or(Hover::Nothing, Hover::StartItem);
            }
        }
        if self.taskbar().contains(at) {
            if self.start_button().contains(at) {
                return Hover::Start;
            }
            return (0..self.launchers.len())
                .find(|&i| self.launcher_button(i).contains(at))
                .map_or(Hover::Nothing, Hover::Launcher);
        }
        let top = self
            .windows
            .iter()
            .rposition(|w| !w.minimised && w.frame.contains(at));
        top.and_then(|index| {
            let window = &self.windows[index];
            window
                .control_at(at)
                .map(|which| Hover::Control(window.id, which))
        })
        .unwrap_or(Hover::Nothing)
    }

    fn hover_area(&self, hover: Hover) -> Rect {
        match hover {
            Hover::Nothing => Rect::default(),
            Hover::Start => self.start_button(),
            // The label shown above a hovered icon is part of it.
            Hover::Launcher(index) => self.launcher_button(index).union(&self.tooltip(index)),
            Hover::Control(id, _) => self
                .index_of(id)
                .map_or(Rect::default(), |index| self.windows[index].capsule()),
            Hover::StartItem(index) => self.start_item(index),
            Hover::Power => self.power_button(),
        }
    }

    /// Where the label of the `index`-th task bar icon appears while
    /// the pointer is over it.
    fn tooltip(&self, index: usize) -> Rect {
        let button = self.launcher_button(index);
        let width = REGULAR_12.width(&self.launchers[index].label) + 20;
        let x = (button.x + (button.w - width) / 2).clamp(6, (self.width - width - 6).max(6));
        Rect::new(x, self.taskbar().y - 8 - 26, width, 26).inset(-8)
    }

    fn update_hover(&mut self) {
        let hover = if self.drag.is_some() || self.resize.is_some() || self.capture.is_some() {
            Hover::Nothing
        } else {
            self.hover_at(self.pointer)
        };
        if hover != self.hover {
            self.damage(self.hover_area(self.hover));
            self.damage(self.hover_area(hover));
            self.hover = hover;
        }
    }

    /// Reports mouse movement (`dx` right, `dy` down) and the state of the
    /// buttons (bit 0 is the left button).
    pub fn mouse(&mut self, dx: i32, dy: i32, buttons: u32) {
        let old = self.pointer_rect();
        self.pointer = Point::new(
            self.pointer.x.saturating_add(dx).clamp(0, self.width - 1),
            self.pointer.y.saturating_add(dy).clamp(0, self.height - 1),
        );
        if self.pointer_rect() != old {
            self.damage(old);
            self.damage(self.pointer_rect());
        }

        let left = buttons & 1 != 0;
        let pressed = left && !self.left_button;
        self.left_button = left;

        if !left {
            // A window let go at an edge of the screen is arranged there.
            if let (Some((id, _)), Some(zone)) = (self.drag, self.zone)
                && let Some(index) = self.index_of(id)
            {
                self.arrange(index, zone);
            }
            self.set_zone(None);
            self.drag = None;
            self.resize = None;
        }
        if let Some((id, grab)) = self.resize
            && let Some(index) = self.index_of(id)
        {
            // The corner follows the pointer; the top-left stays put.
            let frame = self.windows[index].frame;
            let min_w = MIN_CONTENT.0 + 2 * BORDER;
            let min_h = MIN_CONTENT.1 + 2 * BORDER + TITLE_HEIGHT;
            let w = (self.pointer.x - grab.x - frame.x).clamp(min_w, self.width.max(min_w));
            let h = (self.pointer.y - grab.y - frame.y)
                .clamp(min_h, (self.height - TASKBAR_HEIGHT - frame.y).max(min_h));
            let resized = Rect::new(frame.x, frame.y, w, h);
            if resized != frame {
                self.damage_window(index);
                self.windows[index].frame = resized;
                // A window given a size by hand is no longer arranged.
                self.windows[index].arranged = None;
                self.damage_window(index);
            }
        }
        if let Some((id, grab)) = self.drag
            && let Some(index) = self.index_of(id)
        {
            // An arranged window dragged away gets its own size back,
            // under the pointer.
            let mut grab = grab;
            if let Some((w, h)) = self.windows[index].arranged.take() {
                self.damage_window(index);
                let frame = self.windows[index].frame;
                grab.x = grab.x * w / frame.w.max(1);
                self.windows[index].frame = Rect::new(frame.x, frame.y, w, h);
                self.drag = Some((id, grab));
            }
            let frame = self.windows[index].frame;
            let x =
                (self.pointer.x - grab.x).clamp(KEEP_VISIBLE - frame.w, self.width - KEEP_VISIBLE);
            let y = (self.pointer.y - grab.y).clamp(0, self.height - TASKBAR_HEIGHT - TITLE_HEIGHT);
            let moved = Rect::new(x, y, frame.w, frame.h);
            if moved != frame {
                self.damage_window(index);
                self.windows[index].frame = moved;
                self.damage_window(index);
            }
            // At an edge of the screen: show where letting go would
            // put the window. (Windows of a fixed size stay as they are.)
            let zone = if !self.windows[index].app.resizable() {
                None
            } else if self.pointer.x == 0 {
                Some(Zone::Left)
            } else if self.pointer.x == self.width - 1 {
                Some(Zone::Right)
            } else if self.pointer.y == 0 {
                Some(Zone::Whole)
            } else {
                None
            };
            self.set_zone(zone);
        }
        if pressed {
            self.press();
        } else if let Some(id) = self.capture {
            // Movement and the release go to the window that was pressed.
            if let Some(index) = self.index_of(id) {
                let content = self.windows[index].content();
                let mut context = Context::default();
                self.windows[index].app.pointer(
                    self.pointer.x - content.x,
                    self.pointer.y - content.y,
                    buttons,
                    &mut context,
                );
                self.perform(context, Some(id));
            }
        }
        if !left {
            self.capture = None;
        }
        self.update_hover();
    }

    fn press(&mut self) {
        let at = self.pointer;
        if self.start_open {
            // A press anywhere closes the panel; on one of its rows it
            // also starts that program.
            let chosen = self.hover_at(at);
            let on_start_button = self.start_button().contains(at);
            let matches = self.start_matches();
            if self.start_field().contains(at) {
                // The field is for typing in; a click there changes nothing.
                return;
            }
            self.set_start(false);
            match chosen {
                Hover::StartItem(row) => {
                    if let Some(&index) = matches.get(row) {
                        self.run_launcher(index);
                    }
                }
                Hover::Power => self.quit = true,
                _ => {}
            }
            if self.start_panel().contains(at) || on_start_button {
                return;
            }
        }
        if self.taskbar().contains(at) {
            if self.start_button().contains(at) {
                self.set_start(true);
                return;
            }
            let hit = (0..self.launchers.len()).find(|&i| self.launcher_button(i).contains(at));
            if let Some(index) = hit {
                self.launcher_pressed(index);
            }
            return;
        }
        // The topmost window under the pointer.
        let Some(index) = self
            .windows
            .iter()
            .rposition(|w| !w.minimised && w.frame.contains(at))
        else {
            return;
        };
        // Raising moves it to the end of the list; a window that is in
        // front already may have minimised ones after it.
        let top = if Some(index) == self.front_index() {
            index
        } else {
            self.raise(index);
            self.windows.len() - 1
        };
        let (id, frame) = (self.windows[top].id, self.windows[top].frame);
        self.damage_window(top);

        match self.windows[top].control_at(at) {
            Some(Control::Close) => return self.close(id),
            Some(Control::Maximise) => return self.toggle_maximised(top),
            Some(Control::Minimise) => return self.minimise(top),
            None => {}
        }
        if self.windows[top]
            .grip()
            .is_some_and(|grip| grip.contains(at))
        {
            // Remember where in the corner the pointer is, so that the
            // corner does not jump to it.
            self.resize = Some((id, Point::new(at.x - frame.right(), at.y - frame.bottom())));
        } else if self.windows[top].title_bar().contains(at) && self.windows[top].restore.is_none()
        {
            // (A maximised window is not dragged about.)
            self.drag = Some((id, Point::new(at.x - frame.x, at.y - frame.y)));
        } else {
            let content = self.windows[top].content();
            if content.contains(at) {
                let mut context = Context::default();
                self.capture = Some(id);
                self.windows[top]
                    .app
                    .click(at.x - content.x, at.y - content.y, &mut context);
                self.perform(context, Some(id));
            }
        }
    }

    /// A task bar icon was pressed: start the program, or bring its
    /// window forward, or — if that is in front already — minimise it.
    fn launcher_pressed(&mut self, index: usize) {
        let Some(window) = self.windows_of(index).last() else {
            return self.run_launcher(index);
        };
        if Some(window) == self.front_index() {
            self.minimise(window);
        } else {
            self.raise(window);
        }
    }

    /// The part of the screen `zone` stands for.
    pub fn zone_frame(&self, zone: Zone) -> Rect {
        let height = self.height - TASKBAR_HEIGHT;
        let half = (self.width - 3 * GUTTER) / 2;
        match zone {
            Zone::Left => Rect::new(GUTTER, GUTTER, half, height - 2 * GUTTER),
            Zone::Right => Rect::new(
                self.width - GUTTER - half,
                GUTTER,
                half,
                height - 2 * GUTTER,
            ),
            Zone::Whole => Rect::new(0, 0, self.width, height),
        }
    }

    fn set_zone(&mut self, zone: Option<Zone>) {
        if self.zone != zone {
            for changed in [self.zone, zone].into_iter().flatten() {
                self.damage(self.zone_frame(changed));
            }
            self.zone = zone;
        }
    }

    /// Puts the window at `index` into `zone`.
    fn arrange(&mut self, index: usize, zone: Zone) {
        self.damage_window(index);
        let target = self.zone_frame(zone);
        let window = &mut self.windows[index];
        // What to go back to: the size the window had (it has been
        // dragged about since, so its place means nothing).
        let own = window.arranged.unwrap_or((window.frame.w, window.frame.h));
        match zone {
            Zone::Whole => {
                let back = Rect::new((target.w - own.0) / 2, GUTTER * 4, own.0, own.1);
                window.arranged = None;
                window.restore = Some(back);
            }
            Zone::Left | Zone::Right => window.arranged = Some(own),
        }
        window.frame = target;
        self.damage_window(index);
    }

    /// Maximises the window at `index` to the whole screen above the
    /// task bar, or puts it back where and how large it was.
    fn toggle_maximised(&mut self, index: usize) {
        self.damage_window(index);
        let (width, height) = (self.width, self.height);
        let window = &mut self.windows[index];
        window.frame = match window.restore.take() {
            Some(previous) => previous,
            None => {
                window.restore = Some(window.frame);
                Rect::new(0, 0, width, height - TASKBAR_HEIGHT)
            }
        };
        self.damage_window(index);
    }

    /// Reports a key press; it goes to the window in front. While the
    /// Start panel is open, Escape closes it instead.
    pub fn key(&mut self, code: i32) {
        if self.start_open {
            self.start_key(code);
            return;
        }
        let Some(index) = self.front_index() else {
            return;
        };
        let id = self.windows[index].id;
        let mut context = Context::default();
        self.windows[index].app.key(code, &mut context);
        self.damage(self.windows[index].frame);
        self.perform(context, Some(id));
    }

    /// A key pressed while the Start panel is open: typing searches,
    /// the arrows choose, Enter starts, Escape closes.
    fn start_key(&mut self, code: i32) {
        const ESCAPE: i32 = 0x1B;
        const ENTER: i32 = 13;
        const BACKSPACE: [i32; 2] = [8, 127];
        const UP: i32 = 0x101;
        const DOWN: i32 = 0x102;
        let listed = self.start_matches();
        match code {
            ESCAPE => return self.set_start(false),
            ENTER => {
                let chosen = listed.get(self.start_selected).copied();
                self.set_start(false);
                if let Some(index) = chosen {
                    self.run_launcher(index);
                }
                return;
            }
            UP => self.start_selected = self.start_selected.saturating_sub(1),
            DOWN => {
                self.start_selected = (self.start_selected + 1).min(listed.len().saturating_sub(1));
            }
            code if BACKSPACE.contains(&code) => {
                self.start_query.pop();
                self.start_selected = 0;
            }
            0x20..=0x7E if self.start_query.len() < 32 => {
                self.start_query.push(code as u8 as char);
                self.start_selected = 0;
            }
            _ => return,
        }
        self.damage(self.start_panel());
    }

    /// Gives every app the chance to update itself; call this regularly.
    pub fn tick(&mut self) {
        let ids: Vec<WindowId> = self.windows.iter().map(|w| w.id).collect();
        for id in ids {
            // An earlier app in this loop may have closed this window.
            let Some(index) = self.index_of(id) else {
                continue;
            };
            let mut context = Context::default();
            if self.windows[index].app.tick(&mut context) && !self.windows[index].minimised {
                self.damage(self.windows[index].content());
            }
            self.perform(context, Some(id));
        }
    }

    /// Whether anything needs repainting.
    pub fn needs_render(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// Repaints what changed since the last call and returns that area
    /// (empty if nothing changed). `surface` should have the size the
    /// desktop was created with and must be the same image on every call.
    pub fn render(&mut self, surface: &mut Surface) -> Rect {
        let dirty = self.dirty.intersect(&surface.bounds());
        self.dirty = Rect::default();
        if dirty.is_empty() {
            return dirty;
        }
        let theme = self.theme;
        // The wallpaper is scaled to the screen once per mode, and then
        // only copied.
        if self
            .wallpaper
            .as_ref()
            .is_none_or(|(mode, _)| *mode != theme.mode)
        {
            self.wallpaper = Some((theme.mode, make_wallpaper(&theme, self.width, self.height)));
        }
        let mut root = surface.canvas();
        let mut canvas = root.clipped(dirty);
        if let Some((_, wallpaper)) = &self.wallpaper {
            let width = self.width as usize;
            canvas.rows(Rect::new(0, 0, self.width, self.height), |x, y, row| {
                let start = y as usize * width + x as usize;
                row.copy_from_slice(&wallpaper[start..start + row.len()]);
            });
        }

        let front = self.front_index();
        let hover = self.hover;
        for (index, window) in self.windows.iter_mut().enumerate() {
            if window.minimised || window.extent().intersect(&dirty).is_empty() {
                continue;
            }
            let hovered = match hover {
                Hover::Control(id, which) if id == window.id => Some(which),
                _ => None,
            };
            window.app.alignment(self.alignment);
            draw_window(&mut canvas, window, Some(index) == front, hovered, &theme);
        }

        if let Some(zone) = self.zone {
            // Where the window being dragged would go: a soft sheet in
            // the accent colour.
            let area = self
                .zone_frame(zone)
                .inset(if zone == Zone::Whole { GUTTER } else { 0 });
            canvas.round_rect(area, RADIUS, theme.accent, 46);
            canvas.round_outline(area, RADIUS, theme.accent, 160);
        }
        if !self.taskbar().intersect(&dirty).is_empty() {
            self.draw_taskbar(&mut canvas);
        }
        if self.start_open {
            self.draw_start(&mut canvas);
        }
        if let Hover::Launcher(index) = self.hover {
            self.draw_tooltip(&mut canvas, index);
        }

        for (row, line) in POINTER.iter().enumerate() {
            for (column, mark) in line.bytes().enumerate() {
                let colour = match mark {
                    b'#' => POINTER_EDGE,
                    b'.' => POINTER_FILL,
                    _ => continue,
                };
                canvas.set(
                    self.pointer.x + column as i32,
                    self.pointer.y + row as i32,
                    colour,
                );
            }
        }
        dirty
    }

    fn draw_taskbar(&self, canvas: &mut Canvas<'_>) {
        let theme = &self.theme;
        let bar = self.taskbar();
        // Glass: a tint over whatever is behind, with a light top edge.
        tone(canvas, bar, 0, theme.bar);
        tone(
            canvas,
            Rect::new(bar.x, bar.y, bar.w, 1),
            0,
            theme.highlight,
        );

        let start = self.start_button();
        if self.start_open {
            tone(canvas, start, 10, theme.press);
        } else if self.hover == Hover::Start {
            tone(canvas, start, 10, theme.hover);
        }
        draw_icon(canvas, start, "start", 24, theme.accent_text);

        let front = self.front_index();
        for index in 0..self.launchers.len() {
            let button = self.launcher_button(index);
            let running = self.windows_of(index).next().is_some();
            let active = front.is_some_and(|front| self.windows_of(index).any(|w| w == front));
            if self.hover == Hover::Launcher(index) || active {
                tone(canvas, button, 10, theme.hover);
            }
            draw_program_icon(
                canvas,
                button,
                &self.launchers[index].icon,
                30,
                24,
                theme.text,
            );
            // Running: a small dot. In front: a short bar in the accent.
            let mark_y = button.bottom() - 6;
            if active {
                canvas.round_rect(
                    Rect::new(button.x + (button.w - 14) / 2, mark_y, 14, 4),
                    2,
                    theme.accent_text,
                    255,
                );
            } else if running {
                tone(
                    canvas,
                    Rect::new(button.x + (button.w - 4) / 2, mark_y, 4, 4),
                    2,
                    theme.running,
                );
            }
        }

        // The clock: the time over the date, right-aligned.
        let right = bar.right() - 18;
        let (time, date) = (&self.clock.0, &self.clock.1);
        let single = date.is_empty();
        let time_y = bar.y + if single { 18 } else { 10 };
        canvas.write(
            right - SEMIBOLD_13.width(time),
            time_y,
            time,
            SEMIBOLD_13,
            theme.text,
        );
        canvas.write(
            right - REGULAR_12.width(date),
            bar.y + 27,
            date,
            REGULAR_12,
            theme.text_2,
        );
    }

    fn draw_start(&self, canvas: &mut Canvas<'_>) {
        let theme = &self.theme;
        let panel = self.start_panel();
        if panel.is_empty() {
            return;
        }
        canvas.shadow(
            panel.translate(0, SHADOW_BEHIND.1),
            16,
            SHADOW_FRONT.0,
            theme.shadow,
            theme.shadow_popup,
        );
        // Without blur behind it, glass this size has to be nearly opaque
        // for what is written on it to be readable.
        canvas.round_rect(panel, 16, theme.glass.colour, 242);
        canvas.round_outline(panel, 16, theme.highlight.colour, theme.highlight.alpha);
        // The search field: what has been typed, or what typing does.
        let field = self.start_field();
        tone(canvas, field, 10, theme.fill);
        canvas.round_outline(field, 10, theme.separator.colour, theme.separator.alpha);
        draw_icon(
            canvas,
            Rect::new(field.x + 10, field.y + 8, 20, 20),
            "search",
            20,
            theme.text_3,
        );
        let text_y = field.y + (field.h - REGULAR_13.height()) / 2;
        if self.start_query.is_empty() {
            canvas.write(
                field.x + 38,
                text_y,
                "Type to search",
                REGULAR_13,
                theme.text_3,
            );
        } else {
            let (shown, _) = REGULAR_13.fit(&self.start_query, field.w - 60);
            let end = canvas.write(field.x + 38, text_y, shown, REGULAR_13, theme.text);
            // The caret.
            canvas.fill(
                Rect::new(end + 1, field.y + 9, 1, field.h - 18),
                theme.accent_text,
            );
        }

        let mut list = canvas.clipped(Rect::new(
            panel.x,
            panel.y + START_HEADER,
            panel.w,
            (panel.h - START_HEADER - START_FOOTER).max(0),
        ));
        let listed = self.start_matches();
        if listed.is_empty() {
            list.write(
                panel.x + 20,
                panel.y + START_HEADER + 10,
                "No program has that in its name",
                REGULAR_13,
                theme.text_3,
            );
        }
        for (row_index, &index) in listed.iter().enumerate() {
            let launcher = &self.launchers[index];
            let row = self.start_item(row_index);
            // Filled: the row under the pointer, or else the one Enter
            // would start.
            let chosen = match self.hover {
                Hover::StartItem(hovered) => hovered == row_index,
                _ => row_index == self.start_selected,
            };
            let (ink, icon_ink) = if chosen {
                list.round_rect(row, 8, theme.accent, 255);
                (theme.on_accent, theme.on_accent)
            } else {
                (theme.text, theme.text_2)
            };
            draw_program_icon(
                &mut list,
                Rect::new(row.x + 8, row.y + (row.h - 20) / 2, 20, 20),
                &launcher.icon,
                20,
                20,
                icon_ink,
            );
            list.write(
                row.x + 40,
                row.y + (row.h - REGULAR_13.height()) / 2,
                &launcher.label,
                REGULAR_13,
                ink,
            );
        }

        let footer = Rect::new(
            panel.x,
            panel.bottom() - START_FOOTER,
            panel.w,
            START_FOOTER,
        );
        tone(
            canvas,
            Rect::new(footer.x + 12, footer.y, footer.w - 24, 1),
            0,
            theme.separator,
        );
        canvas.write(
            footer.x + 20,
            footer.y + 18,
            "Omit OS",
            MEDIUM_13,
            theme.text_2,
        );
        let power = self.power_button();
        if self.hover == Hover::Power {
            canvas.round_rect(power, 10, theme.danger, 255);
        }
        let ink = if self.hover == Hover::Power {
            0xFF_FF_FF
        } else {
            theme.text_2
        };
        draw_icon(canvas, power, "power", 20, ink);
    }

    fn draw_tooltip(&self, canvas: &mut Canvas<'_>, index: usize) {
        let theme = &self.theme;
        let chip = self.tooltip(index).inset(8);
        canvas.shadow(
            chip.translate(0, 2),
            8,
            8,
            theme.shadow,
            theme.shadow_popup / 2,
        );
        // A tooltip must be readable over anything: nearly opaque.
        canvas.round_rect(chip, 8, theme.glass.colour, 245);
        canvas.round_outline(chip, 8, theme.separator.colour, theme.separator.alpha);
        canvas.write(
            chip.x + 10,
            chip.y + (chip.h - REGULAR_12.height()) / 2,
            &self.launchers[index].label,
            REGULAR_12,
            theme.text,
        );
    }
}

/// Blends a tone over `area`, its corners rounded to `radius`.
fn tone(canvas: &mut Canvas<'_>, area: Rect, radius: i32, tone: Tone) {
    if radius == 0 {
        canvas.tint(area, tone.colour, tone.alpha);
    } else {
        canvas.round_rect(area, radius, tone.colour, tone.alpha);
    }
}

/// Draws a program's icon in the middle of `area`: the design system's
/// picture of it, `picture` pixels wide, if there is one called `name`;
/// otherwise the line icon of that name, `glyph` pixels wide, in
/// `colour`.
fn draw_program_icon(
    canvas: &mut Canvas<'_>,
    area: Rect,
    name: &str,
    picture: i32,
    glyph: i32,
    colour: Colour,
) {
    match art::icon(name, picture) {
        Some(image) => canvas.picture(
            area.x + (area.w - picture) / 2,
            area.y + (area.h - picture) / 2,
            &image,
        ),
        None => draw_icon(canvas, area, name, glyph, colour),
    }
}

/// Draws the icon `name` at `size` pixels in the middle of `area`.
/// Names the icon set lacks are drawn as the generic document.
fn draw_icon(canvas: &mut Canvas<'_>, area: Rect, name: &str, size: i32, colour: Colour) {
    if let Some(mask) = icon(name, size).or_else(|| icon("doc", size)) {
        canvas.mask(
            area.x + (area.w - size) / 2,
            area.y + (area.h - size) / 2,
            mask,
            colour,
            255,
        );
    }
}

/// The colour of the wallpaper at (`x`, `y`) on a `width` x `height`
/// screen: what shows wherever nothing is drawn over it. It is the
/// design system's picture for the theme's mode, scaled to cover the
/// screen.
pub fn wallpaper_at(theme: &Theme, width: i32, height: i32, x: i32, y: i32) -> Colour {
    match art::wallpaper(theme.mode) {
        Some(picture) => picture.scaled(width, height, x, y),
        // (Only if the picture built into the library were damaged.)
        None => theme.wallpaper[1],
    }
}

/// The whole wallpaper for a `width` x `height` screen, row by row.
fn make_wallpaper(theme: &Theme, width: i32, height: i32) -> Vec<Colour> {
    let mut pixels = Vec::with_capacity((width.max(0) * height.max(0)) as usize);
    for y in 0..height {
        for x in 0..width {
            pixels.push(wallpaper_at(theme, width, height, x, y));
        }
    }
    pixels
}

fn draw_window(
    canvas: &mut Canvas<'_>,
    window: &mut Window,
    active: bool,
    hovered: Option<Control>,
    theme: &Theme,
) {
    let frame = window.frame;
    let radius = window.radius();
    if radius > 0 {
        let ((blur, drop), strength) = if active {
            (SHADOW_FRONT, theme.shadow_active)
        } else {
            (SHADOW_BEHIND, theme.shadow_inactive)
        };
        canvas.shadow(
            frame.translate(0, drop),
            radius,
            blur,
            theme.shadow,
            strength,
        );
    }
    // What is behind the two bottom corners, to put back around the
    // rounding once the contents (which are rectangular) are drawn.
    let corners = [
        Rect::new(frame.x, frame.bottom() - radius, radius, radius),
        Rect::new(
            frame.right() - radius,
            frame.bottom() - radius,
            radius,
            radius,
        ),
    ];
    let saved = corners.map(|corner| save(canvas, corner));

    canvas.round_rect(frame, radius, theme.window, 255);
    let bar = window.title_bar();
    {
        // The title bar: the top of the same rounded sheet, in the
        // chrome colour.
        let mut top = canvas.clipped(Rect::new(frame.x, frame.y, frame.w, BORDER + TITLE_HEIGHT));
        top.round_rect(frame, radius, theme.chrome, 255);
    }
    tone(
        canvas,
        Rect::new(bar.x, bar.bottom() - 1, bar.w, 1),
        0,
        theme.separator,
    );

    let ink = if active { theme.text } else { theme.text_3 };
    let capsule = window.capsule();
    draw_program_icon(
        canvas,
        Rect::new(bar.x + 13, bar.y + (bar.h - 20) / 2, 20, 20),
        window.app.icon(),
        20,
        20,
        if active {
            theme.accent_text
        } else {
            theme.text_3
        },
    );
    {
        // The title must not run into the buttons.
        let room = capsule.x - 10 - (bar.x + 42);
        let (title, cut) = MEDIUM_13.fit(window.app.title(), room);
        let end = canvas.write(
            bar.x + 42,
            bar.y + (bar.h - MEDIUM_13.height()) / 2,
            title,
            MEDIUM_13,
            ink,
        );
        if cut {
            canvas.write(
                end,
                bar.y + (bar.h - MEDIUM_13.height()) / 2,
                "…",
                MEDIUM_13,
                ink,
            );
        }
    }
    tone(canvas, capsule, 9, theme.fill);
    for &which in window.controls() {
        let Some(button) = window.control(which) else {
            continue;
        };
        let mut glyph = if active { theme.text_2 } else { theme.text_3 };
        if hovered == Some(which) {
            if which == Control::Close {
                canvas.round_rect(button, 7, theme.danger, 255);
                glyph = 0xFF_FF_FF;
            } else {
                tone(canvas, button, 7, theme.fill_2);
                glyph = theme.text;
            }
        }
        let name = match which {
            Control::Minimise => "minimize",
            Control::Maximise if window.restore.is_some() => "restore",
            Control::Maximise => "maximize",
            Control::Close => "close",
        };
        draw_icon(canvas, button, name, 13, glyph);
    }

    let content = window.content();
    canvas.fill(content, theme.window);
    {
        let mut inner = canvas.sub(content);
        window.app.draw(&mut inner, content.w, content.h, theme);
    }
    if let Some(grip) = window.grip() {
        // Three short diagonal strokes in the corner, over the contents.
        for offset in [5, 8, 11] {
            for step in 0..GRIP - offset - 2 {
                canvas.cover(
                    Rect::new(grip.x + offset + step, grip.bottom() - 4 - step, 1, 1),
                    theme.text_3,
                    |_, _| 150,
                );
            }
        }
    }
    for (corner, saved) in corners.into_iter().zip(saved) {
        restore_outside(canvas, corner, &saved, frame, radius);
    }
    canvas.round_outline(frame, radius, theme.edge.colour, theme.edge.alpha);
}

/// Copies the visible pixels of `area`; pixels that are clipped away are
/// not recorded (and are not written by [`restore_outside`] either).
fn save(canvas: &mut Canvas<'_>, area: Rect) -> Vec<Colour> {
    let mut saved = alloc::vec![0; (area.w.max(0) * area.h.max(0)) as usize];
    canvas.rows(area, |x, y, row| {
        let start = (y * area.w + x) as usize;
        saved[start..start + row.len()].copy_from_slice(row);
    });
    saved
}

/// Puts `saved` (taken from `area` by [`save`]) back wherever the rounded
/// rectangle `frame` does not cover, blending along its edge.
fn restore_outside(
    canvas: &mut Canvas<'_>,
    area: Rect,
    saved: &[Colour],
    frame: Rect,
    radius: i32,
) {
    if radius == 0 {
        return;
    }
    // How much of each pixel the window covers is found by drawing the
    // window's shape in white on black.
    let mut shape = Surface::new(area.w.max(1) as usize, area.h.max(1) as usize, 0);
    shape
        .canvas()
        .round_rect(frame.translate(-area.x, -area.y), radius, 0xFF_FF_FF, 255);
    canvas.rows(area, |x, y, row| {
        for (offset, pixel) in row.iter_mut().enumerate() {
            let column = x + offset as i32;
            let covered = (shape.pixel(column, y).unwrap_or(0) & 0xFF) as u8;
            if covered < 255 {
                let behind = saved[(y * area.w + column) as usize];
                *pixel = mix(*pixel, behind, covered);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Accent, Mode};
    use alloc::rc::Rc;
    use core::cell::RefCell;
    use std::format;

    #[derive(Default)]
    struct Log {
        keys: Vec<(String, i32)>,
        clicks: Vec<(String, i32, i32)>,
        pointer: Vec<(String, i32, i32, u32)>,
        draws: usize,
        modes: Vec<Mode>,
        launched: Vec<&'static str>,
    }

    /// An app that records what happens to it and fills its window with a
    /// colour.
    struct Probe {
        name: String,
        colour: Colour,
        fixed: bool,
        log: Rc<RefCell<Log>>,
    }

    impl App for Probe {
        fn title(&self) -> &str {
            &self.name
        }
        fn icon(&self) -> &str {
            "drive"
        }
        fn draw(&mut self, canvas: &mut Canvas<'_>, width: i32, height: i32, theme: &Theme) {
            let mut log = self.log.borrow_mut();
            log.draws += 1;
            log.modes.push(theme.mode);
            // Deliberately far too large: must be confined to the window.
            canvas.fill(
                Rect::new(-500, -500, width + 1000, height + 1000),
                self.colour,
            );
        }
        fn key(&mut self, code: i32, context: &mut Context) {
            self.log.borrow_mut().keys.push((self.name.clone(), code));
            match code {
                0x71 => context.close(),
                0x6E => context.open(
                    Box::new(Probe {
                        name: "child".into(),
                        colour: 0x00_FF_00,
                        fixed: false,
                        log: self.log.clone(),
                    }),
                    200,
                    60,
                ),
                0x51 => context.quit(),
                _ => {}
            }
        }
        fn click(&mut self, x: i32, y: i32, _: &mut Context) {
            self.log.borrow_mut().clicks.push((self.name.clone(), x, y));
        }
        fn pointer(&mut self, x: i32, y: i32, buttons: u32, _: &mut Context) {
            self.log
                .borrow_mut()
                .pointer
                .push((self.name.clone(), x, y, buttons));
        }
        fn resizable(&self) -> bool {
            !self.fixed
        }
        fn tick(&mut self, _: &mut Context) -> bool {
            self.name == "ticker"
        }
    }

    const W: usize = 800;
    const H: usize = 600;

    struct Fixture {
        desktop: Desktop,
        surface: Surface,
        log: Rc<RefCell<Log>>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                desktop: Desktop::new(W, H),
                surface: Surface::new(W, H, 0),
                log: Rc::default(),
            }
        }

        fn probe(&self, name: &str, colour: Colour, fixed: bool) -> Box<Probe> {
            Box::new(Probe {
                name: name.into(),
                colour,
                fixed,
                log: self.log.clone(),
            })
        }

        fn open(&mut self, name: &str, colour: Colour) -> WindowId {
            let app = self.probe(name, colour, false);
            self.desktop.open(app, 240, 120)
        }

        /// A program on the task bar that records being started and
        /// opens a window titled with its label.
        fn add_program(&mut self, label: &'static str, icon: &str) {
            let log = self.log.clone();
            self.desktop.add_launcher(label, icon, move |context| {
                log.borrow_mut().launched.push(label);
                context.open(
                    Box::new(Probe {
                        name: format!("{label} window"),
                        colour: BLUE,
                        fixed: false,
                        log: log.clone(),
                    }),
                    240,
                    120,
                );
            });
        }

        fn move_to(&mut self, x: i32, y: i32) {
            let at = self.desktop.pointer();
            self.desktop
                .mouse(x - at.x, y - at.y, u32::from(self.desktop.left_button));
        }

        fn click_at(&mut self, x: i32, y: i32) {
            self.move_to(x, y);
            self.desktop.mouse(0, 0, 1);
            self.desktop.mouse(0, 0, 0);
        }

        fn click_in(&mut self, area: Rect) {
            self.click_at(area.x + area.w / 2, area.y + area.h / 2);
        }

        fn render(&mut self) -> Rect {
            self.desktop.render(&mut self.surface)
        }

        fn pixel(&self, x: i32, y: i32) -> Colour {
            self.surface.pixel(x, y).unwrap()
        }

        fn wallpaper(&self, x: i32, y: i32) -> Colour {
            wallpaper_at(self.desktop.theme(), W as i32, H as i32, x, y)
        }

        /// How many pixels of `area` have exactly `colour`.
        fn count(&self, area: Rect, colour: Colour) -> usize {
            (area.y..area.bottom())
                .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
                .filter(|&(x, y)| self.surface.pixel(x, y) == Some(colour))
                .count()
        }

        /// How many pixels of `area` differ from `other`: what was drawn
        /// there since (text is anti-aliased, so few of its pixels have
        /// exactly the text colour).
        fn changed(&self, area: Rect, other: &Surface) -> usize {
            (area.y..area.bottom())
                .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
                .filter(|&(x, y)| self.surface.pixel(x, y) != other.pixel(x, y))
                .count()
        }

        /// The image a repaint of everything gives.
        fn repainted(&mut self) -> Surface {
            let mut fresh = Surface::new(W, H, 0);
            self.desktop.damage(Rect::new(0, 0, W as i32, H as i32));
            self.desktop.render(&mut fresh);
            fresh
        }
    }

    const RED: Colour = 0xAA_00_00;
    const GREEN: Colour = 0x00_AA_00;
    const BLUE: Colour = 0x00_00_AA;

    #[test]
    fn a_press_in_a_window_captures_the_pointer_until_release() {
        let mut f = Fixture::new();
        let back = f.open("back", RED);
        let front = f.open("front", GREEN);
        let content = f.desktop.windows[1].content();
        // Movement without a button held is nobody's business.
        f.move_to(content.x + 10, content.y + 10);
        assert!(f.log.borrow().pointer.is_empty());
        // Press: a click. Then movement, even out of the window and over
        // another one, and finally the release, go to the same app.
        f.desktop.mouse(0, 0, 1);
        f.desktop.mouse(5, 7, 1);
        let far = f.desktop.window_frame(back).unwrap();
        f.move_to(far.x + 2, far.y + 60);
        f.desktop.mouse(0, 0, 0);
        f.desktop.mouse(3, 3, 0);
        let log = f.log.borrow();
        assert_eq!(log.clicks, [("front".into(), 10, 10)]);
        let (x, y) = (far.x + 2 - content.x, far.y + 60 - content.y);
        assert_eq!(
            log.pointer,
            [
                ("front".into(), 15, 17, 1),
                ("front".into(), x, y, 1),
                ("front".into(), x, y, 0),
            ]
        );
        // The window order did not change by dragging across.
        assert_eq!(f.desktop.front(), Some(front));
        drop(log);

        // A press on a title bar drags the window; no pointer events.
        let bar = f.desktop.windows[1].title_bar();
        f.move_to(bar.x + 5, bar.y + 5);
        f.desktop.mouse(0, 0, 1);
        f.desktop.mouse(4, 4, 1);
        f.desktop.mouse(0, 0, 0);
        assert_eq!(f.log.borrow().pointer.len(), 3);

        // A window closed while it has the pointer is simply forgotten.
        let content = f.desktop.windows[1].content();
        f.move_to(content.x + 10, content.y + 10);
        f.desktop.mouse(0, 0, 1);
        f.desktop.close(front);
        f.desktop.mouse(1, 1, 1);
        f.desktop.mouse(0, 0, 0);
        assert_eq!(f.log.borrow().pointer.len(), 3);
    }

    #[test]
    fn first_render_paints_wallpaper_task_bar_and_pointer() {
        let mut f = Fixture::new();
        f.add_program("Files", "drive");
        assert_eq!(f.render(), Rect::new(0, 0, W as i32, H as i32));
        let without_clock = f.surface.clone();
        f.desktop.set_clock("12:34", "Tue 10 Nov");
        f.render();
        let theme = *f.desktop.theme();
        // The wallpaper: a gradient between the theme's corner colours.
        assert_eq!(f.pixel(0, 0), f.wallpaper(0, 0));
        assert_eq!(f.pixel(5, 5), f.wallpaper(5, 5));
        assert_ne!(f.wallpaper(5, 5), f.wallpaper(700, 400));
        // The task bar: glass over the wallpaper, with a lighter top edge.
        let bar = f.desktop.taskbar();
        assert_eq!(bar, Rect::new(0, H as i32 - 52, W as i32, 52));
        let (x, y) = (400, bar.y + 20);
        assert_eq!(f.pixel(x, y), theme.bar.over(f.wallpaper(x, y)));
        assert_eq!(
            f.pixel(x, bar.y),
            theme.highlight.over(theme.bar.over(f.wallpaper(x, bar.y)))
        );
        // The Start mark in the accent colour, the program's icon in the
        // text colour, the clock in two lines at the right.
        assert!(f.count(f.desktop.start_button(), theme.accent_text) > 20);
        let button = f.desktop.launcher_frame(0).unwrap();
        assert!(f.count(button, theme.text) > 10);
        assert_eq!(f.desktop.launcher_frame(1), None);
        let clock = f.desktop.clock_area();
        assert!(f.count(Rect::new(clock.x, bar.y, clock.w, 26), theme.text) > 10);
        assert!(f.changed(Rect::new(clock.x, bar.y, clock.w, 26), &without_clock) > 60);
        assert!(f.changed(Rect::new(clock.x, bar.y + 27, clock.w, 25), &without_clock) > 60);
        // Nothing is running: no marks under the icon.
        let marks = Rect::new(button.x, button.bottom() - 7, button.w, 6);
        assert_eq!(f.count(marks, theme.accent_text), 0);
        // The pointer starts in the middle: black tip, white inside.
        assert_eq!(f.desktop.pointer(), Point::new(400, 300));
        assert_eq!(f.pixel(400, 300), POINTER_EDGE);
        assert_eq!(f.pixel(402, 305), POINTER_FILL);
        // Nothing changed: nothing to render.
        assert!(!f.desktop.needs_render());
        assert!(f.render().is_empty());
    }

    #[test]
    fn windows_are_rounded_sheets_with_a_title_bar_and_confined_content() {
        let mut f = Fixture::new();
        let id = f.open("probe", RED);
        f.move_to(700, 500);
        f.render();
        let theme = *f.desktop.theme();
        let frame = f.desktop.window_frame(id).unwrap();
        assert_eq!((frame.w, frame.h), (242, 120 + TITLE_HEIGHT + 2));
        // The title bar in the chrome colour; the title in the primary
        // text colour, the icon in the accent.
        let bar = f.desktop.windows[0].title_bar();
        assert_eq!(f.pixel(frame.x + frame.w / 2, bar.y + 3), theme.chrome);
        assert!(f.count(bar, theme.text) > 10);
        assert!(f.count(Rect::new(bar.x, bar.y, 40, bar.h), theme.accent_text) > 10);
        // The contents: the app's, inside the frame only.
        let content = f.desktop.windows[0].content();
        assert_eq!(f.pixel(content.x + 20, content.y + 20), RED);
        assert_eq!(f.pixel(content.x, content.y), RED);
        assert_eq!(f.count(Rect::new(0, 0, W as i32, H as i32), RED) as i32, {
            // All of the content area except what the two rounded bottom
            // corners and the grip's strokes take.
            let whole = content.w * content.h;
            let counted = f.count(content, RED) as i32;
            assert!(whole - counted < 120, "{} of {whole}", counted);
            counted
        });
        // Rounded: the frame's very corners are not window.
        for (x, y) in [
            (frame.x, frame.y),
            (frame.right() - 1, frame.y),
            (frame.x, frame.bottom() - 1),
            (frame.right() - 1, frame.bottom() - 1),
        ] {
            let pixel = f.pixel(x, y);
            assert!(
                pixel != RED && pixel != theme.chrome && pixel != theme.window,
                "({x}, {y})"
            );
        }
        // A shadow: just below the window the wallpaper is darkened, far
        // from it it is not.
        let below = (frame.x + frame.w / 2, frame.bottom() + 6);
        assert_ne!(f.pixel(below.0, below.1), f.wallpaper(below.0, below.1));
        assert!((f.pixel(below.0, below.1) & 0xFF) < (f.wallpaper(below.0, below.1) & 0xFF));
        assert_eq!(f.pixel(700, 80), f.wallpaper(700, 80));

        // A window behind another: its title is still there, but no
        // longer in the primary text colour.
        f.open("front", GREEN);
        f.render();
        let title = Rect::new(bar.x + 40, bar.y, 60, 28);
        assert_eq!(f.count(title, theme.text), 0);
        assert!(title.w * title.h - f.count(title, theme.chrome) as i32 > 30);
    }

    #[test]
    fn long_titles_are_cut_before_the_buttons() {
        let mut f = Fixture::new();
        let id = f.open(
            "A window with a title that is far too long to fit in its title bar",
            RED,
        );
        f.move_to(700, 500);
        f.render();
        let theme = *f.desktop.theme();
        let capsule = f.desktop.windows[0].capsule();
        // No title ink in or right of the capsule; the capsule's own
        // fill is intact at its left edge.
        let frame = f.desktop.window_frame(id).unwrap();
        let right = Rect::new(
            capsule.x - 4,
            frame.y,
            frame.right() - capsule.x + 4,
            TITLE_HEIGHT,
        );
        assert_eq!(f.count(right, theme.text), 0);
        assert_eq!(
            f.pixel(capsule.x + 6, capsule.y + 3),
            theme.fill.over(theme.chrome)
        );
    }

    #[test]
    fn dragging_the_corner_resizes_and_the_button_maximises() {
        let mut f = Fixture::new();
        let id = f.open("w", RED);
        let frame = f.desktop.window_frame(id).unwrap();
        // Drag the bottom-right corner out by (60, 40).
        f.move_to(frame.right() - 4, frame.bottom() - 4);
        f.desktop.mouse(0, 0, 1);
        f.desktop.mouse(60, 40, 1);
        f.desktop.mouse(0, 0, 0);
        let larger = f.desktop.window_frame(id).unwrap();
        assert_eq!((larger.x, larger.y), (frame.x, frame.y));
        assert_eq!((larger.w, larger.h), (frame.w + 60, frame.h + 40));
        assert!(
            f.log.borrow().clicks.is_empty(),
            "the corner is not content"
        );
        // In again, further than the minimum allows.
        f.move_to(larger.right() - 4, larger.bottom() - 4);
        f.desktop.mouse(0, 0, 1);
        f.desktop.mouse(-1000, -1000, 1);
        f.desktop.mouse(0, 0, 0);
        let smallest = f.desktop.window_frame(id).unwrap();
        assert_eq!(
            (smallest.w, smallest.h),
            (MIN_CONTENT.0 + 2, MIN_CONTENT.1 + 2 + TITLE_HEIGHT)
        );
        // Not below the task bar.
        f.move_to(smallest.right() - 4, smallest.bottom() - 4);
        f.desktop.mouse(0, 0, 1);
        f.desktop.mouse(100, 5000, 1);
        f.desktop.mouse(0, 0, 0);
        let tall = f.desktop.window_frame(id).unwrap();
        assert_eq!(tall.bottom(), H as i32 - TASKBAR_HEIGHT);

        // Maximise: the whole screen above the task bar, square corners,
        // no shadow; the button then shows "restore".
        let button = f.desktop.control_frame(id, Control::Maximise).unwrap();
        f.click_in(button);
        let full = Rect::new(0, 0, W as i32, H as i32 - TASKBAR_HEIGHT);
        assert_eq!(f.desktop.window_frame(id), Some(full));
        f.move_to(500, 300);
        f.render();
        let theme = *f.desktop.theme();
        assert_eq!(f.pixel(300, 3), theme.chrome);
        assert_eq!(f.pixel(3, 3), theme.chrome, "no rounding when maximised");
        assert_eq!(f.pixel(3, full.bottom() - 3), RED);
        // It cannot be dragged or resized while maximised.
        f.move_to(300, 10);
        f.desktop.mouse(0, 0, 1);
        f.desktop.mouse(50, 50, 1);
        f.desktop.mouse(0, 0, 0);
        assert_eq!(f.desktop.window_frame(id), Some(full));
        assert_eq!(f.desktop.windows[0].grip(), None);
        // And back.
        let button = f.desktop.control_frame(id, Control::Maximise).unwrap();
        f.click_in(button);
        assert_eq!(f.desktop.window_frame(id), Some(tall));
    }

    #[test]
    fn fixed_size_windows_have_no_grip_or_maximise_button() {
        let mut f = Fixture::new();
        let app = f.probe("fixed", RED, true);
        let id = f.desktop.open(app, 240, 120);
        let frame = f.desktop.window_frame(id).unwrap();
        assert_eq!(f.desktop.control_frame(id, Control::Maximise), None);
        assert!(f.desktop.control_frame(id, Control::Minimise).is_some());
        assert!(f.desktop.control_frame(id, Control::Close).is_some());
        assert_eq!(f.desktop.windows[0].grip(), None);
        // A press in the corner is an ordinary click in the contents.
        f.click_at(frame.right() - 4, frame.bottom() - 4);
        assert_eq!(f.desktop.window_frame(id), Some(frame));
        assert_eq!(f.log.borrow().clicks.len(), 1);
    }

    #[test]
    fn clicking_raises_and_focuses() {
        let mut f = Fixture::new();
        let back = f.open("back", RED);
        let front = f.open("front", GREEN);
        f.desktop.key(0x61);
        assert_eq!(f.log.borrow().keys, [("front".into(), 0x61)]);
        // Click a part of the back window that the front one leaves visible.
        let frame = f.desktop.window_frame(back).unwrap();
        f.click_at(frame.x + 6, frame.y + 80);
        assert_eq!(f.desktop.titles(), ["front", "back"]);
        assert_eq!(f.desktop.front(), Some(back));
        f.desktop.key(0x62);
        assert_eq!(f.log.borrow().keys[1], ("back".into(), 0x62));
        // The click itself reached the app, in content coordinates.
        let content = f.desktop.windows[1].content();
        assert_eq!(
            f.log.borrow().clicks,
            [(
                "back".into(),
                frame.x + 6 - content.x,
                frame.y + 80 - content.y
            )]
        );
        // Where the windows overlap, the front one gets the click.
        let overlap = f.desktop.window_frame(front).unwrap();
        f.click_at(overlap.x + 60, overlap.y + 80);
        assert_eq!(f.desktop.front(), Some(back), "back is on top there now");
        // Clicks on the wallpaper do nothing; keys without windows neither.
        f.click_at(700, 60);
        assert_eq!(f.desktop.window_count(), 2);
        let mut empty = Fixture::new();
        empty.desktop.key(0x61);
        empty.click_at(100, 100);
    }

    #[test]
    fn dragging_the_title_bar_moves_the_window() {
        let mut f = Fixture::new();
        let id = f.open("w", RED);
        let frame = f.desktop.window_frame(id).unwrap();
        f.move_to(frame.x + 60, frame.y + 10);
        f.desktop.mouse(0, 0, 1);
        f.desktop.mouse(30, 20, 1);
        f.desktop.mouse(15, -5, 1);
        assert_eq!(
            f.desktop.window_frame(id),
            Some(Rect::new(frame.x + 45, frame.y + 15, frame.w, frame.h))
        );
        // After release the window stays put.
        f.desktop.mouse(0, 0, 0);
        f.desktop.mouse(50, 50, 0);
        assert_eq!(f.desktop.window_frame(id).unwrap().x, frame.x + 45);
        assert!(f.log.borrow().clicks.is_empty());
        // The image follows: content at the new place, wallpaper (under
        // the shadow at most) at the old.
        f.move_to(700, 500);
        f.render();
        let content = f.desktop.windows[0].content();
        assert_eq!(f.pixel(content.x + 5, content.y + 5), RED);
        assert_ne!(f.pixel(frame.x + 2, frame.y + 60), RED);
    }

    #[test]
    fn windows_cannot_be_dragged_out_of_reach() {
        let mut f = Fixture::new();
        let id = f.open("w", RED);
        let frame = f.desktop.window_frame(id).unwrap();
        let grab = |f: &mut Fixture| {
            // (Letting go at a corner of the screen arranges the window;
            // take it from where it is after that.)
            f.desktop.mouse(0, 0, 0);
            let frame = f.desktop.window_frame(id).unwrap();
            f.move_to(
                (frame.x + 60).clamp(0, W as i32 - 1),
                (frame.y + 10).clamp(0, H as i32 - 1),
            );
            f.desktop.mouse(0, 0, 1);
        };
        grab(&mut f);
        f.desktop.mouse(-5000, -5000, 1);
        let moved = f.desktop.window_frame(id).unwrap();
        assert_eq!(moved.y, 0, "the title bar stays on screen");
        assert!(moved.right() >= KEEP_VISIBLE);
        grab(&mut f);
        f.desktop.mouse(5000, 5000, 1);
        let moved = f.desktop.window_frame(id).unwrap();
        assert!(moved.x <= W as i32 - KEEP_VISIBLE);
        assert_eq!(moved.y, H as i32 - TASKBAR_HEIGHT - TITLE_HEIGHT);
        assert_eq!((moved.w, moved.h), (frame.w, frame.h));
    }

    #[test]
    fn close_button_and_requests() {
        let mut f = Fixture::new();
        let a = f.open("a", RED);
        let b = f.open("b", GREEN);
        let close = f.desktop.control_frame(b, Control::Close).unwrap();
        f.click_in(close);
        assert_eq!(f.desktop.titles(), ["a"]);
        assert!(f.log.borrow().clicks.is_empty());
        // An app can open a window, and close its own.
        f.desktop.key(0x6E);
        assert_eq!(f.desktop.titles(), ["a", "child"]);
        f.desktop.key(0x71);
        assert_eq!(f.desktop.titles(), ["a"]);
        assert_eq!(f.desktop.front(), Some(a));
        // And ask for the desktop to end.
        assert!(!f.desktop.quit_requested());
        f.desktop.key(0x51);
        assert!(f.desktop.quit_requested());
        f.desktop.key(0x71);
        assert_eq!(f.desktop.window_count(), 0);
    }

    #[test]
    fn a_task_bar_icon_starts_raises_and_minimises_its_program() {
        let mut f = Fixture::new();
        f.add_program("Files", "drive");
        f.add_program("Run", "term");
        let files = f.desktop.launcher_frame(0).unwrap();
        let run = f.desktop.launcher_frame(1).unwrap();
        assert!(run.x >= files.right());

        // Nothing running: the icon starts the program.
        f.click_in(files);
        assert_eq!(f.log.borrow().launched, ["Files"]);
        assert_eq!(f.desktop.titles(), ["Files window"]);
        let files_window = f.desktop.front().unwrap();
        f.click_in(run);
        assert_eq!(f.desktop.titles(), ["Files window", "Run window"]);
        let run_window = f.desktop.front().unwrap();

        // Marks: an accent bar under the program in front, a dot under
        // the other one that is running.
        f.move_to(600, 300);
        f.render();
        let theme = *f.desktop.theme();
        let marks = |button: Rect| Rect::new(button.x, button.bottom() - 7, button.w, 6);
        assert!(f.count(marks(run), theme.accent_text) >= 14 * 2);
        assert_eq!(f.count(marks(files), theme.accent_text), 0);
        let dot = (files.x + files.w / 2, files.bottom() - 4);
        assert_eq!(
            f.pixel(dot.0, dot.1),
            theme
                .running
                .over(theme.bar.over(f.wallpaper(dot.0, dot.1)))
        );

        // Running but not in front: the icon brings its window forward
        // and does not start another.
        f.click_in(files);
        assert_eq!(f.log.borrow().launched, ["Files", "Run"]);
        assert_eq!(f.desktop.front(), Some(files_window));
        // In front: the icon minimises it; the other window is in front.
        f.click_in(files);
        assert!(f.desktop.is_minimised(files_window));
        assert_eq!(f.desktop.front(), Some(run_window));
        assert_eq!(f.desktop.window_count(), 2);
        // Minimised: the icon brings it back, to the front.
        f.click_in(files);
        assert!(!f.desktop.is_minimised(files_window));
        assert_eq!(f.desktop.front(), Some(files_window));
        // Clicks on the bar between icons do nothing.
        f.click_at(600, H as i32 - 20);
        assert_eq!(f.log.borrow().launched.len(), 2);
    }

    #[test]
    fn a_minimised_window_is_off_the_screen_but_still_there() {
        let mut f = Fixture::new();
        let below = f.open("below", RED);
        let ticker = f.open("ticker", GREEN);
        let button = f.desktop.control_frame(ticker, Control::Minimise).unwrap();
        let content = f.desktop.windows[1].content();
        f.click_in(button);
        assert!(f.desktop.is_minimised(ticker));
        assert_eq!(f.desktop.window_count(), 2);
        assert_eq!(f.desktop.front(), Some(below));
        f.move_to(700, 500);
        f.render();
        // Not drawn.
        assert_eq!(f.count(Rect::new(0, 0, W as i32, H as i32), GREEN), 0);
        // Not clickable: a click where it was goes to what is there now.
        let draws = f.log.borrow().draws;
        f.click_at(content.right() - 5, content.bottom() - 5);
        assert!(
            f.log
                .borrow()
                .clicks
                .iter()
                .all(|(name, _, _)| name == "below")
        );
        // Keys go to the window in front.
        f.desktop.key(0x61);
        assert_eq!(f.log.borrow().keys, [("below".into(), 0x61)]);
        // It still ticks, without anything being repainted for it.
        f.render();
        let draws_before = f.log.borrow().draws;
        assert!(draws_before >= draws);
        f.desktop.tick();
        assert!(f.render().is_empty());
        // With every window minimised there is nobody to type to.
        let button = f.desktop.control_frame(below, Control::Minimise).unwrap();
        f.click_in(button);
        assert_eq!(f.desktop.front(), None);
        f.desktop.key(0x62);
        assert_eq!(f.log.borrow().keys.len(), 1);
    }

    #[test]
    fn the_start_panel_lists_and_starts_programs() {
        let mut f = Fixture::new();
        f.add_program("Files", "drive");
        f.add_program("Run", "term");
        f.add_program("About", "info");
        assert!(!f.desktop.start_is_open());
        assert_eq!(f.desktop.start_item_frame(0), None);
        assert_eq!(f.desktop.power_button_frame(), None);
        f.render();
        let closed = f.surface.clone();

        f.click_in(f.desktop.start_button());
        assert!(f.desktop.start_is_open());
        assert!(f.log.borrow().launched.is_empty());
        f.move_to(700, 100);
        f.render();
        let theme = *f.desktop.theme();
        // One row per program, each with its name written in it.
        let rows: Vec<Rect> = (0..3)
            .map(|i| f.desktop.start_item_frame(i).unwrap())
            .collect();
        assert_eq!(f.desktop.start_item_frame(3), None);
        for pair in rows.windows(2) {
            assert_eq!(pair[1].y, pair[0].bottom());
        }
        // The first is the one Enter would start: filled with the accent,
        // its name in white. The others are plain.
        assert!(f.count(rows[0], theme.accent) > 1000);
        assert!(f.count(rows[0], theme.on_accent) > 10);
        for row in &rows[1..] {
            assert!(f.count(*row, theme.text) > 10, "{row:?}");
            assert_eq!(f.count(*row, theme.accent), 0);
        }
        let panel = f.desktop.start_panel();
        assert!(panel.bottom() < H as i32 - TASKBAR_HEIGHT && panel.y > 0);
        // The row under the pointer is filled with the accent.
        f.move_to(rows[1].x + 100, rows[1].y + 10);
        f.render();
        assert!(f.count(rows[1], theme.accent) > 1000);
        assert_eq!(f.count(rows[0], theme.accent), 0);
        // Keys do not reach programs while the panel is open.
        f.open("w", RED);
        f.desktop.key(0x101);
        assert!(f.log.borrow().keys.is_empty());

        // Choosing a row starts that program and closes the panel.
        f.click_in(rows[1]);
        assert_eq!(f.log.borrow().launched, ["Run"]);
        assert!(!f.desktop.start_is_open());
        assert_eq!(f.desktop.titles(), ["w", "Run window"]);

        // Escape closes it; so does a press elsewhere, or on the mark again.
        f.click_in(f.desktop.start_button());
        f.desktop.key(0x1B);
        assert!(!f.desktop.start_is_open());
        f.click_in(f.desktop.start_button());
        f.click_at(700, 60);
        assert!(!f.desktop.start_is_open());
        f.click_in(f.desktop.start_button());
        f.click_in(f.desktop.start_button());
        assert!(!f.desktop.start_is_open());
        assert_eq!(f.log.borrow().launched.len(), 1);

        // Closed again, with no windows, the screen is as it was before.
        let ids: Vec<WindowId> = f.desktop.windows.iter().map(|w| w.id).collect();
        for id in ids {
            f.desktop.close(id);
        }
        let at = closed.pixel(0, 0);
        f.move_to(400, 300);
        f.render();
        assert_eq!(f.surface.pixel(0, 0), at);
        assert!(f.surface == closed);

        // The power button ends the desktop.
        f.click_in(f.desktop.start_button());
        let power = f.desktop.power_button_frame().unwrap();
        f.move_to(power.x + 5, power.y + 5);
        f.render();
        assert!(f.count(power, theme.danger) > 500);
        assert!(!f.desktop.quit_requested());
        f.click_in(power);
        assert!(f.desktop.quit_requested());
    }

    #[test]
    fn what_the_pointer_is_over_is_shown() {
        let mut f = Fixture::new();
        f.add_program("Files", "drive");
        let id = f.open("w", RED);
        f.move_to(700, 300);
        f.render();
        let theme = *f.desktop.theme();
        let plain = f.surface.clone();

        // The close button turns red; the others get a quiet fill.
        let close = f.desktop.control_frame(id, Control::Close).unwrap();
        f.move_to(close.x + 10, close.y + 10);
        f.render();
        assert!(f.count(close, theme.danger) > 300);
        let minimise = f.desktop.control_frame(id, Control::Minimise).unwrap();
        f.move_to(minimise.x + 10, minimise.y + 10);
        f.render();
        assert_eq!(f.count(close, theme.danger), 0);
        let quiet = theme.fill_2.over(theme.fill.over(theme.chrome));
        assert!(f.count(minimise, quiet) > 300);

        // A task bar icon gets a soft fill and its label above it.
        let button = f.desktop.launcher_frame(0).unwrap();
        f.move_to(button.x + 30, button.y + 30);
        f.render();
        let label = f.desktop.tooltip(0).inset(8);
        assert!(label.bottom() <= H as i32 - TASKBAR_HEIGHT);
        assert!(f.changed(label, &plain) > 200);
        // Its text is dark on light: readable.
        let dark = (label.y..label.bottom())
            .flat_map(|y| (label.x..label.right()).map(move |x| (x, y)))
            .filter(|&(x, y)| (f.pixel(x, y) & 0xFF) < 0x80)
            .count();
        assert!(dark > 15, "{dark}");
        // The Start mark too.
        let start = f.desktop.start_button();
        f.move_to(start.x + 30, start.y + 30);
        f.render();
        assert_eq!(f.changed(label, &plain), 0, "the label is gone again");
        let (x, y) = (start.x + 3, start.y + 20);
        assert_eq!(
            f.pixel(x, y),
            theme.hover.over(theme.bar.over(f.wallpaper(x, y)))
        );
        // Away from everything, the screen is exactly as before.
        f.move_to(700, 300);
        f.render();
        assert!(f.surface == plain);
        // Nothing is highlighted while a window is being dragged over it.
        let bar = f.desktop.windows[0].title_bar();
        f.move_to(bar.x + 60, bar.y + 10);
        f.desktop.mouse(0, 0, 1);
        f.move_to(button.x + 20, H as i32 - TASKBAR_HEIGHT - 20);
        assert_eq!(f.desktop.hover, Hover::Nothing);
        f.desktop.mouse(0, 0, 0);
    }

    #[test]
    fn themes_change_every_colour_and_reach_the_apps() {
        let mut f = Fixture::new();
        f.add_program("Files", "drive");
        let id = f.open("Files", RED);
        f.move_to(700, 300);
        f.render();
        assert_eq!(f.log.borrow().modes.last(), Some(&Mode::Light));
        let light = f.surface.clone();

        let dark = Theme::new(Mode::Dark, Accent::Rose);
        f.desktop.set_theme(dark);
        assert_eq!(f.render(), Rect::new(0, 0, W as i32, H as i32));
        assert_eq!(f.log.borrow().modes.last(), Some(&Mode::Dark));
        assert_eq!(f.pixel(0, 0), wallpaper_at(&dark, W as i32, H as i32, 0, 0));
        assert_ne!(f.pixel(0, 0), light.pixel(0, 0).unwrap());
        let frame = f.desktop.window_frame(id).unwrap();
        assert_eq!(f.pixel(frame.x + frame.w / 2, frame.y + 4), dark.chrome);
        let bar = f.desktop.windows[0].title_bar();
        assert!(f.count(bar, dark.text) > 10);
        // The accent: the Start mark and the bar under the running program.
        assert!(f.count(f.desktop.start_button(), dark.accent_text) > 20);
        let button = f.desktop.launcher_frame(0).unwrap();
        assert!(f.count(button, dark.accent_text) >= 28);
        // Setting the same theme again repaints nothing.
        f.desktop.set_theme(dark);
        assert!(f.render().is_empty());
        // And back: the very same image as before.
        f.desktop.set_theme(Theme::default());
        f.render();
        assert!(f.surface == light);
    }

    #[test]
    fn only_changed_areas_are_repainted() {
        let mut f = Fixture::new();
        f.open("still", RED);
        let ticker = f.open("ticker", GREEN);
        f.render();
        let draws = f.log.borrow().draws;

        // Moving the pointer over empty desktop repaints just around it.
        f.move_to(700, 20);
        f.render();
        f.desktop.mouse(3, 2, 0);
        let dirty = f.render();
        assert!(
            dirty.w <= POINTER_WIDTH + 3 && dirty.h <= POINTER_HEIGHT + 2,
            "{dirty:?}"
        );
        assert_eq!(f.log.borrow().draws, draws, "no window was redrawn");
        // The old pointer image is gone, the new one is there.
        assert_eq!(f.pixel(700, 20), f.wallpaper(700, 20));
        assert_eq!(f.pixel(703, 22), POINTER_EDGE);

        // A tick repaints the contents of the app that asked for it.
        f.desktop.tick();
        let dirty = f.render();
        let index = f.desktop.index_of(ticker).unwrap();
        assert_eq!(dirty, f.desktop.windows[index].content());

        // A new time repaints the clock only; the same time, nothing.
        f.desktop.set_clock("0:01", "");
        assert_eq!(f.render(), f.desktop.clock_area());
        f.desktop.set_clock("0:01", "");
        assert!(f.render().is_empty());
    }

    #[test]
    fn incremental_rendering_matches_a_full_repaint() {
        // After any sequence of events, the incrementally updated image
        // must equal one painted from scratch.
        let mut f = Fixture::new();
        f.add_program("a", "drive");
        f.add_program("ticker", "clock");
        f.add_program("Other", "term");
        f.open("a", RED);
        f.open("ticker", GREEN);
        let fixed = f.probe("fixed", BLUE, true);
        f.desktop.open(fixed, 200, 90);
        f.render();
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        let mut next = move |limit: i32| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % limit as u64) as i32
        };
        let themes = [
            Theme::default(),
            Theme::new(Mode::Dark, Accent::Blue),
            Theme::new(Mode::Light, Accent::Amber),
        ];
        for step in 0..1500 {
            match next(16) {
                0..=3 => f.desktop.mouse(next(300) - 150, next(300) - 150, 0),
                4..=6 => f.desktop.mouse(next(80) - 40, next(80) - 40, 1),
                7 | 8 => f.desktop.mouse(0, 0, 0),
                9 => f.desktop.key(0x61),
                10 => f.desktop.tick(),
                11 => f.desktop.set_clock(&format!("{step}"), "Tue 10 Nov"),
                // Somewhere along the task bar: Start, icons, or nothing.
                12 | 13 => {
                    let x = next(260);
                    f.click_at(x, H as i32 - 26);
                }
                14 => {
                    // A window's buttons.
                    if let Some(id) = f.desktop.front() {
                        let which = [Control::Minimise, Control::Maximise, Control::Minimise]
                            [next(3) as usize];
                        if let Some(button) = f.desktop.control_frame(id, which) {
                            f.move_to(button.x + 10, button.y + 10);
                            if next(2) == 0 {
                                f.desktop.mouse(0, 0, 1);
                                f.desktop.mouse(0, 0, 0);
                            }
                        }
                    }
                }
                _ => match next(8) {
                    0 => f.desktop.set_theme(themes[next(3) as usize]),
                    1 => f
                        .desktop
                        .set_alignment([Alignment::Left, Alignment::Centre][next(2) as usize]),
                    // Typed into the Start panel when it is open.
                    2 | 3 => f
                        .desktop
                        .key([0x74, 0x69, 127, 0x102, 0x101, 13][next(6) as usize]),
                    _ => {}
                },
            }
            f.render();
            if step % 25 == 0 {
                let fresh = f.repainted();
                assert!(fresh == f.surface, "mismatch after step {step}");
            }
        }
        assert!(!f.desktop.quit_requested());
    }

    #[test]
    fn typing_in_the_start_panel_searches_and_enter_starts() {
        let mut f = Fixture::new();
        for (label, icon) in [
            ("Files", "drive"),
            ("Run", "term"),
            ("Scribble", "brush"),
            ("Clock", "clock"),
        ] {
            f.add_program(label, icon);
        }
        f.click_in(f.desktop.start_button());
        f.move_to(700, 100);
        assert_eq!(f.desktop.start_query(), "");
        assert_eq!(f.desktop.start_matches(), [0, 1, 2, 3]);
        f.render();
        let empty = f.surface.clone();

        // Typing narrows the list, whatever the case of the letters.
        f.desktop.key(i32::from(b'C'));
        assert_eq!(f.desktop.start_query(), "C");
        assert_eq!(f.desktop.start_matches(), [2, 3], "Scribble and Clock");
        assert!(f.desktop.start_item_frame(1).is_some());
        assert_eq!(f.desktop.start_item_frame(2), None);
        f.render();
        let field = f.desktop.start_field();
        assert!(
            f.changed(field, &empty) > 30,
            "the field shows what was typed"
        );
        f.desktop.key(i32::from(b'l'));
        assert_eq!(f.desktop.start_matches(), [3]);
        // Nothing matches: the list says so and Enter starts nothing.
        f.desktop.key(i32::from(b'x'));
        assert!(f.desktop.start_matches().is_empty());
        f.render();
        // Backspace takes a letter away again.
        f.desktop.key(127);
        assert_eq!(f.desktop.start_query(), "Cl");
        // Enter starts the program listed first, and closes the panel.
        f.desktop.key(13);
        assert_eq!(f.log.borrow().launched, ["Clock"]);
        assert!(!f.desktop.start_is_open());

        // Opened again, the field is empty; the arrows choose.
        f.click_in(f.desktop.start_button());
        assert_eq!(f.desktop.start_query(), "");
        f.desktop.key(0x102);
        f.desktop.key(0x102);
        f.desktop.key(0x101);
        f.desktop.key(13);
        assert_eq!(f.log.borrow().launched, ["Clock", "Run"]);
        // The arrows stop at the ends of the list.
        f.click_in(f.desktop.start_button());
        for _ in 0..10 {
            f.desktop.key(0x102);
        }
        f.desktop.key(13);
        assert_eq!(f.log.borrow().launched.last(), Some(&"Clock"));
        f.click_in(f.desktop.start_button());
        f.desktop.key(0x101);
        f.desktop.key(13);
        assert_eq!(f.log.borrow().launched.last(), Some(&"Files"));

        // A click in the list starts the program in that row of what is
        // listed, not of all programs.
        f.click_in(f.desktop.start_button());
        f.desktop.key(i32::from(b'r'));
        assert_eq!(f.desktop.start_matches(), [1, 2], "Run and Scribble");
        let second = f.desktop.start_item_frame(1).unwrap();
        f.click_in(second);
        assert_eq!(f.log.borrow().launched.last(), Some(&"Scribble"));
        // A click in the field keeps the panel open; Enter on an empty
        // list just closes it; very long input is cut off.
        f.click_in(f.desktop.start_button());
        f.click_in(f.desktop.start_field());
        assert!(f.desktop.start_is_open());
        for _ in 0..100 {
            f.desktop.key(i32::from(b'z'));
        }
        assert_eq!(f.desktop.start_query().len(), 32);
        let before = f.log.borrow().launched.len();
        f.desktop.key(13);
        assert!(!f.desktop.start_is_open());
        assert_eq!(f.log.borrow().launched.len(), before);
        // With no windows of probes open, no key ever reached a program.
        assert!(f.log.borrow().keys.is_empty());
    }

    #[test]
    fn the_task_bar_icons_can_sit_in_the_middle() {
        let mut f = Fixture::new();
        f.add_program("Files", "drive");
        f.add_program("Run", "term");
        f.add_program("About", "info");
        let left: Vec<Rect> = (0..3)
            .map(|i| f.desktop.launcher_frame(i).unwrap())
            .collect();
        f.move_to(700, 300);
        f.render();
        assert_eq!(f.desktop.alignment(), Alignment::Left);

        f.desktop.set_alignment(Alignment::Centre);
        let centre: Vec<Rect> = (0..3)
            .map(|i| f.desktop.launcher_frame(i).unwrap())
            .collect();
        // The group is centred on the screen; the Start mark stays put.
        assert_eq!(
            centre[0].x + (centre[2].right() - centre[0].x) / 2,
            W as i32 / 2
        );
        assert_eq!(centre[1].x - centre[0].x, left[1].x - left[0].x);
        assert_eq!(f.desktop.start_button().x, 10);
        assert_eq!(f.render(), f.desktop.taskbar());
        // The icons are drawn where they now are, and clicks follow.
        let theme = *f.desktop.theme();
        assert!(f.count(centre[0], theme.text) > 10);
        assert_eq!(f.count(left[1], theme.text), 0);
        f.click_in(centre[1]);
        assert_eq!(f.log.borrow().launched, ["Run"]);
        f.click_in(left[0]);
        assert_eq!(f.log.borrow().launched.len(), 1);
        // Setting it again changes nothing.
        f.move_to(700, 300);
        f.render();
        f.desktop.set_alignment(Alignment::Centre);
        assert!(f.render().is_empty());
        // With more icons than fit in the middle they start after the
        // Start mark rather than under it.
        let mut narrow = Desktop::new(300, 200);
        for _ in 0..6 {
            narrow.add_launcher("x", "doc", |_| {});
        }
        narrow.set_alignment(Alignment::Centre);
        assert!(narrow.launcher_frame(0).unwrap().x >= narrow.start_button().right());
    }

    #[test]
    fn an_app_can_change_the_theme_and_the_alignment() {
        struct Chooser;
        impl App for Chooser {
            fn title(&self) -> &str {
                "Chooser"
            }
            fn draw(&mut self, _: &mut Canvas<'_>, _: i32, _: i32, _: &Theme) {}
            fn key(&mut self, code: i32, context: &mut Context) {
                match code {
                    0x64 => context.set_theme(Theme::new(Mode::Dark, Accent::Violet)),
                    0x63 => context.set_alignment(Alignment::Centre),
                    _ => {}
                }
            }
        }
        let mut f = Fixture::new();
        f.desktop.open(Box::new(Chooser), 200, 100);
        f.render();
        f.desktop.key(0x64);
        assert_eq!(f.desktop.theme().mode, Mode::Dark);
        assert_eq!(f.desktop.theme().accent_name, Accent::Violet);
        assert_eq!(f.render(), Rect::new(0, 0, W as i32, H as i32));
        f.desktop.key(0x63);
        assert_eq!(f.desktop.alignment(), Alignment::Centre);
    }

    #[test]
    fn programs_with_a_picture_of_their_own_show_it() {
        let mut f = Fixture::new();
        // "files" has a picture in the design system; "info" has only a
        // line icon.
        f.add_program("Files", "files");
        f.add_program("About", "info");
        f.move_to(700, 300);
        f.render();
        let theme = *f.desktop.theme();
        let files = f.desktop.launcher_frame(0).unwrap();
        let about = f.desktop.launcher_frame(1).unwrap();
        // The folder's yellows, and no line icon drawn over them.
        let yellow = |area: Rect| {
            (area.y..area.bottom())
                .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let p = f.pixel(x, y);
                    (p >> 16) > 0xD0 && ((p >> 8) & 0xFF) > 0x90 && (p & 0xFF) < 0xA0
                })
                .count()
        };
        assert!(yellow(files) > 150, "{}", yellow(files));
        assert_eq!(f.count(files, theme.text), 0);
        assert_eq!(yellow(about), 0);
        assert!(f.count(about, theme.text) > 10);
        // The picture is centred in its button, and is the picture.
        let picture = art::icon("files", 30).unwrap();
        let (colour, alpha) = picture.pixel(15, 20);
        assert_eq!(alpha, 255);
        assert_eq!(f.pixel(files.x + 6 + 15, files.y + 6 + 20), colour);
    }

    #[test]
    fn the_wallpaper_is_the_picture_for_the_mode_scaled_to_the_screen() {
        let mut f = Fixture::new();
        f.move_to(400, 590);
        f.render();
        let light = Theme::default();
        let picture = art::wallpaper(Mode::Light).unwrap();
        // Every pixel above the task bar that the pointer does not cover.
        for y in (0..H as i32 - TASKBAR_HEIGHT).step_by(7) {
            for x in (0..W as i32).step_by(11) {
                assert_eq!(
                    f.pixel(x, y),
                    picture.scaled(W as i32, H as i32, x, y),
                    "({x}, {y})"
                );
            }
        }
        // The top-left corner is the picture's (it is cut at the sides
        // or top and bottom only if the shapes differ: 800 x 600 is
        // squarer than the picture, so the sides are cut).
        assert_eq!(
            f.wallpaper(400, 0),
            picture.scaled(W as i32, H as i32, 400, 0)
        );
        let dark = Theme::new(Mode::Dark, Accent::Blue);
        f.desktop.set_theme(dark);
        f.render();
        let dusk = art::wallpaper(Mode::Dark).unwrap();
        assert_eq!(f.pixel(100, 100), dusk.scaled(W as i32, H as i32, 100, 100));
        assert_ne!(
            wallpaper_at(&light, 64, 48, 10, 10),
            wallpaper_at(&dark, 64, 48, 10, 10)
        );
    }

    /// Drags the window `id` by its title bar until the pointer is at
    /// (`x`, `y`), and lets go.
    fn drag_to(f: &mut Fixture, id: WindowId, x: i32, y: i32) {
        let frame = f.desktop.window_frame(id).unwrap();
        f.desktop.mouse(0, 0, 0);
        f.move_to(frame.x + 60, frame.y + 10);
        f.desktop.mouse(0, 0, 1);
        f.move_to(x, y);
        f.desktop.mouse(0, 0, 0);
    }

    #[test]
    fn a_window_let_go_at_a_side_of_the_screen_takes_that_half() {
        let mut f = Fixture::new();
        let a = f.open("a", RED);
        let b = f.open("b", GREEN);
        let own = f.desktop.window_frame(a).unwrap();
        let height = H as i32 - TASKBAR_HEIGHT;

        // (The one in front first: the other would cover it.)
        drag_to(&mut f, b, W as i32 - 1, 300);
        let right = f.desktop.window_frame(b).unwrap();
        drag_to(&mut f, a, 0, 200);
        let left = f.desktop.window_frame(a).unwrap();
        assert_eq!(left, Rect::new(8, 8, (W as i32 - 24) / 2, height - 16));
        // Side by side, the same size, gutters between and around.
        assert_eq!((right.w, right.h, right.y), (left.w, left.h, left.y));
        assert_eq!(right.x - left.right(), 8);
        assert_eq!(W as i32 - right.right(), 8);
        // They keep their rounded corners and their grips.
        f.move_to(400, 590);
        f.render();
        assert_ne!(f.pixel(left.x, left.y), f.desktop.theme().chrome);
        assert!(f.desktop.windows[0].grip().is_some());

        // Dragged away again, a window has its own size back, and is
        // under the pointer where it was taken.
        let left = f.desktop.window_frame(a).unwrap();
        f.move_to(left.x + 200, left.y + 10);
        f.desktop.mouse(0, 0, 1);
        f.desktop.mouse(30, 40, 1);
        let free = f.desktop.window_frame(a).unwrap();
        assert_eq!((free.w, free.h), (own.w, own.h));
        assert!(free.x <= f.desktop.pointer().x && f.desktop.pointer().x < free.right());
        assert_eq!(free.y, left.y + 40);
        f.desktop.mouse(0, 0, 0);
        assert_eq!(f.desktop.window_frame(a), Some(free));
        // Straight from one side to the other: still its own size after.
        drag_to(&mut f, a, 0, 100);
        drag_to(&mut f, a, W as i32 - 1, 100);
        assert_eq!(f.desktop.window_frame(a), Some(right));
        drag_to(&mut f, a, 300, 200);
        let free = f.desktop.window_frame(a).unwrap();
        assert_eq!((free.w, free.h), (own.w, own.h));
    }

    #[test]
    fn a_window_let_go_at_the_top_is_maximised() {
        let mut f = Fixture::new();
        let id = f.open("w", RED);
        let own = f.desktop.window_frame(id).unwrap();
        drag_to(&mut f, id, 400, 0);
        let full = Rect::new(0, 0, W as i32, H as i32 - TASKBAR_HEIGHT);
        assert_eq!(f.desktop.window_frame(id), Some(full));
        // The button puts it back at its own size.
        let button = f.desktop.control_frame(id, Control::Maximise).unwrap();
        f.click_in(button);
        let back = f.desktop.window_frame(id).unwrap();
        assert_eq!((back.w, back.h), (own.w, own.h));
        assert!(back.x >= 0 && back.right() <= W as i32);
    }

    #[test]
    fn the_place_a_dragged_window_would_take_is_shown_while_dragging() {
        let mut f = Fixture::new();
        let id = f.open("w", RED);
        f.move_to(700, 300);
        f.render();
        let plain = f.surface.clone();
        let frame = f.desktop.window_frame(id).unwrap();
        f.move_to(frame.x + 60, frame.y + 10);
        f.desktop.mouse(0, 0, 1);
        // In the middle of the screen: nothing is shown.
        f.move_to(300, 200);
        assert_eq!(f.desktop.zone, None);
        // At the right edge: the right half is tinted with the accent.
        f.move_to(W as i32 - 1, 200);
        assert_eq!(f.desktop.zone, Some(Zone::Right));
        f.render();
        let zone = f.desktop.zone_frame(Zone::Right);
        let probe = (zone.x + 20, zone.bottom() - 20);
        let theme = *f.desktop.theme();
        assert_eq!(
            f.pixel(probe.0, probe.1),
            mix(theme.accent, plain.pixel(probe.0, probe.1).unwrap(), 46)
        );
        // Moved off the edge without letting go: the tint goes, and the
        // window is not arranged.
        f.move_to(500, 200);
        assert_eq!(f.desktop.zone, None);
        f.render();
        assert_eq!(
            f.pixel(probe.0, probe.1),
            plain.pixel(probe.0, probe.1).unwrap()
        );
        f.desktop.mouse(0, 0, 0);
        assert_eq!(f.desktop.window_frame(id).unwrap().w, frame.w);
        // The top edge shows the whole screen.
        f.move_to(
            f.desktop.window_frame(id).unwrap().x + 60,
            f.desktop.window_frame(id).unwrap().y + 10,
        );
        f.desktop.mouse(0, 0, 1);
        f.move_to(400, 0);
        assert_eq!(f.desktop.zone, Some(Zone::Whole));
        f.desktop.mouse(0, 0, 0);
        assert_eq!(f.desktop.zone, None);
    }

    #[test]
    fn windows_of_a_fixed_size_are_not_arranged_and_resizing_ends_it() {
        let mut f = Fixture::new();
        let app = f.probe("fixed", RED, true);
        let fixed = f.desktop.open(app, 240, 120);
        let before = f.desktop.window_frame(fixed).unwrap();
        drag_to(&mut f, fixed, 0, 200);
        let after = f.desktop.window_frame(fixed).unwrap();
        assert_eq!((after.w, after.h), (before.w, before.h));
        assert_eq!(f.desktop.zone, None);

        // An arranged window resized by its corner is an ordinary
        // window again: dragging it does not change its size.
        let id = f.open("w", GREEN);
        drag_to(&mut f, id, 0, 200);
        let half = f.desktop.window_frame(id).unwrap();
        f.move_to(half.right() - 4, half.bottom() - 4);
        f.desktop.mouse(0, 0, 1);
        f.desktop.mouse(-50, -60, 1);
        f.desktop.mouse(0, 0, 0);
        let resized = f.desktop.window_frame(id).unwrap();
        assert_eq!((resized.w, resized.h), (half.w - 50, half.h - 60));
        drag_to(&mut f, id, 300, 300);
        let moved = f.desktop.window_frame(id).unwrap();
        assert_eq!((moved.w, moved.h), (resized.w, resized.h));
    }
}
