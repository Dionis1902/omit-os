// SPDX-License-Identifier: GPL-2.0-only
//! Keyboard and mouse events for the process that owns the display.
//!
//! Normally key presses feed the text console. While a process owns the
//! display (see [`crate::display`]), key presses and mouse reports are
//! queued here instead and handed to that process by the `input_read`
//! system call.

use core::sync::atomic::{AtomicBool, Ordering};

use omit_abi::{InputEvent, event, key};
use omit_term::mouse::Report;
use omit_term::scancode::Key;

use crate::sched::WaitQueue;
use crate::sync::SpinLock;

const CAPACITY: usize = 256;

struct Queue {
    events: [InputEvent; CAPACITY],
    head: usize,
    len: usize,
}

const EMPTY: InputEvent = InputEvent {
    kind: 0,
    a: 0,
    b: 0,
    c: 0,
    time_us: 0,
};

fn now_us() -> u64 {
    crate::time::monotonic_ns() / 1000
}

static QUEUE: SpinLock<Queue> = SpinLock::new(Queue {
    events: [EMPTY; CAPACITY],
    head: 0,
    len: 0,
});
/// The display owner, waiting for input.
static ARRIVED: WaitQueue = WaitQueue::new();
/// Whether events are being collected for a display owner.
static GRABBED: AtomicBool = AtomicBool::new(false);

impl Queue {
    fn last_mut(&mut self) -> Option<&mut InputEvent> {
        (self.len > 0).then(|| &mut self.events[(self.head + self.len - 1) % CAPACITY])
    }

    fn push(&mut self, event: InputEvent) {
        if self.len == CAPACITY {
            // The owner is not reading; the newest input is lost.
            return;
        }
        self.events[(self.head + self.len) % CAPACITY] = event;
        self.len += 1;
    }

    fn pop(&mut self) -> Option<InputEvent> {
        (self.len > 0).then(|| {
            let event = self.events[self.head];
            self.head = (self.head + 1) % CAPACITY;
            self.len -= 1;
            event
        })
    }
}

/// Starts or stops collecting events. Either way, pending events are
/// discarded: they were meant for the previous recipient.
pub fn set_grabbed(grabbed: bool) {
    let mut queue = QUEUE.lock();
    queue.len = 0;
    GRABBED.store(grabbed, Ordering::Relaxed);
}

/// Handles a key press from the keyboard driver (interrupt context).
pub fn key_pressed(pressed: Key) {
    if !GRABBED.load(Ordering::Relaxed) {
        if let Key::Char(ch) = pressed {
            crate::console::push_input(ch);
        }
        return;
    }
    let code = match pressed {
        Key::Char(ch) => i32::from(ch),
        Key::Up => key::UP,
        Key::Down => key::DOWN,
        Key::Left => key::LEFT,
        Key::Right => key::RIGHT,
        Key::Home => key::HOME,
        Key::End => key::END,
        Key::PageUp => key::PAGE_UP,
        Key::PageDown => key::PAGE_DOWN,
        Key::Insert => key::INSERT,
        Key::Delete => key::DELETE,
        Key::Function(number) => key::F1 + i32::from(number) - 1,
    };
    QUEUE.lock().push(InputEvent {
        kind: event::KEY,
        a: code,
        b: 0,
        c: 0,
        time_us: now_us(),
    });
    ARRIVED.wake_all();
}

/// Handles a report from the mouse driver (interrupt context).
pub fn mouse_moved(report: Report) {
    if !GRABBED.load(Ordering::Relaxed) {
        return;
    }
    let buttons = u32::from(report.buttons);
    let mut queue = QUEUE.lock();
    // Movement reports arrive far faster than a program redraws; merge
    // one into the previous event when no button changed in between, so
    // that a burst of movement cannot push clicks out of the queue.
    if let Some(last) = queue.last_mut()
        && last.kind == event::MOUSE
        && last.c == buttons
    {
        last.a = last.a.saturating_add(i32::from(report.dx));
        last.b = last.b.saturating_add(i32::from(report.dy));
        return;
    }
    queue.push(InputEvent {
        kind: event::MOUSE,
        a: i32::from(report.dx),
        b: i32::from(report.dy),
        c: buttons,
        time_us: now_us(),
    });
    drop(queue);
    ARRIVED.wake_all();
}

/// Removes the oldest pending event.
pub fn pop() -> Option<InputEvent> {
    QUEUE.lock().pop()
}

/// Blocks the calling thread until an event is pending, the thread has
/// been killed, or `ticks` timer ticks have passed. The interrupt handlers
/// wake the thread as soon as they queue an event, so input is delivered
/// without waiting for the next timer tick.
pub fn wait(ticks: u64) {
    ARRIVED.wait_until_or_timeout(
        || QUEUE.lock().len != 0 || crate::sched::current_killed(),
        ticks,
    );
}
