// SPDX-License-Identifier: GPL-2.0-only
//! Kernel threads and the scheduler.
//!
//! A preemptive round-robin scheduler for a single CPU. Every thread gets
//! one timer tick (10 ms) before the next ready thread runs. Threads can
//! also yield, sleep, wait on a [`WaitQueue`] and exit.
//!
//! This is deliberately the simplest scheduler that is correct. Priorities,
//! latency classes, SMP and topology awareness are future work that will
//! build on the same thread and wait-queue interfaces; nothing here should
//! be read as a performance claim.
//!
//! # Locking
//!
//! All scheduler state is behind one interrupt-disabling lock. A context
//! switch is prepared under the lock, the lock is released, and the switch
//! happens with interrupts still disabled, so on a single CPU nothing can
//! intervene.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::{context, interrupts};
use crate::mm::kstack::KernelStack;
use crate::sync::SpinLock;
use crate::time;

/// Identifies a thread. Never reused.
pub type ThreadId = u64;

/// What a thread is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    /// Currently executing.
    Running,
    /// Waiting for its turn on the CPU.
    Ready,
    /// Waiting for an event or a timeout.
    Blocked,
    /// Finished; its resources are released at the next opportunity.
    Dead,
}

impl ThreadState {
    /// Short human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Ready => "ready",
            Self::Blocked => "blocked",
            Self::Dead => "dead",
        }
    }
}

struct Thread {
    name: String,
    state: ThreadState,
    /// Saved stack pointer while the thread is not running.
    rsp: u64,
    /// `None` for the boot thread, which runs on the boot stack.
    stack: Option<KernelStack>,
    /// Tick at which a sleeping thread becomes ready.
    wake_tick: Option<u64>,
    /// Timer ticks during which this thread was the one running.
    cpu_ticks: u64,
}

/// A snapshot of one thread, for `ps`.
#[derive(Debug, Clone)]
pub struct ThreadInfo {
    /// The thread's ID.
    pub id: ThreadId,
    /// Its name.
    pub name: String,
    /// Its state at the time of the snapshot.
    pub state: ThreadState,
    /// Timer ticks of CPU time consumed.
    pub cpu_ticks: u64,
}

struct Scheduler {
    threads: BTreeMap<ThreadId, Box<Thread>>,
    ready: VecDeque<ThreadId>,
    current: ThreadId,
    idle: ThreadId,
    next_id: ThreadId,
}

static SCHEDULER: SpinLock<Option<Scheduler>> = SpinLock::new(None);
/// Set by the timer tick; consumed on the way out of the interrupt.
static NEED_RESCHEDULE: AtomicBool = AtomicBool::new(false);

/// Why a thread could not be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnError {
    /// [`init`] has not run.
    NotInitialised,
    /// No memory for the thread's stack.
    OutOfMemory,
}

/// Starts the scheduler: the calling context becomes thread 1 and an idle
/// thread is created.
pub fn init() -> Result<(), SpawnError> {
    let mut threads = BTreeMap::new();
    threads.insert(
        1,
        Box::new(Thread {
            name: "kernel".into(),
            state: ThreadState::Running,
            rsp: 0,
            stack: None,
            wake_tick: None,
            cpu_ticks: 0,
        }),
    );
    *SCHEDULER.lock() = Some(Scheduler {
        threads,
        ready: VecDeque::new(),
        current: 1,
        idle: 0,
        next_id: 2,
    });

    let idle = spawn("idle", idle_loop, 0)?;
    let mut guard = SCHEDULER.lock();
    let scheduler = guard.as_mut().ok_or(SpawnError::NotInitialised)?;
    // The idle thread is never on the ready queue; it runs only when the
    // queue is empty.
    scheduler.ready.retain(|&id| id != idle);
    scheduler.idle = idle;
    Ok(())
}

fn idle_loop(_: usize) {
    loop {
        interrupts::wait();
    }
}

