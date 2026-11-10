// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
// Sources: an original test program; Microsoft Learn reference pages of the
//   functions it calls. It is part of the behaviour tests of the Windows
//   compatibility layer (QEMU tests windows-programs and desktop).
//! A graphical Windows program in the classic shape: a window class, a
//! window, a message loop and a window procedure that paints and reacts
//! to the keyboard and the mouse. It uses only documented functions of
//! `USER32.dll`, `GDI32.dll` and `KERNEL32.dll`.
//!
//! * A click puts a red square where the pointer is and counts.
//! * Typed characters are shown; `m` opens a message box, `t` puts the
//!   number of clicks in the title, `x` starts a timer that counts to
//!   five, `q` closes the window.
//!
//! On exit it reports what happened on standard output, which is how
//! the test checks the parts that cannot be seen on the screen.

#![no_std]
#![no_main]

use core::ffi::c_void;
use core::fmt::Write as _;
use core::ptr::{null, null_mut};
use core::sync::atomic::{AtomicI32, AtomicU8, AtomicU32, Ordering};

use omit_win_samples::*;

/// What `WM_CREATE` must find in `lpCreateParams`.
const CREATE_PARAMETER: usize = 0x1234_5678;
/// Posted by the program to itself, to check that posted messages arrive
/// in order and reach the window procedure.
const WM_APP_PING: u32 = WM_USER + 1;

static CLICKS: AtomicU32 = AtomicU32::new(0);
static LAST_X: AtomicI32 = AtomicI32::new(-100);
static LAST_Y: AtomicI32 = AtomicI32::new(-100);
/// Bit 0: `WM_CREATE` carried the parameter; bit 1: `WM_SIZE` reported
/// the client size; bit 2: the posted message arrived; bit 3: a key
/// press was followed by its release.
static SEEN: AtomicU32 = AtomicU32::new(0);
static PAINTS: AtomicU32 = AtomicU32::new(0);
/// `WM_TIMER` messages received from the timer that `x` starts.
static TICKS: AtomicU32 = AtomicU32::new(0);
/// Calls of [`timer_procedure`].
static CALLBACKS: AtomicU32 = AtomicU32::new(0);
/// The identifier of the timer that `x` starts.
const TICK_TIMER: usize = 7;
static TYPED_LENGTH: AtomicU32 = AtomicU32::new(0);
/// The characters typed so far.
static TYPED: [AtomicU8; 24] = [const { AtomicU8::new(0) }; 24];

const WIDTH: i32 = 340;
const HEIGHT: i32 = 150;

/// The characters typed so far, as text.
fn typed() -> Text<24> {
    let mut text = Text::new();
    for slot in &TYPED[..TYPED_LENGTH.load(Ordering::Relaxed) as usize] {
        let _ = text.write_char(char::from(slot.load(Ordering::Relaxed)));
    }
    text
}

