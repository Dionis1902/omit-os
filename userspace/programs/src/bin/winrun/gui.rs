// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
// Sources: the Microsoft Learn reference page of each USER32 and GDI32 function
//   implemented (named in its comment) and "About Messages and Message
//   Queues". Behaviour tests: wingui.exe (QEMU test desktop).
//! Windows for Windows programs: a small part of `USER32.dll` and
//! `GDI32.dll`.
//!
//! Enough for the classic shape of a graphical Windows program: register
//! a window class, create a window, run a message loop, paint in
//! `WM_PAINT`, react to keys and the mouse. Each window is a window on
//! the Omit OS desktop (`omit_rt::window`); drawing goes into an image
//! kept here and is copied to the desktop when painting ends.
//!
//! Written from the public documentation of the functions (Microsoft
//! Learn). As in `kernel32`, a function is implemented for the cases its
//! comment describes or it is absent.
//!
//! Known differences from Windows — this is a first slice, not a window
//! system:
//!
//! * One kind of window: a top-level window with a title bar, drawn by
//!   the desktop. No child windows, so no controls (buttons, edit
//!   boxes), no menus, no dialogs except `MessageBoxA`.
//! * A window is visible from the moment it is created and has the size
//!   it was created with: the requested size is the size of its client
//!   area, and it cannot be moved, resized or hidden by the program.
//!   Creating a window fails if no desktop is running.
//! * One built-in fixed-width font; text is UTF-8.
//! * Messages delivered: `WM_CREATE`, `WM_SIZE` (once), `WM_PAINT`,
//!   `WM_KEYDOWN`, `WM_CHAR`, `WM_KEYUP` (sent right after the key
//!   press: the system has no key releases), `WM_LBUTTONDOWN`,
//!   `WM_MOUSEMOVE` (only while the left button is held), `WM_LBUTTONUP`,
//!   `WM_TIMER`, `WM_CLOSE`, `WM_DESTROY`, `WM_QUIT`, and whatever the
//!   program posts.
//! * Timers are checked when the program asks for a message and have
//!   the resolution of the system timer, 10 ms.
//! * Virtual-key codes are provided for letters, digits and the
//!   navigation and function keys; other keys report code 0 and are
//!   best read through `WM_CHAR`.
//! * Cursors and icons are accepted and ignored.
//! * The message queue belongs to the process, not to a thread: use the
//!   windows from one thread.

#![allow(
    non_snake_case,
    reason = "the functions carry the names Windows programs import"
)]

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{CStr, c_char};

use omit_gfx::geometry::Rect as Area;
use omit_gfx::surface::{Canvas, Surface, TEXT_HEIGHT, text_width};
use omit_pe::Symbol;
use omit_rt::abi::key;
use omit_rt::sync::Mutex;
use omit_rt::window::{Event, Window};

use crate::kernel32::set_last_error;

type Bool = i32;
const TRUE: Bool = 1;
const FALSE: Bool = 0;
type WndProc = extern "win64" fn(usize, u32, usize, isize) -> isize;

const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_NOT_SUPPORTED: u32 = 50;
const ERROR_INVALID_WINDOW_HANDLE: u32 = 1400;
const ERROR_CANNOT_FIND_WND_CLASS: u32 = 1407;
const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;

const WM_CREATE: u32 = 0x0001;
const WM_DESTROY: u32 = 0x0002;
const WM_SIZE: u32 = 0x0005;
const WM_PAINT: u32 = 0x000F;
const WM_CLOSE: u32 = 0x0010;
const WM_QUIT: u32 = 0x0012;
const WM_TIMER: u32 = 0x0113;
const WM_KEYDOWN: u32 = 0x0100;
const WM_KEYUP: u32 = 0x0101;
const WM_CHAR: u32 = 0x0102;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const MK_LBUTTON: usize = 1;

const CW_USEDEFAULT: i32 = i32::MIN;
const WS_CHILD: u32 = 0x4000_0000;
const DEFAULT_SIZE: (i32, i32) = (480, 320);
/// Largest client area accepted.
const MAX_SIZE: i32 = 2048;

// Handles are small numbers with the kind in the upper bits, so that a
// handle of one kind passed as another is recognised.
const KIND_MASK: usize = 0xFFFF_0000;
const HWND_BASE: usize = 0x0001_0000;
const HDC_BASE: usize = 0x0002_0000;
const OBJECT_BASE: usize = 0x0003_0000;
const STOCK_BASE: usize = 0x0004_0000;

const WHITE: u32 = 0xFF_FF_FF;
const BLACK: u32 = 0x00_00_00;
/// The colour of dialog and button surfaces.
const FACE: u32 = 0xF0_F0_F0;

/// `POINT`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Point {
    x: i32,
    y: i32,
}

/// `RECT`: right and bottom are exclusive.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Rect {
    fn area(&self) -> Area {
        Area::new(
            self.left,
            self.top,
            self.right.saturating_sub(self.left),
            self.bottom.saturating_sub(self.top),
        )
    }
}

/// `SIZE`.
#[repr(C)]
pub struct Size {
    cx: i32,
    cy: i32,
}

/// `MSG`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Msg {
    hwnd: usize,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt: Point,
    /// Undocumented in Windows (`lPrivate`); here it carries the
    /// character of a key press from `GetMessageA` to
    /// `TranslateMessage`.
    private: u32,
}

/// `WNDCLASSA`.
#[repr(C)]
pub struct WndClassA {
    style: u32,
    wndproc: Option<WndProc>,
    class_extra: i32,
    window_extra: i32,
    instance: usize,
    icon: usize,
    cursor: usize,
    background: usize,
    menu_name: *const c_char,
    class_name: *const c_char,
}

/// `WNDCLASSEXA`.
#[repr(C)]
pub struct WndClassExA {
    size: u32,
    style: u32,
    wndproc: Option<WndProc>,
    class_extra: i32,
    window_extra: i32,
    instance: usize,
    icon: usize,
    cursor: usize,
    background: usize,
    menu_name: *const c_char,
    class_name: *const c_char,
    small_icon: usize,
}

