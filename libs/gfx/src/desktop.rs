// SPDX-License-Identifier: GPL-2.0-only
//! A small window manager.
//!
//! The [`Desktop`] owns a stack of windows, a task bar and the pointer. It
//! is driven entirely by method calls — [`Desktop::mouse`],
//! [`Desktop::key`], [`Desktop::tick`] — and draws on request into a
//! [`Surface`] with [`Desktop::render`], which repaints only the area that
//! changed and reports it, so the caller can copy just that part to the
//! screen.
//!
//! Window contents come from the program as [`App`] objects. An app draws
//! through a [`Canvas`] confined to its window and reacts to keys and
//! clicks; it can ask for windows to be opened or closed, or for the
//! desktop to end, through the [`Context`] it is handed.
//!
//! Behaviour: clicking a window raises it and gives it the keyboard;
//! dragging its title bar moves it; the button at the right of the title
//! bar closes it; clicking a task bar button runs that button's action.
//! Windows can be moved partly off-screen but their title bars always
//! stay reachable.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use crate::Colour;
use crate::geometry::{Point, Rect};
use crate::surface::{Canvas, Surface, TEXT_HEIGHT, text_width};

/// Height of the task bar at the bottom of the screen.
pub const TASKBAR_HEIGHT: i32 = 30;
/// Height of a window's title bar.
pub const TITLE_HEIGHT: i32 = 24;
/// Width of a window's border.
pub const BORDER: i32 = 1;
/// How much of a window must stay on screen horizontally.
const KEEP_VISIBLE: i32 = 48;
/// Side of the square in a window's bottom-right corner that resizes it.
pub const GRIP: i32 = 12;
/// Smallest content area a window can be resized to.
const MIN_CONTENT: (i32, i32) = (120, 60);

/// The colours the desktop draws with.
pub mod theme {
    use crate::Colour;

    /// Desktop background.
    pub const BACKGROUND: Colour = 0x2B_5B_84;
    /// Task bar.
    pub const TASKBAR: Colour = 0x1E_1E_24;
    /// Task bar buttons.
    pub const BUTTON: Colour = 0x3A_3A_46;
    /// Text on the task bar and on title bars.
    pub const LIGHT_TEXT: Colour = 0xFF_FF_FF;
    /// Title bar of the window that has the keyboard.
    pub const TITLE_ACTIVE: Colour = 0x2F_6F_DB;
    /// Title bars of the other windows.
    pub const TITLE_INACTIVE: Colour = 0x6B_72_80;
    /// The close button.
    pub const CLOSE: Colour = 0xD9_53_4F;
    /// Window border.
    pub const BORDER: Colour = 0x10_10_14;
    /// Window background.
    pub const WINDOW: Colour = 0xF2_F2_F2;
    /// Text in windows.
    pub const TEXT: Colour = 0x20_20_20;
    /// The strokes of the resize grip in a window's corner.
    pub const GRIP: Colour = 0x9A_A0_A8;
    /// Pointer outline.
    pub const POINTER_EDGE: Colour = 0x00_00_00;
    /// Pointer fill.
    pub const POINTER_FILL: Colour = 0xFF_FF_FF;
}

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

/// Something an [`App`] or task bar button wants the desktop to do.
enum Request {
    Open {
        app: Box<dyn App>,
        width: i32,
        height: i32,
    },
    Close,
    Quit,
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
}

/// The contents of a window.
pub trait App {
    /// The text in the title bar.
    fn title(&self) -> &str;

    /// Draws the contents. (0, 0) is the top-left corner of the content
    /// area, which is `width` x `height` pixels and already filled with
    /// [`theme::WINDOW`].
    fn draw(&mut self, canvas: &mut Canvas<'_>, width: i32, height: i32);

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
}

/// Identifies a window for as long as it is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowId(u32);

struct Window {
    id: WindowId,
    /// The whole window, border included.
    frame: Rect,
    app: Box<dyn App>,
    /// While the window is maximised: the frame to go back to.
    restore: Option<Rect>,
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

    fn close_button(&self) -> Rect {
        let bar = self.title_bar();
        let size = TITLE_HEIGHT - 8;
        Rect::new(bar.right() - size - 4, bar.y + 4, size, size)
    }