/// Creates a thread that runs `entry(argument)` and exits when it returns.
pub fn spawn(name: &str, entry: fn(usize), argument: usize) -> Result<ThreadId, SpawnError> {
    let stack = KernelStack::new().ok_or(SpawnError::OutOfMemory)?;
    // SAFETY: `stack.top()` is the aligned end of a freshly mapped 64 KiB
    // stack that only this new thread will use.
    let rsp = unsafe { context::initial_stack(stack.top(), entry as usize, argument) };

    let mut guard = SCHEDULER.lock();
    let scheduler = guard.as_mut().ok_or(SpawnError::NotInitialised)?;
    let id = scheduler.next_id;
    scheduler.next_id += 1;
    scheduler.threads.insert(
        id,
        Box::new(Thread {
            name: name.into(),
            state: ThreadState::Ready,
            rsp,
            stack: Some(stack),
            wake_tick: None,
            cpu_ticks: 0,
        }),
    );
    scheduler.ready.push_back(id);
    Ok(id)
}

/// First Rust code of every new thread; called by the assembly trampoline.
pub extern "C" fn thread_start(entry: usize, argument: usize) -> ! {
    // A new thread is first switched to with interrupts disabled.
    interrupts::enable();
    // SAFETY: `entry` is the `fn(usize)` that `spawn` stored on the initial
    // stack; function pointers round-trip through `usize` unchanged.
    let entry = unsafe { core::mem::transmute::<usize, fn(usize)>(entry) };
    entry(argument);
    exit();
}

/// Picks the next thread and switches to it. The caller has already put
/// the current thread into the state it should be in while not running.
fn reschedule() {
    let interrupts_were_enabled = interrupts::save_and_disable();

    // Stacks of dead threads are collected here and dropped after the
    // scheduler lock is released (dropping one takes other locks).
    let mut finished: Vec<Box<Thread>> = Vec::new();
    let switch = {
        let mut guard = SCHEDULER.lock();
        guard.as_mut().and_then(|scheduler| {
            let current = scheduler.current;
            let dead: Vec<ThreadId> = scheduler
                .threads
                .iter()
                .filter(|(id, thread)| thread.state == ThreadState::Dead && **id != current)
                .map(|(id, _)| *id)
                .collect();
            for id in dead {
                finished.extend(scheduler.threads.remove(&id));
            }

            let current_state = scheduler.threads.get(&current)?.state;
            if current_state == ThreadState::Running {
                scheduler.threads.get_mut(&current)?.state = ThreadState::Ready;
                if current != scheduler.idle {
                    scheduler.ready.push_back(current);
                }
            }
            let next = scheduler.ready.pop_front().unwrap_or(scheduler.idle);
            scheduler.threads.get_mut(&next)?.state = ThreadState::Running;
            if next == current {
                return None;
            }
            scheduler.current = next;
            let load = scheduler.threads.get(&next)?.rsp;
            // The thread lives in a `Box`, so this pointer stays valid
            // after the lock is released, until the thread is removed —
            // which cannot happen before it has been switched away from.
            let save = &raw mut scheduler.threads.get_mut(&current)?.rsp;
            Some((save, load))
        })
    };
    drop(finished);

    if let Some((save, load)) = switch {
        // SAFETY: `save` points into the current thread's control block
        // (see above). `load` was saved by an earlier switch away from the
        // next thread or built by `initial_stack`; that thread's stack is
        // mapped and it is not running. Interrupts are disabled.
        unsafe { context::switch_context(save, load) };
    }
    interrupts::restore(interrupts_were_enabled);
}

/// Lets other ready threads run; returns when this thread is scheduled
/// again.
pub fn yield_now() {
    reschedule();
}

