// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! System Monitor: what is running, and how busy the machine is.
//!
//! Two pages. *Processes* lists every process with the share of the
//! processor it used during the last second and the time it has used
//! in all; one of them can be selected and ended. *Performance* draws
//! the last minute of processor and memory use.
//!
//! The window does not know where its figures come from: it asks a
//! [`Source`] once a second, which the desktop program implements with
//! system calls and the tests with made-up numbers.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::desktop::{App, Context};
use crate::font::{MEDIUM_13, REGULAR_12, REGULAR_13, SEMIBOLD_20};
use crate::geometry::{Point, Rect};
use crate::surface::Canvas;
use crate::theme::Theme;
use crate::widgets;

/// The size the window asks for.
pub const SIZE: (i32, i32) = (600, 420);

/// Milliseconds between two looks at the system.
pub const INTERVAL_MS: u64 = 1000;
/// How many looks the graphs remember: a minute.
pub const HISTORY: usize = 60;

const PAGES: [&str; 2] = ["Processes", "Performance"];
const MARGIN: i32 = 16;
const TOP: i32 = 56;
const HEADER: i32 = 26;
const ROW: i32 = 28;
const STATUS: i32 = 30;
const END_BUTTON: (i32, i32) = (92, 30);

/// What a thread is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    /// Finished, not yet cleared away.
    Ended,
    /// Waiting for something to happen.
    Waiting,
    /// Waiting for its turn on the processor.
    Ready,
    /// On the processor right now.
    Running,
}

impl State {
    fn name(self) -> &'static str {
        match self {
            Self::Ended => "Ended",
            Self::Waiting => "Waiting",
            Self::Ready => "Ready",
            Self::Running => "Running",
        }
    }
}

/// One thread, as the [`Source`] reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// The thread's ID.
    pub thread: u64,
    /// Its process; `None` for a thread of the kernel.
    pub pid: Option<u64>,
    /// Its name (for a process, the program).
    pub name: String,
    /// What it is doing.
    pub state: State,
    /// Milliseconds of processor time it has used.
    pub cpu_ms: u64,
    /// It is the thread that runs when nothing else can.
    pub idle: bool,
}

/// The system at one moment.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sample {
    /// Bytes of memory in the machine.
    pub memory_total: u64,
    /// Bytes of it not in use.
    pub memory_free: u64,
    /// Milliseconds since the system started.
    pub uptime_ms: u64,
    /// Every thread.
    pub tasks: Vec<Task>,
}

/// Where the figures come from.
pub trait Source {
    /// Milliseconds since the system started. Asked often.
    fn now_ms(&mut self) -> u64;
    /// The system now. Asked once per [`INTERVAL_MS`].
    fn sample(&mut self) -> Sample;
    /// Ends process `pid`; whether there was one to end.
    fn end(&mut self, pid: u64) -> bool;
}

/// One line of the list: a process, or the kernel's threads together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The process; `None` for the line that stands for the kernel.
    pub pid: Option<u64>,
    /// What it is called.
    pub name: String,
    /// How many threads it has.
    pub threads: usize,
    /// The most active state among them.
    pub state: State,
    /// Tenths of a percent of the processor used during the last
    /// interval.
    pub share: u32,
    /// Milliseconds of processor time used in all.
    pub cpu_ms: u64,
}

/// The name shown for the kernel's own threads.
pub const KERNEL_NAME: &str = "System";

/// The System Monitor window.
pub struct Monitor<S: Source> {
    source: S,
    page: usize,
    rows: Vec<Row>,
    /// The selected process.
    selected: Option<u64>,
    /// The first row shown.
    scroll: usize,
    /// Processor time of each line at the last look (`None`: the
    /// kernel), to tell how much was used since.
    before: BTreeMap<Option<u64>, u64>,
    idle_before: u64,
    /// When the last look was taken, if one was.
    looked: Option<u64>,
    latest: Sample,
    threads: usize,
    /// Tenths of a percent of the processor in use, newest last.
    processor: VecDeque<u32>,
    /// Tenths of a percent of memory in use, newest last.
    memory: VecDeque<u32>,
    size: (i32, i32),
}

/// `part` of `whole` in tenths of a percent, at most 1000.
fn per_mille(part: u64, whole: u64) -> u32 {
    part.saturating_mul(1000)
        .checked_div(whole)
        .map_or(0, |share| share.min(1000) as u32)
}

/// Tenths of a percent as text: `7.5%`, `100%`.
fn percent(share: u32) -> String {
    if share >= 1000 {
        "100%".into()
    } else {
        format!("{}.{}%", share / 10, share % 10)
    }
}

