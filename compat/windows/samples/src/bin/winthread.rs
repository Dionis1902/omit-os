// SPDX-License-Identifier: GPL-2.0-only
//! Tests threads in a Windows program: creation and exit codes, critical
//! sections, events, thread-local storage and per-thread error codes.

#![no_std]
#![no_main]

use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use omit_win_samples::*;

const THREADS: usize = 4;
const ROUNDS: u32 = 5_000;

/// Data shared by the worker threads.
struct Shared {
    section: UnsafeCell<CriticalSection>,
    /// Protected by `section`; deliberately not atomic.
    counter: UnsafeCell<u32>,
    tls_index: AtomicU32,
    /// How many workers found their thread-local value intact.
    tls_intact: AtomicU32,
    thread_ids: [AtomicU32; THREADS],
}

// SAFETY: `counter` is only touched while holding `section`; everything
// else is atomic or owned by the system.
unsafe impl Sync for Shared {}

static SHARED: Shared = Shared {
    section: UnsafeCell::new(CriticalSection([0; 40])),
    counter: UnsafeCell::new(0),
    tls_index: AtomicU32::new(0),
    tls_intact: AtomicU32::new(0),
    thread_ids: [const { AtomicU32::new(0) }; THREADS],
};

/// Worker: counts under the critical section (entering it twice, as the
/// owner may), and checks that its thread-local value and last-error code
/// are its own.
unsafe extern "system" fn worker(parameter: *mut c_void) -> u32 {
    let number = parameter as usize;
    let index = SHARED.tls_index.load(Ordering::Relaxed);
    let mine = (0x1000 + number) as *mut c_void;
    TlsSetValue(index, mine);
    SetLastError(100 + number as u32);
    SHARED.thread_ids[number].store(GetCurrentThreadId(), Ordering::Relaxed);

    for round in 0..ROUNDS {
        // SAFETY: the section was initialised before the threads started;
        // the counter is only accessed between Enter and Leave.
        unsafe {
            EnterCriticalSection(SHARED.section.get());
            EnterCriticalSection(SHARED.section.get());
            let value = *SHARED.counter.get();
            if round % 256 == number as u32 {
                // Give the others a chance to interfere.
                Sleep(0);
            }
            *SHARED.counter.get() = value + 1;
            LeaveCriticalSection(SHARED.section.get());
            LeaveCriticalSection(SHARED.section.get());
        }
    }
    if TlsGetValue(index) == mine && {
        // TlsGetValue reset the error code on success; set and check the
        // thread's own code once more.
        SetLastError(100 + number as u32);
        Sleep(5);
        GetLastError() == 100 + number as u32
    } {
        SHARED.tls_intact.fetch_add(1, Ordering::Relaxed);
    }
    if number == 0 {
        // From inside a call, without returning from the start routine.
        ExitThread(77);
    }
    10 + number as u32
}

fn workers(checks: &mut Checks) {
    let index = TlsAlloc();
    checks.check(index != u32::MAX, "TlsAlloc");
    SHARED.tls_index.store(index, Ordering::Relaxed);
    let main_value = 0xABCD as *mut c_void;
    checks.check(TlsSetValue(index, main_value) != 0, "TlsSetValue");
    // SAFETY: the section memory is static and not yet in use.
    unsafe { InitializeCriticalSection(SHARED.section.get()) };

    let mut handles = [null_mut(); THREADS];
    for (number, handle) in handles.iter_mut().enumerate() {
        let mut id = 0u32;
        // SAFETY: `worker` has the start routine signature and takes its
        // number as the parameter.
        *handle =
            unsafe { CreateThread(null_mut(), 0, worker, number as *mut c_void, 0, &raw mut id) };
        checks.check(!handle.is_null() && id != 0, "CreateThread");
    }
    for (number, &handle) in handles.iter().enumerate() {
        checks.check(
            WaitForSingleObject(handle, INFINITE) == WAIT_OBJECT_0,
            "thread ended",
        );
        let mut code = 0u32;
        // SAFETY: `code` is a live variable.
        let ok = unsafe { GetExitCodeThread(handle, &raw mut code) };
        let expected = if number == 0 { 77 } else { 10 + number as u32 };
        checks.check(ok != 0 && code == expected, "thread exit code");
        // Waiting again for a thread that has ended returns at once.
        checks.check(
            WaitForSingleObject(handle, 0) == WAIT_OBJECT_0,
            "wait after end",
        );
        checks.check(CloseHandle(handle) != 0, "close thread handle");
    }
    // SAFETY: all workers have ended; nobody else uses the counter.
    let total = unsafe { *SHARED.counter.get() };
    checks.check(
        total == THREADS as u32 * ROUNDS,
        "no update lost in the critical section",
    );
    checks.check(
        SHARED.tls_intact.load(Ordering::Relaxed) == THREADS as u32,
        "thread-local values and error codes are per thread",
    );
    checks.check(
        TlsGetValue(index) == main_value,
        "main thread's value untouched",
    );
    let mut ids = [GetCurrentThreadId(); THREADS + 1];
    for (slot, id) in ids.iter_mut().zip(&SHARED.thread_ids) {
        *slot = id.load(Ordering::Relaxed);
    }
    let distinct = (0..ids.len()).all(|a| (0..a).all(|b| ids[a] != ids[b]));
    checks.check(
        distinct && ids.iter().all(|&id| id != 0),
        "thread ids are distinct",
    );

    // A freed index comes back clean.
    checks.check(TlsFree(index) != 0, "TlsFree");
    checks.check(
        TlsFree(index) == 0 && GetLastError() == 87,
        "double TlsFree",
    );
    let again = TlsAlloc();
    checks.check(
        TlsGetValue(again).is_null(),
        "a reused index starts at zero",
    );
    TlsFree(again);

    // SAFETY: the section is initialised and free.
    unsafe {
        checks.check(
            TryEnterCriticalSection(SHARED.section.get()) != 0,
            "TryEnter when free",
        );
        LeaveCriticalSection(SHARED.section.get());
        DeleteCriticalSection(SHARED.section.get());
    }
    say!("winthread: {} threads counted to {total}", THREADS);
}