/// Called from the timer interrupt handler on every tick.
pub fn on_tick() {
    let now = time::ticks();
    if let Some(scheduler) = SCHEDULER.lock().as_mut() {
        let current = scheduler.current;
        let mut woken = Vec::new();
        for (&id, thread) in &mut scheduler.threads {
            if id == current {
                thread.cpu_ticks += 1;
            }
            if thread.state == ThreadState::Blocked
                && thread.wake_tick.is_some_and(|tick| tick <= now)
            {
                thread.wake_tick = None;
                thread.state = ThreadState::Ready;
                woken.push(id);
            }
        }
        scheduler.ready.extend(woken);
    }
    NEED_RESCHEDULE.store(true, Ordering::Relaxed);
}

/// Called at the end of interrupt handling, after the interrupt has been
/// acknowledged: preempts the interrupted thread if a reschedule is due.
pub fn preempt_if_needed() {
    if NEED_RESCHEDULE.swap(false, Ordering::Relaxed) {
        reschedule();
    }
}

/// Blocks the calling thread for at least `ticks` timer ticks.
pub fn sleep_ticks(ticks: u64) {
    let interrupts_were_enabled = interrupts::save_and_disable();
    if let Some(scheduler) = SCHEDULER.lock().as_mut() {
        let current = scheduler.current;
        if let Some(thread) = scheduler.threads.get_mut(&current) {
            thread.state = ThreadState::Blocked;
            // +1: the current tick is already partly over.
            thread.wake_tick = Some(time::ticks() + ticks + 1);
        }
    }
    reschedule();
    interrupts::restore(interrupts_were_enabled);
}

/// Ends the calling thread. Its stack is released once another thread is
/// running.
pub fn exit() -> ! {
    interrupts::disable();
    if let Some(scheduler) = SCHEDULER.lock().as_mut() {
        let current = scheduler.current;
        if let Some(thread) = scheduler.threads.get_mut(&current) {
            thread.state = ThreadState::Dead;
        }
    }
    EXITED.wake_all();
    reschedule();
    unreachable!("a dead thread was scheduled");
}

/// Woken whenever a thread exits; [`join`] waits on it.
static EXITED: WaitQueue = WaitQueue::new();

/// Whether thread `id` exists and has not finished.
pub fn is_alive(id: ThreadId) -> bool {
    SCHEDULER
        .lock()
        .as_ref()
        .and_then(|scheduler| scheduler.threads.get(&id))
        .is_some_and(|thread| thread.state != ThreadState::Dead)
}

/// Blocks until thread `id` has exited (returns at once if it already has
/// or never existed).
pub fn join(id: ThreadId) {
    EXITED.wait_until(|| !is_alive(id));
}

/// The ID of the calling thread (0 before [`init`]).
pub fn current() -> ThreadId {
    SCHEDULER.lock().as_ref().map_or(0, |s| s.current)
}

/// A snapshot of all threads, ordered by ID.
pub fn threads() -> Vec<ThreadInfo> {
    SCHEDULER
        .lock()
        .as_ref()
        .map_or_else(Vec::new, |scheduler| {
            scheduler
                .threads
                .iter()
                .map(|(&id, thread)| ThreadInfo {
                    id,
                    name: thread.name.clone(),
                    state: thread.state,
                    cpu_ticks: thread.cpu_ticks,
                })
                .collect()
        })
}