/// Processor time as text: `0.42 s`, `12.3 s`, `4:05 min`, `2:03 h`.
pub fn readable_time(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0..10 => format!("{}.{:02} s", seconds, ms % 1000 / 10),
        10..60 => format!("{}.{} s", seconds, ms % 1000 / 100),
        60..3600 => format!("{}:{:02} min", seconds / 60, seconds % 60),
        _ => format!("{}:{:02} h", seconds / 3600, seconds / 60 % 60),
    }
}

/// An amount of memory as text: `512 KB`, `34.5 MB`, `3.9 GB` (of 1024).
pub fn readable_memory(bytes: u64) -> String {
    let scaled = |unit: u64, name: &str| {
        let tenths = bytes * 10 / unit;
        if tenths < 1000 {
            format!("{}.{} {name}", tenths / 10, tenths % 10)
        } else {
            format!("{} {name}", tenths / 10)
        }
    };
    match bytes {
        0..0x10_0000 => format!("{} KB", bytes / 1024),
        0x10_0000..0x4000_0000 => scaled(0x10_0000, "MB"),
        _ => scaled(0x4000_0000, "GB"),
    }
}

/// How long the system has been running: `0:00:42`, `3:12:05`.
fn readable_uptime(ms: u64) -> String {
    let seconds = ms / 1000;
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// The last part of a program's path.
fn short_name(name: &str) -> &str {
    name.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(name)
}

impl<S: Source> Monitor<S> {
    /// A monitor that reads from `source`; the first look is taken at
    /// once.
    pub fn new(source: S) -> Self {
        let mut monitor = Self {
            source,
            page: 0,
            rows: Vec::new(),
            selected: None,
            scroll: 0,
            before: BTreeMap::new(),
            idle_before: 0,
            looked: None,
            latest: Sample::default(),
            threads: 0,
            processor: VecDeque::new(),
            memory: VecDeque::new(),
            size: SIZE,
        };
        monitor.look();
        monitor
    }

    /// The lines of the list, as they are shown.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The selected process, if any.
    pub fn selected(&self) -> Option<u64> {
        self.selected
    }

    /// Which page is shown: 0 for Processes, 1 for Performance.
    pub fn page(&self) -> usize {
        self.page
    }

    /// The processor's use over the last looks, in tenths of a percent,
    /// newest last.
    pub fn processor_history(&self) -> impl Iterator<Item = u32> + '_ {
        self.processor.iter().copied()
    }

    /// Takes a look at the system and works out what changed since the
    /// last one.
    fn look(&mut self) {
        let sample = self.source.sample();
        let elapsed = self
            .looked
            .map(|then| sample.uptime_ms.saturating_sub(then));
        let mut totals: BTreeMap<Option<u64>, Row> = BTreeMap::new();
        let mut idle = 0;
        for task in &sample.tasks {
            if task.idle {
                idle = task.cpu_ms;
                continue;
            }
            let row = totals.entry(task.pid).or_insert_with(|| Row {
                pid: task.pid,
                name: match task.pid {
                    Some(_) => short_name(&task.name).into(),
                    None => KERNEL_NAME.into(),
                },
                threads: 0,
                state: State::Ended,
                share: 0,
                cpu_ms: 0,
            });
            row.threads += 1;
            row.state = row.state.max(task.state);
            row.cpu_ms += task.cpu_ms;
        }
        // The share of the last interval. (Nothing is known at the
        // first look, or about a process that was not there before.)
        for (key, row) in &mut totals {
            row.share = match (elapsed, self.before.get(key)) {
                (Some(elapsed), Some(&before)) => {
                    per_mille(row.cpu_ms.saturating_sub(before), elapsed)
                }
                _ => 0,
            };
        }
        if let Some(elapsed) = elapsed {
            let idle_share = per_mille(idle.saturating_sub(self.idle_before), elapsed);
            self.processor.push_back(1000 - idle_share);
            self.memory.push_back(per_mille(
                sample.memory_total.saturating_sub(sample.memory_free),
                sample.memory_total,
            ));
            for history in [&mut self.processor, &mut self.memory] {
                while history.len() > HISTORY {
                    history.pop_front();
                }
            }
        }
        self.before = totals.iter().map(|(&key, row)| (key, row.cpu_ms)).collect();
        self.idle_before = idle;
        self.looked = Some(sample.uptime_ms);
        self.threads = sample.tasks.len();
        // The kernel first, then the processes in the order they were
        // started: lines do not jump about from one look to the next.
        self.rows = totals.into_values().collect();
        if self
            .selected
            .is_some_and(|pid| !self.rows.iter().any(|row| row.pid == Some(pid)))
        {
            self.selected = None;
        }
        self.scroll = self.scroll.min(self.last_scroll());
        self.latest = sample;
    }

    fn visible_rows(&self) -> usize {
        ((self.size.1 - TOP - HEADER - STATUS) / ROW).max(1) as usize
    }

    fn last_scroll(&self) -> usize {
        self.rows.len().saturating_sub(self.visible_rows())
    }

    /// Where page `index`'s name is, to click on.
    pub fn page_frame(index: usize) -> Option<Rect> {
        widgets::segments(MARGIN, 12, &PAGES).get(index).copied()
    }

    /// Where the button that ends the selected process is, in a window
    /// `width` wide.
    pub fn end_button(width: i32) -> Rect {
        Rect::new(
            width - MARGIN - END_BUTTON.0,
            13,
            END_BUTTON.0,
            END_BUTTON.1,
        )
    }

    /// Where row `index` of the list is, if it is on the screen.
    pub fn row_frame(&self, index: usize) -> Option<Rect> {
        let shown = index.checked_sub(self.scroll)?;
        (index < self.rows.len() && shown < self.visible_rows()).then(|| {
            Rect::new(
                MARGIN - 8,
                TOP + HEADER + shown as i32 * ROW,
                self.size.0 - 2 * (MARGIN - 8),
                ROW,
            )
        })
    }

    /// Ends the selected process.
    fn end_selected(&mut self) {
        if let Some(pid) = self.selected {
            self.source.end(pid);
            // Show at once that it is gone (or going).
            self.look_again();
        }
    }

    /// A look outside the rhythm, which must not count as an interval:
    /// the shares are kept as they were.
    fn look_again(&mut self) {
        let sample = self.source.sample();
        let shares: BTreeMap<Option<u64>, u32> =
            self.rows.iter().map(|row| (row.pid, row.share)).collect();
        self.rows.retain(|row| match row.pid {
            Some(pid) => sample.tasks.iter().any(|task| task.pid == Some(pid)),
            None => true,
        });
        for row in &mut self.rows {
            row.share = shares.get(&row.pid).copied().unwrap_or(0);
        }
        if self
            .selected
            .is_some_and(|pid| !self.rows.iter().any(|row| row.pid == Some(pid)))
        {
            self.selected = None;
        }
        self.before.retain(|key, _| match key {
            Some(pid) => sample.tasks.iter().any(|task| task.pid == Some(*pid)),
            None => true,
        });
        self.scroll = self.scroll.min(self.last_scroll());
    }

    fn keep_selection_visible(&mut self) {
        let Some(at) = self
            .selected
            .and_then(|pid| self.rows.iter().position(|row| row.pid == Some(pid)))
        else {
            return;
        };
        let visible = self.visible_rows();
        if at < self.scroll {
            self.scroll = at;
        } else if at >= self.scroll + visible {
            self.scroll = at + 1 - visible;
        }
    }

    fn draw_processes(&self, canvas: &mut Canvas<'_>, width: i32, height: i32, theme: &Theme) {
        // Columns, from the right edge: time, share, state, threads, PID.
        let right = width - MARGIN;
        let (time, share, state, threads, pid) =
            (right, right - 90, right - 228, right - 250, right - 320);
        let name_width = (pid - 60 - MARGIN).max(40);
        let right_aligned = |canvas: &mut Canvas<'_>, x: i32, y: i32, text: &str, font, ink| {
            canvas.write(
                x - crate::font::Font::width(&font, text),
                y,
                text,
                font,
                ink,
            );
        };

        let y = TOP + (HEADER - REGULAR_12.height()) / 2;
        canvas.write(MARGIN, y, "Name", REGULAR_12, theme.text_3);
        right_aligned(canvas, pid, y, "PID", REGULAR_12, theme.text_3);
        right_aligned(canvas, threads, y, "Threads", REGULAR_12, theme.text_3);
        canvas.write(state, y, "State", REGULAR_12, theme.text_3);
        right_aligned(canvas, share, y, "Processor", REGULAR_12, theme.text_3);
        right_aligned(canvas, time, y, "Time", REGULAR_12, theme.text_3);
        canvas.tint(
            Rect::new(MARGIN, TOP + HEADER - 1, width - 2 * MARGIN, 1),
            theme.separator.colour,
            theme.separator.alpha,
        );

        for (index, row) in self.rows.iter().enumerate() {
            let Some(area) = self.row_frame(index) else {
                continue;
            };
            if row.pid.is_some() && row.pid == self.selected {
                canvas.round_rect(
                    area.inset(1),
                    7,
                    theme.accent_soft.colour,
                    theme.accent_soft.alpha,
                );
            }
            let y = area.y + (ROW - REGULAR_13.height()) / 2;
            let (name, cut) = REGULAR_13.fit(&row.name, name_width);
            let end = canvas.write(MARGIN, y, name, REGULAR_13, theme.text);
            if cut {
                canvas.write(end, y, "…", REGULAR_13, theme.text_3);
            }
            if let Some(pid_number) = row.pid {
                right_aligned(
                    canvas,
                    pid,
                    y,
                    &format!("{pid_number}"),
                    REGULAR_13,
                    theme.text_2,
                );
            }
            right_aligned(
                canvas,
                threads,
                y,
                &format!("{}", row.threads),
                REGULAR_13,
                theme.text_2,
            );
            let ink = if row.state == State::Running {
                theme.success
            } else {
                theme.text_2
            };
            canvas.write(state, y, row.state.name(), REGULAR_13, ink);
            right_aligned(
                canvas,
                share,
                y,
                &percent(row.share),
                REGULAR_13,
                theme.text,
            );
            right_aligned(
                canvas,
                time,
                y,
                &readable_time(row.cpu_ms),
                REGULAR_13,
                theme.text_2,
            );
        }

        // The button that ends the selected process.
        let button = Self::end_button(width);
        let enabled = self.selected.is_some();
        if enabled {
            canvas.round_rect(button, 8, theme.danger, 255);
        } else {
            canvas.round_rect(button, 8, theme.fill.colour, theme.fill.alpha);
        }
        let label = "End task";
        canvas.write(
            button.x + (button.w - MEDIUM_13.width(label)) / 2,
            button.y + (button.h - MEDIUM_13.height()) / 2,
            label,
            MEDIUM_13,
            if enabled { 0xFF_FF_FF } else { theme.text_3 },
        );

        // The status line.
        let processes = self.rows.iter().filter(|row| row.pid.is_some()).count();
        let status = format!(
            "{processes} {}, {} threads, running for {}",
            if processes == 1 {
                "process"
            } else {
                "processes"
            },
            self.threads,
            readable_uptime(self.latest.uptime_ms)
        );
        canvas.tint(
            Rect::new(0, height - STATUS, width, 1),
            theme.separator.colour,
            theme.separator.alpha,
        );
        canvas.write(
            MARGIN,
            height - STATUS + (STATUS - REGULAR_12.height()) / 2,
            &status,
            REGULAR_12,
            theme.text_2,
        );
    }

    fn draw_performance(&self, canvas: &mut Canvas<'_>, width: i32, height: i32, theme: &Theme) {
        let used = self
            .latest
            .memory_total
            .saturating_sub(self.latest.memory_free);
        let cards = [
            (
                "Processor",
                self.processor
                    .back()
                    .map_or("...".into(), |&now| percent(now)),
                format!(
                    "{} threads, running for {}",
                    self.threads,
                    readable_uptime(self.latest.uptime_ms)
                ),
                &self.processor,
                theme.accent,
            ),
            (
                "Memory",
                readable_memory(used),
                format!(
                    "of {} in use, {} free",
                    readable_memory(self.latest.memory_total),
                    readable_memory(self.latest.memory_free)
                ),
                &self.memory,
                theme.success,
            ),
        ];
        let gap = 12;
        let card_height = ((height - TOP - MARGIN - gap) / 2).max(96);
        for (index, (title, figure, detail, history, ink)) in cards.into_iter().enumerate() {
            let card = Rect::new(
                MARGIN,
                TOP + index as i32 * (card_height + gap),
                width - 2 * MARGIN,
                card_height,
            );
            widgets::card(canvas, card, theme);
            canvas.write(card.x + 14, card.y + 10, title, MEDIUM_13, theme.text_2);
            let end = canvas.write(card.x + 14, card.y + 30, &figure, SEMIBOLD_20, theme.text);
            canvas.write(
                end + 10,
                card.y + 30 + SEMIBOLD_20.height() - REGULAR_12.height() - 3,
                &detail,
                REGULAR_12,
                theme.text_3,
            );
            // The graph: a column per look, the newest at the right.
            let graph = Rect::new(card.x + 14, card.y + 64, card.w - 28, card.h - 64 - 12);
            if graph.h < 8 || graph.w < HISTORY as i32 {
                continue;
            }
            canvas.tint(
                Rect::new(graph.x, graph.bottom() - 1, graph.w, 1),
                theme.separator.colour,
                theme.separator.alpha,
            );
            let step = graph.w / HISTORY as i32;
            let first = HISTORY - history.len();
            for (at, &value) in history.iter().enumerate() {
                let tall = ((graph.h - 1) * value as i32 / 1000).max(1);
                let column = Rect::new(
                    graph.right() - (HISTORY - first - at) as i32 * step,
                    graph.bottom() - 1 - tall,
                    step,
                    tall,
                );
                canvas.tint(column, ink, 60);
                canvas.fill(Rect::new(column.x, column.y, column.w, 2.min(tall)), ink);
            }
        }
    }
}