fn paint(window: Handle) {
    let mut paint = core::mem::MaybeUninit::<PaintStruct>::uninit();
    // SAFETY: `BeginPaint` fills the structure.
    let context = unsafe { BeginPaint(window, paint.as_mut_ptr()) };
    let line = |row: i32, text: &[u8]| {
        // SAFETY: the pointer and length describe a live slice.
        unsafe { TextOutA(context, 12, 10 + row * 20, text.as_ptr(), text.len() as i32) };
    };
    line(0, b"Hello from a Windows GUI program");
    let mut text = Text::<64>::new();
    let _ = write!(text, "clicks: {}", CLICKS.load(Ordering::Relaxed));
    line(1, text.as_bytes());
    text.clear();
    let _ = write!(text, "typed: {}", typed().as_str());
    line(2, text.as_bytes());
    text.clear();
    let _ = write!(text, "ticks: {}", TICKS.load(Ordering::Relaxed));
    line(3, text.as_bytes());

    // A red square, outlined in black, where the last click was.
    let brush = CreateSolidBrush(rgb(200, 30, 30));
    let old_brush = SelectObject(context, brush);
    SelectObject(context, GetStockObject(BLACK_PEN));
    let (x, y) = (
        LAST_X.load(Ordering::Relaxed),
        LAST_Y.load(Ordering::Relaxed),
    );
    Rectangle(context, x - 6, y - 6, x + 7, y + 7);
    SelectObject(context, old_brush);
    DeleteObject(brush);
    // A blue line along the bottom.
    let pen = CreatePen(0, 1, rgb(30, 60, 200));
    let old_pen = SelectObject(context, pen);
    // SAFETY: a null "previous position" pointer is allowed.
    unsafe { MoveToEx(context, 10, HEIGHT - 10, null_mut()) };
    LineTo(context, WIDTH - 10, HEIGHT - 10);
    SelectObject(context, old_pen);
    DeleteObject(pen);
    // SAFETY: the structure was filled by `BeginPaint`.
    unsafe { EndPaint(window, paint.as_ptr()) };
    PAINTS.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "system" fn window_procedure(
    window: Handle,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    match message {
        WM_CREATE => {
            // SAFETY: with `WM_CREATE`, `lparam` points to the
            // `CREATESTRUCTA` of the window being created.
            let create = unsafe { &*(lparam as *const CreateStructA) };
            if create.create_params as usize == CREATE_PARAMETER && create.cx == WIDTH {
                SEEN.fetch_or(1, Ordering::Relaxed);
            }
            0
        }
        WM_SIZE => {
            if (lparam & 0xFFFF) as i32 == WIDTH && ((lparam >> 16) & 0xFFFF) as i32 == HEIGHT {
                SEEN.fetch_or(2, Ordering::Relaxed);
            }
            0
        }
        WM_APP_PING => {
            if wparam == 42 && lparam == -7 {
                SEEN.fetch_or(4, Ordering::Relaxed);
            }
            0
        }
        WM_PAINT => {
            paint(window);
            0
        }
        WM_LBUTTONDOWN => {
            CLICKS.fetch_add(1, Ordering::Relaxed);
            LAST_X.store((lparam & 0xFFFF) as i16 as i32, Ordering::Relaxed);
            LAST_Y.store(((lparam >> 16) & 0xFFFF) as i16 as i32, Ordering::Relaxed);
            // SAFETY: a null rectangle means the whole window.
            unsafe { InvalidateRect(window, null(), 1) };
            0
        }
        WM_TIMER if wparam == TICK_TIMER => {
            // Five ticks, then the timer stops itself.
            if TICKS.fetch_add(1, Ordering::Relaxed) + 1 == 5 {
                KillTimer(window, TICK_TIMER);
            }
            // SAFETY: a null rectangle means the whole window.
            unsafe { InvalidateRect(window, null(), 1) };
            0
        }
        WM_KEYUP => {
            SEEN.fetch_or(8, Ordering::Relaxed);
            0
        }
        WM_CHAR => {
            match wparam as u8 {
                b'q' => {
                    DestroyWindow(window);
                }
                b'm' => {
                    // SAFETY: NUL-terminated strings.
                    unsafe {
                        MessageBoxA(
                            window,
                            c"This is a message box.\nPress Enter to close it.".as_ptr(),
                            c"wingui".as_ptr(),
                            0,
                        );
                    }
                }
                b'x' => {
                    // SAFETY: no callback; the message goes to this
                    // window procedure.
                    unsafe { SetTimer(window, TICK_TIMER, 50, None) };
                }
                b't' => {
                    let mut title = Text::<48>::new();
                    let _ = write!(title, "{} click(s)\0", CLICKS.load(Ordering::Relaxed));
                    // SAFETY: the text ends with a NUL.
                    unsafe { SetWindowTextA(window, title.as_bytes().as_ptr().cast()) };
                }
                character @ b' '..=b'~' => {
                    let length = TYPED_LENGTH.load(Ordering::Relaxed) as usize;
                    if let Some(slot) = TYPED.get(length) {
                        slot.store(character, Ordering::Relaxed);
                        TYPED_LENGTH.store(length as u32 + 1, Ordering::Relaxed);
                    }
                }
                _ => {}
            }
            // SAFETY: as above. (After `DestroyWindow` this fails,
            // harmlessly.)
            unsafe { InvalidateRect(window, null(), 1) };
            0
        }
        WM_DESTROY => {
            PostQuitMessage(7);
            0
        }
        _ => DefWindowProcA(window, message, wparam, lparam),
    }
}

/// Counts its calls; see the timer checks in [`startup_checks`].
unsafe extern "system" fn timer_procedure(_window: Handle, message: u32, _id: usize, _time: u32) {
    if message == WM_TIMER {
        CALLBACKS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Checks, right after the window exists, the functions whose results
/// can be judged without looking at the screen — and leaves a green
/// square in the window (drawn outside `WM_PAINT`, through `GetDC`) that
/// the test looks for.
fn startup_checks(window: Handle, class: &WndClassA) {
    let mut checks = Checks::new("wingui");
    checks.check(
        PAINTS.load(Ordering::Relaxed) == 1,
        "UpdateWindow paints at once",
    );
    checks.check(
        UpdateWindow(window) != 0 && PAINTS.load(Ordering::Relaxed) == 1,
        "UpdateWindow leaves a painted window alone",
    );

    let mut area = Rect::default();
    // SAFETY: `area` is a live `RECT`.
    checks.check(
        unsafe { GetClientRect(window, &raw mut area) } != 0,
        "GetClientRect",
    );
    let expected = Rect {
        left: 0,
        top: 0,
        right: WIDTH,
        bottom: HEIGHT,
    };
    checks.check(area == expected, "the client area has the requested size");
    checks.check(
        GetSystemMetrics(0) >= WIDTH,
        "GetSystemMetrics(SM_CXSCREEN)",
    );

    // Classes and windows that cannot be.
    SetLastError(0);
    // SAFETY: the structure is the one registered before.
    checks.check(
        unsafe { RegisterClassA(class) } == 0 && GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
        "a class cannot be registered twice",
    );
    // SAFETY: NUL-terminated strings.
    let nothing = unsafe {
        CreateWindowExA(
            0,
            c"NoSuchClass".as_ptr(),
            c"".as_ptr(),
            0,
            0,
            0,
            10,
            10,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
        )
    };
    checks.check(
        nothing.is_null() && GetLastError() == ERROR_CANNOT_FIND_WND_CLASS,
        "no window of an unregistered class",
    );
    let extended = WndClassExA {
        size: size_of::<WndClassExA>() as u32,
        style: 0,
        window_procedure: Some(window_procedure),
        class_extra: 0,
        window_extra: 0,
        instance: class.instance,
        icon: null_mut(),
        cursor: null_mut(),
        background: 0,
        menu_name: null(),
        class_name: c"OmitGuiTestEx".as_ptr(),
        small_icon: null_mut(),
    };
    // SAFETY: the structure is complete.
    checks.check(
        unsafe { RegisterClassExA(&raw const extended) } != 0,
        "RegisterClassExA",
    );

    // Messages: sent ones are delivered at once, posted ones wait.
    SEEN.fetch_and(!4, Ordering::Relaxed);
    checks.check(
        SendMessageA(window, WM_APP_PING, 42, -7) == 0 && SEEN.load(Ordering::Relaxed) & 4 != 0,
        "SendMessageA calls the window procedure",
    );
    SEEN.fetch_and(!4, Ordering::Relaxed);
    let mut message = core::mem::MaybeUninit::<Msg>::uninit();
    // SAFETY: `PeekMessageA` fills the structure when it returns nonzero.
    unsafe {
        checks.check(
            PeekMessageA(message.as_mut_ptr(), null_mut(), 0, 0, PM_NOREMOVE) == 0,
            "no message is waiting",
        );
        PostMessageA(window, WM_USER + 2, 5, 6);
        checks.check(
            PeekMessageA(message.as_mut_ptr(), null_mut(), 0, 0, PM_NOREMOVE) != 0
                && message.assume_init_ref().message == WM_USER + 2,
            "a posted message can be looked at",
        );
        checks.check(
            GetMessageA(message.as_mut_ptr(), null_mut(), 0, 0) > 0
                && message.assume_init_ref().message == WM_USER + 2
                && message.assume_init_ref().wparam == 5
                && message.assume_init_ref().lparam == 6,
            "and is then received",
        );
    }
    checks.check(
        PostMessageA(null_mut(), WM_USER, 0, 0) == 0,
        "nothing is posted to a window that does not exist",
    );

    // Timers: one without a window, delivered to a callback when its
    // message is dispatched; it must fire repeatedly, at about the
    // period asked for, and stop when killed.
    // SAFETY: the callback has the signature of a `TIMERPROC`.
    let timer = unsafe { SetTimer(null_mut(), 0, 30, Some(timer_procedure)) };
    checks.check(timer != 0, "SetTimer without a window");
    let started = GetTickCount64();
    // SAFETY: as for the message checks above.
    unsafe {
        while CALLBACKS.load(Ordering::Relaxed) < 3 && GetTickCount64() - started < 5000 {
            if GetMessageA(message.as_mut_ptr(), null_mut(), 0, 0) > 0 {
                DispatchMessageA(message.as_ptr());
            }
        }
    }
    let elapsed = GetTickCount64() - started;
    checks.check(
        CALLBACKS.load(Ordering::Relaxed) == 3,
        "the timer callback is called",
    );
    checks.check(
        (60..1000).contains(&elapsed),
        "three ticks of 30 ms take about 90 ms",
    );
    checks.check(KillTimer(null_mut(), timer) != 0, "KillTimer");
    checks.check(KillTimer(null_mut(), timer) == 0, "a timer is killed once");
    checks.check(KillTimer(window, 99) == 0, "no timer 99");
    // SAFETY: a null window that is not null: an invalid handle.
    checks.check(
        unsafe { SetTimer(0x1234 as Handle, 1, 10, None) } == 0,
        "no timer for a window that does not exist",
    );

    // Drawing objects and a device context outside WM_PAINT.
    checks.check(GetStockObject(99).is_null(), "no stock object 99");
    checks.check(
        DeleteObject(GetStockObject(BLACK_PEN)) != 0,
        "stock objects survive deletion",
    );
    let green = CreateSolidBrush(rgb(20, 160, 60));
    checks.check(!green.is_null(), "CreateSolidBrush");
    let context = GetDC(window);
    checks.check(!context.is_null(), "GetDC");
    checks.check(
        SetTextColor(context, rgb(1, 2, 3)) == 0,
        "the text colour starts black",
    );
    checks.check(
        SetTextColor(context, 0) == rgb(1, 2, 3),
        "SetTextColor returns the old colour",
    );
    checks.check(
        SetBkColor(context, 0) == rgb(255, 255, 255),
        "the background starts white",
    );
    checks.check(
        SetBkMode(context, TRANSPARENT) == OPAQUE,
        "the background mode starts opaque",
    );
    checks.check(
        SetBkMode(context, 7) == 0,
        "an unknown background mode is refused",
    );
    let (mut short, mut long) = (Size::default(), Size::default());
    // SAFETY: the pointers and lengths describe live data.
    unsafe {
        checks.check(
            GetTextExtentPoint32A(context, b"abc".as_ptr(), 3, &raw mut short) != 0
                && GetTextExtentPoint32A(context, b"abcabc".as_ptr(), 6, &raw mut long) != 0,
            "GetTextExtentPoint32A",
        );
    }
    checks.check(
        short.cx > 0 && short.cy > 0 && long.cx == 2 * short.cx && long.cy == short.cy,
        "text extents are proportional to the text",
    );
    let square = Rect {
        left: 300,
        top: 10,
        right: 320,
        bottom: 30,
    };
    // SAFETY: `square` is a live `RECT`.
    checks.check(
        unsafe { FillRect(context, &raw const square, green) } != 0,
        "FillRect",
    );
    checks.check(
        SetPixel(context, 310, 20, rgb(255, 255, 0)) == rgb(255, 255, 0),
        "SetPixel",
    );
    checks.check(
        SelectObject(context, green) == GetStockObject(0),
        "the brush starts white",
    );
    checks.check(ReleaseDC(window, context) == 1, "ReleaseDC");
    checks.check(
        ReleaseDC(window, context) == 0,
        "a released context is gone",
    );
    checks.check(
        SetTextColor(context, 0) == u32::MAX,
        "and cannot be drawn with",
    );
    checks.check(DeleteObject(green) != 0, "DeleteObject");
    checks.check(DeleteObject(green) == 0, "an object is deleted once");
    say!("wingui: {} startup checks passed", checks.count());
}

/// Entry point of a program built without the C runtime.
#[unsafe(no_mangle)]
pub extern "C" fn mainCRTStartup() -> u32 {
    // SAFETY: a null name asks for the program's own module.
    let instance = unsafe { GetModuleHandleA(null()) };
    let class = WndClassA {
        style: 0,
        window_procedure: Some(window_procedure),
        class_extra: 0,
        window_extra: 0,
        instance,
        icon: null_mut(),
        // SAFETY: `IDC_ARROW` (32512) is a resource identifier, passed
        // in place of a name as documented.
        cursor: unsafe { LoadCursorA(null_mut(), 32512 as *const _) },
        background: COLOR_WINDOW + 1,
        menu_name: null(),
        class_name: c"OmitGuiTest".as_ptr(),
    };
    // SAFETY: the structure is complete and its strings NUL-terminated.
    if unsafe { RegisterClassA(&raw const class) } == 0 {
        say!(
            "wingui: cannot register the window class (error {})",
            GetLastError()
        );
        ExitProcess(1);
    }
    // SAFETY: NUL-terminated strings; the other arguments are plain
    // values.
    let window = unsafe {
        CreateWindowExA(
            0,
            c"OmitGuiTest".as_ptr(),
            c"Windows GUI test".as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            WIDTH,
            HEIGHT,
            null_mut(),
            null_mut(),
            instance,
            CREATE_PARAMETER as *mut c_void,
        )
    };
    if window.is_null() {
        say!("wingui: cannot create a window (error {})", GetLastError());
        ExitProcess(1);
    }
    ShowWindow(window, SW_SHOW);
    UpdateWindow(window);
    startup_checks(window, &class);
    PostMessageA(window, WM_APP_PING, 42, -7);

    let mut message = core::mem::MaybeUninit::<Msg>::uninit();
    let mut messages = 0u32;
    // SAFETY: `GetMessageA` fills the structure before the other
    // functions read it.
    let exit_code = unsafe {
        while GetMessageA(message.as_mut_ptr(), null_mut(), 0, 0) > 0 {
            messages += 1;
            TranslateMessage(message.as_ptr());
            DispatchMessageA(message.as_ptr());
        }
        message.assume_init_ref().wparam
    };
    say!(
        "wingui: code {exit_code}, {} click(s), typed \"{}\", checks {:#x}, paints {}, ticks {}",
        CLICKS.load(Ordering::Relaxed),
        typed().as_str(),
        SEEN.load(Ordering::Relaxed),
        if PAINTS.load(Ordering::Relaxed) >= 2 && messages >= 4 {
            "ok"
        } else {
            "missing"
        },
        TICKS.load(Ordering::Relaxed),
    );
    ExitProcess(0)
}