/// A set of threads waiting for an event.
pub struct WaitQueue {
    waiters: SpinLock<VecDeque<ThreadId>>,
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl WaitQueue {
    /// Creates an empty queue.
    pub const fn new() -> Self {
        Self {
            waiters: SpinLock::new(VecDeque::new()),
        }
    }

    /// Blocks the calling thread until `condition` returns true.
    ///
    /// `condition` is evaluated with interrupts disabled, and the thread is
    /// queued before it stops running, so a wake-up that happens between
    /// the check and the block cannot be lost. Whoever makes the condition
    /// true must call [`WaitQueue::wake_all`] afterwards.
    pub fn wait_until(&self, mut condition: impl FnMut() -> bool) {
        let interrupts_were_enabled = interrupts::save_and_disable();
        while !condition() {
            let blocked = {
                let mut guard = SCHEDULER.lock();
                guard.as_mut().and_then(|scheduler| {
                    let current = scheduler.current;
                    scheduler.threads.get_mut(&current)?.state = ThreadState::Blocked;
                    Some(current)
                })
            };
            match blocked {
                Some(current) => {
                    self.waiters.lock().push_back(current);
                    reschedule();
                }
                // No scheduler yet: there is only one context, so just wait
                // for the next interrupt and look again.
                None => {
                    interrupts::wait();
                    interrupts::disable();
                }
            }
        }
        interrupts::restore(interrupts_were_enabled);
    }

    /// Makes every waiting thread ready. Safe to call from interrupt
    /// handlers.
    pub fn wake_all(&self) {
        let waiters: VecDeque<ThreadId> = core::mem::take(&mut *self.waiters.lock());
        if waiters.is_empty() {
            return;
        }
        if let Some(scheduler) = SCHEDULER.lock().as_mut() {
            for id in waiters {
                if let Some(thread) = scheduler.threads.get_mut(&id)
                    && thread.state == ThreadState::Blocked
                {
                    thread.state = ThreadState::Ready;
                    thread.wake_tick = None;
                    scheduler.ready.push_back(id);
                }
            }
        }
        NEED_RESCHEDULE.store(true, Ordering::Relaxed);
    }
}

/// Exercises the scheduler: several threads share the CPU by yielding and
/// sleeping, one of them never yields and must be preempted, and all are
/// joined.
pub fn self_test() -> Result<(), &'static str> {
    use core::sync::atomic::AtomicU64;

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    static SPINNER_RELEASED: AtomicBool = AtomicBool::new(false);
    static SPINNER_DONE: AtomicBool = AtomicBool::new(false);
    const WORKERS: u64 = 4;
    // Each pass over the ready queue costs one full time slice (the
    // spinner's), so this bounds the test to well under a second.
    const ROUNDS: u64 = 40;

    fn worker(index: usize) {
        for round in 0..ROUNDS {
            COUNTER.fetch_add(1, Ordering::Relaxed);
            if round % 10 == 0 && index % 2 == 0 {
                sleep_ticks(1);
            } else {
                yield_now();
            }
        }
    }

    // Never yields: it only finishes if the scheduler preempts it so the
    // main thread can set the flag.
    fn spinner(_: usize) {
        while !SPINNER_RELEASED.load(Ordering::Relaxed) {
            core::hint::spin_loop();
        }
        SPINNER_DONE.store(true, Ordering::Relaxed);
    }

    COUNTER.store(0, Ordering::Relaxed);
    SPINNER_RELEASED.store(false, Ordering::Relaxed);
    SPINNER_DONE.store(false, Ordering::Relaxed);
    let before = threads().len();

    let spinner_id = spawn("test-spinner", spinner, 0).map_err(|_| "cannot spawn spinner")?;
    let mut workers = Vec::new();
    for index in 0..WORKERS {
        workers
            .push(spawn("test-worker", worker, index as usize).map_err(|_| "cannot spawn worker")?);
    }
    for id in workers {
        join(id);
    }
    if COUNTER.load(Ordering::Relaxed) != WORKERS * ROUNDS {
        return Err("workers did not complete all their rounds");
    }

    // The workers are done, yet the spinner has been hogging its time
    // slices all along; being here at all means it was preempted.
    if SPINNER_DONE.load(Ordering::Relaxed) {
        return Err("spinner finished before it was released");
    }
    SPINNER_RELEASED.store(true, Ordering::Relaxed);
    join(spinner_id);
    if !SPINNER_DONE.load(Ordering::Relaxed) {
        return Err("spinner did not run to completion");
    }

    let start = time::ticks();
    sleep_ticks(3);
    if time::ticks() - start < 3 {
        return Err("sleep returned early");
    }
    // The sleep also gave the scheduler a chance to reap the dead threads.
    yield_now();
    if threads().len() != before {
        return Err("finished threads were not cleaned up");
    }
    Ok(())
}