impl<S: Source> App for Monitor<S> {
    fn title(&self) -> &str {
        "System Monitor"
    }

    fn icon(&self) -> &str {
        "mon"
    }

    fn draw(&mut self, canvas: &mut Canvas<'_>, width: i32, height: i32, theme: &Theme) {
        self.size = (width, height);
        self.scroll = self.scroll.min(self.last_scroll());
        widgets::segmented(canvas, MARGIN, 12, &PAGES, self.page, theme);
        if self.page == 0 {
            self.draw_processes(canvas, width, height, theme);
        } else {
            self.draw_performance(canvas, width, height, theme);
        }
    }

    fn click(&mut self, x: i32, y: i32, _: &mut Context) {
        let at = Point::new(x, y);
        if let Some(page) = widgets::segment_at(MARGIN, 12, &PAGES, at) {
            self.page = page;
            return;
        }
        if self.page != 0 {
            return;
        }
        if Self::end_button(self.size.0).contains(at) {
            self.end_selected();
            return;
        }
        let row = (0..self.rows.len())
            .find(|&index| self.row_frame(index).is_some_and(|area| area.contains(at)));
        // The kernel's line cannot be selected: there is nothing to
        // end. A click beside the rows selects nothing.
        self.selected = row.and_then(|index| self.rows[index].pid);
    }