/// `CREATESTRUCTA`, passed with `WM_CREATE`.
#[repr(C)]
struct CreateStructA {
    create_params: usize,
    instance: usize,
    menu: usize,
    parent: usize,
    cy: i32,
    cx: i32,
    y: i32,
    x: i32,
    style: i32,
    name: *const c_char,
    class: *const c_char,
    ex_style: u32,
}

/// `PAINTSTRUCT`.
#[repr(C)]
pub struct PaintStruct {
    hdc: usize,
    erase: Bool,
    paint: Rect,
    restore: Bool,
    inc_update: Bool,
    reserved: [u8; 32],
}

struct Class {
    name: String,
    procedure: WndProc,
    /// Colour the client area is erased with before painting.
    background: Option<u32>,
}

struct Win {
    hwnd: usize,
    procedure: WndProc,
    background: Option<u32>,
    window: Window,
    /// What the program has drawn; copied to the desktop's window when
    /// painting ends.
    image: Surface,
    /// Needs `WM_PAINT`.
    invalid: bool,
    /// The desktop closed the window; `WM_CLOSE` has been queued.
    gone: bool,
    /// Whether the left button was down at the last pointer event.
    left_down: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Object {
    Brush(Option<u32>),
    Pen(Option<u32>),
    Font,
}

struct Dc {
    hwnd: usize,
    text_colour: u32,
    background_colour: u32,
    opaque: bool,
    brush: usize,
    pen: usize,
    font: usize,
    position: (i32, i32),
}

/// A timer set with `SetTimer`.
struct Timer {
    /// 0 for a timer that belongs to no window.
    hwnd: usize,
    id: usize,
    period_ms: u64,
    /// When it fires next, in milliseconds of uptime.
    due: u64,
    /// The program's `TIMERPROC`, or 0 to deliver `WM_TIMER` to the
    /// window procedure.
    callback: usize,
}

struct Gui {
    timers: Vec<Timer>,
    classes: Vec<Class>,
    windows: Vec<Win>,
    queue: VecDeque<Msg>,
    objects: Vec<Option<Object>>,
    contexts: Vec<Option<Dc>>,
    next_hwnd: usize,
}

static GUI: Mutex<Gui> = Mutex::new(Gui {
    timers: Vec::new(),
    classes: Vec::new(),
    windows: Vec::new(),
    queue: VecDeque::new(),
    objects: Vec::new(),
    contexts: Vec::new(),
    next_hwnd: 1,
});

/// Runs `f` on the state. The lock is never held while code of the
/// program (a window procedure) runs.
fn with_gui<R>(f: impl FnOnce(&mut Gui) -> R) -> R {
    f(&mut GUI.lock())
}

/// `COLORREF` (`0x00BBGGRR`) to the system's `0x00RRGGBB`.
fn colour(colorref: u32) -> u32 {
    ((colorref & 0xFF) << 16) | (colorref & 0xFF00) | ((colorref >> 16) & 0xFF)
}

fn colorref(colour: u32) -> u32 {
    // The conversion is its own inverse.
    self::colour(colour)
}

fn stock(index: usize) -> Option<Object> {
    Some(match index {
        0 => Object::Brush(Some(WHITE)),
        1 => Object::Brush(Some(0xC0_C0_C0)),
        2 => Object::Brush(Some(0x80_80_80)),
        3 => Object::Brush(Some(0x40_40_40)),
        4 => Object::Brush(Some(BLACK)),
        5 => Object::Brush(None),
        6 => Object::Pen(Some(WHITE)),
        7 => Object::Pen(Some(BLACK)),
        8 => Object::Pen(None),
        // The fonts: there is one.
        10..=14 | 16 | 17 => Object::Font,
        _ => return None,
    })
}

impl Gui {
    fn window(&mut self, hwnd: usize) -> Option<&mut Win> {
        self.windows.iter_mut().find(|window| window.hwnd == hwnd)
    }

    fn object(&self, handle: usize) -> Option<Object> {
        match handle & KIND_MASK {
            STOCK_BASE => stock(handle & 0xFFFF),
            OBJECT_BASE => self.objects.get(handle & 0xFFFF).copied().flatten(),
            _ => None,
        }
    }

    fn add_object(&mut self, object: Object) -> usize {
        let index = match self.objects.iter().position(Option::is_none) {
            Some(index) => index,
            None if self.objects.len() < 0xFFFF => {
                self.objects.push(None);
                self.objects.len() - 1
            }
            None => return 0,
        };
        self.objects[index] = Some(object);
        OBJECT_BASE | index
    }

    fn open_context(&mut self, hwnd: usize) -> usize {
        let context = Dc {
            hwnd,
            text_colour: BLACK,
            background_colour: WHITE,
            opaque: true,
            brush: STOCK_BASE,     // WHITE_BRUSH
            pen: STOCK_BASE | 7,   // BLACK_PEN
            font: STOCK_BASE | 13, // SYSTEM_FONT
            position: (0, 0),
        };
        let index = match self.contexts.iter().position(Option::is_none) {
            Some(index) => index,
            None if self.contexts.len() < 0xFFFF => {
                self.contexts.push(None);
                self.contexts.len() - 1
            }
            None => return 0,
        };
        self.contexts[index] = Some(context);
        HDC_BASE | index
    }

    fn close_context(&mut self, hdc: usize) -> Option<Dc> {
        if hdc & KIND_MASK != HDC_BASE {
            return None;
        }
        self.contexts.get_mut(hdc & 0xFFFF)?.take()
    }

