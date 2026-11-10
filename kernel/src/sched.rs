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

use crate::arch::uspace::{self, AddressSpace};
use crate::arch::{context, cpu, gdt, interrupts, syscall};
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
    /// The user address space, for threads that run a process.
    address_space: Option<AddressSpace>,
    /// The process this thread belongs to, if any.
    pid: Option<u64>,
    /// Set by [`kill`]; the thread exits the next time it is about to
    /// return to user mode.
    killed: bool,
    /// User-mode `GS` base (the thread pointer), 0 if unused.
    thread_pointer: u64,
    /// Saved x87/SSE state, for threads that run a process. Kernel code
    /// does not use those registers.
    fpu: Option<Box<cpu::FpuState>>,
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
    /// The process ID, for threads that run a user process.
    pub pid: Option<u64>,
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
            address_space: None,
            pid: None,
            killed: false,
            thread_pointer: 0,
            fpu: None,
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

/// Creates a kernel thread that runs `entry(argument)` and exits when it
/// returns.
pub fn spawn(name: &str, entry: fn(usize), argument: usize) -> Result<ThreadId, SpawnError> {
    spawn_with(name, entry, argument, None, None)
}

/// Creates the thread of a user process: like [`spawn`], but the thread
/// runs with `address_space` active and is labelled with `pid`.
pub fn spawn_process(
    name: &str,
    entry: fn(usize),
    argument: usize,
    address_space: AddressSpace,
    pid: u64,
) -> Result<ThreadId, SpawnError> {
    spawn_with(name, entry, argument, Some(address_space), Some(pid))
}

fn spawn_with(
    name: &str,
    entry: fn(usize),
    argument: usize,
    address_space: Option<AddressSpace>,
    pid: Option<u64>,
) -> Result<ThreadId, SpawnError> {
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
            address_space,
            fpu: pid.map(|_| Box::new(cpu::FpuState::new())),
            pid,
            killed: false,
            thread_pointer: 0,
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
            let next_thread = scheduler.threads.get(&next)?;
            let load = next_thread.rsp;
            let root = next_thread
                .address_space
                .as_ref()
                .map_or_else(uspace::kernel_root, AddressSpace::root);
            let kernel_stack = next_thread
                .stack
                .as_ref()
                .map_or(omit_bootproto::KERNEL_STACK_TOP, KernelStack::top);
            let thread_pointer = next_thread.thread_pointer;
            let load_fpu = next_thread
                .fpu
                .as_deref()
                .map_or(core::ptr::null(), core::ptr::from_ref);
            // The thread lives in a `Box`, so this pointer stays valid
            // after the lock is released, until the thread is removed —
            // which cannot happen before it has been switched away from.
            let current_thread = scheduler.threads.get_mut(&current)?;
            let save_fpu = current_thread
                .fpu
                .as_deref_mut()
                .map_or(core::ptr::null_mut(), core::ptr::from_mut);
            let save = &raw mut current_thread.rsp;
            Some((
                save,
                load,
                root,
                kernel_stack,
                thread_pointer,
                save_fpu,
                load_fpu,
            ))
        })
    };
    // Safe to release now: a dead thread was switched away from when it
    // died, so neither its stack nor its page table is in use.
    drop(finished);

    if let Some((save, load, root, kernel_stack, thread_pointer, save_fpu, load_fpu)) = switch {
        // SAFETY: interrupts are disabled. `kernel_stack` is the top of the
        // next thread's own kernel stack. `root` is either the kernel's
        // page table or the next thread's address space, which lives as
        // long as the thread does; both map the kernel identically, so the
        // code and stack in use right now stay valid across the switch.
        unsafe {
            gdt::set_kernel_stack(kernel_stack);
            syscall::set_kernel_stack(kernel_stack);
            if cpu::read_cr3() & !0xFFF != root {
                cpu::write_cr3(root);
            }
            // The kernel does not use GS itself, so the register simply
            // carries the running thread's value. Writing the MSR is
            // skipped when the value does not change (the common case:
            // both threads leave it at zero).
            if ACTIVE_THREAD_POINTER.swap(thread_pointer, Ordering::Relaxed) != thread_pointer {
                cpu::write_msr(MSR_GS_BASE, thread_pointer);
            }
            // The x87/SSE registers belong to user code only, so they are
            // saved for a process thread being left and loaded for one
            // being entered; kernel threads in between leave them alone.
            // Both areas live in their thread's `Box` (see `save`).
            if !save_fpu.is_null() {
                cpu::FpuState::save(save_fpu);
            }
            if !load_fpu.is_null() {
                cpu::FpuState::restore(load_fpu);
            }
        }
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
                    pid: thread.pid,
                })
                .collect()
        })
}

/// Base address of the `GS` segment.
const MSR_GS_BASE: u32 = 0xC000_0101;
/// The value currently loaded in the `GS` base register.
static ACTIVE_THREAD_POINTER: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Sets the user-mode `GS` base of the calling thread. `address` must be
/// zero or a user-space address.
pub fn set_thread_pointer(address: u64) {
    let interrupts_were_enabled = interrupts::save_and_disable();
    if let Some(scheduler) = SCHEDULER.lock().as_mut() {
        let current = scheduler.current;
        if let Some(thread) = scheduler.threads.get_mut(&current) {
            thread.thread_pointer = address;
        }
    }
    ACTIVE_THREAD_POINTER.store(address, Ordering::Relaxed);
    // SAFETY: GS base exists on every x86-64 CPU; the caller guarantees
    // the address is canonical (zero or in the user half), so the write
    // cannot fault. The kernel never addresses memory through GS.
    unsafe { cpu::write_msr(MSR_GS_BASE, address) };
    interrupts::restore(interrupts_were_enabled);
}