    fn key(&mut self, code: i32, _: &mut Context) {
        const TAB: i32 = 9;
        const DELETE: i32 = 0x10A;
        const UP: i32 = 0x101;
        const DOWN: i32 = 0x102;
        match code {
            TAB => self.page = (self.page + 1) % PAGES.len(),
            UP | DOWN if self.page == 0 => {
                let processes: Vec<u64> = self.rows.iter().filter_map(|row| row.pid).collect();
                let at = self
                    .selected
                    .and_then(|pid| processes.iter().position(|&other| other == pid));
                let next = match (at, code) {
                    (None, _) => 0,
                    (Some(at), UP) => at.saturating_sub(1),
                    (Some(at), _) => (at + 1).min(processes.len().saturating_sub(1)),
                };
                self.selected = processes.get(next).copied();
                self.keep_selection_visible();
            }
            DELETE if self.page == 0 => self.end_selected(),
            _ => {}
        }
    }

    fn tick(&mut self, _: &mut Context) -> bool {
        let now = self.source.now_ms();
        if self
            .looked
            .is_some_and(|then| now.saturating_sub(then) < INTERVAL_MS)
        {
            return false;
        }
        self.look();
        true
    }
}

#[cfg(test)]
mod tests {
    use alloc::rc::Rc;
    use alloc::vec;
    use core::cell::RefCell;