    /// The button that maximises the window or puts it back; left of
    /// the close button. Only resizable windows have one.
    fn maximise_button(&self) -> Option<Rect> {
        let close = self.close_button();
        self.app
            .resizable()
            .then(|| Rect::new(close.x - close.w - 4, close.y, close.w, close.h))
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
}

struct Launcher {
    label: String,
    action: Box<dyn Fn(&mut Context)>,
}

/// The window manager. See the module documentation.
pub struct Desktop {
    width: i32,
    height: i32,
    /// Back to front: the last window is on top and has the keyboard.
    windows: Vec<Window>,
    launchers: Vec<Launcher>,
    status: String,
    pointer: Point,
    left_button: bool,
    /// The window being moved, and where in its frame it was grabbed.
    drag: Option<(WindowId, Point)>,
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
    /// pointer in the middle.
    pub fn new(width: usize, height: usize) -> Self {
        let (width, height) = (width as i32, height as i32);
        Self {
            width,
            height,
            windows: Vec::new(),
            launchers: Vec::new(),
            status: String::new(),
            pointer: Point::new(width / 2, height / 2),
            left_button: false,
            drag: None,
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

    fn launcher_button(&self, index: usize) -> Rect {
        let bar = self.taskbar();
        let mut x = 6;
        for launcher in &self.launchers[..index] {
            x += text_width(&launcher.label) + 20 + 6;
        }
        let width = text_width(&self.launchers[index].label) + 20;
        Rect::new(x, bar.y + 4, width, TASKBAR_HEIGHT - 8)
    }

    /// The area of the `index`-th task bar button.
    pub fn launcher_frame(&self, index: usize) -> Option<Rect> {
        (index < self.launchers.len()).then(|| self.launcher_button(index))
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

    /// Adds a button to the task bar; clicking it calls `action`.
    pub fn add_launcher(&mut self, label: &str, action: impl Fn(&mut Context) + 'static) {
        self.launchers.push(Launcher {
            label: label.into(),
            action: Box::new(action),
        });
        self.damage(self.taskbar());
    }

    /// Sets the text shown at the right end of the task bar.
    pub fn set_status(&mut self, status: &str) {
        if self.status != status {
            self.status = status.into();
            self.damage(self.taskbar());
        }
    }

    /// Opens a window showing `app` with a `width` x `height` content
    /// area, on top of the others. Each new window is placed a little
    /// further down and right than the previous one.
    pub fn open(&mut self, app: Box<dyn App>, width: i32, height: i32) -> WindowId {
        let id = WindowId(self.next_id);
        self.next_id += 1;
        let frame_width = width.max(80) + 2 * BORDER;
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
        if let Some(top) = self.windows.last() {
            // Its title bar changes colour.
            self.damage(top.frame);
        }
        self.windows.push(Window {
            id,
            frame,
            app,
            restore: None,
        });
        self.damage(frame);
        id
    }

    /// Number of open windows.
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
            let window = self.windows.remove(index);
            self.damage(window.frame);
            if let Some(top) = self.windows.last() {
                self.damage(top.frame);
            }
            if self.drag.is_some_and(|(dragged, _)| dragged == id) {
                self.drag = None;
            }
            if self.resize.is_some_and(|(resized, _)| resized == id) {
                self.resize = None;
            }
        }
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
            }
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
                self.windows[index].frame = resized;
                self.damage(frame);
                self.damage(resized);
            }
        }
        if let Some((id, grab)) = self.drag
            && let Some(index) = self.index_of(id)
        {
            let frame = self.windows[index].frame;
            let x =
                (self.pointer.x - grab.x).clamp(KEEP_VISIBLE - frame.w, self.width - KEEP_VISIBLE);
            let y = (self.pointer.y - grab.y).clamp(0, self.height - TASKBAR_HEIGHT - TITLE_HEIGHT);
            let moved = Rect::new(x, y, frame.w, frame.h);
            if moved != frame {
                self.windows[index].frame = moved;
                self.damage(frame);
                self.damage(moved);
            }
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
    }