/// The process ID of the calling thread, if it runs a user process.
pub fn current_pid() -> Option<u64> {
    let guard = SCHEDULER.lock();
    let scheduler = guard.as_ref()?;
    scheduler.threads.get(&scheduler.current)?.pid
}

/// Runs `f` on the calling thread's user address space. Returns `None` for
/// kernel threads.
///
/// `f` runs under the scheduler lock with interrupts disabled, so it must
/// be short and must not block.
pub fn with_address_space<R>(f: impl FnOnce(&mut AddressSpace) -> R) -> Option<R> {
    let mut guard = SCHEDULER.lock();
    let scheduler = guard.as_mut()?;
    let current = scheduler.current;
    scheduler
        .threads
        .get_mut(&current)?
        .address_space
        .as_mut()
        .map(f)
}

/// Asks the process `pid` to terminate. It exits the next time it is about
/// to return to user mode; if it is blocked, it is woken first. Returns
/// `false` if there is no such process.
pub fn kill(pid: u64) -> bool {
    let mut guard = SCHEDULER.lock();
    let Some(scheduler) = guard.as_mut() else {
        return false;
    };
    let Some((&id, thread)) = scheduler
        .threads
        .iter_mut()
        .find(|(_, thread)| thread.pid == Some(pid) && thread.state != ThreadState::Dead)
    else {
        return false;
    };
    thread.killed = true;
    if thread.state == ThreadState::Blocked {
        thread.state = ThreadState::Ready;
        thread.wake_tick = None;
        scheduler.ready.push_back(id);
    }
    true
}

/// Whether the calling thread has been asked to terminate.
pub fn current_killed() -> bool {
    let guard = SCHEDULER.lock();
    guard
        .as_ref()
        .and_then(|scheduler| scheduler.threads.get(&scheduler.current))
        .is_some_and(|thread| thread.killed)
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

    /// Like [`WaitQueue::wait_until`], but gives up after `ticks` timer
    /// ticks. Returns whether the condition became true.
    pub fn wait_until_or_timeout(&self, mut condition: impl FnMut() -> bool, ticks: u64) -> bool {
        let interrupts_were_enabled = interrupts::save_and_disable();
        let deadline = time::ticks() + ticks;
        let satisfied = loop {
            if condition() {
                break true;
            }
            if time::ticks() >= deadline {
                break false;
            }
            let blocked = {
                let mut guard = SCHEDULER.lock();
                guard.as_mut().and_then(|scheduler| {
                    let current = scheduler.current;
                    let thread = scheduler.threads.get_mut(&current)?;
                    thread.state = ThreadState::Blocked;
                    // The timer wakes the thread at the deadline if nothing
                    // else does first.
                    thread.wake_tick = Some(deadline);
                    Some(current)
                })
            };
            match blocked {
                Some(current) => {
                    self.waiters.lock().push_back(current);
                    reschedule();
                    // If the timer (rather than `wake_all`) ended the wait,
                    // the thread is still listed; a later `wake_all` must
                    // not find it there and cut short some unrelated sleep.
                    self.waiters.lock().retain(|&id| id != current);
                }
                None => {
                    interrupts::wait();
                    interrupts::disable();
                }
            }
        };
        interrupts::restore(interrupts_were_enabled);
        satisfied
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
            if round.is_multiple_of(10) && index.is_multiple_of(2) {
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

    // Waiting with a timeout: returns at once when the condition holds,
    // gives up after the timeout when it does not, is ended early by a
    // wake-up, and leaves nothing behind on the queue.
    static TIMED: WaitQueue = WaitQueue::new();
    static SIGNAL: AtomicBool = AtomicBool::new(false);
    if !TIMED.wait_until_or_timeout(|| true, 50) {
        return Err("timed wait missed a condition that already held");
    }
    let start = time::ticks();
    if TIMED.wait_until_or_timeout(|| false, 3) {
        return Err("timed wait reported a condition that never held");
    }
    let waited = time::ticks() - start;
    if !(3..=5).contains(&waited) {
        return Err("timed wait did not last as long as its timeout");
    }
    if !TIMED.waiters.lock().is_empty() {
        return Err("timed wait left the thread on the queue");
    }
    fn signaller(_: usize) {
        sleep_ticks(2);
        SIGNAL.store(true, Ordering::Relaxed);
        TIMED.wake_all();
    }
    let helper = spawn("timed-wait-test", signaller, 0).map_err(|_| "cannot spawn a thread")?;
    let start = time::ticks();
    let woken = TIMED.wait_until_or_timeout(|| SIGNAL.load(Ordering::Relaxed), 200);
    if !woken || time::ticks() - start > 50 {
        return Err("timed wait was not ended by a wake-up");
    }
    join(helper);
    Ok(())
}