    use super::*;
    use crate::surface::Surface;
    use crate::theme::{Accent, Mode};

    /// A made-up system the tests change between looks.
    #[derive(Default)]
    struct World {
        now: u64,
        tasks: Vec<Task>,
        ended: Vec<u64>,
        free: u64,
    }

    #[derive(Clone, Default)]
    struct Fake(Rc<RefCell<World>>);

    impl Source for Fake {
        fn now_ms(&mut self) -> u64 {
            self.0.borrow().now
        }

        fn sample(&mut self) -> Sample {
            let world = self.0.borrow();
            Sample {
                memory_total: 1000 << 20,
                memory_free: world.free,
                uptime_ms: world.now,
                tasks: world.tasks.clone(),
            }
        }

        fn end(&mut self, pid: u64) -> bool {
            let mut world = self.0.borrow_mut();
            world.ended.push(pid);
            let before = world.tasks.len();
            world.tasks.retain(|task| task.pid != Some(pid));
            world.tasks.len() != before
        }
    }

    fn task(thread: u64, pid: Option<u64>, name: &str, cpu_ms: u64) -> Task {
        Task {
            thread,
            pid,
            name: name.into(),
            state: State::Waiting,
            cpu_ms,
            idle: false,
        }
    }

    /// A system with an idle thread, two kernel threads, a shell and a
    /// desktop with two threads.
    fn world() -> Fake {
        let fake = Fake::default();
        {
            let mut world = fake.0.borrow_mut();
            world.free = 750 << 20;
            world.tasks = vec![
                Task {
                    idle: true,
                    ..task(1, None, "idle", 0)
                },
                task(2, None, "net", 0),
                task(3, None, "status", 0),
                task(4, Some(1), "/bin/sh", 0),
                Task {
                    state: State::Running,
                    ..task(5, Some(2), "/bin/desktop", 0)
                },
                task(6, Some(2), "/bin/desktop", 0),
            ];
        }
        fake
    }