    /// Runs `draw` on the image of the window the device context `hdc`
    /// belongs to.
    fn draw<R>(
        &mut self,
        hdc: usize,
        draw: impl FnOnce(&Dc, &Gui, &mut Canvas<'_>) -> R,
    ) -> Option<R> {
        if hdc & KIND_MASK != HDC_BASE {
            return None;
        }
        let context = self.contexts.get_mut(hdc & 0xFFFF)?.take()?;
        let result = match self.windows.iter().position(|w| w.hwnd == context.hwnd) {
            Some(index) => {
                // The image is taken out while it is drawn on, so that
                // `draw` can look at the rest of the state.
                let mut image =
                    core::mem::replace(&mut self.windows[index].image, Surface::new(0, 0, 0));
                let result = draw(&context, self, &mut image.canvas());
                self.windows[index].image = image;
                Some(result)
            }
            None => None,
        };
        self.contexts[hdc & 0xFFFF] = Some(context);
        result
    }

    fn context(&mut self, hdc: usize) -> Option<&mut Dc> {
        if hdc & KIND_MASK != HDC_BASE {
            return None;
        }
        self.contexts.get_mut(hdc & 0xFFFF)?.as_mut()
    }

    /// Copies a window's image to the desktop.
    fn present(&mut self, hwnd: usize) {
        if let Some(window) = self.window(hwnd)
            && !window.gone
        {
            window
                .window
                .pixels()
                .copy_from_slice(window.image.pixels());
            window.window.present();
        }
    }

    /// Fetches what the desktop has sent and turns it into messages.
    fn pump(&mut self) {
        let Self { windows, queue, .. } = self;
        for window in windows.iter_mut().filter(|window| !window.gone) {
            let hwnd = window.hwnd;
            let mut post = |message: u32, wparam: usize, lparam: isize, private: u32| {
                queue.push_back(Msg {
                    hwnd,
                    message,
                    wparam,
                    lparam,
                    time: omit_rt::uptime_ms() as u32,
                    pt: Point::default(),
                    private,
                });
            };
            for event in window.window.pending_events() {
                match event {
                    Event::Key(code) => {
                        let (virtual_key, character) = translate_key(code);
                        // Bit 0: repeat count 1.
                        post(WM_KEYDOWN, virtual_key, 1, character);
                        // Bits 30 and 31: was down, is being released.
                        post(WM_KEYUP, virtual_key, 0xC000_0001u32 as i32 as isize, 0);
                    }
                    Event::Pointer { x, y, buttons } => {
                        let position = ((y as u16 as u32) << 16 | x as u16 as u32) as i32 as isize;
                        let down = buttons & 1 != 0;
                        let message = match (down, window.left_down) {
                            (true, false) => WM_LBUTTONDOWN,
                            (true, true) => WM_MOUSEMOVE,
                            (false, _) => WM_LBUTTONUP,
                        };
                        window.left_down = down;
                        post(message, if down { MK_LBUTTON } else { 0 }, position, 0);
                    }
                    Event::Closed => {
                        window.gone = true;
                        post(WM_CLOSE, 0, 0, 0);
                    }
                }
            }
        }
    }

    /// The next message, if one is ready: a queued one, else `WM_PAINT`
    /// for a window that needs painting.
    fn next_message(&mut self, remove: bool) -> Option<Msg> {
        self.pump();
        if let Some(message) = self.queue.front().copied() {
            if remove {
                self.queue.pop_front();
            }
            return Some(message);
        }
        let now = omit_rt::uptime_ms();
        if let Some(window) = self
            .windows
            .iter()
            .find(|window| window.invalid && !window.gone)
        {
            return Some(Msg {
                hwnd: window.hwnd,
                message: WM_PAINT,
                time: now as u32,
                ..Msg::default()
            });
        }
        // Timers come last, as on Windows: only when nothing else is
        // waiting.
        let timer = self.timers.iter_mut().find(|timer| timer.due <= now)?;
        if remove {
            // The next tick is one period from now: ticks the program
            // was too busy for are dropped, not queued up.
            timer.due = now + timer.period_ms;
        }
        Some(Msg {
            hwnd: timer.hwnd,
            message: WM_TIMER,
            wparam: timer.id,
            lparam: timer.callback as isize,
            time: now as u32,
            ..Msg::default()
        })
    }

    /// Milliseconds until the next timer fires, if there is a timer.
    fn next_timer(&self) -> Option<u64> {
        let now = omit_rt::uptime_ms();
        self.timers
            .iter()
            .map(|timer| timer.due.saturating_sub(now))
            .min()
    }
}

/// The virtual-key code and the character (0 if none) of a key.
fn translate_key(code: i32) -> (usize, u32) {
    const VK_BACK: usize = 0x08;
    match code {
        127 => (VK_BACK, 8),
        // Tab, Enter, Escape, space: the codes coincide.
        9 | 13 | 27 | 32 => (code as usize, code as u32),
        0x30..=0x39 | 0x41..=0x5A => (code as usize, code as u32),
        0x61..=0x7A => (code as usize - 0x20, code as u32),
        0x21..=0x7E => (0, code as u32),
        // Control characters (Ctrl+letter): the letter's key.
        1..=26 => (code as usize + 0x40, code as u32),
        key::PAGE_UP => (0x21, 0),
        key::PAGE_DOWN => (0x22, 0),
        key::END => (0x23, 0),
        key::HOME => (0x24, 0),
        key::LEFT => (0x25, 0),
        key::UP => (0x26, 0),
        key::RIGHT => (0x27, 0),
        key::DOWN => (0x28, 0),
        key::INSERT => (0x2D, 0),
        key::DELETE => (0x2E, 0),
        code if (key::F1..key::F1 + 12).contains(&code) => (0x70 + (code - key::F1) as usize, 0),
        _ => (0, 0),
    }
}

/// Calls a window's procedure.
fn send(hwnd: usize, message: u32, wparam: usize, lparam: isize) -> isize {
    match with_gui(|gui| gui.window(hwnd).map(|window| window.procedure)) {
        Some(procedure) => procedure(hwnd, message, wparam, lparam),
        None => 0,
    }
}

/// The text of a NUL-terminated string argument; `None` for a null
/// pointer or invalid UTF-8.
///
/// # Safety
///
/// `pointer` must be null or point to a NUL-terminated string.
unsafe fn text<'a>(pointer: *const c_char) -> Option<&'a str> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: guaranteed by the caller.
    unsafe { CStr::from_ptr(pointer) }.to_str().ok()
}

/// The class background: a brush handle, or a system colour index plus
/// one (documented for `hbrBackground`).
fn class_background(gui: &Gui, value: usize) -> Option<u32> {
    match value {
        0 => None,
        // COLOR_WINDOW + 1.
        6 => Some(WHITE),
        1..=31 => Some(FACE),
        handle => match gui.object(handle) {
            Some(Object::Brush(colour)) => colour,
            _ => None,
        },
    }
}

fn register(name: *const c_char, procedure: Option<WndProc>, background: usize) -> u16 {
    // A class "name" below 0x10000 is an atom, which is not supported.
    // SAFETY: the pointer comes from the program's class structure,
    // which the documentation requires to hold a NUL-terminated name.
    let (Some(name), Some(procedure)) = (
        (name as usize > 0xFFFF)
            .then(|| unsafe { text(name) })
            .flatten(),
        procedure,
    ) else {
        set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    };
    with_gui(|gui| {
        if gui
            .classes
            .iter()
            .any(|class| class.name.eq_ignore_ascii_case(name))
        {
            set_last_error(ERROR_CLASS_ALREADY_EXISTS);
            return 0;
        }
        let background = class_background(gui, background);
        gui.classes.push(Class {
            name: name.into(),
            procedure,
            background,
        });
        // Atoms for classes start at 0xC000.
        0xC000 + (gui.classes.len() - 1) as u16
    })
}

/// `RegisterClassA`. The class name, window procedure and background
/// are used; the rest is ignored.
unsafe extern "win64" fn RegisterClassA(class: *const WndClassA) -> u16 {
    if class.is_null() {
        set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    // SAFETY: the program passes a pointer to its `WNDCLASSA`.
    let class = unsafe { &*class };
    register(class.class_name, class.wndproc, class.background)
}

/// `RegisterClassExA`. See [`RegisterClassA`].
unsafe extern "win64" fn RegisterClassExA(class: *const WndClassExA) -> u16 {
    if class.is_null() {
        set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    // SAFETY: the program passes a pointer to its `WNDCLASSEXA`.
    let class = unsafe { &*class };
    if class.size as usize != size_of::<WndClassExA>() {
        set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    register(class.class_name, class.wndproc, class.background)
}

/// `CreateWindowExA`: a top-level window of a class registered by the
/// program. `width` and `height` are the size of the client area
/// (`CW_USEDEFAULT` picks 480 x 320); the position and the styles are
/// ignored, except that child windows are refused. Fails with
/// `ERROR_NOT_SUPPORTED` if no desktop is running.
#[allow(
    clippy::too_many_arguments,
    reason = "the signature is that of the Windows function"
)]
unsafe extern "win64" fn CreateWindowExA(
    ex_style: u32,
    class_name: *const c_char,
    window_name: *const c_char,
    style: u32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    parent: usize,
    menu: usize,
    instance: usize,
    param: usize,
) -> usize {
    // SAFETY: the documentation requires NUL-terminated strings (or, for
    // the class, an atom, which is refused).
    let class = (class_name as usize > 0xFFFF)
        .then(|| unsafe { text(class_name) })
        .flatten();
    // SAFETY: as above; a null title is an empty one.
    let title = unsafe { text(window_name) }.unwrap_or("");
    let Some(class) = class else {
        set_last_error(ERROR_CANNOT_FIND_WND_CLASS);
        return 0;
    };
    if parent != 0 || style & WS_CHILD != 0 {
        set_last_error(ERROR_NOT_SUPPORTED);
        return 0;
    }
    let (width, height) = if width == CW_USEDEFAULT || height == CW_USEDEFAULT {
        DEFAULT_SIZE
    } else {
        (width, height)
    };
    if !(1..=MAX_SIZE).contains(&width) || !(1..=MAX_SIZE).contains(&height) {
        set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let found = with_gui(|gui| {
        gui.classes
            .iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(class))
            .map(|class| (class.procedure, class.background))
    });
    let Some((procedure, background)) = found else {
        set_last_error(ERROR_CANNOT_FIND_WND_CLASS);
        return 0;
    };
    let Ok(window) = Window::open(title, width as usize, height as usize) else {
        set_last_error(ERROR_NOT_SUPPORTED);
        return 0;
    };
    let hwnd = with_gui(|gui| {
        let hwnd = HWND_BASE | gui.next_hwnd;
        gui.next_hwnd = gui.next_hwnd % 0xFFFF + 1;
        gui.windows.push(Win {
            hwnd,
            procedure,
            background,
            window,
            image: Surface::new(width as usize, height as usize, background.unwrap_or(WHITE)),
            invalid: true,
            gone: false,
            left_down: false,
        });
        hwnd
    });
    let create = CreateStructA {
        create_params: param,
        instance,
        menu,
        parent,
        cy: height,
        cx: width,
        y,
        x,
        style: style as i32,
        name: window_name,
        class: class_name,
        ex_style,
    };
    if send(hwnd, WM_CREATE, 0, &raw const create as isize) == -1 {
        with_gui(|gui| gui.windows.retain(|window| window.hwnd != hwnd));
        return 0;
    }
    let size = ((height as u32) << 16 | width as u32 & 0xFFFF) as isize;
    send(hwnd, WM_SIZE, 0, size);
    hwnd
}

/// `ShowWindow`: windows are always shown, so this only reports that.
extern "win64" fn ShowWindow(hwnd: usize, _command: i32) -> Bool {
    with_gui(|gui| gui.window(hwnd).is_some()) as Bool
}

/// `UpdateWindow`: paints the window now if it needs painting.
extern "win64" fn UpdateWindow(hwnd: usize) -> Bool {
    match with_gui(|gui| {
        gui.window(hwnd)
            .map(|window| window.invalid && !window.gone)
    }) {
        Some(true) => {
            send(hwnd, WM_PAINT, 0, 0);
            TRUE
        }
        Some(false) => TRUE,
        None => FALSE,
    }
}

/// Waits for the next message.
fn wait_message() -> Msg {
    loop {
        let (message, descriptors, timer) = with_gui(|gui| {
            let message = gui.next_message(true);
            let descriptors: Vec<u64> = gui
                .windows
                .iter()
                .filter(|window| !window.gone)
                .map(|window| window.window.descriptor())
                .collect();
            (message, descriptors, gui.next_timer())
        });
        if let Some(message) = message {
            return message;
        }
        // Sleep until the desktop sends something or a timer is due.
        // (Another thread's `PostMessageA` is only noticed at the next
        // look, hence the upper limit.)
        if descriptors.is_empty() {
            omit_rt::sleep_ms(timer.unwrap_or(20).clamp(1, 20));
        } else {
            let _ = omit_rt::poll(&descriptors, timer.unwrap_or(1000).clamp(1, 1000), false);
        }
    }
}

/// `GetMessageA`: waits for a message for any window of the program
/// (the window and range filters are ignored). Returns 0 for `WM_QUIT`.
unsafe extern "win64" fn GetMessageA(
    message: *mut Msg,
    _hwnd: usize,
    _min: u32,
    _max: u32,
) -> Bool {
    if message.is_null() {
        set_last_error(ERROR_INVALID_PARAMETER);
        return -1;
    }
    let next = wait_message();
    // SAFETY: the program passes a pointer to its `MSG`.
    unsafe { message.write(next) };
    (next.message != WM_QUIT) as Bool
}

/// `PeekMessageA`: like `GetMessageA` without waiting. `PM_REMOVE` (1)
/// is honoured; the filters are ignored.
unsafe extern "win64" fn PeekMessageA(
    message: *mut Msg,
    _hwnd: usize,
    _min: u32,
    _max: u32,
    remove: u32,
) -> Bool {
    if message.is_null() {
        return FALSE;
    }
    match with_gui(|gui| gui.next_message(remove & 1 != 0)) {
        Some(next) => {
            // SAFETY: the program passes a pointer to its `MSG`.
            unsafe { message.write(next) };
            TRUE
        }
        None => FALSE,
    }
}

/// `TranslateMessage`: after a `WM_KEYDOWN` of a key that produces a
/// character, queues the `WM_CHAR` (ahead of everything else).
unsafe extern "win64" fn TranslateMessage(message: *const Msg) -> Bool {
    if message.is_null() {
        return FALSE;
    }
    // SAFETY: the program passes a pointer to a `MSG` it received.
    let message = unsafe { *message };
    if message.message != WM_KEYDOWN || message.private == 0 {
        return FALSE;
    }
    with_gui(|gui| {
        gui.queue.push_front(Msg {
            message: WM_CHAR,
            wparam: message.private as usize,
            private: 0,
            ..message
        });
    });
    TRUE
}

/// `DispatchMessageA`: calls the window procedure of the message's
/// window.
unsafe extern "win64" fn DispatchMessageA(message: *const Msg) -> isize {
    if message.is_null() {
        return 0;
    }
    // SAFETY: the program passes a pointer to a `MSG` it received.
    let message = unsafe { *message };
    if message.message == WM_TIMER && message.lparam != 0 {
        // A timer with a callback: `lparam` is the `TIMERPROC` that was
        // given to `SetTimer` (the message was built from the timer
        // table, or forged by the program itself, at its own risk — as
        // on Windows).
        // SAFETY: see above; the signature is that of `TIMERPROC`.
        let callback: extern "win64" fn(usize, u32, usize, u32) =
            unsafe { core::mem::transmute(message.lparam as usize) };
        callback(message.hwnd, WM_TIMER, message.wparam, message.time);
        return 0;
    }
    send(
        message.hwnd,
        message.message,
        message.wparam,
        message.lparam,
    )
}

/// `SetTimer`: `WM_TIMER` every `elapse` milliseconds (at least 10),
/// delivered to the window's procedure or, if `callback` is given, to
/// that function when the message is dispatched. With a window, `id`
/// names the timer and setting it again replaces it; without one, a new
/// identifier is chosen. Returns the identifier, 0 on failure.
extern "win64" fn SetTimer(hwnd: usize, id: usize, elapse: u32, callback: usize) -> usize {
    let period_ms = u64::from(elapse.clamp(10, 0x7FFF_FFFF));
    with_gui(|gui| {
        if hwnd != 0 && gui.window(hwnd).is_none() {
            set_last_error(ERROR_INVALID_WINDOW_HANDLE);
            return 0;
        }
        let id = if hwnd != 0 {
            id
        } else {
            // An identifier no window-less timer has.
            (1..)
                .find(|candidate| {
                    gui.timers
                        .iter()
                        .all(|timer| timer.hwnd != 0 || timer.id != *candidate)
                })
                .unwrap_or(1)
        };
        gui.timers
            .retain(|timer| timer.hwnd != hwnd || timer.id != id);
        gui.timers.push(Timer {
            hwnd,
            id,
            period_ms,
            due: omit_rt::uptime_ms() + period_ms,
            callback,
        });
        // The result must be nonzero even for a window's timer 0.
        if id == 0 { 1 } else { id }
    })
}

/// `KillTimer`.
extern "win64" fn KillTimer(hwnd: usize, id: usize) -> Bool {
    with_gui(|gui| {
        let before = gui.timers.len();
        gui.timers
            .retain(|timer| timer.hwnd != hwnd || timer.id != id);
        (gui.timers.len() != before) as Bool
    })
}

/// `DefWindowProcA`: `WM_CLOSE` destroys the window, `WM_PAINT` erases
/// it to the class background; everything else is ignored.
extern "win64" fn DefWindowProcA(
    hwnd: usize,
    message: u32,
    _wparam: usize,
    _lparam: isize,
) -> isize {
    match message {
        WM_CLOSE => {
            DestroyWindow(hwnd);
        }
        WM_PAINT => {
            let hdc = begin_paint(hwnd);
            end_paint(hwnd, hdc);
        }
        _ => {}
    }
    0
}

/// `DestroyWindow`: sends `WM_DESTROY`, then closes the window.
extern "win64" fn DestroyWindow(hwnd: usize) -> Bool {
    if !with_gui(|gui| gui.window(hwnd).is_some()) {
        set_last_error(ERROR_INVALID_WINDOW_HANDLE);
        return FALSE;
    }
    send(hwnd, WM_DESTROY, 0, 0);
    // Dropping the window closes it on the desktop — outside the lock:
    // it makes system calls.
    let closed = with_gui(|gui| {
        gui.queue
            .retain(|message| message.hwnd != hwnd || message.message == WM_QUIT);
        gui.timers.retain(|timer| timer.hwnd != hwnd);
        for context in &mut gui.contexts {
            if context.as_ref().is_some_and(|context| context.hwnd == hwnd) {
                *context = None;
            }
        }
        let index = gui.windows.iter().position(|window| window.hwnd == hwnd);
        index.map(|index| gui.windows.swap_remove(index))
    });
    drop(closed);
    TRUE
}

/// `PostQuitMessage`.
extern "win64" fn PostQuitMessage(exit_code: i32) {
    with_gui(|gui| {
        gui.queue.push_back(Msg {
            message: WM_QUIT,
            wparam: exit_code as usize,
            ..Msg::default()
        });
    });
}

/// `PostMessageA`: queues a message for a window of this program.
extern "win64" fn PostMessageA(hwnd: usize, message: u32, wparam: usize, lparam: isize) -> Bool {
    with_gui(|gui| {
        if gui.window(hwnd).is_none() {
            set_last_error(ERROR_INVALID_WINDOW_HANDLE);
            return FALSE;
        }
        gui.queue.push_back(Msg {
            hwnd,
            message,
            wparam,
            lparam,
            time: omit_rt::uptime_ms() as u32,
            ..Msg::default()
        });
        TRUE
    })
}

/// `SendMessageA`: calls the window procedure directly (windows of this
/// program only).
extern "win64" fn SendMessageA(hwnd: usize, message: u32, wparam: usize, lparam: isize) -> isize {
    send(hwnd, message, wparam, lparam)
}

fn begin_paint(hwnd: usize) -> usize {
    with_gui(|gui| {
        let Some(window) = gui.window(hwnd) else {
            return 0;
        };
        window.invalid = false;
        if let Some(background) = window.background {
            let bounds = window.image.bounds();
            window.image.canvas().fill(bounds, background);
        }
        gui.open_context(hwnd)
    })
}

fn end_paint(hwnd: usize, hdc: usize) {
    with_gui(|gui| {
        if gui.close_context(hdc).is_some() {
            gui.present(hwnd);
        }
    });
}

/// `BeginPaint`: erases the client area to the class background (if the
/// class has one) and returns a device context for the whole of it.
unsafe extern "win64" fn BeginPaint(hwnd: usize, paint: *mut PaintStruct) -> usize {
    let hdc = begin_paint(hwnd);
    if hdc != 0 && !paint.is_null() {
        let area = client_rect(hwnd).unwrap_or_default();
        // SAFETY: the program passes a pointer to its `PAINTSTRUCT`.
        unsafe {
            paint.write(PaintStruct {
                hdc,
                erase: FALSE,
                paint: area,
                restore: FALSE,
                inc_update: FALSE,
                reserved: [0; 32],
            });
        }
    }
    hdc
}

/// `EndPaint`: shows what was painted.
unsafe extern "win64" fn EndPaint(hwnd: usize, paint: *const PaintStruct) -> Bool {
    if !paint.is_null() {
        // SAFETY: the program passes the structure `BeginPaint` filled.
        end_paint(hwnd, unsafe { (*paint).hdc });
    }
    TRUE
}

/// `GetDC`: a device context for a window's client area. (Not for the
/// screen: a null window is refused.)
extern "win64" fn GetDC(hwnd: usize) -> usize {
    with_gui(|gui| {
        if gui.window(hwnd).is_none() {
            return 0;
        }
        gui.open_context(hwnd)
    })
}

/// `ReleaseDC`: shows what was drawn.
extern "win64" fn ReleaseDC(hwnd: usize, hdc: usize) -> i32 {
    with_gui(|gui| match gui.close_context(hdc) {
        Some(context) => {
            gui.present(context.hwnd);
            let _ = hwnd;
            1
        }
        None => 0,
    })
}

fn client_rect(hwnd: usize) -> Option<Rect> {
    with_gui(|gui| {
        gui.window(hwnd).map(|window| Rect {
            left: 0,
            top: 0,
            right: window.window.width() as i32,
            bottom: window.window.height() as i32,
        })
    })
}

/// `GetClientRect`.
unsafe extern "win64" fn GetClientRect(hwnd: usize, rect: *mut Rect) -> Bool {
    match client_rect(hwnd) {
        Some(area) if !rect.is_null() => {
            // SAFETY: the program passes a pointer to its `RECT`.
            unsafe { rect.write(area) };
            TRUE
        }
        _ => {
            set_last_error(ERROR_INVALID_WINDOW_HANDLE);
            FALSE
        }
    }
}

/// `InvalidateRect`: the whole window will be painted again, whatever
/// the rectangle.
extern "win64" fn InvalidateRect(hwnd: usize, _rect: *const Rect, _erase: Bool) -> Bool {
    with_gui(|gui| match gui.window(hwnd) {
        Some(window) => {
            window.invalid = true;
            TRUE
        }
        None => FALSE,
    })
}

/// `SetWindowTextA`: changes the title.
unsafe extern "win64" fn SetWindowTextA(hwnd: usize, title: *const c_char) -> Bool {
    // SAFETY: the documentation requires a NUL-terminated string.
    let title = unsafe { text(title) }.unwrap_or("");
    with_gui(|gui| match gui.window(hwnd) {
        Some(window) => {
            window.window.set_title(title);
            TRUE
        }
        None => {
            set_last_error(ERROR_INVALID_WINDOW_HANDLE);
            FALSE
        }
    })
}

/// `LoadCursorA`: cursors are not shown; any request "succeeds" with a
/// placeholder handle so that class registration code works unchanged.
extern "win64" fn LoadCursorA(_instance: usize, _name: *const c_char) -> usize {
    STOCK_BASE | 0xFFFF
}

/// `LoadIconA`: see [`LoadCursorA`].
extern "win64" fn LoadIconA(_instance: usize, _name: *const c_char) -> usize {
    STOCK_BASE | 0xFFFE
}

/// `GetSystemMetrics`: the screen size (`SM_CXSCREEN`, `SM_CYSCREEN`);
/// 0 for everything else.
extern "win64" fn GetSystemMetrics(index: i32) -> i32 {
    match (index, omit_rt::display_info()) {
        (0, Ok(info)) => info.width as i32,
        (1, Ok(info)) => info.height as i32,
        _ => 0,
    }
}

/// `FillRect`.
unsafe extern "win64" fn FillRect(hdc: usize, rect: *const Rect, brush: usize) -> i32 {
    if rect.is_null() {
        return 0;
    }
    // SAFETY: the program passes a pointer to its `RECT`.
    let area = unsafe { *rect }.area();
    with_gui(|gui| {
        // A system colour index plus one is allowed here as well.
        let colour = match class_background(gui, brush) {
            Some(colour) => colour,
            None => return 0,
        };
        gui.draw(hdc, |_, _, canvas| canvas.fill(area, colour))
            .map_or(0, |()| 1)
    })
}

/// `MessageBoxA`: shows the text in a window with an OK button and
/// waits until it is dismissed (Enter, Escape, a click on the button, or
/// closing the window). Always returns `IDOK`: other button sets are
/// not offered. Without a desktop the text goes to standard error.
unsafe extern "win64" fn MessageBoxA(
    _owner: usize,
    message: *const c_char,
    caption: *const c_char,
    _kind: u32,
) -> i32 {
    const IDOK: i32 = 1;
    const MARGIN: i32 = 16;
    const BUTTON: (i32, i32) = (80, 26);
    // SAFETY: the documentation requires NUL-terminated strings.
    let (message, caption) = unsafe {
        (
            text(message).unwrap_or(""),
            text(caption).unwrap_or("Error"),
        )
    };
    let lines: Vec<&str> = message.lines().collect();
    let text_width_max = lines.iter().map(|line| text_width(line)).max().unwrap_or(0);
    let width = (text_width_max + 2 * MARGIN).clamp(200, 800);
    let height = lines.len().min(30) as i32 * (TEXT_HEIGHT + 2) + 3 * MARGIN + BUTTON.1;
    let Ok(mut window) = Window::open(caption, width as usize, height as usize) else {
        omit_rt::eprintln!("{caption}: {message}");
        return IDOK;
    };
    let button = Area::new(
        (width - BUTTON.0) / 2,
        height - MARGIN - BUTTON.1,
        BUTTON.0,
        BUTTON.1,
    );
    let mut image = Surface::new(width as usize, height as usize, FACE);
    {
        let mut canvas = image.canvas();
        for (index, line) in lines.iter().take(30).enumerate() {
            canvas.text(
                MARGIN,
                MARGIN + index as i32 * (TEXT_HEIGHT + 2),
                line,
                BLACK,
            );
        }
        canvas.fill(button, 0xE1_E1_E1);
        canvas.outline(button, 0x00_78_D7);
        canvas.text(
            button.x + (button.w - text_width("OK")) / 2,
            button.y + (button.h - TEXT_HEIGHT) / 2,
            "OK",
            BLACK,
        );
    }
    window.pixels().copy_from_slice(image.pixels());
    window.present();
    loop {
        match window.event(None) {
            Some(Event::Key(13 | 27 | 32)) | Some(Event::Closed) => return IDOK,
            Some(Event::Pointer { x, y, buttons })
                if buttons & 1 != 0 && button.contains(omit_gfx::geometry::Point::new(x, y)) =>
            {
                return IDOK;
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------
// GDI32
// ---------------------------------------------------------------------

/// `GetStockObject`: the stock brushes, pens and (single) font.
extern "win64" fn GetStockObject(index: i32) -> usize {
    match usize::try_from(index)
        .ok()
        .filter(|&index| stock(index).is_some())
    {
        Some(index) => STOCK_BASE | index,
        None => 0,
    }
}

/// `CreateSolidBrush`.
extern "win64" fn CreateSolidBrush(color: u32) -> usize {
    with_gui(|gui| gui.add_object(Object::Brush(Some(colour(color)))))
}

/// `CreatePen`: a solid pen one pixel wide, whatever width is asked
/// for; style `PS_NULL` (5) gives a pen that draws nothing.
extern "win64" fn CreatePen(style: i32, _width: i32, color: u32) -> usize {
    let pen = if style == 5 {
        None
    } else {
        Some(colour(color))
    };
    with_gui(|gui| gui.add_object(Object::Pen(pen)))
}

/// `DeleteObject`. Stock objects are left alone, as documented.
extern "win64" fn DeleteObject(object: usize) -> Bool {
    with_gui(|gui| match object & KIND_MASK {
        STOCK_BASE => TRUE,
        OBJECT_BASE => match gui.objects.get_mut(object & 0xFFFF) {
            Some(slot @ Some(_)) => {
                *slot = None;
                TRUE
            }
            _ => FALSE,
        },
        _ => FALSE,
    })
}

/// `SelectObject`: brushes, pens and fonts. Returns the object of the
/// same kind that was selected before.
extern "win64" fn SelectObject(hdc: usize, object: usize) -> usize {
    with_gui(|gui| {
        let Some(kind) = gui.object(object) else {
            return 0;
        };
        let Some(context) = gui.context(hdc) else {
            return 0;
        };
        let slot = match kind {
            Object::Brush(_) => &mut context.brush,
            Object::Pen(_) => &mut context.pen,
            Object::Font => &mut context.font,
        };
        core::mem::replace(slot, object)
    })
}

/// `SetTextColor`. Returns the previous colour.
extern "win64" fn SetTextColor(hdc: usize, color: u32) -> u32 {
    with_gui(|gui| match gui.context(hdc) {
        Some(context) => colorref(core::mem::replace(&mut context.text_colour, colour(color))),
        None => u32::MAX,
    })
}

/// `SetBkColor`. Returns the previous colour.
extern "win64" fn SetBkColor(hdc: usize, color: u32) -> u32 {
    with_gui(|gui| match gui.context(hdc) {
        Some(context) => colorref(core::mem::replace(
            &mut context.background_colour,
            colour(color),
        )),
        None => u32::MAX,
    })
}

/// `SetBkMode`: `TRANSPARENT` (1) or `OPAQUE` (2). Returns the previous
/// mode.
extern "win64" fn SetBkMode(hdc: usize, mode: i32) -> i32 {
    with_gui(|gui| match gui.context(hdc) {
        Some(context) if mode == 1 || mode == 2 => {
            let previous = if context.opaque { 2 } else { 1 };
            context.opaque = mode == 2;
            previous
        }
        _ => 0,
    })
}

/// The text of a counted string argument.
///
/// # Safety
///
/// `pointer` must point to `length` readable bytes.
unsafe fn counted<'a>(pointer: *const u8, length: i32) -> Option<&'a str> {
    let length = usize::try_from(length).ok()?;
    if pointer.is_null() {
        return (length == 0).then_some("");
    }
    // SAFETY: guaranteed by the caller.
    core::str::from_utf8(unsafe { core::slice::from_raw_parts(pointer, length) }).ok()
}

/// `TextOutA`: draws text in the built-in font with its top-left corner
/// at (`x`, `y`), on the background colour if the mode is `OPAQUE`.
unsafe extern "win64" fn TextOutA(
    hdc: usize,
    x: i32,
    y: i32,
    string: *const u8,
    length: i32,
) -> Bool {
    // SAFETY: the documentation requires `length` bytes at `string`.
    let Some(string) = (unsafe { counted(string, length) }) else {
        return FALSE;
    };
    with_gui(|gui| {
        gui.draw(hdc, |context, _, canvas| {
            if context.opaque {
                canvas.fill(
                    Area::new(x, y, text_width(string), TEXT_HEIGHT),
                    context.background_colour,
                );
            }
            canvas.text(x, y, string, context.text_colour);
        })
        .is_some() as Bool
    })
}

/// `GetTextExtentPoint32A`: the size of text in the built-in font.
unsafe extern "win64" fn GetTextExtentPoint32A(
    _hdc: usize,
    string: *const u8,
    length: i32,
    size: *mut Size,
) -> Bool {
    // SAFETY: the documentation requires `length` bytes at `string`.
    let Some(string) = (unsafe { counted(string, length) }) else {
        return FALSE;
    };
    if size.is_null() {
        return FALSE;
    }
    // SAFETY: the program passes a pointer to its `SIZE`.
    unsafe {
        size.write(Size {
            cx: text_width(string),
            cy: TEXT_HEIGHT,
        });
    }
    TRUE
}

/// `Rectangle`: filled with the selected brush, outlined with the
/// selected pen.
extern "win64" fn Rectangle(hdc: usize, left: i32, top: i32, right: i32, bottom: i32) -> Bool {
    let area = Rect {
        left,
        top,
        right,
        bottom,
    }
    .area();
    with_gui(|gui| {
        gui.draw(hdc, |context, gui, canvas| {
            if let Some(Object::Brush(Some(colour))) = gui.object(context.brush) {
                canvas.fill(area, colour);
            }
            if let Some(Object::Pen(Some(colour))) = gui.object(context.pen) {
                canvas.outline(area, colour);
            }
        })
        .is_some() as Bool
    })
}

/// `SetPixel`. Returns the colour set.
extern "win64" fn SetPixel(hdc: usize, x: i32, y: i32, color: u32) -> u32 {
    with_gui(|gui| {
        gui.draw(hdc, |_, _, canvas| canvas.set(x, y, colour(color)))
            .map_or(u32::MAX, |()| color)
    })
}

/// `MoveToEx`: sets the position `LineTo` starts from.
unsafe extern "win64" fn MoveToEx(hdc: usize, x: i32, y: i32, previous: *mut Point) -> Bool {
    with_gui(|gui| match gui.context(hdc) {
        Some(context) => {
            let (old_x, old_y) = core::mem::replace(&mut context.position, (x, y));
            if !previous.is_null() {
                // SAFETY: the program passes a pointer to its `POINT`.
                unsafe { previous.write(Point { x: old_x, y: old_y }) };
            }
            TRUE
        }
        None => FALSE,
    })
}

/// `LineTo`: a line in the selected pen's colour from the current
/// position up to, but not including, (`x`, `y`), which becomes the
/// current position.
extern "win64" fn LineTo(hdc: usize, x: i32, y: i32) -> Bool {
    with_gui(|gui| {
        let drawn = gui.draw(hdc, |context, gui, canvas| {
            let Some(Object::Pen(Some(colour))) = gui.object(context.pen) else {
                return;
            };
            let (from_x, from_y) = context.position;
            let steps = (x - from_x).abs().max((y - from_y).abs());
            for step in 0..steps {
                canvas.set(
                    from_x + (x - from_x) * step / steps,
                    from_y + (y - from_y) * step / steps,
                    colour,
                );
            }
        });
        if let Some(context) = gui.context(hdc) {
            context.position = (x, y);
        }
        drawn.is_some() as Bool
    })
}

// ---------------------------------------------------------------------
// The export tables
// ---------------------------------------------------------------------

macro_rules! exports {
    ($names:ident, $lookup:ident: $($name:ident),* $(,)?) => {
        /// Names of the implemented functions, in alphabetical order.
        pub const $names: &[&str] = &[$(stringify!($name)),*];

        fn $lookup(name: &str) -> Option<usize> {
            match name {
                $(stringify!($name) => Some($name as *const () as usize),)*
                _ => None,
            }
        }
    };
}

exports! { USER32_NAMES, user32:
    BeginPaint,
    CreateWindowExA,
    DefWindowProcA,
    DestroyWindow,
    DispatchMessageA,
    EndPaint,
    FillRect,
    GetClientRect,
    GetDC,
    GetMessageA,
    GetSystemMetrics,
    InvalidateRect,
    KillTimer,
    LoadCursorA,
    LoadIconA,
    MessageBoxA,
    PeekMessageA,
    PostMessageA,
    PostQuitMessage,
    RegisterClassA,
    RegisterClassExA,
    ReleaseDC,
    SendMessageA,
    SetTimer,
    SetWindowTextA,
    ShowWindow,
    TranslateMessage,
    UpdateWindow,
}

exports! { GDI32_NAMES, gdi32:
    CreatePen,
    CreateSolidBrush,
    DeleteObject,
    GetStockObject,
    GetTextExtentPoint32A,
    LineTo,
    MoveToEx,
    Rectangle,
    SelectObject,
    SetBkColor,
    SetBkMode,
    SetPixel,
    SetTextColor,
    TextOutA,
}

/// The implementation of `symbol` in the system library `library`
/// (`USER32.dll` or `GDI32.dll`), if this module provides that library:
/// `None` if it does not, `Some(None)` if the function is missing.
pub fn resolve(library: &str, symbol: Symbol<'_>) -> Option<Option<usize>> {
    let lookup: fn(&str) -> Option<usize> = if library.eq_ignore_ascii_case("user32.dll") {
        user32
    } else if library.eq_ignore_ascii_case("gdi32.dll") {
        gdi32
    } else {
        return None;
    };
    Some(match symbol {
        Symbol::Name(name) => lookup(name),
        Symbol::Ordinal(_) => None,
    })
}