/// For the event tests.
static EVENT: AtomicUsize = AtomicUsize::new(0);
static RELEASED: AtomicU32 = AtomicU32::new(0);

fn event_handle() -> Handle {
    EVENT.load(Ordering::Relaxed) as Handle
}

unsafe extern "system" fn waiter(_: *mut c_void) -> u32 {
    let outcome = WaitForSingleObject(event_handle(), 5000);
    if outcome == WAIT_OBJECT_0 {
        RELEASED.fetch_add(1, Ordering::Relaxed);
    }
    outcome
}

fn start(routine: unsafe extern "system" fn(*mut c_void) -> u32) -> Handle {
    // SAFETY: the routines used here ignore their parameter.
    unsafe { CreateThread(null_mut(), 0, routine, null_mut(), 0, null_mut()) }
}

fn events(checks: &mut Checks) {
    // An event that is not signalled makes a wait time out — after the
    // time asked for, not before.
    // SAFETY: null name and attributes.
    let auto = unsafe { CreateEventA(null_mut(), 0, 0, null_mut()) };
    checks.check(!auto.is_null(), "CreateEventA");
    let before = GetTickCount64();
    checks.check(
        WaitForSingleObject(auto, 60) == WAIT_TIMEOUT,
        "WAIT_TIMEOUT",
    );
    checks.check(GetTickCount64() - before >= 50, "the timeout lasted");

    // Auto-reset: each SetEvent releases exactly one of two waiters.
    EVENT.store(auto as usize, Ordering::Relaxed);
    let (first, second) = (start(waiter), start(waiter));
    Sleep(30);
    checks.check(RELEASED.load(Ordering::Relaxed) == 0, "waiters wait");
    SetEvent(auto);
    Sleep(30);
    checks.check(
        RELEASED.load(Ordering::Relaxed) == 1,
        "auto-reset releases one waiter",
    );
    SetEvent(auto);
    WaitForSingleObject(first, INFINITE);
    WaitForSingleObject(second, INFINITE);
    checks.check(
        RELEASED.load(Ordering::Relaxed) == 2,
        "second SetEvent releases the other",
    );
    checks.check(
        WaitForSingleObject(auto, 0) == WAIT_TIMEOUT,
        "auto-reset event was consumed",
    );
    CloseHandle(first);
    CloseHandle(second);
    CloseHandle(auto);

    // Manual-reset: one SetEvent releases everybody and it stays set.
    // SAFETY: null name and attributes.
    let manual = unsafe { CreateEventA(null_mut(), 1, 0, null_mut()) };
    EVENT.store(manual as usize, Ordering::Relaxed);
    RELEASED.store(0, Ordering::Relaxed);
    let (first, second) = (start(waiter), start(waiter));
    Sleep(30);
    SetEvent(manual);
    WaitForSingleObject(first, INFINITE);
    WaitForSingleObject(second, INFINITE);
    checks.check(
        RELEASED.load(Ordering::Relaxed) == 2,
        "manual-reset releases all waiters",
    );
    checks.check(
        WaitForSingleObject(manual, 0) == WAIT_OBJECT_0,
        "manual-reset stays set",
    );
    ResetEvent(manual);
    checks.check(WaitForSingleObject(manual, 0) == WAIT_TIMEOUT, "ResetEvent");
    CloseHandle(first);
    CloseHandle(second);
    CloseHandle(manual);

    // Waiting on something that is not waitable fails.
    checks.check(
        WaitForSingleObject(0x1234_0000usize as Handle, 0) == u32::MAX && GetLastError() == 6,
        "WAIT_FAILED for a bad handle",
    );
    // SAFETY: a name, which is the unsupported case.
    let named = unsafe { CreateEventA(null_mut(), 0, 0, c"Global\\name".as_ptr()) };
    checks.check(
        named.is_null() && GetLastError() == 50,
        "named events are refused",
    );
    say!("winthread: events ok");
}

unsafe extern "system" fn sleeper(_: *mut c_void) -> u32 {
    Sleep(INFINITE);
    0
}

/// Entry point.
#[unsafe(no_mangle)]
pub extern "C" fn mainCRTStartup() -> u32 {
    let mut checks = Checks::new("winthread");
    workers(&mut checks);
    events(&mut checks);

    // A thread that is still running when the first thread returns ends
    // with the process.
    let lingering = start(sleeper);
    let mut code = 0u32;
    // SAFETY: `code` is a live variable.
    let ok = unsafe { GetExitCodeThread(lingering, &raw mut code) };
    checks.check(ok != 0 && code == STILL_ACTIVE, "STILL_ACTIVE");
    checks.check(
        WaitForSingleObject(lingering, 20) == WAIT_TIMEOUT,
        "running thread",
    );

    say!("winthread: all {} checks passed", checks.count());
    0
}