    fn press(&mut self) {
        let at = self.pointer;
        if self.taskbar().contains(at) {
            let hit = (0..self.launchers.len()).find(|&i| self.launcher_button(i).contains(at));
            if let Some(index) = hit {
                let mut context = Context::default();
                (self.launchers[index].action)(&mut context);
                self.perform(context, None);
            }
            return;
        }
        // The topmost window under the pointer.
        let Some(index) = self.windows.iter().rposition(|w| w.frame.contains(at)) else {
            return;
        };
        // Raise it.
        if index != self.windows.len() - 1 {
            if let Some(top) = self.windows.last() {
                self.damage(top.frame);
            }
            let window = self.windows.remove(index);
            self.windows.push(window);
        }
        let top = self.windows.len() - 1;
        let (id, frame) = (self.windows[top].id, self.windows[top].frame);
        self.damage(frame);

        if self.windows[top].close_button().contains(at) {
            self.close(id);
        } else if self.windows[top]
            .maximise_button()
            .is_some_and(|button| button.contains(at))
        {
            self.toggle_maximised(top);
        } else if self.windows[top]
            .grip()
            .is_some_and(|grip| grip.contains(at))
        {
            // Remember where in the corner the pointer is, so that the
            // corner does not jump to it.
            self.resize = Some((id, Point::new(at.x - frame.right(), at.y - frame.bottom())));
        } else if self.windows[top].restore.is_some() {
            // A maximised window is not dragged about; its contents
            // still get the click.
            let content = self.windows[top].content();
            if content.contains(at) {
                let mut context = Context::default();
                self.capture = Some(id);
                self.windows[top]
                    .app
                    .click(at.x - content.x, at.y - content.y, &mut context);
                self.perform(context, Some(id));
            }
        } else if self.windows[top].title_bar().contains(at) {
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

    /// Maximises the window at `index` to the whole screen above the
    /// task bar, or puts it back where and how large it was.
    fn toggle_maximised(&mut self, index: usize) {
        let window = &mut self.windows[index];
        let old = window.frame;
        window.frame = match window.restore.take() {
            Some(previous) => previous,
            None => {
                window.restore = Some(old);
                Rect::new(0, 0, self.width, self.height - TASKBAR_HEIGHT)
            }
        };
        let new = window.frame;
        self.damage(old);
        self.damage(new);
    }

    /// Reports a key press; it goes to the top window.
    pub fn key(&mut self, code: i32) {
        let Some(window) = self.windows.last_mut() else {
            return;
        };
        let (id, frame) = (window.id, window.frame);
        let mut context = Context::default();
        window.app.key(code, &mut context);
        self.damage(frame);
        self.perform(context, Some(id));
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
            if self.windows[index].app.tick(&mut context) {
                self.damage(self.windows[index].frame);
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
        let mut root = surface.canvas();
        let mut canvas = root.clipped(dirty);
        canvas.fill(Rect::new(0, 0, self.width, self.height), theme::BACKGROUND);

        let top = self.windows.len().saturating_sub(1);
        for (index, window) in self.windows.iter_mut().enumerate() {
            if window.frame.intersect(&dirty).is_empty() {
                continue;
            }
            draw_window(&mut canvas, window, index == top);
        }

        let bar = self.taskbar();
        if !bar.intersect(&dirty).is_empty() {
            canvas.fill(bar, theme::TASKBAR);
            let text_y = bar.y + (TASKBAR_HEIGHT - TEXT_HEIGHT) / 2;
            for index in 0..self.launchers.len() {
                let button = self.launcher_button(index);
                canvas.fill(button, theme::BUTTON);
                canvas.text(
                    button.x + 10,
                    text_y,
                    &self.launchers[index].label,
                    theme::LIGHT_TEXT,
                );
            }
            let x = bar.right() - text_width(&self.status) - 10;
            canvas.text(x, text_y, &self.status, theme::LIGHT_TEXT);
        }

        for (row, line) in POINTER.iter().enumerate() {
            for (column, mark) in line.bytes().enumerate() {
                let colour = match mark {
                    b'#' => theme::POINTER_EDGE,
                    b'.' => theme::POINTER_FILL,
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
}

fn draw_window(canvas: &mut Canvas<'_>, window: &mut Window, active: bool) {
    canvas.outline(window.frame, theme::BORDER);
    let bar = window.title_bar();
    let bar_colour: Colour = if active {
        theme::TITLE_ACTIVE
    } else {
        theme::TITLE_INACTIVE
    };
    canvas.fill(bar, bar_colour);
    let close = window.close_button();
    {
        // The title must not run into the close button.
        let mut title_area = canvas.clipped(Rect::new(bar.x, bar.y, close.x - bar.x - 4, bar.h));
        title_area.text(
            bar.x + 8,
            bar.y + (TITLE_HEIGHT - TEXT_HEIGHT) / 2,
            window.app.title(),
            theme::LIGHT_TEXT,
        );
    }
    if let Some(button) = window.maximise_button() {
        // A square outline: "make this window as large as the screen".
        canvas.fill(button, theme::BUTTON);
        canvas.outline(
            Rect::new(button.x + 4, button.y + 4, button.w - 8, button.h - 8),
            theme::LIGHT_TEXT,
        );
    }
    canvas.fill(close, theme::CLOSE);
    // An "x" in the close button.
    for step in 4..close.w - 4 {
        for thickness in 0..2 {
            canvas.set(
                close.x + step + thickness,
                close.y + step,
                theme::LIGHT_TEXT,
            );
            canvas.set(
                close.right() - 1 - step - thickness,
                close.y + step,
                theme::LIGHT_TEXT,
            );
        }
    }
    let content = window.content();
    canvas.fill(content, theme::WINDOW);
    {
        let mut inner = canvas.sub(content);
        window.app.draw(&mut inner, content.w, content.h);
    }
    if let Some(grip) = window.grip() {
        // Three diagonal strokes in the corner, over the contents.
        for offset in [3, 6, 9] {
            for step in 0..GRIP - offset {
                canvas.set(
                    grip.x + offset + step,
                    grip.bottom() - 1 - step,
                    theme::GRIP,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::rc::Rc;
    use core::cell::RefCell;
    use std::format;
    use std::vec;

    #[derive(Default)]
    struct Log {
        keys: Vec<(String, i32)>,
        clicks: Vec<(String, i32, i32)>,
        pointer: Vec<(String, i32, i32, u32)>,
        draws: usize,
    }

    /// An app that records what happens to it and fills its window with a
    /// colour.
    struct Probe {
        name: String,
        colour: Colour,
        log: Rc<RefCell<Log>>,
    }

    impl App for Probe {
        fn title(&self) -> &str {
            &self.name
        }
        fn draw(&mut self, canvas: &mut Canvas<'_>, width: i32, height: i32) {
            self.log.borrow_mut().draws += 1;
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
                        log: self.log.clone(),
                    }),
                    100,
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
        fn tick(&mut self, _: &mut Context) -> bool {
            self.name == "ticker"
        }
    }

    const W: usize = 640;
    const H: usize = 480;

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

        fn open(&mut self, name: &str, colour: Colour) -> WindowId {
            let app = Probe {
                name: name.into(),
                colour,
                log: self.log.clone(),
            };
            self.desktop.open(Box::new(app), 200, 100)
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

        fn render(&mut self) -> Rect {
            self.desktop.render(&mut self.surface)
        }

        fn pixel(&self, x: i32, y: i32) -> Colour {
            self.surface.pixel(x, y).unwrap()
        }
    }

    const RED: Colour = 0xAA_00_00;
    const GREEN: Colour = 0x00_AA_00;

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
        f.move_to(far.x + 2, far.y + 40);
        f.desktop.mouse(0, 0, 0);
        f.desktop.mouse(3, 3, 0);
        let log = f.log.borrow();
        assert_eq!(log.clicks, [("front".into(), 10, 10)]);
        assert_eq!(
            log.pointer,
            [
                ("front".into(), 15, 17, 1),
                (
                    "front".into(),
                    far.x + 2 - content.x,
                    far.y + 40 - content.y,
                    1
                ),
                (
                    "front".into(),
                    far.x + 2 - content.x,
                    far.y + 40 - content.y,
                    0
                ),
            ]
        );
        // The window order did not change by dragging across.
        assert_eq!(f.desktop.windows.last().unwrap().id, front);
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
    fn first_render_paints_everything() {
        let mut f = Fixture::new();
        f.desktop.add_launcher("Files", |_| {});
        f.desktop.set_status("12:34");
        assert_eq!(f.render(), Rect::new(0, 0, W as i32, H as i32));
        assert_eq!(f.pixel(5, 5), theme::BACKGROUND);
        assert_eq!(f.pixel(1, H as i32 - 2), theme::TASKBAR);
        let button = f.desktop.launcher_button(0);
        assert_eq!(f.pixel(button.x + 2, button.y + 2), theme::BUTTON);
        // Launcher label and status text are drawn (anti-aliased, so in
        // shades between the background and the text colour).
        let light_in = |f: &Fixture, area: Rect| {
            (area.y..area.bottom())
                .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
                .filter(|&(x, y)| (f.pixel(x, y) & 0xFF) > 0x90)
                .count()
        };
        assert!(light_in(&f, button) > 20);
        let status_area = Rect::new(W as i32 - 70, H as i32 - TASKBAR_HEIGHT, 70, TASKBAR_HEIGHT);
        assert!(light_in(&f, status_area) > 20);
        // The pointer starts in the middle: black tip, white inside.
        assert_eq!(f.desktop.pointer(), Point::new(320, 240));
        assert_eq!(f.pixel(320, 240), theme::POINTER_EDGE);
        assert_eq!(f.pixel(322, 245), theme::POINTER_FILL);
        // Nothing changed: nothing to render.
        assert!(!f.desktop.needs_render());
        assert!(f.render().is_empty());
    }

    #[test]
    fn windows_are_drawn_with_frame_title_and_confined_content() {
        let mut f = Fixture::new();
        let id = f.open("probe", RED);
        f.render();
        let frame = f.desktop.window_frame(id).unwrap();
        assert_eq!((frame.w, frame.h), (202, 100 + TITLE_HEIGHT + 2));
        assert_eq!(f.pixel(frame.x, frame.y), theme::BORDER);
        assert_eq!(
            f.pixel(frame.right() - 1, frame.bottom() - 1),
            theme::BORDER
        );
        assert_eq!(f.pixel(frame.x + 3, frame.y + 3), theme::TITLE_ACTIVE);
        assert_eq!(f.pixel(frame.right() - 8, frame.y + 6), theme::CLOSE);
        // The app tried to paint far outside its content area.
        assert_eq!(f.pixel(frame.x + 1, frame.y + 1 + TITLE_HEIGHT), RED);
        assert_eq!(f.pixel(frame.right() - 2, frame.bottom() - 2), RED);
        assert_eq!(f.pixel(frame.x - 1, frame.y + 50), theme::BACKGROUND);
        assert_eq!(f.pixel(frame.right(), frame.y + 50), theme::BACKGROUND);
        assert_eq!(f.pixel(frame.x + 50, frame.bottom()), theme::BACKGROUND);
        // The content area is all the app's, except the strokes of the
        // resize grip in its corner (9 + 6 + 3 pixels).
        let reds = f.surface.pixels().iter().filter(|&&p| p == RED).count();
        assert_eq!(reds, 200 * 100 - 18);
        let grip = f.desktop.windows[0].grip().unwrap();
        assert_eq!(f.pixel(grip.x + 3, grip.bottom() - 1), theme::GRIP);
    }

    #[test]
    fn dragging_the_corner_resizes_and_the_button_maximises() {
        let mut f = Fixture::new();
        let id = f.open("sizeable", RED);
        f.render();
        let before = f.desktop.window_frame(id).unwrap();
        let grip = f.desktop.windows[0].grip().unwrap();

        // Drag the corner 40 right and 25 down: the frame grows by that,
        // its top-left stays, and the app is drawn at the new size.
        f.move_to(grip.x + 5, grip.y + 5);
        f.desktop.mouse(0, 0, 1);
        f.desktop.mouse(40, 25, 1);
        f.desktop.mouse(0, 0, 0);
        let grown = f.desktop.window_frame(id).unwrap();
        assert_eq!(
            grown,
            Rect::new(before.x, before.y, before.w + 40, before.h + 25)
        );
        f.render();
        assert_eq!(f.pixel(grown.right() - 20, grown.bottom() - 20), RED);
        assert_eq!(
            f.pixel(grown.right() + 2, grown.bottom() - 20),
            theme::BACKGROUND
        );
        // No pointer events went to the app, and releasing ends it.
        assert!(f.log.borrow().clicks.is_empty() && f.log.borrow().pointer.is_empty());
        f.desktop.mouse(30, 30, 0);
        assert_eq!(f.desktop.window_frame(id), Some(grown));

        // It cannot be made smaller than the minimum, nor larger than
        // the space above the task bar.
        let grip = f.desktop.windows[0].grip().unwrap();
        f.move_to(grip.x + 5, grip.y + 5);
        f.desktop.mouse(0, 0, 1);
        f.desktop.mouse(-2000, -2000, 1);
        let small = f.desktop.window_frame(id).unwrap();
        assert_eq!(
            (small.w, small.h),
            (
                MIN_CONTENT.0 + 2 * BORDER,
                MIN_CONTENT.1 + 2 * BORDER + TITLE_HEIGHT
            )
        );
        f.desktop.mouse(5000, 5000, 1);
        f.desktop.mouse(0, 0, 0);
        let large = f.desktop.window_frame(id).unwrap();
        assert_eq!(large.bottom(), H as i32 - TASKBAR_HEIGHT);
        assert!(large.w <= W as i32);

        // Maximise: the whole screen above the task bar, no grip, and
        // the title bar no longer drags. Again: back as it was.
        let button = f.desktop.windows[0].maximise_button().unwrap();
        f.click_at(button.x + 2, button.y + 2);
        let full = Rect::new(0, 0, W as i32, H as i32 - TASKBAR_HEIGHT);
        assert_eq!(f.desktop.window_frame(id), Some(full));
        assert!(f.desktop.windows[0].grip().is_none());
        f.render();
        assert_eq!(f.pixel(W as i32 - 3, H as i32 - TASKBAR_HEIGHT - 3), RED);
        assert_eq!(f.pixel(3, H as i32 - 3), theme::TASKBAR);
        f.move_to(100, 10);
        f.desktop.mouse(0, 0, 1);
        f.desktop.mouse(50, 50, 1);
        f.desktop.mouse(0, 0, 0);
        assert_eq!(f.desktop.window_frame(id), Some(full));
        // A click in the contents still arrives.
        f.click_at(300, 200);
        assert_eq!(f.log.borrow().clicks.len(), 1);
        let button = f.desktop.windows[0].maximise_button().unwrap();
        f.click_at(button.x + 2, button.y + 2);
        assert_eq!(f.desktop.window_frame(id), Some(large));
        // The incremental repaint still equals a full one.
        f.render();
        let mut fresh = Surface::new(W, H, 0);
        f.desktop.damage(Rect::new(0, 0, W as i32, H as i32));
        f.desktop.render(&mut fresh);
        assert!(f.surface.pixels() == fresh.pixels());
    }

    #[test]
    fn fixed_size_windows_have_no_grip_or_maximise_button() {
        struct Fixed;
        impl App for Fixed {
            fn title(&self) -> &str {
                "fixed"
            }
            fn draw(&mut self, _: &mut Canvas<'_>, _: i32, _: i32) {}
            fn resizable(&self) -> bool {
                false
            }
        }
        let mut f = Fixture::new();
        let id = f.desktop.open(Box::new(Fixed), 200, 100);
        let frame = f.desktop.window_frame(id).unwrap();
        assert!(f.desktop.windows[0].grip().is_none());
        assert!(f.desktop.windows[0].maximise_button().is_none());
        // Where the grip would be: a press there is an ordinary click in
        // the contents, and nothing changes size.
        f.move_to(frame.right() - 5, frame.bottom() - 5);
        f.desktop.mouse(0, 0, 1);
        f.desktop.mouse(30, 30, 1);
        f.desktop.mouse(0, 0, 0);
        assert_eq!(f.desktop.window_frame(id), Some(frame));
        // Where the maximise button would be is title bar: it drags.
        let close = f.desktop.windows[0].close_button();
        f.move_to(close.x - 10, close.y + 2);
        f.desktop.mouse(0, 0, 1);
        f.desktop.mouse(10, 10, 1);
        f.desktop.mouse(0, 0, 0);
        let moved = f.desktop.window_frame(id).unwrap();
        assert_eq!((moved.w, moved.h), (frame.w, frame.h));
        assert_eq!((moved.x, moved.y), (frame.x + 10, frame.y + 10));
    }

    #[test]
    fn clicking_raises_and_focuses() {
        let mut f = Fixture::new();
        let a = f.open("a", RED);
        let b = f.open("b", GREEN);
        f.render();
        let (fa, fb) = (
            f.desktop.window_frame(a).unwrap(),
            f.desktop.window_frame(b).unwrap(),
        );
        // They overlap; `b` is on top.
        let shared = fa.intersect(&fb).inset(30);
        assert!(!shared.is_empty());
        assert_eq!(f.pixel(shared.x, shared.y + TITLE_HEIGHT), GREEN);
        assert_eq!(f.pixel(fa.x + 3, fa.y + 3), theme::TITLE_INACTIVE);
        assert_eq!(f.pixel(fb.x + 3, fb.y + 3), theme::TITLE_ACTIVE);

        f.desktop.key(0x61);
        assert_eq!(f.log.borrow().keys, [("b".into(), 0x61)]);

        // Click a part of `a` that `b` does not cover.
        f.click_at(fa.x + 5, fa.bottom() - 5);
        assert_eq!(f.desktop.titles(), ["b", "a"]);
        f.render();
        assert_eq!(f.pixel(shared.x, shared.y + TITLE_HEIGHT), RED);
        assert_eq!(f.pixel(fa.x + 3, fa.y + 3), theme::TITLE_ACTIVE);
        // The part of `b`'s title bar that sticks out from under `a`.
        assert_eq!(f.pixel(fa.right() + 2, fb.y + 3), theme::TITLE_INACTIVE);
        f.desktop.key(0x62);
        assert_eq!(f.log.borrow().keys.last(), Some(&("a".into(), 0x62)));
        // The click was delivered in content coordinates.
        assert_eq!(
            f.log.borrow().clicks,
            [("a".into(), 4, fa.h - 6 - TITLE_HEIGHT)]
        );
        // A click where both overlap goes to the one on top only.
        f.click_at(shared.x, shared.y + TITLE_HEIGHT);
        assert_eq!(f.log.borrow().clicks.len(), 2);
        assert_eq!(f.log.borrow().clicks[1].0, "a");
    }

    #[test]
    fn dragging_the_title_bar_moves_the_window() {
        let mut f = Fixture::new();
        let id = f.open("w", RED);
        f.render();
        let before = f.desktop.window_frame(id).unwrap();
        f.move_to(before.x + 50, before.y + 10);
        f.desktop.mouse(0, 0, 1);
        f.desktop.mouse(120, 80, 1);
        let after = f.desktop.window_frame(id).unwrap();
        assert_eq!(after, before.translate(120, 80));
        // Repaint covers the old and the new position, and no more than
        // needed.
        let dirty = f.render();
        let both = before.union(&after);
        assert!(dirty.intersect(&both) == both);
        assert_eq!(f.pixel(before.x + 5, before.y + 5), theme::BACKGROUND);
        assert_eq!(f.pixel(after.x + 5, after.y + 5), theme::TITLE_ACTIVE);
        // Releasing ends the drag.
        f.desktop.mouse(0, 0, 0);
        f.desktop.mouse(-30, -30, 0);
        assert_eq!(f.desktop.window_frame(id).unwrap(), after);
        // Dragging the content does not move the window.
        f.move_to(after.x + 50, after.y + 60);
        f.desktop.mouse(0, 0, 1);
        f.desktop.mouse(40, 40, 1);
        f.desktop.mouse(0, 0, 0);
        assert_eq!(f.desktop.window_frame(id).unwrap(), after);
    }

    #[test]
    fn windows_cannot_be_dragged_out_of_reach() {
        let mut f = Fixture::new();
        let id = f.open("w", RED);
        let frame = f.desktop.window_frame(id).unwrap();
        let grab = |f: &mut Fixture| {
            let frame = f.desktop.window_frame(id).unwrap();
            f.move_to((frame.x + 20).clamp(0, W as i32 - 1), frame.y + 10);
            f.desktop.mouse(0, 0, 1);
        };
        for (dx, dy) in [(-5000, -5000), (5000, 5000), (5000, -5000), (-5000, 5000)] {
            grab(&mut f);
            f.desktop.mouse(dx, dy, 1);
            f.desktop.mouse(0, 0, 0);
            let moved = f.desktop.window_frame(id).unwrap();
            assert_eq!((moved.w, moved.h), (frame.w, frame.h));
            // The title bar is on screen, above the task bar.
            assert!(moved.y >= 0 && moved.y + TITLE_HEIGHT <= H as i32 - TASKBAR_HEIGHT);
            assert!(moved.right() >= KEEP_VISIBLE && moved.x <= W as i32 - KEEP_VISIBLE);
        }
        // The pointer itself stays on screen.
        f.desktop.mouse(i32::MAX, i32::MAX, 0);
        assert_eq!(f.desktop.pointer(), Point::new(W as i32 - 1, H as i32 - 1));
        f.desktop.mouse(i32::MIN, i32::MIN, 0);
        assert_eq!(f.desktop.pointer(), Point::new(0, 0));
        f.render();
    }

    #[test]
    fn close_button_and_requests() {
        let mut f = Fixture::new();
        let a = f.open("a", RED);
        let b = f.open("b", GREEN);
        let fb = f.desktop.window_frame(b).unwrap();
        f.click_at(fb.right() - 10, fb.y + 10);
        assert_eq!(f.desktop.titles(), ["a"]);
        assert!(f.desktop.window_frame(b).is_none());
        f.render();
        assert_eq!(f.pixel(fb.right() - 5, fb.bottom() - 5), theme::BACKGROUND);

        // An app opens another window, which then closes itself.
        f.desktop.key(0x6E);
        assert_eq!(f.desktop.titles(), ["a", "child"]);
        f.desktop.key(0x71);
        assert_eq!(f.desktop.titles(), ["a"]);
        assert_eq!(f.log.borrow().keys.last(), Some(&("child".into(), 0x71)));
        // Keys with no window open go nowhere.
        f.desktop.key(0x71);
        assert_eq!(f.desktop.window_count(), 0);
        let _ = a;
        f.desktop.key(0x61);
        assert!(!f.desktop.quit_requested());
        f.open("q", RED);
        f.desktop.key(0x51);
        assert!(f.desktop.quit_requested());
    }

    #[test]
    fn launchers_run_their_actions() {
        let mut f = Fixture::new();
        let log = f.log.clone();
        f.desktop.add_launcher("New", move |context| {
            context.open(
                Box::new(Probe {
                    name: "launched".into(),
                    colour: RED,
                    log: log.clone(),
                }),
                120,
                80,
            );
        });
        f.desktop.add_launcher("Exit", Context::quit);
        let first = f.desktop.launcher_button(0);
        let second = f.desktop.launcher_button(1);
        assert!(second.x >= first.right());
        f.click_at(first.x + 3, first.y + 3);
        f.click_at(first.x + 3, first.y + 3);
        assert_eq!(f.desktop.titles(), ["launched", "launched"]);
        // The task bar between buttons does nothing.
        f.click_at(second.right() + 20, second.y + 3);
        assert!(!f.desktop.quit_requested());
        f.click_at(second.x + 3, second.y + 3);
        assert!(f.desktop.quit_requested());
        // Holding the button while moving over a launcher is not a click.
        let mut g = Fixture::new();
        g.desktop.add_launcher("Exit", Context::quit);
        g.move_to(300, 100);
        g.desktop.mouse(0, 0, 1);
        let button = g.desktop.launcher_button(0);
        g.move_to(button.x + 3, button.y + 3);
        g.desktop.mouse(0, 0, 0);
        assert!(!g.desktop.quit_requested());
    }

    #[test]
    fn only_changed_areas_are_repainted() {
        let mut f = Fixture::new();
        f.open("still", RED);
        let ticker = f.open("ticker", GREEN);
        f.render();
        let draws = f.log.borrow().draws;

        // Moving the pointer over empty desktop repaints just around it.
        f.move_to(600, 20);
        f.render();
        f.desktop.mouse(3, 2, 0);
        let dirty = f.render();
        assert!(
            dirty.w <= POINTER_WIDTH + 3 && dirty.h <= POINTER_HEIGHT + 2,
            "{dirty:?}"
        );
        assert_eq!(f.log.borrow().draws, draws, "no window was redrawn");
        // The old pointer image is gone, the new one is there.
        assert_eq!(f.pixel(600, 20), theme::BACKGROUND);
        assert_eq!(f.pixel(603, 22), theme::POINTER_EDGE);

        // A tick repaints the app that asked for it.
        f.desktop.tick();
        let dirty = f.render();
        assert_eq!(dirty, f.desktop.window_frame(ticker).unwrap());

        // Status changes repaint the task bar only; an unchanged status
        // repaints nothing.
        f.desktop.set_status("up 0:01");
        assert_eq!(f.render(), f.desktop.taskbar());
        f.desktop.set_status("up 0:01");
        assert!(f.render().is_empty());
    }

    #[test]
    fn incremental_rendering_matches_a_full_repaint() {
        // After any sequence of events, the incrementally updated image
        // must equal one painted from scratch.
        let mut f = Fixture::new();
        f.desktop.add_launcher("Exit", Context::quit);
        let a = f.open("a", RED);
        f.open("ticker", GREEN);
        f.render();
        let fa = f.desktop.window_frame(a).unwrap();
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        let mut next = move |limit: i32| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % limit as u64) as i32
        };
        f.click_at(fa.x + 4, fa.bottom() - 4);
        for step in 0..400 {
            match next(6) {
                0 => f.desktop.mouse(next(200) - 100, next(200) - 100, 0),
                1 => f.desktop.mouse(next(60) - 30, next(60) - 30, 1),
                2 => f.desktop.mouse(0, 0, 0),
                3 => f.desktop.key(0x61),
                4 => f.desktop.tick(),
                _ => f.desktop.set_status(&format!("step {step}")),
            }
            f.render();
            if step % 50 == 0 {
                let mut fresh = Surface::new(W, H, 0);
                f.desktop.damage(Rect::new(0, 0, W as i32, H as i32));
                f.desktop.render(&mut fresh);
                assert!(fresh == f.surface, "mismatch after step {step}");
            }
        }
        assert_eq!(f.desktop.window_count(), 2);
        let _ = vec![0u8; 0];
    }
}