    /// Lets `ms` pass, in which each thread uses the given time.
    fn pass(fake: &Fake, ms: u64, used: &[(u64, u64)]) {
        let mut world = fake.0.borrow_mut();
        world.now += ms;
        for task in &mut world.tasks {
            if let Some((_, time)) = used.iter().find(|(thread, _)| *thread == task.thread) {
                task.cpu_ms += time;
            }
        }
    }

    fn tick<S: Source>(monitor: &mut Monitor<S>) -> bool {
        let mut context = Context::default();
        monitor.tick(&mut context)
    }

    fn click<S: Source>(monitor: &mut Monitor<S>, area: Rect) {
        let mut context = Context::default();
        monitor.click(area.x + area.w / 2, area.y + area.h / 2, &mut context);
    }

    fn click_row<S: Source>(monitor: &mut Monitor<S>, index: usize) {
        let area = monitor.row_frame(index).expect("the row is shown");
        click(monitor, area);
    }

    #[test]
    fn threads_are_gathered_into_processes_with_the_kernel_first() {
        let monitor = Monitor::new(world());
        let rows = monitor.rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            (rows[0].pid, rows[0].name.as_str(), rows[0].threads),
            (None, KERNEL_NAME, 2)
        );
        assert_eq!(
            (rows[1].pid, rows[1].name.as_str(), rows[1].threads),
            (Some(1), "sh", 1)
        );
        assert_eq!(
            (rows[2].pid, rows[2].name.as_str(), rows[2].threads),
            (Some(2), "desktop", 2)
        );
        // The most active thread speaks for the process.
        assert_eq!(rows[2].state, State::Running);
        assert_eq!(rows[1].state, State::Waiting);
        // Nothing is known about shares at the first look.
        assert!(rows.iter().all(|row| row.share == 0));
    }

    #[test]
    fn shares_are_of_the_time_since_the_last_look() {
        let fake = world();
        let mut monitor = Monitor::new(fake.clone());
        // Too early: nothing happens.
        pass(&fake, 400, &[]);
        assert!(!tick(&mut monitor));
        // 1000 ms in all: the desktop's threads used 200 and 50 ms, the
        // network thread 10, and the processor idled for 740.
        pass(&fake, 600, &[(5, 200), (6, 50), (2, 10), (1, 740)]);
        assert!(tick(&mut monitor));
        let rows = monitor.rows();
        assert_eq!(rows[2].share, 250);
        assert_eq!(rows[2].cpu_ms, 250);
        assert_eq!(rows[0].share, 10);
        assert_eq!(rows[1].share, 0);
        assert_eq!(monitor.processor_history().collect::<Vec<_>>(), [260]);
        // The next interval is twice as long and quieter.
        pass(&fake, 2000, &[(5, 100), (1, 1900)]);
        assert!(tick(&mut monitor));
        assert_eq!(monitor.rows()[2].share, 50);
        assert_eq!(monitor.rows()[2].cpu_ms, 350);
        assert_eq!(monitor.processor_history().collect::<Vec<_>>(), [260, 50]);
    }

    #[test]
    fn a_new_process_has_no_share_until_its_second_look() {
        let fake = world();
        let mut monitor = Monitor::new(fake.clone());
        fake.0
            .borrow_mut()
            .tasks
            .push(task(9, Some(7), "/bin/clock", 5000));
        pass(&fake, 1000, &[(1, 1000)]);
        tick(&mut monitor);
        let row = monitor
            .rows()
            .iter()
            .find(|row| row.pid == Some(7))
            .unwrap();
        // Not 500%: the 5000 ms were not used in this interval.
        assert_eq!((row.share, row.cpu_ms), (0, 5000));
        pass(&fake, 1000, &[(9, 300), (1, 700)]);
        tick(&mut monitor);
        let row = monitor
            .rows()
            .iter()
            .find(|row| row.pid == Some(7))
            .unwrap();
        assert_eq!(row.share, 300);
    }

    #[test]
    fn the_history_is_a_minute_long() {
        let fake = world();
        let mut monitor = Monitor::new(fake.clone());
        for second in 0..(HISTORY as u64 + 15) {
            // Busier every second.
            pass(&fake, 1000, &[(1, 1000 - second * 10), (5, second * 10)]);
            tick(&mut monitor);
        }
        let history: Vec<u32> = monitor.processor_history().collect();
        assert_eq!(history.len(), HISTORY);
        assert_eq!(history[HISTORY - 1], 740);
        assert_eq!(history[0], 150);
        assert_eq!(monitor.memory.len(), HISTORY);
        assert_eq!(monitor.memory[0], 250);
    }

    #[test]
    fn a_process_is_selected_by_a_click_and_ended_by_the_button() {
        let fake = world();
        let mut monitor = Monitor::new(fake.clone());
        // The kernel's line cannot be selected.
        click_row(&mut monitor, 0);
        assert_eq!(monitor.selected(), None);
        // The button does nothing without a selection.
        click(&mut monitor, Monitor::<Fake>::end_button(SIZE.0));
        assert!(fake.0.borrow().ended.is_empty());

        click_row(&mut monitor, 2);
        assert_eq!(monitor.selected(), Some(2));
        click(&mut monitor, Monitor::<Fake>::end_button(SIZE.0));
        assert_eq!(fake.0.borrow().ended, [2]);
        // Its line is gone at once, and so is the selection.
        assert_eq!(monitor.rows().len(), 2);
        assert_eq!(monitor.selected(), None);
        assert!(monitor.row_frame(2).is_none());
    }

    #[test]
    fn the_extra_look_after_ending_does_not_disturb_the_shares() {
        let fake = world();
        let mut monitor = Monitor::new(fake.clone());
        pass(&fake, 1000, &[(4, 100), (1, 900)]);
        tick(&mut monitor);
        assert_eq!(monitor.rows()[1].share, 100);
        // 300 ms later the desktop is ended.
        pass(&fake, 300, &[(4, 30), (1, 270)]);
        click_row(&mut monitor, 2);
        click(&mut monitor, Monitor::<Fake>::end_button(SIZE.0));
        assert_eq!(monitor.rows()[1].share, 100);
        // The interval runs on to its end: 1000 ms, 100 of them used.
        pass(&fake, 700, &[(4, 70), (1, 630)]);
        assert!(tick(&mut monitor));
        assert_eq!(monitor.rows()[1].share, 100);
        assert_eq!(monitor.processor_history().last(), Some(100));
    }

    #[test]
    fn a_selected_process_that_ends_by_itself_is_no_longer_selected() {
        let fake = world();
        let mut monitor = Monitor::new(fake.clone());
        click_row(&mut monitor, 1);
        assert_eq!(monitor.selected(), Some(1));
        fake.0.borrow_mut().tasks.retain(|task| task.pid != Some(1));
        pass(&fake, 1000, &[]);
        tick(&mut monitor);
        assert_eq!(monitor.selected(), None);
    }

    #[test]
    fn keys_move_the_selection_switch_pages_and_end() {
        const UP: i32 = 0x101;
        const DOWN: i32 = 0x102;
        let fake = world();
        let mut monitor = Monitor::new(fake.clone());
        let mut context = Context::default();
        monitor.key(DOWN, &mut context);
        assert_eq!(monitor.selected(), Some(1));
        monitor.key(DOWN, &mut context);
        monitor.key(DOWN, &mut context);
        assert_eq!(monitor.selected(), Some(2));
        monitor.key(UP, &mut context);
        assert_eq!(monitor.selected(), Some(1));
        monitor.key(9, &mut context);
        assert_eq!(monitor.page(), 1);
        // On the other page nothing can be ended by accident.
        monitor.key(0x10A, &mut context);
        assert!(fake.0.borrow().ended.is_empty());
        monitor.key(9, &mut context);
        monitor.key(0x10A, &mut context);
        assert_eq!(fake.0.borrow().ended, [1]);
    }

    #[test]
    fn a_long_list_scrolls_with_the_selection() {
        let fake = world();
        for pid in 10..40 {
            fake.0
                .borrow_mut()
                .tasks
                .push(task(100 + pid, Some(pid), "/bin/worker", 0));
        }
        let mut monitor = Monitor::new(fake);
        let visible = monitor.visible_rows();
        assert!(visible < monitor.rows().len());
        assert!(monitor.row_frame(visible).is_none());
        let mut context = Context::default();
        for _ in 0..monitor.rows().len() {
            monitor.key(0x102, &mut context);
        }
        let last = monitor.rows().len() - 1;
        assert_eq!(monitor.selected(), monitor.rows()[last].pid);
        let frame = monitor.row_frame(last).expect("the last row is shown");
        assert!(frame.bottom() <= SIZE.1 - STATUS);
        assert!(monitor.row_frame(0).is_none());
    }

    #[test]
    fn figures_are_written_as_people_read_them() {
        assert_eq!(percent(0), "0.0%");
        assert_eq!(percent(75), "7.5%");
        assert_eq!(percent(1000), "100%");
        assert_eq!(per_mille(5, 0), 0);
        assert_eq!(per_mille(30, 20), 1000);
        assert_eq!(readable_time(420), "0.42 s");
        assert_eq!(readable_time(12_340), "12.3 s");
        assert_eq!(readable_time(245_000), "4:05 min");
        assert_eq!(readable_time(7_380_000), "2:03 h");
        assert_eq!(readable_memory(524_288), "512 KB");
        assert_eq!(readable_memory(36_175_872), "34.5 MB");
        assert_eq!(readable_memory(512 << 20), "512 MB");
        assert_eq!(readable_memory(4 << 30), "4.0 GB");
        assert_eq!(readable_uptime(11_525_000), "3:12:05");
        assert_eq!(short_name("/bin/desktop"), "desktop");
        assert_eq!(short_name("idle"), "idle");
    }

    #[test]
    fn both_pages_draw_at_any_size_in_every_theme() {
        let fake = world();
        let mut monitor = Monitor::new(fake.clone());
        for second in 0..5 {
            pass(&fake, 1000, &[(5, 100 * second), (1, 1000 - 100 * second)]);
            tick(&mut monitor);
        }
        click_row(&mut monitor, 2);
        for mode in [Mode::Light, Mode::Dark] {
            let theme = Theme::new(mode, Accent::Blue);
            for (width, height) in [SIZE, (1280, 700), (200, 120), (60, 40)] {
                for page in 0..2 {
                    monitor.page = page;
                    let mut surface = Surface::new(width as usize, height as usize, theme.window);
                    monitor.draw(&mut surface.canvas(), width, height, &theme);
                    if (width, height) == SIZE {
                        // Something was drawn in the accent's or the
                        // graph's colours.
                        let drawn = surface
                            .pixels()
                            .iter()
                            .filter(|&&pixel| pixel != theme.window)
                            .count();
                        assert!(drawn > 2000, "page {page}: {drawn} pixels");
                    }
                }
            }
        }
    }

    #[test]
    fn the_newest_look_is_the_rightmost_column_of_the_graph() {
        let fake = world();
        let mut monitor = Monitor::new(fake.clone());
        // One idle second, then one fully busy.
        pass(&fake, 1000, &[(1, 1000)]);
        tick(&mut monitor);
        pass(&fake, 1000, &[(5, 1000)]);
        tick(&mut monitor);
        monitor.page = 1;
        let theme = Theme::default();
        let mut surface = Surface::new(SIZE.0 as usize, SIZE.1 as usize, theme.window);
        monitor.draw(&mut surface.canvas(), SIZE.0, SIZE.1, &theme);
        // In the processor card, the accent's solid cap is at the top of
        // the graph in the last column and at the bottom in the one
        // before.
        let card_height = (SIZE.1 - TOP - MARGIN - 12) / 2;
        let graph = Rect::new(
            MARGIN + 14,
            TOP + 64,
            SIZE.0 - 2 * MARGIN - 28,
            card_height - 64 - 12,
        );
        let step = graph.w / HISTORY as i32;
        let column = |back: i32| graph.right() - back * step + step / 2;
        assert_eq!(surface.pixel(column(1), graph.y), Some(theme.accent));
        assert_ne!(surface.pixel(column(2), graph.y), Some(theme.accent));
        assert_eq!(
            surface.pixel(column(2), graph.bottom() - 2),
            Some(theme.accent)
        );
    }
}
